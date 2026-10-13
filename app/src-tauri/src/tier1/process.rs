//! The processing pass: pending files -> hash -> (link | decode, resize, JPEG, embed) -> store.
//! Document / code files take the text route instead: extract -> chunk -> FTS rows + text vectors.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use winseek_llama::prep::prepare_jpeg;
use winseek_store::{ChunkVec, PendingFile, Store, VecKind};

use super::chunk::{self, Chunk};
use super::crawler::KIND_IMAGE;
use super::embedder::{EmbedError, Embedder};
use super::extract::{self, ExtractError};
use super::ocr::{self, Ocr, OcrError, OcrText, WinOcr};

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

/// Per-process state of the pass loop.
pub struct Processor {
    strikes: HashMap<i64, u32>,
    ocr: Box<dyn Ocr>,
    /// Screenshots folder used by detection (None = name / dimension rules only).
    screenshots_dir: Option<String>,
    /// Last value written to the `ocr_unavailable` setting.
    ocr_flag: Option<bool>,
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
        Processor { strikes: HashMap::new(), ocr, screenshots_dir, ocr_flag: None }
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
        let pending = lock(store).next_pending(BATCH).map_err(|e| e.to_string())?;
        if pending.is_empty() {
            return Ok(Pass::Idle);
        }
        let mut handled = 0usize;
        let mut prepared: Vec<Prepared> = Vec::new();
        let mut jpegs: Vec<Vec<u8>> = Vec::new();
        let mut by_hash: HashMap<String, usize> = HashMap::new();
        // OCR output per unique JPEG slot.
        let mut ocr_by_slot: HashMap<usize, OcrText> = HashMap::new();
        let mut ocr_unavailable = false;
        let mut ocr_ran = false;

        for f in pending {
            if f.kind != KIND_IMAGE {
                self.process_text(store, embedder, &f)?;
                handled += 1;
                continue;
            }
            if f.size > MAX_FILE_BYTES {
                lock(store).skip(f.id, "file larger than 64 MB").map_err(|e| e.to_string())?;
                handled += 1;
                continue;
            }
            let bytes = match std::fs::read(Path::new(&f.path)) {
                Ok(b) => b,
                Err(e) => {
                    lock(store).fail(f.id, &format!("read: {e}")).map_err(|e| e.to_string())?;
                    handled += 1;
                    continue;
                }
            };
            if bytes.len() as u64 > MAX_FILE_BYTES {
                lock(store).skip(f.id, "file larger than 64 MB").map_err(|e| e.to_string())?;
                handled += 1;
                continue;
            }
            let hash = sha256_hex(&bytes);
            let known = lock(store).has_content(&hash).map_err(|e| e.to_string())?;
            if known {
                lock(store).complete(f.id, &hash, &[], &[]).map_err(|e| e.to_string())?;
                handled += 1;
                continue;
            }
            if let Some(&slot) = by_hash.get(&hash) {
                prepared.push(Prepared { file: f, hash, slot });
                continue;
            }
            match prepare_jpeg(&bytes) {
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
                }
                Err(msg) => lock(store).fail(p.file.id, &format!("embed: {msg}")).map_err(|e| e.to_string())?,
            }
            handled += 1;
        }
        Ok(Pass::Progress(handled))
    }

    /// The text route for one document / code file: read, hash (content dedup as for images),
    /// extract, chunk, embed the chunks. FTS gets every chunk; if embedding fails the file is
    /// still stored with text only.
    fn process_text(&mut self, store: &Mutex<Store>, embedder: &Mutex<Box<dyn Embedder + Send>>, f: &PendingFile) -> Result<(), String> {
        let db = |e: winseek_store::Error| e.to_string();
        if f.size > extract::MAX_FILE_BYTES {
            return lock(store).skip(f.id, "too_large").map_err(db);
        }
        let bytes = match std::fs::read(Path::new(&f.path)) {
            Ok(b) => b,
            Err(e) => return lock(store).fail(f.id, &format!("read: {e}")).map_err(db),
        };
        if bytes.len() as u64 > extract::MAX_FILE_BYTES {
            return lock(store).skip(f.id, "too_large").map_err(db);
        }
        let hash = sha256_hex(&bytes);
        if lock(store).has_content(&hash).map_err(db)? {
            return lock(store).complete(f.id, &hash, &[], &[]).map_err(db);
        }
        let doc = match extract::extract_file(&f.path, &bytes) {
            Ok(d) => d,
            Err(ExtractError::Skip(reason)) => return lock(store).skip(f.id, reason).map_err(db),
            Err(ExtractError::Failed(msg)) => return lock(store).fail(f.id, &msg).map_err(db),
        };
        drop(bytes);
        let chunks = chunk::chunk(&doc);
        if chunks.is_empty() {
            return lock(store).skip(f.id, "no_text").map_err(db);
        }
        let vecs = Self::embed_chunks(embedder, &chunks);
        let texts: Vec<(u32, String)> = chunks.iter().enumerate().map(|(i, c)| (i as u32, c.text.clone())).collect();
        let vectors: Vec<ChunkVec> = vecs
            .into_iter()
            .zip(&chunks)
            .enumerate()
            .map(|(i, (vec, c))| ChunkVec { chunk: i as u32, kind: VecKind::Text, locator: Some(c.locator.clone()), vec })
            .collect();
        lock(store).complete(f.id, &hash, &vectors, &texts).map_err(db)
    }

    /// One text vector per chunk, or none at all when embedding is unavailable / fails part-way
    /// (all-or-nothing keeps a file's vectors consistent).
    fn embed_chunks(embedder: &Mutex<Box<dyn Embedder + Send>>, chunks: &[Chunk]) -> Vec<Vec<f32>> {
        let mut out = Vec::with_capacity(chunks.len());
        let mut e = lock(embedder);
        for group in chunks.chunks(TEXT_EMBED_BATCH) {
            let docs: Vec<String> = group.iter().map(|c| c.text.clone()).collect();
            match e.embed_documents(&docs) {
                Ok(v) if v.len() == group.len() => out.extend(v),
                _ => return Vec::new(),
            }
        }
        out
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

    #[test]
    fn sha256_known_vector() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
