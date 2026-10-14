//! Folder-watch volumes (FAT, FAT32, exFAT, ReFS without a journal): `FindFirstFileExW`
//! worklist scan + overlapped `ReadDirectoryChangesW`, rescanning on overflow or error.
//! See docs/specs/2026-10-14-p3-nonntfs.md §2 (Everything's FAT folder index / RDC monitor).

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winseek_folder::{stat, walk_names, DirWatch, PathEvent, Poll};
use winseek_index::{Index, IndexBuilder, VolKind, FIRST_SYNTHETIC_ID};
use winseek_ipc::VolumeState;

use crate::index_ops::PathOp;
use crate::log::{error, info, warn};
use crate::state::{Msg, Shared};
use crate::worker::{guard_removal, present, settle_removal, sleep_stoppable, Vol};

/// Everything's `monitor_update_delay` (ms): changes are batched this long.
const BATCH: Duration = Duration::from_millis(1000);
/// Everything's `monitor_retry_delay`.
const ERROR_RETRY: Duration = Duration::from_secs(30);
/// An added directory is walked so its contents are indexed; past this many entries a
/// rescan is cheaper and safer.
const MAX_ADDED_SUBTREE: usize = 100_000;
/// Overflow rescans back off (5 s, 10 s, ... up to 5 min) while the volume keeps overflowing.
const OVERFLOW_BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Why a follow loop ended.
#[derive(Debug)]
enum Follow {
    /// `ctl.stop` (shutdown, rescan request, removal).
    Stopped,
    /// Zero-byte completion / notify enum dir / bad buffer / too large an added subtree.
    Overflow,
    Error(io::Error),
}

/// Scan `root` (a volume root, or any directory in tests) into a fresh single-volume index of
/// kind `Folder`: root id 5, folders get synthetic ids, files 0.
pub fn scan_tree(root: &str, vol: u8, serial: u64, stop: &AtomicBool, progress: &mut dyn FnMut(u64)) -> io::Result<Index> {
    let mut b = IndexBuilder::new(true);
    b.add_volume(vol, 5);
    let mut next_id = FIRST_SYNTHETIC_ID;
    let mut count = 0u64;
    let mut last = Instant::now();
    walk_names(root, 5, &mut next_id, stop, &mut |e| {
        count += 1;
        b.push(vol, e.id, e.parent_id, e.name, e.is_dir, e.size, e.mtime);
        if last.elapsed() >= Duration::from_secs(1) {
            progress(count);
            last = Instant::now();
        }
    })?;
    if let Some(m) = b.volume_mut(vol) {
        m.kind = VolKind::Folder;
        m.serial = serial;
        m.next_id = next_id;
    }
    Ok(b.finish())
}

fn join(root: &str, rel: &str) -> String {
    format!("{}\\{}", root.trim_end_matches('\\'), rel)
}

/// Turn one watch event into stat'ed index ops (I/O happens here, on the monitor thread,
/// never under the index lock). A path that vanished again is dropped; an added directory
/// is walked. `Err(())` = the added subtree is too large: rescan instead.
pub fn event_ops(root: &str, ev: PathEvent, out: &mut Vec<PathOp>) -> Result<(), ()> {
    match ev {
        PathEvent::Added(rel) | PathEvent::Modified(rel) if rel.is_empty() => {}
        PathEvent::Added(rel) => {
            let Some(st) = stat(&join(root, &rel)) else { return Ok(()) };
            out.push(PathOp::Upsert { rel: rel.clone(), is_dir: st.is_dir, size: st.size, mtime: st.mtime });
            if st.is_dir && !st.is_reparse {
                add_subtree(root, &rel, out)?;
            }
        }
        PathEvent::Modified(rel) => {
            if let Some(st) = stat(&join(root, &rel)).filter(|s| !s.is_dir) {
                out.push(PathOp::Upsert { rel, is_dir: false, size: st.size, mtime: st.mtime });
            }
        }
        PathEvent::Removed(rel) => out.push(PathOp::Remove { rel }),
        PathEvent::Renamed { old, new } => match stat(&join(root, &new)) {
            Some(st) => out.push(PathOp::Rename { old, new, is_dir: st.is_dir, size: st.size, mtime: st.mtime }),
            None => out.push(PathOp::Remove { rel: old }),
        },
    }
    Ok(())
}

/// A directory that appears with content (moved in from another volume, extracted, …) only
/// reports itself; walk it. Idempotent with any child events that also arrive.
fn add_subtree(root: &str, rel: &str, out: &mut Vec<PathOp>) -> Result<(), ()> {
    let stop = AtomicBool::new(false);
    let mut paths: HashMap<u64, String> = HashMap::new();
    paths.insert(1, rel.to_string());
    let mut next = 2u64;
    let mut n = 0usize;
    let r = walk_names(&join(root, rel), 1, &mut next, &stop, &mut |e| {
        n += 1;
        if n > MAX_ADDED_SUBTREE {
            stop.store(true, Ordering::Relaxed);
            return;
        }
        let Some(parent) = paths.get(&e.parent_id) else { return };
        let child = format!("{parent}\\{}", e.name);
        if e.is_dir {
            paths.insert(e.id, child.clone());
        }
        out.push(PathOp::Upsert { rel: child, is_dir: e.is_dir, size: e.size, mtime: e.mtime });
    });
    match r {
        _ if n > MAX_ADDED_SUBTREE => Err(()),
        Err(e) => {
            warn!("walk of added folder {rel} failed: {e}");
            Ok(())
        }
        Ok(_) => Ok(()),
    }
}

/// Follow `watch`, sending stat'ed batches to the writer at most every [`BATCH`].
fn follow(watch: &mut DirWatch, root: &str, vol: u8, tx: &Sender<Msg>, stop: &AtomicBool) -> Follow {
    let mut pending: Vec<PathOp> = Vec::new();
    let mut last_flush = Instant::now();
    let flush = |pending: &mut Vec<PathOp>| {
        if !pending.is_empty() {
            let _ = tx.send(Msg::Paths { vol, ops: std::mem::take(pending) });
        }
    };
    loop {
        if stop.load(Ordering::SeqCst) {
            flush(&mut pending);
            return Follow::Stopped;
        }
        match watch.poll(250) {
            Ok(Poll::Events(evs)) => {
                for ev in evs {
                    if event_ops(root, ev, &mut pending).is_err() {
                        return Follow::Overflow; // queued ops are superseded by the rescan
                    }
                }
            }
            // Everything: "need to rescan" — queued tasks for the volume are purged.
            Ok(Poll::Overflow) => return Follow::Overflow,
            Ok(Poll::Timeout) => {}
            Err(_) if stop.load(Ordering::SeqCst) => {
                flush(&mut pending);
                return Follow::Stopped;
            }
            Err(e) => return Follow::Error(e),
        }
        if last_flush.elapsed() >= BATCH {
            flush(&mut pending);
            last_flush = Instant::now();
        }
    }
}

/// Folder-watch volume loop. The watch is armed BEFORE each scan, so changes made while
/// scanning are delivered (and replayed idempotently) after the volume replace.
pub fn run(sh: Arc<Shared>, tx: Sender<Msg>, vol: Vol) {
    let l = vol.letter;
    let v = l as u8;
    let root = vol.root();
    let ctl = sh.ctl(l);
    sh.set_info(l, &vol.fs, vol.monitor());
    let mut overflows = 0u32;
    while !sh.shutdown.load(Ordering::Relaxed) {
        if ctl.removing.load(Ordering::SeqCst) && settle_removal(&sh, &tx, &ctl, l, None) {
            return;
        }
        let mut watch = match DirWatch::open(&root) {
            Ok(w) => w,
            Err(e) => {
                if !present(l) {
                    let _ = tx.send(Msg::Drop { vol: v });
                    return;
                }
                error!("{l}: cannot watch {root}: {e}; retrying in {}s", ERROR_RETRY.as_secs());
                sh.set_state(l, VolumeState::Error { msg: format!("cannot watch the volume: {e}") });
                sleep_stoppable(&sh, ERROR_RETRY);
                continue;
            }
        };
        let reg = guard_removal(watch.raw_handle(), ctl.clone(), Some(watch.canceller()), l);
        let has_index = sh.index.read().unwrap().root_of(v).is_some();
        sh.set_state(l, if has_index { VolumeState::Rescanning } else { VolumeState::Scanning { done: 0, total: 0 } });
        let t = Instant::now();
        let scanned = scan_tree(&root, v, vol.serial, &ctl.stop, &mut |n| {
            if !has_index {
                sh.set_state(l, VolumeState::Scanning { done: n, total: 0 });
            }
        });
        let started = Instant::now();
        let exit = match scanned {
            Ok(fresh) => {
                info!("{l}: {} scan: {} entries in {:.1?}", vol.fs, fresh.live_count(), t.elapsed());
                let _ = tx.send(Msg::Replace { fresh: Box::new(fresh) });
                info!("{l}: watching {root} (ReadDirectoryChangesW)");
                follow(&mut watch, &root, v, &tx, &ctl.stop)
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Follow::Stopped,
            Err(e) => Follow::Error(e),
        };
        drop(watch);
        if ctl.removing.load(Ordering::SeqCst) {
            if settle_removal(&sh, &tx, &ctl, l, reg) {
                return;
            }
            continue;
        }
        drop(reg);
        match exit {
            Follow::Stopped => {
                ctl.stop.store(false, Ordering::Relaxed);
                if ctl.rescan.swap(false, Ordering::Relaxed) {
                    info!("{l}: rescan requested");
                }
                // otherwise shutdown (loop ends) or a spurious stop (rescan: no cursor here)
            }
            Follow::Overflow => {
                overflows = if started.elapsed() > Duration::from_secs(600) { 1 } else { overflows + 1 };
                let wait = (Duration::from_secs(5) * 2u32.pow(overflows.min(6) - 1)).min(OVERFLOW_BACKOFF_MAX);
                warn!("{l}: change buffer overflowed; need to rescan (in {}s)", wait.as_secs());
                sh.set_state(l, VolumeState::Rescanning);
                sleep_stoppable(&sh, wait);
            }
            Follow::Error(e) => {
                if !present(l) {
                    info!("{l}: volume is gone");
                    let _ = tx.send(Msg::Drop { vol: v });
                    return;
                }
                error!("{l}: folder watch failed: {e}; rescanning in {}s", ERROR_RETRY.as_secs());
                sh.set_state(l, VolumeState::Error { msg: e.to_string() });
                sleep_stoppable(&sh, ERROR_RETRY);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_ops::apply_path_op;
    use winseek_index::SearchOptions;

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(name: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("winseek-searchd-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn s(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn paths(ix: &Index, q: &str) -> Vec<String> {
        let mut v: Vec<String> = ix.search_str(q, &SearchOptions::default()).items.iter().map(|&i| ix.full_path(i)).collect();
        v.sort();
        v
    }

    /// The real scan + event pipeline on an NTFS temp dir standing in for a FAT root (the
    /// APIs are filesystem-agnostic; a FAT VHDX run is `winseek-folder`'s ignored test).
    #[test]
    fn scan_then_watch_events_keep_the_index_in_sync() {
        let t = Tmp::new("follow");
        let root = t.s();
        std::fs::create_dir_all(t.0.join("Docs").join("Sub")).unwrap();
        std::fs::write(t.0.join("Docs").join("a.txt"), b"12345").unwrap();
        std::fs::write(t.0.join("Docs").join("Sub").join("deep.bin"), b"x").unwrap();
        let stop = AtomicBool::new(false);
        let mut watch = DirWatch::open(root).unwrap(); // armed before the scan, like run()
        let mut ix = scan_tree(root, b'Q', 0x1234, &stop, &mut |_| {}).unwrap();
        let m = ix.volumes()[0].clone();
        assert_eq!((m.kind, m.serial), (VolKind::Folder, 0x1234));
        assert_eq!(m.next_id, FIRST_SYNTHETIC_ID + 2, "two folders got ids");
        assert_eq!(paths(&ix, "deep"), vec![r"Q:\Docs\Sub\deep.bin"]);
        let i = ix.find_path(b'Q', r"Docs\a.txt").unwrap();
        assert_eq!(ix.size(i), Some(5));

        // changes: a file, a folder moved in with content, a rename, a delete
        std::fs::write(t.0.join("Docs").join("new.txt"), b"abc").unwrap();
        let outside = Tmp::new("outside");
        std::fs::create_dir_all(outside.0.join("Pack").join("Inner")).unwrap();
        std::fs::write(outside.0.join("Pack").join("Inner").join("p.txt"), b"p").unwrap();
        std::fs::rename(outside.0.join("Pack"), t.0.join("Pack")).ok(); // same volume: a rename event
        std::fs::rename(t.0.join("Docs").join("a.txt"), t.0.join("Docs").join("b.txt")).unwrap();
        std::fs::remove_file(t.0.join("Docs").join("Sub").join("deep.bin")).unwrap();

        let mut ops = Vec::new();
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            match watch.poll(200).unwrap() {
                Poll::Events(evs) => {
                    for ev in evs {
                        event_ops(root, ev, &mut ops).unwrap();
                    }
                }
                Poll::Overflow => panic!("overflow"),
                Poll::Timeout if !ops.is_empty() => break,
                Poll::Timeout => {}
            }
        }
        for op in &ops {
            apply_path_op(&mut ix, b'Q', op);
        }
        ix.refresh();
        assert_eq!(paths(&ix, "new.txt"), vec![r"Q:\Docs\new.txt"]);
        assert_eq!(paths(&ix, "b.txt"), vec![r"Q:\Docs\b.txt"]);
        assert!(paths(&ix, "a.txt").is_empty());
        assert!(paths(&ix, "deep").is_empty());
        assert_eq!(paths(&ix, "p.txt"), vec![r"Q:\Pack\Inner\p.txt"], "moved-in folder content: {ops:?}");
    }

    #[test]
    fn added_folder_is_walked_and_vanished_paths_are_dropped() {
        let t = Tmp::new("added");
        std::fs::create_dir_all(t.0.join("New").join("A")).unwrap();
        std::fs::write(t.0.join("New").join("A").join("f.txt"), b"hi").unwrap();
        let mut ops = Vec::new();
        event_ops(t.s(), PathEvent::Added("New".into()), &mut ops).unwrap();
        let rels: Vec<&str> = ops
            .iter()
            .map(|o| match o {
                PathOp::Upsert { rel, .. } => rel.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(rels, vec!["New", r"New\A", r"New\A\f.txt"]);
        ops.clear();
        event_ops(t.s(), PathEvent::Added("gone.txt".into()), &mut ops).unwrap();
        event_ops(t.s(), PathEvent::Modified("New".into()), &mut ops).unwrap(); // folder mtime: ignored
        assert!(ops.is_empty());
        event_ops(t.s(), PathEvent::Renamed { old: "x".into(), new: "missing".into() }, &mut ops).unwrap();
        assert_eq!(ops, vec![PathOp::Remove { rel: "x".into() }]);
    }
}
