//! The document-parser enrichment pass (spec 2026-10-14-p3-parser, section C).
//!
//! Runs in the worker only when the embed queue is empty, and never blocks it:
//! - the *route pass* walks image vectors newer than the stored cursor and queues the document-like
//!   ones (zero-shot against prototype queries), applying the OCR-quality gate;
//! - the *parse pass* takes up to [`PARSE_BATCH`] queued items, renders them at 1024 px and runs
//!   the parser — the first page through the GPU phase gate, the rest concurrently across the
//!   server's slots — and replaces the item's OCR text with the parsed text (FTS rows + vectors).
//!
//! Files are indexed exactly as before; this only upgrades text afterwards.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use pdfium_render::prelude::Pdfium;
use winseek_llama::prep::prepare_jpeg_long;
use winseek_llama::Parsed;
use winseek_store::{ParseChunk, ParseJob, ParseResult, Store, MAX_PARSE_ATTEMPTS, SETTING_PARSE_GATE_REEVAL, SETTING_PARSE_ROUTE_CURSOR};

use super::chunk;
use super::embedder::Embedder;
use super::ocr::{Ocr, WinOcr};
use super::parser::{parse_phased, ParseError, Parser};
use super::pdf::{self, PdfError};
use super::process::sha256_hex;
use super::route::{ocr_looks_poor, Prototypes, ROUTE_BATCH};

/// Queued items parsed per pass.
pub const PARSE_BATCH: usize = 4;
/// Text chunks kept per parsed item (same cap as a PDF page).
const PARSE_CHUNKS: usize = 8;
/// Long side of the page image handed to the parser.
pub const PARSE_LONG_SIDE: u32 = pdf::RENDER_LONG_SIDE;
/// After the prototype queries failed to embed, wait this long before trying again.
const PROTO_RETRY: Duration = Duration::from_secs(60);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn db(e: winseek_store::Error) -> String {
    e.to_string()
}

/// What preparing a job's page image came to.
enum Input {
    Jpeg(Vec<u8>),
    /// The item cannot be parsed (file gone / unreadable / page cannot be rendered).
    Fail(String),
    /// Not worth parsing any more (the file changed since it was indexed).
    Skip(&'static str),
    /// pdfium is not usable right now: leave the job queued.
    Defer,
}

pub struct ParsePass {
    ocr: Box<dyn Ocr>,
    protos: Option<Prototypes>,
    proto_retry_at: Option<Instant>,
    /// Binds / returns the process-wide pdfium (tests inject a stub).
    pdf_runtime: fn() -> Result<&'static Pdfium, PdfError>,
}

impl Default for ParsePass {
    fn default() -> Self {
        ParsePass::with_ocr(Box::new(WinOcr::new()))
    }
}

impl ParsePass {
    pub fn with_ocr(ocr: Box<dyn Ocr>) -> Self {
        ParsePass { ocr, protos: None, proto_retry_at: None, pdf_runtime: pdf::runtime }
    }

    /// One step of routing or parsing. `Ok(n)` = `n` items of work were done (queued for the parser
    /// or parsed), call again at once; `Ok(0)` = nothing to do (or the parser / embedder is not
    /// available): the worker may nap.
    pub fn run(
        &mut self,
        store: &Mutex<Store>,
        embedder: &Mutex<Box<dyn Embedder + Send>>,
        parser: &Mutex<Box<dyn Parser + Send>>,
        paused: &AtomicBool,
    ) -> Result<usize, String> {
        if paused.load(Ordering::Relaxed) || !lock(parser).present() {
            return Ok(0);
        }
        self.reeval_stale(store)?;
        let routed = self.route_pass(store, embedder, paused)?;
        if routed > 0 {
            return Ok(routed);
        }
        self.parse_jobs(store, embedder, parser, paused)
    }

    // ------------------------------------------------------------ stale queue entries

    /// One-time re-check of queue entries queued under an older routing gate: an item whose
    /// stored text already reads well by the current OCR-quality gate leaves the queue
    /// (`ocr_ok`, OCR text kept) instead of waiting for a parse that would only replace good
    /// text. Guarded by a store setting so it runs once per store; every step is an idempotent
    /// update, so an aborted run simply repeats on the next pass.
    fn reeval_stale(&mut self, store: &Mutex<Store>) -> Result<(), String> {
        if lock(store).get_setting(SETTING_PARSE_GATE_REEVAL).map_err(db)?.is_some() {
            return Ok(());
        }
        let jobs = lock(store).queued_all().map_err(db)?;
        let mut dropped = 0usize;
        for (hash, page) in &jobs {
            let text = lock(store).page_text(hash, *page).map_err(db)?;
            if !text.trim().is_empty() && !ocr_looks_poor(&text) {
                lock(store).skip_parse(hash, *page, "ocr_ok").map_err(db)?;
                dropped += 1;
            }
        }
        lock(store).set_setting(SETTING_PARSE_GATE_REEVAL, Some("1")).map_err(db)?;
        if !jobs.is_empty() {
            eprintln!(
                "winseek: tier 1: re-checked {} queued item(s) against the current OCR gate; {dropped} already have readable text and left the queue",
                jobs.len()
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------ routing

    /// Look at the next batch of image vectors; returns how many were queued for the parser.
    fn route_pass(&mut self, store: &Mutex<Store>, embedder: &Mutex<Box<dyn Embedder + Send>>, paused: &AtomicBool) -> Result<usize, String> {
        let cursor: i64 = lock(store).get_setting(SETTING_PARSE_ROUTE_CURSOR).map_err(db)?.and_then(|s| s.parse().ok()).unwrap_or(0);
        let batch = lock(store).image_vectors_after(cursor, ROUTE_BATCH).map_err(db)?;
        if batch.is_empty() {
            return Ok(0);
        }
        if self.protos.is_none() {
            if self.proto_retry_at.is_some_and(|t| Instant::now() < t) {
                return Ok(0);
            }
            match Prototypes::embed(&mut **lock(embedder)) {
                Ok(p) => self.protos = Some(p),
                Err(_) => {
                    // no embedder yet (models missing / server down): try again in a minute
                    self.proto_retry_at = Some(Instant::now() + PROTO_RETRY);
                    return Ok(0);
                }
            }
        }
        let Some(protos) = self.protos.clone() else { return Ok(0) };
        let (mut last, mut queued) = (cursor, 0usize);
        for img in &batch {
            if paused.load(Ordering::Relaxed) {
                break;
            }
            if protos.is_doc(&img.vec) {
                self.route_doc(store, embedder, &img.hash)?;
                queued += 1;
            }
            last = img.rowid;
        }
        if last != cursor {
            lock(store).set_setting(SETTING_PARSE_ROUTE_CURSOR, Some(&last.to_string())).map_err(db)?;
        }
        Ok(queued)
    }

    /// Queue one document-like image. The OCR-quality gate, not a language gate, decides whether
    /// it actually waits for the parser (the parser reads Latin, CJK and Arabic alike): text the
    /// parser cannot plausibly beat — substantial and readable, whatever the script — keeps the
    /// item out of the queue; sparse or junky text, or none, leaves it queued. An image without
    /// stored text gets Windows OCR first (cheap): if that reads the page well, the text is stored
    /// and the item is skipped (`ocr_ok`), so it is text-searchable without spending the GPU.
    fn route_doc(&mut self, store: &Mutex<Store>, embedder: &Mutex<Box<dyn Embedder + Send>>, hash: &str) -> Result<(), String> {
        let have: String = lock(store).texts_of(hash).map_err(db)?.into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n");
        lock(store).queue_parse(hash, 0, "doc_image").map_err(db)?;
        if !have.trim().is_empty() {
            if !ocr_looks_poor(&have) {
                lock(store).skip_parse(hash, 0, "ocr_ok").map_err(db)?;
            }
            return Ok(());
        }
        let Some(path) = lock(store).live_path(hash).map_err(db)? else { return Ok(()) };
        let Some(ocr) = std::fs::read(Path::new(&path)).ok().and_then(|b| self.ocr.recognize(&b).ok()) else { return Ok(()) };
        if ocr.is_empty() || ocr_looks_poor(&ocr.text) {
            return Ok(()); // nothing readable: the parser reads the page itself
        }
        let locator = ocr.locator_json();
        let result = text_result(embedder, chunk_texts(&ocr.text, 0), &locator);
        lock(store).skip_parse_replacing(hash, 0, "ocr_ok", &result).map_err(db)
    }

    // ------------------------------------------------------------ parsing

    /// Parse up to [`PARSE_BATCH`] queued items; returns how many were handled (parsed, skipped,
    /// failed, or attempted). Page images are prepared first (CPU only); the first page goes
    /// through the GPU phase gate — it may release the index model and retry, and it brings the
    /// server up — then the remaining pages are sent concurrently across the server's slots.
    fn parse_jobs(
        &mut self,
        store: &Mutex<Store>,
        embedder: &Mutex<Box<dyn Embedder + Send>>,
        parser: &Mutex<Box<dyn Parser + Send>>,
        paused: &AtomicBool,
    ) -> Result<usize, String> {
        let jobs = lock(store).next_parse(PARSE_BATCH).map_err(db)?;
        let mut n = 0usize;
        let mut pages: Vec<(&ParseJob, Vec<u8>)> = Vec::new();
        for job in &jobs {
            if paused.load(Ordering::Relaxed) {
                break;
            }
            match self.job_input(job) {
                Input::Jpeg(j) => pages.push((job, j)),
                Input::Fail(msg) => {
                    lock(store).fail_parse(&job.hash, job.page, &msg).map_err(db)?;
                    n += 1;
                }
                Input::Skip(reason) => {
                    lock(store).skip_parse(&job.hash, job.page, reason).map_err(db)?;
                    n += 1;
                }
                Input::Defer => break, // this job and the rest stay queued for when pdfium works
            }
        }
        let Some(((first, first_jpeg), rest)) = pages.split_first() else { return Ok(n) };
        match parse_phased(parser, embedder, first_jpeg) {
            Ok(parsed) => {
                finish_job(store, embedder, first, parsed)?;
                n += 1;
            }
            // Nothing is wrong with the item: the model is missing, or the GPU is still the index model's.
            Err(ParseError::ModelsMissing | ParseError::GpuBusy) => return Ok(n),
            Err(ParseError::Other(msg)) => {
                bump_attempt(store, parser, first, &msg)?;
                return Ok(n + 1); // the server was unloaded: stop this pass, it starts fresh next time
            }
        }
        if !rest.is_empty() {
            let jpegs: Vec<&[u8]> = rest.iter().map(|(_, j)| j.as_slice()).collect();
            let results = lock(parser).parse_batch(&jpegs);
            let mut retriable = false;
            for ((job, _), r) in rest.iter().zip(results) {
                match r {
                    Ok(parsed) => {
                        finish_job(store, embedder, job, parsed)?;
                        n += 1;
                    }
                    // The server went away mid-batch (crash / unloaded): the rest stay queued.
                    Err(ParseError::ModelsMissing | ParseError::GpuBusy) => break,
                    Err(ParseError::Other(msg)) => {
                        if bump_attempt(store, parser, job, &msg)? {
                            retriable = true;
                        }
                        n += 1;
                    }
                }
            }
            if retriable {
                return Ok(n);
            }
        }
        Ok(n)
    }

    /// The page image of a job: the standalone image re-read from disk, or the PDF page rendered.
    fn job_input(&self, job: &ParseJob) -> Input {
        let bytes = match std::fs::read(Path::new(&job.path)) {
            Ok(b) => b,
            Err(e) => return Input::Fail(format!("read: {e}")),
        };
        if sha256_hex(&bytes) != job.hash {
            return Input::Skip("changed");
        }
        if job.page == 0 {
            return match prepare_jpeg_long(&bytes, PARSE_LONG_SIDE) {
                Ok(j) => Input::Jpeg(j),
                Err(m) => Input::Fail(m),
            };
        }
        let Ok(rt) = (self.pdf_runtime)() else { return Input::Defer };
        match pdf::render_pages(rt, &bytes, &[job.page]) {
            Ok(mut pages) if !pages.is_empty() => Input::Jpeg(pages.remove(0).1),
            Ok(_) => Input::Fail(format!("page {} could not be rendered", job.page)),
            Err(PdfError::Unavailable(_)) => Input::Defer,
            Err(e) => Input::Fail(e.to_string()),
        }
    }
}

/// Store one parsed page: empty output keeps the OCR text (`empty`), otherwise the parsed text
/// replaces it (FTS chunks + text vectors) and its boxes land on the locator.
fn finish_job(
    store: &Mutex<Store>,
    embedder: &Mutex<Box<dyn Embedder + Send>>,
    job: &ParseJob,
    parsed: Parsed,
) -> Result<(), String> {
    let text = parsed.plain_text();
    if text.trim().is_empty() {
        lock(store).skip_parse(&job.hash, job.page, "empty").map_err(db)?; // the OCR text stays
        return Ok(());
    }
    let locator = parsed.locator_json((job.page > 0).then_some(job.page));
    let result = text_result(embedder, chunk_texts(&text, job.page), &locator);
    lock(store).finish_parse(&job.hash, job.page, &result).map_err(db)
}

/// Count a failed parse attempt; the third strikes the item out. `true` = the attempt is
/// recoverable: the server was dropped so the next pass starts fresh (a wedged server must not
/// eat the retries).
fn bump_attempt(store: &Mutex<Store>, parser: &Mutex<Box<dyn Parser + Send>>, job: &ParseJob, msg: &str) -> Result<bool, String> {
    let attempts = lock(store).bump_parse(&job.hash, job.page, msg).map_err(db)?;
    if attempts >= MAX_PARSE_ATTEMPTS {
        lock(store).fail_parse(&job.hash, job.page, msg).map_err(db)?;
        Ok(false)
    } else {
        lock(parser).unload();
        Ok(true)
    }
}

/// The text of one item as chunks (blank ones dropped).
fn chunk_texts(text: &str, page: u32) -> Vec<String> {
    chunk::chunk_page(text, page, PARSE_CHUNKS).into_iter().map(|c| c.text).filter(|t| !t.is_empty()).collect()
}

/// Text chunks with their vectors (`embed_documents` on the CPU query server; on any error the
/// vectors are left owed and the upgrade pass computes them later) as a [`ParseResult`]; the same
/// locator goes on the chunks and on the item's image / page vector.
fn text_result(embedder: &Mutex<Box<dyn Embedder + Send>>, texts: Vec<String>, locator: &str) -> ParseResult {
    let vecs: Vec<Option<Vec<f32>>> = match lock(embedder).embed_documents(&texts) {
        Ok(v) if v.len() == texts.len() => v.into_iter().map(Some).collect(),
        _ => vec![None; texts.len()],
    };
    ParseResult {
        chunks: texts.into_iter().zip(vecs).map(|(text, vec)| ParseChunk { text, locator: locator.to_string(), vec }).collect(),
        locator: locator.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use winseek_llama::{Element, Parsed};
    use winseek_store::{ChunkVec, FileKey, ParseCounts, VecKind};

    use super::super::embedder::{index_meta, EmbedError, DIMS};
    use super::super::ocr::{OcrError, OcrLine, OcrText};
    use super::super::route::DOC_PROMPTS;

    fn unit(i: usize) -> Vec<f32> {
        let mut v = vec![0f32; DIMS];
        v[i] = 1.0;
        v
    }

    /// Clean, long-enough pages (doubled past [`super::super::route::MIN_OCR_CHARS`] in the tests).
    const ARABIC_SENTENCE: &str = "هذه فاتورة صادرة عن الشركة المغربية العربية تحتوي على عدة بنود وأسعار وتاريخ الإصدار ورقم البطاقة وتفاصيل الدفع الكاملة المستحقة للفاتورة ";
    const RUSSIAN_SENTENCE: &str = "Это страница напечатанного документа с несколькими строками текста и цифрами 4711 для проверки качества распознавания ";

    /// Prompts on axes 0 (documents) / 1 (others); documents embed to axis 2; images are never embedded here.
    #[derive(Default)]
    struct Emb {
        doc_calls: Arc<AtomicUsize>,
        released: Arc<AtomicBool>,
        fail_docs: bool,
    }
    impl Embedder for Emb {
        fn embed_images(&mut self, _: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Err(EmbedError::ModelsMissing)
        }
        fn embed_query(&mut self, q: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(unit(if DOC_PROMPTS.contains(&q) { 0 } else { 1 }))
        }
        fn embed_documents(&mut self, t: &[String]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            self.doc_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_docs {
                return Err(EmbedError::Other("server down".into()));
            }
            Ok(t.iter().map(|_| unit(2)).collect())
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            index_meta("test")
        }
        fn release_gpu(&mut self) {
            self.released.store(true, Ordering::SeqCst);
        }
    }

    type Script = Arc<Mutex<Vec<Result<Parsed, ParseError>>>>;

    /// Parser answering from a script, one entry per call.
    struct Fake {
        present: bool,
        script: Script,
        calls: Arc<AtomicUsize>,
        unloads: Arc<AtomicUsize>,
    }
    impl Parser for Fake {
        fn present(&self) -> bool {
            self.present
        }
        fn parse(&mut self, _: &[u8]) -> Result<Parsed, ParseError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut s = self.script.lock().unwrap();
            if s.is_empty() {
                return Ok(invoice());
            }
            s.remove(0)
        }
        fn unload(&mut self) {
            self.unloads.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn invoice() -> Parsed {
        Parsed {
            elements: vec![
                Element { bbox: Some([100.0, 50.0, 900.0, 120.0]), category: "title".into(), text: "# Quarterly Invoice 4471".into() },
                Element {
                    bbox: Some([100.0, 200.0, 900.0, 400.0]),
                    category: "table".into(),
                    text: "<table><tr><td>Total due</td><td>1,869.00</td></tr></table>".into(),
                },
            ],
            prompt_version: "doc2json-v1".into(),
        }
    }

    struct Env {
        store: Mutex<Store>,
        embedder: Mutex<Box<dyn Embedder + Send>>,
        parser: Mutex<Box<dyn Parser + Send>>,
        dir: tempfile::TempDir,
        script: Script,
        calls: Arc<AtomicUsize>,
        unloads: Arc<AtomicUsize>,
        doc_calls: Arc<AtomicUsize>,
        released: Arc<AtomicBool>,
    }

    fn env(present: bool, fail_docs: bool) -> Env {
        let script: Script = Arc::default();
        let (calls, unloads) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let emb = Emb { fail_docs, ..Default::default() };
        let (doc_calls, released) = (emb.doc_calls.clone(), emb.released.clone());
        Env {
            store: Mutex::new(Store::open_in_memory(&index_meta("test")).unwrap()),
            embedder: Mutex::new(Box::new(emb)),
            parser: Mutex::new(Box::new(Fake { present, script: script.clone(), calls: calls.clone(), unloads: unloads.clone() })),
            dir: tempfile::tempdir().unwrap(),
            script,
            calls,
            unloads,
            doc_calls,
            released,
        }
    }

    fn png(w: u32, h: u32, seed: u8) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([seed, (x % 251) as u8, (y % 251) as u8]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    impl Env {
        /// An indexed image file whose image vector is `vec` (chunk 0) plus optional OCR text (chunk 1).
        fn add_image(&self, name: &str, seed: u8, vec: Vec<f32>, ocr_text: Option<&str>) -> String {
            let bytes = png(40, 30, seed);
            let path = self.dir.path().join(name);
            std::fs::write(&path, &bytes).unwrap();
            let hash = sha256_hex(&bytes);
            let mut s = self.store.lock().unwrap();
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: u64::from(seed) }, &path.to_string_lossy(), bytes.len() as u64, 1, "image").unwrap();
            let loc = r#"{"ocr":{"w":40,"h":30,"lines":[{"t":"old","r":[1,2,3,4]}]}}"#;
            let mut vecs = vec![ChunkVec { chunk: 0, kind: VecKind::Image, locator: ocr_text.map(|_| loc.to_string()), vec }];
            let mut texts = vec![];
            if let Some(t) = ocr_text {
                vecs.push(ChunkVec { chunk: 1, kind: VecKind::Text, locator: Some(loc.into()), vec: unit(3) });
                texts.push((1, t.to_string()));
            }
            s.complete(id, &hash, &vecs, &texts).unwrap();
            hash
        }

        fn pass(&self, p: &mut ParsePass) -> bool {
            p.run(&self.store, &self.embedder, &self.parser, &AtomicBool::new(false)).unwrap() > 0
        }

        fn counts(&self) -> ParseCounts {
            self.store.lock().unwrap().parse_counts().unwrap()
        }

        fn cursor(&self) -> Option<String> {
            self.store.lock().unwrap().get_setting(SETTING_PARSE_ROUTE_CURSOR).unwrap()
        }

        /// Hashes of the items still waiting for the parser.
        fn queued(&self) -> Vec<String> {
            self.store.lock().unwrap().next_parse(100).unwrap().into_iter().map(|j| j.hash).collect()
        }
    }

    struct FakeOcr(Option<String>);
    impl Ocr for FakeOcr {
        fn recognize(&mut self, _: &[u8]) -> Result<OcrText, OcrError> {
            match &self.0 {
                Some(t) => Ok(OcrText { text: t.clone(), lines: vec![OcrLine { text: t.clone(), rect: [1, 2, 3, 4] }], width: 40, height: 30 }),
                None => Err(OcrError::Unavailable),
            }
        }
    }

    fn pass_with(ocr: Option<String>) -> ParsePass {
        ParsePass::with_ocr(Box::new(FakeOcr(ocr)))
    }

    #[test]
    fn route_queues_document_like_images_and_advances_the_cursor() {
        let e = env(true, false);
        let doc = e.add_image("scan.png", 1, unit(0), Some("Invoice"));
        let photo = e.add_image("cat.png", 2, unit(1), None);
        let _blank = e.add_image("x.png", 3, unit(5), None);
        let mut p = pass_with(None);
        // routing found something to do; the queued item is not parsed in the same step
        assert!(e.pass(&mut p));
        assert_eq!(e.counts(), ParseCounts { queued: 1, done: 0, failed: 0, skipped: 0 });
        assert_eq!(e.queued(), vec![doc]);
        assert!(!e.queued().contains(&photo));
        assert!(e.cursor().is_some());
        assert_eq!(e.calls.load(Ordering::SeqCst), 0);
        // second step: nothing new to route, the queued job is parsed
        assert!(e.pass(&mut p));
        assert_eq!(e.calls.load(Ordering::SeqCst), 1);
        assert_eq!(e.counts(), ParseCounts { queued: 0, done: 1, failed: 0, skipped: 0 });
        // nothing left
        assert!(!e.pass(&mut p));
        assert_eq!(e.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn route_is_skipped_without_a_parser_or_while_paused() {
        let e = env(false, false);
        e.add_image("scan.png", 1, unit(0), Some("Invoice"));
        let mut p = pass_with(None);
        assert!(!e.pass(&mut p));
        assert_eq!((e.cursor(), e.counts()), (None, ParseCounts::default()), "no parser: nothing routed, the cursor waits");
        let e = env(true, false);
        e.add_image("scan.png", 1, unit(0), Some("Invoice"));
        let paused = AtomicBool::new(true);
        assert_eq!(p.run(&e.store, &e.embedder, &e.parser, &paused).unwrap(), 0);
        assert_eq!((e.cursor(), e.counts()), (None, ParseCounts::default()));
    }

    #[test]
    fn route_waits_when_the_prototypes_cannot_be_embedded() {
        struct Down;
        impl Embedder for Down {
            fn embed_images(&mut self, _: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
                Err(EmbedError::ModelsMissing)
            }
            fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
                Err(EmbedError::ModelsMissing)
            }
            fn meta(&self) -> winseek_store::IndexMeta {
                index_meta("test")
            }
        }
        let mut e = env(true, false);
        e.embedder = Mutex::new(Box::new(Down));
        e.add_image("scan.png", 1, unit(0), Some("Invoice"));
        let mut p = pass_with(None);
        assert!(!e.pass(&mut p));
        assert_eq!(e.cursor(), None, "the images are looked at once the embedder works");
        assert!(p.proto_retry_at.is_some(), "and not retried every second");
    }

    #[test]
    fn stale_queue_entries_with_readable_text_leave_the_queue_once() {
        let e = env(true, false);
        let readable = e.add_image("a.png", 1, unit(0), Some(&ARABIC_SENTENCE.repeat(2)));
        let junk = e.add_image("b.png", 2, unit(0), Some(&"·-\\|/ *_—".repeat(40)));
        // a scanned PDF whose page 2 was queued although its own text reads fine (page 1 is junk)
        let bytes = png(40, 30, 3);
        let path = e.dir.path().join("scan.pdf");
        std::fs::write(&path, &bytes).unwrap();
        let hp = sha256_hex(&bytes);
        {
            let mut s = e.store.lock().unwrap();
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 9 }, &path.to_string_lossy(), bytes.len() as u64, 1, "pdf").unwrap();
            let v = |chunk, page: u32| ChunkVec { chunk, kind: VecKind::Text, locator: Some(format!(r#"{{"page":{page}}}"#)), vec: unit(3) };
            s.complete(id, &hp, &[v(1, 1), v(3, 2)], &[(1, "junk".into()), (3, ARABIC_SENTENCE.repeat(2).into())]).unwrap();
            s.queue_parse(&readable, 0, "doc_image").unwrap();
            s.queue_parse(&junk, 0, "doc_image").unwrap();
            s.queue_parse(&hp, 2, "scanned_pdf").unwrap();
            s.queue_parse(&hp, 1, "scanned_pdf").unwrap();
        }
        let mut p = pass_with(None);
        p.reeval_stale(&e.store).unwrap();
        assert_eq!(e.counts(), ParseCounts { queued: 2, done: 0, failed: 0, skipped: 2 }, "the readable image and the readable PDF page left");
        {
            let s = e.store.lock().unwrap();
            assert!(s.next_parse(10).unwrap().iter().all(|j| j.hash == junk || (j.hash == hp && j.page == 1)), "junk text still waits");
            assert_eq!(s.texts_of(&readable).unwrap()[0].1, ARABIC_SENTENCE.repeat(2), "the kept item's text is untouched");
            assert!(s.get_setting(SETTING_PARSE_GATE_REEVAL).unwrap().is_some(), "the re-check is marked done");
        }
        // the next pass does not repeat the walk (the setting short-circuits it)
        e.store.lock().unwrap().skip_parse(&junk, 0, "empty").unwrap();
        e.store.lock().unwrap().skip_parse(&hp, 1, "empty").unwrap();
        assert_eq!(e.queued().len(), 0);
        assert_eq!(p.reeval_stale(&e.store).unwrap(), ());
        assert_eq!(e.counts().skipped, 4, "nothing changed the second time");
    }

    #[test]
    fn quality_gate_on_existing_ocr_text() {
        let arabic_page = ARABIC_SENTENCE.repeat(2); // long, clean, any script: the parser is not needed
        let junk = "·-\\|/ *_—".repeat(40); // long but garbage: worth the parser
        let e = env(true, false);
        let readable = e.add_image("a.png", 1, unit(0), Some(&arabic_page));
        let junky = e.add_image("b.png", 2, unit(0), Some(&junk));
        let short = e.add_image("c.png", 3, unit(0), Some("Invoice"));
        let mut p = pass_with(None);
        assert!(e.pass(&mut p));
        let s = e.store.lock().unwrap();
        let queued: Vec<String> = s.next_parse(10).unwrap().into_iter().map(|j| j.hash).collect();
        assert_eq!(queued.len(), 2, "the readable page stays out, junk and sparse text wait");
        assert!(queued.contains(&junky) && queued.contains(&short) && !queued.contains(&readable));
        assert_eq!(s.parse_counts().unwrap().queued, 2);
        drop(s);
        // the readable page's OCR text is untouched
        assert_eq!(e.store.lock().unwrap().texts_of(&readable).unwrap().len(), 1);
    }

    #[test]
    fn doc_image_without_text_gets_windows_ocr_and_readable_text_is_stored_without_parsing() {
        let e = env(true, false);
        let hash = e.add_image("ru.png", 1, unit(0), None);
        let russian = format!("{RUSSIAN_SENTENCE}{RUSSIAN_SENTENCE}");
        let mut p = pass_with(Some(russian.clone()));
        assert!(e.pass(&mut p));
        let s = e.store.lock().unwrap();
        assert_eq!(s.parse_counts().unwrap().queued, 0, "stored as ocr_ok, not queued");
        assert!(s.next_parse(10).unwrap().is_empty());
        assert_eq!(s.texts_of(&hash).unwrap(), vec![(1, russian.trim_end().to_string())], "chunking trims the text");
        assert_eq!(s.search_text(&winseek_store::fts_escape("напечатанного"), 5).unwrap().len(), 1, "text-searchable now");
        let hits = s.search_vectors(&unit(2), 3, Some(VecKind::Text)).unwrap();
        assert_eq!(hits.len(), 1, "with a text vector");
        assert!(hits[0].locator.as_deref().unwrap().contains("\"ocr\""));
        drop(s);
        assert_eq!(e.calls.load(Ordering::SeqCst), 0, "the parser never saw it");
    }

    #[test]
    fn doc_image_without_text_and_latin_ocr_is_queued_and_ocr_failure_does_not_block() {
        let e = env(true, false);
        let a = e.add_image("a.png", 1, unit(0), None);
        e.add_image("b.png", 2, unit(0), None);
        let mut p = pass_with(Some("Latin text here".to_string()));
        assert!(e.pass(&mut p));
        assert_eq!(e.counts().queued, 2);
        assert!(e.store.lock().unwrap().texts_of(&a).unwrap().is_empty(), "Latin: the parser will provide the text");
        // OCR unavailable: still queued
        let e2 = env(true, false);
        e2.add_image("c.png", 3, unit(0), None);
        let mut p2 = pass_with(None);
        assert!(e2.pass(&mut p2));
        assert_eq!(e2.counts().queued, 1);
    }

    #[test]
    fn parse_replaces_ocr_text_and_rewrites_the_locator() {
        let e = env(true, false);
        let hash = e.add_image("scan.png", 1, unit(0), Some("Invoice garbled text"));
        let mut p = pass_with(None);
        e.pass(&mut p); // route
        e.pass(&mut p); // parse
        let s = e.store.lock().unwrap();
        assert_eq!(s.parse_counts().unwrap(), ParseCounts { queued: 0, done: 1, failed: 0, skipped: 0 });
        let texts = s.texts_of(&hash).unwrap();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].1.contains("Quarterly Invoice 4471") && texts[0].1.contains("Total due\t1,869.00"), "{:?}", texts);
        assert_eq!(s.search_text(&winseek_store::fts_escape("garbled"), 5).unwrap().len(), 0, "the OCR text is gone");
        let hit = s.search_vectors(&unit(2), 3, Some(VecKind::Text)).unwrap();
        assert!(hit[0].locator.as_deref().unwrap().contains("\"parser\""), "text vector carries the boxes");
        let img = s.search_vectors(&unit(0), 3, Some(VecKind::Image)).unwrap();
        let loc: serde_json::Value = serde_json::from_str(img[0].locator.as_deref().unwrap()).unwrap();
        assert_eq!(loc["parser"]["v"], "doc2json-v1");
        assert_eq!(loc["parser"]["boxes"].as_array().unwrap().len(), 2);
        assert!(loc.get("page").is_none(), "standalone image: no page");
        assert_eq!(s.stats().unwrap().done, 1);
    }

    #[test]
    fn empty_parser_output_keeps_the_ocr_text() {
        let e = env(true, false);
        let hash = e.add_image("scan.png", 1, unit(0), Some("Invoice kept text"));
        e.script.lock().unwrap().push(Ok(Parsed {
            elements: vec![Element { bbox: None, category: "figure".into(), text: String::new() }],
            prompt_version: "v".into(),
        }));
        let mut p = pass_with(None);
        e.pass(&mut p);
        e.pass(&mut p);
        let s = e.store.lock().unwrap();
        assert_eq!(s.parse_counts().unwrap(), ParseCounts { queued: 0, done: 0, failed: 0, skipped: 1 }, "skipped('empty'): not queued, done or failed");
        assert_eq!(s.texts_of(&hash).unwrap(), vec![(1, "Invoice kept text".to_string())]);
    }

    #[test]
    fn embedding_failure_leaves_the_text_pending_and_searchable() {
        let e = env(true, true);
        let hash = e.add_image("scan.png", 1, unit(0), Some("old"));
        let mut p = pass_with(None);
        e.pass(&mut p);
        e.pass(&mut p);
        let s = e.store.lock().unwrap();
        assert_eq!(s.parse_counts().unwrap().done, 1);
        assert_eq!(s.search_text(&winseek_store::fts_escape("Quarterly"), 5).unwrap().len(), 1, "FTS has it");
        let pend = s.pending_vecs(&hash).unwrap();
        assert_eq!(pend.len(), 1, "the vector is owed, the upgrade pass computes it");
        assert_eq!(s.stats().unwrap().text_only, 1);
        drop(s);
        assert!(e.doc_calls.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn errors_bump_attempts_and_the_third_fails_the_item() {
        let e = env(true, false);
        e.add_image("scan.png", 1, unit(0), Some("old"));
        let mut p = pass_with(None);
        e.pass(&mut p); // route
        for _ in 0..3 {
            e.script.lock().unwrap().push(Err(ParseError::Other("boom".into())));
        }
        for i in 1..=3 {
            assert!(e.pass(&mut p), "attempt {i} counts as progress");
            let expect = if i < 3 { ParseCounts { queued: 1, done: 0, failed: 0, skipped: 0 } } else { ParseCounts { queued: 0, done: 0, failed: 1, skipped: 0 } };
            assert_eq!(e.counts(), expect, "after attempt {i}");
        }
        assert_eq!(e.unloads.load(Ordering::SeqCst), 2, "a failed attempt drops the server, the final one does not need to");
        assert!(!e.pass(&mut p));
    }

    #[test]
    fn models_missing_and_busy_gpu_leave_the_job_queued() {
        let e = env(true, false);
        e.add_image("scan.png", 1, unit(0), Some("old"));
        let mut p = pass_with(None);
        e.pass(&mut p);
        e.script.lock().unwrap().push(Err(ParseError::ModelsMissing));
        assert!(!e.pass(&mut p));
        assert_eq!(e.counts().queued, 1);
        // GpuBusy: the index server is told to let go, the page is retried once, then it is left for later
        e.script.lock().unwrap().extend([Err(ParseError::GpuBusy), Err(ParseError::GpuBusy)]);
        assert!(!e.pass(&mut p));
        assert!(e.released.load(Ordering::SeqCst), "release_gpu was called");
        assert_eq!(e.counts(), ParseCounts { queued: 1, done: 0, failed: 0, skipped: 0 }, "no attempt was counted");
        // busy once, free after the release: parsed
        e.script.lock().unwrap().push(Err(ParseError::GpuBusy));
        assert!(e.pass(&mut p));
        assert_eq!(e.counts().done, 1);
    }

    #[test]
    fn unreadable_or_changed_files_are_failed_or_skipped() {
        let e = env(true, false);
        let gone = e.add_image("gone.png", 1, unit(0), Some("old"));
        let changed = e.add_image("changed.png", 2, unit(0), Some("old"));
        let mut p = pass_with(None);
        e.pass(&mut p);
        std::fs::remove_file(e.dir.path().join("gone.png")).unwrap();
        std::fs::write(e.dir.path().join("changed.png"), png(9, 9, 77)).unwrap();
        e.pass(&mut p);
        assert_eq!(e.counts(), ParseCounts { queued: 0, done: 0, failed: 1, skipped: 1 }, "gone: failed; changed: skipped");
        assert_eq!(e.calls.load(Ordering::SeqCst), 0);
        let s = e.store.lock().unwrap();
        assert_eq!(s.texts_of(&gone).unwrap().len(), 1);
        assert_eq!(s.texts_of(&changed).unwrap().len(), 1);
    }

    #[test]
    fn paused_mid_batch_stops_cleanly() {
        let e = env(true, false);
        e.add_image("a.png", 1, unit(0), Some("old"));
        let mut p = pass_with(None);
        e.pass(&mut p); // route
        let paused = AtomicBool::new(true);
        assert_eq!(p.run(&e.store, &e.embedder, &e.parser, &paused).unwrap(), 0);
        assert_eq!(e.counts().queued, 1);
        assert_eq!(e.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn pdf_page_job_renders_and_replaces_only_that_pages_text() {
        use crate::tier1::pdf::testutil::{make_pdf, pdfium};
        fn rt() -> Result<&'static Pdfium, PdfError> {
            pdfium().ok_or_else(|| PdfError::Unavailable("pdfium not available in this environment".into()))
        }
        let Some(pd) = pdfium() else { return };
        let e = env(true, false);
        let bytes = make_pdf(pd, &[None, None]);
        let path = e.dir.path().join("scan.pdf");
        std::fs::write(&path, &bytes).unwrap();
        let hash = sha256_hex(&bytes);
        {
            let mut s = e.store.lock().unwrap();
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 1 }, &path.to_string_lossy(), bytes.len() as u64, 1, "pdf").unwrap();
            let v = |chunk, kind, page: u32, vec| ChunkVec { chunk, kind, locator: Some(format!("{{\"page\":{page}}}")), vec };
            let vecs = [
                v(0, VecKind::Image, 1, unit(0)),
                v(1, VecKind::Text, 1, unit(3)),
                v(2, VecKind::Image, 2, unit(0)),
                v(3, VecKind::Text, 2, unit(3)),
            ];
            s.complete(id, &hash, &vecs, &[(1, "page one ocr".into()), (3, "page two ocr".into())]).unwrap();
            s.queue_parse(&hash, 2, "scanned_pdf").unwrap();
        }
        let mut p = pass_with(None);
        p.pdf_runtime = rt;
        assert!(e.pass(&mut p));
        let s = e.store.lock().unwrap();
        let t = s.texts_of(&hash).unwrap();
        assert_eq!(t[0], (1, "page one ocr".to_string()), "page 1 untouched");
        assert!(t.len() == 2 && t[1].1.contains("Quarterly Invoice"), "{t:?}");
        let hit = s.search_vectors(&unit(2), 3, Some(VecKind::Text)).unwrap();
        let loc: serde_json::Value = serde_json::from_str(hit[0].locator.as_deref().unwrap()).unwrap();
        assert_eq!((loc["page"].as_u64(), loc["parser"]["v"].as_str()), (Some(2), Some("doc2json-v1")));
        assert_eq!(s.parse_counts().unwrap().done, 1);
    }
}
