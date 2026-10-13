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
pub mod ocr;
pub mod policy;
pub mod process;
pub mod progress;
pub mod scopes;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use serde::Serialize;
use winseek_llama::Manifest;
use winseek_store::{default_ignore, fts_escape, in_scope_with, Ignore, Scopes, Stats, Store};

use embedder::{EmbedError, Embedder, LlamaEmbedder, RuntimeInfo};
use policy::PauseReason;
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
    /// Effective pause flag read by the processor: user pause or an automatic reason.
    paused: AtomicBool,
    user_paused: AtomicBool,
    /// 0 = none, else a [`PauseReason`] code (battery / fullscreen) from the policy poller.
    auto_pause: AtomicU8,
    /// Set by [`Tier1::kick`]; wakes the worker's models-missing / idle sleeps.
    wake: (Mutex<bool>, Condvar),
    state: AtomicU8,
    snap: RwLock<Snapshot>,
    gen: AtomicU64,
    last_error: Mutex<Option<String>>,
    /// Rolling completed-files rate for the status page (fed by the worker loop).
    throughput: Mutex<progress::Throughput>,
    /// Last known embedding-server state (the embedder lock is held for whole passes).
    runtime: Mutex<RuntimeInfo>,
}

/// Scopes + compiled ignore list + generation, read together so a walk never mixes versions.
#[derive(Clone)]
pub struct Snapshot {
    pub scopes: Scopes,
    pub ignore: Arc<Ignore>,
    pub gen: u64,
}

impl Snapshot {
    pub fn in_scope(&self, path: &str) -> bool {
        in_scope_with(&self.scopes, &self.ignore, path)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IgnoreDto {
    pub list: Vec<String>,
    pub defaults: Vec<String>,
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
    /// Why indexing is paused: "user" | "battery" | "fullscreen" | null.
    pub paused_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticHit {
    pub path: String,
    pub score: f32,
    /// On-disk size in bytes (None when the file cannot be stat-ed).
    pub size: Option<u64>,
    /// Modified time, unix ms.
    pub mtime: Option<i64>,
    /// Extracted text (OCR / PDF / text file) around the query words, matches wrapped in `[ ]`.
    pub snippet: Option<String>,
}

/// Size + mtime for the UI's date / size chips.
fn stat(path: &str) -> (Option<u64>, Option<i64>) {
    match std::fs::metadata(path) {
        Ok(m) => (
            m.is_file().then_some(m.len()),
            m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64),
        ),
        Err(_) => (None, None),
    }
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
        let ignore = Arc::new(Ignore::new(&store.get_ignore().unwrap_or_else(|_| default_ignore())));
        Arc::new(Tier1 {
            store: Mutex::new(store),
            embedder: Mutex::new(embedder),
            paused: AtomicBool::new(false),
            user_paused: AtomicBool::new(false),
            auto_pause: AtomicU8::new(0),
            wake: (Mutex::new(false), Condvar::new()),
            state: AtomicU8::new(ST_IDLE),
            snap: RwLock::new(Snapshot { scopes, ignore, gen: 1 }),
            gen: AtomicU64::new(1),
            last_error: Mutex::new(None),
            throughput: Mutex::new(progress::Throughput::default()),
            runtime: Mutex::new(RuntimeInfo::default()),
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

    pub fn snapshot(&self) -> Snapshot {
        self.snap.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn apply_ops(&self, ops: &[feed::StoreOp]) -> usize {
        feed::real_apply(self, ops)
    }

    pub fn set_scopes(&self, include: Vec<String>, exclude: Vec<String>) -> Result<(), String> {
        let s = Scopes { include, exclude };
        lock(&self.store).set_scopes(&s).map_err(|e| e.to_string())?;
        let mut snap = self.snap.write().unwrap_or_else(|e| e.into_inner());
        snap.scopes = s;
        snap.gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(())
    }

    /// The current ignore list plus the built-in defaults (for "Reset to defaults" diffing).
    pub fn get_ignore(&self) -> Result<IgnoreDto, String> {
        let list = lock(&self.store).get_ignore().map_err(|e| e.to_string())?;
        Ok(IgnoreDto { list, defaults: default_ignore() })
    }

    /// Save the ignore list (`None` = back to defaults) and make the crawler re-run.
    pub fn set_ignore(&self, list: Option<Vec<String>>) -> Result<IgnoreDto, String> {
        let mut store = lock(&self.store);
        store.set_ignore(list.as_deref()).map_err(|e| e.to_string())?;
        let saved = store.get_ignore().map_err(|e| e.to_string())?;
        drop(store);
        let mut snap = self.snap.write().unwrap_or_else(|e| e.into_inner());
        snap.ignore = Arc::new(Ignore::new(&saved));
        snap.gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(IgnoreDto { list: saved, defaults: default_ignore() })
    }

    /// The user's pause (tray / `tier1_pause`). It wins over every automatic reason.
    pub fn set_paused(&self, p: bool) {
        self.user_paused.store(p, Ordering::SeqCst);
        self.recompute_pause();
    }

    /// Automatic pause from the policy poller (`None` resumes, unless the user paused).
    pub fn set_auto_pause(&self, r: Option<PauseReason>) {
        self.auto_pause.store(r.map_or(0, PauseReason::code), Ordering::SeqCst);
        self.recompute_pause();
    }

    fn recompute_pause(&self) {
        self.paused.store(self.paused_reason().is_some(), Ordering::SeqCst);
    }

    pub fn paused_reason(&self) -> Option<PauseReason> {
        policy::combine(self.user_paused.load(Ordering::SeqCst), PauseReason::from_code(self.auto_pause.load(Ordering::SeqCst)))
    }

    /// Wake the worker now (e.g. after the models finished downloading).
    pub fn kick(&self) {
        *lock(&self.wake.0) = true;
        self.wake.1.notify_all();
    }

    /// Sleep up to `d`, returning early on [`kick`](Self::kick).
    fn nap(&self, d: Duration) {
        let mut kicked = lock(&self.wake.0);
        if !*kicked {
            kicked = self.wake.1.wait_timeout_while(kicked, d, |k| !*k).unwrap_or_else(|e| e.into_inner()).0;
        }
        *kicked = false;
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
        let scopes = self.snapshot().scopes;
        StatusDto {
            state: self.state_str().into(),
            stats: lock(&self.store).stats().ok().map(Into::into),
            include: scopes.include,
            exclude: scopes.exclude,
            error: lock(&self.last_error).clone(),
            paused_reason: self.paused_reason().map(PauseReason::as_str),
        }
    }

    /// Files per second over the last minute, `None` when idle or unknown.
    pub fn rate(&self) -> Option<f64> {
        lock(&self.throughput).rate(std::time::Instant::now())
    }

    /// Embedding-server state without waiting for a running pass: while the embedder is busy
    /// the server is by definition up, so the last known port is reused.
    pub fn runtime_info(&self) -> RuntimeInfo {
        match self.embedder.try_lock() {
            Ok(mut e) => {
                let r = e.runtime_info();
                *lock(&self.runtime) = r.clone();
                r
            }
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().runtime_info(),
            Err(std::sync::TryLockError::WouldBlock) => RuntimeInfo { running: true, unload_in_s: None, ..lock(&self.runtime).clone() },
        }
    }

    /// Swap in a freshly reset store (embeddings dropped, files back to pending) and make the
    /// crawler re-run. `reset` builds the new store; the store lock is held throughout so the
    /// worker cannot write to the old one meanwhile. Never touches Tier 0.
    pub fn rebuild_with(&self, reset: impl FnOnce() -> Result<Store, String>) -> Result<(), String> {
        {
            let mut store = lock(&self.store);
            *store = reset()?;
        }
        *lock(&self.last_error) = None;
        lock(&self.throughput).clear();
        {
            let mut snap = self.snap.write().unwrap_or_else(|e| e.into_inner());
            snap.gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        }
        self.kick();
        Ok(())
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
        let snap = self.snapshot();
        let (raw, texts) = {
            let store = lock(&self.store);
            let raw = store.search_vectors(&vec, k * 4, None).map_err(|e| e.to_string())?;
            // Words found inside the file (OCR / PDF text): a short snippet for the "why it matched" line.
            let texts = store.search_text(&fts_escape(q), k * 4).unwrap_or_default();
            (raw, texts)
        };
        let snippets: std::collections::HashMap<String, String> =
            texts.into_iter().map(|t| (t.path.to_lowercase(), t.snippet)).collect();
        let hits = raw
            .into_iter()
            .filter(|h| snap.in_scope(&h.path))
            .take(k)
            .map(|h| {
                let (size, mtime) = stat(&h.path);
                let snippet = snippets.get(&h.path.to_lowercase()).filter(|s| s.contains('[')).cloned();
                SemanticHit { path: h.path, score: h.score, size, mtime, snippet }
            })
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
                t.nap(MODELS_POLL);
                continue;
            }
        }
        let started = std::time::Instant::now();
        let r = proc.run_pass(&t.store, &t.embedder, &t.paused);
        match r {
            Ok(Pass::Progress(n)) => {
                lock(&t.throughput).record(started, std::time::Instant::now(), n);
                t.state.store(ST_INDEXING, Ordering::Relaxed);
                *lock(&t.last_error) = None;
            }
            Ok(Pass::Idle) | Ok(Pass::Paused) => {
                t.state.store(ST_IDLE, Ordering::Relaxed);
                t.nap(IDLE_POLL);
            }
            Ok(Pass::ModelsMissing) => {
                t.state.store(ST_MODELS_MISSING, Ordering::Relaxed);
                t.nap(MODELS_POLL);
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
        let snap = t.snapshot();
        if snap.gen != done_gen {
            crawler::crawl(&t.store, &snap.scopes, &snap.ignore, &|| t.snapshot().gen != snap.gen);
            // An aborted crawl (scopes changed mid-way) is redone on the next iteration.
            done_gen = snap.gen;
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
    policy::spawn(t.clone()); // pause on battery / full-screen, resume by itself
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
        Err(e) => StatusDto { state: "unavailable".into(), stats: None, include: vec![], exclude: vec![], error: Some(e), paused_reason: None },
    }
}

#[tauri::command]
pub fn tier1_set_scopes(h: tauri::State<'_, Handle>, include: Vec<String>, exclude: Vec<String>) -> Result<(), String> {
    h.get()?.set_scopes(include, exclude)
}

#[tauri::command]
pub fn tier1_get_ignore(h: tauri::State<'_, Handle>) -> Result<IgnoreDto, String> {
    h.get()?.get_ignore()
}

/// `None` resets to the built-in defaults. Returns the saved list.
#[tauri::command]
pub fn tier1_set_ignore(h: tauri::State<'_, Handle>, list: Option<Vec<String>>) -> Result<IgnoreDto, String> {
    h.get()?.set_ignore(list)
}

#[tauri::command]
pub fn tier1_pause(app: tauri::AppHandle, h: tauri::State<'_, Handle>, paused: bool) -> Result<(), String> {
    h.get()?.set_paused(paused);
    crate::watcher::refresh_pause_item(&app);
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
    fn semantic_hits_carry_ocr_snippet_and_stat() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        {
            let mut s = lock(&t.store);
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 9 }, r"C:\Pics\shot.png", 1, 1, "image").unwrap();
            let v = [ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec: unit(1) }];
            s.complete(id, "h9", &v, &[(0, "Invoice 2025 total due".into())]).unwrap();
        }
        add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        let r = t.semantic_search("cat", 5).unwrap();
        assert!(r.hits.iter().all(|h| h.snippet.is_none()), "no text matches 'cat'");
        assert!(r.hits.iter().all(|h| h.size.is_none() && h.mtime.is_none()), "files do not exist on disk");
        // A query that the stub embeds the same way and whose word is in the OCR text.
        let r = t.semantic_search("invoice", 5).unwrap();
        let shot = r.hits.iter().find(|h| h.path.ends_with("shot.png")).expect("hit");
        assert!(shot.snippet.as_deref().unwrap().contains("[Invoice]"), "{:?}", shot.snippet);
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
        let g0 = t.snapshot().gen;
        t.set_scopes(vec![r"D:\x".into()], vec![r"D:\x\y".into()]).unwrap();
        let st = t.status();
        assert_eq!((st.include, st.exclude), (vec![r"D:\x".to_string()], vec![r"D:\x\y".to_string()]));
        assert!(t.snapshot().gen > g0);
        assert_eq!(lock(&t.store).get_scopes().unwrap().include, vec![r"D:\x".to_string()]);
    }

    #[test]
    fn user_pause_wins_and_auto_pause_resumes() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        t.set_auto_pause(Some(PauseReason::Battery));
        assert_eq!((t.status().state.as_str(), t.status().paused_reason), ("paused", Some("battery")));
        assert!(t.paused.load(Ordering::Relaxed));
        t.set_paused(true);
        assert_eq!(t.status().paused_reason, Some("user"));
        t.set_auto_pause(None); // user pause stays
        assert!(t.paused.load(Ordering::Relaxed));
        assert_eq!(t.status().paused_reason, Some("user"));
        t.set_paused(false);
        assert!(!t.paused.load(Ordering::Relaxed));
        assert_eq!((t.status().state.as_str(), t.status().paused_reason), ("idle", None));
    }

    #[test]
    fn rebuild_swaps_the_store_and_restarts_the_crawl() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        assert_eq!(lock(&t.store).stats().unwrap().done, 1);
        *lock(&t.last_error) = Some("old".into());
        let g0 = t.snapshot().gen;
        t.rebuild_with(|| Store::open_in_memory(&index_meta("test")).map_err(|e| e.to_string())).unwrap();
        assert_eq!(lock(&t.store).stats().unwrap().files, 0);
        assert!(t.snapshot().gen > g0); // the crawler sees a new generation and walks the scopes again
        assert_eq!(t.status().error, None);
        // a failing reset leaves the current store alone
        add_done(&t, 2, r"C:\Pics\dog.jpg", 2);
        assert_eq!(t.rebuild_with(|| Err("boom".into())), Err("boom".to_string()));
        assert_eq!(lock(&t.store).stats().unwrap().done, 1);
    }

    #[test]
    fn runtime_info_reports_busy_embedder_as_running() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        assert_eq!(t.runtime_info(), RuntimeInfo::default());
        let held = lock(&t.embedder); // a pass in progress: the server must be up
        assert!(t.runtime_info().running);
        drop(held);
    }

    #[test]
    fn kick_wakes_nap_early() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        let t2 = t.clone();
        let h = std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            t2.nap(Duration::from_secs(30));
            t0.elapsed()
        });
        std::thread::sleep(Duration::from_millis(50));
        t.kick();
        assert!(h.join().unwrap() < Duration::from_secs(5));
        // a kick before the nap is not lost
        t.kick();
        let t0 = std::time::Instant::now();
        t.nap(Duration::from_secs(30));
        assert!(t0.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn end_to_end_crawl_then_process() {
        let dir = test_tmp();
        let img = image::RgbImage::from_pixel(20, 10, image::Rgb([1, 2, 3]));
        img.save(dir.path().join("a.png")).unwrap();
        let t = tier(Box::new(Q), &[&dir.path().to_string_lossy()]);
        let snap = t.snapshot();
        assert_eq!(crawler::crawl(&t.store, &snap.scopes, &snap.ignore, &|| false).upserted, 1);
        let mut p = Processor::default();
        assert_eq!(p.run_pass(&t.store, &t.embedder, &t.paused).unwrap(), Pass::Progress(1));
        assert_eq!(lock(&t.store).stats().unwrap().done, 1);
    }
}
