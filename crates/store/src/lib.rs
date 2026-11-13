//! Tier 1 semantic store: one SQLite file holding file identity, content-hash keyed
//! vectors (brute-force cosine), an FTS5 text index and settings (folder scopes).
//!
//! Rows are keyed by `(volume GUID, FRN)`; vectors and texts are keyed by content hash,
//! so a rename costs nothing and identical content at two paths shares its vectors.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

pub mod fuse;
mod provenance;
mod recover;
pub mod relevance;
mod tail;
pub use provenance::{drift, verdict, Verdict, PROBE_MIN_COS};
pub use recover::{is_corruption_message, Recovery};
pub use tail::{TailCounts, TailJob};
use relevance::{Candidate, Modality};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: i64 = 5;

const STATE_PENDING: i64 = 0;
const STATE_DONE: i64 = 1;
const STATE_FAILED: i64 = 2;
const STATE_SKIPPED: i64 = 3;
/// Text is extracted and in FTS, but the vectors still wait for the models ("pending vectors").
const STATE_TEXT_ONLY: i64 = 4;

/// Default ignore list: folder/file *names* (one path segment each, case-insensitive,
/// `*` and `?` wildcards) never indexed inside any scope. The user edits it in Settings;
/// it is stored under the `ignore` settings key, and unset means this list.
pub const DEFAULT_IGNORE: &[&str] = &[
    // Windows / system / trash
    "AppData", "$RECYCLE.BIN", "System Volume Information", "$WinREAgent", "Config.Msi",
    "Thumbs.db", "desktop.ini", "__MACOSX", ".DS_Store", ".Trash", ".Trash-*", ".Spotlight-V100",
    ".fseventsd", ".cache",
    // secrets & credentials (privacy)
    ".ssh", ".gnupg", ".password-store", ".aws", ".azure", ".kube", ".docker", ".pki",
    "*.kdbx", "*.kdb", "*.agilekeychain", "*.opvault", "*.1pux",
    // version control
    ".git", ".hg", ".svn", ".bzr", "_darcs", ".jj", ".pijul", ".fossil", "CVS",
    // editors / IDEs
    ".idea", ".vs", ".vscode", ".fleet", ".history", ".metadata", ".settings", ".sts4-cache",
    ".ccls-cache", ".clangd", ".cache-loader",
    // JavaScript / TypeScript / web
    "node_modules", "bower_components", "jspm_packages", "web_modules", ".npm", ".yarn",
    ".pnpm-store", ".pnp", ".next", ".nuxt", ".output", ".svelte-kit", ".astro", ".angular",
    ".parcel-cache", ".turbo", ".vercel", ".netlify", ".docusaurus", ".expo", ".vite",
    ".webpack", ".serverless", ".wrangler", ".nx", ".rush", "storybook-static", ".nyc_output",
    "coverage", ".eslintcache", ".stylelintcache", ".sass-cache", ".deno", "elm-stuff",
    // Python
    "venv", ".venv", "virtualenv", ".virtualenv", "env.bak", "venv.bak", "__pycache__",
    "__pypackages__", "site-packages", "dist-packages", ".tox", ".nox", ".pytest_cache",
    ".mypy_cache", ".ruff_cache", ".pyre", ".pytype", ".hypothesis", ".ipynb_checkpoints",
    ".eggs", "*.egg-info", ".pixi", ".conda", "conda-meta", "htmlcov", ".pdm-build",
    // Rust
    "target", ".cargo", ".rustup",
    // Java / Kotlin / Scala / Clojure (JVM)
    ".gradle", ".m2", ".mvn", ".kotlin", ".bloop", ".metals", ".bsp", ".scala-build",
    ".cpcache", ".clj-kondo", ".lsp", ".shadow-cljs",
    // build outputs (generic, any language)
    "build", "builds", "dist", "out", "bin", "obj", "_build", ".build", "cmake-build-*",
    "CMakeFiles", "bazel-*", ".cxx", ".externalNativeBuild", "zig-cache", ".zig-cache",
    "zig-out", ".stack-work", "dist-newstyle", "_opam", ".elixir_ls", "deps",
    // .NET / C / C++
    "packages", "TestResults", "ipch", ".nuget",
    // Go / PHP / Ruby
    "vendor", ".bundle",
    // mobile / Apple / Dart
    "Pods", "DerivedData", "Carthage", ".swiftpm", ".dart_tool", ".pub-cache", ".fvm",
    // game engines
    ".godot", ".import",
    // infra / misc tooling
    ".terraform", ".terragrunt-cache", ".direnv", ".devenv", ".vagrant", ".Rproj.user",
];

/// Hard deny-list: names (any path segment, case-insensitive, `*`/`?` wildcards) that are NEVER
/// indexed or read, whatever the user's ignore list says. Secrets, key material, password
/// stores and tool caches. Unlike [`DEFAULT_IGNORE`] this is not editable and applies to every
/// segment of a path, include roots too ([`in_scope_with`], [`hard_denied`]).
pub const HARD_DENY: &[&str] = &[
    // credential / key folders
    ".ssh", ".gnupg", ".password-store", ".aws", ".azure", ".kube", ".docker", ".pki",
    // password managers
    "*.kdbx", "*.kdb", "*.agilekeychain", "*.opvault", "*.1pux", "*.psafe3", "*.keychain", "*.keychain-db", "*.wallet",
    // keys and certificates
    "*.pem", "*.key", "*.pfx", "*.p12", "*.ppk", "*.jks", "*.keystore", "*.gpg", "*.pgp", "*.crt", "*.cer", "*.der", "*.csr",
    "id_rsa*", "id_dsa*", "id_ecdsa*", "id_ed25519*",
    // credential files
    ".env", ".env.*", "*.env", ".netrc", "_netrc", ".npmrc", ".pypirc", ".pgpass", ".git-credentials", ".htpasswd",
    "credentials", "credentials.json", "secrets.json", "secrets.yml", "secrets.yaml", "service-account*.json",
    "*password*.txt", "*passwords*.csv", "terraform.tfstate", "*.tfvars",
    // caches
    ".cache", "__pycache__", "INetCache", "GPUCache", "Code Cache", "ShaderCache", "Cache_Data", "CacheStorage", ".npm", ".pnpm-store",
    ".gradle", ".m2", ".nuget", ".cargo",
];

fn hard_deny_set() -> &'static Ignore {
    static H: std::sync::OnceLock<Ignore> = std::sync::OnceLock::new();
    H.get_or_init(|| Ignore::new(HARD_DENY))
}

/// True when any segment of `path` (root included) matches [`HARD_DENY`].
pub fn hard_denied(path: &str) -> bool {
    let p = norm_path(path);
    let ig = hard_deny_set();
    p.split('\\').any(|seg| !seg.is_empty() && ig.matches_seg(seg))
}

/// The default ignore list as owned strings (what Settings shows on "Reset to defaults").
pub fn default_ignore() -> Vec<String> {
    DEFAULT_IGNORE.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------- errors

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    /// The store was built with different model/build/quant/budget/dims.
    MetaMismatch {
        stored: Box<IndexMeta>,
        requested: Box<IndexMeta>,
    },
    /// The database was written by a newer schema version.
    SchemaTooNew(i64),
    /// A vector (or query) has the wrong number of dimensions.
    BadVector { expected: usize, got: usize },
    NoSuchFile(FileId),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "sqlite: {e}"),
            Error::Json(e) => write!(f, "json: {e}"),
            Error::MetaMismatch { stored, requested } => write!(
                f,
                "index meta mismatch: stored {stored:?}, requested {requested:?}"
            ),
            Error::SchemaTooNew(v) => write!(f, "store schema version {v} is newer than supported"),
            Error::BadVector { expected, got } => {
                write!(f, "vector has {got} dims, expected {expected}")
            }
            Error::NoSuchFile(id) => write!(f, "no such file id {}", id.0),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(e) => Some(e),
            Error::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<rusqlite::types::FromSqlError> for Error {
    fn from(e: rusqlite::types::FromSqlError) -> Self {
        Error::Sqlite(e.into())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

// ---------------------------------------------------------------- types

/// Everything that changes the meaning of a vector. A mismatch means "rebuild".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexMeta {
    pub model_id: String,
    pub llama_build: String,
    pub quant: String,
    pub image_budget: u32,
    pub dims: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub i64);

/// Stable file identity: volume GUID + NTFS file reference number.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileKey {
    pub vol_guid: String,
    pub frn: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecKind {
    Image,
    Text,
    Page,
    /// A window of a soundtrack (audio / video files), embedded straight from the waveform.
    Audio,
}

impl VecKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VecKind::Image => "image",
            VecKind::Text => "text",
            VecKind::Page => "page",
            VecKind::Audio => "audio",
        }
    }
}

impl VecKind {
    pub fn parse(s: &str) -> Option<VecKind> {
        match s {
            "image" => Some(VecKind::Image),
            "text" => Some(VecKind::Text),
            "page" => Some(VecKind::Page),
            "audio" => Some(VecKind::Audio),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingFile {
    pub id: FileId,
    pub key: FileKey,
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub kind: String,
}

/// A vector that still has to be computed for a text-only file: the chunk's text lives in the
/// `texts` table, an image chunk is re-rendered from the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingVec {
    pub chunk: u32,
    pub kind: VecKind,
    pub locator: Option<String>,
}

/// A file in the "text searchable, waiting for models" state.
#[derive(Debug, Clone)]
pub struct TextOnlyFile {
    pub id: FileId,
    pub path: String,
    pub kind: String,
    pub hash: String,
}

/// Why an item sits in the parse queue.
pub const PARSE_REASONS: [&str; 3] = ["scanned_pdf", "doc_image", "screenshot"];
/// Parse attempts before an item is given up on (`fail_parse`).
pub const MAX_PARSE_ATTEMPTS: u32 = 3;
/// Settings key: rowid of `vectors` the document-image route pass has scanned up to.
pub const SETTING_PARSE_ROUTE_CURSOR: &str = "parse_route_cursor";
/// Settings key: set once queue entries queued under an older routing gate have been re-checked
/// against the current one (`parse_pass::reeval_stale`).
pub const SETTING_PARSE_GATE_REEVAL: &str = "parse_gate_reeval_v1";

/// One queued item for the document parser. `page` 0 is a standalone image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseJob {
    pub hash: String,
    pub page: u32,
    pub reason: String,
    /// A live file with this content (the newest one).
    pub path: String,
    pub attempts: u32,
}

/// One text chunk produced by the parser (or by OCR when a document image is only made searchable).
#[derive(Debug, Clone)]
pub struct ParseChunk {
    pub text: String,
    pub locator: String,
    /// The chunk's text vector; `None` while it is still owed (stored as a pending vector).
    pub vec: Option<Vec<f32>>,
}

/// What replaces an item's text: new text chunks plus the locator for its image / page vector.
#[derive(Debug, Clone)]
pub struct ParseResult {
    pub chunks: Vec<ParseChunk>,
    pub locator: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseCounts {
    pub queued: u64,
    pub done: u64,
    pub failed: u64,
    /// Skipped by the parser (empty output / unsupported script): their previous text stays.
    pub skipped: u64,
}

/// An image vector for the document-image route pass.
#[derive(Debug, Clone)]
pub struct RouteImage {
    pub rowid: i64,
    pub hash: String,
    pub vec: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct ChunkVec {
    pub chunk: u32,
    pub kind: VecKind,
    /// Page number / bbox / timestamp, as JSON text.
    pub locator: Option<String>,
    /// Pre-normalised, length == `IndexMeta::dims`.
    pub vec: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct VecHit {
    pub file_id: FileId,
    pub path: String,
    pub chunk: u32,
    pub locator: Option<String>,
    /// Cosine similarity (dot product of normalised vectors); higher is better.
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct TextHit {
    pub file_id: FileId,
    pub path: String,
    pub chunk: u32,
    pub snippet: String,
    /// Locator JSON of the chunk (page, character range, ...), from its vector or, while the vector
    /// is still pending, from the pending row.
    pub locator: Option<String>,
    /// Raw `bm25()` value; lower (more negative) is better.
    pub rank: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub files: u64,
    pub pending: u64,
    pub done: u64,
    pub failed: u64,
    pub skipped: u64,
    pub vectors: u64,
    /// Text is searchable (FTS) but vectors are still pending (waiting for models).
    pub text_only: u64,
}

/// Files that failed / were skipped for the same recorded reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonGroup {
    /// The recorded message or reason code (empty when none was stored).
    pub reason: String,
    pub count: u64,
    /// A few example paths.
    pub examples: Vec<String>,
}

/// A finished image file, as the library window lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRow {
    pub file_id: FileId,
    pub path: String,
    pub size: u64,
    /// Modified time, unix ms.
    pub mtime: i64,
    pub content_hash: String,
}

/// Files that share one content hash (byte-identical).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupGroup {
    pub hash: String,
    /// Newest first.
    pub files: Vec<ImageRow>,
}

impl DupGroup {
    /// Bytes freed by keeping one copy (the others are redundant).
    pub fn reclaimable(&self) -> u64 {
        let largest = self.files.iter().map(|f| f.size).max().unwrap_or(0);
        self.files.iter().map(|f| f.size).sum::<u64>().saturating_sub(largest)
    }
}

/// Group `vecs` (pre-normalised) whose cosine similarity is `>= threshold`, transitively
/// (single linkage). Only groups of two or more are returned, as sorted index lists ordered by
/// their first index.
pub fn cluster_near(vecs: &[&[f32]], threshold: f32) -> Vec<Vec<usize>> {
    let n = vecs.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..n {
        for j in (i + 1)..n {
            if vecs[i].len() != vecs[j].len() {
                continue;
            }
            let dot: f32 = vecs[i].iter().zip(vecs[j]).map(|(a, b)| a * b).sum();
            if dot >= threshold {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a.max(b)] = a.min(b);
                }
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() > 1).collect();
    out.sort_by_key(|g| g[0]);
    out
}

/// Folder scopes. A path is in scope if it is under an `include` prefix, not under an
/// `exclude` prefix, and has no ignored name below its root. An empty `include` list
/// means nothing is in scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scopes {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    /// The user has saved a folder list at least once. Without it an empty `include` is
    /// indistinguishable from "never configured", and the defaults would resurrect after
    /// removing the last folder.
    #[serde(default)]
    pub configured: bool,
}

// ---------------------------------------------------------------- scopes

fn norm_path(p: &str) -> String {
    let mut s: String = p.chars().map(|c| if c == '/' { '\\' } else { c }).collect();
    while s.len() > 1 && s.ends_with('\\') && !s.ends_with(":\\") {
        s.pop();
    }
    s.to_lowercase()
}

/// Case-insensitive prefix match on a `\` boundary (`C:\a` matches `C:\a\x`, not `C:\ab`).
fn under(prefix_norm: &str, path_norm: &str) -> bool {
    if path_norm == prefix_norm {
        return true;
    }
    match path_norm.strip_prefix(prefix_norm) {
        Some(rest) => prefix_norm.ends_with('\\') || rest.starts_with('\\'),
        None => false,
    }
}

/// Glob over one segment: `*` = any run, `?` = one char (inputs already lowercased).
fn glob_match(pat: &[char], s: &[char]) -> bool {
    let (mut p, mut i, mut star, mut mark) = (0, 0, None, 0);
    while i < s.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            mark = i;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == '*')
}

/// Compiled ignore list: exact names in a set, wildcard patterns scanned.
#[derive(Debug, Clone, Default)]
pub struct Ignore {
    exact: HashSet<String>,
    globs: Vec<Vec<char>>,
}

impl Ignore {
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Ignore {
        let mut ig = Ignore::default();
        for p in patterns {
            let p = p.as_ref().trim().trim_matches(['\\', '/']).to_lowercase();
            if p.is_empty() {
                continue;
            }
            if p.contains(['*', '?']) {
                ig.globs.push(p.chars().collect());
            } else {
                ig.exact.insert(p);
            }
        }
        ig
    }

    pub fn defaults() -> Ignore {
        Ignore::new(DEFAULT_IGNORE)
    }

    /// One name (not a path) matches the list, case-insensitively.
    pub fn matches_name(&self, name: &str) -> bool {
        self.matches_seg(&name.to_lowercase())
    }

    fn matches_seg(&self, seg: &str) -> bool {
        if self.exact.contains(seg) {
            return true;
        }
        if self.globs.is_empty() {
            return false;
        }
        let c: Vec<char> = seg.chars().collect();
        self.globs.iter().any(|g| glob_match(g, &c))
    }

    /// Any segment of `path_norm` *below* `root_norm` matches. The root's own segments are
    /// exempt, so explicitly including `D:\build\photos` still indexes it.
    fn hits(&self, root_norm: &str, path_norm: &str) -> bool {
        path_norm
            .get(root_norm.len()..)
            .unwrap_or("")
            .split('\\')
            .any(|seg| !seg.is_empty() && self.matches_seg(seg))
    }
}

/// [`in_scope_with`] using [`DEFAULT_IGNORE`].
pub fn in_scope(scopes: &Scopes, path: &str) -> bool {
    thread_local! {
        static DEFAULTS: Ignore = Ignore::defaults();
    }
    DEFAULTS.with(|ig| in_scope_with(scopes, ig, path))
}

/// In scope: under an `include` root, not under an `exclude` prefix, and no path segment
/// below that root matches `ignore`.
pub fn in_scope_with(scopes: &Scopes, ignore: &Ignore, path: &str) -> bool {
    let p = norm_path(path);
    if hard_denied(path) {
        return false;
    }
    if scopes.exclude.iter().any(|e| under(&norm_path(e), &p)) {
        return false;
    }
    scopes.include.iter().any(|i| {
        let r = norm_path(i);
        under(&r, &p) && !ignore.hits(&r, &p)
    })
}

/// Quote user text so FTS5 treats every whitespace-separated token as a literal term.
pub fn fts_escape(input: &str) -> String {
    input
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- store

pub struct Store {
    conn: Connection,
    dims: usize,
    meta: IndexMeta,
}

impl Store {
    pub fn open(path: impl AsRef<Path>, meta: &IndexMeta) -> Result<Store> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, meta)
    }

    pub fn open_in_memory(meta: &IndexMeta) -> Result<Store> {
        Self::init(Connection::open_in_memory()?, meta)
    }

    /// Drop all vectors/texts/content and rewrite the meta. Files stay (identity is
    /// cheap) but go back to pending; settings (scopes) are kept.
    pub fn reset(path: impl AsRef<Path>, meta: &IndexMeta) -> Result<Store> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        migrate(&conn)?;
        clear_derived(&conn)?;
        Self::init(conn, meta)
    }

    /// [`Store::reset`] over this store's own connection. Opening a second connection to the
    /// same file while this one is live is what [`Store::reset`] would do from a running app,
    /// and two writers on one WAL file are exactly the setup SQLite's WAL-reset race needs.
    /// A damaged file is salvaged instead (settings and files kept when readable).
    pub fn reset_in_place(&mut self, meta: &IndexMeta) -> Result<()> {
        match self.reset_derived(meta) {
            Err(e) if e.is_corruption() && self.path().is_some() => self.salvage_in_place(meta),
            r => r,
        }
    }

    fn reset_derived(&mut self, meta: &IndexMeta) -> Result<()> {
        clear_derived(&self.conn)?;
        self.conn.execute(
            "INSERT INTO index_meta(id, model_id, llama_build, quant, image_budget, dims)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)",
            params![meta.model_id, meta.llama_build, meta.quant, meta.image_budget, meta.dims],
        )?;
        self.dims = meta.dims as usize;
        self.meta = meta.clone();
        Ok(())
    }

    fn init(conn: Connection, meta: &IndexMeta) -> Result<Store> {
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&conn)?;
        let stored: Option<IndexMeta> = conn
            .query_row(
                "SELECT model_id, llama_build, quant, image_budget, dims FROM index_meta WHERE id = 1",
                [],
                |r| {
                    Ok(IndexMeta {
                        model_id: r.get(0)?,
                        llama_build: r.get(1)?,
                        quant: r.get(2)?,
                        image_budget: r.get(3)?,
                        dims: r.get(4)?,
                    })
                },
            )
            .optional()?;
        match stored {
            None => {
                conn.execute(
                    "INSERT INTO index_meta(id, model_id, llama_build, quant, image_budget, dims)
                     VALUES (1, ?1, ?2, ?3, ?4, ?5)",
                    params![
                        meta.model_id,
                        meta.llama_build,
                        meta.quant,
                        meta.image_budget,
                        meta.dims
                    ],
                )?;
            }
            Some(s) if s != *meta => {
                return Err(Error::MetaMismatch {
                    stored: Box::new(s),
                    requested: Box::new(meta.clone()),
                })
            }
            Some(_) => {}
        }
        Ok(Store {
            conn,
            dims: meta.dims as usize,
            meta: meta.clone(),
        })
    }

    // ------------------------------------------------------------ files

    pub fn upsert_file(
        &mut self,
        key: &FileKey,
        path: &str,
        size: u64,
        mtime: i64,
        kind: &str,
    ) -> Result<FileId> {
        let frn = key.frn as i64;
        let size_i = size as i64;
        let existing: Option<(i64, String, i64, i64)> = self
            .conn
            .prepare_cached("SELECT id, path, size, mtime FROM files WHERE vol_guid = ?1 AND frn = ?2")?
            .query_row(params![key.vol_guid, frn], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?;
        match existing {
            None => {
                self.conn
                    .prepare_cached(
                        "INSERT INTO files(vol_guid, frn, path, size, mtime, kind, state)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
                    )?
                    .execute(params![key.vol_guid, frn, path, size_i, mtime, kind])?;
                Ok(FileId(self.conn.last_insert_rowid()))
            }
            Some((id, old_path, old_size, old_mtime)) => {
                if old_size != size_i || old_mtime != mtime {
                    self.conn
                        .prepare_cached(
                            "UPDATE files SET path=?2, size=?3, mtime=?4, kind=?5,
                                 content_hash=NULL, state=0, error=NULL WHERE id=?1",
                        )?
                        .execute(params![id, path, size_i, mtime, kind])?;
                } else if old_path != path {
                    self.conn
                        .prepare_cached("UPDATE files SET path=?2 WHERE id=?1")?
                        .execute(params![id, path])?;
                }
                Ok(FileId(id))
            }
        }
    }

    /// Returns false if the key is unknown.
    pub fn rename(&mut self, key: &FileKey, new_path: &str) -> Result<bool> {
        let n = self
            .conn
            .prepare_cached("UPDATE files SET path=?3 WHERE vol_guid=?1 AND frn=?2")?
            .execute(params![key.vol_guid, key.frn as i64, new_path])?;
        Ok(n > 0)
    }

    /// Returns false if the key is unknown. Content rows are kept until [`Store::gc`].
    pub fn remove(&mut self, key: &FileKey) -> Result<bool> {
        let n = self
            .conn
            .prepare_cached("DELETE FROM files WHERE vol_guid=?1 AND frn=?2")?
            .execute(params![key.vol_guid, key.frn as i64])?;
        Ok(n > 0)
    }

    /// Remove every file at or below `path_prefix` (case-insensitive, `\` boundary).
    /// Case folding is ASCII-only (SQLite `LIKE`).
    pub fn remove_under(&mut self, path_prefix: &str) -> Result<usize> {
        let p = path_prefix.replace('/', "\\");
        let p = p.trim_end_matches('\\');
        let esc = like_escape(p);
        let n = self.conn.execute(
            "DELETE FROM files WHERE path LIKE ?1 ESCAPE '^' OR path LIKE ?2 ESCAPE '^'",
            params![esc, format!("{esc}\\%")],
        )?;
        Ok(n)
    }

    /// Re-point the file currently at `old_path` (case-insensitive, ASCII-folded) to
    /// `new_path`. For USN rename events where the file can no longer be opened to read its
    /// identity. Returns false if no row has that path.
    pub fn rename_path(&mut self, old_path: &str, new_path: &str) -> Result<bool> {
        let n = self
            .conn
            .prepare_cached("UPDATE files SET path=?2 WHERE path = ?1 COLLATE NOCASE")?
            .execute(params![norm_sep(old_path), norm_sep(new_path)])?;
        Ok(n > 0)
    }

    /// Remove the file at `path` (case-insensitive). Returns false if there is none.
    /// Content rows are kept until [`Store::gc`].
    pub fn remove_path(&mut self, path: &str) -> Result<bool> {
        let n = self
            .conn
            .prepare_cached("DELETE FROM files WHERE path = ?1 COLLATE NOCASE")?
            .execute(params![norm_sep(path)])?;
        Ok(n > 0)
    }

    /// Folder rename: every file at or below `old_prefix` (case-insensitive, `\` boundary)
    /// moves to the same relative place under `new_prefix`. Returns the number of rows moved.
    pub fn rename_under(&mut self, old_prefix: &str, new_prefix: &str) -> Result<usize> {
        let old = norm_sep(old_prefix);
        let old = old.trim_end_matches('\\');
        let new = norm_sep(new_prefix);
        let new = new.trim_end_matches('\\');
        let esc = like_escape(old);
        // substr() is 1-based and counts characters; ASCII case folding keeps lengths equal.
        let n = self.conn.execute(
            "UPDATE files SET path = ?3 || substr(path, ?4)
             WHERE path LIKE ?1 ESCAPE '^' OR path LIKE ?2 ESCAPE '^'",
            params![esc, format!("{esc}\\%"), new, old.chars().count() as i64 + 1],
        )?;
        Ok(n)
    }

    /// Remove every file row of `kind` (e.g. source code when the opt-in is switched off) and
    /// collect the content nothing references any more. Returns the number of files removed.
    pub fn remove_kind(&mut self, kind: &str) -> Result<usize> {
        let n = self.conn.execute("DELETE FROM files WHERE kind = ?1", [kind])?;
        self.gc()?;
        Ok(n)
    }

    /// Delete content/vectors/texts that no file references any more.
    pub fn gc(&mut self) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let orphan = "hash NOT IN (SELECT content_hash FROM files WHERE content_hash IS NOT NULL)";
        tx.execute(&format!("DELETE FROM vectors WHERE {orphan}"), [])?;
        tx.execute(&format!("DELETE FROM texts WHERE {orphan}"), [])?;
        tx.execute(&format!("DELETE FROM pending_vecs WHERE {orphan}"), [])?;
        tx.execute(&format!("DELETE FROM parse_queue WHERE {orphan}"), [])?;
        tx.execute(&format!("DELETE FROM pdf_tail WHERE {orphan}"), [])?;
        let n = tx.execute(&format!("DELETE FROM content WHERE {orphan}"), [])?;
        tx.commit()?;
        Ok(n)
    }

    pub fn next_pending(&self, limit: usize) -> Result<Vec<PendingFile>> {
        let mut st = self.conn.prepare_cached(
            "SELECT id, vol_guid, frn, path, size, mtime, kind FROM files
             WHERE state = 0 ORDER BY mtime DESC, id LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(PendingFile {
                id: FileId(r.get(0)?),
                key: FileKey {
                    vol_guid: r.get(1)?,
                    frn: r.get::<_, i64>(2)? as u64,
                },
                path: r.get(3)?,
                size: r.get::<_, i64>(4)? as u64,
                mtime: r.get(5)?,
                kind: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Pending files (state 0) filtered by kind and id: only kinds in `only` (all when empty), never
    /// kinds in `exclude`, ids greater than `after_id`; newest first, or by id when `after_id > 0`
    /// (a cursor walk). Kinds are internal constants, not user input.
    pub fn next_pending_where(
        &self,
        limit: usize,
        only: &[&str],
        exclude: &[&str],
        after_id: i64,
    ) -> Result<Vec<PendingFile>> {
        let list = |k: &[&str]| k.iter().map(|x| format!("'{}'", x.replace('\'', ""))).collect::<Vec<_>>().join(",");
        let mut sql = String::from("SELECT id, vol_guid, frn, path, size, mtime, kind FROM files WHERE state = 0 AND id > ?2");
        if !only.is_empty() {
            sql.push_str(&format!(" AND kind IN ({})", list(only)));
        }
        if !exclude.is_empty() {
            sql.push_str(&format!(" AND kind NOT IN ({})", list(exclude)));
        }
        sql.push_str(if after_id > 0 { " ORDER BY id LIMIT ?1" } else { " ORDER BY mtime DESC, id LIMIT ?1" });
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(params![limit as i64, after_id], |r| {
            Ok(PendingFile {
                id: FileId(r.get(0)?),
                key: FileKey { vol_guid: r.get(1)?, frn: r.get::<_, i64>(2)? as u64 },
                path: r.get(3)?,
                size: r.get::<_, i64>(4)? as u64,
                mtime: r.get(5)?,
                kind: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Is any file of `kind` waiting to be processed?
    pub fn has_pending_kind(&self, kind: &str) -> Result<bool> {
        Ok(self.conn.prepare_cached("SELECT 1 FROM files WHERE state = 0 AND kind = ?1 LIMIT 1")?.exists([kind])?)
    }

    // ------------------------------------------------------------ content

    pub fn has_content(&self, hash: &str) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT 1 FROM content WHERE hash = ?1")?
            .exists([hash])?)
    }

    /// Store the results for a file in one transaction. If the hash is already known the
    /// file is just linked and `vectors`/`texts` are ignored (hash-skip).
    pub fn complete(
        &mut self,
        file_id: FileId,
        hash: &str,
        vectors: &[ChunkVec],
        texts: &[(u32, String)],
    ) -> Result<()> {
        for v in vectors {
            if v.vec.len() != self.dims {
                return Err(Error::BadVector {
                    expected: self.dims,
                    got: v.vec.len(),
                });
            }
        }
        let tx = self.conn.transaction()?;
        let known = tx
            .prepare_cached("SELECT 1 FROM content WHERE hash = ?1")?
            .exists([hash])?;
        if !known {
            tx.execute("INSERT INTO content(hash) VALUES (?1)", [hash])?;
            {
                let mut ins = tx.prepare_cached(
                    "INSERT INTO vectors(hash, chunk, kind, locator, vec) VALUES (?1,?2,?3,?4,?5)",
                )?;
                for v in vectors {
                    let mut blob = Vec::with_capacity(v.vec.len() * 4);
                    for x in &v.vec {
                        blob.extend_from_slice(&x.to_le_bytes());
                    }
                    ins.execute(params![hash, v.chunk, v.kind.as_str(), v.locator, blob])?;
                }
                let mut ins_t =
                    tx.prepare_cached("INSERT INTO texts(hash, chunk, body) VALUES (?1,?2,?3)")?;
                for (chunk, body) in texts {
                    ins_t.execute(params![hash, chunk, body])?;
                }
            }
        }
        // Identical content that is still waiting for vectors keeps its twin text-only too.
        let waiting = tx
            .prepare_cached("SELECT 1 FROM pending_vecs WHERE hash = ?1")?
            .exists([hash])?;
        let state = if waiting { STATE_TEXT_ONLY } else { STATE_DONE };
        let n = tx
            .prepare_cached("UPDATE files SET content_hash=?2, state=?3, error=NULL WHERE id=?1")?
            .execute(params![file_id.0, hash, state])?;
        if n == 0 {
            return Err(Error::NoSuchFile(file_id));
        }
        tx.commit()?;
        Ok(())
    }

    /// FTS-only completion (no models yet): store the chunk texts and remember which vectors are
    /// still owed (`pending`). The file becomes searchable by text (`text_only` state) and
    /// [`Store::finish_vectors`] later turns it into a normal done file without re-extraction.
    /// If the hash is already known the file is just linked (state follows the content's).
    pub fn complete_text_only(
        &mut self,
        file_id: FileId,
        hash: &str,
        pending: &[PendingVec],
        texts: &[(u32, String)],
    ) -> Result<()> {
        if pending.is_empty() {
            return self.complete(file_id, hash, &[], texts);
        }
        let tx = self.conn.transaction()?;
        let known = tx.prepare_cached("SELECT 1 FROM content WHERE hash = ?1")?.exists([hash])?;
        if !known {
            tx.execute("INSERT INTO content(hash) VALUES (?1)", [hash])?;
            {
                let mut ins_t = tx.prepare_cached("INSERT INTO texts(hash, chunk, body) VALUES (?1,?2,?3)")?;
                for (chunk, body) in texts {
                    ins_t.execute(params![hash, chunk, body])?;
                }
                let mut ins_p = tx.prepare_cached(
                    "INSERT INTO pending_vecs(hash, chunk, kind, locator) VALUES (?1,?2,?3,?4)",
                )?;
                for p in pending {
                    ins_p.execute(params![hash, p.chunk, p.kind.as_str(), p.locator])?;
                }
            }
        }
        let waiting = tx.prepare_cached("SELECT 1 FROM pending_vecs WHERE hash = ?1")?.exists([hash])?;
        let state = if waiting { STATE_TEXT_ONLY } else { STATE_DONE };
        let n = tx
            .prepare_cached("UPDATE files SET content_hash=?2, state=?3, error=NULL WHERE id=?1")?
            .execute(params![file_id.0, hash, state])?;
        if n == 0 {
            return Err(Error::NoSuchFile(file_id));
        }
        tx.commit()?;
        Ok(())
    }

    /// Up to `limit` text-only files (newest first), one per content hash.
    pub fn next_text_only(&self, limit: usize) -> Result<Vec<TextOnlyFile>> {
        let mut st = self.conn.prepare_cached(
            "SELECT id, path, kind, content_hash FROM files
             WHERE state = 4 AND content_hash IS NOT NULL
             GROUP BY content_hash ORDER BY MAX(mtime) DESC, id LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(TextOnlyFile { id: FileId(r.get(0)?), path: r.get(1)?, kind: r.get(2)?, hash: r.get(3)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The vectors still owed for `hash`, in chunk order.
    pub fn pending_vecs(&self, hash: &str) -> Result<Vec<PendingVec>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT chunk, kind, locator FROM pending_vecs WHERE hash = ?1 ORDER BY chunk")?;
        let rows = st.query_map([hash], |r| {
            let kind: String = r.get(1)?;
            Ok(PendingVec { chunk: r.get(0)?, kind: VecKind::parse(&kind).unwrap_or(VecKind::Text), locator: r.get(2)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The stored chunk texts of `hash`, in chunk order.
    pub fn texts_of(&self, hash: &str) -> Result<Vec<(u32, String)>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT chunk, body FROM texts WHERE hash = ?1 ORDER BY CAST(chunk AS INTEGER)")?;
        let rows = st.query_map([hash], |r| Ok((r.get::<_, i64>(0)? as u32, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Store the owed vectors for `hash` and move every text-only file with that content to done.
    pub fn finish_vectors(&mut self, hash: &str, vectors: &[ChunkVec]) -> Result<()> {
        for v in vectors {
            if v.vec.len() != self.dims {
                return Err(Error::BadVector { expected: self.dims, got: v.vec.len() });
            }
        }
        let tx = self.conn.transaction()?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT OR REPLACE INTO vectors(hash, chunk, kind, locator, vec) VALUES (?1,?2,?3,?4,?5)",
            )?;
            for v in vectors {
                let mut blob = Vec::with_capacity(v.vec.len() * 4);
                for x in &v.vec {
                    blob.extend_from_slice(&x.to_le_bytes());
                }
                ins.execute(params![hash, v.chunk, v.kind.as_str(), v.locator, blob])?;
            }
        }
        tx.execute("DELETE FROM pending_vecs WHERE hash = ?1", [hash])?;
        tx.execute("UPDATE files SET state = 1 WHERE content_hash = ?1 AND state = 4", [hash])?;
        tx.commit()?;
        Ok(())
    }

    pub fn fail(&mut self, file_id: FileId, msg: &str) -> Result<()> {
        self.set_state(file_id, STATE_FAILED, msg)
    }

    pub fn skip(&mut self, file_id: FileId, reason: &str) -> Result<()> {
        self.set_state(file_id, STATE_SKIPPED, reason)
    }

    /// Every file row as `(path, kind)`, for the stale-row purge.
    pub fn all_file_paths(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare_cached("SELECT path, kind FROM files")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Skipped files whose skip reason is one of `reasons` go back to pending (their cause may be
    /// gone: a cloud file was hydrated, a codec got installed). Returns how many.
    pub fn requeue_skipped(&mut self, reasons: &[&str]) -> Result<usize> {
        let mut n = 0;
        for r in reasons {
            n += self.conn.prepare_cached("UPDATE files SET state=0, error=NULL WHERE state=?1 AND error=?2")?.execute(params![STATE_SKIPPED, r])?;
        }
        Ok(n)
    }

    /// Pending files per kind (what is queued, by what it needs).
    pub fn pending_by_kind(&self) -> Result<Vec<(String, u64)>> {
        let mut st = self.conn.prepare_cached("SELECT kind, COUNT(*) FROM files WHERE state = ?1 GROUP BY kind ORDER BY 2 DESC")?;
        let rows = st.query_map([STATE_PENDING], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Pending files of `kinds` become skipped with `reason` (they cannot be processed on this machine
    /// right now; `requeue_skipped` brings them back). Returns how many.
    pub fn skip_pending_kinds(&mut self, kinds: &[&str], reason: &str) -> Result<usize> {
        let mut n = 0;
        for k in kinds {
            n += self
                .conn
                .prepare_cached("UPDATE files SET state=?1, error=?2, content_hash=NULL WHERE state=?3 AND kind=?4")?
                .execute(params![STATE_SKIPPED, reason, STATE_PENDING, k])?;
        }
        Ok(n)
    }

    /// Failed (`skipped == false`) or skipped (`true`) files grouped by their recorded reason, biggest
    /// group first, with up to `examples` paths each. At most `max_groups` groups.
    pub fn reason_groups(&self, skipped: bool, examples: usize, max_groups: usize) -> Result<Vec<ReasonGroup>> {
        let state = if skipped { STATE_SKIPPED } else { STATE_FAILED };
        let mut st = self.conn.prepare_cached("SELECT COALESCE(error, ''), path FROM files WHERE state = ?1 ORDER BY id DESC")?;
        let mut rows = st.query([state])?;
        let mut groups: Vec<ReasonGroup> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        while let Some(r) = rows.next()? {
            let (reason, path): (String, String) = (r.get(0)?, r.get(1)?);
            let i = *index.entry(reason.clone()).or_insert_with(|| {
                groups.push(ReasonGroup { reason, count: 0, examples: Vec::new() });
                groups.len() - 1
            });
            groups[i].count += 1;
            if groups[i].examples.len() < examples {
                groups[i].examples.push(path);
            }
        }
        groups.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.reason.cmp(&b.reason)));
        groups.truncate(max_groups);
        Ok(groups)
    }

    /// The reason / message recorded for a skipped or failed file at `path` (case-insensitive).
    pub fn file_error(&self, path: &str) -> Result<Option<String>> {
        let v: Option<Option<String>> = self
            .conn
            .query_row("SELECT error FROM files WHERE path = ?1 COLLATE NOCASE", [norm_sep(path)], |r| r.get(0))
            .optional()?;
        Ok(v.flatten())
    }

    fn set_state(&mut self, id: FileId, state: i64, msg: &str) -> Result<()> {
        let n = self
            .conn
            .prepare_cached("UPDATE files SET state=?2, error=?3, content_hash=NULL WHERE id=?1")?
            .execute(params![id.0, state, msg])?;
        if n == 0 {
            return Err(Error::NoSuchFile(id));
        }
        Ok(())
    }

    // ------------------------------------------------------------ search

    /// Brute-force cosine search. One hit per file (its best chunk); files sharing a hash
    /// each get a hit. Only files in state `done` are considered.
    pub fn search_vectors(
        &self,
        query: &[f32],
        k: usize,
        kind: Option<VecKind>,
    ) -> Result<Vec<VecHit>> {
        if query.len() != self.dims {
            return Err(Error::BadVector {
                expected: self.dims,
                got: query.len(),
            });
        }
        if k == 0 {
            return Ok(Vec::new());
        }
        // hash -> files that are done with that content
        let mut by_hash: HashMap<String, Vec<(FileId, String)>> = HashMap::new();
        {
            let mut st = self.conn.prepare_cached(
                "SELECT content_hash, id, path FROM files WHERE state = 1 AND content_hash IS NOT NULL AND kind != 'code'",
            )?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                by_hash
                    .entry(r.get(0)?)
                    .or_default()
                    .push((FileId(r.get(1)?), r.get(2)?));
            }
        }
        // best chunk per hash
        let mut best: HashMap<String, (f32, u32)> = HashMap::new();
        {
            let sql = if kind.is_some() {
                "SELECT hash, chunk, vec FROM vectors WHERE kind = ?1"
            } else {
                "SELECT hash, chunk, vec FROM vectors WHERE ?1 IS NULL"
            };
            let mut st = self.conn.prepare_cached(sql)?;
            let kind_s = kind.map(VecKind::as_str);
            let mut rows = st.query(params![kind_s])?;
            while let Some(r) = rows.next()? {
                let hash = r.get_ref(0)?.as_str()?;
                if !by_hash.contains_key(hash) {
                    continue;
                }
                let blob = r.get_ref(2)?.as_blob()?;
                if blob.len() != self.dims * 4 {
                    continue;
                }
                let score = dot_le(query, blob);
                let chunk: u32 = r.get(1)?;
                match best.get_mut(hash) {
                    Some(b) => {
                        if score > b.0 {
                            *b = (score, chunk);
                        }
                    }
                    None => {
                        best.insert(hash.to_owned(), (score, chunk));
                    }
                }
            }
        }
        // top-k over files with a min-heap
        let mut heap: BinaryHeap<Reverse<Scored>> = BinaryHeap::new();
        for (hash, (score, chunk)) in &best {
            for (fid, _) in &by_hash[hash] {
                let item = Scored {
                    score: *score,
                    file: *fid,
                    hash: hash.clone(),
                    chunk: *chunk,
                };
                if heap.len() < k {
                    heap.push(Reverse(item));
                } else if heap.peek().is_some_and(|m| item > m.0) {
                    heap.pop();
                    heap.push(Reverse(item));
                }
            }
        }
        let mut top: Vec<Scored> = heap.into_iter().map(|r| r.0).collect();
        top.sort_by(|a, b| b.cmp(a));
        let mut out = Vec::with_capacity(top.len());
        for s in top {
            let path = by_hash[&s.hash]
                .iter()
                .find(|(f, _)| *f == s.file)
                .map(|(_, p)| p.clone())
                .unwrap_or_default();
            let locator: Option<String> = self
                .conn
                .prepare_cached("SELECT locator FROM vectors WHERE hash=?1 AND chunk=?2")?
                .query_row(params![s.hash, s.chunk], |r| r.get(0))?;
            out.push(VecHit {
                file_id: s.file,
                path,
                chunk: s.chunk,
                locator,
                score: s.score,
            });
        }
        Ok(out)
    }

    /// Calibrated search by meaning (see [`relevance`]): every vector is scored against the
    /// query, hub-corrected with `hub`, and each file content's best `adj` per modality is placed
    /// in that modality's distribution. Returns the best `per_modality` file contents of each
    /// modality as candidates (one per file path), ungated: [`relevance::gate`] decides.
    pub fn search_calibrated(&self, query: &[f32], hub: &[f32], per_modality: usize) -> Result<Vec<Candidate>> {
        self.search_calibrated_multi(&[query], hub, per_modality, &HashSet::new())
    }

    /// [`Store::search_calibrated`] for several phrasings of one wish at once (a collection's
    /// "receipts, invoices or bills" is three queries): one pass over the vectors, each query
    /// calibrated on its own (own best chunk, own distribution), and a content is ranked by the
    /// query it stands out most for. Contents whose hash is in `skip` do not take part at all:
    /// they are neither candidates nor part of the population a hit must stand out from.
    pub fn search_calibrated_multi(
        &self,
        queries: &[&[f32]],
        hub: &[f32],
        per_modality: usize,
        skip: &HashSet<String>,
    ) -> Result<Vec<Candidate>> {
        self.calibrated(queries, hub, per_modality, skip, None)
    }

    /// Every content that passes the relevance gate for `query` (see [`relevance`]), with no
    /// count limit: what "Show all" lists. Costs the same single pass over the vectors as
    /// [`Store::search_calibrated`]; only the contents that pass are looked up further.
    pub fn search_relevant(&self, query: &[f32], hub: &[f32]) -> Result<Vec<Candidate>> {
        self.calibrated(&[query], hub, usize::MAX, &HashSet::new(), Some(relevance::Z_MIN))
    }

    /// `passing`: keep only the contents that pass the gate at this strictness.
    fn calibrated(
        &self,
        queries: &[&[f32]],
        hub: &[f32],
        per_modality: usize,
        skip: &HashSet<String>,
        passing: Option<f32>,
    ) -> Result<Vec<Candidate>> {
        if let Some(bad) = queries.iter().map(|q| q.len()).chain([hub.len()]).find(|n| *n != self.dims) {
            return Err(Error::BadVector { expected: self.dims, got: bad });
        }
        let nq = queries.len();
        if nq == 0 {
            return Ok(Vec::new());
        }
        let mut by_hash: HashMap<String, Vec<(FileId, String)>> = HashMap::new();
        {
            let mut st = self.conn.prepare_cached(
                "SELECT content_hash, id, path FROM files WHERE state = 1 AND content_hash IS NOT NULL AND kind != 'code'",
            )?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let hash: String = r.get(0)?;
                if skip.contains(&hash) {
                    continue;
                }
                by_hash.entry(hash).or_default().push((FileId(r.get(1)?), r.get(2)?));
            }
        }
        // (hash, modality) -> per query (adj, raw, chunk)
        type PerQuery = Vec<(f32, f32, u32)>;
        let mut best: HashMap<(String, Modality), PerQuery> = HashMap::new();
        {
            let mut st = self.conn.prepare_cached("SELECT hash, chunk, kind, vec FROM vectors")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let hash = r.get_ref(0)?.as_str()?;
                if !by_hash.contains_key(hash) {
                    continue;
                }
                let Some(kind) = VecKind::parse(r.get_ref(2)?.as_str()?) else { continue };
                let blob = r.get_ref(3)?.as_blob()?;
                if blob.len() != self.dims * 4 {
                    continue;
                }
                let hub_dot = dot_le(hub, blob);
                let chunk: u32 = r.get(1)?;
                let slot = best
                    .entry((hash.to_owned(), Modality::from(kind)))
                    .or_insert_with(|| vec![(f32::NEG_INFINITY, 0.0, 0); nq]);
                for (qi, q) in queries.iter().enumerate() {
                    let raw = dot_le(q, blob);
                    let adj = raw - hub_dot;
                    if adj > slot[qi].0 {
                        slot[qi] = (adj, raw, chunk);
                    }
                }
            }
        }
        type Pick<'a> = (&'a String, f32, f32, u32, Option<f32>);
        let mut out = Vec::new();
        for m in [Modality::Visual, Modality::Text, Modality::Audio] {
            let items: Vec<(&String, &PerQuery)> =
                best.iter().filter(|((_, km), _)| *km == m).map(|((h, _), v)| (h, v)).collect();
            let n = items.len();
            let stats: Vec<(f32, f32)> = (0..nq)
                .map(|qi| relevance::mean_sd(&items.iter().map(|i| i.1[qi].0).collect::<Vec<f32>>()))
                .collect();
            // each content under the query it stands out most for: (hash, adj, raw, chunk, z)
            let mut picked: Vec<Pick<'_>> = items
                .iter()
                .map(|(hash, per)| {
                    let mut win = 0usize;
                    let mut win_key = f32::NEG_INFINITY;
                    for (qi, v) in per.iter().enumerate() {
                        let key = relevance::z_of(v.0, n, stats[qi].0, stats[qi].1).unwrap_or(v.0);
                        if key > win_key {
                            win = qi;
                            win_key = key;
                        }
                    }
                    let (adj, raw, chunk) = per[win];
                    (*hash, adj, raw, chunk, relevance::z_of(adj, n, stats[win].0, stats[win].1))
                })
                .collect();
            let key = |i: &Pick<'_>| i.4.unwrap_or(i.1);
            picked.sort_by(|a, b| key(b).total_cmp(&key(a)).then_with(|| a.0.cmp(b.0)));
            let keep = |adj: f32, z: Option<f32>| passing.is_none_or(|zm| adj >= relevance::ADJ_MIN && z.is_none_or(|z| z >= zm));
            for (hash, adj, raw, chunk, z) in picked.into_iter().filter(|p| keep(p.1, p.4)).take(per_modality) {
                let locator: Option<String> = self
                    .conn
                    .prepare_cached("SELECT locator FROM vectors WHERE hash=?1 AND chunk=?2")?
                    .query_row(params![hash, chunk], |r| r.get(0))?;
                for (fid, path) in &by_hash[hash] {
                    out.push(Candidate {
                        file_id: *fid,
                        path: path.clone(),
                        hash: hash.clone(),
                        chunk,
                        locator: locator.clone(),
                        modality: m,
                        score: raw,
                        adj,
                        z,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Full-text search over extracted text (FTS5 syntax; see [`fts_escape`] for raw user
    /// input). One hit per file, best bm25 first.
    pub fn search_text(&self, fts_query: &str, k: usize) -> Result<Vec<TextHit>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare_cached(
            "SELECT files.id, files.path, texts.chunk,
                    snippet(texts, 2, '[', ']', '…', 12), bm25(texts),
                    COALESCE((SELECT v.locator FROM vectors v WHERE v.hash = texts.hash AND v.chunk = texts.chunk),
                             (SELECT p.locator FROM pending_vecs p WHERE p.hash = texts.hash AND p.chunk = texts.chunk))
             FROM texts JOIN files ON files.content_hash = texts.hash AND files.state IN (1, 4) AND files.kind != 'code'
             WHERE texts MATCH ?1
             ORDER BY bm25(texts) LIMIT ?2",
        )?;
        let rows = st.query_map(params![fts_query, (k * 5) as i64], |r| {
            Ok(TextHit {
                file_id: FileId(r.get(0)?),
                path: r.get(1)?,
                chunk: r.get(2)?,
                snippet: r.get(3)?,
                rank: r.get(4)?,
                locator: r.get(5)?,
            })
        })?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for h in rows {
            let h = h?;
            if seen.insert(h.file_id) {
                out.push(h);
                if out.len() == k {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Full-text search over the *code* partition only (`code:` queries). Same shape as
    /// [`Store::search_text`]; code never appears in any other search.
    pub fn search_code(&self, fts_query: &str, k: usize) -> Result<Vec<TextHit>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare_cached(
            "SELECT files.id, files.path, texts.chunk, snippet(texts, 2, '[', ']', '…', 12), bm25(texts)
             FROM texts JOIN files ON files.content_hash = texts.hash AND files.state IN (1, 4) AND files.kind = 'code'
             WHERE texts MATCH ?1
             ORDER BY bm25(texts) LIMIT ?2",
        )?;
        let rows = st.query_map(params![fts_query, (k * 5) as i64], |r| {
            Ok(TextHit {
                file_id: FileId(r.get(0)?),
                path: r.get(1)?,
                chunk: r.get(2)?,
                snippet: r.get(3)?,
                rank: r.get(4)?,
                locator: None,
            })
        })?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for h in rows {
            let h = h?;
            if seen.insert(h.file_id) {
                out.push(h);
                if out.len() == k {
                    break;
                }
            }
        }
        Ok(out)
    }

    pub fn stats(&self) -> Result<Stats> {
        // One pass over `files` (polled by the UI; was one scan per state).
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(state = {STATE_PENDING}), 0), COALESCE(SUM(state = {STATE_DONE}), 0), \
             COALESCE(SUM(state = {STATE_FAILED}), 0), COALESCE(SUM(state = {STATE_SKIPPED}), 0), \
             COALESCE(SUM(state = {STATE_TEXT_ONLY}), 0), (SELECT COUNT(*) FROM vectors) FROM files"
        );
        let n = |r: &rusqlite::Row<'_>, i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
        Ok(self.conn.query_row(&sql, [], |r| {
            Ok(Stats { files: n(r, 0)?, pending: n(r, 1)?, done: n(r, 2)?, failed: n(r, 3)?, skipped: n(r, 4)?, text_only: n(r, 5)?, vectors: n(r, 6)? })
        })?)
    }

    // ------------------------------------------------------------ library

    fn image_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ImageRow> {
        Ok(ImageRow {
            file_id: FileId(r.get(0)?),
            path: r.get(1)?,
            size: r.get::<_, i64>(2)?.max(0) as u64,
            mtime: r.get(3)?,
            content_hash: r.get(4)?,
        })
    }

    /// Every finished image file, newest first (the library timeline).
    pub fn list_images(&self) -> Result<Vec<ImageRow>> {
        let mut st = self.conn.prepare_cached(
            "SELECT id, path, size, mtime, content_hash FROM files
             WHERE state = 1 AND kind = 'image' AND content_hash IS NOT NULL
             ORDER BY mtime DESC, id DESC",
        )?;
        let rows = st.query_map([], Self::image_row)?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Pixel size of every image content measured so far (`(0, 0)` = could not be read).
    pub fn image_dims(&self) -> Result<HashMap<String, (u32, u32)>> {
        let mut st = self.conn.prepare_cached("SELECT hash, w, h FROM image_dims")?;
        let mut rows = st.query([])?;
        let mut out = HashMap::new();
        while let Some(r) = rows.next()? {
            out.insert(r.get(0)?, (r.get::<_, i64>(1)?.max(0) as u32, r.get::<_, i64>(2)?.max(0) as u32));
        }
        Ok(out)
    }

    /// Remember pixel sizes (by content hash), replacing older values.
    pub fn set_image_dims(&mut self, rows: &[(String, u32, u32)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached("INSERT OR REPLACE INTO image_dims(hash, w, h) VALUES (?1, ?2, ?3)")?;
            for (hash, w, h) in rows {
                st.execute(params![hash, w, h])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Byte-identical image files (same content hash), biggest saving first. Documents and
    /// source files are not offered for clean-up.
    pub fn dup_groups(&self) -> Result<Vec<DupGroup>> {
        let mut st = self.conn.prepare_cached(
            "SELECT id, path, size, mtime, content_hash FROM files
             WHERE state = 1 AND kind = 'image' AND content_hash IN (
                 SELECT content_hash FROM files WHERE state = 1 AND kind = 'image' AND content_hash IS NOT NULL
                 GROUP BY content_hash HAVING COUNT(*) > 1)
             ORDER BY content_hash, mtime DESC, id DESC",
        )?;
        let mut groups: Vec<DupGroup> = Vec::new();
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let row = Self::image_row(r)?;
            match groups.last_mut() {
                Some(g) if g.hash == row.content_hash => g.files.push(row),
                _ => groups.push(DupGroup { hash: row.content_hash.clone(), files: vec![row] }),
            }
        }
        groups.sort_by(|a, b| b.reclaimable().cmp(&a.reclaimable()).then_with(|| a.hash.cmp(&b.hash)));
        Ok(groups)
    }

    /// Finished images modified in `[from_ms, to_ms)` with their image vector (chunk 0), newest
    /// first. Used by the near-duplicate scan, one time bucket at a time.
    pub fn images_with_vectors(&self, from_ms: i64, to_ms: i64) -> Result<Vec<(ImageRow, Vec<f32>)>> {
        let mut st = self.conn.prepare_cached(
            "SELECT f.id, f.path, f.size, f.mtime, f.content_hash, v.vec
             FROM files f JOIN vectors v ON v.hash = f.content_hash AND v.chunk = 0 AND v.kind = 'image'
             WHERE f.state = 1 AND f.kind = 'image' AND f.mtime >= ?1 AND f.mtime < ?2
             ORDER BY f.mtime DESC, f.id DESC",
        )?;
        let mut rows = st.query(params![from_ms, to_ms])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let row = Self::image_row(r)?;
            let blob: Vec<u8> = r.get(5)?;
            if blob.len() != self.dims * 4 {
                continue;
            }
            let v = blob.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            out.push((row, v));
        }
        Ok(out)
    }

    // ------------------------------------------------------------ parse queue

    /// Queue an item for the document parser (no-op when it is already queued / done / skipped).
    /// Returns whether a row was added.
    pub fn queue_parse(&mut self, hash: &str, page: u32, reason: &str) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO parse_queue(hash, page, reason) VALUES (?1, ?2, ?3)",
            params![hash, page, reason],
        )?;
        Ok(n > 0)
    }

    /// Up to `limit` queued items, newest file first. Items whose content no live file has any more
    /// are left out (the next `gc` drops them).
    pub fn next_parse(&self, limit: usize) -> Result<Vec<ParseJob>> {
        let mut st = self.conn.prepare_cached(
            "SELECT q.hash, q.page, q.reason, q.attempts, f.path, MAX(f.mtime)
             FROM parse_queue q JOIN files f ON f.content_hash = q.hash AND f.state IN (1, 4)
             WHERE q.state = 0
             GROUP BY q.hash, q.page ORDER BY MAX(f.mtime) DESC, q.hash, q.page LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(ParseJob { hash: r.get(0)?, page: r.get(1)?, reason: r.get(2)?, attempts: r.get(3)?, path: r.get(4)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The newest live file with this content, if any.
    pub fn live_path(&self, hash: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT path FROM files WHERE content_hash = ?1 AND state IN (1, 4) ORDER BY mtime DESC, id LIMIT 1",
                [hash],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Replace the item's text with the parser's output and mark it done, in one transaction.
    pub fn finish_parse(&mut self, hash: &str, page: u32, result: &ParseResult) -> Result<()> {
        let tx = self.conn.transaction()?;
        replace_item_text(&tx, self.dims, hash, page, result)?;
        tx.execute(
            "UPDATE parse_queue SET state = ?3, error = NULL WHERE hash = ?1 AND page = ?2",
            params![hash, page, STATE_DONE],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Count a failed attempt (the item stays queued); returns the attempts so far.
    pub fn bump_parse(&mut self, hash: &str, page: u32, error: &str) -> Result<u32> {
        self.conn.execute(
            "UPDATE parse_queue SET attempts = attempts + 1, error = ?3 WHERE hash = ?1 AND page = ?2",
            params![hash, page, error],
        )?;
        Ok(self
            .conn
            .query_row("SELECT attempts FROM parse_queue WHERE hash = ?1 AND page = ?2", params![hash, page], |r| r.get(0))
            .optional()?
            .unwrap_or(0))
    }

    /// Give up on the item.
    pub fn fail_parse(&mut self, hash: &str, page: u32, error: &str) -> Result<()> {
        self.set_parse_state(hash, page, STATE_FAILED, error)
    }

    /// Leave the item as it is (e.g. `empty` parser output, `script` not supported).
    pub fn skip_parse(&mut self, hash: &str, page: u32, reason: &str) -> Result<()> {
        self.set_parse_state(hash, page, STATE_SKIPPED, reason)
    }

    /// Like [`skip_parse`](Self::skip_parse), but also stores `result` as the item's text (the OCR text of
    /// a document image the parser does not support, so it still becomes text-searchable).
    pub fn skip_parse_replacing(&mut self, hash: &str, page: u32, reason: &str, result: &ParseResult) -> Result<()> {
        let tx = self.conn.transaction()?;
        replace_item_text(&tx, self.dims, hash, page, result)?;
        tx.execute(
            "UPDATE parse_queue SET state = ?3, error = ?4 WHERE hash = ?1 AND page = ?2",
            params![hash, page, STATE_SKIPPED, reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn set_parse_state(&mut self, hash: &str, page: u32, state: i64, msg: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE parse_queue SET state = ?3, error = ?4 WHERE hash = ?1 AND page = ?2",
            params![hash, page, state, msg],
        )?;
        Ok(())
    }

    pub fn parse_counts(&self) -> Result<ParseCounts> {
        let n = |s: i64| -> Result<u64> {
            Ok(self.conn.query_row("SELECT COUNT(*) FROM parse_queue WHERE state = ?1", [s], |r| r.get::<_, i64>(0))? as u64)
        };
        Ok(ParseCounts { queued: n(STATE_PENDING)?, done: n(STATE_DONE)?, failed: n(STATE_FAILED)?, skipped: n(STATE_SKIPPED)? })
    }

    /// Standalone image vectors (chunk kind `image` without a PDF page in the locator) with a rowid
    /// above `after_rowid`, oldest first, for the document-image route pass.
    pub fn image_vectors_after(&self, after_rowid: i64, limit: usize) -> Result<Vec<RouteImage>> {
        let mut st = self.conn.prepare_cached(
            "SELECT rowid, hash, vec FROM vectors
             WHERE rowid > ?1 AND kind = 'image'
               AND (locator IS NULL OR NOT json_valid(locator) OR json_extract(locator, '$.page') IS NULL)
             ORDER BY rowid LIMIT ?2",
        )?;
        let mut rows = st.query(params![after_rowid, limit as i64])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let blob = r.get_ref(2)?.as_blob()?;
            if blob.len() != self.dims * 4 {
                continue;
            }
            let vec = blob.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            out.push(RouteImage { rowid: r.get(0)?, hash: r.get(1)?, vec });
        }
        Ok(out)
    }

    /// Highest rowid in `vectors` (0 when empty).
    pub fn max_vector_rowid(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COALESCE(MAX(rowid), 0) FROM vectors", [], |r| r.get(0))?)
    }

    /// Every still-waiting `(hash, page)` pair, for the one-time routing-gate re-check.
    pub fn queued_all(&self) -> Result<Vec<(String, u32)>> {
        let mut st = self.conn.prepare_cached("SELECT hash, page FROM parse_queue WHERE state = ?1")?;
        let rows = st.query_map([STATE_PENDING], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The stored text belonging to one page: chunk texts whose chunk's locator carries that
    /// `page` (locator on the vector, or on the pending vector while the embedding is owed).
    /// `page` 0 means a standalone image's chunks, whose locator has no page. Empty when the
    /// item has no text for that page.
    pub fn page_text(&self, hash: &str, page: u32) -> Result<String> {
        let mut st = self.conn.prepare_cached(
            "SELECT t.body FROM texts t
             LEFT JOIN vectors v ON v.hash = t.hash AND v.chunk = t.chunk
             LEFT JOIN pending_vecs p ON p.hash = t.hash AND p.chunk = t.chunk
             WHERE t.hash = ?1
               AND (CASE WHEN ?2 = 0
                    THEN COALESCE(json_extract(v.locator, '$.page'), json_extract(p.locator, '$.page')) IS NULL
                    ELSE COALESCE(json_extract(v.locator, '$.page'), json_extract(p.locator, '$.page')) = ?2 END)
             ORDER BY CAST(t.chunk AS INTEGER)",
        )?;
        let rows = st.query_map(params![hash, page], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?.join("\n"))
    }

    // ------------------------------------------------------------ settings

    /// Free-form string setting (`None` when unset).
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        Ok(v)
    }

    /// Set (`Some`) or clear (`None`) a free-form string setting.
    pub fn set_setting(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => {
                self.conn.execute(
                    "INSERT INTO settings(key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, v],
                )?;
            }
            None => {
                self.conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
            }
        }
        Ok(())
    }

    /// Opt-in: also index source-code text (setting `index_code`, default off).
    pub fn get_index_code(&self) -> Result<bool> {
        Ok(self.get_setting("index_code")?.as_deref() == Some("1"))
    }

    pub fn set_index_code(&mut self, on: bool) -> Result<()> {
        self.set_setting("index_code", on.then_some("1"))
    }

    pub fn get_scopes(&self) -> Result<Scopes> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = 'scopes'", [], |r| {
                r.get(0)
            })
            .optional()?;
        match v {
            Some(s) => Ok(serde_json::from_str(&s)?),
            None => Ok(Scopes::default()),
        }
    }

    pub fn set_scopes(&mut self, scopes: &Scopes) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES ('scopes', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [serde_json::to_string(scopes)?],
        )?;
        Ok(())
    }

    /// The user's ignore list, or [`DEFAULT_IGNORE`] when never saved.
    pub fn get_ignore(&self) -> Result<Vec<String>> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = 'ignore'", [], |r| r.get(0))
            .optional()?;
        match v {
            Some(s) => Ok(serde_json::from_str(&s)?),
            None => Ok(default_ignore()),
        }
    }

    /// Save the ignore list (trimmed, blanks and duplicates dropped, order kept);
    /// `None` forgets it so the defaults apply again.
    pub fn set_ignore(&mut self, list: Option<&[String]>) -> Result<()> {
        let Some(list) = list else {
            self.conn.execute("DELETE FROM settings WHERE key = 'ignore'", [])?;
            return Ok(());
        };
        let mut seen = HashSet::new();
        let clean: Vec<&str> = list
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && seen.insert(s.to_lowercase()))
            .collect();
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES ('ignore', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [serde_json::to_string(&clean)?],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------- internals

#[derive(Debug, PartialEq)]
struct Scored {
    score: f32,
    file: FileId,
    hash: String,
    chunk: u32,
}

impl Eq for Scored {}

impl Ord for Scored {
    fn cmp(&self, o: &Self) -> Ordering {
        self.score
            .total_cmp(&o.score)
            .then_with(|| o.file.0.cmp(&self.file.0))
    }
}

impl PartialOrd for Scored {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

fn dot_le(q: &[f32], blob: &[u8]) -> f32 {
    let (words, _) = blob.as_chunks::<4>();
    q.iter()
        .zip(words)
        .map(|(a, b)| a * f32::from_le_bytes(*b))
        .sum()
}

fn norm_sep(p: &str) -> String {
    p.replace('/', "\\")
}

fn like_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '^') {
            o.push('^');
        }
        o.push(c);
    }
    o
}

/// `page` of a locator JSON (0 when absent or unparseable): standalone images have none.
fn locator_page(locator: Option<&str>) -> u32 {
    locator
        .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .and_then(|v| v["page"].as_u64())
        .unwrap_or(0) as u32
}

/// The body of [`Store::finish_parse`]: drop the item's text chunks (OCR text, or an earlier parse),
/// add the new chunks from `MAX(chunk)+1`, and rewrite the locator of its image / page vector.
/// New chunks without a vector become pending vectors, which moves a done file to `text_only` so
/// the upgrade pass embeds them.
fn replace_item_text(tx: &rusqlite::Transaction<'_>, dims: usize, hash: &str, page: u32, result: &ParseResult) -> Result<()> {
    for c in &result.chunks {
        if let Some(v) = &c.vec {
            if v.len() != dims {
                return Err(Error::BadVector { expected: dims, got: v.len() });
            }
        }
    }
    let rows = |sql: &str| -> Result<Vec<(u32, Option<String>)>> {
        let mut st = tx.prepare_cached(sql)?;
        let r = st.query_map([hash], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, Option<String>>(1)?)))?;
        Ok(r.collect::<std::result::Result<_, _>>()?)
    };
    let in_page = |r: &&(u32, Option<String>)| locator_page(r.1.as_deref()) == page;
    let vec_text = rows("SELECT chunk, locator FROM vectors WHERE hash = ?1 AND kind = 'text'")?;
    let pend_text = rows("SELECT chunk, locator FROM pending_vecs WHERE hash = ?1 AND kind = 'text'")?;
    let mut old: Vec<u32> = vec_text.iter().chain(&pend_text).filter(in_page).map(|r| r.0).collect();
    if page == 0 {
        // text rows that never got a vector (embedding failed) have no locator to match on
        let known: HashSet<u32> = vec_text.iter().chain(&pend_text).map(|r| r.0).collect();
        old.extend(rows("SELECT chunk, NULL FROM texts WHERE hash = ?1")?.into_iter().map(|r| r.0).filter(|c| !known.contains(c)));
    }
    let next: u32 = tx.query_row(
        "SELECT COALESCE(MAX(c), -1) + 1 FROM (
             SELECT chunk AS c FROM vectors WHERE hash = ?1 UNION ALL
             SELECT chunk FROM pending_vecs WHERE hash = ?1 UNION ALL
             SELECT chunk FROM texts WHERE hash = ?1)",
        [hash],
        |r| r.get(0),
    )?;
    for c in &old {
        tx.execute("DELETE FROM texts WHERE hash = ?1 AND chunk = ?2", params![hash, c])?;
        tx.execute("DELETE FROM vectors WHERE hash = ?1 AND chunk = ?2", params![hash, c])?;
        tx.execute("DELETE FROM pending_vecs WHERE hash = ?1 AND chunk = ?2", params![hash, c])?;
    }
    for (i, c) in result.chunks.iter().enumerate() {
        let chunk = next + i as u32;
        tx.execute("INSERT INTO texts(hash, chunk, body) VALUES (?1, ?2, ?3)", params![hash, chunk, c.text])?;
        match &c.vec {
            Some(v) => {
                let mut blob = Vec::with_capacity(v.len() * 4);
                for x in v {
                    blob.extend_from_slice(&x.to_le_bytes());
                }
                tx.execute(
                    "INSERT INTO vectors(hash, chunk, kind, locator, vec) VALUES (?1, ?2, 'text', ?3, ?4)",
                    params![hash, chunk, c.locator, blob],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO pending_vecs(hash, chunk, kind, locator) VALUES (?1, ?2, 'text', ?3)",
                    params![hash, chunk, c.locator],
                )?;
            }
        }
    }
    for table in ["vectors", "pending_vecs"] {
        let imgs = rows(&format!("SELECT chunk, locator FROM {table} WHERE hash = ?1 AND kind IN ('image', 'page')"))?;
        for (chunk, _) in imgs.iter().filter(in_page) {
            tx.execute(
                &format!("UPDATE {table} SET locator = ?3 WHERE hash = ?1 AND chunk = ?2"),
                params![hash, chunk, result.locator],
            )?;
        }
    }
    let waiting = tx.prepare_cached("SELECT 1 FROM pending_vecs WHERE hash = ?1")?.exists([hash])?;
    if waiting {
        tx.execute("UPDATE files SET state = ?2 WHERE content_hash = ?1 AND state = ?3", params![hash, STATE_TEXT_ONLY, STATE_DONE])?;
    }
    Ok(())
}

/// Everything derived from file content goes; files go back to pending; settings stay.
fn clear_derived(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "BEGIN;
         DELETE FROM vectors;
         DELETE FROM texts;
         DELETE FROM content;
         DELETE FROM pending_vecs;
         DELETE FROM parse_queue;
         DELETE FROM pdf_tail;
         DELETE FROM ref_vectors;
         DELETE FROM settings WHERE key = 'parse_route_cursor';
         DELETE FROM index_meta;
         UPDATE files SET content_hash = NULL, state = 0, error = NULL;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v > SCHEMA_VERSION {
        return Err(Error::SchemaTooNew(v));
    }
    if v < 1 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS index_meta(
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 model_id TEXT NOT NULL, llama_build TEXT NOT NULL, quant TEXT NOT NULL,
                 image_budget INTEGER NOT NULL, dims INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS files(
                 id INTEGER PRIMARY KEY,
                 vol_guid TEXT NOT NULL, frn INTEGER NOT NULL,
                 path TEXT NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL,
                 kind TEXT NOT NULL, content_hash TEXT,
                 state INTEGER NOT NULL DEFAULT 0, error TEXT,
                 UNIQUE(vol_guid, frn));
             CREATE INDEX IF NOT EXISTS files_path ON files(path);
             CREATE INDEX IF NOT EXISTS files_queue ON files(state, mtime DESC);
             CREATE INDEX IF NOT EXISTS files_hash ON files(content_hash);
             CREATE TABLE IF NOT EXISTS content(hash TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS vectors(
                 hash TEXT NOT NULL, chunk INTEGER NOT NULL,
                 kind TEXT NOT NULL, locator TEXT, vec BLOB NOT NULL,
                 PRIMARY KEY(hash, chunk));
             CREATE VIRTUAL TABLE IF NOT EXISTS texts USING fts5(
                 hash UNINDEXED, chunk UNINDEXED, body,
                 tokenize = 'unicode61 remove_diacritics 2');
             CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    if v < 2 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS pending_vecs(
                 hash TEXT NOT NULL, chunk INTEGER NOT NULL,
                 kind TEXT NOT NULL, locator TEXT,
                 PRIMARY KEY(hash, chunk));
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    if v < 3 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS parse_queue(
                 hash TEXT NOT NULL, page INTEGER NOT NULL,
                 reason TEXT NOT NULL,
                 state INTEGER NOT NULL DEFAULT 0,
                 attempts INTEGER NOT NULL DEFAULT 0, error TEXT,
                 PRIMARY KEY(hash, page));
             PRAGMA user_version = 3;
             COMMIT;",
        )?;
    }
    if v < 4 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS image_dims(
                 hash TEXT PRIMARY KEY, w INTEGER NOT NULL, h INTEGER NOT NULL);
             PRAGMA user_version = 4;
             COMMIT;",
        )?;
    }
    if v < 5 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS pdf_tail(
                 hash TEXT PRIMARY KEY, next_page INTEGER NOT NULL, total INTEGER NOT NULL,
                 state INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, error TEXT);
             CREATE TABLE IF NOT EXISTS ref_vectors(
                 idx INTEGER PRIMARY KEY, build TEXT NOT NULL, vec BLOB NOT NULL);
             PRAGMA user_version = 5;
             COMMIT;",
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> IndexMeta {
        IndexMeta {
            model_id: "embgemma".into(),
            llama_build: "b11588".into(),
            quant: "Q8_0".into(),
            image_budget: 280,
            dims: 4,
        }
    }

    fn key(n: u64) -> FileKey {
        FileKey {
            vol_guid: "{vol}".into(),
            frn: n,
        }
    }

    fn cv(chunk: u32, v: [f32; 4]) -> ChunkVec {
        ChunkVec {
            chunk,
            kind: VecKind::Image,
            locator: Some(format!("{{\"page\":{chunk}}}")),
            vec: v.to_vec(),
        }
    }

    fn add(s: &mut Store, n: u64, path: &str, mtime: i64) -> FileId {
        s.upsert_file(&key(n), path, 10, mtime, "image").unwrap()
    }

    #[test]
    fn meta_mismatch_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        {
            let mut s = Store::open(&p, &meta()).unwrap();
            let id = add(&mut s, 1, r"C:\a.jpg", 1);
            s.complete(id, "h1", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
            s.set_scopes(&Scopes {
                include: vec![r"C:\a".into()],
                exclude: vec![],
                ..Default::default()
            })
            .unwrap();
        }
        let mut m2 = meta();
        m2.image_budget = 560;
        match Store::open(&p, &m2) {
            Err(Error::MetaMismatch { stored, requested }) => {
                assert_eq!(stored.image_budget, 280);
                assert_eq!(requested.image_budget, 560);
            }
            _ => panic!("expected mismatch"),
        }
        assert!(Store::open(&p, &meta()).is_ok());
        let s = Store::reset(&p, &m2).unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.files, st.pending, st.vectors), (1, 1, 0));
        assert_eq!(s.get_scopes().unwrap().include.len(), 1);
        drop(s);
        assert!(Store::open(&p, &m2).is_ok());
    }

    #[test]
    fn text_only_then_vectors_without_reextraction() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, r"C:.pdf", 1);
        let b = add(&mut s, 2, r"C:\copy.pdf", 2);
        let pend = [
            PendingVec { chunk: 0, kind: VecKind::Text, locator: Some("{\"page\":1}".into()) },
            PendingVec { chunk: 1, kind: VecKind::Image, locator: Some("{\"page\":2}".into()) },
        ];
        s.complete_text_only(a, "h", &pend, &[(0, "quarterly invoice".into())]).unwrap();
        // same content at another path: linked, and also waiting
        s.complete_text_only(b, "h", &pend, &[(0, "quarterly invoice".into())]).unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.text_only, st.done, st.pending, st.vectors), (2, 0, 0, 0));
        // searchable by text immediately, but not by vector
        assert_eq!(s.search_text(&fts_escape("invoice"), 5).unwrap().len(), 2);
        assert!(s.search_vectors(&[1., 0., 0., 0.], 5, None).unwrap().is_empty());
        // the queue offers one entry per content, with everything needed to finish it
        let q = s.next_text_only(10).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].hash, "h");
        assert_eq!(s.pending_vecs("h").unwrap(), pend);
        assert_eq!(s.texts_of("h").unwrap(), vec![(0, "quarterly invoice".to_string())]);
        // models arrive: vectors are stored, both files become done, FTS unchanged
        let vecs = [
            ChunkVec { chunk: 0, kind: VecKind::Text, locator: pend[0].locator.clone(), vec: vec![1., 0., 0., 0.] },
            ChunkVec { chunk: 1, kind: VecKind::Image, locator: pend[1].locator.clone(), vec: vec![0., 1., 0., 0.] },
        ];
        s.finish_vectors("h", &vecs).unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.text_only, st.done, st.vectors), (0, 2, 2));
        assert!(s.next_text_only(10).unwrap().is_empty());
        assert!(s.pending_vecs("h").unwrap().is_empty());
        assert_eq!(s.search_vectors(&[0., 1., 0., 0.], 5, None).unwrap().len(), 2);
        assert_eq!(s.search_text(&fts_escape("invoice"), 5).unwrap().len(), 2);
        // a later copy of the finished content is simply done
        let c = add(&mut s, 3, r"C:\c.pdf", 3);
        s.complete_text_only(c, "h", &pend, &[]).unwrap();
        assert_eq!(s.stats().unwrap().done, 3);
    }

    #[test]
    fn pending_filters_and_text_locator() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        s.upsert_file(&key(1), "a.png", 1, 5, "image").unwrap();
        s.upsert_file(&key(2), "b.pdf", 1, 4, "pdf").unwrap();
        s.upsert_file(&key(3), "c.docx", 1, 3, "doc").unwrap();
        s.upsert_file(&key(4), "d.png", 1, 2, "image").unwrap();
        let kinds = |v: Vec<PendingFile>| v.into_iter().map(|f| f.path).collect::<Vec<_>>();
        assert_eq!(kinds(s.next_pending_where(10, &[], &["image"], 0).unwrap()), ["b.pdf", "c.docx"]);
        assert_eq!(kinds(s.next_pending_where(10, &[], &["image", "pdf"], 0).unwrap()), ["c.docx"]);
        assert_eq!(kinds(s.next_pending_where(10, &["pdf"], &[], 0).unwrap()), ["b.pdf"]);
        // cursor walk over images by id
        let first = s.next_pending_where(1, &["image"], &[], 0).unwrap();
        assert_eq!(first[0].path, "a.png");
        let next = s.next_pending_where(5, &["image"], &[], first[0].id.0).unwrap();
        assert_eq!(kinds(next), ["d.png"]);
        assert!(s.has_pending_kind("pdf").unwrap() && !s.has_pending_kind("code").unwrap());
        // text hits carry the chunk locator, from the pending row first and from the vector later
        let f = s.upsert_file(&key(2), "b.pdf", 1, 4, "pdf").unwrap();
        let pend = [PendingVec { chunk: 3, kind: VecKind::Text, locator: Some("{\"page\":7}".into()) }];
        s.complete_text_only(f, "hp", &pend, &[(3, "needle in haystack".into())]).unwrap();
        let h = s.search_text(&fts_escape("needle"), 5).unwrap();
        assert_eq!(h[0].locator.as_deref(), Some("{\"page\":7}"));
        let v = ChunkVec { chunk: 3, kind: VecKind::Text, locator: pend[0].locator.clone(), vec: vec![1., 0., 0., 0.] };
        s.finish_vectors("hp", &[v]).unwrap();
        assert_eq!(s.search_text(&fts_escape("needle"), 5).unwrap()[0].locator.as_deref(), Some("{\"page\":7}"));
    }

    #[test]
    fn text_only_edge_cases() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, r"C:.pdf", 1);
        // nothing owed -> a normal done file
        s.complete_text_only(a, "h0", &[], &[(0, "plain".into())]).unwrap();
        assert_eq!((s.stats().unwrap().done, s.stats().unwrap().text_only), (1, 0));
        // modifying a text-only file sends it back to pending and gc forgets its pending rows
        let b = add(&mut s, 2, r"C:.pdf", 1);
        let pend = [PendingVec { chunk: 0, kind: VecKind::Text, locator: None }];
        s.complete_text_only(b, "h1", &pend, &[(0, "words".into())]).unwrap();
        assert_eq!(s.stats().unwrap().text_only, 1);
        s.upsert_file(&key(2), r"C:.pdf", 11, 5, "doc").unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.text_only, st.pending), (0, 1));
        s.gc().unwrap();
        assert!(s.pending_vecs("h1").unwrap().is_empty() && s.texts_of("h1").unwrap().is_empty());
        // reset (rebuild) drops pending rows too
        s.complete_text_only(b, "h2", &pend, &[(0, "again".into())]).unwrap();
        assert_eq!(s.stats().unwrap().text_only, 1);
        s.remove(&key(2)).unwrap();
        s.gc().unwrap();
        assert!(s.pending_vecs("h2").unwrap().is_empty());
        // wrong dimension is rejected
        let bad = ChunkVec { chunk: 0, kind: VecKind::Text, locator: None, vec: vec![1.0] };
        assert!(matches!(s.finish_vectors("h2", &[bad]), Err(Error::BadVector { .. })));
    }

    #[test]
    fn schema_v1_database_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("old.db");
        {
            let conn = Connection::open(&p).unwrap();
            conn.execute_batch("PRAGMA user_version = 0;").unwrap();
            migrate(&conn).unwrap();
            conn.execute_batch("DROP TABLE pending_vecs; PRAGMA user_version = 1;").unwrap();
        }
        let s = Store::open(&p, &meta()).unwrap();
        assert!(s.pending_vecs("x").unwrap().is_empty());
    }

    #[test]
    fn rename_keeps_vectors() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let id = add(&mut s, 1, r"C:\old\a.jpg", 1);
        s.complete(id, "h1", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        assert!(s.rename(&key(1), r"C:\new\b.jpg").unwrap());
        // upsert with only a path change is also free
        let id2 = s.upsert_file(&key(1), r"C:\newer\c.jpg", 10, 1, "image").unwrap();
        assert_eq!(id, id2);
        assert_eq!(s.stats().unwrap().done, 1);
        let hits = s.search_vectors(&[1., 0., 0., 0.], 5, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, r"C:\newer\c.jpg");
        assert!(!s.rename(&key(99), "x").unwrap());
    }

    #[test]
    fn hash_skip_shares_vectors() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, r"C:\a.jpg", 1);
        let b = add(&mut s, 2, r"C:\copy\a.jpg", 2);
        assert!(!s.has_content("h").unwrap());
        s.complete(a, "h", &[cv(0, [1., 0., 0., 0.])], &[(0, "hello".into())])
            .unwrap();
        assert!(s.has_content("h").unwrap());
        s.complete(b, "h", &[], &[]).unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.done, st.vectors), (2, 1));
        let hits = s.search_vectors(&[1., 0., 0., 0.], 10, None).unwrap();
        let mut paths: Vec<_> = hits.iter().map(|h| h.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, [r"C:\a.jpg", r"C:\copy\a.jpg"]);
        assert_eq!(s.search_text("hello", 10).unwrap().len(), 2);
        // removing one keeps the shared content; gc only drops it when both are gone
        s.remove(&key(1)).unwrap();
        assert_eq!(s.gc().unwrap(), 0);
        s.remove(&key(2)).unwrap();
        assert_eq!(s.gc().unwrap(), 1);
        assert_eq!(s.stats().unwrap().vectors, 0);
    }

    #[test]
    fn modified_file_goes_pending() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let id = add(&mut s, 1, r"C:\a.jpg", 1);
        s.complete(id, "h", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        assert!(s.next_pending(10).unwrap().is_empty());
        // unchanged upsert stays done
        add(&mut s, 1, r"C:\a.jpg", 1);
        assert_eq!(s.stats().unwrap().done, 1);
        add(&mut s, 1, r"C:\a.jpg", 2);
        let p = s.next_pending(10).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].id, id);
        // not searchable while pending
        assert!(s.search_vectors(&[1., 0., 0., 0.], 5, None).unwrap().is_empty());
        // failed/skipped are not pending
        s.fail(id, "boom").unwrap();
        assert!(s.next_pending(10).unwrap().is_empty());
        s.skip(id, "too big").unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.skipped, st.failed, st.pending), (1, 0, 0));
    }

    #[test]
    fn remove_under_boundary() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        add(&mut s, 1, r"C:\Users\a\x.jpg", 1);
        add(&mut s, 2, r"C:\Users\a\sub\y.jpg", 1);
        add(&mut s, 3, r"C:\Users\ab\z.jpg", 1);
        add(&mut s, 4, r"C:\Users\a", 1);
        assert_eq!(s.remove_under(r"c:\users\A\").unwrap(), 3);
        let left = s.next_pending(10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].path, r"C:\Users\ab\z.jpg");
        // LIKE wildcards in the prefix are literal
        add(&mut s, 5, r"C:\100%\q.jpg", 1);
        add(&mut s, 6, r"C:\100x\q.jpg", 1);
        assert_eq!(s.remove_under(r"C:\100%").unwrap(), 1);
    }

    #[test]
    fn rename_and_remove_by_path() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        add(&mut s, 1, r"C:\Pics\a.jpg", 1);
        add(&mut s, 2, r"C:\Pics\b.jpg", 1);
        assert!(s.rename_path(r"c:\pics\A.JPG", r"C:\Pics\sub\a.jpg").unwrap());
        assert!(!s.rename_path(r"C:\nope.jpg", r"C:\x.jpg").unwrap());
        let p: Vec<_> = s.next_pending(10).unwrap().into_iter().map(|f| f.path).collect();
        assert!(p.contains(&r"C:\Pics\sub\a.jpg".to_string()));
        assert!(s.remove_path(r"C:/PICS/b.jpg").unwrap());
        assert!(!s.remove_path(r"C:\Pics\b.jpg").unwrap());
        assert_eq!(s.stats().unwrap().files, 1);
    }

    #[test]
    fn rename_under_boundary_and_case() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        add(&mut s, 1, r"C:\Pics\a.jpg", 3);
        add(&mut s, 2, r"C:\Pics\deep\b.jpg", 2);
        add(&mut s, 3, r"C:\Pics2\c.jpg", 1);
        add(&mut s, 4, r"C:\other\d.jpg", 0);
        assert_eq!(s.rename_under(r"c:\pics\", r"D:\Moved\Pics").unwrap(), 2);
        let mut p: Vec<_> = s.next_pending(10).unwrap().into_iter().map(|f| f.path).collect();
        p.sort();
        assert_eq!(
            p,
            [
                r"C:\Pics2\c.jpg",
                r"C:\other\d.jpg",
                r"D:\Moved\Pics\a.jpg",
                r"D:\Moved\Pics\deep\b.jpg"
            ]
        );
        // LIKE metacharacters in the prefix are literal
        add(&mut s, 5, r"C:\100%_x\e.jpg", 0);
        add(&mut s, 6, r"C:\100Zax\f.jpg", 0);
        assert_eq!(s.rename_under(r"C:\100%_x", r"C:\y").unwrap(), 1);
    }

    #[test]
    fn calibrated_search_corrects_hubs_and_splits_modalities() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, "a", 1);
        let b = add(&mut s, 2, "b", 1);
        let t = add(&mut s, 3, "t", 1);
        // a: on topic; b: a hub (close to the hub direction, so close to everything)
        s.complete(a, "ha", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        s.complete(b, "hb", &[cv(0, [0.8, 0.6, 0., 0.])], &[]).unwrap();
        let text = ChunkVec { kind: VecKind::Text, ..cv(1, [0.6, 0., 0.8, 0.]) };
        s.complete(t, "ht", &[text], &[]).unwrap();
        let hub = [0., 1., 0., 0.];
        let c = s.search_calibrated(&[0.8, 0.6, 0., 0.], &hub, 10).unwrap();
        assert_eq!(c.len(), 3);
        let visual: Vec<&str> = c.iter().filter(|c| c.modality == Modality::Visual).map(|c| c.path.as_str()).collect();
        assert_eq!(visual, ["a", "b"], "raw score favours the hub b, adj favours a");
        let ca = c.iter().find(|c| c.path == "a").unwrap();
        assert!((ca.adj - 0.8).abs() < 1e-5 && (ca.score - 0.8).abs() < 1e-5);
        assert!(ca.z.is_none(), "two files are no population");
        let ct = c.iter().find(|c| c.path == "t").unwrap();
        assert_eq!((ct.modality, ct.chunk), (Modality::Text, 1));
        assert!(matches!(s.search_calibrated(&[1., 0.], &hub, 5), Err(Error::BadVector { .. })));
    }

    #[test]
    fn calibrated_multi_ranks_by_the_best_phrase_and_skips_contents() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, "a", 1);
        let b = add(&mut s, 2, "b", 1);
        let c = add(&mut s, 3, "c", 1);
        s.complete(a, "ha", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap(); // matches phrase 1 only
        s.complete(b, "hb", &[cv(0, [0., 0., 1., 0.])], &[]).unwrap(); // matches phrase 2 only
        s.complete(c, "hc", &[cv(0, [0., 0., 0., 1.])], &[]).unwrap(); // matches neither
        let hub = [0., 1., 0., 0.];
        let (q1, q2) = ([1., 0., 0., 0.], [0., 0., 1., 0.]);
        let none = HashSet::new();
        let one = s.search_calibrated_multi(&[&q1], &hub, 10, &none).unwrap();
        assert_eq!(one.iter().find(|x| x.path == "b").unwrap().adj, 0.0, "one phrase misses b");
        let both = s.search_calibrated_multi(&[&q1, &q2], &hub, 10, &none).unwrap();
        let adj = |p: &str| both.iter().find(|x| x.path == p).unwrap().adj;
        assert_eq!((adj("a"), adj("b"), adj("c")), (1.0, 1.0, 0.0), "each content is scored under its best phrase");
        assert_eq!(both.iter().find(|x| x.path == "a").unwrap().hash, "ha");
        let skip: HashSet<String> = ["ha".to_string()].into();
        let rest = s.search_calibrated_multi(&[&q1, &q2], &hub, 10, &skip).unwrap();
        assert!(rest.iter().all(|x| x.path != "a") && rest.len() == 2, "skipped contents are not candidates");
        assert!(s.search_calibrated_multi(&[], &hub, 10, &none).unwrap().is_empty());
    }

    #[test]
    fn image_dims_roundtrip_and_replace() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        assert!(s.image_dims().unwrap().is_empty());
        s.set_image_dims(&[("h1".into(), 640, 480), ("h2".into(), 0, 0)]).unwrap();
        s.set_image_dims(&[("h1".into(), 800, 600)]).unwrap();
        let d = s.image_dims().unwrap();
        assert_eq!((d["h1"], d["h2"], d.len()), ((800, 600), (0, 0), 2));
    }

    #[test]
    fn pending_newest_first() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        add(&mut s, 1, "a", 100);
        add(&mut s, 2, "b", 300);
        add(&mut s, 3, "c", 200);
        let p: Vec<_> = s.next_pending(10).unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(p, ["b", "c", "a"]);
        assert_eq!(s.next_pending(2).unwrap().len(), 2);
    }

    #[test]
    fn vector_search_ranks_and_dedups() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, "a", 1);
        let b = add(&mut s, 2, "b", 1);
        let c = add(&mut s, 3, "c", 1);
        // a: two chunks, one close to the query
        s.complete(a, "ha", &[cv(0, [0., 1., 0., 0.]), cv(1, [0.8, 0.6, 0., 0.])], &[])
            .unwrap();
        s.complete(b, "hb", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        s.complete(c, "hc", &[cv(0, [0., 0., 1., 0.])], &[]).unwrap();
        let hits = s.search_vectors(&[1., 0., 0., 0.], 10, None).unwrap();
        assert_eq!(hits.len(), 3, "one hit per file");
        assert_eq!(hits[0].path, "b");
        assert_eq!(hits[1].path, "a");
        assert_eq!(hits[1].chunk, 1);
        assert!(hits[1].locator.as_deref().unwrap().contains("1"));
        assert!(hits[0].score > hits[1].score && hits[1].score > hits[2].score);
        assert_eq!(s.search_vectors(&[1., 0., 0., 0.], 1, None).unwrap().len(), 1);
        assert!(s
            .search_vectors(&[1., 0., 0., 0.], 5, Some(VecKind::Text))
            .unwrap()
            .is_empty());
        assert!(matches!(
            s.search_vectors(&[1., 0.], 5, None),
            Err(Error::BadVector { .. })
        ));
        assert!(matches!(
            s.complete(a, "x", &[ChunkVec { vec: vec![1.0], ..cv(0, [0.; 4]) }], &[]),
            Err(Error::BadVector { .. })
        ));
    }

    #[test]
    fn fts_diacritics_and_dedup() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, "a.pdf", 1);
        let b = add(&mut s, 2, "b.pdf", 1);
        s.complete(
            a,
            "ha",
            &[],
            &[
                (0, "Le café de la gare est fermé".into()),
                (1, "un autre café très bon".into()),
            ],
        )
        .unwrap();
        s.complete(b, "hb", &[], &[(0, "tea only".into())]).unwrap();
        let hits = s.search_text("cafe", 10).unwrap();
        assert_eq!(hits.len(), 1, "chunks dedup to one hit per file");
        assert_eq!(hits[0].path, "a.pdf");
        assert!(hits[0].snippet.contains("[caf"));
        assert_eq!(s.search_text("FERME", 10).unwrap().len(), 1);
        assert!(s.search_text("zzz", 10).unwrap().is_empty());
        assert_eq!(s.search_text(&fts_escape("tea \"only"), 10).unwrap().len(), 1);
    }

    #[test]
    fn scopes_matching() {
        let sc = Scopes {
            include: vec![r"C:\Users\a".into(), "D:/Photos/".into()],
            exclude: vec![r"C:\Users\a\Private".into()],
            ..Default::default()
        };
        assert!(in_scope(&sc, r"C:\Users\a"));
        assert!(in_scope(&sc, r"c:\users\A\Docs\x.pdf"));
        assert!(!in_scope(&sc, r"C:\Users\ab\x"), "boundary");
        assert!(!in_scope(&sc, r"C:\Users\a\private\x"), "exclude wins");
        assert!(!in_scope(&sc, r"C:\Users\a\Private"));
        assert!(in_scope(&sc, r"C:\Users\a\PrivateStuff\x"));
        assert!(!in_scope(&sc, r"C:\Users\a\AppData\Local\x"), "default segment");
        assert!(!in_scope(&sc, r"C:\Users\a\proj\node_modules\x"));
        assert!(!in_scope(&sc, r"C:\Users\a\proj\.git\config"));
        assert!(in_scope(&sc, r"C:\Users\a\gitstuff\x"));
        assert!(in_scope(&sc, r"d:\photos\2025\x.jpg"));
        assert!(!in_scope(&Scopes::default(), r"C:\x"));
        let root = Scopes {
            include: vec![r"C:\".into()],
            exclude: vec![],
            ..Default::default()
        };
        assert!(in_scope(&root, r"C:\anything\x"));
        assert!(!in_scope(&root, r"D:\x"));
    }

    #[test]
    fn hard_deny_beats_any_user_ignore_list_and_include_root() {
        let none = Ignore::new::<&str>(&[]); // user cleared the whole ignore list
        let sc = Scopes { include: vec![r"C:\u\Documents".into(), r"C:\u\.ssh".into()], exclude: vec![], ..Default::default() };
        for p in [
            r"C:\u\Documents\.ssh\id_rsa",
            r"C:\u\Documents\keys\server.pem",
            r"C:\u\Documents\Vault.KDBX",
            r"C:\u\Documents\app\.env.production",
            r"C:\u\Documents\.aws\credentials",
            r"C:\u\Documents\id_ed25519.pub",
            r"C:\u\Documents\x\__pycache__\a.png",
            r"C:\u\.ssh\notes.txt", // the include root itself is a denied folder
        ] {
            assert!(!in_scope_with(&sc, &none, p), "{p}");
            assert!(hard_denied(p), "{p}");
        }
        assert!(in_scope_with(&sc, &none, r"C:\u\Documents\Taxes\2024.pdf"));
        assert!(!hard_denied(r"C:\u\Documents\envelope.docx"));
    }

    #[test]
    fn skips_for_a_temporary_cause_can_be_requeued() {
        let meta = IndexMeta { model_id: "t".into(), llama_build: "b".into(), quant: "q".into(), image_budget: 280, dims: 4 };
        let mut s = Store::open_in_memory(&meta).unwrap();
        let k = |n: u64| FileKey { vol_guid: "v".into(), frn: n };
        let a = s.upsert_file(&k(1), r"C:\p\a.jpg", 10, 1, "image").unwrap();
        let b = s.upsert_file(&k(2), r"C:\p\b.jpg", 10, 1, "image").unwrap();
        s.skip(a, "cloud_only").unwrap();
        s.skip(b, "too_large").unwrap();
        assert_eq!(s.requeue_skipped(&["cloud_only", "no_heic_codec"]).unwrap(), 1);
        let pending: Vec<String> = s.next_pending(10).unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(pending, [r"C:\p\a.jpg"]);
        assert_eq!(s.all_file_paths().unwrap().len(), 2);
    }

    #[test]
    fn code_files_are_searchable_only_through_search_code() {
        let meta = IndexMeta { model_id: "t".into(), llama_build: "b".into(), quant: "q".into(), image_budget: 280, dims: 4 };
        let mut s = Store::open_in_memory(&meta).unwrap();
        let k = |n: u64| FileKey { vol_guid: "v".into(), frn: n };
        let code = s.upsert_file(&k(1), r"C:\p\main.rs", 10, 1, "code").unwrap();
        let doc = s.upsert_file(&k(2), r"C:\p\notes.txt", 10, 1, "doc").unwrap();
        s.complete(code, "h1", &[], &[(0, "frobnicate the widget".into())]).unwrap();
        s.complete(doc, "h2", &[], &[(0, "meeting about the widget".into())]).unwrap();
        let general = s.search_text("widget", 10).unwrap();
        assert_eq!(general.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), [r"C:\p\notes.txt"]);
        assert!(s.search_text("frobnicate", 10).unwrap().is_empty(), "code never mixes into general results");
        let c = s.search_code("widget", 10).unwrap();
        assert_eq!(c.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), [r"C:\p\main.rs"]);
    }

    #[test]
    fn default_ignore_covers_dev_junk() {
        let sc = Scopes { include: vec![r"C:\u\Documents".into()], exclude: vec![], ..Default::default() };
        for junk in [
            r"proj\node_modules\x.png",
            r"proj\.venv\Lib\x.png",
            r"proj\venv\x.png",
            r"proj\__pycache__\x.png",
            r"proj\pkg.egg-info\x.png",
            r"crate\target\debug\x.png",
            r"app\build\x.png",
            r"app\.gradle\x.png",
            r"java\.m2\repository\x.png",
            r"net\bin\Debug\x.png",
            r"net\obj\x.png",
            r"c\cmake-build-debug\x.png",
            r"b\bazel-out\x.png",
            r"go\vendor\x.png",
            r"ios\Pods\x.png",
            r"web\.next\x.png",
            r"Vault.KDBX",
            r"a\.git\x.png",
            r"a\Thumbs.db",
        ] {
            assert!(!in_scope(&sc, &format!(r"C:\u\Documents\{junk}")), "{junk}");
        }
        assert!(in_scope(&sc, r"C:\u\Documents\Targets 2025\x.png"), "whole names only");
        assert!(in_scope(&sc, r"C:\u\Documents\builder\x.png"));
        // the include root's own segments are exempt
        let root = Scopes { include: vec![r"D:\build\photos".into()], exclude: vec![], ..Default::default() };
        assert!(in_scope(&root, r"D:\build\photos\x.jpg"));
        assert!(!in_scope(&root, r"D:\build\photos\out\x.jpg"));
        // a projects library: sources in, build output and environments out
        let lib = Scopes { include: vec![r"D:\Projects".into()], exclude: vec![], ..Default::default() };
        assert!(in_scope(&lib, r"D:\Projects\winseek\docs\shot.png"));
        assert!(!in_scope(&lib, r"D:\Projects\winseek\target\debug\x.png"));
        assert!(!in_scope(&lib, r"D:\Projects\winseek\app\ui\node_modules\x.png"));
        assert!(!in_scope(&lib, r"D:\Projects\photo\.venv\Lib\x.png"));
    }

    #[test]
    fn custom_ignore_and_globs() {
        let sc = Scopes { include: vec![r"C:\p".into()], exclude: vec![], ..Default::default() };
        let ig = Ignore::new(&["Raw*", "?tmp", " /Secret\\ ", ""]);
        assert!(!in_scope_with(&sc, &ig, r"C:\p\RAWs\x.jpg"));
        assert!(!in_scope_with(&sc, &ig, r"C:\p\xtmp\x.jpg"));
        assert!(in_scope_with(&sc, &ig, r"C:\p\tmp\x.jpg"), "? needs one char");
        assert!(!in_scope_with(&sc, &ig, r"C:\p\secret\x.jpg"));
        assert!(in_scope_with(&sc, &ig, r"C:\p\node_modules\x.jpg"), "custom list replaces defaults");
        assert!(in_scope_with(&sc, &Ignore::new::<&str>(&[]), r"C:\p\.git\x"));
        assert!(glob_match(&"a*b*c".chars().collect::<Vec<_>>(), &"axxbyyc".chars().collect::<Vec<_>>()));
        assert!(!glob_match(&"a*b".chars().collect::<Vec<_>>(), &"axxbc".chars().collect::<Vec<_>>()));
    }

    #[test]
    fn ignore_persists() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        assert_eq!(s.get_ignore().unwrap(), default_ignore());
        s.set_ignore(Some(&[" build ".into(), "".into(), "Build".into(), "x*".into()])).unwrap();
        assert_eq!(s.get_ignore().unwrap(), vec!["build".to_string(), "x*".into()]);
        s.set_ignore(Some(&[])).unwrap();
        assert!(s.get_ignore().unwrap().is_empty(), "an empty list is a real choice");
        s.set_ignore(None).unwrap();
        assert_eq!(s.get_ignore().unwrap(), default_ignore());
    }

    #[test]
    fn scopes_persist() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        assert_eq!(s.get_scopes().unwrap(), Scopes::default());
        let sc = Scopes {
            include: vec!["C:\\a".into()],
            exclude: vec!["C:\\a\\b".into()],
            configured: true,
        };
        s.set_scopes(&sc).unwrap();
        s.set_scopes(&sc).unwrap();
        assert_eq!(s.get_scopes().unwrap(), sc, "including the configured marker");
        // An old store row without the field reads as never-configured.
        let mut s = Store::open_in_memory(&meta()).unwrap();
        s.set_setting("scopes", Some(r#"{"include":["C:\\a"],"exclude":[]}"#)).unwrap();
        assert_eq!(s.get_scopes().unwrap(), Scopes { include: vec!["C:\\a".into()], exclude: vec![], configured: false });
    }

    #[test]
    fn stats_counts() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, "a", 1);
        let b = add(&mut s, 2, "b", 1);
        add(&mut s, 3, "c", 1);
        add(&mut s, 4, "d", 1);
        s.complete(a, "ha", &[cv(0, [1., 0., 0., 0.]), cv(1, [0., 1., 0., 0.])], &[])
            .unwrap();
        s.fail(b, "bad").unwrap();
        let st = s.stats().unwrap();
        assert_eq!(
            st,
            Stats {
                files: 4,
                pending: 2,
                done: 1,
                failed: 1,
                skipped: 0,
                vectors: 2,
                text_only: 0
            }
        );
    }

    #[test]
    fn schema_too_new() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        Connection::open(&p)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        assert!(matches!(Store::open(&p, &meta()), Err(Error::SchemaTooNew(99))));
    }

    #[test]
    fn library_listings_and_dup_groups() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, r"C:\p\a.jpg", 300);
        let b = add(&mut s, 2, r"C:\p\copy of a.jpg", 100);
        let c = add(&mut s, 3, r"C:\p\c.png", 200);
        let d = s.upsert_file(&key(4), r"C:\p\notes.pdf", 10, 400, "pdf").unwrap();
        add(&mut s, 5, r"C:\p\pending.jpg", 500);
        s.complete(a, "ha", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        s.complete(b, "ha", &[], &[]).unwrap(); // same content as a
        s.complete(c, "hc", &[cv(0, [0., 1., 0., 0.])], &[]).unwrap();
        s.complete(d, "hd", &[cv(0, [0., 0., 1., 0.])], &[]).unwrap();

        let imgs = s.list_images().unwrap();
        let paths: Vec<_> = imgs.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, vec![r"C:\p\a.jpg", r"C:\p\c.png", r"C:\p\copy of a.jpg"], "done images only, newest first");
        assert_eq!((imgs[0].size, imgs[0].mtime, imgs[0].content_hash.as_str()), (10, 300, "ha"));

        let groups = s.dup_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].hash, "ha");
        assert_eq!(groups[0].files.iter().map(|f| f.mtime).collect::<Vec<_>>(), vec![300, 100]);
        assert_eq!(groups[0].reclaimable(), 10);
    }

    #[test]
    fn dup_groups_sorted_by_reclaimable() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let mk = |s: &mut Store, n: u64, size: u64, hash: &str| {
            let id = s.upsert_file(&key(n), &format!("C:\\x\\{n}.jpg"), size, n as i64, "image").unwrap();
            s.complete(id, hash, &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        };
        mk(&mut s, 1, 5, "small");
        mk(&mut s, 2, 5, "small");
        mk(&mut s, 3, 100, "big");
        mk(&mut s, 4, 100, "big");
        mk(&mut s, 5, 100, "big");
        let g = s.dup_groups().unwrap();
        assert_eq!(g.iter().map(|g| (g.hash.as_str(), g.reclaimable())).collect::<Vec<_>>(), vec![("big", 200), ("small", 5)]);
    }

    #[test]
    fn images_with_vectors_filters_by_time_and_decodes() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, r"C:\p\a.jpg", 100);
        let b = add(&mut s, 2, r"C:\p\b.jpg", 200);
        let c = add(&mut s, 3, r"C:\p\c.jpg", 300);
        s.complete(a, "ha", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        s.complete(b, "hb", &[cv(0, [0., 1., 0., 0.]), cv(1, [0., 0., 1., 0.])], &[]).unwrap();
        s.complete(c, "hc", &[cv(0, [0., 0., 0., 1.])], &[]).unwrap();
        let r = s.images_with_vectors(100, 300).unwrap();
        assert_eq!(r.len(), 2, "[from, to) range");
        assert_eq!(r[0].0.path, r"C:\p\b.jpg");
        assert_eq!(r[0].1, vec![0., 1., 0., 0.], "chunk 0 only");
        assert_eq!(r[1].1, vec![1., 0., 0., 0.]);
    }

    #[test]
    fn cluster_near_is_transitive_and_thresholded() {
        let a = [1.0f32, 0.0];
        let b = [0.96f32, 0.28]; // cos(a,b) = 0.96
        let c = [0.848f32, 0.53]; // cos(b,c) ~ 0.96, cos(a,c) ~ 0.85
        let d = [0.0f32, 1.0];
        let v: Vec<&[f32]> = vec![&a, &b, &c, &d];
        assert_eq!(cluster_near(&v, 0.95), vec![vec![0, 1, 2]], "single linkage joins a-b-c");
        assert!(cluster_near(&v, 0.99).is_empty());
        assert_eq!(cluster_near(&v, 0.5), vec![vec![0, 1, 2, 3]]);
        assert!(cluster_near(&[], 0.9).is_empty());
    }

    #[test]
    fn index_code_flag_and_remove_kind() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        assert!(!s.get_index_code().unwrap());
        s.set_index_code(true).unwrap();
        assert!(s.get_index_code().unwrap());
        s.set_index_code(false).unwrap();
        assert!(!s.get_index_code().unwrap());

        let a = s.upsert_file(&key(1), r"C:\p\a.rs", 10, 1, "code").unwrap();
        let b = s.upsert_file(&key(2), r"C:\p\b.jpg", 10, 1, "image").unwrap();
        s.complete(a, "hc", &[], &[(0, "fn main".into())]).unwrap();
        s.complete(b, "hi", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        assert_eq!(s.remove_kind("code").unwrap(), 1);
        assert_eq!(s.stats().unwrap().files, 1);
        assert!(!s.has_content("hc").unwrap(), "orphaned content collected");
        assert!(s.has_content("hi").unwrap());
    }

    #[test]
    fn dup_groups_only_offer_images() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        for (n, p, k) in [(1, r"C:\a.txt", "doc"), (2, r"C:\b.txt", "doc"), (3, r"C:\c.jpg", "image"), (4, r"C:\d.jpg", "image")] {
            let id = s.upsert_file(&key(n), p, 10, 1, k).unwrap();
            s.complete(id, if k == "doc" { "hd" } else { "hj" }, &[], &[]).unwrap();
        }
        let g = s.dup_groups().unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].hash, "hj");
    }

    // ---- parse queue --------------------------------------------------------------

    fn tv(chunk: u32, page: u32, v: [f32; 4]) -> ChunkVec {
        ChunkVec { chunk, kind: VecKind::Text, locator: Some(format!("{{\"page\":{page}}}")), vec: v.to_vec() }
    }

    fn iv(chunk: u32, page: u32, v: [f32; 4]) -> ChunkVec {
        ChunkVec { chunk, kind: VecKind::Image, locator: Some(format!("{{\"page\":{page}}}")), vec: v.to_vec() }
    }

    fn pc(text: &str, page: u32, v: Option<[f32; 4]>) -> ParseChunk {
        ParseChunk { text: text.into(), locator: format!("{{\"page\":{page},\"parser\":{{}}}}"), vec: v.map(|v| v.to_vec()) }
    }

    fn locator_of(s: &Store, hash: &str, chunk: u32) -> Option<String> {
        s.conn
            .query_row("SELECT locator FROM vectors WHERE hash = ?1 AND chunk = ?2", params![hash, chunk], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn schema_v2_database_gets_parse_queue() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v2.db");
        {
            let conn = Connection::open(&p).unwrap();
            conn.execute_batch("PRAGMA user_version = 0;").unwrap();
            migrate(&conn).unwrap();
            conn.execute_batch("DROP TABLE parse_queue; PRAGMA user_version = 2;").unwrap();
        }
        let mut s = Store::open(&p, &meta()).unwrap();
        assert!(s.queue_parse("h", 0, "screenshot").unwrap());
        assert_eq!(s.parse_counts().unwrap(), ParseCounts { queued: 1, done: 0, failed: 0, skipped: 0 });
        let v: i64 = s.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn parse_queue_lifecycle() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let old = add(&mut s, 1, r"C:\p\old.png", 100);
        let new = add(&mut s, 2, r"C:\p\new.png", 300);
        let dead = add(&mut s, 3, r"C:\p\dead.png", 500);
        s.complete(old, "ho", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        s.complete(new, "hn", &[cv(0, [0., 1., 0., 0.])], &[]).unwrap();
        s.complete(dead, "hd", &[cv(0, [0., 0., 1., 0.])], &[]).unwrap();
        assert!(s.queue_parse("ho", 0, "doc_image").unwrap());
        assert!(s.queue_parse("hn", 0, "screenshot").unwrap());
        assert!(s.queue_parse("hd", 0, "doc_image").unwrap());
        assert!(!s.queue_parse("hn", 0, "doc_image").unwrap(), "INSERT OR IGNORE");
        s.remove(&key(3)).unwrap();
        s.queue_parse("hn", 2, "scanned_pdf").unwrap();

        let jobs = s.next_parse(10).unwrap();
        let got: Vec<_> = jobs.iter().map(|j| (j.hash.as_str(), j.page, j.reason.as_str(), j.path.as_str())).collect();
        assert_eq!(
            got,
            vec![("hn", 0, "screenshot", r"C:\p\new.png"), ("hn", 2, "scanned_pdf", r"C:\p\new.png"), ("ho", 0, "doc_image", r"C:\p\old.png")],
            "newest file first, dead content left out"
        );
        assert_eq!(s.next_parse(1).unwrap().len(), 1);
        assert_eq!(s.live_path("hn").unwrap().as_deref(), Some(r"C:\p\new.png"));
        assert_eq!(s.live_path("hd").unwrap(), None);

        assert_eq!(s.bump_parse("ho", 0, "boom").unwrap(), 1);
        assert_eq!(s.bump_parse("ho", 0, "boom").unwrap(), 2);
        assert_eq!(s.next_parse(10).unwrap().iter().find(|j| j.hash == "ho").unwrap().attempts, 2);
        s.fail_parse("ho", 0, "boom").unwrap();
        s.skip_parse("hn", 2, "empty").unwrap();
        s.finish_parse("hn", 0, &ParseResult { chunks: vec![], locator: "{}".into() }).unwrap();
        assert_eq!(s.parse_counts().unwrap(), ParseCounts { queued: 1, done: 1, failed: 1, skipped: 1 }, "only the dead one is still queued");
        assert!(s.next_parse(10).unwrap().is_empty());
    }

    #[test]
    fn queued_all_lists_waiting_items_only() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        assert!(s.queued_all().unwrap().is_empty());
        s.queue_parse("ha", 0, "doc_image").unwrap();
        s.queue_parse("hb", 3, "scanned_pdf").unwrap();
        s.queue_parse("hb", 0, "doc_image").unwrap();
        s.skip_parse("hb", 3, "ocr_ok").unwrap();
        s.fail_parse("hb", 0, "boom").unwrap();
        s.queue_parse("hc", 0, "doc_image").unwrap();
        s.finish_parse("hc", 0, &ParseResult { chunks: vec![], locator: "{}".into() }).unwrap();
        let mut got = s.queued_all().unwrap();
        got.sort();
        assert_eq!(got, vec![("ha".into(), 0)]);
    }

    #[test]
    fn page_text_takes_the_chunks_of_that_page() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let id = s.upsert_file(&key(1), r"C:\p\scan.pdf", 10, 1, "pdf").unwrap();
        // page 1: OCR text with its vector; page 2: text whose vector is still owed
        let v = |chunk, page: u32, vec| ChunkVec { chunk, kind: VecKind::Text, locator: Some(format!(r#"{{"page":{page}}}"#)), vec };
        s.complete(id, "hs", &[v(1, 1, cvv()), v(3, 2, cvv())], &[(1, "page one words".into()), (3, "page two words".into())]).unwrap();
        assert_eq!(s.page_text("hs", 1).unwrap(), "page one words");
        assert_eq!(s.page_text("hs", 2).unwrap(), "page two words", "owed vector's locator counts");
        assert_eq!(s.page_text("hs", 0).unwrap(), "", "no standalone chunks");
        assert_eq!(s.page_text("hs", 7).unwrap(), "");
        assert_eq!(s.page_text("missing", 1).unwrap(), "");
        // a text-only file: the vector is owed, its locator lives in pending_vecs
        let oid = s.upsert_file(&key(3), r"C:\p\notes.pdf", 10, 1, "pdf").unwrap();
        let pend = [PendingVec { chunk: 5, kind: VecKind::Text, locator: Some(r#"{"page":7}"#.into()) }];
        s.complete_text_only(oid, "hn", &pend, &[(5, "page seven".into())]).unwrap();
        assert_eq!(s.page_text("hn", 7).unwrap(), "page seven", "pending_vecs fallback");
        // a parsed page-0 image: its chunks carry no page key, so page 0 finds them
        let img = s.upsert_file(&key(2), r"C:\p\shot.png", 10, 1, "image").unwrap();
        let noloc = ChunkVec { chunk: 0, kind: VecKind::Text, locator: Some(r#"{"ocr":{"w":1}}"#.into()), vec: cvv() };
        s.complete(img, "hi", &[noloc], &[(0, "standalone ocr text".into())]).unwrap();
        assert_eq!(s.page_text("hi", 0).unwrap(), "standalone ocr text");
        assert_eq!(s.page_text("hi", 1).unwrap(), "");
    }

    fn cvv() -> Vec<f32> {
        vec![1., 0., 0., 0.]
    }

    #[test]
    fn finish_parse_replaces_only_that_pages_text_and_rewrites_locator() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let f = s.upsert_file(&key(1), r"C:\p\scan.pdf", 10, 1, "pdf").unwrap();
        // pages 1 and 2 are scanned: image vector, then OCR text; chunk numbers run across pages
        let vecs = [
            iv(0, 1, [1., 0., 0., 0.]),
            tv(1, 1, [0., 1., 0., 0.]),
            iv(2, 2, [0., 0., 1., 0.]),
            tv(3, 2, [0., 0., 0., 1.]),
            tv(4, 2, [1., 1., 0., 0.]),
        ];
        let texts: Vec<(u32, String)> = vec![(1, "p1 ocr".into()), (3, "p2 ocr a".into()), (4, "p2 ocr b".into())];
        s.complete(f, "h", &vecs, &texts).unwrap();

        let res = ParseResult {
            chunks: vec![pc("p2 parsed one", 2, Some([0., 0., 1., 1.])), pc("p2 parsed two", 2, Some([1., 0., 1., 0.]))],
            locator: r#"{"page":2,"parser":{"v":"x"}}"#.into(),
        };
        s.queue_parse("h", 2, "scanned_pdf").unwrap();
        s.finish_parse("h", 2, &res).unwrap();

        let t = s.texts_of("h").unwrap();
        assert_eq!(
            t,
            vec![(1, "p1 ocr".into()), (5, "p2 parsed one".into()), (6, "p2 parsed two".into())],
            "page 1 untouched, new numbers from MAX+1"
        );
        assert_eq!(locator_of(&s, "h", 2).as_deref(), Some(r#"{"page":2,"parser":{"v":"x"}}"#), "page image locator rewritten");
        assert_eq!(locator_of(&s, "h", 0).as_deref(), Some(r#"{"page":1}"#), "page 1 image untouched");
        assert_eq!(locator_of(&s, "h", 1).as_deref(), Some(r#"{"page":1}"#));
        assert_eq!(locator_of(&s, "h", 5).as_deref(), Some(r#"{"page":2,"parser":{}}"#));
        assert!(s.search_text("ocr", 5).unwrap().len() == 1, "only the page 1 text still says ocr");
        let hits = s.search_vectors(&[0., 0., 1., 1.], 3, Some(VecKind::Text)).unwrap();
        assert_eq!(hits[0].chunk, 5, "the new text vectors are searchable");
        assert!(s.search_text("parsed", 5).unwrap().iter().all(|h| h.chunk >= 5));
        assert_eq!(s.stats().unwrap().done, 1);
        assert_eq!(s.parse_counts().unwrap().done, 1);

        // re-parsing replaces the previous parse again
        let again = ParseResult { chunks: vec![pc("p2 third", 2, Some([1., 1., 1., 1.]))], locator: res.locator.clone() };
        s.finish_parse("h", 2, &again).unwrap();
        assert_eq!(s.texts_of("h").unwrap(), vec![(1, "p1 ocr".into()), (7, "p2 third".into())]);
    }

    #[test]
    fn finish_parse_standalone_image_and_pending_vectors() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let f = add(&mut s, 1, r"C:\p\shot.png", 1);
        let ocr_loc = r#"{"ocr":{"w":10,"h":10,"lines":[]}}"#;
        let vecs = [
            ChunkVec { chunk: 0, kind: VecKind::Image, locator: Some(ocr_loc.into()), vec: vec![1., 0., 0., 0.] },
            ChunkVec { chunk: 1, kind: VecKind::Text, locator: Some(ocr_loc.into()), vec: vec![0., 1., 0., 0.] },
        ];
        s.complete(f, "h", &vecs, &[(1, "short ocr".into())]).unwrap();
        let res = ParseResult {
            chunks: vec![ParseChunk { text: "parsed".into(), locator: "{\"parser\":{}}".into(), vec: None }],
            locator: "{\"parser\":{\"v\":\"x\"}}".into(),
        };
        s.finish_parse("h", 0, &res).unwrap();
        assert_eq!(s.texts_of("h").unwrap(), vec![(2, "parsed".to_string())]);
        assert_eq!(locator_of(&s, "h", 0).as_deref(), Some("{\"parser\":{\"v\":\"x\"}}"));
        // the text vector is owed: the file waits as text_only and the upgrade pass can find it
        let pend = s.pending_vecs("h").unwrap();
        assert_eq!(pend, vec![PendingVec { chunk: 2, kind: VecKind::Text, locator: Some("{\"parser\":{}}".into()) }]);
        assert_eq!(s.stats().unwrap().text_only, 1);
        assert_eq!(s.next_text_only(5).unwrap().len(), 1);
        s.finish_vectors("h", &[ChunkVec { chunk: 2, kind: VecKind::Text, locator: Some("{}".into()), vec: vec![0., 0., 1., 0.] }]).unwrap();
        assert_eq!(s.stats().unwrap().done, 1);
        // wrong dimension is rejected before anything changes
        let bad = ParseResult {
            chunks: vec![ParseChunk { text: "x".into(), locator: "{}".into(), vec: Some(vec![1.0]) }],
            locator: "{}".into(),
        };
        assert!(matches!(s.finish_parse("h", 0, &bad), Err(Error::BadVector { .. })));
        assert_eq!(s.texts_of("h").unwrap().len(), 1);
    }

    #[test]
    fn skip_parse_replacing_stores_ocr_text() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let f = add(&mut s, 1, r"C:\p\doc.jpg", 1);
        s.complete(f, "h", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        s.queue_parse("h", 0, "doc_image").unwrap();
        let res = ParseResult {
            chunks: vec![ParseChunk { text: "arabic text".into(), locator: "{\"ocr\":{}}".into(), vec: Some(vec![0., 1., 0., 0.]) }],
            locator: "{\"ocr\":{}}".into(),
        };
        s.skip_parse_replacing("h", 0, "script", &res).unwrap();
        assert_eq!(s.texts_of("h").unwrap(), vec![(1, "arabic text".to_string())]);
        assert_eq!(s.search_text("arabic", 3).unwrap().len(), 1);
        let (state, err): (i64, String) =
            s.conn.query_row("SELECT state, error FROM parse_queue WHERE hash = 'h'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((state, err.as_str()), (STATE_SKIPPED, "script"));
        assert_eq!(s.parse_counts().unwrap(), ParseCounts { queued: 0, done: 0, failed: 0, skipped: 1 });
    }

    #[test]
    fn route_scan_skips_pdf_pages_and_follows_cursor() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        let a = add(&mut s, 1, r"C:\p\a.jpg", 1);
        let p = s.upsert_file(&key(2), r"C:\p\b.pdf", 10, 2, "pdf").unwrap();
        let c = add(&mut s, 3, r"C:\p\c.jpg", 3);
        s.complete(a, "ha", &[ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec: vec![1., 0., 0., 0.] }], &[]).unwrap();
        s.complete(p, "hp", &[iv(0, 1, [0., 1., 0., 0.]), tv(1, 1, [0., 0., 1., 0.])], &[(1, "t".into())]).unwrap();
        let ocr = r#"{"ocr":{"w":1,"h":1,"lines":[]}}"#;
        s.complete(c, "hc", &[ChunkVec { chunk: 0, kind: VecKind::Image, locator: Some(ocr.into()), vec: vec![0., 0., 0., 1.] }], &[]).unwrap();
        assert!(s.max_vector_rowid().unwrap() >= 4);
        let r = s.image_vectors_after(0, 10).unwrap();
        assert_eq!(r.iter().map(|x| x.hash.as_str()).collect::<Vec<_>>(), vec!["ha", "hc"], "page vectors are not standalone images");
        assert_eq!(r[1].vec, vec![0., 0., 0., 1.]);
        assert_eq!(s.image_vectors_after(r[0].rowid, 10).unwrap().len(), 1);
        assert_eq!(s.image_vectors_after(r[1].rowid, 10).unwrap().len(), 0);
        assert_eq!(s.image_vectors_after(0, 1).unwrap().len(), 1, "limit");
        s.set_setting(SETTING_PARSE_ROUTE_CURSOR, Some("7")).unwrap();
        assert_eq!(s.get_setting(SETTING_PARSE_ROUTE_CURSOR).unwrap().as_deref(), Some("7"));
    }

    #[test]
    fn gc_and_reset_drop_parse_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        let mut s = Store::open(&p, &meta()).unwrap();
        let a = add(&mut s, 1, r"C:\p\a.jpg", 1);
        s.complete(a, "ha", &[cv(0, [1., 0., 0., 0.])], &[]).unwrap();
        s.queue_parse("ha", 0, "doc_image").unwrap();
        s.queue_parse("ghost", 0, "doc_image").unwrap(); // content nothing references
        s.set_setting(SETTING_PARSE_ROUTE_CURSOR, Some("3")).unwrap();
        s.gc().unwrap();
        assert_eq!(s.parse_counts().unwrap().queued, 1, "only the live content keeps its row");
        s.remove(&key(1)).unwrap();
        s.gc().unwrap();
        assert_eq!(s.parse_counts().unwrap().queued, 0);
        s.queue_parse("ha", 0, "doc_image").unwrap();
        drop(s);
        let s = Store::reset(&p, &meta()).unwrap();
        assert_eq!(s.parse_counts().unwrap(), ParseCounts::default());
        assert_eq!(s.get_setting(SETTING_PARSE_ROUTE_CURSOR).unwrap(), None);
    }
}
