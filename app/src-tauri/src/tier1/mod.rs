//! Tier 1 worker: semantic image indexing, run as the user inside the app (never in searchd).
//!
//! Threads (all below-normal priority):
//! - crawler: walks the include roots at start and whenever the scopes change;
//! - change feed: searchd's USN-derived events for those roots, reconnecting with backoff;
//! - processor: pending files -> hash -> decode/resize -> embed -> store.

pub mod crawler;
pub mod embedder;
pub mod feed;
pub mod identity;
pub mod process;
pub mod scopes;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::Serialize;
use winseek_llama::Manifest;
use winseek_store::{in_scope, Scopes, Stats, Store};

use embedder::{EmbedError, Embedder, LlamaEmbedder};
use process::{Pass, Processor};

const ST_MODELS_MISSING: u8 = 0;
const ST_IDLE: u8 = 1;
const ST_INDEXING: u8 = 2;

/// How often a worker without models re-checks the disk.
pub const MODELS_POLL: Duration = Duration::from_secs(30);
const IDLE_POLL: Duration = Duration::from_secs(1);
const ERROR_BACKOFF: Duration = Duration::from_secs(30);

pub struct Tier1 {
    pub store: Mutex<Store>,
    embedder: Mutex<Box<dyn Embedder + Send>>,
    paused: AtomicBool,
    state: AtomicU8,
    scopes: RwLock<(Scopes, u64)>,
    gen: AtomicU64,
    last_error: Mutex<Option<String>>,
}

/// Tauri managed state: the worker, or why it could not start.
pub struct Handle {
    inner: Result<Arc<Tier1>, String>,
}

impl Handle {
    pub fn get(&self) -> Result<Arc<Tier1>, String> {
        self.inner.clone()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StatsDto {
    pub files: u64,
    pub pending: u64,
    pub done: u64,
    pub failed: u64,
    pub skipped: u64,
    pub vectors: u64,
}

impl From<Stats> for StatsDto {
    fn from(s: Stats) -> Self {
        StatsDto { files: s.files, pending: s.pending, done: s.done, failed: s.failed, skipped: s.skipped, vectors: s.vectors }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusDto {
    /// "models_missing" | "idle" | "indexing" | "paused" | "unavailable"
    pub state: String,
    pub stats: Option<StatsDto>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticHit {
    pub path: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticResult {
    pub state: String,
    pub hits: Vec<SemanticHit>,
}

pub fn lower_thread_priority() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    // SAFETY: pseudo-handle of the current thread.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

/// `%LOCALAPPDATA%\winseek`.
pub fn data_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("winseek"))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Tier1 {
    /// Build from parts (tests inject a fake embedder and an in-memory store).
    pub fn new(store: Store, embedder: Box<dyn Embedder + Send>, scopes: Scopes) -> Arc<Tier1> {
        Arc::new(Tier1 {
            store: Mutex::new(store),
            embedder: Mutex::new(embedder),
            paused: AtomicBool::new(false),
            state: AtomicU8::new(ST_IDLE),
            scopes: RwLock::new((scopes, 1)),
            gen: AtomicU64::new(1),
            last_error: Mutex::new(None),
        })
    }

    /// Open `%LOCALAPPDATA%\winseek\tier1.sqlite` (rebuilding on a meta mismatch) with the
    /// real llama-backed embedder.
    pub fn open_default() -> Result<Arc<Tier1>, String> {
        let root = data_root().ok_or("LOCALAPPDATA is not set")?;
        std::fs::create_dir_all(&root).map_err(|e| format!("create {}: {e}", root.display()))?;
        let embedder = LlamaEmbedder::new(root.clone(), Manifest::embedded());
        let meta = embedder.meta();
        let path = root.join("tier1.sqlite");
        let store = match Store::open(&path, &meta) {
            Ok(s) => s,
            Err(winseek_store::Error::MetaMismatch { .. }) => Store::reset(&path, &meta).map_err(|e| format!("reset store: {e}"))?,
            Err(e) => return Err(format!("open store: {e}")),
        };
        let (scopes, defaulted) = scopes::effective(store.get_scopes().map_err(|e| e.to_string())?);
        let t = Tier1::new(store, Box::new(embedder), scopes.clone());
        if defaulted {
            let _ = lock(&t.store).set_scopes(&scopes);
        }
        Ok(t)
    }

    pub fn scopes_snapshot(&self) -> (Scopes, u64) {
        self.scopes.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn apply_ops(&self, ops: &[feed::StoreOp]) -> usize {
        feed::real_apply(self, ops)
    }

    pub fn set_scopes(&self, include: Vec<String>, exclude: Vec<String>) -> Result<(), String> {
        let s = Scopes { include, exclude };
        lock(&self.store).set_scopes(&s).map_err(|e| e.to_string())?;
        let g = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        *self.scopes.write().unwrap_or_else(|e| e.into_inner()) = (s, g);
        Ok(())
    }

    pub fn set_paused(&self, p: bool) {
        self.paused.store(p, Ordering::SeqCst);
    }

    pub fn state_str(&self) -> &'static str {
        if self.paused.load(Ordering::Relaxed) {
            return "paused";
        }
        match self.state.load(Ordering::Relaxed) {
            ST_MODELS_MISSING => "models_missing",
            ST_INDEXING => "indexing",
            _ => "idle",
        }
    }

    pub fn status(&self) -> StatusDto {
        let (scopes, _) = self.scopes_snapshot();
        StatusDto {
            state: self.state_str().into(),
            stats: lock(&self.store).stats().ok().map(Into::into),
            include: scopes.include,
            exclude: scopes.exclude,
            error: lock(&self.last_error).clone(),
        }
    }

    /// Embed the query and return the best `k` in-scope files.
    pub fn semantic_search(&self, query: &str, k: usize) -> Result<SemanticResult, String> {
        let k = k.clamp(1, 200);
        let q = query.trim();
        if q.is_empty() {
            return Ok(SemanticResult { state: self.state_str().into(), hits: vec![] });
        }
        let vec = {
            let mut e = lock(&self.embedder);
            if !e.models_present() {
                return Ok(SemanticResult { state: "models_missing".into(), hits: vec![] });
            }
            match e.embed_query(q) {
                Ok(v) => v,
                Err(EmbedError::ModelsMissing) => return Ok(SemanticResult { state: "models_missing".into(), hits: vec![] }),
                Err(EmbedError::Other(m)) => return Err(m),
            }
        };
        // Over-fetch: rows outside the current scopes (scopes shrank since indexing) are dropped.
        let (scopes, _) = self.scopes_snapshot();
        let raw = lock(&self.store).search_vectors(&vec, k * 4, None).map_err(|e| e.to_string())?;
        let hits = raw
            .into_iter()
            .filter(|h| in_scope(&scopes, &h.path))
            .take(k)
            .map(|h| SemanticHit { path: h.path, score: h.score })
            .collect();
        Ok(SemanticResult { state: self.state_str().into(), hits })
    }
}

/// The processing loop. Runs forever.
fn worker_loop(t: Arc<Tier1>) {
    lower_thread_priority();
    let mut proc = Processor::default();
    loop {
        {
            let mut e = lock(&t.embedder);
            e.tick_idle();
            if !e.models_present() {
                drop(e);
                t.state.store(ST_MODELS_MISSING, Ordering::Relaxed);
                std::thread::sleep(MODELS_POLL);
                continue;
            }
        }
        let r = proc.run_pass(&t.store, &t.embedder, &t.paused);
        match r {
            Ok(Pass::Progress(_)) => {
                t.state.store(ST_INDEXING, Ordering::Relaxed);
                *lock(&t.last_error) = None;
            }
            Ok(Pass::Idle) | Ok(Pass::Paused) => {
                t.state.store(ST_IDLE, Ordering::Relaxed);
                std::thread::sleep(IDLE_POLL);
            }
            Ok(Pass::ModelsMissing) => {
                t.state.store(ST_MODELS_MISSING, Ordering::Relaxed);
                std::thread::sleep(MODELS_POLL);
            }
            Ok(Pass::EmbedFailed(msg)) => {
                *lock(&t.last_error) = Some(msg);
                std::thread::sleep(ERROR_BACKOFF);
            }
            Err(msg) => {
                *lock(&t.last_error) = Some(msg);
                std::thread::sleep(ERROR_BACKOFF);
            }
        }
    }
}

fn crawler_loop(t: Arc<Tier1>) {
    lower_thread_priority();
    let mut done_gen = 0;
    loop {
        let (scopes, gen) = t.scopes_snapshot();
        if gen != done_gen {
            crawler::crawl(&t.store, &scopes, &|| t.scopes_snapshot().1 != gen);
            // An aborted crawl (scopes changed mid-way) is redone on the next iteration.
            done_gen = gen;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Start the three worker threads.
pub fn start(t: &Arc<Tier1>) {
    let spawn = |name: &str, f: fn(Arc<Tier1>)| {
        let t = t.clone();
        let _ = std::thread::Builder::new().name(name.into()).spawn(move || f(t));
    };
    spawn("tier1-crawler", crawler_loop);
    spawn("tier1-feed", feed::run);
    spawn("tier1-worker", worker_loop);
}

/// Open the default worker and start it. The returned handle is registered as Tauri state.
pub fn init() -> Handle {
    match Tier1::open_default() {
        Ok(t) => {
            start(&t);
            Handle { inner: Ok(t) }
        }
        Err(e) => {
            eprintln!("winseek: tier 1 unavailable: {e}");
            Handle { inner: Err(e) }
        }
    }
}

// ---------------------------------------------------------------- Tauri commands

#[tauri::command]
pub fn tier1_status(h: tauri::State<'_, Handle>) -> StatusDto {
    match h.get() {
        Ok(t) => t.status(),
        Err(e) => StatusDto { state: "unavailable".into(), stats: None, include: vec![], exclude: vec![], error: Some(e) },
    }
}

#[tauri::command]
pub fn tier1_set_scopes(h: tauri::State<'_, Handle>, include: Vec<String>, exclude: Vec<String>) -> Result<(), String> {
    h.get()?.set_scopes(include, exclude)
}

#[tauri::command]
pub fn tier1_pause(h: tauri::State<'_, Handle>, paused: bool) -> Result<(), String> {
    h.get()?.set_paused(paused);
    Ok(())
}

#[tauri::command]
pub async fn semantic_search(h: tauri::State<'_, Handle>, query: String, k: usize) -> Result<SemanticResult, String> {
    let t = h.get()?;
    tauri::async_runtime::spawn_blocking(move || t.semantic_search(&query, k)).await.map_err(|e| e.to_string())?
}

/// Temp dir outside `%LOCALAPPDATA%` (paths containing an `appdata` segment are never in scope).
#[cfg(test)]
pub(crate) fn test_tmp() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().join("target").join("tier1-tests");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new().prefix("t").tempdir_in(base).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedder::{index_meta, DIMS};
    use winseek_store::{ChunkVec, FileKey, VecKind};

    struct Q;
    impl Embedder for Q {
        fn embed_images(&mut self, j: &[Vec<u8>]) -> embedder::Result<Vec<Vec<f32>>> {
            Ok(j.iter().map(|_| unit(0)).collect())
        }
        fn embed_query(&mut self, q: &str) -> embedder::Result<Vec<f32>> {
            Ok(unit(if q == "cat" { 1 } else { 2 }))
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            index_meta("test")
        }
    }

    struct Missing;
    impl Embedder for Missing {
        fn embed_images(&mut self, _: &[Vec<u8>]) -> embedder::Result<Vec<Vec<f32>>> {
            Err(EmbedError::ModelsMissing)
        }
        fn embed_query(&mut self, _: &str) -> embedder::Result<Vec<f32>> {
            Err(EmbedError::ModelsMissing)
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            index_meta("test")
        }
        fn models_present(&self) -> bool {
            false
        }
    }

    fn unit(i: usize) -> Vec<f32> {
        let mut v = vec![0f32; DIMS];
        v[i] = 1.0;
        v
    }

    fn tier(e: Box<dyn Embedder + Send>, include: &[&str]) -> Arc<Tier1> {
        let store = Store::open_in_memory(&index_meta("test")).unwrap();
        Tier1::new(store, e, Scopes { include: include.iter().map(|s| s.to_string()).collect(), exclude: vec![] })
    }

    fn add_done(t: &Tier1, n: u64, path: &str, hot: usize) {
        let mut s = lock(&t.store);
        let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: n }, path, 1, 1, "image").unwrap();
        s.complete(id, &format!("h{n}"), &[ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec: unit(hot) }], &[]).unwrap();
    }

    #[test]
    fn semantic_search_ranks_and_filters_scope() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        add_done(&t, 2, r"C:\Pics\dog.jpg", 2);
        add_done(&t, 3, r"C:\Old\cat2.jpg", 1); // no longer in scope
        let r = t.semantic_search("cat", 5).unwrap();
        assert_eq!(r.hits.len(), 2);
        assert_eq!(r.hits[0].path, r"C:\Pics\cat.jpg");
        assert!(r.hits[0].score > 0.99 && r.hits[1].score.abs() < 1e-6);
        assert!(t.semantic_search("  ", 5).unwrap().hits.is_empty());
    }

    #[test]
    fn models_missing_state_and_empty_results() {
        let t = tier(Box::new(Missing), &[r"C:\Pics"]);
        let r = t.semantic_search("cat", 5).unwrap();
        assert_eq!(r.state, "models_missing");
        assert!(r.hits.is_empty());
    }

    #[test]
    fn status_scopes_and_pause() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        assert_eq!(t.status().state, "idle");
        t.set_paused(true);
        assert_eq!(t.status().state, "paused");
        t.set_paused(false);
        t.state.store(ST_INDEXING, Ordering::Relaxed);
        assert_eq!(t.status().state, "indexing");
        let g0 = t.scopes_snapshot().1;
        t.set_scopes(vec![r"D:\x".into()], vec![r"D:\x\y".into()]).unwrap();
        let st = t.status();
        assert_eq!((st.include, st.exclude), (vec![r"D:\x".to_string()], vec![r"D:\x\y".to_string()]));
        assert!(t.scopes_snapshot().1 > g0);
        assert_eq!(lock(&t.store).get_scopes().unwrap().include, vec![r"D:\x".to_string()]);
    }

    #[test]
    fn end_to_end_crawl_then_process() {
        let dir = test_tmp();
        let img = image::RgbImage::from_pixel(20, 10, image::Rgb([1, 2, 3]));
        img.save(dir.path().join("a.png")).unwrap();
        let t = tier(Box::new(Q), &[&dir.path().to_string_lossy()]);
        let (scopes, _) = t.scopes_snapshot();
        assert_eq!(crawler::crawl(&t.store, &scopes, &|| false).upserted, 1);
        let mut p = Processor::default();
        assert_eq!(p.run_pass(&t.store, &t.embedder, &t.paused).unwrap(), Pass::Progress(1));
        assert_eq!(lock(&t.store).stats().unwrap().done, 1);
    }
}
