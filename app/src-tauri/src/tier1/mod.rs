//! Tier 1 worker: semantic image indexing, run as the user inside the app (never in searchd).
//!
//! Threads (all below-normal priority):
//! - crawler: walks the include roots at start and whenever the scopes change;
//! - change feed: searchd's USN-derived events for those roots, reconnecting with backoff;
//! - processor: pending files -> hash -> decode/resize -> embed -> store (images), or
//!   extract text -> chunk -> FTS + text vectors (Office / text documents, opt-in source code).

pub mod chunk;
pub mod crawler;
pub mod estimate;
pub mod embedder;
pub mod extract;
pub mod feed;
pub mod gitignore;
pub mod gpu_phase;
pub mod guard;
pub mod heic;
pub mod hitbox;
pub mod identity;
pub mod imgclass;
pub mod library;
pub mod media;
pub mod ocr;
pub mod parse_pass;
pub mod parser;
pub mod pdf;
pub mod policy;
pub mod process;
pub mod provenance;
pub mod progress;
pub mod route;
pub mod scopes;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

/// The largest quick top-k. A search that asks for more (or for a later page) is "Show all":
/// every relevant file, in pages; the relevance gate, not a count, decides what is listed.
const SEM_K_SHALLOW: usize = 200;
/// Files found by their words alone that "Show all" lists (the FTS row limit; above any real index).
const TEXT_ALL: usize = 1_000_000;

use serde::Serialize;
use winseek_llama::prep::prepare_jpeg_long;
use winseek_llama::{Manifest, Profile, Table};
use winseek_store::relevance::{self, HUB_PROMPTS};
use winseek_store::{default_ignore, fts_escape, in_scope_with, Ignore, ParseCounts, Recovery, Scopes, Stats, Store};

use embedder::{EmbedError, Embedder, LlamaEmbedder, RuntimeInfo};
use hitbox::HitBox;
use parse_pass::{ParsePass, PARSE_LONG_SIDE};
use parser::{LlamaParser, Parser};
use policy::PauseReason;
use process::{Pass, Processor};

const ST_MODELS_MISSING: u8 = 0;
const ST_IDLE: u8 = 1;
const ST_INDEXING: u8 = 2;

/// How often a worker without models re-checks the disk.
pub const MODELS_POLL: Duration = Duration::from_secs(30);
const IDLE_POLL: Duration = Duration::from_secs(1);
const ERROR_BACKOFF: Duration = Duration::from_secs(30);
/// Minimum time between two integrity checks of the store after corruption errors.
const RECOVERY_INTERVAL: Duration = Duration::from_secs(600);
/// Store setting: the performance profile (`low` | `balanced` | `fast`).
pub const SETTING_PROFILE: &str = "profile";
/// On-demand parse results kept in memory (by content hash and page).
const DOC_CACHE: usize = 8;

pub struct Tier1 {
    pub store: Mutex<Store>,
    embedder: Mutex<Box<dyn Embedder + Send>>,
    /// The document parser (Infinity-Parser2). Never locked together with the embedder.
    parser: Mutex<Box<dyn Parser + Send>>,
    /// Last on-demand parse results, newest last.
    doc_cache: Mutex<Vec<((String, u32), ParsedDoc)>>,
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
    /// When the store was last checked after a corruption error (checks are not repeated sooner
    /// than [`RECOVERY_INTERVAL`]).
    last_recovery: Mutex<Option<Instant>>,
    /// Rolling completed-files rate for the status page (fed by the worker loop).
    throughput: Mutex<progress::Throughput>,
    /// Rolling parsed-pages rate for the status page (fed by the worker loop, parse passes only).
    parse_throughput: Mutex<progress::Throughput>,
    /// Last known embedding-server state (the embedder lock is held for whole passes).
    runtime: Mutex<RuntimeInfo>,
    /// Mean embedding of the generic hub prompts (relevance calibration), per query model.
    hub: Mutex<Option<(String, Vec<f32>)>>,
    /// "Show all": the full hit list of the last such query (see [`Tier1::semantic_page`]).
    sem_all: Mutex<Option<Arc<SemList>>>,
}

/// Scopes + compiled ignore list + generation, read together so a walk never mixes versions.
#[derive(Clone)]
pub struct Snapshot {
    pub scopes: Scopes,
    pub ignore: Arc<Ignore>,
    /// Opt-in: also index source-code text (setting `index_code`).
    pub index_code: bool,
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
    /// Text is searchable but the vectors wait for the models ("text searchable, waiting for models").
    pub text_only: u64,
}

impl From<Stats> for StatsDto {
    fn from(s: Stats) -> Self {
        StatsDto {
            files: s.files,
            pending: s.pending,
            done: s.done,
            failed: s.failed,
            skipped: s.skipped,
            vectors: s.vectors,
            text_only: s.text_only,
        }
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
    /// PDF page (1-based) the hit is on, from the chunk locator.
    pub page: Option<u32>,
    /// Audio / video: the moment (seconds) the matching window or frame starts; the preview seeks there.
    pub at: Option<f64>,
    /// "meaning" (vector hit) or "text" (found in the extracted text only, e.g. before the models are installed).
    pub source: &'static str,
    /// Where the query words sit on the image / PDF page (fractions 0-1), from the chunk's OCR or parser
    /// boxes; only boxes whose text contains a query word. `None` when there are none.
    pub boxes: Option<Vec<HitBox>>,
}

/// Result of the on-demand "Extract text" in the preview.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ParsedDoc {
    pub markdown: String,
    pub tables: Vec<Table>,
    pub elements_count: usize,
    /// Answered from the in-memory cache (same content and page parsed a moment ago).
    pub cached: bool,
}

/// The `page` of a chunk locator (`{"page":3,"chars":[0,90]}`).
pub fn locator_page(locator: Option<&str>) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(locator?).ok()?;
    u32::try_from(v.get("page")?.as_u64()?).ok().filter(|p| *p > 0)
}

/// The saved performance profile (`balanced` when unset or unreadable).
pub fn stored_profile(store: &Store) -> Profile {
    store.get_setting(SETTING_PROFILE).ok().flatten().and_then(|s| Profile::parse(&s)).unwrap_or_default()
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
    /// Hits in the whole list this page is cut from ("Show all" pages); else the hits returned.
    pub total: usize,
    /// More relevant files exist than were returned.
    pub more: bool,
}

impl SemanticResult {
    pub fn new(state: impl Into<String>, hits: Vec<SemanticHit>) -> Self {
        Self { state: state.into(), total: hits.len(), more: false, hits }
    }
}

/// A meaning / text hit before the per-file work (stat, highlight boxes) a returned page pays for.
struct SemRow {
    path: String,
    score: f32,
    locator: Option<String>,
    snippet: Option<String>,
    page: Option<u32>,
    source: &'static str,
}

impl SemRow {
    fn hit(&self, q: &str) -> SemanticHit {
        let (size, mtime) = stat(&self.path);
        SemanticHit {
            path: self.path.clone(),
            score: self.score,
            size,
            mtime,
            snippet: self.snippet.clone(),
            page: self.page,
            at: media::locator_time(self.locator.as_deref()),
            source: self.source,
            boxes: hitbox::hit_boxes(self.locator.as_deref(), q),
        }
    }
}

/// The whole "Show all" list of the last such query, handed out in pages.
struct SemList {
    query: String,
    state: String,
    rows: Vec<SemRow>,
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
    pub fn new(store: Store, embedder: Box<dyn Embedder + Send>, parser: Box<dyn Parser + Send>, scopes: Scopes) -> Arc<Tier1> {
        let ignore = Arc::new(Ignore::new(&store.get_ignore().unwrap_or_else(|_| default_ignore())));
        let index_code = store.get_index_code().unwrap_or(false);
        Arc::new(Tier1 {
            store: Mutex::new(store),
            embedder: Mutex::new(embedder),
            parser: Mutex::new(parser),
            doc_cache: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false),
            user_paused: AtomicBool::new(false),
            auto_pause: AtomicU8::new(0),
            wake: (Mutex::new(false), Condvar::new()),
            state: AtomicU8::new(ST_IDLE),
            snap: RwLock::new(Snapshot { scopes, ignore, index_code, gen: 1 }),
            gen: AtomicU64::new(1),
            last_error: Mutex::new(None),
            last_recovery: Mutex::new(None),
            throughput: Mutex::new(progress::Throughput::default()),
            parse_throughput: Mutex::new(progress::Throughput::default()),
            runtime: Mutex::new(RuntimeInfo::default()),
            hub: Mutex::new(None),
            sem_all: Mutex::new(None),
        })
    }

    /// Open `%LOCALAPPDATA%\winseek\tier1.sqlite` (rebuilding on a meta mismatch) with the
    /// real llama-backed embedder.
    pub fn open_default() -> Result<Arc<Tier1>, String> {
        let root = data_root().ok_or("LOCALAPPDATA is not set")?;
        std::fs::create_dir_all(&root).map_err(|e| format!("create {}: {e}", root.display()))?;
        let manifest = Manifest::embedded();
        let mut embedder = LlamaEmbedder::new(root.clone(), manifest.clone());
        let meta = embedder.meta();
        let path = root.join("tier1.sqlite");
        let store = match Store::open(&path, &meta) {
            Ok(s) => s,
            // Only the llama.cpp build differs and the index holds reference vectors: keep it open under
            // its old stamp; the worker compares the probes before indexing and rebuilds only if they moved.
            Err(winseek_store::Error::MetaMismatch { stored, .. }) if stored.same_but_build(&meta) && has_baseline(&path, &stored) => {
                Store::open(&path, &stored).map_err(|e| format!("open store: {e}"))?
            }
            Err(winseek_store::Error::MetaMismatch { .. }) => Store::reset(&path, &meta).map_err(|e| format!("reset store: {e}"))?,
            // Too damaged to open: keep what is readable, the rest is indexed again.
            Err(e) if e.is_corruption() => {
                let (s, backup, lost) = Store::salvage(&path, &meta, &Default::default()).map_err(|e| format!("salvage store: {e}"))?;
                eprintln!("winseek: tier 1 store was damaged; kept {} and re-indexing {lost:?}", backup.display());
                s
            }
            Err(e) => return Err(format!("open store: {e}")),
        };
        let (scopes, defaulted) = scopes::effective(store.get_scopes().map_err(|e| e.to_string())?);
        // The saved profile applies to both GPU models; they share one phase tracker.
        let profile = stored_profile(&store);
        embedder.set_profile(profile);
        let parser = LlamaParser::new(root, manifest, profile, embedder.gpu_phase());
        let t = Tier1::new(store, Box::new(embedder), Box::new(parser), scopes.clone());
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
        self.persist_scopes(scopes::normalize(Scopes { include, exclude, ..Default::default() }))
    }

    /// Forget a folder: drop it from the include roots and delete its file rows together with the
    /// vectors/texts only they referenced. Unlike unchecking (which keeps the data so a re-check
    /// is instant), removal is meant to be final. Returns the number of files forgotten.
    pub fn remove_folder(&self, path: &str) -> Result<usize, String> {
        let p = scopes::normalize_root(path);
        if p.is_empty() {
            return Err("remove_folder: empty path".into());
        }
        let lower = p.to_ascii_lowercase();
        let snap = self.snapshot();
        let included = snap.scopes.include.iter().any(|i| scopes::normalize_root(i).to_ascii_lowercase() == lower);
        if included {
            let include: Vec<String> =
                snap.scopes.include.iter().filter(|i| scopes::normalize_root(i).to_ascii_lowercase() != lower).cloned().collect();
            self.persist_scopes(Scopes { include, exclude: snap.scopes.exclude.clone(), ..Default::default() })?;
        }
        let n = {
            let mut store = lock(&self.store);
            let files = store.remove_under(&p).map_err(|e| e.to_string())?;
            let content = store.gc().map_err(|e| e.to_string())?;
            eprintln!("winseek: tier 1: removed {p} from the folders ({files} files, {content} content rows forgotten)");
            files
        };
        // If the folder is gone but a nested root is still included, the crawler re-runs and
        // re-queues whatever is still in scope.
        Ok(n)
    }

    /// Save the scopes and make the crawler re-run. The one mutation path for scopes: both
    /// [`Self::set_scopes`] and [`Self::remove_folder`] go through here. Any save means the
    /// user has configured the list, so an empty one is respected on restart (see
    /// [`scopes::effective`]) instead of falling back to the defaults.
    fn persist_scopes(&self, mut s: Scopes) -> Result<(), String> {
        s.configured = true;
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

    /// Whether source code is indexed too.
    pub fn index_code(&self) -> bool {
        self.snapshot().index_code
    }

    /// Switch the source-code scope. Turning it off forgets the already indexed code files; either
    /// way the crawler re-runs.
    pub fn set_index_code(&self, on: bool) -> Result<bool, String> {
        {
            let mut store = lock(&self.store);
            store.set_index_code(on).map_err(|e| e.to_string())?;
            if !on {
                store.remove_kind(crawler::KIND_CODE).map_err(|e| e.to_string())?;
            }
        }
        let mut snap = self.snap.write().unwrap_or_else(|e| e.into_inner());
        snap.index_code = on;
        snap.gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(on)
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
        let paused = self.paused_reason().is_some();
        self.paused.store(paused, Ordering::SeqCst);
        if paused {
            // Unload the GPU model now, not just stop the work. If a pass holds the embedder the
            // worker loop does it as soon as the pass returns (see `worker_loop`).
            if let Ok(mut e) = self.embedder.try_lock() {
                e.on_pause();
            }
            if let Ok(mut p) = self.parser.try_lock() {
                p.unload();
            }
        }
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

    /// Pages per second over the last minute of parsing, `None` when the parser was not recently working.
    pub fn parse_rate(&self) -> Option<f64> {
        lock(&self.parse_throughput).rate(std::time::Instant::now())
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

    /// Document-parser server state (see [`runtime_info`](Self::runtime_info): a parse in progress
    /// means the server is up).
    pub fn parser_runtime(&self) -> RuntimeInfo {
        match self.parser.try_lock() {
            Ok(mut p) => p.runtime_info(),
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().runtime_info(),
            Err(std::sync::TryLockError::WouldBlock) => RuntimeInfo { running: true, port: None, unload_in_s: None },
        }
    }

    /// The parser's model files and a llama build are on disk (a parse in progress counts as present).
    pub fn parser_present(&self) -> bool {
        match self.parser.try_lock() {
            Ok(p) => p.present(),
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().present(),
            Err(std::sync::TryLockError::WouldBlock) => true,
        }
    }

    /// Items in the parse queue: waiting / done / failed.
    pub fn parse_counts(&self) -> ParseCounts {
        lock(&self.store).parse_counts().unwrap_or_default()
    }

    pub fn profile(&self) -> Profile {
        stored_profile(&lock(&self.store))
    }

    /// Save the profile and apply it: both GPU models are dropped and use the new profile when they
    /// next load. Waits for a running pass (the embedder lock is held for whole passes), so call it
    /// off the UI thread. The two locks are taken one after the other, never together.
    pub fn set_profile(&self, p: Profile) -> Result<Profile, String> {
        lock(&self.store).set_setting(SETTING_PROFILE, Some(p.as_str())).map_err(|e| e.to_string())?;
        lock(&self.embedder).set_profile(p);
        lock(&self.parser).set_profile(p);
        Ok(p)
    }

    /// "Extract text" for one image or PDF page (preview footer): the page is rendered at 1024 px, run
    /// through the parser (loading it, and unloading the index model, as needed) and returned as
    /// markdown plus tables. Nothing is stored. The last [`DOC_CACHE`] results are kept in memory by
    /// content hash and page, so Copy table / Copy as Markdown right after do not parse again.
    pub fn parse_document(&self, path: &str, page: Option<u32>) -> Result<ParsedDoc, String> {
        use crate::preview::{classify, ext_of, Kind};
        if !self.parser_present() {
            return Err("The document parser is not installed. Add it in Setup.".into());
        }
        let (page, limit) = match classify(&ext_of(path)) {
            Kind::Image => (0, process::MAX_FILE_BYTES),
            Kind::Pdf => (page.filter(|p| *p > 0).unwrap_or(1), pdf::MAX_FILE_BYTES),
            _ => return Err("Only images and PDF pages can be read.".into()),
        };
        let md = std::fs::metadata(path).map_err(|e| format!("Cannot read file: {e}"))?;
        if md.len() > limit {
            return Err("The file is too large to read.".into());
        }
        let bytes = guard::read_guarded(std::path::Path::new(path)).map_err(|e| format!("Cannot read file: {e}"))?;
        let key = (process::sha256_hex(&bytes), page);
        if let Some(mut d) = self.cached_doc(&key) {
            d.cached = true;
            return Ok(d);
        }
        let jpeg = if page == 0 {
            let bytes = heic::normalize(path, bytes.clone()).ok_or("This HEIC photo cannot be decoded: the Windows HEIF codec is missing.")?;
            prepare_jpeg_long(&bytes, PARSE_LONG_SIDE)?
        } else {
            let rt = pdf::runtime().map_err(|e| e.to_string())?;
            let mut pages = pdf::render_pages(rt, &bytes, &[page]).map_err(|e| e.to_string())?;
            if pages.is_empty() {
                return Err(format!("Page {page} could not be rendered."));
            }
            pages.remove(0).1
        };
        let parsed = parser::parse_phased(&self.parser, &self.embedder, &jpeg).map_err(|e| e.to_string())?;
        let doc = ParsedDoc { markdown: parsed.markdown(), tables: parsed.tables(), elements_count: parsed.elements.len(), cached: false };
        let mut cache = lock(&self.doc_cache);
        cache.retain(|(k, _)| *k != key);
        cache.push((key, doc.clone()));
        if cache.len() > DOC_CACHE {
            cache.remove(0);
        }
        Ok(doc)
    }

    fn cached_doc(&self, key: &(String, u32)) -> Option<ParsedDoc> {
        lock(&self.doc_cache).iter().find(|(k, _)| k == key).map(|(_, d)| d.clone())
    }

    /// Reset the store (embeddings dropped, files back to pending) and make the crawler re-run.
    /// `reset` works on the live store under its lock, over its own connection: a second
    /// connection writing the same WAL file is what SQLite's WAL-reset race needs. Never touches
    /// Tier 0.
    pub fn rebuild_with(&self, reset: impl FnOnce(&mut Store) -> Result<(), String>) -> Result<(), String> {
        reset(&mut lock(&self.store))?;
        self.restart_indexing();
        Ok(())
    }

    fn restart_indexing(&self) {
        *lock(&self.last_error) = None;
        lock(&self.throughput).clear();
        lock(&self.parse_throughput).clear();
        {
            let mut snap = self.snap.write().unwrap_or_else(|e| e.into_inner());
            snap.gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        }
        self.kick();
    }

    /// Record a worker error. A corrupt store is repaired ([`Store::recover`]) instead of
    /// leaving "database disk image is malformed" up for good.
    fn report_error(&self, msg: String) {
        if winseek_store::is_corruption_message(&msg) {
            match self.recover_store() {
                Some(Ok(r)) if r != Recovery::Healthy => {
                    eprintln!("winseek: tier 1 store repaired: {r:?}");
                    self.restart_indexing();
                    return;
                }
                Some(Err(e)) => {
                    *lock(&self.last_error) =
                        Some(format!("The search-by-meaning index is damaged and could not be repaired ({e}). Rebuild the index."));
                    return;
                }
                _ => {}
            }
        }
        *lock(&self.last_error) = Some(msg);
    }

    /// Check and repair the store; `None` when it was checked within [`RECOVERY_INTERVAL`].
    fn recover_store(&self) -> Option<Result<Recovery, String>> {
        {
            let mut last = lock(&self.last_recovery);
            if last.is_some_and(|t| t.elapsed() < RECOVERY_INTERVAL) {
                return None;
            }
            *last = Some(Instant::now());
        }
        let mut store = lock(&self.store);
        let meta = store.meta().clone();
        let path = store.path();
        let placeholder = match Store::open_in_memory(&meta) {
            Ok(s) => s,
            Err(e) => return Some(Err(e.to_string())),
        };
        let old = std::mem::replace(&mut *store, placeholder);
        Some(match old.recover() {
            Ok((s, r)) => {
                *store = s;
                Ok(r)
            }
            Err(e) => {
                // Keep using the file as it is rather than an empty in-memory store.
                if let Some(s) = path.and_then(|p| Store::open(p, &meta).ok()) {
                    *store = s;
                }
                Err(e.to_string())
            }
        })
    }

    /// The hub direction for relevance calibration ([`relevance`]): computed once per query
    /// model (48 short queries), kept in memory and in the store's settings.
    fn hub_vector(&self, e: &mut Box<dyn Embedder + Send>) -> Result<Vec<f32>, EmbedError> {
        let model = e.meta().model_id;
        if let Some((m, v)) = lock(&self.hub).as_ref() {
            if *m == model {
                return Ok(v.clone());
            }
        }
        let key = format!("relevance_hub:{model}");
        let stored: Option<Vec<f32>> = lock(&self.store)
            .get_setting(&key)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok());
        let v = match stored.filter(|v| !v.is_empty()) {
            Some(v) => v,
            None => {
                let bank = HUB_PROMPTS.iter().map(|p| e.embed_query(p)).collect::<Result<Vec<_>, _>>()?;
                let v = relevance::hub_vector(&bank);
                if let Ok(json) = serde_json::to_string(&v) {
                    let _ = lock(&self.store).set_setting(&key, Some(&json));
                }
                v
            }
        };
        *lock(&self.hub) = Some((model, v.clone()));
        Ok(v)
    }

    /// Embed the query and return the in-scope files that are *relevant* (calibrated gate, see
    /// [`relevance`]), best first, at most `k`; then files whose extracted text contains the query
    /// words (`source: "text"`), which are evidence on their own. Nothing relevant -> no hits.
    pub fn semantic_search(&self, query: &str, k: usize) -> Result<SemanticResult, String> {
        self.semantic_page(query, 0, k)
    }

    /// [`Tier1::semantic_search`] in pages. A first page of at most [`SEM_K_SHALLOW`] hits is the
    /// quick top-k the overview shows. Anything else is "Show all": the whole list of relevant
    /// files is computed once (no count limit: the gate decides), kept, and handed out `k` at a
    /// time from `offset`; `total` is then exact. Only the returned page pays for the per-file
    /// work (stat, highlight boxes), so a list of any length answers as fast as a short one.
    pub fn semantic_page(&self, query: &str, offset: usize, k: usize) -> Result<SemanticResult, String> {
        let k = k.max(1);
        let q = query.trim();
        if q.is_empty() {
            return Ok(SemanticResult::new(self.state_str(), vec![]));
        }
        if offset == 0 && k <= SEM_K_SHALLOW {
            let (state, rows, more) = self.sem_rows(q, Some(k))?;
            let hits: Vec<SemanticHit> = rows.iter().map(|r| r.hit(q)).collect();
            return Ok(SemanticResult { state, total: hits.len(), more, hits });
        }
        // A later page of the list computed a moment ago; the first page always recomputes
        // (files may have been indexed or deleted since).
        let kept = lock(&self.sem_all).clone().filter(|l| offset > 0 && l.query == q);
        let list = match kept {
            Some(l) => l,
            None => {
                let (state, rows, _) = self.sem_rows(q, None)?;
                let l = Arc::new(SemList { query: q.to_string(), state, rows });
                *lock(&self.sem_all) = Some(l.clone());
                l
            }
        };
        let hits: Vec<SemanticHit> = list.rows.iter().skip(offset).take(k).map(|r| r.hit(q)).collect();
        Ok(SemanticResult { state: list.state.clone(), total: list.rows.len(), more: offset + hits.len() < list.rows.len(), hits })
    }

    /// `code:` queries: words found in the separate code index (source, notebooks, project prose).
    /// Needs the code option; code is never part of any other search.
    fn code_rows(&self, q: &str, limit: Option<usize>) -> (String, Vec<SemRow>, bool) {
        let snap = self.snapshot();
        if !snap.index_code || q.is_empty() {
            return (self.state_str().into(), vec![], false);
        }
        let raw = lock(&self.store).search_code(&fts_escape(q), limit.map_or(TEXT_ALL, |k| k * 4)).unwrap_or_default();
        let mut rows: Vec<SemRow> = raw
            .into_iter()
            .filter(|h| snap.in_scope(&h.path))
            .map(|h| SemRow { page: None, path: h.path, score: 0.0, locator: None, snippet: Some(h.snippet).filter(|s| s.contains('[')), source: "code" })
            .collect();
        let more = limit.is_some_and(|k| rows.len() > k);
        rows.truncate(limit.unwrap_or(usize::MAX));
        (self.state_str().into(), rows, more)
    }

    /// The hits for `q` before the per-file work: `(state, rows, more exist)`. `limit` = the
    /// top-k of the overview; `None` = every relevant file. Without models the query words found
    /// in extracted text (documents, PDFs, screenshot OCR) answer alone, state `models_missing`.
    fn sem_rows(&self, q: &str, limit: Option<usize>) -> Result<(String, Vec<SemRow>, bool), String> {
        if let Some(cq) = code_query(q) {
            return Ok(self.code_rows(cq, limit));
        }
        let text_k = limit.map_or(TEXT_ALL, |k| k * 4);
        let embedded = {
            let mut e = lock(&self.embedder);
            if !e.models_present() {
                None
            } else {
                let r = e.embed_query(q).and_then(|v| Ok((v, self.hub_vector(&mut e)?)));
                match r {
                    Ok(v) => Some(v),
                    Err(EmbedError::ModelsMissing) => None,
                    Err(EmbedError::Other(m)) => return Err(m),
                }
            }
        };
        let snap = self.snapshot();
        let Some((vec, hub)) = embedded else {
            let raw = lock(&self.store).search_text(&fts_escape(q), text_k).unwrap_or_default();
            let mut rows: Vec<SemRow> = raw
                .into_iter()
                .filter(|h| snap.in_scope(&h.path))
                .map(|h| SemRow {
                    page: locator_page(h.locator.as_deref()),
                    path: h.path,
                    score: 0.0,
                    locator: h.locator,
                    snippet: Some(h.snippet).filter(|s| s.contains('[')),
                    source: "text",
                })
                .collect();
            let more = limit.is_some_and(|k| rows.len() > k);
            rows.truncate(limit.unwrap_or(usize::MAX));
            return Ok(("models_missing".into(), rows, more));
        };
        let (cands, texts) = {
            let store = lock(&self.store);
            let cands = match limit {
                // Over-fetch: rows outside the current scopes (scopes shrank since indexing) are dropped.
                Some(k) => store.search_calibrated(&vec, &hub, (k * 4).max(40)),
                None => store.search_relevant(&vec, &hub),
            }
            .map_err(|e| e.to_string())?;
            let texts = store.search_text(&fts_escape(q), text_k).unwrap_or_default();
            (cands, texts)
        };
        let in_scope: Vec<_> = cands.into_iter().filter(|c| snap.in_scope(&c.path)).collect();
        let mut gated = relevance::gate(in_scope, limit.map_or(usize::MAX, |k| k.saturating_add(1)));
        let mut more = limit.is_some_and(|k| gated.len() > k);
        gated.truncate(limit.unwrap_or(usize::MAX));
        let text_pages: std::collections::HashMap<String, u32> =
            texts.iter().filter_map(|t| Some((t.path.to_lowercase(), locator_page(t.locator.as_deref())?))).collect();
        let snippets: std::collections::HashMap<String, String> =
            texts.iter().map(|t| (t.path.to_lowercase(), t.snippet.clone())).collect();
        let mut seen = std::collections::HashSet::new();
        let mut rows: Vec<SemRow> = gated
            .into_iter()
            .map(|h| {
                let key = h.path.to_lowercase();
                let snippet = snippets.get(&key).filter(|s| s.contains('[')).cloned();
                let page = locator_page(h.locator.as_deref()).or_else(|| text_pages.get(&key).copied());
                seen.insert(key);
                SemRow { path: h.path, score: h.score, locator: h.locator, snippet, page, source: "meaning" }
            })
            .collect();
        // The words themselves, found inside files (documents, PDFs, screenshot text), fused with
        // the meaning list: a literal hit is separate evidence and is not held to the meaning gate.
        let text_keys: Vec<String> = texts
            .iter()
            .filter(|t| snap.in_scope(&t.path) && t.snippet.contains('['))
            .map(|t| t.path.to_lowercase())
            .collect();
        for t in texts.into_iter().filter(|t| snap.in_scope(&t.path) && t.snippet.contains('[')) {
            if seen.contains(&t.path.to_lowercase()) {
                continue;
            }
            seen.insert(t.path.to_lowercase());
            rows.push(SemRow {
                page: locator_page(t.locator.as_deref()),
                path: t.path,
                score: 0.0,
                locator: t.locator,
                snippet: Some(t.snippet),
                source: "text",
            });
        }
        let sem_keys: Vec<String> = rows.iter().filter(|r| r.source == "meaning").map(|r| r.path.to_lowercase()).collect();
        let order = winseek_store::fuse::fuse_order(&sem_keys, &text_keys, winseek_store::fuse::exact_lean(q));
        let pos: std::collections::HashMap<&str, usize> = order.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
        rows.sort_by_key(|r| pos.get(r.path.to_lowercase().as_str()).copied().unwrap_or(usize::MAX));
        if let Some(k) = limit {
            let cap = k + (k / 3).max(3);
            if rows.len() > cap {
                more = true;
                rows.truncate(cap);
            }
        }
        Ok((self.state_str().into(), rows, more))
    }
}

/// `code: rest` -> `Some("rest")` (prefix case-insensitive, optional space after the colon).
pub fn code_query(q: &str) -> Option<&str> {
    let t = q.trim_start();
    let head = t.get(..5)?;
    head.eq_ignore_ascii_case("code:").then(|| t[5..].trim())
}

/// The processing loop. Runs forever.
///
/// With models: files that were stored text-only are embedded first (no re-extraction), then the
/// queue is processed as usual. Without models the state stays `models_missing`, but text-bearing
/// files (documents, enabled code, PDFs, screenshot OCR) are still extracted into FTS.
fn worker_loop(t: Arc<Tier1>) {
    lower_thread_priority();
    let requester = {
        let t = t.clone();
        Box::new(move || crate::models::request_pdfium({
            let t = t.clone();
            move |ok| {
                if ok {
                    t.kick();
                }
            }
        })) as Box<dyn Fn() + Send>
    };
    let mut proc = Processor::default().with_pdf_requester(requester);
    let mut parse_pass = ParsePass::default();
    let (mut prov_done, mut prov_tries) = (false, 0u32);
    loop {
        let have_models = {
            let mut e = lock(&t.embedder);
            e.tick_idle();
            e.models_present()
        };
        if let Ok(mut p) = t.parser.try_lock() {
            p.tick_idle(); // a parse in progress (preview) is not idle
        }
        // Embedding work comes first: the parser gives the GPU back before the index model loads.
        if have_models && proc.has_index_work(&t.store) {
            lock(&t.parser).unload();
        }
        if have_models && !provenance_step(&t, &mut prov_done, &mut prov_tries) {
            t.nap(IDLE_POLL);
            continue;
        }
        let started = std::time::Instant::now();
        let r = if have_models {
            match proc.run_upgrade_pass(&t.store, &t.embedder, &t.paused) {
                Ok(Pass::Idle) => match proc.run_pass(&t.store, &t.embedder, &t.paused) {
                    // The embed queue is empty: route images to the document parser / parse queued items.
                Ok(Pass::Idle) => match parse_pass.run(&t.store, &t.embedder, &t.parser, &t.paused) {
                    Ok(n) if n > 0 => {
                        lock(&t.parse_throughput).record(started, std::time::Instant::now(), n);
                        t.state.store(ST_INDEXING, Ordering::Relaxed);
                        *lock(&t.last_error) = None;
                        continue;
                    }
                    // Everything else is done: the rest of long documents, a slice at a time.
                    Ok(_) => proc.run_tail_pass(&t.store, &t.embedder, &t.paused),
                    Err(msg) => Err(msg),
                },
                    other => other,
                },
                other => other,
            }
        } else {
            t.state.store(ST_MODELS_MISSING, Ordering::Relaxed);
            match proc.run_fts_pass(&t.store, &t.paused) {
                Ok(Pass::Progress(n)) => {
                    lock(&t.throughput).record(started, std::time::Instant::now(), n);
                    *lock(&t.last_error) = None;
                    continue;
                }
                Ok(_) => {
                    t.nap(IDLE_POLL);
                    continue;
                }
                Err(msg) => {
                    t.report_error(msg);
                    std::thread::sleep(ERROR_BACKOFF);
                    continue;
                }
            }
        };
        if t.paused.load(Ordering::Relaxed) {
            lock(&t.embedder).on_pause();
            lock(&t.parser).unload();
        }
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
            // The document parser holds the GPU: unload it and go again right away.
            Ok(Pass::EmbedFailed(msg)) if embedder::is_gpu_busy(&msg) => lock(&t.parser).unload(),
            Ok(Pass::EmbedFailed(msg)) => {
                *lock(&t.last_error) = Some(msg);
                std::thread::sleep(ERROR_BACKOFF);
            }
            Err(msg) => {
                t.report_error(msg);
                std::thread::sleep(ERROR_BACKOFF);
            }
        }
    }
}

/// Does the store at `path`, stamped `stamp`, hold reference vectors to compare a new build against?
fn has_baseline(path: &std::path::Path, stamp: &winseek_store::IndexMeta) -> bool {
    Store::open(path, stamp).and_then(|s| s.ref_vectors()).is_ok_and(|(_, v)| !v.is_empty())
}

/// Probe attempts (each retried a second or so apart) before a verdict is forced.
const PROVENANCE_TRIES: u32 = 5;

/// Provenance step of the worker loop (see [`provenance`]). Returns `true` when the passes may run:
/// the stamp matches the running build or was just brought in line. While a verdict on a build
/// change is pending the passes wait; after [`PROVENANCE_TRIES`] failed probe attempts the stale
/// index is rebuilt (the old behaviour). A missing baseline never blocks indexing.
fn provenance_step(t: &Tier1, done: &mut bool, tries: &mut u32) -> bool {
    use provenance::Outcome;
    if *done {
        return true;
    }
    let rebuild = |why: String| {
        eprintln!("winseek: {why}; re-embedding the index");
        let meta = lock(&t.embedder).meta();
        if let Err(e) = t.rebuild_with(|s| s.reset_in_place(&meta).map_err(|e| e.to_string())) {
            t.report_error(e);
        }
    };
    match provenance::check(&t.store, &t.embedder) {
        Ok(Outcome::Current) => {
            *done = true;
            true
        }
        Ok(Outcome::Baselined) => {
            eprintln!("winseek: index stamped with {} and reference vectors stored", lock(&t.store).stamp().llama_build);
            *done = true;
            true
        }
        Ok(Outcome::Kept(c)) => {
            eprintln!("winseek: llama.cpp build changed; reference vectors unchanged (cos {c:.5}), index kept");
            *done = true;
            true
        }
        Ok(Outcome::Rebuild(c)) => {
            rebuild(match c {
                Some(c) => format!("llama.cpp build changed and vectors moved (cos {c:.5} < {})", winseek_store::PROBE_MIN_COS),
                None => "llama.cpp build changed and the index has no reference vectors".into(),
            });
            *tries = 0; // the reset index gets its baseline from the new build on the next step
            false
        }
        Err(e) => {
            *tries += 1;
            let stale = lock(&t.store).stamp() != &lock(&t.embedder).meta();
            if *tries < PROVENANCE_TRIES {
                return !stale;
            }
            *tries = 0;
            if stale {
                rebuild(format!("could not verify vectors after a llama.cpp build change ({e})"));
                false
            } else {
                *done = true; // no baseline this session; indexing goes on
                true
            }
        }
    }
}

/// How often skipped-for-now files (cloud placeholders, missing HEIC codec) are retried.
const RETRY_SKIPS_EVERY: Duration = Duration::from_secs(600);

fn crawler_loop(t: Arc<Tier1>) {
    lower_thread_priority();
    let mut done_gen = 0;
    let mut last_retry = std::time::Instant::now();
    loop {
        let snap = t.snapshot();
        if snap.gen != done_gen {
            let stop = || t.snapshot().gen != snap.gen;
            crawler::crawl(&t.store, &snap.scopes, &snap.ignore, snap.index_code, &stop);
            // Rows indexed under older rules (secrets, dataset folders, code projects) go now.
            if !stop() {
                crawler::purge_stale(&t.store, &snap.scopes, &snap.ignore, snap.index_code);
                let _ = lock(&t.store).requeue_skipped(crawler::RETRY_SKIPS);
            }
            // An aborted crawl (scopes changed mid-way) is redone on the next iteration.
            done_gen = snap.gen;
            last_retry = std::time::Instant::now();
        } else if last_retry.elapsed() >= RETRY_SKIPS_EVERY {
            // A cloud file hydrated or a codec installed since: look at those skips again (one stat each).
            let _ = lock(&t.store).requeue_skipped(crawler::RETRY_SKIPS);
            last_retry = std::time::Instant::now();
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

// Every command here runs off the main thread: they take the store lock, which the worker can
// hold for a while, and a plain (non-async) command would freeze the window meanwhile.

#[tauri::command]
pub async fn tier1_status(h: tauri::State<'_, Handle>) -> Result<StatusDto, String> {
    match h.get() {
        Ok(t) => crate::commands::blocking(move || t.status()).await,
        Err(e) => Ok(StatusDto { state: "unavailable".into(), stats: None, include: vec![], exclude: vec![], error: Some(e), paused_reason: None }),
    }
}

#[tauri::command]
pub async fn tier1_set_scopes(h: tauri::State<'_, Handle>, include: Vec<String>, exclude: Vec<String>) -> Result<(), String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.set_scopes(include, exclude)).await?
}

/// Remove a folder from indexing for good: out of the scopes, and its indexed files are
/// forgotten. Returns how many files were removed from the index.
#[tauri::command]
pub async fn tier1_remove_folder(h: tauri::State<'_, Handle>, path: String) -> Result<usize, String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.remove_folder(&path)).await?
}

#[tauri::command]
pub async fn tier1_get_ignore(h: tauri::State<'_, Handle>) -> Result<IgnoreDto, String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.get_ignore()).await?
}

/// `None` resets to the built-in defaults. Returns the saved list.
#[tauri::command]
pub async fn tier1_set_ignore(h: tauri::State<'_, Handle>, list: Option<Vec<String>>) -> Result<IgnoreDto, String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.set_ignore(list)).await?
}

#[tauri::command]
pub async fn tier1_get_index_code(h: tauri::State<'_, Handle>) -> Result<bool, String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.index_code()).await
}

/// Opt in / out of indexing source-code text. Returns the saved value.
#[tauri::command]
pub async fn tier1_set_index_code(h: tauri::State<'_, Handle>, on: bool) -> Result<bool, String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.set_index_code(on)).await?
}

#[tauri::command]
pub async fn tier1_pause(app: tauri::AppHandle, h: tauri::State<'_, Handle>, paused: bool) -> Result<(), String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.set_paused(paused)).await?;
    crate::watcher::refresh_pause_item(&app);
    Ok(())
}

/// "Extract text" for an image or one PDF page (`page` is 1-based, ignored for images).
#[tauri::command]
pub async fn parse_document(h: tauri::State<'_, Handle>, path: String, page: Option<u32>) -> Result<ParsedDoc, String> {
    let t = h.get()?;
    tauri::async_runtime::spawn_blocking(move || t.parse_document(&path, page)).await.map_err(|e| e.to_string())?
}

/// Are the document parser's model files installed (the preview offers its actions only then)?
#[tauri::command]
pub async fn parser_present(h: tauri::State<'_, Handle>) -> Result<bool, String> {
    let t = h.get()?;
    tauri::async_runtime::spawn_blocking(move || t.parser_present()).await.map_err(|e| e.to_string())
}

/// The saved performance profile: `low` | `balanced` | `fast`.
#[tauri::command]
pub async fn get_profile(h: tauri::State<'_, Handle>) -> Result<String, String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.profile().as_str().to_string()).await
}

/// Save and apply a performance profile. Returns what was saved.
#[tauri::command]
pub async fn set_profile(h: tauri::State<'_, Handle>, profile: String) -> Result<String, String> {
    let p = Profile::parse(&profile).ok_or_else(|| format!("Unknown profile: {profile}"))?;
    let t = h.get()?;
    let saved = tauri::async_runtime::spawn_blocking(move || t.set_profile(p)).await.map_err(|e| e.to_string())??;
    Ok(saved.as_str().to_string())
}

#[tauri::command]
pub async fn semantic_search(h: tauri::State<'_, Handle>, query: String, k: usize, offset: Option<usize>) -> Result<SemanticResult, String> {
    // Only the popup asks, one query at a time: a search still queued behind the embedder when a
    // newer one arrives is dropped (its answer would be thrown away by the UI anyway).
    static LATEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static TURN: Mutex<()> = Mutex::new(());
    let t = h.get()?;
    let me = LATEST.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn_blocking(move || {
        let _turn = lock(&TURN);
        if LATEST.load(Ordering::SeqCst) != me {
            return Ok(SemanticResult::new("superseded", vec![]));
        }
        t.semantic_page(&query, offset.unwrap_or(0), k)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Temp dir outside `%LOCALAPPDATA%` (paths containing an `appdata` segment are never in scope).
#[cfg(test)]
pub(crate) fn test_tmp() -> tempfile::TempDir {
    // Outside the repository: a project marker (Cargo.toml) above the temp dir would make every test
    // folder "code project content" for the crawler.
    let base = std::env::temp_dir().join("winseek-tier1-tests");
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
            // The relevance hub prompts get their own direction, unrelated to every test vector.
            Ok(unit(if HUB_PROMPTS.contains(&q) { 3 } else if q == "cat" { 1 } else { 2 }))
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
        tier_with(e, Box::new(parser::NoParser), include)
    }

    fn tier_with(e: Box<dyn Embedder + Send>, p: Box<dyn Parser + Send>, include: &[&str]) -> Arc<Tier1> {
        let store = Store::open_in_memory(&index_meta("test")).unwrap();
        Tier1::new(store, e, p, Scopes { include: include.iter().map(|s| s.to_string()).collect(), ..Default::default() })
    }

    fn add_done(t: &Tier1, n: u64, path: &str, hot: usize) {
        let mut s = lock(&t.store);
        let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: n }, path, 1, 1, "image").unwrap();
        s.complete(id, &format!("h{n}"), &[ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec: unit(hot) }], &[]).unwrap();
    }

    /// The live failure: SQLite reported "database disk image is malformed" for good. The worker's
    /// error path now repairs the store (settings kept), clears the error and re-crawls.
    #[test]
    fn corruption_error_repairs_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tier1.sqlite");
        {
            let t = Tier1::new(Store::open(&db, &index_meta("test")).unwrap(), Box::new(Q), Box::new(parser::NoParser), Scopes::default());
            t.set_scopes(vec![r"C:\Pics".into()], vec![]).unwrap();
            add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        } // closed: checkpointed
        // Garbage over the vectors root page, as a lost WAL frame leaves it.
        let (root, size): (i64, i64) = {
            let c = rusqlite::Connection::open(&db).unwrap();
            let root = c.query_row("SELECT rootpage FROM sqlite_master WHERE name = 'vectors'", [], |r| r.get(0)).unwrap();
            (root, c.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap())
        };
        let mut bytes = std::fs::read(&db).unwrap();
        bytes[((root - 1) * size) as usize..][..64].fill(0xEE);
        std::fs::write(&db, bytes).unwrap();

        let t = Tier1::new(Store::open(&db, &index_meta("test")).unwrap(), Box::new(Q), Box::new(parser::NoParser), Scopes::default());
        let msg = lock(&t.store).max_vector_rowid().unwrap_err().to_string();
        assert!(winseek_store::is_corruption_message(&msg), "{msg}");
        let g0 = t.snapshot().gen;
        t.report_error(msg);
        assert_eq!(t.status().error, None);
        assert!(t.snapshot().gen > g0, "the crawler walks the scopes again");
        let s = lock(&t.store);
        assert_eq!(s.get_scopes().unwrap().include, vec![r"C:\Pics".to_string()]);
        assert_eq!((s.stats().unwrap().pending, s.stats().unwrap().vectors), (1, 0));
        assert!(dir.path().join("tier1.sqlite.corrupt").exists());
    }

    /// Diagnose a stalled crawl: run the real worker passes against a copy of the installed store.
    /// `WINSEEK_DIAG_DB=<copy of tier1.sqlite> cargo test -p winseek-app diag_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn diag_live() {
        let Some(db) = std::env::var_os("WINSEEK_DIAG_DB") else { return };
        let root = data_root().unwrap();
        let mut e = LlamaEmbedder::new(root, Manifest::embedded());
        e.set_profile(Profile::Balanced);
        let store = Store::open(std::path::Path::new(&db), &e.meta()).unwrap();
        let store = Mutex::new(store);
        let emb: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(e));
        let paused = AtomicBool::new(false);
        let mut proc = process::Processor::default();
        for i in 0..6 {
            let t0 = std::time::Instant::now();
            let up = proc.run_upgrade_pass(&store, &emb, &paused);
            let t1 = t0.elapsed();
            let r = proc.run_pass(&store, &emb, &paused);
            eprintln!("pass {i}: upgrade {up:?} in {t1:?}; run {r:?} in {:?}; stats {:?}", t0.elapsed(), lock(&store).stats().ok());
        }
    }

    #[test]
    fn semantic_search_ranks_and_filters_scope() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        add_done(&t, 2, r"C:\Pics\dog.jpg", 2);
        add_done(&t, 3, r"C:\Old\cat2.jpg", 1); // no longer in scope
        let r = t.semantic_search("cat", 5).unwrap();
        assert_eq!(r.hits.len(), 1, "dog.jpg is unrelated to the query: gated out, not listed last");
        assert_eq!(r.hits[0].path, r"C:\Pics\cat.jpg");
        assert!(r.hits[0].score > 0.99);
        assert!(t.semantic_search("  ", 5).unwrap().hits.is_empty());
    }

    #[test]
    fn show_all_lists_every_relevant_file_in_pages() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        for i in 0..7u64 {
            add_done(&t, i + 1, &format!(r"C:\Pics\cat{i}.jpg"), 1);
        }
        add_done(&t, 50, r"C:\Pics\dog.jpg", 2);
        // The overview's quick top-k says that there is more.
        let top = t.semantic_search("cat", 3).unwrap();
        assert_eq!((top.hits.len(), top.more), (3, true));
        // "Show all": every relevant file, the unrelated one never.
        let all = t.semantic_page("cat", 0, SEM_K_SHALLOW + 1).unwrap();
        assert_eq!((all.hits.len(), all.total, all.more), (7, 7, false));
        assert!(all.hits.iter().all(|h| h.path.contains("cat")));
        // Later pages are cut from the same list, in the same order.
        let p = t.semantic_page("cat", 2, 3).unwrap();
        assert_eq!((p.total, p.more), (7, true));
        assert_eq!(p.hits, all.hits[2..5]);
        let last = t.semantic_page("cat", 5, 3).unwrap();
        assert_eq!((last.hits.len(), last.more), (2, false));
        assert!(t.semantic_page("cat", 7, 3).unwrap().hits.is_empty());
    }

    #[test]
    fn words_found_in_text_are_listed_after_meaning_hits_and_hub_is_stored() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        {
            let mut s = lock(&t.store);
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 9 }, r"C:\Pics\notes.txt", 1, 1, "doc").unwrap();
            let v = [ChunkVec { chunk: 0, kind: VecKind::Text, locator: None, vec: unit(0) }];
            s.complete(id, "h9", &v, &[(0, "feed the cat at noon".into())]).unwrap();
        }
        let r = t.semantic_search("cat", 5).unwrap();
        let src: Vec<(&str, &str)> = r.hits.iter().map(|h| (h.path.rsplit('\\').next().unwrap(), h.source)).collect();
        assert_eq!(src, [("cat.jpg", "meaning"), ("notes.txt", "text")]);
        assert!(r.hits[1].snippet.as_deref().unwrap().contains("[cat]"));
        let stored = lock(&t.store).get_setting("relevance_hub:embeddinggemma-2").unwrap();
        assert!(stored.is_some_and(|s| s.starts_with('[')), "hub persisted per query model");
    }

    #[test]
    fn a_code_in_the_query_lifts_the_literal_hit_above_meaning_hits() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        add_done(&t, 1, r"C:\Pics\something.jpg", 2); // the stub's meaning match for any non-"cat" query
        {
            let mut s = lock(&t.store);
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 9 }, r"C:\Pics\inv.txt", 1, 1, "doc").unwrap();
            let v = [ChunkVec { chunk: 0, kind: VecKind::Text, locator: None, vec: unit(0) }]; // no meaning match
            s.complete(id, "h9", &v, &[(0, "Invoice INV-2024-00817: total 2480 euros".into())]).unwrap();
        }
        let names = |q: &str| -> Vec<String> {
            t.semantic_search(q, 5).unwrap().hits.iter().map(|h| h.path.rsplit('\\').next().unwrap().to_string()).collect()
        };
        assert_eq!(names("INV-2024-00817"), ["inv.txt", "something.jpg"], "an invoice number is a literal: it leads");
        // The same file found by a plain word keeps the old order: meaning first, words after.
        assert_eq!(names("invoice"), ["something.jpg", "inv.txt"]);
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
    fn locator_page_parses_only_positive_pages() {
        assert_eq!(locator_page(Some(r#"{"page":3,"chars":[0,9]}"#)), Some(3));
        assert_eq!(locator_page(Some(r#"{"page":0}"#)), None);
        assert_eq!(locator_page(Some(r#"{"slide":2}"#)), None);
        assert_eq!(locator_page(Some("not json")), None);
        assert_eq!(locator_page(None), None);
    }

    #[test]
    fn no_models_still_returns_text_hits_labelled_text() {
        let t = tier(Box::new(Missing), &[r"C:\Docs"]);
        {
            let mut s = lock(&t.store);
            let pend = |c, l: &str| winseek_store::PendingVec { chunk: c, kind: VecKind::Text, locator: Some(l.into()) };
            let a = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 1 }, r"C:\Docs\report.pdf", 1, 1, "pdf").unwrap();
            s.complete_text_only(
                a,
                "ha",
                &[pend(0, r#"{"page":1,"chars":[0,20]}"#), pend(1, r#"{"page":7,"chars":[0,30]}"#)],
                &[(0, "cover page".into()), (1, "the quarterly invoice total".into())],
            )
            .unwrap();
            let b = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 2 }, r"C:\Elsewhere\x.pdf", 1, 1, "pdf").unwrap();
            s.complete_text_only(b, "hb", &[pend(0, "{}")], &[(0, "invoice outside the scope".into())]).unwrap();
        }
        let r = t.semantic_search("invoice", 5).unwrap();
        assert_eq!(r.state, "models_missing");
        assert_eq!(r.hits.len(), 1, "out-of-scope file dropped");
        let h = &r.hits[0];
        assert_eq!((h.path.as_str(), h.source, h.page), (r"C:\Docs\report.pdf", "text", Some(7)));
        assert!(h.snippet.as_deref().unwrap().contains("[invoice]"));
        assert!(t.semantic_search("zebra", 5).unwrap().hits.is_empty());
        // status counts them as text-only (waiting for models)
        let st = t.status().stats.unwrap();
        assert_eq!((st.text_only, st.done), (2, 0));
    }

    #[test]
    fn meaning_hits_carry_the_pdf_page() {
        let t = tier(Box::new(Q), &[r"C:\Docs"]);
        {
            let mut s = lock(&t.store);
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 1 }, r"C:\Docs\scan.pdf", 1, 1, "pdf").unwrap();
            let v = [ChunkVec { chunk: 4, kind: VecKind::Image, locator: Some(r#"{"page":5}"#.into()), vec: unit(1) }];
            s.complete(id, "h1", &v, &[]).unwrap();
        }
        let r = t.semantic_search("cat", 5).unwrap();
        assert_eq!((r.hits[0].page, r.hits[0].source), (Some(5), "meaning"));
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
    fn remove_folder_drops_the_scope_and_forgets_its_files() {
        let t = tier(Box::new(Q), &[r"C:\Pics", r"D:\Docs"]);
        {
            let mut s = lock(&t.store);
            let a = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 1 }, r"C:\Pics\cat.jpg", 1, 1, "image").unwrap();
            s.complete(a, "h1", &[ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec: unit(1) }], &[(0, "a cat picture".into())]).unwrap();
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 2 }, r"D:\Docs\report.pdf", 1, 1, "doc").unwrap();
            s.complete(id, "h2", &[], &[(0, "the quarterly invoice total".into())]).unwrap();
        }
        let g0 = t.snapshot().gen;
        assert_eq!(t.remove_folder(r"C:\Pics\").unwrap(), 1, "trailing separator is trimmed, the file is gone");
        assert!(t.snapshot().gen > g0, "the crawler re-runs");
        let st = t.status();
        assert_eq!(st.include, vec![r"D:\Docs".to_string()]);
        assert_eq!(lock(&t.store).get_scopes().unwrap().include, vec![r"D:\Docs".to_string()]);
        {
            let s = lock(&t.store);
            assert_eq!(s.stats().unwrap().files, 1, "only the kept folder's row is left");
            assert!(s.search_text("invoice", 5).unwrap().len() == 1);
            assert!(s.search_text("cat", 5).unwrap().is_empty(), "the removed folder's text is forgotten");
        }
        // Case-insensitive and idempotent; an unknown path only purges (nothing to purge).
        assert_eq!(t.remove_folder(r"c:\pics").unwrap(), 0);
        assert_eq!(t.remove_folder("").unwrap_err(), "remove_folder: empty path");
    }

    /// Removing the LAST folder must survive a restart: the saved list is configured, so the
    /// defaults (Pictures/Documents/Desktop/Downloads) must not resurrect (`scopes::effective`).
    #[test]
    fn removing_every_folder_survives_a_restart() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        let dir = test_tmp();
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3]));
        img.save(dir.path().join("only.png")).unwrap();
        t.set_scopes(vec![dir.path().to_string_lossy().into_owned()], vec![]).unwrap();
        assert_eq!(t.remove_folder(&dir.path().to_string_lossy()).unwrap(), 0, "nothing indexed yet, scope still drops");
        let saved = lock(&t.store).get_scopes().unwrap();
        assert!(saved.include.is_empty() && saved.configured, "the empty list is saved as configured: {saved:?}");
        let (effective, defaulted) = scopes::effective(saved);
        assert!(!defaulted, "the defaults must not resurrect");
        assert!(effective.include.is_empty());
        // And the crawler acts on it: the snapshot scopes are the empty list too.
        assert!(t.snapshot().scopes.include.is_empty());
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
    fn index_code_opt_in_persists_and_off_forgets_code_files() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        assert!(!t.index_code());
        let g0 = t.snapshot().gen;
        assert_eq!(t.set_index_code(true), Ok(true));
        assert!(t.index_code() && t.snapshot().index_code);
        assert!(t.snapshot().gen > g0, "the crawler re-runs");
        assert!(lock(&t.store).get_index_code().unwrap());
        {
            let mut s = lock(&t.store);
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 1 }, r"C:\Pics\a.rs", 1, 1, "code").unwrap();
            s.complete(id, "hc", &[], &[(0, "fn main".into())]).unwrap();
        }
        add_done(&t, 2, r"C:\Pics\cat.jpg", 1);
        assert_eq!(t.set_index_code(false), Ok(false));
        let s = lock(&t.store);
        assert!(!s.get_index_code().unwrap());
        assert_eq!(s.stats().unwrap().files, 1, "only the image row is left");
        assert!(s.search_text("main", 5).unwrap().is_empty());
    }

    #[test]
    fn rebuild_swaps_the_store_and_restarts_the_crawl() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        assert_eq!(lock(&t.store).stats().unwrap().done, 1);
        *lock(&t.last_error) = Some("old".into());
        let g0 = t.snapshot().gen;
        t.rebuild_with(|s| s.reset_in_place(&index_meta("test")).map_err(|e| e.to_string())).unwrap();
        let st = lock(&t.store).stats().unwrap();
        assert_eq!((st.done, st.pending, st.vectors), (0, 1, 0)); // the file is kept, its vectors are not
        assert!(t.snapshot().gen > g0); // the crawler sees a new generation and walks the scopes again
        assert_eq!(t.status().error, None);
        // a failing reset leaves the current store alone
        add_done(&t, 2, r"C:\Pics\dog.jpg", 2);
        assert_eq!(t.rebuild_with(|_| Err("boom".into())), Err("boom".to_string()));
        assert_eq!(lock(&t.store).stats().unwrap().done, 1);
    }

    struct PauseProbe(Arc<std::sync::atomic::AtomicU32>);
    impl Embedder for PauseProbe {
        fn embed_images(&mut self, _: &[Vec<u8>]) -> embedder::Result<Vec<Vec<f32>>> {
            Err(EmbedError::ModelsMissing)
        }
        fn embed_query(&mut self, _: &str) -> embedder::Result<Vec<f32>> {
            Err(EmbedError::ModelsMissing)
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            index_meta("test")
        }
        fn on_pause(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn pausing_unloads_the_embedder_but_resuming_does_not() {
        let n = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let t = tier(Box::new(PauseProbe(n.clone())), &[r"C:\Pics"]);
        t.set_auto_pause(Some(PauseReason::Fullscreen));
        assert_eq!(n.load(Ordering::SeqCst), 1);
        t.set_auto_pause(None);
        assert_eq!(n.load(Ordering::SeqCst), 1, "resuming must not unload");
        t.set_paused(true);
        assert_eq!(n.load(Ordering::SeqCst), 2);
        t.set_paused(false);
        assert_eq!(n.load(Ordering::SeqCst), 2);
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

    // ---- parser, boxes, profile ----------------------------------------------------

    type Shared<T> = Arc<Mutex<T>>;

    /// Parser stand-in: one canned page, counts its calls / unloads, remembers the profile it was given.
    struct FakeParser {
        present: bool,
        calls: Arc<std::sync::atomic::AtomicU32>,
        unloads: Arc<std::sync::atomic::AtomicU32>,
        profile: Shared<Option<Profile>>,
    }
    impl FakeParser {
        fn boxed(present: bool) -> (Box<dyn Parser + Send>, Self) {
            let f = FakeParser { present, calls: Arc::default(), unloads: Arc::default(), profile: Arc::default() };
            let probe = FakeParser { present, calls: f.calls.clone(), unloads: f.unloads.clone(), profile: f.profile.clone() };
            (Box::new(f), probe)
        }
    }
    impl Parser for FakeParser {
        fn present(&self) -> bool {
            self.present
        }
        fn parse(&mut self, _: &[u8]) -> Result<winseek_llama::Parsed, parser::ParseError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            winseek_llama::parse::parse_output(
                r##"[{"bbox":[100,50,900,120],"category":"title","text":"# Quarterly Invoice 4471"},
                    {"bbox":[100,200,900,400],"category":"table","text":"<table><tr><th>Item</th><th>Qty</th></tr><tr><td>Card</td><td>1</td></tr></table>"}]"##,
            )
            .map(|elements| winseek_llama::Parsed { elements, prompt_version: "doc2json-v1".into() })
            .map_err(|e| parser::ParseError::Other(e.to_string()))
        }
        fn unload(&mut self) {
            self.unloads.fetch_add(1, Ordering::SeqCst);
        }
        fn set_profile(&mut self, p: Profile) {
            *self.profile.lock().unwrap() = Some(p);
        }
    }

    struct ProfileProbe(Shared<Option<Profile>>);
    impl Embedder for ProfileProbe {
        fn embed_images(&mut self, _: &[Vec<u8>]) -> embedder::Result<Vec<Vec<f32>>> {
            Err(EmbedError::ModelsMissing)
        }
        fn embed_query(&mut self, _: &str) -> embedder::Result<Vec<f32>> {
            Err(EmbedError::ModelsMissing)
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            index_meta("test")
        }
        fn set_profile(&mut self, p: Profile) {
            *self.0.lock().unwrap() = Some(p);
        }
    }

    #[test]
    fn profile_setting_round_trips_and_reaches_both_models() {
        let seen: Shared<Option<Profile>> = Arc::default();
        let (parser, probe) = FakeParser::boxed(true);
        let t = tier_with(Box::new(ProfileProbe(seen.clone())), parser, &[r"C:\Pics"]);
        assert_eq!(t.profile(), Profile::Balanced, "default");
        assert_eq!(t.set_profile(Profile::Low), Ok(Profile::Low));
        assert_eq!(t.profile(), Profile::Low);
        assert_eq!(lock(&t.store).get_setting(SETTING_PROFILE).unwrap().as_deref(), Some("low"));
        assert_eq!((*seen.lock().unwrap(), *probe.profile.lock().unwrap()), (Some(Profile::Low), Some(Profile::Low)));
        t.set_profile(Profile::Fast).unwrap();
        assert_eq!((t.profile(), *seen.lock().unwrap(), *probe.profile.lock().unwrap()), (Profile::Fast, Some(Profile::Fast), Some(Profile::Fast)));
        // an unreadable stored value falls back to the default
        lock(&t.store).set_setting(SETTING_PROFILE, Some("turbo")).unwrap();
        assert_eq!(t.profile(), Profile::Balanced);
    }

    #[test]
    fn pausing_unloads_the_parser_too() {
        let (parser, probe) = FakeParser::boxed(true);
        let t = tier_with(Box::new(Q), parser, &[r"C:\Pics"]);
        t.set_auto_pause(Some(PauseReason::Battery));
        assert_eq!(probe.unloads.load(Ordering::SeqCst), 1);
        t.set_auto_pause(None);
        assert_eq!(probe.unloads.load(Ordering::SeqCst), 1, "resuming must not unload");
    }

    #[test]
    fn parser_state_for_the_status_page() {
        let (parser, _) = FakeParser::boxed(true);
        let t = tier_with(Box::new(Q), parser, &[r"C:\Pics"]);
        assert!(t.parser_present());
        assert_eq!(t.parser_runtime(), RuntimeInfo::default());
        assert_eq!(t.parse_counts(), ParseCounts::default());
        lock(&t.store).queue_parse("h", 0, "doc_image").unwrap();
        assert_eq!(t.parse_counts().queued, 1);
        let held = lock(&t.parser); // a parse in progress: present, and the server is by definition up
        assert!(t.parser_present() && t.parser_runtime().running);
        drop(held);
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        assert!(!t.parser_present());
    }

    #[test]
    fn hits_carry_boxes_for_the_query_words() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        {
            let mut s = lock(&t.store);
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 9 }, r"C:\Pics\scan.png", 1, 1, "image").unwrap();
            let loc = r#"{"ocr":{"w":200,"h":100,"lines":[{"t":"Invoice 4711","r":[20,10,100,20]},{"t":"other","r":[0,50,10,10]}]}}"#;
            let v = [ChunkVec { chunk: 0, kind: VecKind::Image, locator: Some(loc.into()), vec: unit(2) }];
            s.complete(id, "h9", &v, &[(0, "Invoice 4711 other".into())]).unwrap();
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 10 }, r"C:\Pics\parsed.png", 1, 1, "image").unwrap();
            let loc = r#"{"parser":{"v":"doc2json-v1","boxes":[{"b":[100,200,600,300],"c":"text","t":"Total due: Invoice"}]}}"#;
            let v = [ChunkVec { chunk: 0, kind: VecKind::Image, locator: Some(loc.into()), vec: unit(2) }];
            s.complete(id, "h10", &v, &[]).unwrap();
        }
        let r = t.semantic_search("invoice", 5).unwrap();
        let by = |name: &str| r.hits.iter().find(|h| h.path.ends_with(name)).unwrap();
        assert_eq!(by("scan.png").boxes, Some(vec![HitBox { x: 0.1, y: 0.1, w: 0.5, h: 0.2 }]));
        assert_eq!(by("parsed.png").boxes, Some(vec![HitBox { x: 0.1, y: 0.2, w: 0.5, h: 0.1 }]));
        // words that are in no box: no highlight
        let r = t.semantic_search("zebra", 5).unwrap();
        assert!(r.hits.iter().all(|h| h.boxes.is_none()));
        // JSON shape the UI reads
        let v = serde_json::to_value(by_name(&t.semantic_search("invoice", 5).unwrap(), "scan.png")).unwrap();
        assert_eq!(v["boxes"][0]["w"], 0.5);
    }

    fn by_name<'a>(r: &'a SemanticResult, name: &str) -> &'a SemanticHit {
        r.hits.iter().find(|h| h.path.ends_with(name)).unwrap()
    }

    #[test]
    fn text_only_hits_carry_boxes_too() {
        let t = tier(Box::new(Missing), &[r"C:\Pics"]);
        {
            let mut s = lock(&t.store);
            let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: 1 }, r"C:\Pics\shot.png", 1, 1, "image").unwrap();
            let loc = r#"{"ocr":{"w":10,"h":10,"lines":[{"t":"invoice","r":[1,2,3,4]}]}}"#;
            let pend = [winseek_store::PendingVec { chunk: 1, kind: VecKind::Text, locator: Some(loc.into()) }];
            s.complete_text_only(id, "h", &pend, &[(1, "invoice".into())]).unwrap();
        }
        let r = t.semantic_search("invoice", 5).unwrap();
        assert_eq!(r.hits[0].source, "text");
        assert_eq!(r.hits[0].boxes, Some(vec![HitBox { x: 0.1, y: 0.2, w: 0.3, h: 0.4 }]));
    }

    fn write_png(dir: &std::path::Path, name: &str) -> String {
        let img = image::RgbImage::from_pixel(60, 40, image::Rgb([200, 200, 200]));
        let p = dir.join(name);
        img.save(&p).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn parse_document_returns_markdown_tables_and_caches_by_content() {
        let dir = test_tmp();
        let (parser, probe) = FakeParser::boxed(true);
        let t = tier_with(Box::new(Q), parser, &[r"C:\Pics"]);
        let path = write_png(dir.path(), "invoice.png");
        let d = t.parse_document(&path, Some(7)).unwrap(); // the page is ignored for images
        assert!(!d.cached);
        assert_eq!(d.elements_count, 2);
        assert_eq!(d.markdown, "# Quarterly Invoice 4471\n\n| Item | Qty |\n| --- | --- |\n| Card | 1 |");
        assert_eq!((d.tables.len(), d.tables[0].tsv.as_str()), (1, "Item\tQty\nCard\t1"));
        let again = t.parse_document(&path, None).unwrap();
        assert!(again.cached, "same content: no second parse");
        assert_eq!((again.markdown, again.elements_count), (d.markdown.clone(), 2));
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        // the same bytes under another name hit the cache too; different bytes do not
        std::fs::copy(&path, dir.path().join("copy.png")).unwrap();
        assert!(t.parse_document(&dir.path().join("copy.png").to_string_lossy(), None).unwrap().cached);
        let other = image::RgbImage::from_pixel(61, 40, image::Rgb([1, 2, 3]));
        other.save(dir.path().join("other.png")).unwrap();
        assert!(!t.parse_document(&dir.path().join("other.png").to_string_lossy(), None).unwrap().cached);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn parse_document_cache_keeps_the_last_eight() {
        let dir = test_tmp();
        let (parser, probe) = FakeParser::boxed(true);
        let t = tier_with(Box::new(Q), parser, &[r"C:\Pics"]);
        let paths: Vec<String> = (0..9u32)
            .map(|i| {
                let img = image::RgbImage::from_pixel(10 + i, 10, image::Rgb([i as u8, 0, 0]));
                let p = dir.path().join(format!("{i}.png"));
                img.save(&p).unwrap();
                p.to_string_lossy().into_owned()
            })
            .collect();
        for p in &paths {
            t.parse_document(p, None).unwrap();
        }
        assert_eq!(probe.calls.load(Ordering::SeqCst), 9);
        assert!(t.parse_document(&paths[8], None).unwrap().cached);
        assert!(t.parse_document(&paths[1], None).unwrap().cached, "the 8 newest are kept");
        assert!(!t.parse_document(&paths[0], None).unwrap().cached, "the oldest was evicted");
        assert_eq!(probe.calls.load(Ordering::SeqCst), 10);
    }

    #[test]
    fn parse_document_refuses_what_it_cannot_read() {
        let dir = test_tmp();
        let (parser, probe) = FakeParser::boxed(true);
        let t = tier_with(Box::new(Q), parser, &[r"C:\Pics"]);
        let txt = dir.path().join("a.txt");
        std::fs::write(&txt, "hello").unwrap();
        assert!(t.parse_document(&txt.to_string_lossy(), None).unwrap_err().contains("images and PDF"));
        assert!(t.parse_document(&dir.path().join("missing.png").to_string_lossy(), None).unwrap_err().contains("Cannot read"));
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"not an image").unwrap();
        assert!(t.parse_document(&bad.to_string_lossy(), None).unwrap_err().contains("decode"));
        assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
        // no parser installed
        let none = tier(Box::new(Q), &[r"C:\Pics"]);
        let png = write_png(dir.path(), "x.png");
        assert!(none.parse_document(&png, None).unwrap_err().contains("not installed"));
    }

    #[test]
    fn end_to_end_crawl_then_process() {
        let dir = test_tmp();
        let img = image::RgbImage::from_pixel(20, 10, image::Rgb([1, 2, 3]));
        img.save(dir.path().join("a.png")).unwrap();
        let t = tier(Box::new(Q), &[&dir.path().to_string_lossy()]);
        let snap = t.snapshot();
        assert_eq!(crawler::crawl(&t.store, &snap.scopes, &snap.ignore, snap.index_code, &|| false).upserted, 1);
        let mut p = Processor::default();
        assert_eq!(p.run_pass(&t.store, &t.embedder, &t.paused).unwrap(), Pass::Progress(1));
        assert_eq!(lock(&t.store).stats().unwrap().done, 1);
    }
}
