//! Shared daemon state, the single writer thread, and the read-side queries.
//!
//! Locking: `index` is an `RwLock<Index>`. Exactly one thread (the writer) ever takes the
//! write lock, so readers (pipe connections, the snapshot saver) only contend with short
//! batches. A volume replace builds the new index under a *read* lock and takes the write
//! lock only for the pointer swap.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Mutex, RwLock};
use std::time::Instant;

use winseek_index::{Index, SearchOptions, VolKind};
use winseek_ipc::{ChangeKind, Hit, Monitor, Response, Sort, VolumeState, VolumeStatus};
use winseek_usn::{Change, JournalCursor};

use crate::index_ops::{
    apply_change, apply_path_op, filetime_to_unix_ms, map_refs_change, replace_volume, retain_volumes, Event, PathOp,
};
use crate::log::{info, warn};

pub const RECENT_CAP: usize = 2000;
/// Max stat() calls per batch (to refine size/mtime of created/modified files).
const STAT_BUDGET: usize = 256;
const SUB_QUEUE: usize = 4096;

/// Per-volume control flags shared with that volume's worker thread.
#[derive(Default)]
pub struct VolCtl {
    /// Makes the volume's monitor (or folder scan) return (shutdown, rescan, removal).
    pub stop: AtomicBool,
    /// Set together with `stop` to request a full rescan.
    pub rescan: AtomicBool,
    /// Device query-remove in progress: the worker must close its handles.
    pub removing: AtomicBool,
    /// Set by the worker once its long-lived handles are closed (answers `removing`).
    pub released: AtomicBool,
}

/// What the status page shows besides counts and state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolInfo {
    pub fs: String,
    pub monitor: Monitor,
}

struct Sub {
    /// Lowercased, no trailing backslash. Empty = everything.
    roots: Vec<String>,
    tx: SyncSender<Response>,
}

pub struct Shared {
    pub index: RwLock<Index>,
    pub recent: Mutex<VecDeque<Hit>>,
    pub vols: Mutex<BTreeMap<char, VolumeState>>,
    pub info: Mutex<BTreeMap<char, VolInfo>>,
    ctl: Mutex<BTreeMap<char, std::sync::Arc<VolCtl>>>,
    subs: Mutex<Vec<Sub>>,
    pub shutdown: AtomicBool,
    /// Asks the main loop to save a snapshot soon (set after a volume replace).
    pub save_now: AtomicBool,
    pub started: Instant,
}

impl Shared {
    pub fn new(index: Index, letters: &[char]) -> Self {
        Shared {
            index: RwLock::new(index),
            recent: Mutex::new(VecDeque::with_capacity(RECENT_CAP)),
            vols: Mutex::new(letters.iter().map(|&l| (l, VolumeState::Scanning { done: 0, total: 0 })).collect()),
            info: Mutex::new(BTreeMap::new()),
            ctl: Mutex::new(letters.iter().map(|&l| (l, Default::default())).collect()),
            subs: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
            save_now: AtomicBool::new(false),
            started: Instant::now(),
        }
    }

    pub fn set_state(&self, letter: char, st: VolumeState) {
        self.vols.lock().unwrap().insert(letter, st);
    }

    pub fn set_info(&self, letter: char, fs: &str, monitor: Monitor) {
        self.info.lock().unwrap().insert(letter, VolInfo { fs: fs.to_string(), monitor });
    }

    /// The control flags of a volume (created on first use: volumes can arrive later).
    pub fn ctl(&self, letter: char) -> std::sync::Arc<VolCtl> {
        self.ctl.lock().unwrap().entry(letter).or_default().clone()
    }

    pub fn has_ctl(&self, letter: char) -> bool {
        self.ctl.lock().unwrap().contains_key(&letter)
    }

    /// Signal every worker to stop (shutdown).
    pub fn stop_all(&self) {
        for c in self.ctl.lock().unwrap().values() {
            c.stop.store(true, Ordering::SeqCst);
        }
    }

    /// Forget a volume that went away: status row, info and control flags.
    pub fn forget(&self, letter: char) {
        self.vols.lock().unwrap().remove(&letter);
        self.info.lock().unwrap().remove(&letter);
        self.ctl.lock().unwrap().remove(&letter);
    }

    /// Register a subscriber; the returned receiver yields `Response::Change`s. A subscriber
    /// whose queue overflows is dropped (its receiver disconnects) so it can resync.
    pub fn subscribe(&self, roots: &[String]) -> std::sync::mpsc::Receiver<Response> {
        let (tx, rx) = std::sync::mpsc::sync_channel(SUB_QUEUE);
        let roots = roots.iter().map(|r| normalize_root(r)).collect();
        self.subs.lock().unwrap().push(Sub { roots, tx });
        rx
    }

    fn notify(&self, ev: &Event) {
        let mut subs = self.subs.lock().unwrap();
        if subs.is_empty() {
            return;
        }
        let path = ev.path.to_lowercase();
        let old = ev.old_path.as_ref().map(|p| p.to_lowercase());
        subs.retain(|s| {
            let hit = s.roots.is_empty()
                || s.roots.iter().any(|r| under(&path, r) || old.as_deref().is_some_and(|o| under(o, r)));
            if !hit {
                return true;
            }
            let msg = Response::Change { kind: ev.kind, path: ev.path.clone(), old_path: ev.old_path.clone(), is_dir: ev.is_dir };
            match s.tx.try_send(msg) {
                Ok(()) => true,
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    warn!("dropping a subscriber whose queue overflowed");
                    false
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }
}

fn normalize_root(r: &str) -> String {
    r.trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase()
}

fn under(path_lower: &str, root: &str) -> bool {
    path_lower.strip_prefix(root).is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
}

pub enum Msg {
    /// Changes for one volume plus the journal cursor reached after them. ReFS changes
    /// carry 128-bit FRNs and are interned by the writer.
    Changes { vol: u8, changes: Vec<Change>, cursor: JournalCursor },
    /// Folder-watch changes (FAT/exFAT/ReFS without a journal), already stat'ed.
    Paths { vol: u8, ops: Vec<PathOp> },
    /// A finished fresh scan (journal cursor stored in its volume meta) replacing its volume.
    Replace { fresh: Box<Index> },
    /// The volume went away: drop its records.
    Drop { vol: u8 },
    Shutdown,
}

/// The one thread that mutates the index.
pub fn writer_loop(sh: &Shared, rx: Receiver<Msg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Changes { vol, changes, cursor } => apply_batch(sh, vol, &changes, cursor),
            Msg::Paths { vol, ops } => apply_paths(sh, vol, &ops),
            Msg::Replace { fresh } => replace(sh, *fresh),
            Msg::Drop { vol } => drop_volume(sh, vol),
            Msg::Shutdown => break,
        }
    }
}

fn apply_batch(sh: &Shared, vol: u8, changes: &[Change], cursor: JournalCursor) {
    let mut recent: Vec<Hit> = Vec::new();
    let mut events: Vec<Event> = Vec::new();
    {
        let mut ix = sh.index.write().unwrap();
        let mut budget = STAT_BUDGET;
        let refs = ix.kind_of(vol) == Some(VolKind::Refs);
        for c in changes {
            let mapped;
            let c = if refs {
                mapped = map_refs_change(&mut ix, vol, c);
                &mapped
            } else {
                c
            };
            let Some(ev) = apply_change(&mut ix, vol, c) else { continue };
            if let (Some(i), false, true) = (ev.idx, ev.is_dir, budget > 0) {
                if let Ok(md) = std::fs::metadata(&ev.path) {
                    budget -= 1;
                    let mt = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                    let ft = mt.map_or(0, |d| crate::index_ops::filetime_from_unix_ms(d.as_millis() as i64));
                    let ft = if ft != 0 { ft } else { ix.mtime(i).unwrap_or(0) };
                    ix.update_meta(i, md.len(), ft);
                }
            }
            if let (Some(i), true) = (ev.idx, ev.kind != ChangeKind::Deleted) {
                recent.push(hit_at(&ix, i));
            }
            events.push(ev);
        }
        if let Some(m) = ix.volume_mut(vol) {
            m.journal_id = cursor.journal_id;
            m.next_usn = cursor.next_usn;
        }
        ix.refresh();
    }
    publish(sh, recent, &events);
}

/// Apply stat'ed folder-watch ops (no I/O under the lock).
fn apply_paths(sh: &Shared, vol: u8, ops: &[PathOp]) {
    let mut recent: Vec<Hit> = Vec::new();
    let mut events: Vec<Event> = Vec::new();
    {
        let mut ix = sh.index.write().unwrap();
        if ix.root_of(vol).is_none() {
            return; // not scanned yet (or dropped): the scan/rescan covers these
        }
        for op in ops {
            let Some(ev) = apply_path_op(&mut ix, vol, op) else { continue };
            if let (Some(i), true) = (ev.idx, ev.kind != ChangeKind::Deleted) {
                recent.push(hit_at(&ix, i));
            }
            events.push(ev);
        }
        ix.refresh();
    }
    publish(sh, recent, &events);
}

fn drop_volume(sh: &Shared, vol: u8) {
    let t = Instant::now();
    {
        let mut ix = sh.index.write().unwrap();
        let old = std::mem::take(&mut *ix);
        *ix = retain_volumes(old, |v| v.letter != vol);
    }
    let prefix = format!("{}:\\", vol as char).to_lowercase();
    sh.recent.lock().unwrap().retain(|h| !h.path.to_lowercase().starts_with(&prefix));
    sh.forget(vol as char);
    sh.save_now.store(true, Ordering::Relaxed);
    info!("{}: volume dropped from the index in {:.1?}", vol as char, t.elapsed());
}

/// Push hits into the recent ring and change events to subscribers.
fn publish(sh: &Shared, recent: Vec<Hit>, events: &[Event]) {
    if !recent.is_empty() {
        let mut ring = sh.recent.lock().unwrap();
        for h in recent {
            if let Some(p) = ring.iter().position(|x| x.path == h.path) {
                ring.remove(p);
            }
            ring.push_front(h);
        }
        ring.truncate(RECENT_CAP);
    }
    for ev in events {
        sh.notify(ev);
    }
}

fn replace(sh: &Shared, fresh: Index) {
    let letters: Vec<u8> = fresh.volumes().iter().map(|v| v.letter).collect();
    let t = Instant::now();
    let merged = {
        let old = sh.index.read().unwrap();
        if old.volumes().iter().all(|v| letters.contains(&v.letter)) {
            None // nothing else to preserve: just swap
        } else {
            Some(replace_volume(&old, &fresh))
        }
    };
    let new = merged.unwrap_or(fresh);
    let old = std::mem::replace(&mut *sh.index.write().unwrap(), new);
    drop(old);
    for l in letters {
        sh.set_state(l as char, VolumeState::Live);
    }
    sh.save_now.store(true, Ordering::Relaxed);
    info!("volume replace applied in {:.1?}", t.elapsed());
}

// ------------------------------------------------------------------ read side

pub fn hit_at(ix: &Index, i: u32) -> Hit {
    let is_dir = ix.is_dir(i);
    Hit {
        name: ix.name(i).to_string(),
        path: ix.full_path(i),
        is_dir,
        size: if is_dir { None } else { ix.size(i) },
        mtime: ix.mtime(i).and_then(filetime_to_unix_ms),
    }
}

/// Returns (total, hits, note). Only name sorting exists in Tier 0; others fall back with a note.
pub fn search(sh: &Shared, query: &str, offset: u32, limit: u32, sort: Sort) -> (u64, Vec<Hit>, Option<&'static str>) {
    let (descending, note) = match sort {
        Sort::NameAsc => (false, None),
        Sort::NameDesc => (true, None),
        _ => (false, Some("only name_asc/name_desc sorts are supported; used name_asc")),
    };
    let opts = SearchOptions { offset: offset as usize, limit: limit.min(10_000) as usize, descending };
    let ix = sh.index.read().unwrap();
    let r = ix.search_str(query, &opts);
    let hits = r.items.iter().map(|&i| hit_at(&ix, i)).collect();
    (r.total as u64, hits, note)
}

pub fn status(sh: &Shared) -> Vec<VolumeStatus> {
    let states = sh.vols.lock().unwrap().clone();
    let info = sh.info.lock().unwrap().clone();
    let ix = sh.index.read().unwrap();
    states
        .into_iter()
        .map(|(letter, state)| {
            let (files, folders) = ix.counts(letter as u8);
            let (fs, monitor) = info.get(&letter).map_or((String::new(), Monitor::Usn), |i| (i.fs.clone(), i.monitor));
            VolumeStatus { letter, files, folders, state, fs, monitor }
        })
        .collect()
}

pub fn recent(sh: &Shared, limit: u32) -> Vec<Hit> {
    sh.recent.lock().unwrap().iter().take(limit.min(500) as usize).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use winseek_index::IndexBuilder;
    use winseek_usn::UsnRecord;

    fn shared() -> Shared {
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'C', 5);
        b.push(b'C', 10, 5, "Docs", true, 0, 0);
        Shared::new(b.finish(), &['C'])
    }
    fn create(frn: u128, parent: u128, name: &str) -> Change {
        Change::Create(UsnRecord { frn, parent_frn: parent, usn: 0, reason: 0, attrs: 0x20, name: name.into() })
    }
    const CUR: JournalCursor = JournalCursor { journal_id: 7, next_usn: 99 };

    #[test]
    fn batch_updates_index_cursor_recent_and_subscribers() {
        let sh = shared();
        let rx_docs = sh.subscribe(&["C:\\Docs".into()]);
        let rx_other = sh.subscribe(&["C:\\Other\\".into()]);
        apply_batch(&sh, b'C', &[create(100, 10, "a.txt"), create(101, 10, "b.txt"), create(100, 10, "a.txt")], CUR);
        let (total, hits, note) = search(&sh, "txt", 0, 10, Sort::NameDesc);
        assert_eq!((total, hits[0].name.as_str(), note), (2, "b.txt", None));
        assert!(search(&sh, "txt", 0, 10, Sort::SizeDesc).2.is_some());
        let m = &sh.index.read().unwrap().volumes()[0].clone();
        assert_eq!((m.journal_id, m.next_usn), (7, 99));
        let r = recent(&sh, 10);
        assert_eq!(r.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), vec!["C:\\Docs\\a.txt", "C:\\Docs\\b.txt"]);
        assert!(matches!(rx_docs.try_recv(), Ok(Response::Change { kind: ChangeKind::Created, .. })));
        assert!(rx_other.try_recv().is_err());
        let st = status(&sh);
        assert_eq!((st[0].files, st[0].folders), (2, 1));
    }

    #[test]
    fn folder_volume_paths_drop_and_status_fields() {
        let sh = shared();
        // a folder-watch volume arrives: scan result replaces into the index
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'E', 5);
        b.volume_mut(b'E').unwrap().kind = VolKind::Folder;
        let fresh = b.finish();
        sh.set_info('E', "exFAT", Monitor::FolderWatch);
        sh.set_state('E', VolumeState::Scanning { done: 0, total: 0 });
        replace(&sh, fresh);
        let rx = sh.subscribe(&[r"E:\Photos".into()]);
        let ops = vec![
            PathOp::Upsert { rel: r"Photos\a.jpg".into(), is_dir: false, size: 7, mtime: 0 },
            PathOp::Upsert { rel: r"Photos\b.jpg".into(), is_dir: false, size: 8, mtime: 0 },
            PathOp::Remove { rel: r"Photos\b.jpg".into() },
        ];
        apply_paths(&sh, b'E', &ops);
        let (total, hits, _) = search(&sh, "jpg", 0, 10, Sort::NameAsc);
        assert_eq!((total, hits[0].path.as_str(), hits[0].size), (1, r"E:\Photos\a.jpg", Some(7)));
        assert!(matches!(rx.try_recv(), Ok(Response::Change { kind: ChangeKind::Created, .. })));
        let st = status(&sh);
        let e = st.iter().find(|v| v.letter == 'E').unwrap();
        assert_eq!((e.fs.as_str(), e.monitor, e.files, e.folders), ("exFAT", Monitor::FolderWatch, 1, 1));
        assert_eq!(e.state, VolumeState::Live);
        // ops for a volume that is not indexed are ignored
        apply_paths(&sh, b'Z', &ops);
        assert!(sh.index.read().unwrap().root_of(b'Z').is_none());
        // unplugged: records, recent entries and the status row go away
        assert!(recent(&sh, 10).iter().any(|h| h.path.starts_with("E:")));
        drop_volume(&sh, b'E');
        assert_eq!(search(&sh, "jpg", 0, 10, Sort::NameAsc).0, 0);
        assert!(recent(&sh, 10).iter().all(|h| !h.path.starts_with("E:")));
        assert!(status(&sh).iter().all(|v| v.letter != 'E'));
        assert!(!sh.has_ctl('E'));
        assert_eq!(search(&sh, "Docs", 0, 10, Sort::NameAsc).0, 1, "other volumes kept");
    }

    #[test]
    fn refs_batches_are_interned_by_the_writer() {
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'R', 5);
        b.volume_mut(b'R').unwrap().kind = VolKind::Refs;
        b.set_id128(b'R', 1 << 100, 5);
        let sh = Shared::new(b.finish(), &['R']);
        let dir = (3u128 << 64) | 9;
        let mk = |frn: u128, parent: u128, attrs: u32, name: &str| {
            Change::Create(UsnRecord { frn, parent_frn: parent, usn: 0, reason: 0, attrs, name: name.into() })
        };
        apply_batch(&sh, b'R', &[mk(dir, 1 << 100, 0x10, "Big"), mk(dir + 1, dir, 0x20, "x.txt")], CUR);
        let (_, hits, _) = search(&sh, "x.txt", 0, 10, Sort::NameAsc);
        assert_eq!(hits[0].path, r"R:\Big\x.txt");
    }

    #[test]
    fn status_counts_follow_live_changes() {
        let sh = shared();
        assert_eq!({ let s = status(&sh); (s[0].files, s[0].folders) }, (0, 1)); // primes the cache
        apply_batch(&sh, b'C', &[create(100, 10, "a.txt"), Change::Create(UsnRecord { frn: 20, parent_frn: 10, usn: 0, reason: 0, attrs: 0x10, name: "Sub".into() }), create(101, 20, "b.txt")], CUR);
        assert_eq!({ let s = status(&sh); (s[0].files, s[0].folders) }, (2, 2));
        // rename: no change
        let ren = Change::Rename { frn: 100, old_parent: 10, old_name: "a.txt".into(), new_parent: 10, new_name: "c.txt".into() };
        apply_batch(&sh, b'C', &[ren], CUR);
        assert_eq!({ let s = status(&sh); (s[0].files, s[0].folders) }, (2, 2));
        // recursive folder delete removes the folder and its child
        let del = Change::Delete(UsnRecord { frn: 20, parent_frn: 10, usn: 0, reason: 0, attrs: 0x10, name: "Sub".into() });
        apply_batch(&sh, b'C', &[del], CUR);
        assert_eq!({ let s = status(&sh); (s[0].files, s[0].folders) }, (1, 1));
    }

    #[test]
    fn root_matching() {
        assert!(under("c:\\docs\\a", &normalize_root("C:\\Docs\\")));
        assert!(under("c:\\docs", &normalize_root("C:/Docs")));
        assert!(!under("c:\\docsx\\a", &normalize_root("C:\\Docs")));
        assert!(under("c:\\a", &normalize_root("C:\\")));
    }
}
