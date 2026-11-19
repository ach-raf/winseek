//! Tier 1 worker: semantic image indexing, run as the user inside the app (never in searchd).
//!
//! Threads (all below-normal priority):
//! - crawler: walks the include roots at start and whenever the scopes change;
//! - change feed: searchd's USN-derived events for those roots, reconnecting with backoff;
//! - processor: pending files -> hash -> decode/resize -> embed -> store (images), or
//!   extract text -> chunk -> FTS + text vectors (Office / text documents, opt-in source code).

pub mod chunk;
pub mod code_rank;
pub mod crawler;
pub mod embedder;
pub mod estimate;
pub mod extract;
pub mod feed;
pub mod git;
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
pub mod progress;
pub mod provenance;
pub mod reranker;
pub mod route;
pub mod scopes;
pub mod timeline;

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
use winseek_store::{
    default_ignore, fts_escape, in_scope_with, CodeStretch, CodeWordsHit, Ignore, ParseCounts,
    Recovery, Scopes, Stats, Store,
};

use embedder::{EmbedError, Embedder, LlamaEmbedder, RuntimeInfo};
use hitbox::HitBox;
use parse_pass::{ParsePass, PARSE_LONG_SIDE};
use parser::{LlamaParser, Parser};
use policy::PauseReason;
use process::{Pass, Processor};
use reranker::{LlamaReranker, NoReranker, Reranker};

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
/// Store setting: `0` switches the reranker off; absent / anything else = on (when installed).
pub const SETTING_RERANK_ENABLED: &str = "rerank_enabled";
/// Candidates handed to the reranker, overview and "Show all" alike: on the GPU 20 documents of
/// [`RERANK_DOC_CHARS`] take ~0.7 s, well inside [`reranker::BUDGET`].
const RERANK_TOP_DOCS: usize = 20;
/// Fewer candidates than this are not worth a reranker call.
const RERANK_MIN_DOCS: usize = 3;
/// A document is cut to this many characters (the server's 2048-token context holds query +
/// document; dense non-English text runs ~0.7 tokens per character).
const RERANK_DOC_CHARS: usize = 1000;
/// On-demand parse results kept in memory (by content hash and page).
const DOC_CACHE: usize = 8;

pub struct Tier1 {
    pub store: Mutex<Store>,
    embedder: Mutex<Box<dyn Embedder + Send>>,
    /// The document parser (Infinity-Parser2). Never locked together with the embedder.
    parser: Mutex<Box<dyn Parser + Send>>,
    /// The optional reranker (Qwen3-Reranker). Locked on its own, never together with another model.
    reranker: Mutex<Box<dyn Reranker + Send>>,
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
    /// The page batch the document parser is reading right now (fed by the parse pass).
    parse_live: parse_pass::ParseLiveSlot,
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
    /// Unchecked folders: kept in the list (and their indexed rows kept) so a re-check is
    /// instant, but not crawled and not in scope.
    pub disabled: Vec<String>,
    pub error: Option<String>,
    /// Why indexing is paused: "user" | "battery" | "fullscreen" | null.
    pub paused_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticHit {
    pub path: String,
    /// The final score relative to the best hit of the list (1.0 = the top one): the badge.
    pub score: f32,
    /// The final score itself, on the gate's scale (a meaning hit starts at `Z_MIN`, the words add
    /// their bonus; words alone = the bonus). Comparable between lists of one query, so the UI can
    /// add what it knows (the file name) and still rank by one number.
    pub rank: f32,
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
    /// The reranker's relevance for this file, 0..1 (`None`: not reranked). The UI shows it as the
    /// match badge when present.
    pub rerank: Option<f32>,
    /// Code hit: the 1-based line to open the file at, from the chunk locator.
    pub line: Option<u32>,
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

/// The file name (last path component) — the `title:` half of a document embedding.
pub fn file_title(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A code file's `title:` half: its path from the project's folder, the project's name first
/// (`"wineseek_app/app/src-tauri/src/tier1/mod.rs"` — where the file lives in the repo is
/// context the model can use; DigUp measured +0.10 MRR over the bare name). Outside any
/// project: the folder and the file, as written.
pub fn code_title(path: &str) -> String {
    if let Some(root) = guard::code_project_root(path) {
        let root = root
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_owned();
        let rel = path
            .strip_prefix(&root)
            .map(|r| r.trim_start_matches(['\\', '/']))
            .filter(|r| !r.is_empty());
        if let (Some(repo), Some(rel)) = (
            std::path::Path::new(&root)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            rel,
        ) {
            return format!("{repo}\\{rel}");
        }
    }
    let comps: Vec<&str> = path.split(['\\', '/']).filter(|c| !c.is_empty()).collect();
    let n = comps.len();
    comps[n.saturating_sub(2)..].join("\\")
}

/// The `page` of a chunk locator (`{"page":3,"chars":[0,90]}`).
pub fn locator_page(locator: Option<&str>) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(locator?).ok()?;
    u32::try_from(v.get("page")?.as_u64()?)
        .ok()
        .filter(|p| *p > 0)
}

/// The 1-based line to open at, from a code chunk's locator (`{"loc":12,"loc_end":40}`).
pub fn locator_code_line(locator: Option<&str>) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(locator?).ok()?;
    let loc = |k: &str| u32::try_from(v.get(k)?.as_u64()?).ok().filter(|l| *l > 0);
    loc("line")
        .or_else(|| loc("loc"))
        .or_else(|| loc("loc_end"))
}

/// The stretch's first 1-based line, from its locator's `loc`.
fn locator_first_line(locator: Option<&str>) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(locator?).ok()?;
    u32::try_from(v.get("loc")?.as_u64()?)
        .ok()
        .filter(|l| *l > 0)
}

/// The locator for opening at 1-based `line`: the stretch's own locator plus an explicit `line`
/// the opener prefers, so a hit can land inside the stretch, not just at its top.
fn with_line(locator: Option<&str>, line: u32) -> String {
    let mut v: serde_json::Value = locator
        .and_then(|l| serde_json::from_str(l).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("line".into(), serde_json::json!(line));
    }
    v.to_string()
}

/// Where each shown code hit opens: a looked-up name opens at the stretch that defines it, when
/// one of the first ten results defines it (a name used all over a repo sits in hundreds of
/// stretches — DigUp's measured cut, 560 -> 100 ms a search); otherwise the line is the first
/// one of the pointed stretch carrying the query's words. A match by meaning alone keeps the
/// stretch's first line. Port of DigUp `codePlace`.
fn code_focus(
    rows: &mut [(SemRow, String, bool)],
    name: Option<&str>,
    terms: &[String],
    stretches_of: &dyn Fn(&str) -> Vec<CodeStretch>,
) {
    let mut cache: std::collections::HashMap<String, Vec<CodeStretch>> =
        std::collections::HashMap::new();
    for (rank, (row, hash, words_here)) in rows.iter_mut().enumerate() {
        let all = cache
            .entry(hash.clone())
            .or_insert_with(|| stretches_of(hash));
        if all.is_empty() {
            continue;
        }
        // 1. The definition of the looked-up name, in the first ten results only.
        if let (Some(n), true) = (name, rank < 10) {
            let mut best: Option<(u32, u32, i32)> = None; // (chunk, line, strength)
            for s in all.iter() {
                let first = locator_first_line(s.locator.as_deref()).unwrap_or(1);
                if let Some((line, strength)) = code_rank::definition(n, &s.body, first) {
                    if strength > best.map_or(0, |(_, _, s)| s) {
                        best = Some((s.chunk, line, strength));
                    }
                    if strength == 2 {
                        break;
                    }
                }
            }
            if let Some((chunk, line, _)) = best {
                let def_locator = all
                    .iter()
                    .find(|s| s.chunk == chunk)
                    .and_then(|s| s.locator.clone());
                let same = row.locator.is_some() && row.locator == def_locator
                    || row
                        .chunk
                        .as_ref()
                        .is_some_and(|(h, c)| h == hash && *c == chunk);
                row.chunk = Some((hash.clone(), chunk));
                row.locator = Some(with_line(def_locator.as_deref(), line));
                if !same {
                    row.snippet = None; // the words matched elsewhere; no excerpt of this stretch
                }
                continue;
            }
        }
        // 2. The first line of the pointed stretch carrying the query's words.
        if !(*words_here || name.is_some()) {
            continue;
        }
        let words: Vec<String> = match name {
            Some(n) => vec![n.to_lowercase()],
            None => terms.to_vec(),
        };
        let pointed = match row
            .chunk
            .as_ref()
            .filter(|(h, _)| h == hash)
            .and_then(|(_, c)| all.iter().find(|s| s.chunk == *c))
            .or_else(|| all.iter().find(|s| Some(&s.locator) == Some(&row.locator)))
        {
            Some(s) => s,
            None => continue,
        };
        let first = locator_first_line(pointed.locator.as_deref()).unwrap_or(1);
        if let Some(line) = code_rank::first_line(&words, &pointed.body, first) {
            if line != first {
                row.locator = Some(with_line(row.locator.as_deref(), line));
            }
        }
    }
}

/// The saved performance profile (`balanced` when unset or unreadable).
pub fn stored_profile(store: &Store) -> Profile {
    store
        .get_setting(SETTING_PROFILE)
        .ok()
        .flatten()
        .and_then(|s| Profile::parse(&s))
        .unwrap_or_default()
}

/// Size + mtime for the UI's date / size chips.
fn stat(path: &str) -> (Option<u64>, Option<i64>) {
    match std::fs::metadata(path) {
        Ok(m) => (
            m.is_file().then_some(m.len()),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64),
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
    /// The reranker refined this list (some hit carries `rerank`): the UI shows "Refined".
    pub reranked: bool,
}

impl SemanticResult {
    pub fn new(state: impl Into<String>, hits: Vec<SemanticHit>) -> Self {
        Self {
            state: state.into(),
            total: hits.len(),
            more: false,
            reranked: false,
            hits,
        }
    }
}

/// The query's embedding and the hub's, what a meaning search is built on.
type QueryVectors = (Vec<f32>, Vec<f32>);

/// A meaning / text hit before the per-file work (stat, highlight boxes) a returned page pays for.
#[derive(Clone)]
struct SemRow {
    path: String,
    /// Content hash + chunk of the best-matching chunk (meaning hits only): where its text is stored.
    chunk: Option<(String, u32)>,
    /// The reranker's relevance, 0..1.
    rerank: Option<f32>,
    score: f32,
    /// `score` before [`finalize_scores`] made it relative.
    rank: f32,
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
            rank: self.rank,
            size,
            mtime,
            snippet: self.snippet.clone(),
            page: self.page,
            at: media::locator_time(self.locator.as_deref()),
            source: self.source,
            boxes: hitbox::hit_boxes(self.locator.as_deref(), q),
            rerank: self.rerank,
            line: locator_code_line(self.locator.as_deref()),
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
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
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
    pub fn new(
        store: Store,
        embedder: Box<dyn Embedder + Send>,
        parser: Box<dyn Parser + Send>,
        scopes: Scopes,
    ) -> Arc<Tier1> {
        let ignore = Arc::new(Ignore::new(
            &store.get_ignore().unwrap_or_else(|_| default_ignore()),
        ));
        let index_code = store.get_index_code().unwrap_or(false);
        Arc::new(Tier1 {
            store: Mutex::new(store),
            embedder: Mutex::new(embedder),
            parser: Mutex::new(parser),
            reranker: Mutex::new(Box::new(NoReranker)),
            doc_cache: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false),
            user_paused: AtomicBool::new(false),
            auto_pause: AtomicU8::new(0),
            wake: (Mutex::new(false), Condvar::new()),
            state: AtomicU8::new(ST_IDLE),
            snap: RwLock::new(Snapshot {
                scopes,
                ignore,
                index_code,
                gen: 1,
            }),
            gen: AtomicU64::new(1),
            last_error: Mutex::new(None),
            last_recovery: Mutex::new(None),
            throughput: Mutex::new(progress::Throughput::default()),
            parse_throughput: Mutex::new(progress::Throughput::default()),
            parse_live: std::sync::Arc::new(Mutex::new(None)),
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
            Err(winseek_store::Error::MetaMismatch { stored, .. })
                if stored.same_but_build(&meta) && has_baseline(&path, &stored) =>
            {
                Store::open(&path, &stored).map_err(|e| format!("open store: {e}"))?
            }
            Err(winseek_store::Error::MetaMismatch { .. }) => {
                Store::reset(&path, &meta).map_err(|e| format!("reset store: {e}"))?
            }
            // Too damaged to open: keep what is readable, the rest is indexed again.
            Err(e) if e.is_corruption() => {
                let (s, backup, lost) = Store::salvage(&path, &meta, &Default::default())
                    .map_err(|e| format!("salvage store: {e}"))?;
                eprintln!(
                    "winseek: tier 1 store was damaged; kept {} and re-indexing {lost:?}",
                    backup.display()
                );
                s
            }
            Err(e) => return Err(format!("open store: {e}")),
        };
        let (scopes, defaulted) = scopes::effective(store.get_scopes().map_err(|e| e.to_string())?);
        // The saved profile applies to both GPU models; they share one phase tracker.
        let profile = stored_profile(&store);
        embedder.set_profile(profile);
        let gpu = embedder.gpu_phase();
        let parser = LlamaParser::new(root.clone(), manifest.clone(), profile, gpu.clone());
        let t = Tier1::new(store, Box::new(embedder), Box::new(parser), scopes.clone());
        t.set_reranker(Box::new(LlamaReranker::new(root, manifest, profile, gpu)));
        if defaulted {
            let _ = lock(&t.store).set_scopes(&scopes);
        }
        t.check_code_chunker();
        t.note_index_stamp(false); // the status page shows which version last verified the index
        Ok(t)
    }

    /// The code chunker changed since the code index was built: forget the indexed code files once
    /// so the crawler re-chunks and re-embeds them under the current stretch shape (DigUp's
    /// fingerprint gate). Without this, old char-offset and new line locators would mix.
    fn check_code_chunker(&self) {
        let mut s = lock(&self.store);
        if !s.get_index_code().unwrap_or(false) {
            return; // nothing indexed under any chunker
        }
        let stored = s
            .get_setting(winseek_store::SETTING_CODE_CHUNKER)
            .ok()
            .flatten();
        if stored.as_deref() == Some(chunk::CODE_CHUNKER_FINGERPRINT) {
            return;
        }
        match s.remove_kind(crawler::KIND_CODE) {
            Ok(n) => eprintln!(
                "winseek: code chunker changed ({} -> {}); {n} code file(s) re-index with line locators",
                stored.as_deref().unwrap_or("none"),
                chunk::CODE_CHUNKER_FINGERPRINT
            ),
            Err(e) => eprintln!("winseek: could not forget code for re-chunking: {e}"),
        }
        if let Err(e) = s.set_setting(
            winseek_store::SETTING_CODE_CHUNKER,
            Some(chunk::CODE_CHUNKER_FINGERPRINT),
        ) {
            eprintln!("winseek: could not record the code chunker fingerprint: {e}");
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snap.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn apply_ops(&self, ops: &[feed::StoreOp]) -> usize {
        feed::real_apply(self, ops)
    }

    pub fn set_scopes(&self, include: Vec<String>, exclude: Vec<String>) -> Result<(), String> {
        // Unchecking parks the folder: its entry moves to the persisted `disabled` list, so
        // it stays visible (unchecked) across restarts and its rows stay in the store for an
        // instant re-check. Removal is a different command (`remove_folder`) and clears the
        // parked entry too. Checked wins over unchecked (scopes::normalize drops the stale one).
        let prev = self.snapshot().scopes;
        let parked: Vec<String> = prev
            .include
            .iter()
            .filter(|p| {
                let lp = scopes::normalize_root(p).to_ascii_lowercase();
                !include
                    .iter()
                    .any(|i| scopes::normalize_root(i).to_ascii_lowercase() == lp)
            })
            .cloned()
            .collect();
        self.persist_scopes(scopes::normalize(Scopes {
            include,
            exclude,
            disabled: [prev.disabled, parked].concat(),
            ..Default::default()
        }))
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
        let listed = |list: &[String]| {
            list.iter()
                .any(|i| scopes::normalize_root(i).to_ascii_lowercase() == lower)
        };
        let (was_included, was_disabled) =
            (listed(&snap.scopes.include), listed(&snap.scopes.disabled));
        if was_included || was_disabled {
            let keep = |list: &[String]| {
                list.iter()
                    .filter(|i| scopes::normalize_root(i).to_ascii_lowercase() != lower)
                    .cloned()
                    .collect::<Vec<String>>()
            };
            self.persist_scopes(Scopes {
                include: keep(&snap.scopes.include),
                exclude: snap.scopes.exclude.clone(),
                disabled: keep(&snap.scopes.disabled),
                ..Default::default()
            })?;
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
        lock(&self.store)
            .set_scopes(&s)
            .map_err(|e| e.to_string())?;
        let mut snap = self.snap.write().unwrap_or_else(|e| e.into_inner());
        snap.scopes = s;
        snap.gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(())
    }

    /// The current ignore list plus the built-in defaults (for "Reset to defaults" diffing).
    pub fn get_ignore(&self) -> Result<IgnoreDto, String> {
        let list = lock(&self.store).get_ignore().map_err(|e| e.to_string())?;
        Ok(IgnoreDto {
            list,
            defaults: default_ignore(),
        })
    }

    /// Save the ignore list (`None` = back to defaults) and make the crawler re-run.
    pub fn set_ignore(&self, list: Option<Vec<String>>) -> Result<IgnoreDto, String> {
        let mut store = lock(&self.store);
        store
            .set_ignore(list.as_deref())
            .map_err(|e| e.to_string())?;
        let saved = store.get_ignore().map_err(|e| e.to_string())?;
        drop(store);
        let mut snap = self.snap.write().unwrap_or_else(|e| e.into_inner());
        snap.ignore = Arc::new(Ignore::new(&saved));
        snap.gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(IgnoreDto {
            list: saved,
            defaults: default_ignore(),
        })
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
                store
                    .remove_kind(crawler::KIND_CODE)
                    .map_err(|e| e.to_string())?;
            } else {
                // Nothing indexed under an older chunker can remain after a fresh opt-in.
                let _ = store.set_setting(
                    winseek_store::SETTING_CODE_CHUNKER,
                    Some(chunk::CODE_CHUNKER_FINGERPRINT),
                );
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
        if !p {
            self.kick(); // "Resume" starts the next batch now, not at the next poll
        }
    }

    /// Automatic pause from the policy poller (`None` resumes, unless the user paused).
    pub fn set_auto_pause(&self, r: Option<PauseReason>) {
        self.auto_pause
            .store(r.map_or(0, PauseReason::code), Ordering::SeqCst);
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
        policy::combine(
            self.user_paused.load(Ordering::SeqCst),
            PauseReason::from_code(self.auto_pause.load(Ordering::SeqCst)),
        )
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
            kicked = self
                .wake
                .1
                .wait_timeout_while(kicked, d, |k| !*k)
                .unwrap_or_else(|e| e.into_inner())
                .0;
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
            disabled: scopes.disabled,
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

    /// Progress of the current / last parse run, relative to when the queue filled (see
    /// [`winseek_store::ParseRun`]): "this burst of work", not the queue table's whole lifetime.
    pub fn parse_run(&self) -> Option<winseek_store::ParseRun> {
        lock(&self.store).parse_run().ok().flatten()
    }

    /// What the document parser is reading right now, as long as that entry is still fresh.
    pub fn parse_live(&self) -> Option<parse_pass::ParseLive> {
        const MAX_AGE: Duration = Duration::from_secs(600);
        let live = lock(&self.parse_live).clone();
        live.filter(|l| l.at.elapsed() < MAX_AGE)
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
            Err(std::sync::TryLockError::WouldBlock) => RuntimeInfo {
                running: true,
                unload_in_s: None,
                ..lock(&self.runtime).clone()
            },
        }
    }

    /// Document-parser server state (see [`runtime_info`](Self::runtime_info): a parse in progress
    /// means the server is up).
    pub fn parser_runtime(&self) -> RuntimeInfo {
        match self.parser.try_lock() {
            Ok(mut p) => p.runtime_info(),
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().runtime_info(),
            Err(std::sync::TryLockError::WouldBlock) => RuntimeInfo {
                running: true,
                port: None,
                unload_in_s: None,
            },
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
        lock(&self.store)
            .set_setting(SETTING_PROFILE, Some(p.as_str()))
            .map_err(|e| e.to_string())?;
        lock(&self.embedder).set_profile(p);
        lock(&self.parser).set_profile(p);
        lock(&self.reranker).set_profile(p);
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
        let bytes = guard::read_guarded(std::path::Path::new(path))
            .map_err(|e| format!("Cannot read file: {e}"))?;
        let key = (process::sha256_hex(&bytes), page);
        if let Some(mut d) = self.cached_doc(&key) {
            d.cached = true;
            return Ok(d);
        }
        let jpeg = if page == 0 {
            let bytes = heic::normalize(path, bytes.clone())
                .ok_or("This HEIC photo cannot be decoded: the Windows HEIF codec is missing.")?;
            prepare_jpeg_long(&bytes, PARSE_LONG_SIDE)?
        } else {
            let rt = pdf::runtime().map_err(|e| e.to_string())?;
            let mut pages = pdf::render_pages(rt, &bytes, &[page]).map_err(|e| e.to_string())?;
            if pages.is_empty() {
                return Err(format!("Page {page} could not be rendered."));
            }
            pages.remove(0).1
        };
        let parsed =
            parser::parse_phased(&self.parser, &self.embedder, &jpeg).map_err(|e| e.to_string())?;
        let doc = ParsedDoc {
            markdown: parsed.markdown(),
            tables: parsed.tables(),
            elements_count: parsed.elements.len(),
            cached: false,
        };
        let mut cache = lock(&self.doc_cache);
        cache.retain(|(k, _)| *k != key);
        cache.push((key, doc.clone()));
        if cache.len() > DOC_CACHE {
            cache.remove(0);
        }
        Ok(doc)
    }

    fn cached_doc(&self, key: &(String, u32)) -> Option<ParsedDoc> {
        lock(&self.doc_cache)
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, d)| d.clone())
    }

    /// Reset the store (embeddings dropped, files back to pending) and make the crawler re-run.
    /// `reset` works on the live store under its lock, over its own connection: a second
    /// connection writing the same WAL file is what SQLite's WAL-reset race needs. Never touches
    /// Tier 0.
    pub fn rebuild_with(
        &self,
        reset: impl FnOnce(&mut Store) -> Result<(), String>,
    ) -> Result<(), String> {
        reset(&mut lock(&self.store))?;
        self.note_index_stamp(true);
        self.restart_indexing();
        Ok(())
    }

    /// Record which app version last opened / verified this index, and when its current content
    /// was created or re-stamped (both for the index-status row in Settings). `restamped` marks a
    /// fresh index or a re-stamp after a verified build change: the stamp time is renewed.
    /// Otherwise only the version moves, so "built" keeps meaning what it says.
    pub fn note_index_stamp(&self, restamped: bool) {
        let version = env!("CARGO_PKG_VERSION");
        let mut s = lock(&self.store);
        if s.get_setting(winseek_store::SETTING_INDEX_APP_VERSION)
            .ok()
            .flatten()
            .as_deref()
            != Some(version)
        {
            if let Err(e) = s.set_setting(winseek_store::SETTING_INDEX_APP_VERSION, Some(version)) {
                eprintln!("winseek: could not record the index app version: {e}");
            }
        }
        if restamped {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if let Err(e) = s.set_setting(
                winseek_store::SETTING_INDEX_STAMPED_UNIX,
                Some(&now.to_string()),
            ) {
                eprintln!("winseek: could not record the index stamp time: {e}");
            }
        }
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
                let bank = HUB_PROMPTS
                    .iter()
                    .map(|p| e.embed_query(p))
                    .collect::<Result<Vec<_>, _>>()?;
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
    pub fn semantic_page(
        &self,
        query: &str,
        offset: usize,
        k: usize,
    ) -> Result<SemanticResult, String> {
        let k = k.max(1);
        let q = query.trim();
        if q.is_empty() {
            return Ok(SemanticResult::new(self.state_str(), vec![]));
        }
        if offset == 0 && k <= SEM_K_SHALLOW {
            let (state, rows, more) = self.sem_rows(q, Some(k))?;
            let hits: Vec<SemanticHit> = rows.iter().map(|r| r.hit(q)).collect();
            return Ok(SemanticResult {
                state,
                total: hits.len(),
                more,
                reranked: rows.iter().any(|r| r.rerank.is_some()),
                hits,
            });
        }
        // A later page of the list computed a moment ago; the first page always recomputes
        // (files may have been indexed or deleted since).
        let kept = lock(&self.sem_all)
            .clone()
            .filter(|l| offset > 0 && l.query == q);
        let list = match kept {
            Some(l) => l,
            None => {
                let (state, rows, _) = self.sem_rows(q, None)?;
                let l = Arc::new(SemList {
                    query: q.to_string(),
                    state,
                    rows,
                });
                *lock(&self.sem_all) = Some(l.clone());
                l
            }
        };
        let hits: Vec<SemanticHit> = list
            .rows
            .iter()
            .skip(offset)
            .take(k)
            .map(|r| r.hit(q))
            .collect();
        Ok(SemanticResult {
            state: list.state.clone(),
            total: list.rows.len(),
            more: offset + hits.len() < list.rows.len(),
            reranked: list.rows.iter().any(|r| r.rerank.is_some()),
            hits,
        })
    }

    /// `code:` queries: a hybrid over the separate code index (source, notebooks, project prose).
    /// Two kinds of evidence, one score ([`code_rank`]): a file's best stretch standing out on the
    /// meaning scale among the code vectors, and how much of the query the file contains as words
    /// — an identifier is searched as one phrase, as written. Code is never part of any other
    /// search.
    fn code_rows(
        &self,
        q: &str,
        limit: Option<usize>,
    ) -> Result<(String, Vec<SemRow>, bool), String> {
        let snap = self.snapshot();
        if !snap.index_code || q.is_empty() {
            return Ok((self.state_str().into(), vec![], false));
        }
        // The query's shape decides the patterns: one identifier is a quoted phrase; words are
        // quoted terms, a word found in no file as typed also as a prefix.
        let id = code_rank::identifier(q);
        let terms = code_rank::terms(q);
        let patterns = match &id {
            Some(w) => vec![code_rank::phrase(w)],
            None => {
                let store = lock(&self.store);
                code_rank::word_patterns(&terms, &|p| store.code_has_pattern(p).unwrap_or(false))
            }
        };
        if patterns.is_empty() {
            return Ok((self.state_str().into(), vec![], false));
        }
        // Words side: which stretches match (best bm25 first), and how much of the query each
        // file answers anywhere in it.
        let words: Vec<CodeWordsHit> = {
            let store = lock(&self.store);
            store
                .search_code_words(&patterns.join(" OR "), limit.map_or(TEXT_ALL, |k| k * 8))
                .unwrap_or_default()
        }
        .into_iter()
        .filter(|h| snap.in_scope(&h.path))
        .collect();
        let mut best_words: std::collections::HashMap<&str, &CodeWordsHit> =
            std::collections::HashMap::new();
        for h in &words {
            best_words.entry(h.hash.as_str()).or_insert(h);
        }
        let lookup = code_rank::is_lookup(&terms) || id.is_some();
        // The identifier as written ("()" stripped): what the definition pass looks for.
        let name = id
            .as_ref()
            .map(|_| q.trim().strip_suffix("()").unwrap_or(q.trim()));
        let mut coverage: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
        if !words.is_empty() {
            let mut hashes: Vec<String> = words.iter().map(|h| h.hash.clone()).collect();
            hashes.sort();
            hashes.dedup();
            let pats: Vec<&str> = patterns.iter().map(String::as_str).collect();
            coverage = lock(&self.store)
                .code_coverage(&pats, &hashes)
                .unwrap_or_default();
        }
        // Meaning side: the best stretch per code file on the relevance scale (the code population
        // only). No gate here: the words side can carry a file the meaning gate would refuse.
        let cands = match self.embed_code_query_vectors(q)? {
            Some((vec, hub)) => lock(&self.store)
                .search_code_candidates(&vec, &hub)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut rows: Vec<SemRow> = Vec::new();
        // Per row, the file's hash and whether the words matched the pointed stretch (the pass
        // that decides where a hit opens needs both).
        let mut focus: Vec<(String, bool)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for c in &cands {
            let key = c.path.to_lowercase();
            if !seen.insert(key) {
                continue;
            }
            let cov = coverage.get(&c.hash).copied().unwrap_or(0.0);
            // A file qualifies by standing out in meaning, or by answering half the query.
            if !c.passes() && cov < code_rank::COVERAGE_CANDIDATE {
                continue;
            }
            // Point at the meaning stretch when it stands out; else at the stretch the words
            // matched (its snippet and line locator).
            let by_meaning = c.passes();
            let hit_w = (!by_meaning)
                .then(|| best_words.get(c.hash.as_str()).copied())
                .flatten();
            let score = code_rank::score(c.z, cov, lookup);
            focus.push((c.hash.clone(), !by_meaning));
            rows.push(SemRow {
                page: None,
                path: c.path.clone(),
                chunk: Some((c.hash.clone(), c.chunk)),
                rerank: None,
                score,
                rank: score,
                locator: if by_meaning {
                    c.locator.clone()
                } else {
                    hit_w.and_then(|w| w.locator.clone())
                },
                snippet: if by_meaning {
                    None
                } else {
                    hit_w.map(|w| w.snippet.clone()).filter(|s| s.contains('['))
                },
                source: "code",
            });
        }
        // Files answered by their words only (no vector: models missing or embedding owed).
        for h in &words {
            let key = h.path.to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            let cov = coverage.get(&h.hash).copied().unwrap_or(0.0);
            if cov < code_rank::COVERAGE_CANDIDATE {
                continue;
            }
            seen.insert(key);
            let score = code_rank::score(None, cov, lookup);
            focus.push((h.hash.clone(), true));
            rows.push(SemRow {
                page: None,
                path: h.path.clone(),
                chunk: None,
                rerank: None,
                score,
                rank: score,
                locator: h.locator.clone(),
                snippet: Some(h.snippet.clone()).filter(|s| s.contains('[')),
                source: "code",
            });
        }
        let mut paired: Vec<(SemRow, String, bool)> = rows
            .into_iter()
            .zip(focus)
            .map(|(r, (h, w))| (r, h, w))
            .collect();
        paired.sort_by(|a, b| {
            b.0.rank
                .total_cmp(&a.0.rank)
                .then_with(|| a.0.path.cmp(&b.0.path))
        });
        let more = limit.is_some_and(|k| paired.len() > k);
        paired.truncate(limit.unwrap_or(usize::MAX));
        // Only what is shown pays for the stretch reads that decide where it opens.
        code_focus(&mut paired, name, &terms, &|hash| {
            lock(&self.store).code_stretches(hash).unwrap_or_default()
        });
        let mut rows: Vec<SemRow> = paired.into_iter().map(|(r, _, _)| r).collect();
        finalize_scores(&mut rows);
        Ok((self.state_str().into(), rows, more))
    }

    /// Install another reranker (tests inject a fake; `open_default` the llama-backed one).
    pub(crate) fn set_reranker(&self, r: Box<dyn Reranker + Send>) {
        *lock(&self.reranker) = r;
    }

    /// The user's switch: everything but an explicit `0` is on.
    pub fn rerank_setting(&self) -> bool {
        lock(&self.store)
            .get_setting(SETTING_RERANK_ENABLED)
            .ok()
            .flatten()
            .is_none_or(|v| v != "0")
    }

    /// Switch the reranker on / off (persisted). Off unloads the server; on starts loading it.
    pub fn set_rerank_enabled(&self, on: bool) -> Result<(), String> {
        lock(&self.store)
            .set_setting(SETTING_RERANK_ENABLED, Some(if on { "1" } else { "0" }))
            .map_err(|e| e.to_string())?;
        let mut r = lock(&self.reranker);
        if on {
            r.warm();
        } else {
            r.unload();
        }
        Ok(())
    }

    /// `{present, enabled, state, reason}` for the status page. `present` = model file + llama build on disk;
    /// `enabled` = present and not switched off; `state` = "off" | "warming" | "ready" | "error".
    pub fn rerank_status(&self) -> RerankStatus {
        let setting = self.rerank_setting();
        match self.reranker.try_lock() {
            Ok(r) => RerankStatus::of(r.present(), setting, r.state(), r.reason()),
            // A rerank call is in flight: present, loaded and working.
            Err(std::sync::TryLockError::WouldBlock) => {
                RerankStatus::of(true, setting, "ready", None)
            }
            Err(std::sync::TryLockError::Poisoned(p)) => {
                let r = p.into_inner();
                RerankStatus::of(r.present(), setting, r.state(), r.reason())
            }
        }
    }

    /// Refine the order of the best meaning hits with the reranker, in place. Never fails the
    /// search: any problem (switched off, not installed, still warming up, timeout, bad reply)
    /// leaves `rows` exactly as they were. Candidates are the first `RERANK_TOP_DOCS` meaning rows;
    /// only those whose best chunk has stored text are scored.
    fn rerank_rows(&self, q: &str, rows: &mut [SemRow]) {
        if q.is_empty() || !self.rerank_setting() {
            return;
        }
        let top = RERANK_TOP_DOCS;
        let cand: Vec<usize> = (0..rows.len())
            .filter(|&i| rows[i].source == "meaning")
            .take(top)
            .collect();
        let texts: Vec<Option<String>> = {
            let store = lock(&self.store);
            cand.iter()
                .map(|&i| {
                    let (hash, chunk) = rows[i].chunk.as_ref()?;
                    let body = store.chunk_text(hash, *chunk).ok().flatten()?;
                    let doc = clip_chars(body.trim(), RERANK_DOC_CHARS);
                    (!doc.is_empty()).then_some(doc)
                })
                .collect()
        };
        let (with_text, docs): (Vec<usize>, Vec<String>) = cand
            .iter()
            .zip(texts)
            .filter_map(|(&i, t)| Some((i, t?)))
            .unzip();
        if docs.len() < RERANK_MIN_DOCS {
            return;
        }
        // A rerank already in flight (another search) is not waited for.
        let Ok(mut r) = self.reranker.try_lock() else {
            return;
        };
        if !r.present() {
            return;
        }
        let scores = match r.rerank(q, &docs) {
            Ok(s) if s.len() == docs.len() => s,
            _ => return,
        };
        drop(r);
        apply_rerank(rows, &cand, &with_text, &scores);
    }

    /// The query's vector and the hub vector: `None` without models (text search answers alone);
    /// `Err` on a real embedder failure.
    fn embed_query_vectors(&self, q: &str) -> Result<Option<QueryVectors>, String> {
        let mut e = lock(&self.embedder);
        if !e.models_present() {
            return Ok(None);
        }
        match e
            .embed_query(q)
            .and_then(|v| Ok((v, self.hub_vector(&mut e)?)))
        {
            Ok(v) => Ok(Some(v)),
            Err(EmbedError::ModelsMissing) => Ok(None),
            Err(EmbedError::Other(m)) => Err(m),
        }
    }

    /// The code query's vector and the hub vector: `None` without models (text search answers
    /// alone); `Err` on a real embedder failure. The query embeds in the code retrieval task
    /// form — the same vector space the code corpus is embedded in (DigUp's measured pairing).
    fn embed_code_query_vectors(&self, q: &str) -> Result<Option<QueryVectors>, String> {
        let mut e = lock(&self.embedder);
        if !e.models_present() {
            return Ok(None);
        }
        match e
            .embed_code_query(q)
            .and_then(|v| Ok((v, self.hub_vector(&mut e)?)))
        {
            Ok(v) => Ok(Some(v)),
            Err(EmbedError::ModelsMissing) => Ok(None),
            Err(EmbedError::Other(m)) => Err(m),
        }
    }

    /// The hits for `q` before the per-file work: `(state, rows, more exist)`. `limit` = the
    /// top-k of the overview; `None` = every relevant file. Without models the query words found
    /// in extracted text (documents, PDFs, screenshot OCR) answer alone, state `models_missing`.
    fn sem_rows(
        &self,
        q: &str,
        limit: Option<usize>,
    ) -> Result<(String, Vec<SemRow>, bool), String> {
        if let Some(cq) = code_query(q) {
            return self.code_rows(cq, limit);
        }
        let text_k = limit.map_or(TEXT_ALL, |k| k * 4);
        let embedded = self.embed_query_vectors(q)?;
        let snap = self.snapshot();
        let Some((vec, hub)) = embedded else {
            let raw = lock(&self.store)
                .search_text(&fts_escape(q), text_k)
                .unwrap_or_default();
            let mut rows: Vec<SemRow> = raw
                .into_iter()
                .filter(|h| snap.in_scope(&h.path))
                .map(|h| SemRow {
                    page: locator_page(h.locator.as_deref()),
                    path: h.path,
                    chunk: None,
                    rerank: None,
                    score: 0.0,
                    rank: 0.0,
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
            let texts = store
                .search_text(&fts_escape(q), text_k)
                .unwrap_or_default();
            (cands, texts)
        };
        let in_scope: Vec<_> = cands
            .into_iter()
            .filter(|c| snap.in_scope(&c.path))
            .collect();
        let gated = relevance::gate(in_scope, usize::MAX);
        let text_pages: std::collections::HashMap<String, u32> = texts
            .iter()
            .filter_map(|t| Some((t.path.to_lowercase(), locator_page(t.locator.as_deref())?)))
            .collect();
        let snippets: std::collections::HashMap<String, String> = texts
            .iter()
            .map(|t| (t.path.to_lowercase(), t.snippet.clone()))
            .collect();
        let mut seen = std::collections::HashSet::new();
        // The words found in extracted text, per file: a second kind of evidence (DigUp's "words bonus").
        let text_keys: Vec<String> = texts
            .iter()
            .filter(|t| snap.in_scope(&t.path) && t.snippet.contains('['))
            .map(|t| t.path.to_lowercase())
            .collect();
        let lean = winseek_store::fuse::exact_lean(q);
        let words = words_bonus(lean);
        let kinds = relevance::kind_hints(q);
        let has_words: std::collections::HashSet<&str> =
            text_keys.iter().map(String::as_str).collect();
        let mut rows: Vec<SemRow> = gated
            .into_iter()
            .map(|h| {
                let key = h.path.to_lowercase();
                let snippet = snippets.get(&key).filter(|s| s.contains('[')).cloned();
                let page =
                    locator_page(h.locator.as_deref()).or_else(|| text_pages.get(&key).copied());
                // One final score per file: how far it stands out for the query (the gate's z), plus the words
                // bonus, plus 1 when the query names this kind of file ("songs about ...": DigUp's kind hint,
                // a soft boost, not a filter).
                let kind = if kinds.contains(&h.modality) {
                    1.0
                } else {
                    0.0
                };
                let score = h.rank()
                    + kind
                    + if has_words.contains(key.as_str()) {
                        words
                    } else {
                        0.0
                    };
                seen.insert(key);
                SemRow {
                    chunk: Some((h.hash, h.chunk)),
                    rerank: None,
                    path: h.path,
                    score,
                    rank: score,
                    locator: h.locator,
                    snippet,
                    page,
                    source: "meaning",
                }
            })
            .collect();
        // The words themselves, found inside files (documents, PDFs, screenshot text), fused with
        // the meaning list: a literal hit is separate evidence and is not held to the meaning gate.
        for t in texts
            .into_iter()
            .filter(|t| snap.in_scope(&t.path) && t.snippet.contains('['))
        {
            if seen.contains(&t.path.to_lowercase()) {
                continue;
            }
            seen.insert(t.path.to_lowercase());
            rows.push(SemRow {
                page: locator_page(t.locator.as_deref()),
                path: t.path,
                chunk: None,
                rerank: None,
                score: words,
                rank: words,
                locator: t.locator,
                snippet: Some(t.snippet),
                source: "text",
            });
        }
        let sem_keys: Vec<String> = rows
            .iter()
            .filter(|r| r.source == "meaning")
            .map(|r| r.path.to_lowercase())
            .collect();
        let order = winseek_store::fuse::fuse_order(
            &sem_keys,
            &text_keys,
            winseek_store::fuse::exact_lean(q),
        );
        let pos: std::collections::HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(i, k)| (k.as_str(), i))
            .collect();
        rows.sort_by_key(|r| {
            pos.get(r.path.to_lowercase().as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        self.rerank_rows(q, &mut rows);
        finalize_scores(&mut rows);
        let more = limit.is_some_and(|k| top_per_kind(&mut rows, k));
        Ok((self.state_str().into(), rows, more))
    }
}

/// What the status page shows about the reranker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RerankStatus {
    /// Model file and a llama build are on disk.
    pub present: bool,
    /// Present and not switched off: searches try to refine their order.
    pub enabled: bool,
    /// "off" | "warming" | "ready" | "error"
    pub state: &'static str,
    /// Why it cannot run on this machine ("needs a graphics card"): then never enabled, state "off".
    pub reason: Option<&'static str>,
}

impl RerankStatus {
    fn of(
        present: bool,
        setting: bool,
        state: &'static str,
        reason: Option<&'static str>,
    ) -> RerankStatus {
        let enabled = present && setting && reason.is_none();
        RerankStatus {
            present,
            enabled,
            state: if enabled { state } else { "off" },
            reason,
        }
    }
}

/// The first `max` characters of `s` (on a character boundary).
fn clip_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Blend the reranker's probabilities into the scores of the candidate rows and re-sort them.
///
/// `cand` = positions of the candidate rows (ascending), `scored` = those of them that were
/// scored, `probs` = their reranker probabilities (0..1). The existing score is a z-like number
/// (unbounded above, words bonus included), the probability lives in 0..1, so the old score is
/// first normalised by the best candidate's (0..1) and the two are averaged 50/50; the average
/// is then scaled back by that best score so it stays on the scale of the rows that were not
/// scored (text-only hits, rows beyond the candidates, rows without text). Candidates keep their
/// slots in the list; only the order among them changes, ties keep the old order. Rows that were
/// not scored (images, audio) keep their score and take part in the re-sort.
fn apply_rerank(rows: &mut [SemRow], cand: &[usize], scored: &[usize], probs: &[f32]) {
    let top = cand.iter().map(|&i| rows[i].score).fold(0.0f32, f32::max);
    if top <= 0.0 || scored.len() != probs.len() {
        return;
    }
    for (&i, &p) in scored.iter().zip(probs) {
        let p = if p.is_finite() {
            p.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let norm = (rows[i].score / top).clamp(0.0, 1.0);
        let blended = (0.5 * norm + 0.5 * p) * top;
        rows[i].score = blended;
        rows[i].rank = blended;
        rows[i].rerank = Some(p);
    }
    let mut order: Vec<usize> = cand.to_vec();
    order.sort_by(|&a, &b| rows[b].score.total_cmp(&rows[a].score)); // stable
    let sorted: Vec<SemRow> = order.iter().map(|&i| rows[i].clone()).collect();
    for (&slot, row) in cand.iter().zip(sorted) {
        rows[slot] = row;
    }
}

/// Switch the reranker on / off. Returns the new status.
#[tauri::command]
pub async fn rerank_set_enabled(
    h: tauri::State<'_, Handle>,
    enabled: bool,
) -> Result<RerankStatus, String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.set_rerank_enabled(enabled).map(|_| t.rerank_status()))
        .await?
}

/// The overview's quick list: the best `k` meaning hits among the pictures and the best `k` among
/// everything else, each followed by a few words-only hits (at most `k + max(k / 3, 3)` rows per
/// kind). Pictures and documents are shown in separate sections, so one kind must not use up the
/// other's places: the picture section then starts with the same pictures as "Show all". Rows stay
/// in their order. Returns whether anything was left out.
fn top_per_kind(rows: &mut Vec<SemRow>, k: usize) -> bool {
    let cap = k + (k / 3).max(3);
    let (mut meaning, mut all) = ([0usize; 2], [0usize; 2]);
    let before = rows.len();
    rows.retain(|r| {
        let g = usize::from(crawler::is_supported_image(&r.path));
        let keep = if r.source == "meaning" {
            meaning[g] < k
        } else {
            all[g] < cap
        };
        if keep {
            all[g] += 1;
            if r.source == "meaning" {
                meaning[g] += 1;
            }
        }
        keep
    });
    rows.len() < before
}

/// Words bonus of the final score, on the gate's z scale (meaning hits sit at `Z_MIN` = 2 and up).
/// Plain queries: a small bonus (a file also found by its words nudges up; a words-only file is a weak
/// match). Code-like queries (invoice number, error code): large, so literal hits lead.
fn words_bonus(lean: f32) -> f32 {
    1.0 + 11.0 * lean.clamp(0.0, 1.0)
}

/// Turn the final scores into what the UI shows, relative to the best result of the list: the first
/// row is 1.0 (100%), every other row `score / top` clamped to 0.01..=0.99, and never higher than the
/// row above (a running minimum), so the badge reads high to low whatever the fusion order was.
/// Rows are already in final order. Words-only rows after models-missing carry score 0 and stay at 0.
fn finalize_scores(rows: &mut [SemRow]) {
    let top = rows.first().map_or(0.0, |r| r.score);
    if top <= 0.0 {
        return;
    }
    let mut cap = 1.0f32;
    for (i, r) in rows.iter_mut().enumerate() {
        let rel = if i == 0 {
            1.0
        } else {
            (r.score / top).clamp(0.01, 0.99)
        };
        r.score = rel.min(cap);
        cap = r.score;
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
        Box::new(move || {
            crate::models::request_pdfium({
                let t = t.clone();
                move |ok| {
                    if ok {
                        t.kick();
                    }
                }
            })
        }) as Box<dyn Fn() + Send>
    };
    let media_requester = {
        let t = t.clone();
        Box::new(move || {
            crate::models::request_ffmpeg({
                let t = t.clone();
                move |ok| {
                    if ok {
                        t.kick();
                    }
                }
            })
        }) as Box<dyn Fn() + Send>
    };
    let mut proc = Processor::default()
        .with_pdf_requester(requester)
        .with_media_requester(media_requester);
    let mut parse_pass = ParsePass::default().with_live(t.parse_live.clone());
    let (mut prov_done, mut prov_tries) = (false, 0u32);
    let mut last_media_gate: Option<std::time::Instant> = None;
    loop {
        // Rows an older build parked for a missing ffmpeg return to the queue once one is usable.
        if last_media_gate.is_none_or(|i| i.elapsed() >= Duration::from_secs(10)) {
            proc.sync_media_gate(&t.store);
            last_media_gate = Some(std::time::Instant::now());
        }
        let have_models = {
            let mut e = lock(&t.embedder);
            e.tick_idle();
            e.models_present()
        };
        if let Ok(mut p) = t.parser.try_lock() {
            p.tick_idle(); // a parse in progress (preview) is not idle
        }
        if let Ok(mut r) = t.reranker.try_lock() {
            r.tick_idle();
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
                    Ok(Pass::Idle) => {
                        match parse_pass.run(&t.store, &t.embedder, &t.parser, &t.paused) {
                            Ok(n) if n > 0 => {
                                lock(&t.parse_throughput).record(
                                    started,
                                    std::time::Instant::now(),
                                    n,
                                );
                                t.state.store(ST_INDEXING, Ordering::Relaxed);
                                *lock(&t.last_error) = None;
                                continue;
                            }
                            // Everything else is done: the rest of long documents, a slice at a time.
                            Ok(_) => proc.run_tail_pass(&t.store, &t.embedder, &t.paused),
                            Err(msg) => Err(msg),
                        }
                    }
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
    Store::open(path, stamp)
        .and_then(|s| s.ref_vectors())
        .is_ok_and(|(_, v)| !v.is_empty())
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
    // The verdict is mirrored into a store setting so the Settings page can tell the user, after
    // an update, whether their index carried over or had to be rebuilt (status.rs::index_section).
    let verdict = |v: String| {
        if let Err(e) =
            lock(&t.store).set_setting(winseek_store::SETTING_PROVENANCE_VERDICT, Some(&v))
        {
            eprintln!("winseek: could not record the provenance verdict: {e}");
        }
    };
    let rebuild = |why: String| {
        eprintln!("winseek: {why}; re-embedding the index");
        let meta = lock(&t.embedder).meta();
        if let Err(e) = t.rebuild_with(|s| s.reset_in_place(&meta).map_err(|e| e.to_string())) {
            t.report_error(e);
        }
    };
    match provenance::check(&t.store, &t.embedder) {
        Ok(Outcome::Current) => {
            verdict("current".into());
            *done = true;
            true
        }
        Ok(Outcome::Baselined) => {
            eprintln!(
                "winseek: index stamped with {} and reference vectors stored",
                lock(&t.store).stamp().llama_build
            );
            verdict("baselined".into());
            *done = true;
            true
        }
        Ok(Outcome::Kept(c)) => {
            eprintln!("winseek: llama.cpp build changed; reference vectors unchanged (cos {c:.5}), index kept");
            verdict(format!("kept:{c:.5}"));
            t.note_index_stamp(true); // the check re-stamped the index for this build
            *done = true;
            true
        }
        Ok(Outcome::Rebuild(c)) => {
            verdict(match c {
                Some(c) => format!("moved:{c:.5}"),
                None => "no_baseline".into(),
            });
            rebuild(match c {
                Some(c) => format!(
                    "llama.cpp build changed and vectors moved (cos {c:.5} < {})",
                    winseek_store::PROBE_MIN_COS
                ),
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
                verdict("unverified".into());
                rebuild(format!(
                    "could not verify vectors after a llama.cpp build change ({e})"
                ));
                false
            } else {
                verdict("unverified".into());
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
        let _ = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || f(t));
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
        Err(e) => Ok(StatusDto {
            state: "unavailable".into(),
            stats: None,
            include: vec![],
            exclude: vec![],
            disabled: vec![],
            error: Some(e),
            paused_reason: None,
        }),
    }
}

#[tauri::command]
pub async fn tier1_set_scopes(
    h: tauri::State<'_, Handle>,
    include: Vec<String>,
    exclude: Vec<String>,
) -> Result<(), String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.set_scopes(include, exclude)).await?
}

/// Remove a folder from indexing for good: out of the scopes, and its indexed files are
/// forgotten. Returns how many files were removed from the index.
#[tauri::command]
pub async fn tier1_remove_folder(
    h: tauri::State<'_, Handle>,
    path: String,
) -> Result<usize, String> {
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
pub async fn tier1_set_ignore(
    h: tauri::State<'_, Handle>,
    list: Option<Vec<String>>,
) -> Result<IgnoreDto, String> {
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
pub async fn tier1_pause(
    app: tauri::AppHandle,
    h: tauri::State<'_, Handle>,
    paused: bool,
) -> Result<(), String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.set_paused(paused)).await?;
    crate::watcher::refresh_pause_item(&app);
    Ok(())
}

/// "Extract text" for an image or one PDF page (`page` is 1-based, ignored for images).
#[tauri::command]
pub async fn parse_document(
    h: tauri::State<'_, Handle>,
    path: String,
    page: Option<u32>,
) -> Result<ParsedDoc, String> {
    let t = h.get()?;
    tauri::async_runtime::spawn_blocking(move || t.parse_document(&path, page))
        .await
        .map_err(|e| e.to_string())?
}

/// Are the document parser's model files installed (the preview offers its actions only then)?
#[tauri::command]
pub async fn parser_present(h: tauri::State<'_, Handle>) -> Result<bool, String> {
    let t = h.get()?;
    tauri::async_runtime::spawn_blocking(move || t.parser_present())
        .await
        .map_err(|e| e.to_string())
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
    let saved = tauri::async_runtime::spawn_blocking(move || t.set_profile(p))
        .await
        .map_err(|e| e.to_string())??;
    Ok(saved.as_str().to_string())
}

#[tauri::command]
pub async fn semantic_search(
    h: tauri::State<'_, Handle>,
    query: String,
    k: usize,
    offset: Option<usize>,
) -> Result<SemanticResult, String> {
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
    tempfile::Builder::new()
        .prefix("t")
        .tempdir_in(base)
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srow(path: &str, score: f32, source: &'static str) -> SemRow {
        SemRow {
            path: path.into(),
            chunk: None,
            rerank: None,
            score,
            rank: score,
            locator: None,
            snippet: None,
            page: None,
            source,
        }
    }
    fn paths(rows: &[SemRow]) -> Vec<&str> {
        rows.iter().map(|r| r.path.as_str()).collect()
    }

    #[test]
    fn badge_is_relative_to_the_top_and_never_rises() {
        // Final order (z + words): 8.0, 9.0 (climbed by words, own z lower), 3.0, 0.5 weak.
        let mut rows = vec![
            srow("a", 8.0, "meaning"),
            srow("b", 9.0, "meaning"),
            srow("c", 4.0, "meaning"),
            srow("w", 1.0, "text"),
        ];
        finalize_scores(&mut rows);
        let got: Vec<i32> = rows
            .iter()
            .map(|r| (r.score * 100.0).round() as i32)
            .collect();
        assert_eq!(got, [100, 99, 50, 13]);
        assert!(rows.windows(2).all(|p| p[0].score >= p[1].score));
        assert_eq!(paths(&rows), ["a", "b", "c", "w"], "order is untouched");
    }

    #[test]
    fn quick_list_keeps_the_best_of_each_kind() {
        // Five documents outrank the pictures; with k = 2 the pictures still get their two places.
        let mut rows: Vec<SemRow> = (0..5)
            .map(|i| srow(&format!("d{i}.pdf"), 9.0 - i as f32, "meaning"))
            .collect();
        rows.extend((0..4).map(|i| srow(&format!("p{i}.jpg"), 4.9 - i as f32 * 0.1, "meaning")));
        rows.push(srow("w.txt", 1.0, "text"));
        assert!(top_per_kind(&mut rows, 2), "something was left out");
        assert_eq!(
            paths(&rows),
            ["d0.pdf", "d1.pdf", "p0.jpg", "p1.jpg", "w.txt"]
        );
        // Nothing over the limit: nothing dropped, "more" stays false.
        let mut rows = vec![srow("a.jpg", 5.0, "meaning"), srow("b.pdf", 4.5, "meaning")];
        assert!(!top_per_kind(&mut rows, 2));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn no_scores_means_nothing_to_normalise() {
        let mut rows = vec![srow("a", 0.0, "text"), srow("b", 0.0, "text")];
        finalize_scores(&mut rows);
        assert_eq!(rows.iter().map(|r| r.score).collect::<Vec<_>>(), [0.0, 0.0]);
    }

    #[test]
    fn code_queries_weight_the_words_more() {
        assert!(words_bonus(1.0) > words_bonus(0.0) * 5.0);
    }
    use embedder::{index_meta, DIMS};
    use winseek_store::{ChunkVec, FileKey, VecKind};

    struct Q;
    impl Embedder for Q {
        fn embed_images(&mut self, j: &[Vec<u8>]) -> embedder::Result<Vec<Vec<f32>>> {
            Ok(j.iter().map(|_| unit(0)).collect())
        }
        fn embed_query(&mut self, q: &str) -> embedder::Result<Vec<f32>> {
            // The relevance hub prompts get their own direction, unrelated to every test vector.
            Ok(unit(if HUB_PROMPTS.contains(&q) {
                3
            } else if q == "cat" {
                1
            } else {
                2
            }))
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

    fn tier_with(
        e: Box<dyn Embedder + Send>,
        p: Box<dyn Parser + Send>,
        include: &[&str],
    ) -> Arc<Tier1> {
        let store = Store::open_in_memory(&index_meta("test")).unwrap();
        Tier1::new(
            store,
            e,
            p,
            Scopes {
                include: include.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
        )
    }

    fn add_done(t: &Tier1, n: u64, path: &str, hot: usize) {
        let mut s = lock(&t.store);
        let id = s
            .upsert_file(
                &FileKey {
                    vol_guid: "{v}".into(),
                    frn: n,
                },
                path,
                1,
                1,
                "image",
            )
            .unwrap();
        s.complete(
            id,
            &format!("h{n}"),
            &[ChunkVec {
                chunk: 0,
                kind: VecKind::Image,
                locator: None,
                vec: unit(hot),
            }],
            &[],
        )
        .unwrap();
    }

    /// The live failure: SQLite reported "database disk image is malformed" for good. The worker's
    /// error path now repairs the store (settings kept), clears the error and re-crawls.
    #[test]
    fn corruption_error_repairs_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tier1.sqlite");
        {
            let t = Tier1::new(
                Store::open(&db, &index_meta("test")).unwrap(),
                Box::new(Q),
                Box::new(parser::NoParser),
                Scopes::default(),
            );
            t.set_scopes(vec![r"C:\Pics".into()], vec![]).unwrap();
            add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        } // closed: checkpointed
          // Garbage over the vectors root page, as a lost WAL frame leaves it.
        let (root, size): (i64, i64) = {
            let c = rusqlite::Connection::open(&db).unwrap();
            let root = c
                .query_row(
                    "SELECT rootpage FROM sqlite_master WHERE name = 'vectors'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (
                root,
                c.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap(),
            )
        };
        let mut bytes = std::fs::read(&db).unwrap();
        bytes[((root - 1) * size) as usize..][..64].fill(0xEE);
        std::fs::write(&db, bytes).unwrap();

        let t = Tier1::new(
            Store::open(&db, &index_meta("test")).unwrap(),
            Box::new(Q),
            Box::new(parser::NoParser),
            Scopes::default(),
        );
        let msg = lock(&t.store).max_vector_rowid().unwrap_err().to_string();
        assert!(winseek_store::is_corruption_message(&msg), "{msg}");
        let g0 = t.snapshot().gen;
        t.report_error(msg);
        assert_eq!(t.status().error, None);
        assert!(t.snapshot().gen > g0, "the crawler walks the scopes again");
        let s = lock(&t.store);
        assert_eq!(
            s.get_scopes().unwrap().include,
            vec![r"C:\Pics".to_string()]
        );
        assert_eq!(
            (s.stats().unwrap().pending, s.stats().unwrap().vectors),
            (1, 0)
        );
        assert!(dir.path().join("tier1.sqlite.corrupt").exists());
    }

    /// Diagnose a stalled crawl: run the real worker passes against a copy of the installed store.
    /// `WINSEEK_DIAG_DB=<copy of tier1.sqlite> cargo test -p winseek-app diag_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn diag_live() {
        let Some(db) = std::env::var_os("WINSEEK_DIAG_DB") else {
            return;
        };
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
            eprintln!(
                "pass {i}: upgrade {up:?} in {t1:?}; run {r:?} in {:?}; stats {:?}",
                t0.elapsed(),
                lock(&store).stats().ok()
            );
        }
    }

    #[test]
    fn semantic_search_ranks_and_filters_scope() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        add_done(&t, 2, r"C:\Pics\dog.jpg", 2);
        add_done(&t, 3, r"C:\Old\cat2.jpg", 1); // no longer in scope
        let r = t.semantic_search("cat", 5).unwrap();
        assert_eq!(
            r.hits.len(),
            1,
            "dog.jpg is unrelated to the query: gated out, not listed last"
        );
        assert_eq!(r.hits[0].path, r"C:\Pics\cat.jpg");
        assert!(r.hits[0].score > 0.99);
        assert!(t.semantic_search("  ", 5).unwrap().hits.is_empty());
    }

    // ---- reranker ------------------------------------------------------------------

    /// A done text file whose single chunk (vector `hot`) has `body` as its stored text.
    fn add_text_doc(t: &Tier1, n: u64, path: &str, hot: usize, body: &str) {
        let mut s = lock(&t.store);
        let id = s
            .upsert_file(
                &FileKey {
                    vol_guid: "{v}".into(),
                    frn: n,
                },
                path,
                1,
                1,
                "doc",
            )
            .unwrap();
        s.complete(
            id,
            &format!("h{n}"),
            &[ChunkVec {
                chunk: 0,
                kind: VecKind::Text,
                locator: None,
                vec: unit(hot),
            }],
            &[(0, body.to_string())],
        )
        .unwrap();
    }

    type Calls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

    /// Scores 0.9 for a document that contains "winner", 0.1 otherwise; remembers what it was given.
    struct FakeRerank {
        calls: Calls,
        fail: bool,
    }
    impl Reranker for FakeRerank {
        fn present(&self) -> bool {
            true
        }
        fn rerank(&mut self, q: &str, docs: &[String]) -> Result<Vec<f32>, reranker::RerankError> {
            self.calls.lock().unwrap().push((q.into(), docs.to_vec()));
            if self.fail {
                return Err(reranker::RerankError::Warming);
            }
            Ok(docs
                .iter()
                .map(|d| if d.contains("winner") { 0.9 } else { 0.1 })
                .collect())
        }
        fn tick_idle(&mut self) {}
        fn unload(&mut self) {}
        fn state(&self) -> &'static str {
            "ready"
        }
    }

    fn rerank_tier(n: u64, winner: u64) -> (Arc<Tier1>, Calls) {
        let t = tier(Box::new(Q), &[r"C:\Docs"]);
        for i in 0..n {
            let body = if i == winner {
                "the winner body".to_string()
            } else {
                format!("plain body {i}")
            };
            add_text_doc(&t, i + 1, &format!(r"C:\Docs\cat{i}.txt"), 1, &body);
        }
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        t.set_reranker(Box::new(FakeRerank {
            calls: calls.clone(),
            fail: false,
        }));
        (t, calls)
    }

    #[test]
    fn rerank_promotes_the_relevant_document_and_flags_the_result() {
        let (t, calls) = rerank_tier(5, 3);
        let r = t.semantic_search("cat", 5).unwrap();
        assert!(r.reranked);
        assert_eq!(r.hits[0].path, r"C:\Docs\cat3.txt", "{:?}", r.hits);
        assert_eq!(r.hits[0].rerank, Some(0.9));
        assert!(r.hits[1..].iter().all(|h| h.rerank == Some(0.1)));
        assert!(r.hits[0].score > 0.99 && r.hits[1].score < r.hits[0].score);
        {
            let c = calls.lock().unwrap();
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].0, "cat", "the plain query, no embedding prefix");
            assert_eq!(c[0].1.len(), 5);
            assert!(c[0].1.contains(&"the winner body".to_string()));
        }
        // "Show all" goes through the same step
        let all = t.semantic_page("cat", 0, SEM_K_SHALLOW + 1).unwrap();
        assert!(all.reranked);
        assert_eq!(all.hits[0].path, r"C:\Docs\cat3.txt");
    }

    #[test]
    fn rerank_failure_leaves_the_order_untouched() {
        let (t, calls) = rerank_tier(5, 3);
        t.set_reranker(Box::new(NoReranker));
        let base = t.semantic_search("cat", 5).unwrap();
        assert!(!base.reranked && base.hits.iter().all(|h| h.rerank.is_none()));
        t.set_reranker(Box::new(FakeRerank {
            calls: calls.clone(),
            fail: true,
        }));
        let r = t.semantic_search("cat", 5).unwrap();
        assert_eq!(calls.lock().unwrap().len(), 1, "it was asked, and failed");
        assert!(!r.reranked);
        assert_eq!(r.hits, base.hits);
    }

    #[test]
    fn rerank_is_skipped_when_off_or_too_few_documents() {
        let (t, calls) = rerank_tier(5, 3);
        assert!(t.rerank_setting(), "absent key = on");
        t.set_rerank_enabled(false).unwrap();
        assert!(!t.rerank_setting());
        assert!(!t.semantic_search("cat", 5).unwrap().reranked);
        assert!(calls.lock().unwrap().is_empty());
        t.set_rerank_enabled(true).unwrap();
        assert!(t.semantic_search("cat", 5).unwrap().reranked);
        assert_eq!(calls.lock().unwrap().len(), 1);

        let (few, calls) = rerank_tier(2, 1);
        assert!(!few.semantic_search("cat", 5).unwrap().reranked);
        assert!(calls.lock().unwrap().is_empty(), "under 3 candidates");
    }

    #[test]
    fn rerank_documents_are_clipped_and_images_without_text_are_not_sent() {
        let t = tier(Box::new(Q), &[r"C:\Docs"]);
        let long = "x".repeat(RERANK_DOC_CHARS * 2);
        for i in 0..3 {
            add_text_doc(&t, i + 1, &format!(r"C:\Docs\cat{i}.txt"), 1, &long);
        }
        add_done(&t, 9, r"C:\Docs\cat.jpg", 1); // an image: no stored text
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        t.set_reranker(Box::new(FakeRerank {
            calls: calls.clone(),
            fail: false,
        }));
        let r = t.semantic_search("cat", 10).unwrap();
        let c = calls.lock().unwrap();
        assert_eq!(c[0].1.len(), 3);
        assert!(c[0].1.iter().all(|d| d.chars().count() == RERANK_DOC_CHARS));
        let img = r.hits.iter().find(|h| h.path.ends_with("cat.jpg")).unwrap();
        assert_eq!(img.rerank, None, "no text, no rerank score");
    }

    #[test]
    fn rerank_needing_a_gpu_is_off_with_a_reason_and_leaves_the_order_alone() {
        struct NoGpu;
        impl Reranker for NoGpu {
            fn present(&self) -> bool {
                true
            }
            fn rerank(&mut self, _: &str, _: &[String]) -> Result<Vec<f32>, reranker::RerankError> {
                Err(reranker::RerankError::Unsupported)
            }
            fn tick_idle(&mut self) {}
            fn unload(&mut self) {}
            fn state(&self) -> &'static str {
                "off"
            }
            fn reason(&self) -> Option<&'static str> {
                Some(reranker::NEEDS_GPU)
            }
        }
        let (t, _) = rerank_tier(5, 3);
        t.set_reranker(Box::new(NoGpu));
        let s = t.rerank_status();
        assert_eq!(
            (s.present, s.enabled, s.state, s.reason),
            (true, false, "off", Some("needs a graphics card"))
        );
        assert!(!t.semantic_search("cat", 5).unwrap().reranked);
    }

    #[test]
    fn rerank_takes_the_top_documents_only() {
        let t = tier(Box::new(Q), &[r"C:\Docs"]);
        for i in 0..(RERANK_TOP_DOCS as u64 + 5) {
            add_text_doc(
                &t,
                i + 1,
                &format!(r"C:\Docs\cat{i}.txt"),
                1,
                &format!("body {i}"),
            );
        }
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        t.set_reranker(Box::new(FakeRerank {
            calls: calls.clone(),
            fail: false,
        }));
        t.semantic_page("cat", 0, SEM_K_SHALLOW + 1).unwrap();
        assert_eq!(calls.lock().unwrap()[0].1.len(), RERANK_TOP_DOCS);
    }

    #[test]
    fn rerank_status_follows_presence_and_the_switch() {
        let t = tier(Box::new(Q), &[]);
        let off = RerankStatus {
            present: false,
            enabled: false,
            state: "off",
            reason: None,
        };
        assert_eq!(t.rerank_status(), off, "NoReranker: not installed");
        let (t, _) = rerank_tier(1, 0);
        let s = t.rerank_status();
        assert_eq!((s.present, s.enabled, s.state), (true, true, "ready"));
        t.set_rerank_enabled(false).unwrap();
        let s = t.rerank_status();
        assert_eq!((s.present, s.enabled, s.state), (true, false, "off"));
    }

    fn row(path: &str, score: f32, text: bool) -> SemRow {
        SemRow {
            path: path.into(),
            chunk: text.then(|| ("h".into(), 0)),
            rerank: None,
            score,
            rank: score,
            locator: None,
            snippet: None,
            page: None,
            source: "meaning",
        }
    }

    #[test]
    fn apply_rerank_blends_half_and_half_on_the_old_scale() {
        // old z scores 4 / 3 / 2; the reranker says the last one is the answer
        let mut rows = vec![
            row("a", 4.0, true),
            row("b", 3.0, true),
            row("c", 2.0, true),
        ];
        apply_rerank(&mut rows, &[0, 1, 2], &[0, 1, 2], &[0.2, 0.2, 1.0]);
        let order: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        // a: (0.5*1 + 0.5*0.2)*4 = 2.4 ; b: (0.5*0.75 + 0.1)*4 = 1.9 ; c: (0.5*0.5 + 0.5)*4 = 3.0
        assert_eq!(order, ["c", "a", "b"]);
        assert!((rows[0].score - 3.0).abs() < 1e-5 && (rows[1].score - 2.4).abs() < 1e-5);
        assert_eq!(rows[0].rank, rows[0].score);
        assert_eq!(rows[0].rerank, Some(1.0));
    }

    #[test]
    fn apply_rerank_keeps_other_rows_and_unscored_scores() {
        let mut rows = vec![
            row("img", 4.0, false), // no text: keeps its score
            SemRow {
                source: "text",
                ..row("words", 1.0, false)
            },
            row("a", 3.0, true),
            row("b", 3.0, true),
            row("tail", 2.0, true), // beyond the candidates
        ];
        apply_rerank(&mut rows, &[0, 2, 3], &[2, 3], &[0.0, 1.0]);
        let order: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        // the text row and the tail stay in their slots; candidates reorder among slots 0, 2, 3
        assert_eq!(order, ["img", "words", "b", "a", "tail"]);
        assert_eq!(rows[0].score, 4.0);
        assert_eq!(rows[0].rerank, None);
        assert_eq!((rows[1].score, rows[4].score), (1.0, 2.0));
        assert_eq!(rows[4].rerank, None);
        // NaN is treated as 0, a zero top changes nothing
        let mut z = vec![row("z", 0.0, true)];
        apply_rerank(&mut z, &[0], &[0], &[0.9]);
        assert_eq!(z[0].rerank, None);
        let mut n = vec![row("n", 2.0, true)];
        apply_rerank(&mut n, &[0], &[0], &[f32::NAN]);
        assert_eq!(n[0].rerank, Some(0.0));
    }

    #[test]
    fn clip_chars_respects_character_boundaries() {
        assert_eq!(clip_chars("héllo wörld", 5), "héllo");
        assert_eq!(clip_chars("short", 50), "short");
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
            let id = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 9,
                    },
                    r"C:\Pics\notes.txt",
                    1,
                    1,
                    "doc",
                )
                .unwrap();
            let v = [ChunkVec {
                chunk: 0,
                kind: VecKind::Text,
                locator: None,
                vec: unit(0),
            }];
            s.complete(id, "h9", &v, &[(0, "feed the cat at noon".into())])
                .unwrap();
        }
        let r = t.semantic_search("cat", 5).unwrap();
        let src: Vec<(&str, &str)> = r
            .hits
            .iter()
            .map(|h| (h.path.rsplit('\\').next().unwrap(), h.source))
            .collect();
        assert_eq!(src, [("cat.jpg", "meaning"), ("notes.txt", "text")]);
        assert!(r.hits[1].snippet.as_deref().unwrap().contains("[cat]"));
        let stored = lock(&t.store)
            .get_setting("relevance_hub:embeddinggemma-2")
            .unwrap();
        assert!(
            stored.is_some_and(|s| s.starts_with('[')),
            "hub persisted per query model"
        );
    }

    #[test]
    fn a_code_in_the_query_lifts_the_literal_hit_above_meaning_hits() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        add_done(&t, 1, r"C:\Pics\something.jpg", 2); // the stub's meaning match for any non-"cat" query
        {
            let mut s = lock(&t.store);
            let id = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 9,
                    },
                    r"C:\Pics\inv.txt",
                    1,
                    1,
                    "doc",
                )
                .unwrap();
            let v = [ChunkVec {
                chunk: 0,
                kind: VecKind::Text,
                locator: None,
                vec: unit(0),
            }]; // no meaning match
            s.complete(
                id,
                "h9",
                &v,
                &[(0, "Invoice INV-2024-00817: total 2480 euros".into())],
            )
            .unwrap();
        }
        let names = |q: &str| -> Vec<String> {
            t.semantic_search(q, 5)
                .unwrap()
                .hits
                .iter()
                .map(|h| h.path.rsplit('\\').next().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            names("INV-2024-00817"),
            ["inv.txt", "something.jpg"],
            "an invoice number is a literal: it leads"
        );
        // The same file found by a plain word keeps the old order: meaning first, words after.
        assert_eq!(names("invoice"), ["something.jpg", "inv.txt"]);
    }

    /// A done code file with one stretch: `hot` is the query direction its vector points at
    /// (`unit(hot)`), `loc` the stretch's line locator.
    fn add_code(t: &Tier1, n: u64, path: &str, hot: usize, body: &str, loc: Option<&str>) {
        let mut s = lock(&t.store);
        let id = s
            .upsert_file(
                &FileKey {
                    vol_guid: "{v}".into(),
                    frn: n,
                },
                path,
                1,
                1,
                "code",
            )
            .unwrap();
        let v = vec![ChunkVec {
            chunk: 0,
            kind: VecKind::Text,
            locator: loc.map(str::to_owned),
            vec: unit(hot),
        }];
        s.complete(id, &format!("hc{n}"), &v, &[(0, body.into())])
            .unwrap();
    }

    /// The hybrid `code:` search: words (with line locators) and meaning fused, code never
    /// leaking into a general search.
    #[test]
    fn code_search_fuses_words_and_meaning_and_carries_lines() {
        let t = tier(Box::new(Q), &[r"C:\Code"]);
        assert_eq!(t.set_index_code(true), Ok(true));
        // A words-only hit with a line locator (its vector never matches a query).
        add_code(
            &t,
            1,
            r"C:\Code\util.rs",
            0,
            "pub fn retry_with_backoff() {\n    nap();\n}\n",
            Some(r#"{"loc":1,"loc_end":3}"#),
        );
        // A meaning hit (the stub answers any non-"cat" query) that also names the phrase.
        add_code(
            &t,
            2,
            r"C:\Code\server.rs",
            2,
            "retry_with_backoff(every request);\n",
            Some(r#"{"loc":7,"loc_end":7}"#),
        );
        let hits = t
            .semantic_search("code: retry_with_backoff", 5)
            .unwrap()
            .hits;
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits.iter().map(|h| h.line).collect::<Vec<_>>(),
            [Some(7), Some(1)],
            "every code hit carries the line to open at"
        );
        assert!(hits.iter().all(|h| h.source == "code"));
        assert!(
            hits[0].path.ends_with("server.rs"),
            "the file with both evidences leads (ties break by path)"
        );
        assert!(
            hits[1].snippet.as_deref().unwrap().contains('['),
            "a words hit shows its matched stretch"
        );
        assert!(
            t.semantic_search("retry", 5)
                .unwrap()
                .hits
                .iter()
                .all(|h| h.source != "code"),
            "code never mixes into a general search"
        );
    }

    /// A looked-up name opens where it is defined — in whichever of the top results defines it —
    /// and a plain words hit opens at its first matched line, not the top of the stretch.
    #[test]
    fn code_hits_open_at_the_definition_of_a_looked_up_name() {
        let t = tier(Box::new(Q), &[r"C:\Code"]);
        assert_eq!(t.set_index_code(true), Ok(true));
        // The caller file ranks first: it uses the name, it doesn't define it (the fake query
        // vector only matches its stretch).
        add_code(
            &t,
            5,
            r"C:\Code\app.rs",
            2,
            "retry_with_backoff(every request);\nretry_with_backoff(all);\n",
            Some(r#"{"loc":10,"loc_end":11}"#),
        );
        // The definition, carried by the words side alone.
        add_code(
            &t,
            6,
            r"C:\Code\util.rs",
            0,
            "use std::time::Duration;\n\nfn retry_with_backoff(d: Duration) {\n    nap(d);\n}\n",
            Some(r#"{"loc":40,"loc_end":44}"#),
        );
        let hits = t
            .semantic_search("code: retry_with_backoff", 5)
            .unwrap()
            .hits;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, r"C:\Code\app.rs");
        assert_eq!(
            hits[0].line,
            Some(10),
            "the caller opens at its first matched line"
        );
        assert_eq!(
            hits[1].line,
            Some(42),
            "the looked-up name opens at its definition, across files"
        );
        assert!(
            hits[1].snippet.as_deref().unwrap().contains('['),
            "a definition in the matched stretch keeps the excerpt"
        );
    }

    /// The words side's gate: a file must answer at least half the query's words to be listed.
    #[test]
    fn code_words_only_files_answer_half_the_query_to_qualify() {
        let t = tier(Box::new(Q), &[r"C:\Code"]);
        assert_eq!(t.set_index_code(true), Ok(true));
        add_code(&t, 1, r"C:\Code\full.rs", 0, "alpha retry backoff\n", None);
        add_code(&t, 2, r"C:\Code\two.rs", 0, "retry backoff\n", None);
        add_code(&t, 3, r"C:\Code\one.rs", 0, "backoff\n", None);
        let names: Vec<String> = t
            .semantic_search("code: alpha retry backoff", 5)
            .unwrap()
            .hits
            .iter()
            .map(|h| h.path.clone())
            .collect();
        assert_eq!(
            names.len(),
            2,
            "one word of three is not enough: one.rs is out"
        );
        assert!(names[0].ends_with("full.rs"), "full coverage leads");
        assert!(names[1].ends_with("two.rs"));
    }

    /// The chunker fingerprint gate: stretches cut by an older chunker are forgotten once, so the
    /// crawler re-chunks them (old char offsets must never mix with new line locators).
    #[test]
    fn a_code_chunker_change_reindexes_the_code_partition() {
        let t = tier(Box::new(Q), &[r"C:\Code"]);
        assert_eq!(t.set_index_code(true), Ok(true));
        add_code(
            &t,
            1,
            r"C:\Code\a.rs",
            0,
            "fn a() {}\n",
            Some(r#"{"loc":1,"loc_end":1}"#),
        );
        lock(&t.store)
            .set_setting(winseek_store::SETTING_CODE_CHUNKER, Some("old"))
            .unwrap();
        t.check_code_chunker();
        {
            let s = lock(&t.store);
            assert_eq!(
                s.stats().unwrap().done,
                0,
                "the old-stretch file was forgotten"
            );
            assert_eq!(
                s.get_setting(winseek_store::SETTING_CODE_CHUNKER)
                    .unwrap()
                    .as_deref(),
                Some(chunk::CODE_CHUNKER_FINGERPRINT)
            );
        }
        t.check_code_chunker(); // again: nothing left to forget
        assert_eq!(lock(&t.store).stats().unwrap().done, 0);
    }

    #[test]
    fn semantic_hits_carry_ocr_snippet_and_stat() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        {
            let mut s = lock(&t.store);
            let id = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 9,
                    },
                    r"C:\Pics\shot.png",
                    1,
                    1,
                    "image",
                )
                .unwrap();
            let v = [ChunkVec {
                chunk: 0,
                kind: VecKind::Image,
                locator: None,
                vec: unit(1),
            }];
            s.complete(id, "h9", &v, &[(0, "Invoice 2025 total due".into())])
                .unwrap();
        }
        add_done(&t, 1, r"C:\Pics\cat.jpg", 1);
        let r = t.semantic_search("cat", 5).unwrap();
        assert!(
            r.hits.iter().all(|h| h.snippet.is_none()),
            "no text matches 'cat'"
        );
        assert!(
            r.hits.iter().all(|h| h.size.is_none() && h.mtime.is_none()),
            "files do not exist on disk"
        );
        // A query that the stub embeds the same way and whose word is in the OCR text.
        let r = t.semantic_search("invoice", 5).unwrap();
        let shot = r
            .hits
            .iter()
            .find(|h| h.path.ends_with("shot.png"))
            .expect("hit");
        assert!(
            shot.snippet.as_deref().unwrap().contains("[Invoice]"),
            "{:?}",
            shot.snippet
        );
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
            let pend = |c, l: &str| winseek_store::PendingVec {
                chunk: c,
                kind: VecKind::Text,
                locator: Some(l.into()),
            };
            let a = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 1,
                    },
                    r"C:\Docs\report.pdf",
                    1,
                    1,
                    "pdf",
                )
                .unwrap();
            s.complete_text_only(
                a,
                "ha",
                &[
                    pend(0, r#"{"page":1,"chars":[0,20]}"#),
                    pend(1, r#"{"page":7,"chars":[0,30]}"#),
                ],
                &[
                    (0, "cover page".into()),
                    (1, "the quarterly invoice total".into()),
                ],
            )
            .unwrap();
            let b = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 2,
                    },
                    r"C:\Elsewhere\x.pdf",
                    1,
                    1,
                    "pdf",
                )
                .unwrap();
            s.complete_text_only(
                b,
                "hb",
                &[pend(0, "{}")],
                &[(0, "invoice outside the scope".into())],
            )
            .unwrap();
        }
        let r = t.semantic_search("invoice", 5).unwrap();
        assert_eq!(r.state, "models_missing");
        assert_eq!(r.hits.len(), 1, "out-of-scope file dropped");
        let h = &r.hits[0];
        assert_eq!(
            (h.path.as_str(), h.source, h.page),
            (r"C:\Docs\report.pdf", "text", Some(7))
        );
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
            let id = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 1,
                    },
                    r"C:\Docs\scan.pdf",
                    1,
                    1,
                    "pdf",
                )
                .unwrap();
            let v = [ChunkVec {
                chunk: 4,
                kind: VecKind::Image,
                locator: Some(r#"{"page":5}"#.into()),
                vec: unit(1),
            }];
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
        t.set_scopes(vec![r"D:\x".into()], vec![r"D:\x\y".into()])
            .unwrap();
        let st = t.status();
        assert_eq!(
            (st.include, st.exclude),
            (vec![r"D:\x".to_string()], vec![r"D:\x\y".to_string()])
        );
        assert!(t.snapshot().gen > g0);
        assert_eq!(
            lock(&t.store).get_scopes().unwrap().include,
            vec![r"D:\x".to_string()]
        );
    }

    #[test]
    fn remove_folder_drops_the_scope_and_forgets_its_files() {
        let t = tier(Box::new(Q), &[r"C:\Pics", r"D:\Docs"]);
        {
            let mut s = lock(&t.store);
            let a = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 1,
                    },
                    r"C:\Pics\cat.jpg",
                    1,
                    1,
                    "image",
                )
                .unwrap();
            s.complete(
                a,
                "h1",
                &[ChunkVec {
                    chunk: 0,
                    kind: VecKind::Image,
                    locator: None,
                    vec: unit(1),
                }],
                &[(0, "a cat picture".into())],
            )
            .unwrap();
            let id = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 2,
                    },
                    r"D:\Docs\report.pdf",
                    1,
                    1,
                    "doc",
                )
                .unwrap();
            s.complete(id, "h2", &[], &[(0, "the quarterly invoice total".into())])
                .unwrap();
        }
        let g0 = t.snapshot().gen;
        assert_eq!(
            t.remove_folder(r"C:\Pics\").unwrap(),
            1,
            "trailing separator is trimmed, the file is gone"
        );
        assert!(t.snapshot().gen > g0, "the crawler re-runs");
        let st = t.status();
        assert_eq!(st.include, vec![r"D:\Docs".to_string()]);
        assert_eq!(
            lock(&t.store).get_scopes().unwrap().include,
            vec![r"D:\Docs".to_string()]
        );
        {
            let s = lock(&t.store);
            assert_eq!(
                s.stats().unwrap().files,
                1,
                "only the kept folder's row is left"
            );
            assert!(s.search_text("invoice", 5).unwrap().len() == 1);
            assert!(
                s.search_text("cat", 5).unwrap().is_empty(),
                "the removed folder's text is forgotten"
            );
        }
        // Case-insensitive and idempotent; an unknown path only purges (nothing to purge).
        assert_eq!(t.remove_folder(r"c:\pics").unwrap(), 0);
        assert_eq!(
            t.remove_folder("").unwrap_err(),
            "remove_folder: empty path"
        );
    }

    /// Removing the LAST folder must survive a restart: the saved list is configured, so the
    /// defaults (Pictures/Documents/Desktop/Downloads) must not resurrect (`scopes::effective`).
    #[test]
    fn removing_every_folder_survives_a_restart() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        let dir = test_tmp();
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3]));
        img.save(dir.path().join("only.png")).unwrap();
        t.set_scopes(vec![dir.path().to_string_lossy().into_owned()], vec![])
            .unwrap();
        assert_eq!(
            t.remove_folder(&dir.path().to_string_lossy()).unwrap(),
            0,
            "nothing indexed yet, scope still drops"
        );
        let saved = lock(&t.store).get_scopes().unwrap();
        assert!(
            saved.include.is_empty() && saved.configured,
            "the empty list is saved as configured: {saved:?}"
        );
        let (effective, defaulted) = scopes::effective(saved);
        assert!(!defaulted, "the defaults must not resurrect");
        assert!(effective.include.is_empty());
        // And the crawler acts on it: the snapshot scopes are the empty list too.
        assert!(t.snapshot().scopes.include.is_empty());
    }

    /// Unchecking a folder parks it: the entry stays in the list (persisted `disabled`) and
    /// its rows stay in the store, so re-checking is instant. Re-checking unparks it.
    #[test]
    fn unchecking_parks_a_folder_until_it_is_rechecked() {
        let t = tier(Box::new(Q), &[r"C:\Pics", r"C:\Docs"]);
        t.set_scopes(vec![r"C:\Docs".into()], vec![]).unwrap();
        let st = t.status();
        assert_eq!(st.include, vec![r"C:\Docs".to_string()]);
        assert_eq!(st.disabled, vec![r"C:\Pics".to_string()]);
        // The save persists it, so a restart keeps the unchecked entry in the list.
        assert_eq!(
            lock(&t.store).get_scopes().unwrap().disabled,
            vec![r"C:\Pics".to_string()]
        );
        // Back in scope: the stale disabled entry is gone again.
        t.set_scopes(vec![r"C:\Pics".into(), r"C:\Docs".into()], vec![])
            .unwrap();
        assert!(t.status().disabled.is_empty());
    }

    /// Removing (the × button) is final for a parked folder too: the entry leaves the list
    /// and the rows are purged, unlike unchecking which keeps both.
    #[test]
    fn removing_a_parked_folder_clears_its_entry() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        t.set_scopes(vec![], vec![]).unwrap();
        assert_eq!(t.status().disabled, vec![r"C:\Pics".to_string()]);
        t.remove_folder(r"C:\Pics").unwrap();
        assert!(t.status().disabled.is_empty());
        assert!(t.status().include.is_empty());
        let saved = lock(&t.store).get_scopes().unwrap();
        assert!(saved.disabled.is_empty() && saved.configured);
    }

    #[test]
    fn user_pause_wins_and_auto_pause_resumes() {
        let t = tier(Box::new(Q), &[r"C:\Pics"]);
        t.set_auto_pause(Some(PauseReason::Battery));
        assert_eq!(
            (t.status().state.as_str(), t.status().paused_reason),
            ("paused", Some("battery"))
        );
        assert!(t.paused.load(Ordering::Relaxed));
        t.set_paused(true);
        assert_eq!(t.status().paused_reason, Some("user"));
        t.set_auto_pause(None); // user pause stays
        assert!(t.paused.load(Ordering::Relaxed));
        assert_eq!(t.status().paused_reason, Some("user"));
        t.set_paused(false);
        assert!(!t.paused.load(Ordering::Relaxed));
        assert_eq!(
            (t.status().state.as_str(), t.status().paused_reason),
            ("idle", None)
        );
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
            let id = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 1,
                    },
                    r"C:\Pics\a.rs",
                    1,
                    1,
                    "code",
                )
                .unwrap();
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
        t.rebuild_with(|s| {
            s.reset_in_place(&index_meta("test"))
                .map_err(|e| e.to_string())
        })
        .unwrap();
        let st = lock(&t.store).stats().unwrap();
        assert_eq!((st.done, st.pending, st.vectors), (0, 1, 0)); // the file is kept, its vectors are not
        assert!(t.snapshot().gen > g0); // the crawler sees a new generation and walks the scopes again
        assert_eq!(t.status().error, None);
        // a failing reset leaves the current store alone
        add_done(&t, 2, r"C:\Pics\dog.jpg", 2);
        assert_eq!(
            t.rebuild_with(|_| Err("boom".into())),
            Err("boom".to_string())
        );
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
            let f = FakeParser {
                present,
                calls: Arc::default(),
                unloads: Arc::default(),
                profile: Arc::default(),
            };
            let probe = FakeParser {
                present,
                calls: f.calls.clone(),
                unloads: f.unloads.clone(),
                profile: f.profile.clone(),
            };
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
        assert_eq!(
            lock(&t.store)
                .get_setting(SETTING_PROFILE)
                .unwrap()
                .as_deref(),
            Some("low")
        );
        assert_eq!(
            (*seen.lock().unwrap(), *probe.profile.lock().unwrap()),
            (Some(Profile::Low), Some(Profile::Low))
        );
        t.set_profile(Profile::Fast).unwrap();
        assert_eq!(
            (
                t.profile(),
                *seen.lock().unwrap(),
                *probe.profile.lock().unwrap()
            ),
            (Profile::Fast, Some(Profile::Fast), Some(Profile::Fast))
        );
        // an unreadable stored value falls back to the default
        lock(&t.store)
            .set_setting(SETTING_PROFILE, Some("turbo"))
            .unwrap();
        assert_eq!(t.profile(), Profile::Balanced);
    }

    #[test]
    fn pausing_unloads_the_parser_too() {
        let (parser, probe) = FakeParser::boxed(true);
        let t = tier_with(Box::new(Q), parser, &[r"C:\Pics"]);
        t.set_auto_pause(Some(PauseReason::Battery));
        assert_eq!(probe.unloads.load(Ordering::SeqCst), 1);
        t.set_auto_pause(None);
        assert_eq!(
            probe.unloads.load(Ordering::SeqCst),
            1,
            "resuming must not unload"
        );
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
            let id = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 9,
                    },
                    r"C:\Pics\scan.png",
                    1,
                    1,
                    "image",
                )
                .unwrap();
            let loc = r#"{"ocr":{"w":200,"h":100,"lines":[{"t":"Invoice 4711","r":[20,10,100,20]},{"t":"other","r":[0,50,10,10]}]}}"#;
            let v = [ChunkVec {
                chunk: 0,
                kind: VecKind::Image,
                locator: Some(loc.into()),
                vec: unit(2),
            }];
            s.complete(id, "h9", &v, &[(0, "Invoice 4711 other".into())])
                .unwrap();
            let id = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 10,
                    },
                    r"C:\Pics\parsed.png",
                    1,
                    1,
                    "image",
                )
                .unwrap();
            let loc = r#"{"parser":{"v":"doc2json-v1","boxes":[{"b":[100,200,600,300],"c":"text","t":"Total due: Invoice"}]}}"#;
            let v = [ChunkVec {
                chunk: 0,
                kind: VecKind::Image,
                locator: Some(loc.into()),
                vec: unit(2),
            }];
            s.complete(id, "h10", &v, &[]).unwrap();
        }
        let r = t.semantic_search("invoice", 5).unwrap();
        let by = |name: &str| r.hits.iter().find(|h| h.path.ends_with(name)).unwrap();
        assert_eq!(
            by("scan.png").boxes,
            Some(vec![HitBox {
                x: 0.1,
                y: 0.1,
                w: 0.5,
                h: 0.2
            }])
        );
        assert_eq!(
            by("parsed.png").boxes,
            Some(vec![HitBox {
                x: 0.1,
                y: 0.2,
                w: 0.5,
                h: 0.1
            }])
        );
        // words that are in no box: no highlight
        let r = t.semantic_search("zebra", 5).unwrap();
        assert!(r.hits.iter().all(|h| h.boxes.is_none()));
        // JSON shape the UI reads
        let v = serde_json::to_value(by_name(
            &t.semantic_search("invoice", 5).unwrap(),
            "scan.png",
        ))
        .unwrap();
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
            let id = s
                .upsert_file(
                    &FileKey {
                        vol_guid: "{v}".into(),
                        frn: 1,
                    },
                    r"C:\Pics\shot.png",
                    1,
                    1,
                    "image",
                )
                .unwrap();
            let loc = r#"{"ocr":{"w":10,"h":10,"lines":[{"t":"invoice","r":[1,2,3,4]}]}}"#;
            let pend = [winseek_store::PendingVec {
                chunk: 1,
                kind: VecKind::Text,
                locator: Some(loc.into()),
            }];
            s.complete_text_only(id, "h", &pend, &[(1, "invoice".into())])
                .unwrap();
        }
        let r = t.semantic_search("invoice", 5).unwrap();
        assert_eq!(r.hits[0].source, "text");
        assert_eq!(
            r.hits[0].boxes,
            Some(vec![HitBox {
                x: 0.1,
                y: 0.2,
                w: 0.3,
                h: 0.4
            }])
        );
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
        assert_eq!(
            d.markdown,
            "# Quarterly Invoice 4471\n\n| Item | Qty |\n| --- | --- |\n| Card | 1 |"
        );
        assert_eq!(
            (d.tables.len(), d.tables[0].tsv.as_str()),
            (1, "Item\tQty\nCard\t1")
        );
        let again = t.parse_document(&path, None).unwrap();
        assert!(again.cached, "same content: no second parse");
        assert_eq!(
            (again.markdown, again.elements_count),
            (d.markdown.clone(), 2)
        );
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        // the same bytes under another name hit the cache too; different bytes do not
        std::fs::copy(&path, dir.path().join("copy.png")).unwrap();
        assert!(
            t.parse_document(&dir.path().join("copy.png").to_string_lossy(), None)
                .unwrap()
                .cached
        );
        let other = image::RgbImage::from_pixel(61, 40, image::Rgb([1, 2, 3]));
        other.save(dir.path().join("other.png")).unwrap();
        assert!(
            !t.parse_document(&dir.path().join("other.png").to_string_lossy(), None)
                .unwrap()
                .cached
        );
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
        assert!(
            t.parse_document(&paths[1], None).unwrap().cached,
            "the 8 newest are kept"
        );
        assert!(
            !t.parse_document(&paths[0], None).unwrap().cached,
            "the oldest was evicted"
        );
        assert_eq!(probe.calls.load(Ordering::SeqCst), 10);
    }

    #[test]
    fn parse_document_refuses_what_it_cannot_read() {
        let dir = test_tmp();
        let (parser, probe) = FakeParser::boxed(true);
        let t = tier_with(Box::new(Q), parser, &[r"C:\Pics"]);
        let txt = dir.path().join("a.txt");
        std::fs::write(&txt, "hello").unwrap();
        assert!(t
            .parse_document(&txt.to_string_lossy(), None)
            .unwrap_err()
            .contains("images and PDF"));
        assert!(t
            .parse_document(&dir.path().join("missing.png").to_string_lossy(), None)
            .unwrap_err()
            .contains("Cannot read"));
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"not an image").unwrap();
        assert!(t
            .parse_document(&bad.to_string_lossy(), None)
            .unwrap_err()
            .contains("decode"));
        assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
        // no parser installed
        let none = tier(Box::new(Q), &[r"C:\Pics"]);
        let png = write_png(dir.path(), "x.png");
        assert!(none
            .parse_document(&png, None)
            .unwrap_err()
            .contains("not installed"));
    }

    #[test]
    fn end_to_end_crawl_then_process() {
        let dir = test_tmp();
        let img = image::RgbImage::from_pixel(20, 10, image::Rgb([1, 2, 3]));
        img.save(dir.path().join("a.png")).unwrap();
        let t = tier(Box::new(Q), &[&dir.path().to_string_lossy()]);
        let snap = t.snapshot();
        assert_eq!(
            crawler::crawl(
                &t.store,
                &snap.scopes,
                &snap.ignore,
                snap.index_code,
                &|| false
            )
            .upserted,
            1
        );
        let mut p = Processor::default();
        assert_eq!(
            p.run_pass(&t.store, &t.embedder, &t.paused).unwrap(),
            Pass::Progress(1)
        );
        assert_eq!(lock(&t.store).stats().unwrap().done, 1);
    }
}
