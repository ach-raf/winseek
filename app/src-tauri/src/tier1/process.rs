//! The processing pass: pending files -> hash -> (link | decode, resize, JPEG, embed) -> store.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use winseek_llama::prep::prepare_jpeg;
use winseek_store::{ChunkVec, PendingFile, Store, VecKind};

use super::embedder::{EmbedError, Embedder};
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
                        // Line rectangles ride on the image vector's locator (preview bbox highlight).
                        chunks[0].locator = Some(t.locator_json());
                        texts.push((OCR_CHUNK, t.text.clone()));
                        if let Some(v) = text_vecs.get(&p.slot) {
                            chunks.push(ChunkVec { chunk: OCR_CHUNK, kind: VecKind::Text, locator: None, vec: v.clone() });
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

    #[test]
    fn sha256_known_vector() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
