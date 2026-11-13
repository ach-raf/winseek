//! The processing pass: pending files -> hash -> (link | decode, resize, JPEG, embed) -> store.
//! Document / code / PDF files take the text route instead: extract -> chunk -> FTS rows + text
//! vectors (PDF pages without a text layer are rendered, OCR-ed and embedded as images too).
//!
//! Without models the worker runs [`Processor::run_fts_pass`]: text-bearing files still get
//! extracted into FTS and are stored as `text_only` with the vectors they owe; once the models
//! arrive [`Processor::run_upgrade_pass`] embeds them from the stored text, without re-extraction.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pdfium_render::prelude::Pdfium;
use sha2::{Digest, Sha256};
use winseek_llama::prep::prepare_jpeg;
use winseek_store::{ChunkVec, PendingFile, PendingVec, Store, TextOnlyFile, VecKind};

use super::chunk::{self, Chunk};
use super::crawler::{KIND_AUDIO, KIND_IMAGE, KIND_PDF, KIND_VIDEO};
use super::embedder::{is_gpu_busy, EmbedError, Embedder};
use super::extract::{self, ExtractError};
use super::guard;
use super::media::{self, Ffmpeg, MediaTool};
use super::ocr::{self, Ocr, OcrError, OcrText, WinOcr};
use super::pdf::{self, PdfError};
use super::route;

pub const BATCH: usize = 16;
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// A file whose batches keep failing as part of an all-failed batch is marked failed after this many strikes.
pub const MAX_STRIKES: u32 = 3;

#[derive(Debug, PartialEq, Eq)]
pub enum Pass {
    /// Nothing pending.
    Idle,
    /// Handled this many files.
    Progress(usize),
    Paused,
    ModelsMissing,
    /// Embedding failed for the whole batch (server down?); caller should back off.
    EmbedFailed(String),
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Most threads [`par_map`] uses: a batch is 16 files, and the machine stays usable.
const MAX_PREP_THREADS: usize = 8;

/// `items.map(f)` on up to [`MAX_PREP_THREADS`] below-normal-priority threads, order kept.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1)).clamp(1, MAX_PREP_THREADS);
    if items.len() <= 1 || threads == 1 {
        return items.iter().map(f).collect();
    }
    let per = items.len().div_ceil(threads);
    std::thread::scope(|s| {
        let f = &f;
        let parts: Vec<_> = items
            .chunks(per)
            .map(|c| {
                s.spawn(move || {
                    super::lower_thread_priority();
                    c.iter().map(f).collect::<Vec<R>>()
                })
            })
            .collect();
        parts.into_iter().flat_map(|h| h.join().expect("prep thread panicked")).collect()
    })
}

/// Per-process state of the pass loop.
pub struct Processor {
    strikes: HashMap<i64, u32>,
    ocr: Box<dyn Ocr>,
    /// Screenshots folder used by detection (None = name / dimension rules only).
    screenshots_dir: Option<String>,
    /// Last value written to the `ocr_unavailable` setting.
    ocr_flag: Option<bool>,
    /// Binds / returns the process-wide pdfium (tests inject a stub).
    pdf_runtime: fn() -> Result<&'static Pdfium, PdfError>,
    /// Is the pdfium DLL on disk (tests stub it: the real check looks at %LOCALAPPDATA%)?
    pdf_on_disk: fn() -> bool,
    /// Asked (from the worker thread, must not block) to fetch pdfium when PDFs are queued without it.
    pdf_requester: Option<Box<dyn Fn() + Send>>,
    /// pdfium is on disk but would not load: leave PDFs alone until then.
    pdf_block_until: Option<Instant>,
    /// FTS-only mode: images are scanned for screenshots in id order from here.
    img_cursor: i64,
    cursor_wrapped: Instant,
    /// Upgrade attempts that failed to embed, per file id.
    upgrade_strikes: HashMap<i64, u32>,
    /// ffmpeg for audio / video files (tests inject a fake).
    media: Box<dyn MediaTool + Send>,
}

impl Default for Processor {
    fn default() -> Self {
        Processor::with_ocr(Box::new(WinOcr::new()), ocr::screenshots_dir().map(str::to_owned))
    }
}

/// Setting key: "1" while screenshots could not be OCR-ed (no engine / language installed).
pub const SETTING_OCR_UNAVAILABLE: &str = "ocr_unavailable";
/// Longest OCR text sent to the embedder (characters).
const OCR_EMBED_CHARS: usize = 3000;
/// Chunk index of the OCR text vector / FTS row (the image vector is chunk 0).
const OCR_CHUNK: u32 = 1;
/// Chunks sent to the embedder per request (1500 characters each).
const TEXT_EMBED_BATCH: usize = 8;
/// Page images sent to the embedder per request.
const IMAGE_EMBED_BATCH: usize = 8;
/// Text chunks kept per PDF page / per PDF.
const PDF_PAGE_CHUNKS: usize = 8;
const PDF_MAX_CHUNKS: usize = 400;
/// Text chunks one document may hold in all, tail included (the first pass keeps [`PDF_MAX_CHUNKS`]).
const PDF_TAIL_MAX_CHUNKS: u32 = 20_000;
/// How long PDFs are left alone after pdfium failed to load.
const PDF_RETRY: Duration = Duration::from_secs(60);
/// Text-only files upgraded per pass.
const UPGRADE_BATCH: usize = 4;
/// Images looked at per FTS-only pass when searching for screenshots.
const SCAN_BATCH: usize = 24;
/// The screenshot scan starts over (picks up modified files) after this long.
const SCAN_WRAP: Duration = Duration::from_secs(3600);
/// Upgrade attempts that may fail to embed before the file is finished without vectors.
const UPGRADE_STRIKES: u32 = MAX_STRIKES;

struct Prepared {
    file: PendingFile,
    hash: String,
    /// Index into the batch's unique JPEG list.
    slot: usize,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Processor {
    pub fn with_ocr(ocr: Box<dyn Ocr>, screenshots_dir: Option<String>) -> Self {
        Processor {
            strikes: HashMap::new(),
            ocr,
            screenshots_dir,
            ocr_flag: None,
            pdf_runtime: pdf::runtime,
            pdf_on_disk: || pdf::locate_dll().is_some(),
            pdf_requester: None,
            pdf_block_until: None,
            img_cursor: 0,
            cursor_wrapped: Instant::now(),
            upgrade_strikes: HashMap::new(),
            media: Box::new(Ffmpeg),
        }
    }

    /// Replace the ffmpeg sidecar (tests).
    #[cfg(test)]
    fn with_media(mut self, m: Box<dyn MediaTool + Send>) -> Self {
        self.media = m;
        self
    }

    /// Callback that starts the pdfium download when PDFs are queued but the DLL is missing.
    pub fn with_pdf_requester(mut self, f: Box<dyn Fn() + Send>) -> Self {
        self.pdf_requester = Some(f);
        self
    }

    /// Can PDFs be processed now? When not, and PDFs are waiting, the requester is asked for pdfium.
    fn pdf_ready(&mut self, store: &Mutex<Store>) -> bool {
        if self.pdf_block_until.is_some_and(|t| Instant::now() < t) {
            return false;
        }
        if (self.pdf_runtime)().is_ok() {
            return true;
        }
        if (self.pdf_on_disk)() {
            // on disk but unloadable (corrupt / wrong architecture): do not hammer it
            self.pdf_block_until = Some(Instant::now() + PDF_RETRY);
        } else if lock(store).has_pending_kind(KIND_PDF).unwrap_or(false) {
            if let Some(r) = &self.pdf_requester {
                r();
            }
        }
        false
    }

    /// Is there embedding work queued (pending files that can be processed now, or text-only files
    /// waiting for their vectors)? The worker unloads the document parser before such work, and
    /// starts parse jobs only when there is none.
    pub fn has_index_work(&mut self, store: &Mutex<Store>) -> bool {
        let skip = self.excluded_kinds(store);
        let s = lock(store);
        s.next_pending_where(1, &[], &skip, 0).is_ok_and(|p| !p.is_empty()) || s.next_text_only(1).is_ok_and(|p| !p.is_empty())
    }

    /// Kinds to leave queued for now.
    fn excluded_kinds(&mut self, store: &Mutex<Store>) -> Vec<&'static str> {
        let mut kinds = if self.pdf_ready(store) { vec![] } else { vec![KIND_PDF] };
        // Audio / video wait for the ffmpeg sidecar, like PDFs wait for pdfium.
        if !self.media.available() {
            kinds.extend([KIND_AUDIO, KIND_VIDEO]);
        }
        kinds
    }

    /// OCR `bytes` when the file looks like a screenshot. `Ok(None)` = not a screenshot or no
    /// text; an unavailable engine is remembered in `unavailable`.
    fn ocr_screenshot(&mut self, path: &str, bytes: &[u8], unavailable: &mut bool) -> Option<OcrText> {
        let dims = ocr::image_dimensions(bytes);
        if !ocr::is_screenshot(path, dims, self.screenshots_dir.as_deref()) {
            return None;
        }
        match self.ocr.recognize(bytes) {
            Ok(t) if !t.is_empty() => Some(t),
            Ok(_) => None,
            Err(OcrError::Unavailable) => {
                *unavailable = true;
                None
            }
            Err(OcrError::Failed(_)) => None, // OCR is best-effort; the image vector is still stored
        }
    }

    /// One pass over up to [`BATCH`] pending files.
    pub fn run_pass(&mut self, store: &Mutex<Store>, embedder: &Mutex<Box<dyn Embedder + Send>>, paused: &AtomicBool) -> Result<Pass, String> {
        if paused.load(Ordering::Relaxed) {
            return Ok(Pass::Paused);
        }
        let skip = self.excluded_kinds(store);
        let pending = lock(store).next_pending_where(BATCH, &[], &skip, 0).map_err(|e| e.to_string())?;
        if pending.is_empty() {
            return Ok(Pass::Idle);
        }
        // Audio / video files take long (decode + dozens of windows): a pass that finds one handles
        // only those; the rest of the batch stays queued for the next pass.
        let (media_files, pending): (Vec<PendingFile>, Vec<PendingFile>) =
            pending.into_iter().partition(|f| f.kind == KIND_AUDIO || f.kind == KIND_VIDEO);
        if !media_files.is_empty() {
            return self.run_media(store, embedder, paused, &media_files);
        }
        let mut handled = 0usize;
        let mut prepared: Vec<Prepared> = Vec::new();
        let mut jpegs: Vec<Vec<u8>> = Vec::new();
        let mut by_hash: HashMap<String, usize> = HashMap::new();
        // OCR output per unique JPEG slot.
        let mut ocr_by_slot: HashMap<usize, OcrText> = HashMap::new();
        let mut ocr_unavailable = false;
        let mut ocr_ran = false;

        let (images, others): (Vec<PendingFile>, Vec<PendingFile>) = pending.into_iter().partition(|f| f.kind == KIND_IMAGE);
        for f in others {
            self.process_text(store, Some(embedder), &f)?;
            handled += 1;
        }
        // Reading + hashing, then decoding + resizing, run on all cores: one thread per file made the
        // CPU the bottleneck of photo folders while the GPU waited.
        let read: Vec<Result<(Vec<u8>, String), String>> = par_map(&images, |f| {
            if f.size > MAX_FILE_BYTES {
                return Err(String::new());
            }
            let bytes = guard::read_guarded(Path::new(&f.path)).map_err(|e| match guard::skip_reason(&e) {
                Some(r) => format!("@{r}"),
                None => format!("read: {e}"),
            })?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err(String::new());
            }
            let hash = sha256_hex(&bytes);
            // HEIC/HEIF: decoded by the Windows codec, the pipeline gets a JPEG (hash stays the file's).
            let bytes = super::heic::normalize(&f.path, bytes).ok_or_else(|| "@no_heic_codec".to_string())?;
            Ok((bytes, hash))
        });
        let mut todo: Vec<(PendingFile, Vec<u8>, String)> = Vec::new();
        for (f, r) in images.into_iter().zip(read) {
            let (bytes, hash) = match r {
                Ok(x) => x,
                Err(e) if e.is_empty() => {
                    lock(store).skip(f.id, "file larger than 64 MB").map_err(|e| e.to_string())?;
                    handled += 1;
                    continue;
                }
                Err(e) if e.starts_with('@') => {
                    lock(store).skip(f.id, &e[1..]).map_err(|e| e.to_string())?;
                    handled += 1;
                    continue;
                }
                Err(e) => {
                    lock(store).fail(f.id, &e).map_err(|e| e.to_string())?;
                    handled += 1;
                    continue;
                }
            };
            let known = lock(store).has_content(&hash).map_err(|e| e.to_string())?;
            if known {
                lock(store).complete(f.id, &hash, &[], &[]).map_err(|e| e.to_string())?;
                handled += 1;
                continue;
            }
            todo.push((f, bytes, hash));
        }
        // Decode only the first file of each content hash; later ones link to its slot.
        let first_of: Vec<bool> = {
            let mut seen = std::collections::HashSet::new();
            todo.iter().map(|(_, _, h)| seen.insert(h.clone())).collect()
        };
        let firsts: Vec<&[u8]> = todo.iter().zip(&first_of).filter(|(_, &first)| first).map(|((_, b, _), _)| b.as_slice()).collect();
        let mut decoded = par_map(&firsts, |b| prepare_jpeg(b)).into_iter();
        for ((f, bytes, hash), first) in todo.into_iter().zip(first_of) {
            if !first {
                match by_hash.get(&hash) {
                    Some(&slot) => prepared.push(Prepared { file: f, hash, slot }),
                    None => {
                        // its twin could not be decoded either
                        lock(store).fail(f.id, "decode failed").map_err(|e| e.to_string())?;
                        handled += 1;
                    }
                }
                continue;
            }
            match decoded.next().expect("one decode per first file") {
                Ok(j) => {
                    if let Some(t) = self.ocr_screenshot(&f.path, &bytes, &mut ocr_unavailable) {
                        ocr_ran = true;
                        ocr_by_slot.insert(jpegs.len(), t);
                    }
                    by_hash.insert(hash.clone(), jpegs.len());
                    prepared.push(Prepared { file: f, hash, slot: jpegs.len() });
                    jpegs.push(j);
                }
                Err(msg) => {
                    lock(store).fail(f.id, &msg).map_err(|e| e.to_string())?;
                    handled += 1;
                }
            }
        }

        self.record_ocr_flag(store, ocr_unavailable, ocr_ran);
        if prepared.is_empty() {
            return Ok(Pass::Progress(handled));
        }
        if paused.load(Ordering::Relaxed) {
            return Ok(if handled > 0 { Pass::Progress(handled) } else { Pass::Paused });
        }

        let vectors = match self.embed_all(embedder, &jpegs) {
            Ok(v) => v,
            Err(Failure::ModelsMissing) => return Ok(Pass::ModelsMissing),
            // The parser holds the GPU: not the files' fault. The worker unloads the parser and retries.
            Err(Failure::Systemic(msg)) if is_gpu_busy(&msg) => {
                return Ok(if handled > 0 { Pass::Progress(handled) } else { Pass::EmbedFailed(msg) });
            }
            Err(Failure::Systemic(msg)) => {
                lock(embedder).reset();
                for p in &prepared {
                    let strikes = self.strikes.entry(p.file.id.0).or_insert(0);
                    *strikes += 1;
                    if *strikes >= MAX_STRIKES {
                        lock(store).fail(p.file.id, &format!("embed: {msg}")).map_err(|e| e.to_string())?;
                        self.strikes.remove(&p.file.id.0);
                        handled += 1;
                    }
                }
                return if handled > 0 { Ok(Pass::Progress(handled)) } else { Ok(Pass::EmbedFailed(msg)) };
            }
        };

        let text_vecs = Self::embed_ocr_texts(embedder, &ocr_by_slot);

        for p in prepared {
            self.strikes.remove(&p.file.id.0);
            match &vectors[p.slot] {
                Ok(vec) => {
                    let mut chunks = vec![ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec: vec.clone() }];
                    let mut texts: Vec<(u32, String)> = Vec::new();
                    if let Some(t) = ocr_by_slot.get(&p.slot) {
                        // Line rectangles ride on both vectors' locator (preview bbox highlight),
                        // so a hit exposes them whichever chunk wins the per-file ranking.
                        let loc = t.locator_json();
                        chunks[0].locator = Some(loc.clone());
                        texts.push((OCR_CHUNK, t.text.clone()));
                        if let Some(v) = text_vecs.get(&p.slot) {
                            chunks.push(ChunkVec { chunk: OCR_CHUNK, kind: VecKind::Text, locator: Some(loc), vec: v.clone() });
                        }
                    }
                    // A duplicate in the same batch finds the content already stored and just links.
                    lock(store).complete(p.file.id, &p.hash, &chunks, &texts).map_err(|e| e.to_string())?;
                    if ocr_by_slot.get(&p.slot).is_some_and(|t| route::screenshot_wants_parser(&t.text)) {
                        lock(store).queue_parse(&p.hash, 0, "screenshot").map_err(|e| e.to_string())?;
                    }
                }
                Err(msg) => lock(store).fail(p.file.id, &format!("embed: {msg}")).map_err(|e| e.to_string())?,
            }
            handled += 1;
        }
        Ok(Pass::Progress(handled))
    }

    /// Audio / video files of one pass, one after the other (see [`media::run_file`]).
    fn run_media(
        &mut self,
        store: &Mutex<Store>,
        embedder: &Mutex<Box<dyn Embedder + Send>>,
        paused: &AtomicBool,
        files: &[PendingFile],
    ) -> Result<Pass, String> {
        let mut handled = 0usize;
        let done = |handled: usize, otherwise: Pass| if handled > 0 { Pass::Progress(handled) } else { otherwise };
        for f in files {
            match media::run_file(&*self.media, store, embedder, paused, f, &mut self.strikes, MAX_STRIKES)? {
                media::Step::Handled => handled += 1,
                media::Step::Paused => return Ok(done(handled, Pass::Paused)),
                media::Step::ModelsMissing => return Ok(done(handled, Pass::ModelsMissing)),
                media::Step::Retry(msg) => {
                    lock(embedder).reset();
                    return Ok(done(handled, Pass::EmbedFailed(msg)));
                }
            }
        }
        Ok(Pass::Progress(handled))
    }

    /// The text route for one document / code / PDF file: read, hash (content dedup as for images),
    /// extract, chunk, embed. FTS gets every chunk. Without an embedder (`None`, FTS-only mode) or when
    /// the models turn out to be missing the file is stored `text_only` with the vectors it owes; if
    /// embedding fails for another reason it is stored with text only.
    fn process_text(
        &mut self,
        store: &Mutex<Store>,
        embedder: Option<&Mutex<Box<dyn Embedder + Send>>>,
        f: &PendingFile,
    ) -> Result<(), String> {
        let db = |e: winseek_store::Error| e.to_string();
        let limit = if f.kind == KIND_PDF { pdf::MAX_FILE_BYTES } else { extract::MAX_FILE_BYTES };
        if f.size > limit {
            return lock(store).skip(f.id, "too_large").map_err(db);
        }
        let bytes = match guard::read_guarded(Path::new(&f.path)) {
            Ok(b) => b,
            Err(e) => {
                return match guard::skip_reason(&e) {
                    Some(r) => lock(store).skip(f.id, r).map_err(db),
                    None => lock(store).fail(f.id, &format!("read: {e}")).map_err(db),
                }
            }
        };
        if bytes.len() as u64 > limit {
            return lock(store).skip(f.id, "too_large").map_err(db);
        }
        let hash = sha256_hex(&bytes);
        if lock(store).has_content(&hash).map_err(db)? {
            return lock(store).complete(f.id, &hash, &[], &[]).map_err(db);
        }
        let items = if f.kind == KIND_PDF {
            match self.extract_pdf(store, &bytes, 1, pdf::HEAD_PAGES, 0, PDF_MAX_CHUNKS) {
                Ok((items, total_pages)) => {
                    // Long documents: the head is searchable at once, the rest follows in the background.
                    if total_pages > pdf::HEAD_PAGES {
                        lock(store).queue_tail(&hash, pdf::HEAD_PAGES + 1, total_pages.min(pdf::TAIL_PAGE_CAP)).map_err(db)?;
                    }
                    items
                }
                Err(Stop::Skip(reason)) => return lock(store).skip(f.id, reason).map_err(db),
                Err(Stop::Fail(msg)) => return lock(store).fail(f.id, &msg).map_err(db),
                Err(Stop::Defer) => {
                    self.pdf_block_until = Some(Instant::now() + PDF_RETRY);
                    return Ok(()); // stays queued
                }
            }
        } else {
            let doc = match extract::extract_file(&f.path, &bytes) {
                Ok(d) => d,
                Err(ExtractError::Skip(reason)) => return lock(store).skip(f.id, reason).map_err(db),
                Err(ExtractError::Failed(msg)) => return lock(store).fail(f.id, &msg).map_err(db),
            };
            text_items(chunk::chunk(&doc), 0)
        };
        drop(bytes);
        if items.is_empty() {
            return lock(store).skip(f.id, "no_text").map_err(db);
        }
        let texts = item_texts(&items);
        let stored = match embedder {
            None => lock(store).complete_text_only(f.id, &hash, &item_pending(&items), &texts).map_err(db),
            Some(embedder) => match embed_items(embedder, &items) {
                Ok(vectors) => lock(store).complete(f.id, &hash, &vectors, &texts).map_err(db),
                Err(EmbedError::ModelsMissing) => lock(store).complete_text_only(f.id, &hash, &item_pending(&items), &texts).map_err(db),
                Err(EmbedError::Other(_)) => lock(store).complete(f.id, &hash, &[], &texts).map_err(db),
            },
        };
        // Scanned pages are enrichment candidates for the document parser (it upgrades their OCR text).
        if stored.is_ok() && f.kind == KIND_PDF {
            for page in scanned_pages(&items) {
                lock(store).queue_parse(&hash, page, "scanned_pdf").map_err(db)?;
            }
        }
        stored
    }

    /// Text chunks per page; scanned pages are OCR-ed and also kept as page images. Chunk numbers
    /// are unique across texts and images (a scanned page's image comes before its OCR text).
    /// Reads the 1-based pages `from..=to`, numbering chunks from `first_chunk` and keeping at most
    /// `max_text_chunks` text chunks; also returns the document's page count.
    fn extract_pdf(
        &mut self,
        store: &Mutex<Store>,
        bytes: &[u8],
        from: u32,
        to: u32,
        first_chunk: u32,
        max_text_chunks: usize,
    ) -> Result<(Vec<Item>, u32), Stop> {
        let rt = (self.pdf_runtime)().map_err(|_| Stop::Defer)?;
        let read = match pdf::read_pdf_range(rt, bytes, true, from, to) {
            Ok(r) => r,
            Err(PdfError::Encrypted) => return Err(Stop::Skip("encrypted")),
            Err(PdfError::Failed(m)) => return Err(Stop::Fail(m)),
            Err(PdfError::Unavailable(_)) => return Err(Stop::Defer),
        };
        let mut items: Vec<Item> = Vec::new();
        let mut next = first_chunk;
        let mut text_chunks = 0usize;
        let total_pages = read.total_pages;
        let (mut ocr_unavailable, mut ocr_ran) = (false, false);
        for page in read.pages {
            let mut page_texts: Vec<Chunk> = chunk::chunk_page(&page.text, page.page, PDF_PAGE_CHUNKS);
            let mut image: Option<Vec<u8>> = None;
            if let Some(jpeg) = &page.jpeg {
                match self.ocr.recognize(jpeg) {
                    Ok(t) => {
                        ocr_ran = true;
                        if !t.is_empty() {
                            page_texts.extend(chunk::chunk_page(&t.text, page.page, PDF_PAGE_CHUNKS));
                        }
                    }
                    Err(OcrError::Unavailable) => ocr_unavailable = true,
                    Err(OcrError::Failed(_)) => {}
                }
                image = prepare_jpeg(jpeg).ok();
            }
            if let Some(j) = image {
                items.push(Item::Image { chunk: next, jpeg: j, locator: page_image_locator(page.page) });
                next += 1;
            }
            for c in page_texts {
                if text_chunks >= max_text_chunks {
                    break;
                }
                items.push(Item::Text { chunk: next, text: c.text, locator: c.locator });
                next += 1;
                text_chunks += 1;
            }
        }
        self.record_ocr_flag(store, ocr_unavailable, ocr_ran);
        Ok((items, total_pages))
    }

    // ------------------------------------------------------------ tail of long documents

    /// Index the next slice of one long document's tail (see `winseek_store::TailJob`). Runs only when
    /// nothing else is waiting; returns `Idle` when no tail is queued. A slice that cannot be embedded
    /// is retried, then stored text-only; a file that changed since the head was indexed is dropped.
    pub fn run_tail_pass(
        &mut self,
        store: &Mutex<Store>,
        embedder: &Mutex<Box<dyn Embedder + Send>>,
        paused: &AtomicBool,
    ) -> Result<Pass, String> {
        if paused.load(Ordering::Relaxed) {
            return Ok(Pass::Paused);
        }
        if self.pdf_block_until.is_some_and(|t| Instant::now() < t) {
            return Ok(Pass::Idle);
        }
        let db = |e: winseek_store::Error| e.to_string();
        let Some(job) = lock(store).next_tail(1).map_err(db)?.into_iter().next() else { return Ok(Pass::Idle) };
        let bytes = match std::fs::read(Path::new(&job.path)) {
            Ok(b) if sha256_hex(&b) == job.hash => b,
            Ok(_) => return lock(store).fail_tail(&job.hash, "file changed").map_err(db).map(|_| Pass::Progress(1)),
            Err(e) => {
                let n = lock(store).bump_tail(&job.hash, &format!("read: {e}")).map_err(db)?;
                if n >= MAX_STRIKES {
                    lock(store).fail_tail(&job.hash, &format!("read: {e}")).map_err(db)?;
                }
                return Ok(Pass::Progress(1));
            }
        };
        let to = job.next_page.saturating_add(pdf::TAIL_SLICE - 1).min(job.total);
        let (first, budget) = {
            let n = lock(store).next_chunk(&job.hash).map_err(db)?;
            (n, PDF_TAIL_MAX_CHUNKS.saturating_sub(n) as usize)
        };
        let (items, _) = match self.extract_pdf(store, &bytes, job.next_page, to, first, budget) {
            Ok(x) => x,
            Err(Stop::Defer) => {
                self.pdf_block_until = Some(Instant::now() + PDF_RETRY);
                return Ok(Pass::Idle);
            }
            Err(Stop::Skip(reason)) => return lock(store).fail_tail(&job.hash, reason).map_err(db).map(|_| Pass::Progress(1)),
            Err(Stop::Fail(msg)) => return lock(store).fail_tail(&job.hash, &msg).map_err(db).map(|_| Pass::Progress(1)),
        };
        // Out of chunk budget: whatever is stored stays, the rest of the document is not indexed.
        let last = if budget == 0 { job.total } else { to };
        let vectors = if items.is_empty() {
            Vec::new()
        } else {
            match embed_items(embedder, &items) {
                Ok(v) => v,
                Err(EmbedError::ModelsMissing) => return Ok(Pass::ModelsMissing),
                Err(EmbedError::Other(msg)) if is_gpu_busy(&msg) => return Ok(Pass::EmbedFailed(msg)),
                Err(EmbedError::Other(msg)) => {
                    if lock(store).bump_tail(&job.hash, &msg).map_err(db)? < UPGRADE_STRIKES {
                        lock(embedder).reset();
                        return Ok(Pass::EmbedFailed(msg));
                    }
                    Vec::new() // text stays searchable
                }
            }
        };
        lock(store).append_tail(&job.hash, last + 1, &vectors, &item_texts(&items)).map_err(db)?;
        for page in scanned_pages(&items) {
            lock(store).queue_parse(&job.hash, page, "scanned_pdf").map_err(db)?;
        }
        Ok(Pass::Progress(1))
    }

    // ------------------------------------------------------------ FTS-only mode (no models yet)

    /// One pass without models: text-bearing files (documents, enabled code, PDFs, screenshots) are
    /// extracted into FTS and stored `text_only`; plain images wait for the models.
    pub fn run_fts_pass(&mut self, store: &Mutex<Store>, paused: &AtomicBool) -> Result<Pass, String> {
        if paused.load(Ordering::Relaxed) {
            return Ok(Pass::Paused);
        }
        let mut skip = self.excluded_kinds(store);
        skip.extend([KIND_IMAGE, KIND_AUDIO, KIND_VIDEO]); // vectors only: nothing to extract without models
        let pending = lock(store).next_pending_where(BATCH, &[], &skip, 0).map_err(|e| e.to_string())?;
        let mut handled = 0usize;
        for f in pending {
            self.process_text(store, None, &f)?;
            handled += 1;
            if paused.load(Ordering::Relaxed) {
                return Ok(Pass::Progress(handled));
            }
        }
        // Screenshots: look at the next few queued images by id; the rest wait for the image model.
        let imgs = lock(store).next_pending_where(SCAN_BATCH, &[KIND_IMAGE], &[], self.img_cursor).map_err(|e| e.to_string())?;
        let scanned = imgs.len();
        let (mut ocr_unavailable, mut ocr_ran) = (false, false);
        for f in imgs {
            self.img_cursor = f.id.0;
            if self.fts_screenshot(store, &f, &mut ocr_unavailable, &mut ocr_ran)? {
                handled += 1;
            }
            if paused.load(Ordering::Relaxed) {
                break;
            }
        }
        self.record_ocr_flag(store, ocr_unavailable, ocr_ran);
        if scanned == 0 && self.img_cursor > 0 && self.cursor_wrapped.elapsed() > SCAN_WRAP {
            self.img_cursor = 0; // pick up files that were modified since the last scan
            self.cursor_wrapped = Instant::now();
        }
        Ok(if handled > 0 || scanned > 0 { Pass::Progress(handled) } else { Pass::Idle })
    }

    /// OCR one queued image if it looks like a screenshot and keep the text (`true` = stored).
    fn fts_screenshot(&mut self, store: &Mutex<Store>, f: &PendingFile, unavailable: &mut bool, ran: &mut bool) -> Result<bool, String> {
        let db = |e: winseek_store::Error| e.to_string();
        if f.size > MAX_FILE_BYTES {
            return Ok(false);
        }
        // Name / folder rules first; a PNG may still match by its pixel size (read from the header).
        if !ocr::is_screenshot(&f.path, None, self.screenshots_dir.as_deref()) {
            if !f.path.to_ascii_lowercase().ends_with(".png") {
                return Ok(false);
            }
            let head = guard::read_head(Path::new(&f.path), 4096).unwrap_or_default();
            if !ocr::is_screenshot(&f.path, ocr::image_dimensions(&head), self.screenshots_dir.as_deref()) {
                return Ok(false);
            }
        }
        let Ok(bytes) = guard::read_guarded(Path::new(&f.path)) else { return Ok(false) };
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Ok(false);
        }
        let hash = sha256_hex(&bytes);
        if lock(store).has_content(&hash).map_err(db)? {
            lock(store).complete(f.id, &hash, &[], &[]).map_err(db)?;
            return Ok(true);
        }
        let text = match self.ocr.recognize(&bytes) {
            Ok(t) if !t.is_empty() => {
                *ran = true;
                t
            }
            Ok(_) => {
                *ran = true;
                return Ok(false);
            }
            Err(OcrError::Unavailable) => {
                *unavailable = true;
                return Ok(false);
            }
            Err(OcrError::Failed(_)) => return Ok(false),
        };
        let loc = text.locator_json();
        let pending = [
            PendingVec { chunk: 0, kind: VecKind::Image, locator: Some(loc.clone()) },
            PendingVec { chunk: OCR_CHUNK, kind: VecKind::Text, locator: Some(loc) },
        ];
        let wants_parser = route::screenshot_wants_parser(&text.text);
        lock(store).complete_text_only(f.id, &hash, &pending, &[(OCR_CHUNK, text.text)]).map_err(db)?;
        if wants_parser {
            lock(store).queue_parse(&hash, 0, "screenshot").map_err(db)?;
        }
        Ok(true)
    }

    // ------------------------------------------------------------ upgrade: text_only -> done

    /// Embed files that were stored `text_only`, from their stored text (and page images re-rendered
    /// from the unchanged file), without extracting again.
    pub fn run_upgrade_pass(
        &mut self,
        store: &Mutex<Store>,
        embedder: &Mutex<Box<dyn Embedder + Send>>,
        paused: &AtomicBool,
    ) -> Result<Pass, String> {
        if paused.load(Ordering::Relaxed) {
            return Ok(Pass::Paused);
        }
        let db = |e: winseek_store::Error| e.to_string();
        let files = lock(store).next_text_only(UPGRADE_BATCH).map_err(db)?;
        if files.is_empty() {
            return Ok(Pass::Idle);
        }
        let mut handled = 0usize;
        for f in files {
            let (pend, texts) = {
                let s = lock(store);
                (s.pending_vecs(&f.hash).map_err(db)?, s.texts_of(&f.hash).map_err(db)?)
            };
            let mut items: Vec<Item> = Vec::new();
            let mut images: Vec<&PendingVec> = Vec::new();
            for p in &pend {
                match p.kind {
                    VecKind::Text => {
                        if let Some((_, body)) = texts.iter().find(|(c, _)| *c == p.chunk) {
                            items.push(Item::Text { chunk: p.chunk, text: body.clone(), locator: p.locator.clone().unwrap_or_default() });
                        }
                    }
                    VecKind::Image | VecKind::Page => images.push(p),
                    VecKind::Audio => {} // audio / video never wait text-only: they are not re-derivable here
                }
            }
            if !images.is_empty() {
                items.extend(self.rebuild_images(&f, &images));
            }
            match embed_items(embedder, &items) {
                Ok(vectors) => {
                    lock(store).finish_vectors(&f.hash, &vectors).map_err(db)?;
                    self.upgrade_strikes.remove(&f.id.0);
                    handled += 1;
                }
                Err(EmbedError::ModelsMissing) => return Ok(if handled > 0 { Pass::Progress(handled) } else { Pass::ModelsMissing }),
                Err(EmbedError::Other(msg)) if is_gpu_busy(&msg) => {
                    return Ok(if handled > 0 { Pass::Progress(handled) } else { Pass::EmbedFailed(msg) });
                }
                Err(EmbedError::Other(msg)) => {
                    let n = self.upgrade_strikes.entry(f.id.0).or_insert(0);
                    *n += 1;
                    if *n >= UPGRADE_STRIKES {
                        self.upgrade_strikes.remove(&f.id.0);
                        lock(store).finish_vectors(&f.hash, &[]).map_err(db)?; // text stays searchable
                        handled += 1;
                    } else {
                        lock(embedder).reset();
                        return Ok(if handled > 0 { Pass::Progress(handled) } else { Pass::EmbedFailed(msg) });
                    }
                }
            }
        }
        Ok(Pass::Progress(handled))
    }

    /// Image vectors a text-only file still owes: PDF pages are re-rendered, an image file is re-read
    /// (both only if the file is unchanged). Anything that cannot be rebuilt is dropped.
    fn rebuild_images(&mut self, f: &TextOnlyFile, wanted: &[&PendingVec]) -> Vec<Item> {
        let Ok(bytes) = guard::read_guarded(Path::new(&f.path)) else { return vec![] };
        if sha256_hex(&bytes) != f.hash {
            return vec![];
        }
        let locator_of = |p: &PendingVec| p.locator.clone().unwrap_or_default();
        if f.kind == KIND_PDF {
            let Ok(rt) = (self.pdf_runtime)() else { return vec![] };
            let page_of = |p: &PendingVec| {
                p.locator.as_deref().and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok()).and_then(|v| v["page"].as_u64())
            };
            let pages: Vec<u32> = wanted.iter().filter_map(|p| page_of(p)).map(|n| n as u32).collect();
            let Ok(rendered) = pdf::render_pages(rt, &bytes, &pages) else { return vec![] };
            wanted
                .iter()
                .filter_map(|p| {
                    let n = page_of(p)? as u32;
                    let (_, jpeg) = rendered.iter().find(|(pn, _)| *pn == n)?;
                    Some(Item::Image { chunk: p.chunk, jpeg: prepare_jpeg(jpeg).ok()?, locator: locator_of(p) })
                })
                .collect()
        } else {
            wanted
                .first()
                .and_then(|p| {
                    let bytes = super::heic::normalize(&f.path, bytes)?;
                    Some(Item::Image { chunk: p.chunk, jpeg: prepare_jpeg(&bytes).ok()?, locator: locator_of(p) })
                })
                .into_iter()
                .collect()
        }
    }

    /// Best-effort text embeddings of the OCR output (FTS still gets the text if this fails).
    fn embed_ocr_texts(embedder: &Mutex<Box<dyn Embedder + Send>>, by_slot: &HashMap<usize, OcrText>) -> HashMap<usize, Vec<f32>> {
        if by_slot.is_empty() {
            return HashMap::new();
        }
        let slots: Vec<usize> = by_slot.keys().copied().collect();
        let docs: Vec<String> = slots.iter().map(|s| by_slot[s].text.chars().take(OCR_EMBED_CHARS).collect()).collect();
        match lock(embedder).embed_documents(&docs) {
            Ok(v) if v.len() == slots.len() => slots.into_iter().zip(v).collect(),
            _ => HashMap::new(),
        }
    }

    /// Keep the `ocr_unavailable` setting in step: set when a screenshot needed OCR and no
    /// engine / language exists, cleared once OCR works again.
    fn record_ocr_flag(&mut self, store: &Mutex<Store>, unavailable: bool, ran: bool) {
        let want = if unavailable {
            true
        } else if ran {
            false
        } else {
            return;
        };
        if self.ocr_flag == Some(want) {
            return;
        }
        let v = want.then_some("1");
        if lock(store).set_setting(SETTING_OCR_UNAVAILABLE, v).is_ok() {
            self.ocr_flag = Some(want);
        }
    }

    /// Embed the batch; on a batch error retry one by one so a single poison image cannot
    /// block the queue. All-failed means the backend is probably down (systemic).
    fn embed_all(&self, embedder: &Mutex<Box<dyn Embedder + Send>>, jpegs: &[Vec<u8>]) -> Result<Vec<Result<Vec<f32>, String>>, Failure> {
        let mut e = lock(embedder);
        match e.embed_images(jpegs) {
            Ok(v) if v.len() == jpegs.len() => return Ok(v.into_iter().map(Ok).collect()),
            Ok(v) => {
                return Err(Failure::Systemic(format!("expected {} vectors, got {}", jpegs.len(), v.len())));
            }
            Err(EmbedError::ModelsMissing) => return Err(Failure::ModelsMissing),
            Err(EmbedError::Other(msg)) if jpegs.len() == 1 => return Err(Failure::Systemic(msg)),
            Err(EmbedError::Other(_)) => {}
        }
        let mut out = Vec::with_capacity(jpegs.len());
        let mut last_err = String::new();
        for j in jpegs {
            match e.embed_images(std::slice::from_ref(j)) {
                Ok(mut v) if v.len() == 1 => out.push(Ok(v.remove(0))),
                Ok(_) => {
                    last_err = "wrong vector count".into();
                    out.push(Err(last_err.clone()));
                }
                Err(EmbedError::ModelsMissing) => return Err(Failure::ModelsMissing),
                Err(EmbedError::Other(msg)) => {
                    last_err = msg.clone();
                    out.push(Err(msg));
                }
            }
        }
        if out.iter().all(Result::is_err) {
            return Err(Failure::Systemic(last_err));
        }
        Ok(out)
    }
}

/// One thing a text-route file owes the index: the vector of a text chunk, or of a page image.
#[derive(Debug, Clone)]
enum Item {
    Text { chunk: u32, text: String, locator: String },
    Image { chunk: u32, jpeg: Vec<u8>, locator: String },
}

/// Why a PDF produced no items.
enum Stop {
    Skip(&'static str),
    Fail(String),
    /// pdfium is not usable right now; the file stays queued.
    Defer,
}

fn page_image_locator(page: u32) -> String {
    serde_json::json!({ "page": page }).to_string()
}

/// The pages stored as page images (the scanned ones), in order.
fn scanned_pages(items: &[Item]) -> Vec<u32> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Image { locator, .. } => super::locator_page(Some(locator)),
            Item::Text { .. } => None,
        })
        .collect()
}

/// Document chunks as text items numbered from `first`.
fn text_items(chunks: Vec<Chunk>, first: u32) -> Vec<Item> {
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| Item::Text { chunk: first + i as u32, text: c.text, locator: c.locator })
        .collect()
}

/// The FTS rows of a file: its text chunks.
fn item_texts(items: &[Item]) -> Vec<(u32, String)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Text { chunk, text, .. } => Some((*chunk, text.clone())),
            Item::Image { .. } => None,
        })
        .collect()
}

/// The vectors a text-only file owes.
fn item_pending(items: &[Item]) -> Vec<PendingVec> {
    items
        .iter()
        .map(|i| match i {
            Item::Text { chunk, locator, .. } => PendingVec { chunk: *chunk, kind: VecKind::Text, locator: Some(locator.clone()) },
            Item::Image { chunk, locator, .. } => PendingVec { chunk: *chunk, kind: VecKind::Image, locator: Some(locator.clone()) },
        })
        .collect()
}

/// Vectors for all items. Text is all-or-nothing (a failure is returned and the caller decides);
/// a page image that cannot be embedded is left out so one poison page does not block the file.
fn embed_items(embedder: &Mutex<Box<dyn Embedder + Send>>, items: &[Item]) -> Result<Vec<ChunkVec>, EmbedError> {
    let mut out: Vec<ChunkVec> = Vec::with_capacity(items.len());
    let texts: Vec<(&u32, &String, &String)> = items
        .iter()
        .filter_map(|i| match i {
            Item::Text { chunk, text, locator } => Some((chunk, text, locator)),
            Item::Image { .. } => None,
        })
        .collect();
    let images: Vec<(&u32, &Vec<u8>, &String)> = items
        .iter()
        .filter_map(|i| match i {
            Item::Image { chunk, jpeg, locator } => Some((chunk, jpeg, locator)),
            Item::Text { .. } => None,
        })
        .collect();
    let mut e = lock(embedder);
    for group in texts.chunks(TEXT_EMBED_BATCH) {
        let docs: Vec<String> = group.iter().map(|g| g.1.clone()).collect();
        let v = e.embed_documents(&docs)?;
        if v.len() != group.len() {
            return Err(EmbedError::Other("wrong vector count".into()));
        }
        for (g, vec) in group.iter().zip(v) {
            out.push(ChunkVec { chunk: *g.0, kind: VecKind::Text, locator: Some(g.2.clone()), vec });
        }
    }
    let mut failed = 0usize;
    for group in images.chunks(IMAGE_EMBED_BATCH) {
        let jpegs: Vec<Vec<u8>> = group.iter().map(|g| g.1.clone()).collect();
        match e.embed_images(&jpegs) {
            Ok(v) if v.len() == group.len() => {
                for (g, vec) in group.iter().zip(v) {
                    out.push(ChunkVec { chunk: *g.0, kind: VecKind::Image, locator: Some(g.2.clone()), vec });
                }
                continue;
            }
            Err(EmbedError::ModelsMissing) => return Err(EmbedError::ModelsMissing),
            _ => {}
        }
        // one by one: isolate a poison page
        for g in group {
            match e.embed_images(std::slice::from_ref(g.1)) {
                Ok(mut v) if v.len() == 1 => out.push(ChunkVec { chunk: *g.0, kind: VecKind::Image, locator: Some(g.2.clone()), vec: v.remove(0) }),
                Err(EmbedError::ModelsMissing) => return Err(EmbedError::ModelsMissing),
                _ => failed += 1,
            }
        }
    }
    if failed > 0 && failed == images.len() && texts.is_empty() {
        return Err(EmbedError::Other("could not embed any page image".into()));
    }
    out.sort_by_key(|c| c.chunk);
    Ok(out)
}

enum Failure {
    ModelsMissing,
    /// The whole batch failed (backend down / bad reply).
    Systemic(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};
    use std::fs;
    use std::io::Cursor;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use winseek_store::{FileId, FileKey, IndexMeta};

    use crate::tier1::embedder::{index_meta, DIMS};

    /// Deterministic fake: vector derived from the JPEG bytes; optionally fails on marker bytes.
    struct Fake {
        calls: Arc<AtomicUsize>,
        images: Arc<AtomicUsize>,
        fail_all: bool,
        missing: bool,
    }

    impl Fake {
        fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            let images = Arc::new(AtomicUsize::new(0));
            (Fake { calls: calls.clone(), images: images.clone(), fail_all: false, missing: false }, calls, images)
        }
    }

    fn vec_of(j: &[u8]) -> Vec<f32> {
        let mut v = vec![0f32; DIMS];
        v[(j.iter().map(|&b| usize::from(b)).sum::<usize>()) % DIMS] = 1.0;
        v
    }

    impl Embedder for Fake {
        fn embed_images(&mut self, jpegs: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.missing {
                return Err(EmbedError::ModelsMissing);
            }
            if self.fail_all {
                return Err(EmbedError::Other("boom".into()));
            }
            self.images.fetch_add(jpegs.len(), Ordering::SeqCst);
            Ok(jpegs.iter().map(|j| vec_of(j)).collect())
        }
        fn embed_query(&mut self, _q: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(vec_of(b"q"))
        }
        fn meta(&self) -> IndexMeta {
            index_meta("test")
        }
    }

    fn png(w: u32, h: u32, seed: u8) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| image::Rgb([seed, (x % 251) as u8, (y % 251) as u8]));
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 255) as u8, (y % 255) as u8, 7]));
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg).unwrap();
        out
    }

    fn setup() -> (Mutex<Store>, tempfile::TempDir) {
        let store = Store::open_in_memory(&index_meta("test")).unwrap();
        (Mutex::new(store), tempfile::tempdir().unwrap())
    }

    fn add(store: &Mutex<Store>, dir: &Path, name: &str, bytes: &[u8], n: u64) -> FileId {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        let key = FileKey { vol_guid: "{v}".into(), frn: n };
        store.lock().unwrap().upsert_file(&key, &p.to_string_lossy(), bytes.len() as u64, n as i64, "image").unwrap()
    }

    fn emb(f: Fake) -> Mutex<Box<dyn Embedder + Send>> {
        Mutex::new(Box::new(f))
    }

    #[test]
    fn identical_bytes_embed_once() {
        let (store, dir) = setup();
        let (fake, _calls, images) = Fake::new();
        let e = emb(fake);
        let same = png(40, 30, 1);
        add(&store, dir.path(), "a.png", &same, 1);
        add(&store, dir.path(), "b.png", &same, 2);
        add(&store, dir.path(), "c.jpg", &jpeg(50, 50), 3);
        let mut p = Processor::default();
        let r = p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap();
        assert_eq!(r, Pass::Progress(3));
        assert_eq!(images.load(Ordering::SeqCst), 2, "duplicate content must be embedded once");
        let s = store.lock().unwrap().stats().unwrap();
        assert_eq!((s.done, s.pending, s.vectors), (3, 0, 2));

        // a later duplicate (next pass) links without calling the embedder at all
        add(&store, dir.path(), "d.png", &same, 4);
        let before = images.load(Ordering::SeqCst);
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        assert_eq!(images.load(Ordering::SeqCst), before);
        assert_eq!(store.lock().unwrap().stats().unwrap().done, 4);
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Idle);
    }

    #[test]
    fn decode_failure_marks_failed_and_oversize_skipped() {
        let (store, dir) = setup();
        let (fake, _c, images) = Fake::new();
        let e = emb(fake);
        add(&store, dir.path(), "bad.png", b"this is not a png", 1);
        add(&store, dir.path(), "ok.png", &png(10, 10, 2), 2);
        // oversize: recorded size above the cap (file content irrelevant, must not even be read)
        let p = dir.path().join("huge.jpg");
        fs::write(&p, b"x").unwrap();
        store
            .lock()
            .unwrap()
            .upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 3 }, &p.to_string_lossy(), MAX_FILE_BYTES + 1, 3, "image")
            .unwrap();
        // missing file on disk
        store
            .lock()
            .unwrap()
            .upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 4 }, &dir.path().join("nope.jpg").to_string_lossy(), 5, 4, "image")
            .unwrap();
        let mut pr = Processor::default();
        assert_eq!(pr.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(4));
        let s = store.lock().unwrap().stats().unwrap();
        assert_eq!((s.done, s.failed, s.skipped, s.pending), (1, 2, 1, 0));
        assert_eq!(images.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pause_flag_stops_work_and_models_missing_is_reported() {
        let (store, dir) = setup();
        let (mut fake, calls, _i) = Fake::new();
        add(&store, dir.path(), "a.png", &png(8, 8, 1), 1);
        let mut pr = Processor::default();
        let paused = AtomicBool::new(true);
        let e = emb(Fake { calls: calls.clone(), images: Arc::new(AtomicUsize::new(0)), fail_all: false, missing: false });
        assert_eq!(pr.run_pass(&store, &e, &paused).unwrap(), Pass::Paused);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(store.lock().unwrap().stats().unwrap().pending, 1);

        fake.missing = true;
        let e = emb(fake);
        assert_eq!(pr.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::ModelsMissing);
        assert_eq!(store.lock().unwrap().stats().unwrap().pending, 1, "file stays queued");
    }

    struct StubMedia(bool);
    impl MediaTool for StubMedia {
        fn available(&self) -> bool {
            self.0
        }
        fn decode_audio(&self, _: &Path) -> Result<media::Pcm, String> {
            Ok(media::Pcm::from_bytes(vec![0; 32_000 * 40]))
        }
        fn shot_frames(&self, _: &Path, _: usize) -> Result<Vec<media::Frame>, String> {
            Ok(vec![])
        }
    }

    struct AudioOk;
    impl Embedder for AudioOk {
        fn embed_images(&mut self, j: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Ok(j.iter().map(|b| vec_of(b)).collect())
        }
        fn embed_audio(&mut self, w: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Ok(w.iter().map(|b| vec_of(&b[..64])).collect())
        }
        fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(vec_of(b"q"))
        }
        fn meta(&self) -> IndexMeta {
            index_meta("test")
        }
    }

    #[test]
    fn audio_and_video_wait_for_ffmpeg_without_blocking_images() {
        let (store, dir) = setup();
        add(&store, dir.path(), "a.png", &png(8, 8, 1), 1);
        add_kind(&store, dir.path(), "memo.mp3", b"audio-bytes", 2, "audio");
        add_kind(&store, dir.path(), "clip.mp4", b"video-bytes", 3, "video");
        let e: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(AudioOk));
        let go = AtomicBool::new(false);

        let mut pr = Processor::default().with_media(Box::new(StubMedia(false)));
        assert!(pr.has_index_work(&store), "the image is work");
        assert_eq!(pr.run_pass(&store, &e, &go).unwrap(), Pass::Progress(1), "only the image: media stays queued");
        assert_eq!(store.lock().unwrap().stats().unwrap().pending, 2);
        assert!(!pr.has_index_work(&store), "queued media is not work while ffmpeg is missing");
        assert_eq!(pr.run_fts_pass(&store, &go).unwrap(), Pass::Idle, "the FTS-only pass leaves media alone");
        assert_eq!(store.lock().unwrap().stats().unwrap().pending, 2);

        let mut pr = Processor::default().with_media(Box::new(StubMedia(true)));
        assert!(pr.has_index_work(&store));
        assert_eq!(pr.run_pass(&store, &e, &go).unwrap(), Pass::Progress(2), "both media files in one pass");
        let st = store.lock().unwrap().stats().unwrap();
        assert_eq!((st.done, st.pending, st.failed), (3, 0, 0));
        assert_eq!(pr.run_pass(&store, &e, &go).unwrap(), Pass::Idle);
    }

    #[test]
    fn systemic_embed_failure_retries_then_fails_after_strikes() {
        let (store, dir) = setup();
        let (mut fake, _c, _i) = Fake::new();
        fake.fail_all = true;
        let e = emb(fake);
        add(&store, dir.path(), "a.png", &png(8, 8, 1), 1);
        let mut pr = Processor::default();
        for _ in 0..MAX_STRIKES - 1 {
            assert!(matches!(pr.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::EmbedFailed(_)));
            assert_eq!(store.lock().unwrap().stats().unwrap().pending, 1);
        }
        assert_eq!(pr.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        assert_eq!(store.lock().unwrap().stats().unwrap().failed, 1);
    }

    /// Fails any batch (or single image) containing a marker JPEG; everything else works.
    struct Poison(Vec<u8>);
    impl Embedder for Poison {
        fn embed_images(&mut self, jpegs: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            if jpegs.contains(&self.0) {
                return Err(EmbedError::Other("bad image".into()));
            }
            Ok(jpegs.iter().map(|j| vec_of(j)).collect())
        }
        fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(vec_of(b"q"))
        }
        fn meta(&self) -> IndexMeta {
            index_meta("test")
        }
    }

    #[test]
    fn poison_image_isolated_by_single_retry() {
        let (store, dir) = setup();
        let bad = png(12, 12, 9);
        let bad_jpeg = prepare_jpeg(&bad).unwrap();
        add(&store, dir.path(), "bad.png", &bad, 1);
        add(&store, dir.path(), "good1.png", &png(12, 12, 1), 2);
        add(&store, dir.path(), "good2.png", &png(12, 12, 2), 3);
        let e: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Poison(bad_jpeg)));
        let mut pr = Processor::default();
        assert_eq!(pr.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(3));
        let s = store.lock().unwrap().stats().unwrap();
        assert_eq!((s.done, s.failed), (2, 1));
    }

    /// Fake OCR returning fixed text (or unavailable); counts calls.
    struct FakeOcr {
        text: Option<&'static str>,
        calls: Arc<AtomicUsize>,
    }
    impl Ocr for FakeOcr {
        fn recognize(&mut self, _: &[u8]) -> Result<OcrText, OcrError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.text {
                Some(t) => Ok(OcrText {
                    text: t.into(),
                    lines: vec![super::super::ocr::OcrLine { text: t.into(), rect: [1, 2, 3, 4] }],
                    width: 8,
                    height: 8,
                }),
                None => Err(OcrError::Unavailable),
            }
        }
    }

    /// Embedder that supports document embedding.
    struct DocEmbedder;
    impl Embedder for DocEmbedder {
        fn embed_images(&mut self, j: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Ok(j.iter().map(|_| vec_of(b"img")).collect())
        }
        fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(vec_of(b"q"))
        }
        fn embed_documents(&mut self, t: &[String]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Ok(t.iter().map(|_| vec_of(b"doc")).collect())
        }
        fn meta(&self) -> IndexMeta {
            index_meta("test")
        }
    }

    #[test]
    fn screenshot_ocr_text_is_fts_indexed_and_embedded() {
        let (store, dir) = setup();
        let calls = Arc::new(AtomicUsize::new(0));
        let e: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(DocEmbedder));
        add(&store, dir.path(), "Screenshot 1.png", &png(8, 8, 1), 1);
        add(&store, dir.path(), "holiday.png", &png(8, 8, 2), 2); // not a screenshot
        let mut p = Processor::with_ocr(Box::new(FakeOcr { text: Some("Invoice 4711 total"), calls: calls.clone() }), None);
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(2));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "only the screenshot is OCR-ed");
        let s = store.lock().unwrap();
        let hits = s.search_text(&winseek_store::fts_escape("invoice"), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("Screenshot 1.png") && hits[0].snippet.contains("[Invoice]"), "{:?}", hits[0].snippet);
        // image vector + OCR text vector for the screenshot, image only for the other file
        assert_eq!(s.stats().unwrap().vectors, 3);
        assert_eq!(s.get_setting(SETTING_OCR_UNAVAILABLE).unwrap(), None);
        let doc_hit = s.search_vectors(&vec_of(b"doc"), 5, Some(VecKind::Text)).unwrap();
        assert_eq!(doc_hit.len(), 1);
        assert!(doc_hit[0].locator.as_deref().unwrap().contains("\"ocr\""), "OCR text hit keeps the boxes: {:?}", doc_hit[0].locator);
        let img_hit = s.search_vectors(&vec_of(b"img"), 5, Some(VecKind::Image)).unwrap();
        let shot = img_hit.iter().find(|h| h.path.ends_with("Screenshot 1.png")).unwrap();
        assert!(shot.locator.as_deref().unwrap().contains("\"ocr\""), "bbox locator kept: {:?}", shot.locator);
    }

    #[test]
    fn ocr_unavailable_is_recorded_and_file_still_indexed() {
        let (store, dir) = setup();
        let calls = Arc::new(AtomicUsize::new(0));
        let (fake, _c, images) = Fake::new();
        let e = emb(fake);
        add(&store, dir.path(), "Screenshot 2.png", &png(8, 8, 3), 1);
        let mut p = Processor::with_ocr(Box::new(FakeOcr { text: None, calls }), None);
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        assert_eq!(images.load(Ordering::SeqCst), 1);
        let s = store.lock().unwrap();
        assert_eq!(s.stats().unwrap().done, 1);
        assert_eq!(s.get_setting(SETTING_OCR_UNAVAILABLE).unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn ocr_text_without_document_embedding_still_goes_to_fts() {
        let (store, dir) = setup();
        let (fake, _c, _i) = Fake::new(); // default embed_documents -> unsupported
        let e = emb(fake);
        add(&store, dir.path(), "Capture 3.png", &png(8, 8, 4), 1);
        let mut p = Processor::with_ocr(Box::new(FakeOcr { text: Some("hello world"), calls: Arc::new(AtomicUsize::new(0)) }), None);
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        let s = store.lock().unwrap();
        assert_eq!(s.search_text("hello", 5).unwrap().len(), 1);
        assert_eq!(s.stats().unwrap().vectors, 1, "image vector only");
    }

    /// Text-only embedder that records the batches it was asked to embed.
    struct DocsOnly {
        batches: Arc<Mutex<Vec<usize>>>,
        fail: bool,
    }
    impl Embedder for DocsOnly {
        fn embed_images(&mut self, _: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Err(EmbedError::Other("no images".into()))
        }
        fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(vec_of(b"q"))
        }
        fn embed_documents(&mut self, t: &[String]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            if self.fail {
                return Err(EmbedError::Other("down".into()));
            }
            self.batches.lock().unwrap().push(t.len());
            Ok(t.iter().map(|_| vec_of(b"doc")).collect())
        }
        fn meta(&self) -> IndexMeta {
            index_meta("test")
        }
    }

    fn add_kind(store: &Mutex<Store>, dir: &Path, name: &str, bytes: &[u8], n: u64, kind: &str) -> FileId {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        let key = FileKey { vol_guid: "{v}".into(), frn: n };
        store.lock().unwrap().upsert_file(&key, &p.to_string_lossy(), bytes.len() as u64, n as i64, kind).unwrap()
    }

    type DocsRig = (Mutex<Box<dyn Embedder + Send>>, Arc<Mutex<Vec<usize>>>);

    fn docs_embedder(fail: bool) -> DocsRig {
        let batches = Arc::new(Mutex::new(Vec::new()));
        (Mutex::new(Box::new(DocsOnly { batches: batches.clone(), fail })), batches)
    }

    #[test]
    fn office_documents_become_fts_chunks_and_text_vectors() {
        use crate::tier1::extract::tests::{docx, pptx, xlsx};
        let (store, dir) = setup();
        let (e, batches) = docs_embedder(false);
        add_kind(&store, dir.path(), "report.docx", &docx(), 1, "doc");
        add_kind(&store, dir.path(), "budget.xlsx", &xlsx(), 2, "doc");
        add_kind(&store, dir.path(), "deck.pptx", &pptx(), 3, "doc");
        add_kind(&store, dir.path(), "notes.md", b"# Plan\nship the thing\n", 4, "doc");
        let mut p = Processor::default();
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(4));
        assert_eq!(*batches.lock().unwrap(), [1, 1, 1, 1], "one short chunk per file, embedded as documents");
        let s = store.lock().unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.done, st.failed, st.skipped, st.pending, st.vectors), (4, 0, 0, 0, 4));

        let hit = |q: &str| s.search_text(&winseek_store::fts_escape(q), 10).unwrap();
        assert!(hit("quarterly")[0].path.ends_with("report.docx"));
        assert!(hit("electricity")[0].path.ends_with("budget.xlsx"));
        assert!(hit("agenda")[0].path.ends_with("deck.pptx"));
        assert!(hit("thing")[0].path.ends_with("notes.md"));
        assert!(hit("PHONETIC").is_empty());

        // vectors are text vectors with a locator
        let v = s.search_vectors(&vec_of(b"doc"), 10, Some(VecKind::Text)).unwrap();
        assert_eq!(v.len(), 4);
        let sheet = v.iter().find(|h| h.path.ends_with("budget.xlsx")).unwrap();
        let loc: serde_json::Value = serde_json::from_str(sheet.locator.as_deref().unwrap()).unwrap();
        assert_eq!(loc["sheet"], "Budget");
        assert!(loc["chars"][1].as_u64().unwrap() > 0);
        let slides = v.iter().find(|h| h.path.ends_with("deck.pptx")).unwrap();
        assert!(slides.locator.as_deref().unwrap().contains("\"slide\":1"));
    }

    #[test]
    fn long_text_is_chunked_batched_and_capped() {
        let (store, dir) = setup();
        let (e, batches) = docs_embedder(false);
        let long = "lorem ipsum dolor sit amet. ".repeat(20_000); // 560 KB
        add_kind(&store, dir.path(), "long.txt", long.as_bytes(), 1, "doc");
        let mut p = Processor::default();
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        let s = store.lock().unwrap();
        assert_eq!(s.stats().unwrap().vectors, super::super::chunk::MAX_CHUNKS as u64);
        let b = batches.lock().unwrap();
        assert_eq!(b.iter().sum::<usize>(), super::super::chunk::MAX_CHUNKS);
        assert!(b.iter().all(|&n| n <= TEXT_EMBED_BATCH));
        assert_eq!(s.search_text("lorem", 5).unwrap().len(), 1);
    }

    #[test]
    fn embed_failure_keeps_fts_only() {
        let (store, dir) = setup();
        let (e, _b) = docs_embedder(true);
        add_kind(&store, dir.path(), "a.txt", b"findable words here", 1, "doc");
        let mut p = Processor::default();
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        let s = store.lock().unwrap();
        assert_eq!((s.stats().unwrap().done, s.stats().unwrap().vectors), (1, 0));
        assert_eq!(s.search_text("findable", 5).unwrap().len(), 1);
    }

    #[test]
    fn skips_and_failures_have_reasons() {
        let (store, dir) = setup();
        let (e, batches) = docs_embedder(false);
        add_kind(&store, dir.path(), "old.doc", b"\xD0\xCF\x11\xE0", 1, "doc");
        add_kind(&store, dir.path(), "blob.txt", b"\x00\x01\x02\x03binary", 2, "doc");
        add_kind(&store, dir.path(), "empty.txt", b"   \n", 3, "doc");
        add_kind(&store, dir.path(), "broken.docx", b"PK not really", 4, "doc");
        // recorded size above the limit: must not even be read
        let big = dir.path().join("big.txt");
        fs::write(&big, b"x").unwrap();
        store
            .lock()
            .unwrap()
            .upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 5 }, &big.to_string_lossy(), extract::MAX_FILE_BYTES + 1, 5, "doc")
            .unwrap();
        store
            .lock()
            .unwrap()
            .upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 6 }, &dir.path().join("missing.txt").to_string_lossy(), 3, 6, "doc")
            .unwrap();
        let mut p = Processor::default();
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(6));
        assert!(batches.lock().unwrap().is_empty());
        let s = store.lock().unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.done, st.failed, st.skipped, st.pending), (0, 2, 4, 0));
        let reason = |n: &str| s.file_error(&dir.path().join(n).to_string_lossy()).unwrap().unwrap();
        assert_eq!(reason("old.doc"), "legacy_office");
        assert_eq!(reason("blob.txt"), "binary");
        assert_eq!(reason("empty.txt"), "no_text");
        assert_eq!(reason("big.txt"), "too_large");
        assert!(reason("broken.docx").starts_with("zip"));
        assert!(reason("missing.txt").starts_with("read"));
    }

    #[test]
    fn identical_documents_are_extracted_once() {
        let (store, dir) = setup();
        let (e, batches) = docs_embedder(false);
        add_kind(&store, dir.path(), "a.txt", b"same words", 1, "doc");
        add_kind(&store, dir.path(), "b.txt", b"same words", 2, "doc");
        let mut p = Processor::default();
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(2));
        assert_eq!(batches.lock().unwrap().len(), 1);
        let s = store.lock().unwrap();
        assert_eq!(s.stats().unwrap().done, 2);
        assert_eq!(s.search_text("same", 5).unwrap().len(), 2, "both paths hit the one stored text");
    }

    #[test]
    fn images_and_documents_mix_in_one_pass() {
        let (store, dir) = setup();
        let (fake, _c, images) = Fake::new();
        let e = emb(fake);
        add(&store, dir.path(), "a.png", &png(8, 8, 1), 1);
        add_kind(&store, dir.path(), "n.txt", b"hello docs", 2, "doc");
        let mut p = Processor::default();
        assert_eq!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::Progress(2));
        assert_eq!(images.load(Ordering::SeqCst), 1);
        let s = store.lock().unwrap();
        assert_eq!((s.stats().unwrap().done, s.stats().unwrap().vectors), (2, 1), "doc has FTS only: Fake has no document embedding");
        assert_eq!(s.search_text("hello", 5).unwrap().len(), 1);
    }


    // ------------------------------------------------------------ PDF + FTS-only mode

    use crate::tier1::pdf::testutil::{make_pdf, pdfium};

    fn test_runtime() -> Result<&'static Pdfium, PdfError> {
        pdfium().ok_or_else(|| PdfError::Unavailable("pdfium not available in this environment".into()))
    }

    fn no_pdfium() -> Result<&'static Pdfium, PdfError> {
        Err(PdfError::Unavailable("not installed".into()))
    }

    fn fake_ocr(text: Option<&'static str>) -> (Box<dyn Ocr>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (Box::new(FakeOcr { text, calls: calls.clone() }), calls)
    }

    /// Document-capable embedder that reports "models missing" for everything until `ready` is set.
    struct Switch {
        ready: Arc<AtomicBool>,
        fail_docs: bool,
    }
    impl Embedder for Switch {
        fn embed_images(&mut self, j: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            if !self.ready.load(Ordering::SeqCst) {
                return Err(EmbedError::ModelsMissing);
            }
            Ok(j.iter().map(|_| vec_of(b"img")).collect())
        }
        fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(vec_of(b"q"))
        }
        fn embed_documents(&mut self, t: &[String]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            if !self.ready.load(Ordering::SeqCst) {
                return Err(EmbedError::ModelsMissing);
            }
            if self.fail_docs {
                return Err(EmbedError::Other("server down".into()));
            }
            Ok(t.iter().map(|_| vec_of(b"doc")).collect())
        }
        fn meta(&self) -> IndexMeta {
            index_meta("test")
        }
    }

    fn doc_embedder() -> Mutex<Box<dyn Embedder + Send>> {
        Mutex::new(Box::new(DocEmbedder))
    }

    #[test]
    fn pdf_text_pages_and_scanned_pages_are_indexed() {
        let Some(rt) = pdfium() else { return };
        let (store, dir) = setup();
        let bytes = make_pdf(rt, &[Some("Quarterly invoice 4711 total due"), None, Some("Gardening tools chapter")]);
        add_kind(&store, dir.path(), "scan.pdf", &bytes, 1, "pdf");
        let (ocr, calls) = fake_ocr(Some("Scanned receipt coffee"));
        let mut p = Processor::with_ocr(ocr, None);
        p.pdf_runtime = test_runtime;
        assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "only the page without a text layer is OCR-ed");
        let s = store.lock().unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.done, st.failed, st.skipped, st.text_only), (1, 0, 0, 0));
        assert_eq!(st.vectors, 4, "3 text chunks (2 text pages + OCR) and 1 page image");
        let page_of = |q: &str| {
            let h = s.search_text(&winseek_store::fts_escape(q), 5).unwrap();
            assert_eq!(h.len(), 1, "{q}");
            let v: serde_json::Value = serde_json::from_str(h[0].locator.as_deref().unwrap()).unwrap();
            assert!(v["chars"][1].as_u64().unwrap() > 0);
            v["page"].as_u64().unwrap()
        };
        assert_eq!(page_of("invoice"), 1);
        assert_eq!(page_of("coffee"), 2, "OCR text of the scanned page is searchable");
        assert_eq!(page_of("gardening"), 3);
        let img = s.search_vectors(&vec_of(b"img"), 5, Some(VecKind::Image)).unwrap();
        assert_eq!(img.len(), 1);
        assert_eq!(serde_json::from_str::<serde_json::Value>(img[0].locator.as_deref().unwrap()).unwrap(), serde_json::json!({"page": 2}));
        let txt = s.search_vectors(&vec_of(b"doc"), 5, Some(VecKind::Text)).unwrap();
        assert_eq!(txt.len(), 1, "one hit per file");
        assert!(txt[0].locator.as_deref().unwrap().contains("\"page\""));
        assert_eq!(s.get_setting(SETTING_OCR_UNAVAILABLE).unwrap(), None);
    }

    #[test]
    fn pdf_without_text_and_without_ocr_keeps_the_page_image() {
        let Some(rt) = pdfium() else { return };
        let (store, dir) = setup();
        add_kind(&store, dir.path(), "blank.pdf", &make_pdf(rt, &[None]), 1, "pdf");
        let (ocr, _) = fake_ocr(None); // no OCR language installed
        let mut p = Processor::with_ocr(ocr, None);
        p.pdf_runtime = test_runtime;
        assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        let s = store.lock().unwrap();
        assert_eq!((s.stats().unwrap().done, s.stats().unwrap().vectors), (1, 1));
        assert_eq!(s.get_setting(SETTING_OCR_UNAVAILABLE).unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn long_pdf_head_is_searchable_at_once_and_the_tail_follows_behind_everything() {
        let Some(rt) = pdfium() else { return };
        let (store, dir) = setup();
        let words: Vec<String> = (1..=34).map(|n| format!("pagemarker{n:02}")).collect();
        let pages: Vec<Option<&str>> = words.iter().map(|w| Some(w.as_str())).collect();
        add_kind(&store, dir.path(), "long.pdf", &make_pdf(rt, &pages), 1, "pdf");
        let (ocr, _) = fake_ocr(None);
        let mut p = Processor::with_ocr(ocr, None);
        p.pdf_runtime = test_runtime;
        let paused = AtomicBool::new(false);
        assert_eq!(p.run_pass(&store, &doc_embedder(), &paused).unwrap(), Pass::Progress(1));
        let hits = |w: &str| store.lock().unwrap().search_text(&winseek_store::fts_escape(w), 5).unwrap().len();
        assert_eq!((hits("pagemarker01"), hits("pagemarker30")), (1, 1), "the first 30 pages are searchable after one pass");
        assert_eq!((hits("pagemarker31"), hits("pagemarker34")), (0, 0), "the rest waits");
        {
            let s = store.lock().unwrap();
            assert_eq!(s.stats().unwrap().done, 1);
            assert_eq!(s.tail_counts().unwrap(), winseek_store::TailCounts { queued: 1, pages_left: 4 });
        }
        // Nothing else is pending: the tail goes now, and appends without touching the head.
        assert_eq!(p.run_pass(&store, &doc_embedder(), &paused).unwrap(), Pass::Idle);
        assert_eq!(p.run_tail_pass(&store, &doc_embedder(), &paused).unwrap(), Pass::Progress(1));
        assert_eq!((hits("pagemarker31"), hits("pagemarker34"), hits("pagemarker01")), (1, 1, 1));
        assert_eq!(p.run_tail_pass(&store, &doc_embedder(), &paused).unwrap(), Pass::Idle, "tail finished");
        let s = store.lock().unwrap();
        assert_eq!(s.tail_counts().unwrap(), winseek_store::TailCounts::default());
        let page_of = |w: &str| {
            let h = s.search_text(&winseek_store::fts_escape(w), 5).unwrap();
            serde_json::from_str::<serde_json::Value>(h[0].locator.as_deref().unwrap()).unwrap()["page"].as_u64().unwrap()
        };
        assert_eq!((page_of("pagemarker31"), page_of("pagemarker34")), (31, 34), "tail chunks keep their page numbers");
    }

    #[test]
    fn a_short_pdf_queues_no_tail_and_a_changed_file_drops_its_tail() {
        let Some(rt) = pdfium() else { return };
        let (store, dir) = setup();
        add_kind(&store, dir.path(), "short.pdf", &make_pdf(rt, &[Some("only page")]), 1, "pdf");
        let (ocr, _) = fake_ocr(None);
        let mut p = Processor::with_ocr(ocr, None);
        p.pdf_runtime = test_runtime;
        let paused = AtomicBool::new(false);
        p.run_pass(&store, &doc_embedder(), &paused).unwrap();
        assert_eq!(store.lock().unwrap().tail_counts().unwrap(), winseek_store::TailCounts::default());

        let words: Vec<String> = (1..=32).map(|n| format!("chapter{n:02}")).collect();
        let pages: Vec<Option<&str>> = words.iter().map(|w| Some(w.as_str())).collect();
        add_kind(&store, dir.path(), "long.pdf", &make_pdf(rt, &pages), 2, "pdf");
        p.run_pass(&store, &doc_embedder(), &paused).unwrap();
        assert_eq!(store.lock().unwrap().tail_counts().unwrap().queued, 1);
        std::fs::write(dir.path().join("long.pdf"), b"replaced after the head was indexed").unwrap();
        assert_eq!(p.run_tail_pass(&store, &doc_embedder(), &paused).unwrap(), Pass::Progress(1));
        let s = store.lock().unwrap();
        assert_eq!(s.tail_counts().unwrap().queued, 0, "content no longer matches: tail dropped");
        assert_eq!(s.search_text("chapter32", 5).unwrap().len(), 0);
    }

    #[test]
    fn pdf_failures_and_limits() {
        let Some(rt) = pdfium() else { return };
        let (store, dir) = setup();
        add_kind(&store, dir.path(), "broken.pdf", b"%PDF-1.4 this is not a real file", 1, "pdf");
        // recorded size above the PDF limit: skipped without reading
        let big = dir.path().join("big.pdf");
        fs::write(&big, make_pdf(rt, &[Some("hi there friend")])).unwrap();
        store
            .lock()
            .unwrap()
            .upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 2 }, &big.to_string_lossy(), pdf::MAX_FILE_BYTES + 1, 2, "pdf")
            .unwrap();
        let (ocr, _) = fake_ocr(Some("x"));
        let mut p = Processor::with_ocr(ocr, None);
        p.pdf_runtime = test_runtime;
        assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Progress(2));
        let s = store.lock().unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.failed, st.skipped, st.done), (1, 1, 0));
        assert_eq!(s.file_error(&big.to_string_lossy()).unwrap().as_deref(), Some("too_large"));
        assert!(s.file_error(&dir.path().join("broken.pdf").to_string_lossy()).unwrap().unwrap().starts_with("pdf"));
    }

    #[test]
    fn pdfs_wait_for_pdfium_and_ask_for_it_while_other_files_go_on() {
        let (store, dir) = setup();
        add_kind(&store, dir.path(), "a.pdf", b"%PDF-1.4", 1, "pdf");
        add_kind(&store, dir.path(), "n.txt", b"plain words", 2, "doc");
        let asked = Arc::new(AtomicUsize::new(0));
        let a2 = asked.clone();
        let (ocr, _) = fake_ocr(None);
        let mut p = Processor::with_ocr(ocr, None).with_pdf_requester(Box::new(move || {
            a2.fetch_add(1, Ordering::SeqCst);
        }));
        p.pdf_runtime = no_pdfium;
        p.pdf_on_disk = || false; // hermetic: the dev machine may have the real DLL installed
        assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        assert!(asked.load(Ordering::SeqCst) >= 1);
        let st = store.lock().unwrap().stats().unwrap();
        assert_eq!((st.pending, st.done), (1, 1));
        let before = asked.load(Ordering::SeqCst);
        assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Idle);
        assert!(asked.load(Ordering::SeqCst) > before, "still queued -> still asking (the downloader rate-limits itself)");
        assert_eq!(store.lock().unwrap().stats().unwrap().pending, 1);
        // no PDFs queued -> nobody is asked
        store.lock().unwrap().remove_path(&dir.path().join("a.pdf").to_string_lossy()).unwrap();
        let n = asked.load(Ordering::SeqCst);
        p.run_fts_pass(&store, &AtomicBool::new(false)).unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), n);
    }

    #[test]
    fn fts_only_pass_indexes_text_then_upgrade_adds_vectors_without_reextraction() {
        let Some(rt) = pdfium() else { return };
        let (store, dir) = setup();
        let pdf_bytes = make_pdf(rt, &[Some("Quarterly invoice 4711 total due"), None]);
        add_kind(&store, dir.path(), "report.pdf", &pdf_bytes, 1, "pdf");
        add_kind(&store, dir.path(), "notes.txt", b"meeting notes about budgets", 2, "doc");
        let shot = png(8, 8, 5);
        add(&store, dir.path(), "Screenshot 1.png", &shot, 3);
        add(&store, dir.path(), "holiday.png", &png(8, 8, 6), 4); // plain image: waits for the image model
        let (ocr, ocr_calls) = fake_ocr(Some("Invoice coffee screenshot"));
        let mut p = Processor::with_ocr(ocr, None);
        p.pdf_runtime = test_runtime;
        let paused = AtomicBool::new(false);

        // ---- no models: everything text-bearing becomes searchable, nothing is embedded
        let mut handled = 0;
        for _ in 0..5 {
            match p.run_fts_pass(&store, &paused).unwrap() {
                Pass::Progress(n) => handled += n,
                Pass::Idle => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(handled, 3);
        let ocr_before = ocr_calls.load(Ordering::SeqCst);
        assert_eq!(ocr_before, 2, "one scanned PDF page + one screenshot (the plain photo is not read)");
        {
            let s = store.lock().unwrap();
            let st = s.stats().unwrap();
            assert_eq!((st.text_only, st.pending, st.done, st.vectors), (3, 1, 0, 0));
            let hit = |q: &str| s.search_text(&winseek_store::fts_escape(q), 10).unwrap();
            assert_eq!(hit("budgets").len(), 1);
            assert_eq!(hit("invoice").len(), 2, "pdf page text and the screenshot's OCR");
            let pdf_hit = hit("quarterly");
            assert!(pdf_hit[0].locator.as_deref().unwrap().contains("\"page\":1"));
            assert!(s.search_vectors(&vec_of(b"doc"), 5, None).unwrap().is_empty(), "no vectors yet");
        }
        // an idle FTS pass does not rescan the images it already looked at
        assert_eq!(p.run_fts_pass(&store, &paused).unwrap(), Pass::Idle);
        assert_eq!(ocr_calls.load(Ordering::SeqCst), ocr_before);

        // ---- models arrive: the text file may even be gone, its stored text is what gets embedded
        fs::remove_file(dir.path().join("notes.txt")).unwrap();
        let e = doc_embedder();
        let mut up = 0;
        loop {
            match p.run_upgrade_pass(&store, &e, &paused).unwrap() {
                Pass::Progress(n) => up += n,
                Pass::Idle => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(up, 3);
        assert_eq!(ocr_calls.load(Ordering::SeqCst), ocr_before, "no second extraction / OCR");
        {
            let s = store.lock().unwrap();
            let st = s.stats().unwrap();
            assert_eq!((st.text_only, st.pending, st.done), (0, 1, 3));
            // pdf: 1 text page + 1 OCR chunk + 1 rebuilt page image; notes: 1; screenshot: image + OCR text
            assert_eq!(st.vectors, 3 + 1 + 2);
            let img = s.search_vectors(&vec_of(b"img"), 10, Some(VecKind::Image)).unwrap();
            assert_eq!(img.len(), 2);
            let pdf_img = img.iter().find(|h| h.path.ends_with("report.pdf")).unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(pdf_img.locator.as_deref().unwrap()).unwrap(), serde_json::json!({"page": 2}));
            let shot_img = img.iter().find(|h| h.path.ends_with("Screenshot 1.png")).unwrap();
            assert!(shot_img.locator.as_deref().unwrap().contains("\"ocr\""), "bbox locator survives the round trip");
            assert_eq!(s.search_text(&winseek_store::fts_escape("invoice"), 10).unwrap().len(), 2, "FTS unchanged");
        }
        assert_eq!(p.run_upgrade_pass(&store, &e, &paused).unwrap(), Pass::Idle);
        // the plain image is picked up by the normal pass
        let (fake, _c, images) = Fake::new();
        assert_eq!(p.run_pass(&store, &emb(fake), &paused).unwrap(), Pass::Progress(1));
        assert_eq!(images.load(Ordering::SeqCst), 1);
        assert_eq!(store.lock().unwrap().stats().unwrap().pending, 0);
    }

    #[test]
    fn models_missing_mid_pass_falls_back_to_text_only_and_a_changed_image_loses_only_its_image_vector() {
        let (store, dir) = setup();
        let ready = Arc::new(AtomicBool::new(false));
        let e: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Switch { ready: ready.clone(), fail_docs: false }));
        add_kind(&store, dir.path(), "a.txt", b"findable alpha words", 1, "doc");
        let (ocr, _) = fake_ocr(Some("shot words"));
        let mut p = Processor::with_ocr(ocr, None);
        let paused = AtomicBool::new(false);
        // the full pass finds the models gone: the text file is kept text-only
        assert_eq!(p.run_pass(&store, &e, &paused).unwrap(), Pass::Progress(1));
        assert_eq!(store.lock().unwrap().stats().unwrap().text_only, 1);
        // while they are missing the upgrade pass reports it and keeps the file text-only
        assert_eq!(p.run_upgrade_pass(&store, &e, &paused).unwrap(), Pass::ModelsMissing);
        assert_eq!(store.lock().unwrap().stats().unwrap().text_only, 1);
        // a screenshot stored in FTS-only mode, then modified before the models arrive
        add(&store, dir.path(), "Screenshot 9.png", &png(8, 8, 1), 2);
        assert_eq!(p.run_fts_pass(&store, &paused).unwrap(), Pass::Progress(1));
        fs::write(dir.path().join("Screenshot 9.png"), png(8, 8, 2)).unwrap();
        ready.store(true, Ordering::SeqCst);
        loop {
            match p.run_upgrade_pass(&store, &e, &paused).unwrap() {
                Pass::Progress(_) => {}
                Pass::Idle => break,
                other => panic!("{other:?}"),
            }
        }
        let s = store.lock().unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.text_only, st.done), (0, 2));
        assert_eq!(s.search_vectors(&vec_of(b"img"), 5, Some(VecKind::Image)).unwrap().len(), 0, "stale image not embedded");
        assert_eq!(st.vectors, 2, "text vectors for both files");
    }

    #[test]
    fn upgrade_embed_failures_retry_then_finish_without_vectors() {
        let (store, dir) = setup();
        let ready = Arc::new(AtomicBool::new(true));
        add_kind(&store, dir.path(), "a.txt", b"same words", 1, "doc");
        add_kind(&store, dir.path(), "b.txt", b"same words", 2, "doc"); // identical content: one embedding, two files
        let mut p = Processor::default();
        assert_eq!(p.run_fts_pass(&store, &AtomicBool::new(false)).unwrap(), Pass::Progress(2));
        assert_eq!(store.lock().unwrap().stats().unwrap().text_only, 2);
        let e: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Switch { ready, fail_docs: true }));
        let paused = AtomicBool::new(false);
        for _ in 0..UPGRADE_STRIKES - 1 {
            assert!(matches!(p.run_upgrade_pass(&store, &e, &paused).unwrap(), Pass::EmbedFailed(_)));
            assert_eq!(store.lock().unwrap().stats().unwrap().text_only, 2);
        }
        assert_eq!(p.run_upgrade_pass(&store, &e, &paused).unwrap(), Pass::Progress(1));
        let s = store.lock().unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.text_only, st.done, st.vectors), (0, 2, 0));
        assert_eq!(s.search_text("same", 5).unwrap().len(), 2, "still found by text");
    }

    #[test]
    fn pause_stops_fts_and_upgrade_passes() {
        let (store, dir) = setup();
        add_kind(&store, dir.path(), "a.txt", b"hello there", 1, "doc");
        let mut p = Processor::default();
        let paused = AtomicBool::new(true);
        assert_eq!(p.run_fts_pass(&store, &paused).unwrap(), Pass::Paused);
        assert_eq!(p.run_upgrade_pass(&store, &doc_embedder(), &paused).unwrap(), Pass::Paused);
        assert_eq!(store.lock().unwrap().stats().unwrap().pending, 1);
    }

    // ------------------------------------------------------------ routing to the document parser

    /// 250 characters of readable Latin text.
    const LONG_OCR: &str = "Quarterly report: revenue grew in every region this year, driven by the new hardware line. \
        Operating costs stayed flat while headcount rose slightly. The board approved the budget for the next fiscal year, \
        including two new offices and a larger research team.";

    /// 260 characters of Cyrillic, cleanly read: any script the OCR reads well stays.
    const RU_OCR: &str = "Это длинный русский текст Это длинный русский текст Это длинный русский текст Это длинный русский текст \
        Это длинный русский текст Это длинный русский текст Это длинный русский текст Это длинный русский текст \
        Это длинный русский текст Это длинный русский текст";

    /// One unit of OCR junk (9 characters of punctuation and symbols).
    const JUNK_OCR: &str = "·-\\|/ *_—";

    #[test]
    fn screenshots_with_poor_text_are_queued_for_the_parser() {
        let (store, dir) = setup();
        add(&store, dir.path(), "Screenshot 1.png", &png(8, 8, 1), 1);
        let junk: String = JUNK_OCR.repeat(30);
        let mut p = Processor::with_ocr(Box::new(FakeOcr { text: Some(junk.clone().leak()), calls: Arc::new(AtomicUsize::new(0)) }), None);
        assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        let jobs = store.lock().unwrap().next_parse(10).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!((jobs[0].page, jobs[0].reason.as_str()), (0, "screenshot"));
        assert!(jobs[0].path.ends_with("Screenshot 1.png"));
        assert!(junk.chars().count() >= 200, "{}", junk.chars().count());
    }

    #[test]
    fn short_or_cleanly_read_screenshots_stay_with_windows_ocr() {
        for text in ["Save changes?  OK  Cancel", LONG_OCR, RU_OCR] {
            let (store, dir) = setup();
            add(&store, dir.path(), "Screenshot 1.png", &png(8, 8, 1), 1);
            let mut p = Processor::with_ocr(Box::new(FakeOcr { text: Some(text), calls: Arc::new(AtomicUsize::new(0)) }), None);
            assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
            assert!(store.lock().unwrap().next_parse(10).unwrap().is_empty(), "{text}");
        }
    }

    #[test]
    fn fts_only_screenshots_are_queued_too() {
        let (store, dir) = setup();
        add(&store, dir.path(), "Screenshot 1.png", &png(8, 8, 1), 1);
        let junk: String = JUNK_OCR.repeat(30);
        let mut p = Processor::with_ocr(Box::new(FakeOcr { text: Some(junk.leak()), calls: Arc::new(AtomicUsize::new(0)) }), None);
        assert_eq!(p.run_fts_pass(&store, &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        assert_eq!(store.lock().unwrap().stats().unwrap().text_only, 1);
        let jobs = store.lock().unwrap().next_parse(10).unwrap();
        assert_eq!(jobs.iter().map(|j| (j.page, j.reason.as_str())).collect::<Vec<_>>(), vec![(0, "screenshot")]);
    }

    #[test]
    fn scanned_pdf_pages_are_queued_text_pages_are_not() {
        let Some(rt) = pdfium() else { return };
        let (store, dir) = setup();
        let bytes = make_pdf(rt, &[Some("Quarterly invoice 4711 total due"), None, None]);
        add_kind(&store, dir.path(), "scan.pdf", &bytes, 1, "pdf");
        let (ocr, _) = fake_ocr(Some("Scanned receipt coffee"));
        let mut p = Processor::with_ocr(ocr, None);
        p.pdf_runtime = test_runtime;
        assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        let mut jobs = store.lock().unwrap().next_parse(10).unwrap();
        jobs.sort_by_key(|j| j.page);
        assert_eq!(jobs.iter().map(|j| (j.page, j.reason.as_str())).collect::<Vec<_>>(), vec![(2, "scanned_pdf"), (3, "scanned_pdf")]);
        assert!(jobs.iter().all(|j| j.hash == sha256_hex(&bytes)));
        // the same content found again (a copy) adds nothing
        add_kind(&store, dir.path(), "copy.pdf", &bytes, 2, "pdf");
        assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        assert_eq!(store.lock().unwrap().parse_counts().unwrap().queued, 2);
    }

    /// Index server that cannot load because the parser holds the GPU.
    struct Busy;
    impl Embedder for Busy {
        fn embed_images(&mut self, _: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Err(EmbedError::Other("GPU is busy with the Parser model; image work waits for it to unload".into()))
        }
        fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(vec_of(b"q"))
        }
        fn meta(&self) -> IndexMeta {
            index_meta("test")
        }
    }

    #[test]
    fn a_busy_gpu_is_not_a_strike_against_the_file() {
        let (store, dir) = setup();
        add(&store, dir.path(), "a.png", &png(30, 30, 1), 1);
        let e: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Busy));
        let mut p = Processor::default();
        for _ in 0..MAX_STRIKES + 2 {
            assert!(matches!(p.run_pass(&store, &e, &AtomicBool::new(false)).unwrap(), Pass::EmbedFailed(m) if m.starts_with("GPU is busy")));
        }
        let st = store.lock().unwrap().stats().unwrap();
        assert_eq!((st.pending, st.failed), (1, 0), "still waiting, never failed");
        assert!(p.has_index_work(&store), "and the worker knows there is embedding work");
    }

    #[test]
    fn index_work_means_pending_files_or_text_only_files() {
        let (store, dir) = setup();
        let mut p = Processor::default();
        assert!(!p.has_index_work(&store));
        add(&store, dir.path(), "a.png", &png(8, 8, 1), 1);
        assert!(p.has_index_work(&store));
        assert_eq!(p.run_pass(&store, &doc_embedder(), &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        assert!(!p.has_index_work(&store), "done");
        add_kind(&store, dir.path(), "a.txt", b"some words", 2, "doc");
        assert_eq!(p.run_fts_pass(&store, &AtomicBool::new(false)).unwrap(), Pass::Progress(1));
        assert!(p.has_index_work(&store), "text-only: vectors still owed");
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
