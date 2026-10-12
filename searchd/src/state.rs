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
use std::time::{Duration, Instant};

use winseek_index::{Index, SearchOptions};
use winseek_ipc::{ChangeKind, Hit, Response, Sort, VolumeState, VolumeStatus};
use winseek_usn::{Change, JournalCursor};

use crate::index_ops::{apply_change, filetime_to_unix_ms, replace_volume, Event};
use crate::log::{info, warn};

pub const RECENT_CAP: usize = 2000;
/// Max stat() calls per batch (to refine size/mtime of created/modified files).
const STAT_BUDGET: usize = 256;
const SUB_QUEUE: usize = 4096;
const COUNTS_TTL: Duration = Duration::from_secs(10);

/// Per-volume control flags shared with that volume's worker thread.
#[derive(Default)]
pub struct VolCtl {
    /// Makes the volume's `run_monitor` return (shutdown or rescan).
    pub stop: AtomicBool,
    /// Set together with `stop` to request a full rescan.
    pub rescan: AtomicBool,
}

/// volume letter -> (files, folders)
type Counts = BTreeMap<u8, (u64, u64)>;

struct Sub {
    /// Lowercased, no trailing backslash. Empty = everything.
    roots: Vec<String>,
    tx: SyncSender<Response>,
}

pub struct Shared {
    pub index: RwLock<Index>,
    pub recent: Mutex<VecDeque<Hit>>,
    pub vols: Mutex<BTreeMap<char, VolumeState>>,
    pub ctl: BTreeMap<char, std::sync::Arc<VolCtl>>,
    subs: Mutex<Vec<Sub>>,
    counts: Mutex<Option<(Instant, Counts)>>,
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
            ctl: letters.iter().map(|&l| (l, Default::default())).collect(),
            subs: Mutex::new(Vec::new()),
            counts: Mutex::new(None),
            shutdown: AtomicBool::new(false),
            save_now: AtomicBool::new(false),
            started: Instant::now(),
        }
    }

    pub fn set_state(&self, letter: char, st: VolumeState) {
        self.vols.lock().unwrap().insert(letter, st);
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
    /// Changes for one volume plus the journal cursor reached after them.
    Changes { vol: u8, changes: Vec<Change>, cursor: JournalCursor },
    /// A finished fresh scan (journal cursor stored in its volume meta) replacing its volume.
    Replace { fresh: Box<Index> },
    Shutdown,
}

/// The one thread that mutates the index.
pub fn writer_loop(sh: &Shared, rx: Receiver<Msg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Changes { vol, changes, cursor } => apply_batch(sh, vol, &changes, cursor),
            Msg::Replace { fresh } => replace(sh, *fresh),
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
        for c in changes {
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
    for ev in &events {
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
    *sh.counts.lock().unwrap() = None;
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
    let counts = {
        let mut c = sh.counts.lock().unwrap();
        if c.as_ref().is_none_or(|(t, _)| t.elapsed() > COUNTS_TTL) {
            let ix = sh.index.read().unwrap();
            let mut m = Counts::new();
            for i in 0..ix.slots() as u32 {
                if ix.is_deleted(i) || ix.is_stub(i) || ix.root_of(ix.vol(i)) == Some(i) {
                    continue;
                }
                let e = m.entry(ix.vol(i)).or_default();
                if ix.is_dir(i) {
                    e.1 += 1;
                } else {
                    e.0 += 1;
                }
            }
            *c = Some((Instant::now(), m));
        }
        c.as_ref().unwrap().1.clone()
    };
    let states = sh.vols.lock().unwrap().clone();
    states
        .into_iter()
        .map(|(letter, state)| {
            let (files, folders) = counts.get(&(letter as u8)).copied().unwrap_or_default();
            VolumeStatus { letter, files, folders, state }
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
    fn create(frn: u64, parent: u64, name: &str) -> Change {
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
    fn root_matching() {
        assert!(under("c:\\docs\\a", &normalize_root("C:\\Docs\\")));
        assert!(under("c:\\docs", &normalize_root("C:/Docs")));
        assert!(!under("c:\\docsx\\a", &normalize_root("C:\\Docs")));
        assert!(under("c:\\a", &normalize_root("C:\\")));
    }
}
