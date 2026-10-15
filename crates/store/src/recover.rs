//! Getting a damaged store working again without losing the user's settings.
//!
//! SQLite reports damage as `SQLITE_CORRUPT` ("database disk image is malformed") or
//! `SQLITE_NOTADB`. [`Store::recover`] runs `integrity_check`, maps the damage to tables, and:
//! 1. repairs in place when only rebuildable structures are hit: ordinary indexes (`REINDEX`) and
//!    the FTS5 index (`'rebuild'` from `texts_content`);
//! 2. otherwise salvages: the file (with its -wal/-shm) is renamed to `<name>.corrupt`, a fresh
//!    store is created, and every undamaged table is copied over. What a lost table leaves
//!    inconsistent goes back to pending, so the worker recomputes it. Settings (scopes, ignore list,
//!    collections, profile) are always copied when readable.

use std::collections::BTreeSet;

use super::*;

/// What [`Store::recover`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// `integrity_check` found nothing to repair.
    Healthy,
    /// Indexes and/or the FTS index were rebuilt in place; no data was lost.
    Repaired,
    /// The damaged file was kept as `backup`, the readable tables were copied into a fresh
    /// store, and the tables in `lost` could not be read (their work is redone).
    Salvaged { backup: PathBuf, lost: Vec<String> },
}

/// Tables copied by [`Store::salvage`], in order. `texts` is read from its content table
/// (`texts_content`) and indexed afresh, so a damaged FTS index does not lose any text.
const TABLES: &[&str] = &["settings", "files", "content", "vectors", "texts", "pending_vecs", "parse_queue"];

/// Shadow tables of the FTS index that `INSERT INTO texts(texts) VALUES('rebuild')` rewrites.
const FTS_INDEX: &[&str] = &["texts_data", "texts_idx", "texts_docsize"];

impl Error {
    /// SQLite says the file is damaged (`SQLITE_CORRUPT` / `SQLITE_NOTADB`).
    pub fn is_corruption(&self) -> bool {
        matches!(
            self,
            Error::Sqlite(e) if matches!(
                e.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
            )
        )
    }
}

/// [`Error::is_corruption`] for errors that were already turned into text (`"sqlite: ..."`).
pub fn is_corruption_message(msg: &str) -> bool {
    msg.contains("database disk image is malformed") || msg.contains("file is not a database")
}

/// Damage found by `integrity_check`, by object name (tables and indexes).
#[derive(Debug, Default)]
struct Damage {
    objects: BTreeSet<String>,
    /// A message that names no object (freelist, page accounting) or a check that died midway.
    unmapped: bool,
}

impl Damage {
    fn is_empty(&self) -> bool {
        self.objects.is_empty() && !self.unmapped
    }
}

/// Every table is checked on its own (`integrity_check(<table>)` also covers its indexes), so one
/// malformed page cannot hide damage elsewhere. When no table is hit, the whole-file check
/// catches the rest: the FTS index's own consistency and page accounting.
fn damage(conn: &Connection) -> Damage {
    let mut d = Damage::default();
    let objects = conn.prepare("SELECT name, type FROM sqlite_master WHERE type IN ('table', 'index') AND rootpage > 0").and_then(|mut st| {
        st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()
    });
    let Ok(objects) = objects else {
        d.unmapped = true;
        return d;
    };
    let indexes: Vec<&str> = objects.iter().filter(|(_, ty)| ty == "index").map(|(n, _)| n.as_str()).collect();
    for (table, _) in objects.iter().filter(|(_, ty)| ty == "table") {
        match check(conn, Some(table)) {
            Err(_) => {
                d.objects.insert(table.clone());
            }
            Ok(msgs) => {
                for m in msgs {
                    match indexes.iter().find(|ix| m.contains(&format!("index {ix}"))) {
                        Some(ix) => d.objects.insert(ix.to_string()),
                        None => d.objects.insert(table.clone()),
                    };
                }
            }
        }
    }
    if d.objects.is_empty() {
        match check(conn, None) {
            Err(_) => d.unmapped = true,
            Ok(msgs) => {
                for m in msgs {
                    if m.contains("fts5") || m.contains("FTS5") {
                        d.objects.insert("texts_data".into());
                    } else {
                        d.unmapped = true;
                    }
                }
            }
        }
    }
    d
}

/// The problems `integrity_check` reports (empty when the answer is "ok").
fn check(conn: &Connection, table: Option<&str>) -> rusqlite::Result<Vec<String>> {
    let sql = match table {
        Some(t) => format!("PRAGMA integrity_check(\"{}\")", t.replace('"', "\"\"")),
        None => "PRAGMA integrity_check(100)".to_string(),
    };
    let mut st = conn.prepare(&sql)?;
    let msgs = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(msgs.into_iter().filter(|m| m != "ok" && !m.starts_with("*** in database")).collect())
}

impl Store {
    /// The meta this store was opened with.
    pub fn meta(&self) -> &IndexMeta {
        &self.meta
    }

    /// The database file (`None` for an in-memory store).
    pub fn path(&self) -> Option<PathBuf> {
        self.conn.path().filter(|p| !p.is_empty()).map(PathBuf::from)
    }

    /// Check the file and repair what is damaged (see the module docs). Consumes the store
    /// because a salvage must close the connection before it renames the file.
    pub fn recover(self) -> Result<(Store, Recovery)> {
        let d = damage(&self.conn);
        if d.is_empty() {
            return Ok((self, Recovery::Healthy));
        }
        let in_place = !d.unmapped
            && d.objects.iter().all(|o| FTS_INDEX.contains(&o.as_str()) || is_index(&self.conn, o));
        if in_place && self.repair_in_place().is_ok() && damage(&self.conn).is_empty() {
            return Ok((self, Recovery::Repaired));
        }
        let path = match self.path() {
            Some(p) => p,
            None => return Err(Error::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                Some("in-memory store is damaged and cannot be salvaged".into()),
            ))),
        };
        let meta = self.meta.clone();
        // Unmapped damage: nothing is known to be good, so every table is read and judged by
        // whether the copy succeeds.
        let damaged: BTreeSet<String> = d.objects.into_iter().collect();
        drop(self);
        let (store, backup, lost) = Store::salvage(&path, &meta, &damaged)?;
        Ok((store, Recovery::Salvaged { backup, lost }))
    }

    /// Replace this store with a salvaged copy that keeps only settings and files (for a reset
    /// of a damaged file).
    pub(crate) fn salvage_in_place(&mut self, meta: &IndexMeta) -> Result<()> {
        let path = self.path().expect("file-backed store");
        let old = std::mem::replace(self, Store::open_in_memory(meta)?);
        drop(old); // close the file before it is replaced
        let derived = TABLES.iter().filter(|t| !matches!(**t, "settings" | "files")).map(|t| t.to_string()).collect();
        let (s, _, _) = Store::salvage(&path, meta, &derived)?;
        *self = s;
        Ok(())
    }

    fn repair_in_place(&self) -> Result<()> {
        self.conn.execute_batch("REINDEX;")?;
        self.conn.execute("INSERT INTO texts(texts) VALUES('rebuild')", [])?;
        Ok(())
    }

    /// Build a fresh store from every table of `path` not in `damaged`, keep the old file as
    /// `<path>.corrupt` and put the new one in its place. Returns the new store, the backup path
    /// and the tables that could not be copied. Usable when the old file cannot even be opened;
    /// no connection to `path` may be open.
    ///
    /// The new store is built beside the old one (`<path>.new`) and closed in rollback-journal
    /// mode, so it has no -wal/-shm. It only moves in once the old -wal/-shm are gone: SQLite
    /// would replay a stale -wal into a new file of the same name.
    pub fn salvage(path: &Path, meta: &IndexMeta, damaged: &BTreeSet<String>) -> Result<(Store, PathBuf, Vec<String>)> {
        let fresh = with_suffix(path, ".new");
        let backup = with_suffix(path, ".corrupt");
        for sfx in ["", "-wal", "-shm", "-journal"] {
            remove(&with_suffix(&fresh, sfx))?;
        }
        let lost = {
            let mut s = Store::open(&fresh, meta)?;
            let lost = s.copy_from(path, damaged);
            s.settle_lost(&lost)?;
            s.conn.pragma_update(None, "journal_mode", "DELETE")?;
            lost
        };
        for sfx in ["", "-wal", "-shm"] {
            let (from, to) = (with_suffix(path, sfx), with_suffix(&backup, sfx));
            remove(&to)?; // an older backup
            if from.exists() {
                std::fs::rename(&from, &to).map_err(|e| io_error(&from, e))?;
            }
        }
        for sfx in ["", "-wal", "-shm"] {
            let left = with_suffix(path, sfx);
            if left.exists() {
                return Err(io_error(&left, std::io::Error::other("still present after it was moved aside")));
            }
        }
        std::fs::rename(&fresh, path).map_err(|e| io_error(&fresh, e))?;
        Ok((Store::open(path, meta)?, backup, lost))
    }

    /// Copy every readable table from the file at `old`; returns the tables left behind.
    fn copy_from(&mut self, old: &Path, damaged: &BTreeSet<String>) -> Vec<String> {
        let mut lost = Vec::new();
        if self.conn.execute("ATTACH DATABASE ?1 AS old", [old.to_string_lossy()]).is_err() {
            return TABLES.iter().map(|t| t.to_string()).collect();
        }
        for &table in TABLES {
            // `texts` is judged by its content table; settings are worth an attempt even when
            // flagged (a partial read beats losing the user's folders).
            let source = if table == "texts" { "texts_content" } else { table };
            if table != "settings" && damaged.contains(source) {
                lost.push(table.to_string());
                continue;
            }
            let sql = match table {
                "settings" => "INSERT OR REPLACE INTO main.settings(key, value) SELECT key, value FROM old.settings",
                "files" => "INSERT INTO main.files(id, vol_guid, frn, path, size, mtime, kind, content_hash, state, error)
                            SELECT id, vol_guid, frn, path, size, mtime, kind, content_hash, state, error FROM old.files NOT INDEXED",
                "content" => "INSERT INTO main.content(hash) SELECT hash FROM old.content",
                "vectors" => "INSERT INTO main.vectors(hash, chunk, kind, locator, vec)
                              SELECT hash, chunk, kind, locator, vec FROM old.vectors NOT INDEXED",
                "texts" => "INSERT INTO main.texts(rowid, hash, chunk, body) SELECT id, c0, c1, c2 FROM old.texts_content NOT INDEXED",
                "pending_vecs" => "INSERT INTO main.pending_vecs(hash, chunk, kind, locator)
                                   SELECT hash, chunk, kind, locator FROM old.pending_vecs NOT INDEXED",
                _ => "INSERT INTO main.parse_queue(hash, page, reason, state, attempts, error)
                      SELECT hash, page, reason, state, attempts, error FROM old.parse_queue NOT INDEXED",
            };
            let ok = self
                .conn
                .execute_batch("BEGIN;")
                .and_then(|_| self.conn.execute(sql, []))
                .and_then(|_| self.conn.execute_batch("COMMIT;"))
                .is_ok();
            if !ok {
                if !self.conn.is_autocommit() {
                    let _ = self.conn.execute_batch("ROLLBACK;");
                }
                lost.push(table.to_string());
            }
        }
        let _ = self.conn.execute_batch("DETACH DATABASE old;");
        lost
    }

    /// Put the store back in a consistent state after `lost` tables were not copied.
    fn settle_lost(&mut self, lost: &[String]) -> Result<()> {
        let lost = |t: &str| lost.iter().any(|l| l == t);
        if lost("content") || lost("vectors") || lost("texts") {
            // Which file's chunks are missing is unknowable: redo everything.
            clear_derived(&self.conn)?;
            self.conn.execute(
                "INSERT OR REPLACE INTO index_meta(id, model_id, llama_build, quant, image_budget, dims)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5)",
                params![self.meta.model_id, self.meta.llama_build, self.meta.quant, self.meta.image_budget, self.meta.dims],
            )?;
            return Ok(());
        }
        if lost("pending_vecs") {
            // Only text-only files have pending vectors: extract them again.
            self.conn.execute(
                "UPDATE files SET content_hash = NULL, state = ?1, error = NULL WHERE state = ?2",
                params![STATE_PENDING, STATE_TEXT_ONLY],
            )?;
        }
        if lost("parse_queue") {
            // The route pass queues images again from the start.
            self.conn.execute("DELETE FROM settings WHERE key = 'parse_route_cursor'", [])?;
        }
        self.gc()?;
        Ok(())
    }
}

fn with_suffix(p: &Path, sfx: &str) -> PathBuf {
    PathBuf::from(format!("{}{sfx}", p.display()))
}

fn remove(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_error(p, e)),
        _ => Ok(()),
    }
}

fn io_error(p: &Path, e: std::io::Error) -> Error {
    Error::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
        Some(format!("{}: {e}", p.display())),
    ))
}

fn is_index(conn: &Connection, name: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1", [name], |_| Ok(()))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> IndexMeta {
        IndexMeta { model_id: "m".into(), llama_build: "b1".into(), quant: "Q8_0".into(), image_budget: 280, dims: 4 }
    }

    fn key(n: u64) -> FileKey {
        FileKey { vol_guid: "{vol}".into(), frn: n }
    }

    /// One embedded image, one text-only document, a scope setting.
    fn populate(p: &Path) {
        let mut s = Store::open(p, &meta()).unwrap();
        s.set_scopes(&Scopes { include: vec![r"C:\docs".into()], exclude: vec![] }).unwrap();
        let a = s.upsert_file(&key(1), r"C:\docs\a.jpg", 10, 1, "image").unwrap();
        let v = ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec: vec![1., 0., 0., 0.] };
        s.complete(a, "ha", &[v], &[(0, "sunset over the harbour".into())]).unwrap();
        let b = s.upsert_file(&key(2), r"C:\docs\b.txt", 10, 2, "document").unwrap();
        let pend = [PendingVec { chunk: 0, kind: VecKind::Text, locator: None }];
        s.complete_text_only(b, "hb", &pend, &[(0, "quarterly invoice".into())]).unwrap();
    } // dropped: checkpointed, WAL gone

    /// Overwrite the root page of `table` with garbage, as a lost WAL frame would leave it.
    fn smash(p: &Path, table: &str) {
        let (root, size): (i64, i64) = {
            let c = Connection::open(p).unwrap();
            let root = c.query_row("SELECT rootpage FROM sqlite_master WHERE name = ?1", [table], |r| r.get(0)).unwrap();
            let size = c.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
            (root, size)
        };
        let mut bytes = std::fs::read(p).unwrap();
        let at = ((root - 1) * size) as usize;
        bytes[at..at + 64].fill(0xEE);
        std::fs::write(p, bytes).unwrap();
    }

    fn state_of(s: &Store, path: &str) -> i64 {
        s.conn.query_row("SELECT state FROM files WHERE path = ?1", [path], |r| r.get(0)).unwrap()
    }

    fn count(s: &Store, table: &str) -> i64 {
        s.conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
    }

    /// The bundled SQLite must carry the WAL-reset fix (3.51.3): with it missing, a checkpoint
    /// racing a write on a second connection silently lost pages and corrupted the live store.
    #[test]
    fn bundled_sqlite_has_wal_reset_fix() {
        assert!(rusqlite::version_number() >= 3_051_003, "SQLite {} has the WAL-reset bug", rusqlite::version());
    }

    #[test]
    fn healthy_store_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        populate(&p);
        let (_, r) = Store::open(&p, &meta()).unwrap().recover().unwrap();
        assert_eq!(r, Recovery::Healthy);
    }

    #[test]
    fn damaged_fts_index_is_rebuilt_without_losing_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        populate(&p);
        smash(&p, "texts_data");
        let (s, r) = Store::open(&p, &meta()).unwrap().recover().unwrap();
        assert!(matches!(&r, Recovery::Repaired) || matches!(&r, Recovery::Salvaged { lost, .. } if lost.is_empty()), "{r:?}");
        assert_eq!(s.search_text("harbour", 5).unwrap().len(), 1);
        assert_eq!(s.search_text("invoice", 5).unwrap().len(), 1);
        assert_eq!(count(&s, "vectors"), 1);
        assert!(damage(&s.conn).is_empty());
    }

    /// The live failure: the pending_vecs b-tree was damaged. The rest survives, the text-only
    /// file is redone, settings are kept and the damaged file stays as a backup.
    #[test]
    fn damaged_table_is_salvaged_keeping_settings_and_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        populate(&p);
        smash(&p, "pending_vecs");
        let s = Store::open(&p, &meta()).unwrap();
        let e = s.pending_vecs("hb").unwrap_err();
        assert!(e.is_corruption() && is_corruption_message(&e.to_string()), "{e}");

        let (mut s, r) = s.recover().unwrap();
        let Recovery::Salvaged { backup, lost } = r else { panic!("{r:?}") };
        assert_eq!(lost, vec!["pending_vecs".to_string()]);
        assert!(backup.exists());
        assert!(damage(&s.conn).is_empty());
        assert_eq!(s.get_scopes().unwrap().include, vec![r"C:\docs".to_string()]);
        assert_eq!(state_of(&s, r"C:\docs\a.jpg"), STATE_DONE);
        assert_eq!(count(&s, "vectors"), 1);
        // The text-only document goes back to pending; its orphaned text is collected.
        assert_eq!(state_of(&s, r"C:\docs\b.txt"), STATE_PENDING);
        assert!(s.texts_of("hb").unwrap().is_empty());
        assert_eq!(s.search_text("harbour", 5).unwrap().len(), 1);
        // The store keeps working.
        s.set_setting("x", Some("1")).unwrap();
        drop(s);
        let s = Store::open(&p, &meta()).unwrap();
        assert_eq!(s.get_setting("x").unwrap().as_deref(), Some("1"));
    }

    /// Losing vectors means nothing derived can be trusted: start over, keep files and settings.
    #[test]
    fn lost_vectors_restart_indexing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        populate(&p);
        smash(&p, "vectors");
        let (s, r) = Store::open(&p, &meta()).unwrap().recover().unwrap();
        assert!(matches!(&r, Recovery::Salvaged { lost, .. } if lost.contains(&"vectors".to_string())), "{r:?}");
        assert_eq!(count(&s, "vectors") + count(&s, "content") + count(&s, "pending_vecs"), 0);
        assert_eq!(count(&s, "files"), 2);
        assert_eq!(state_of(&s, r"C:\docs\a.jpg"), STATE_PENDING);
        assert_eq!(s.get_scopes().unwrap().include.len(), 1);
        assert_eq!(s.meta(), &meta());
    }

    /// "Rebuild index" must work on a damaged file too.
    #[test]
    fn reset_of_a_damaged_store_salvages_settings() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        populate(&p);
        smash(&p, "vectors");
        let mut s = Store::open(&p, &meta()).unwrap();
        s.reset_in_place(&meta()).unwrap();
        assert!(damage(&s.conn).is_empty());
        assert_eq!(count(&s, "vectors") + count(&s, "texts"), 0);
        assert_eq!((count(&s, "files"), state_of(&s, r"C:\docs\a.jpg")), (2, STATE_PENDING));
        assert_eq!(s.get_scopes().unwrap().include.len(), 1);
    }

    /// A store left by a killed app: the latest commits are only in its -wal. The salvage must
    /// read them, and the new file must not inherit the old -wal (SQLite would replay it).
    #[test]
    fn salvage_reads_a_hot_wal_and_leaves_no_stale_wal_behind() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.db");
        populate(&live);
        let mut s = Store::open(&live, &meta()).unwrap();
        s.set_setting("late", Some("in the wal")).unwrap();
        let p = dir.path().join("s.db");
        for sfx in ["", "-wal", "-shm"] {
            std::fs::copy(with_suffix(&live, sfx), with_suffix(&p, sfx)).unwrap(); // "crashed" copy
        }
        drop(s);
        let damaged: BTreeSet<String> = ["vectors".to_string()].into();
        let (s, backup, lost) = Store::salvage(&p, &meta(), &damaged).unwrap();
        assert_eq!(lost, vec!["vectors".to_string()]);
        assert_eq!(s.get_setting("late").unwrap().as_deref(), Some("in the wal"));
        assert_eq!(count(&s, "content") + count(&s, "vectors"), 0);
        assert_eq!(count(&s, "files"), 2);
        assert!(damage(&s.conn).is_empty());
        assert!(backup.exists() && !with_suffix(&p, ".new").exists());
        drop(s);
        assert!(!with_suffix(&p, "-wal").exists() || std::fs::metadata(with_suffix(&p, "-wal")).unwrap().len() == 0);
    }

    #[test]
    fn reset_in_place_keeps_settings_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        populate(&p);
        let mut s = Store::open(&p, &meta()).unwrap();
        let mut m2 = meta();
        m2.llama_build = "b2".into();
        s.reset_in_place(&m2).unwrap();
        assert_eq!(count(&s, "vectors") + count(&s, "texts"), 0);
        assert_eq!(count(&s, "files"), 2);
        assert_eq!(s.get_scopes().unwrap().include.len(), 1);
        drop(s);
        assert!(Store::open(&p, &m2).is_ok());
    }
}
