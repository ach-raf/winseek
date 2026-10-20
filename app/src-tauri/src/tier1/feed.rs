//! Change feed: subscribe to searchd's USN-derived events for the include roots and turn them
//! into store operations.
//!
//! [`map_change`] is pure (event + scopes + ignore list -> [`StoreOp`]s); [`apply_ops`] performs them.

use std::io;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use winseek_ipc::pipe::connect;
use winseek_ipc::{read_message, write_message, ChangeKind, Request, Response, PIPE_NAME};
use winseek_store::{in_scope_with, Ignore, Scopes, Store};

use super::crawler::{crawl, kind_of, skip_attrs, KIND_CODE, KIND_IMAGE};
use super::gitignore;
use super::identity::{file_info, FileInfo, ATTR_DIRECTORY};
use super::Tier1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOp {
    /// Read identity from disk and insert / refresh the row.
    Upsert { path: String },
    RemovePath { path: String },
    RemoveUnder { path: String },
    /// File rename; falls back to `Upsert { new }` when the old path is unknown (moved in).
    RenamePath { old: String, new: String },
    /// Folder rename; falls back to a crawl of `new` when no row lived under `old`.
    RenameUnder { old: String, new: String },
}

/// Pure mapping from one searchd change event to store operations.
pub fn map_change(
    scopes: &Scopes,
    ignore: &Ignore,
    index_code: bool,
    kind: ChangeKind,
    path: &str,
    old_path: Option<&str>,
    is_dir: bool,
) -> Vec<StoreOp> {
    let wanted = |p: &str| kind_of(p, index_code).is_some() && in_scope_with(scopes, ignore, p);
    match kind {
        ChangeKind::Created | ChangeKind::Modified => {
            if !is_dir && wanted(path) {
                vec![StoreOp::Upsert { path: path.into() }]
            } else {
                vec![]
            }
        }
        // Rows only exist for in-scope files, so no scope check: removing is always safe.
        ChangeKind::Deleted => {
            if is_dir {
                vec![StoreOp::RemoveUnder { path: path.into() }]
            } else {
                vec![StoreOp::RemovePath { path: path.into() }]
            }
        }
        ChangeKind::Renamed => match old_path {
            None => map_change(scopes, ignore, index_code, ChangeKind::Created, path, None, is_dir),
            Some(old) if is_dir => {
                if in_scope_with(scopes, ignore, path) {
                    vec![StoreOp::RenameUnder { old: old.into(), new: path.into() }]
                } else {
                    vec![StoreOp::RemoveUnder { path: old.into() }]
                }
            }
            Some(old) => {
                if wanted(path) {
                    vec![StoreOp::RenamePath { old: old.into(), new: path.into() }]
                } else {
                    vec![StoreOp::RemovePath { path: old.into() }]
                }
            }
        },
    }
}

/// Drop the ops that would index a source file its `.gitignore` chain ignores. `ignored` answers
/// for one path (injectable; the real one reads the `.gitignore` files from disk). A rename whose
/// target is ignored becomes a removal of the old row.
pub fn drop_gitignored(ops: Vec<StoreOp>, ignored: &dyn Fn(&str) -> bool) -> Vec<StoreOp> {
    let is_ignored_code = |p: &str| kind_of(p, true) == Some(KIND_CODE) && ignored(p);
    ops.into_iter()
        .filter_map(|op| match op {
            StoreOp::Upsert { path } if is_ignored_code(&path) => None,
            StoreOp::RenamePath { old, new } if is_ignored_code(&new) => Some(StoreOp::RemovePath { path: old }),
            op => Some(op),
        })
        .collect()
}

/// Perform `ops` against `store`. `probe` reads file identity (injectable for tests).
/// `recrawl` is called for a folder moved into scope from somewhere we had no rows for.
/// Returns the number of ops that changed something.
pub fn apply_ops(
    store: &mut Store,
    ops: &[StoreOp],
    probe: &dyn Fn(&str) -> io::Result<FileInfo>,
    recrawl: &mut dyn FnMut(&str),
) -> usize {
    let mut changed = 0;
    let upsert = |store: &mut Store, path: &str| -> bool {
        match probe(path) {
            Ok(i) => store.upsert_file(&i.key, path, i.size, i.mtime_ms, kind_of(path, true).unwrap_or(KIND_IMAGE)).is_ok(),
            Err(_) => false, // gone again before we got to it; its Deleted event follows
        }
    };
    for op in ops {
        let did = match op {
            StoreOp::Upsert { path } => upsert(store, path),
            StoreOp::RemovePath { path } => store.remove_path(path).unwrap_or(false),
            StoreOp::RemoveUnder { path } => store.remove_under(path).unwrap_or(0) > 0,
            StoreOp::RenamePath { old, new } => match store.rename_path(old, new) {
                Ok(true) => true,
                Ok(false) => upsert(store, new),
                Err(_) => false,
            },
            StoreOp::RenameUnder { old, new } => match store.rename_under(old, new) {
                Ok(0) => {
                    recrawl(new);
                    true
                }
                Ok(_) => true,
                Err(_) => false,
            },
        };
        changed += usize::from(did);
    }
    changed
}

const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Next reconnect delay (doubling, capped).
pub fn next_backoff(cur: Duration) -> Duration {
    (cur * 2).min(BACKOFF_MAX)
}

/// Thread body: connect, subscribe, apply events; reconnect with backoff; resubscribe when
/// the scopes change (noticed at the next event or poll tick).
pub fn run(t: Arc<Tier1>) {
    super::lower_thread_priority();
    let mut backoff = BACKOFF_START;
    loop {
        let snap = t.snapshot();
        let (scopes, gen) = (&snap.scopes, snap.gen);
        if scopes.include.is_empty() {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        let conn = match connect(PIPE_NAME, Duration::from_millis(500)) {
            Ok(c) => c,
            Err(_) => {
                std::thread::sleep(backoff);
                backoff = next_backoff(backoff);
                continue;
            }
        };
        let mut conn = conn;
        if write_message(&mut conn, &Request::Subscribe { id: 1, roots: scopes.include.clone() }).is_err() {
            std::thread::sleep(backoff);
            backoff = next_backoff(backoff);
            continue;
        }
        backoff = BACKOFF_START;
        // A reader thread owns the blocking pipe read; if we drop the receiver (scope change)
        // it exits at its next message, closing the connection.
        let (tx, rx) = mpsc::channel::<io::Result<Response>>();
        std::thread::spawn(move || {
            let mut conn = conn;
            loop {
                let r = read_message::<_, Response>(&mut conn);
                let stop = r.is_err();
                if tx.send(r).is_err() || stop {
                    return;
                }
            }
        });
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Ok(Response::Change { kind, path, old_path, is_dir })) => {
                    let ops = map_change(scopes, &snap.ignore, snap.index_code, kind, &path, old_path.as_deref(), is_dir);
                    if !ops.is_empty() {
                        t.apply_ops(&ops);
                    }
                }
                Ok(Ok(_)) => {} // Error / unrelated: ignore
                Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => break, // pipe closed: reconnect
                Err(RecvTimeoutError::Timeout) => {}
            }
            if t.snapshot().gen != gen {
                break; // resubscribe with the new roots
            }
        }
    }
}

/// Real-world probe + recrawl wiring used by [`Tier1::apply_ops`].
pub fn real_apply(t: &Tier1, ops: &[StoreOp]) -> usize {
    let snap = t.snapshot();
    let recrawl = |dir: &str| {
        let sub = Scopes { include: vec![dir.to_string()], exclude: snap.scopes.exclude.clone(), ..Default::default() };
        // The moved-in folder must itself be inside the real scopes (map_change checked that).
        crawl(&t.store, &sub, &snap.ignore, snap.index_code, &|| false);
    };
    let ops = if snap.index_code {
        drop_gitignored(ops.to_vec(), &|p| gitignore::is_ignored(&snap.scopes.include, p))
    } else {
        ops.to_vec()
    };
    let ops = &ops[..];
    let mut store = t.store.lock().unwrap_or_else(|e| e.into_inner());
    // Recrawl locks the store itself; run the store ops first, then the deferred crawls.
    let mut deferred: Vec<String> = Vec::new();
    // Same exclusions as the crawler: no reparse points (cloud placeholders), hidden+system or directories.
    let probe = |p: &str| -> io::Result<FileInfo> {
        let i = file_info(Path::new(p))?;
        if skip_attrs(i.attrs) || i.attrs & ATTR_DIRECTORY != 0 {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        Ok(i)
    };
    let n = apply_ops(&mut store, ops, &probe, &mut |d| deferred.push(d.to_string()));
    drop(store);
    for d in deferred {
        recrawl(&d);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use winseek_store::{FileKey, IndexMeta};

    fn scopes() -> Scopes {
        Scopes { include: vec![r"C:\Pics".into()], exclude: vec![r"C:\Pics\private".into()], ..Default::default() }
    }

    fn map(kind: ChangeKind, path: &str, old: Option<&str>, dir: bool) -> Vec<StoreOp> {
        map_change(&scopes(), &Ignore::defaults(), false, kind, path, old, dir)
    }

    #[test]
    fn created_and_modified() {
        let up = vec![StoreOp::Upsert { path: r"C:\Pics\a.JPG".into() }];
        assert_eq!(map(ChangeKind::Created, r"C:\Pics\a.JPG", None, false), up);
        assert_eq!(map(ChangeKind::Modified, r"C:\Pics\a.JPG", None, false), up);
        // unsupported, out of scope, excluded, directory
        assert!(map(ChangeKind::Created, r"C:\Pics\a.exe", None, false).is_empty());
        assert!(map(ChangeKind::Created, r"C:\Other\a.jpg", None, false).is_empty());
        assert!(map(ChangeKind::Created, r"C:\Pics\private\a.jpg", None, false).is_empty());
        assert!(map(ChangeKind::Created, r"C:\Pics\sub", None, true).is_empty());
        assert!(map(ChangeKind::Modified, r"C:\Pics\node_modules\a.jpg", None, false).is_empty());
    }

    #[test]
    fn documents_always_and_code_only_with_the_opt_in() {
        let m = |idx: bool, p: &str| map_change(&scopes(), &Ignore::defaults(), idx, ChangeKind::Created, p, None, false);
        assert_eq!(m(false, r"C:\Pics\n.docx"), vec![StoreOp::Upsert { path: r"C:\Pics\n.docx".into() }]);
        assert!(m(false, r"C:\Pics\lib.rs").is_empty());
        assert_eq!(m(true, r"C:\Pics\lib.rs"), vec![StoreOp::Upsert { path: r"C:\Pics\lib.rs".into() }]);
        assert!(m(true, r"C:\Pics\private\lib.rs").is_empty());
        // opt-in off: a renamed code file drops out
        let r = map_change(&scopes(), &Ignore::defaults(), false, ChangeKind::Renamed, r"C:\Pics\b.rs", Some(r"C:\Pics\a.rs"), false);
        assert_eq!(r, vec![StoreOp::RemovePath { path: r"C:\Pics\a.rs".into() }]);
    }

    #[test]
    fn gitignored_code_is_dropped_but_documents_and_removals_are_not() {
        let ops = vec![
            StoreOp::Upsert { path: r"C:\Pics\gen\a.rs".into() },
            StoreOp::Upsert { path: r"C:\Pics\gen\a.docx".into() },
            StoreOp::Upsert { path: r"C:\Pics\ok.rs".into() },
            StoreOp::RenamePath { old: r"C:\Pics\x.rs".into(), new: r"C:\Pics\gen\x.rs".into() },
            StoreOp::RemovePath { path: r"C:\Pics\gen\gone.rs".into() },
        ];
        let out = drop_gitignored(ops, &|p| p.contains("gen"));
        assert_eq!(
            out,
            vec![
                StoreOp::Upsert { path: r"C:\Pics\gen\a.docx".into() },
                StoreOp::Upsert { path: r"C:\Pics\ok.rs".into() },
                StoreOp::RemovePath { path: r"C:\Pics\x.rs".into() },
                StoreOp::RemovePath { path: r"C:\Pics\gen\gone.rs".into() },
            ]
        );
    }

    #[test]
    fn deleted_file_and_dir() {
        assert_eq!(
            map(ChangeKind::Deleted, r"C:\Pics\a.jpg", None, false),
            vec![StoreOp::RemovePath { path: r"C:\Pics\a.jpg".into() }]
        );
        assert_eq!(
            map(ChangeKind::Deleted, r"C:\Pics\sub", None, true),
            vec![StoreOp::RemoveUnder { path: r"C:\Pics\sub".into() }]
        );
        // deletes are not scope-filtered
        assert_eq!(map(ChangeKind::Deleted, r"C:\Gone\a.png", None, false).len(), 1);
    }

    #[test]
    fn renamed_file() {
        assert_eq!(
            map(ChangeKind::Renamed, r"C:\Pics\b.jpg", Some(r"C:\Pics\a.jpg"), false),
            vec![StoreOp::RenamePath { old: r"C:\Pics\a.jpg".into(), new: r"C:\Pics\b.jpg".into() }]
        );
        // moved out of scope / renamed to unsupported ext -> remove the old row
        for new in [r"C:\Elsewhere\b.jpg", r"C:\Pics\b.tmp", r"C:\Pics\private\b.jpg"] {
            assert_eq!(
                map(ChangeKind::Renamed, new, Some(r"C:\Pics\a.jpg"), false),
                vec![StoreOp::RemovePath { path: r"C:\Pics\a.jpg".into() }],
                "{new}"
            );
        }
        // no old path: behaves like Created
        assert_eq!(
            map(ChangeKind::Renamed, r"C:\Pics\n.jpg", None, false),
            vec![StoreOp::Upsert { path: r"C:\Pics\n.jpg".into() }]
        );
    }

    #[test]
    fn renamed_dir() {
        assert_eq!(
            map(ChangeKind::Renamed, r"C:\Pics\new", Some(r"C:\Pics\old"), true),
            vec![StoreOp::RenameUnder { old: r"C:\Pics\old".into(), new: r"C:\Pics\new".into() }]
        );
        assert_eq!(
            map(ChangeKind::Renamed, r"D:\away", Some(r"C:\Pics\old"), true),
            vec![StoreOp::RemoveUnder { path: r"C:\Pics\old".into() }]
        );
    }

    fn store() -> Store {
        let m = IndexMeta { model_id: "t".into(), llama_build: "b".into(), quant: "q".into(), image_budget: 280, dims: 4 };
        Store::open_in_memory(&m).unwrap()
    }

    fn fake_probe(path: &str) -> io::Result<FileInfo> {
        if path.contains("gone") {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        let frn = path.bytes().map(u64::from).sum::<u64>();
        Ok(FileInfo { key: FileKey { vol_guid: "{v}".into(), frn }, size: 5, mtime_ms: 1, attrs: 0 })
    }

    #[test]
    fn apply_ops_against_store() {
        let mut s = store();
        let mut deferred = Vec::new();
        let run = |s: &mut Store, ops: Vec<StoreOp>, d: &mut Vec<String>| {
            apply_ops(s, &ops, &fake_probe, &mut |p| d.push(p.to_string()))
        };
        assert_eq!(run(&mut s, vec![StoreOp::Upsert { path: r"C:\Pics\a.jpg".into() }], &mut deferred), 1);
        assert_eq!(run(&mut s, vec![StoreOp::Upsert { path: r"C:\Pics\gone.jpg".into() }], &mut deferred), 0);
        assert_eq!(s.stats().unwrap().files, 1);
        // rename of a known file keeps one row
        run(&mut s, vec![StoreOp::RenamePath { old: r"C:\Pics\a.jpg".into(), new: r"C:\Pics\b.jpg".into() }], &mut deferred);
        assert_eq!(s.next_pending(5).unwrap()[0].path, r"C:\Pics\b.jpg");
        assert_eq!(s.stats().unwrap().files, 1);
        // rename of an unknown file (moved into scope) upserts
        run(&mut s, vec![StoreOp::RenamePath { old: r"C:\Out\x.jpg".into(), new: r"C:\Pics\x.jpg".into() }], &mut deferred);
        assert_eq!(s.stats().unwrap().files, 2);
        // folder rename moves rows; unknown folder requests a crawl
        run(&mut s, vec![StoreOp::RenameUnder { old: r"C:\Pics".into(), new: r"C:\Pix".into() }], &mut deferred);
        assert!(s.next_pending(5).unwrap().iter().all(|f| f.path.starts_with(r"C:\Pix\")));
        assert!(deferred.is_empty());
        run(&mut s, vec![StoreOp::RenameUnder { old: r"C:\Nowhere".into(), new: r"C:\Pix\in".into() }], &mut deferred);
        assert_eq!(deferred, [r"C:\Pix\in"]);
        // deletes
        assert_eq!(run(&mut s, vec![StoreOp::RemovePath { path: r"C:\Pix\b.jpg".into() }], &mut deferred), 1);
        assert_eq!(run(&mut s, vec![StoreOp::RemoveUnder { path: r"C:\Pix".into() }], &mut deferred), 1);
        assert_eq!(s.stats().unwrap().files, 0);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut b = BACKOFF_START;
        let mut seq = vec![];
        for _ in 0..7 {
            seq.push(b.as_secs());
            b = next_backoff(b);
        }
        assert_eq!(seq, [1, 2, 4, 8, 16, 30, 30]);
    }
}
