//! Tier 1 semantic store: one SQLite file holding file identity, content-hash keyed
//! vectors (brute-force cosine), an FTS5 text index and settings (folder scopes).
//!
//! Rows are keyed by `(volume GUID, FRN)`; vectors and texts are keyed by content hash,
//! so a rename costs nothing and identical content at two paths shares its vectors.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::fmt;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: i64 = 1;

const STATE_PENDING: i64 = 0;
const STATE_DONE: i64 = 1;
const STATE_FAILED: i64 = 2;
const STATE_SKIPPED: i64 = 3;

/// Path segments that are never indexed, regardless of user scopes.
pub const DEFAULT_EXCLUDED_SEGMENTS: &[&str] = &["appdata", "node_modules", ".git"];

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
}

impl VecKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VecKind::Image => "image",
            VecKind::Text => "text",
            VecKind::Page => "page",
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
}

/// Folder scopes. A path is in scope if it is under an `include` prefix, not under an
/// `exclude` prefix, and has no default-excluded segment. An empty `include` list
/// means nothing is in scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scopes {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
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

pub fn in_scope(scopes: &Scopes, path: &str) -> bool {
    let p = norm_path(path);
    if p.split('\\')
        .any(|seg| DEFAULT_EXCLUDED_SEGMENTS.contains(&seg))
    {
        return false;
    }
    if scopes.exclude.iter().any(|e| under(&norm_path(e), &p)) {
        return false;
    }
    scopes.include.iter().any(|i| under(&norm_path(i), &p))
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
        conn.execute_batch(
            "BEGIN;
             DELETE FROM vectors;
             DELETE FROM texts;
             DELETE FROM content;
             DELETE FROM index_meta;
             UPDATE files SET content_hash = NULL, state = 0, error = NULL;
             COMMIT;",
        )?;
        Self::init(conn, meta)
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

    /// Delete content/vectors/texts that no file references any more.
    pub fn gc(&mut self) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let orphan = "hash NOT IN (SELECT content_hash FROM files WHERE content_hash IS NOT NULL)";
        tx.execute(&format!("DELETE FROM vectors WHERE {orphan}"), [])?;
        tx.execute(&format!("DELETE FROM texts WHERE {orphan}"), [])?;
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
        let n = tx
            .prepare_cached("UPDATE files SET content_hash=?2, state=1, error=NULL WHERE id=?1")?
            .execute(params![file_id.0, hash])?;
        if n == 0 {
            return Err(Error::NoSuchFile(file_id));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn fail(&mut self, file_id: FileId, msg: &str) -> Result<()> {
        self.set_state(file_id, STATE_FAILED, msg)
    }

    pub fn skip(&mut self, file_id: FileId, reason: &str) -> Result<()> {
        self.set_state(file_id, STATE_SKIPPED, reason)
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
                "SELECT content_hash, id, path FROM files WHERE state = 1 AND content_hash IS NOT NULL",
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

    /// Full-text search over extracted text (FTS5 syntax; see [`fts_escape`] for raw user
    /// input). One hit per file, best bm25 first.
    pub fn search_text(&self, fts_query: &str, k: usize) -> Result<Vec<TextHit>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare_cached(
            "SELECT files.id, files.path, texts.chunk,
                    snippet(texts, 2, '[', ']', '…', 12), bm25(texts)
             FROM texts JOIN files ON files.content_hash = texts.hash AND files.state = 1
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
        let count = |sql: &str| -> Result<u64> {
            Ok(self.conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as u64)
        };
        let by_state = |s: i64| count(&format!("SELECT COUNT(*) FROM files WHERE state = {s}"));
        Ok(Stats {
            files: count("SELECT COUNT(*) FROM files")?,
            pending: by_state(STATE_PENDING)?,
            done: by_state(STATE_DONE)?,
            failed: by_state(STATE_FAILED)?,
            skipped: by_state(STATE_SKIPPED)?,
            vectors: count("SELECT COUNT(*) FROM vectors")?,
        })
    }

    // ------------------------------------------------------------ settings

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
        };
        assert!(in_scope(&root, r"C:\anything\x"));
        assert!(!in_scope(&root, r"D:\x"));
    }

    #[test]
    fn scopes_persist() {
        let mut s = Store::open_in_memory(&meta()).unwrap();
        assert_eq!(s.get_scopes().unwrap(), Scopes::default());
        let sc = Scopes {
            include: vec!["C:\\a".into()],
            exclude: vec!["C:\\a\\b".into()],
        };
        s.set_scopes(&sc).unwrap();
        s.set_scopes(&sc).unwrap();
        assert_eq!(s.get_scopes().unwrap(), sc);
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
                vectors: 2
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
}
