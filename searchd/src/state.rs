//! Shared daemon state, the single writer thread, and the read-side queries.
//!
//! Locking: `index` is an `RwLock<Index>`. Exactly one thread (the writer) ever takes the
//! write lock, so readers (pipe connections, the snapshot saver) only contend with short
//! batches. A volume replace builds the new index under a *read* lock and takes the write
//! lock only for the pointer swap.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use winseek_index::{Index, MetaKey, Query, RankSet, VolKind};
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
/// Cached match sets (instant paging). Name orders keep a 1.8 MB bitmap however many rows
/// match; meta orders keep a sorted slot list (4 bytes per match), so those are bounded by
/// `QUERY_CACHE_BYTES` in total and everything is dropped after `QUERY_CACHE_IDLE` unused.
const QUERY_CACHE_ENTRIES: usize = 6;
const QUERY_CACHE_BYTES: usize = 96 << 20;
const QUERY_CACHE_IDLE: Duration = Duration::from_secs(90);
/// A meta-sorted result bigger than this is not sorted in full for a request near the top:
/// only the first `META_TOP` rows are (one select pass). A jump past them sorts the rest once.
const META_FULL_SORT: usize = 1_000_000;
const META_TOP: usize = 50_000;

/// Folder sizes (bytes under each folder, all levels) are recomputed at most this often while
/// the index keeps changing, and only while somebody is looking at folders.
const FOLDER_SIZES_EVERY: Duration = Duration::from_secs(20);
const FOLDER_SIZES_WANTED: Duration = Duration::from_secs(600);

/// Folder totals keyed by identity (volume + file reference number), not by record slot:
/// slots are recycled by index refreshes, an identity stays right while the table is a few
/// seconds old.
pub struct FolderSizes {
    gen: u64,
    at: Instant,
    /// Sorted by key.
    table: Vec<(u64, u64)>,
}

fn folder_key(ix: &Index, i: u32) -> u64 {
    ((ix.vol(i) as u64) << 56) | (ix.frn(i) & 0x00FF_FFFF_FFFF_FFFF)
}

impl FolderSizes {
    fn build(ix: &Index, totals: &[u64], gen: u64) -> Self {
        let mut table: Vec<(u64, u64)> = (0..totals.len() as u32)
            .filter(|&i| ix.is_dir(i) && !ix.is_deleted(i))
            .map(|i| (folder_key(ix, i), totals[i as usize]))
            .collect();
        table.sort_unstable_by_key(|e| e.0);
        FolderSizes { gen, at: Instant::now(), table }
    }
    fn get(&self, ix: &Index, i: u32) -> Option<u64> {
        let k = folder_key(ix, i);
        self.table.binary_search_by_key(&k, |e| e.0).ok().map(|p| self.table[p].1)
    }
}

/// What a query's result looks like once computed.
enum Cached {
    /// Name orders: asc and desc read the same set from either end.
    Names(RankSet),
    /// Meta orders: every match, newest / largest first (asc reads it backwards).
    Sorted(Vec<u32>),
    /// Meta orders, huge result: only the leading rows are sorted so far.
    Top { total: usize, items: Vec<u32> },
}

impl Cached {
    fn total(&self) -> usize {
        match self {
            Cached::Names(s) => s.total(),
            Cached::Sorted(v) => v.len(),
            Cached::Top { total, .. } => *total,
        }
    }
    fn bytes(&self) -> usize {
        match self {
            Cached::Names(s) => s.mem_bytes(),
            Cached::Sorted(v) | Cached::Top { items: v, .. } => v.len() * 4,
        }
    }
}

/// Sorts that share one cached result (a direction flip costs nothing).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SortClass {
    Name,
    Mtime,
    Size,
}

type CacheKey = (String, SortClass);

struct CacheEntry {
    /// Index generation the result was computed under: any mutation bumps the generation,
    /// so a stale entry never answers.
    gen: u64,
    data: Arc<Cached>,
    used: Instant,
}

#[derive(Default)]
struct QCache {
    map: HashMap<CacheKey, CacheEntry>,
}

impl QCache {
    fn get(&mut self, gen: u64, query: &str, class: SortClass) -> Option<Arc<Cached>> {
        let e = self.map.get_mut(&(query.to_string(), class))?;
        if e.gen != gen {
            return None;
        }
        e.used = Instant::now();
        Some(e.data.clone())
    }
    fn put(&mut self, gen: u64, query: &str, class: SortClass, data: Arc<Cached>) {
        self.map.retain(|_, e| e.gen == gen); // a new generation invalidates everything older
        self.map.insert((query.to_string(), class), CacheEntry { gen, data, used: Instant::now() });
        // Evict least-recently-used entries (never the only one left) over either budget.
        while self.map.len() > 1
            && (self.map.len() > QUERY_CACHE_ENTRIES || self.map.values().map(|e| e.data.bytes()).sum::<usize>() > QUERY_CACHE_BYTES)
        {
            let oldest = self.map.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()).expect("non-empty");
            self.map.remove(&oldest);
        }
    }
    fn trim_idle(&mut self) {
        self.map.retain(|_, e| e.used.elapsed() < QUERY_CACHE_IDLE);
    }
}

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
    /// Index generation: bumped by the writer after every mutation, so the query cache
    /// can tell a stale result list from a live one.
    gen: AtomicU64,
    qcache: Mutex<QCache>,
    folder_sizes: Mutex<Option<Arc<FolderSizes>>>,
    /// Seconds since `started` when a search last returned a folder (0 = never).
    folders_seen: AtomicU64,
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
            gen: AtomicU64::new(1),
            qcache: Mutex::new(QCache::default()),
            folder_sizes: Mutex::new(None),
            folders_seen: AtomicU64::new(0),
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

    /// Give back the memory of cached result lists nobody paged through lately (main loop tick).
    pub fn trim_query_cache(&self) {
        self.qcache.lock().unwrap().trim_idle();
    }

    /// Keep the folder-size table fresh (main loop tick). Does nothing while nobody has seen a
    /// folder row lately, while the table matches the index, or within `FOLDER_SIZES_EVERY`.
    pub fn refresh_folder_sizes(&self) {
        let seen = self.folders_seen.load(Ordering::Relaxed);
        if seen == 0 || self.started.elapsed().as_secs().saturating_sub(seen) > FOLDER_SIZES_WANTED.as_secs() {
            return;
        }
        let cur = self.folder_sizes.lock().unwrap().clone();
        let gen = self.gen.load(Ordering::Relaxed);
        if cur.as_ref().is_some_and(|c| c.gen == gen || c.at.elapsed() < FOLDER_SIZES_EVERY) {
            return;
        }
        let ix = self.index.read().unwrap();
        let gen = self.gen.load(Ordering::Relaxed);
        let fresh = FolderSizes::build(&ix, &ix.folder_totals(), gen);
        drop(ix);
        *self.folder_sizes.lock().unwrap() = Some(Arc::new(fresh));
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
    let mut events: Vec<Event> = Vec::new();
    // Created / modified files whose size and mtime are refined by a stat() *after* the write
    // lock is released: disk (or antivirus) latency must never stall searches.
    let mut to_stat: Vec<(u32, usize)> = Vec::new();
    {
        let mut ix = sh.index.write().unwrap();
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
            if let (Some(i), false, true) = (ev.idx, ev.is_dir, to_stat.len() < STAT_BUDGET) {
                to_stat.push((i, events.len()));
            }
            events.push(ev);
        }
        if let Some(m) = ix.volume_mut(vol) {
            m.journal_id = cursor.journal_id;
            m.next_usn = cursor.next_usn;
        }
        ix.refresh();
    }
    // Only this (writer) thread mutates the index, so the record indices stay valid.
    let stats: Vec<(u32, Option<(u64, i64)>)> = to_stat
        .iter()
        .map(|&(i, e)| {
            let md = std::fs::metadata(&events[e].path).ok();
            (i, md.map(|md| {
                let mt = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                (md.len(), mt.map_or(0, |d| crate::index_ops::filetime_from_unix_ms(d.as_millis() as i64)))
            }))
        })
        .collect();
    let mut recent: Vec<Hit> = Vec::new();
    {
        let mut ix = sh.index.write().unwrap();
        for (i, st) in stats {
            if let Some((len, ft)) = st {
                let ft = if ft != 0 { ft } else { ix.mtime(i).unwrap_or(0) };
                ix.update_meta(i, len, ft);
            }
        }
        for ev in &events {
            // (a record created and removed in the same batch has no path any more)
            if let (Some(i), true) = (ev.idx, ev.kind != ChangeKind::Deleted) {
                if ix.is_deleted(i) {
                    continue;
                }
                recent.push(hit_at(&ix, i));
            }
        }
    }
    sh.gen.fetch_add(1, Ordering::Relaxed); // the query cache must not serve this batch's old results
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
    sh.gen.fetch_add(1, Ordering::Relaxed);
    publish(sh, recent, &events);
}

fn drop_volume(sh: &Shared, vol: u8) {
    let t = Instant::now();
    {
        let mut ix = sh.index.write().unwrap();
        let old = std::mem::take(&mut *ix);
        *ix = retain_volumes(old, |v| v.letter != vol);
    }
    sh.gen.fetch_add(1, Ordering::Relaxed);
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
    sh.gen.fetch_add(1, Ordering::Relaxed);
    for l in letters {
        sh.set_state(l as char, VolumeState::Live);
    }
    sh.save_now.store(true, Ordering::Relaxed);
    info!("volume replace applied in {:.1?}", t.elapsed());
}

// ------------------------------------------------------------------ read side

/// A search row: like [`hit_at`], and a folder carries the bytes it holds when known.
fn hit_sized(ix: &Index, i: u32, sizes: Option<&FolderSizes>) -> Hit {
    let mut h = hit_at(ix, i);
    if h.is_dir {
        h.size = sizes.and_then(|s| s.get(ix, i));
    }
    h
}

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

/// Returns (total, hits, note). Every protocol sort is a real order; `PathAsc` is the one
/// gap and answers in name order with a note (logged server-side, once).
///
/// The first request of a query computes its match set once and caches it; every further
/// window - the next page or a scrollbar jump to row 8,000,000 - is a lookup, whatever the
/// size of the result. `limit == 0` only counts (the type-chip counters) and caches nothing.
pub fn search(sh: &Shared, query: &str, offset: u32, limit: u32, sort: Sort) -> (u64, Vec<Hit>, Option<&'static str>) {
    let (class, reverse, note) = match sort {
        Sort::NameAsc => (SortClass::Name, false, None),
        Sort::NameDesc => (SortClass::Name, true, None),
        Sort::MtimeDesc => (SortClass::Mtime, false, None),
        Sort::MtimeAsc => (SortClass::Mtime, true, None),
        Sort::SizeDesc => (SortClass::Size, false, None),
        Sort::SizeAsc => (SortClass::Size, true, None),
        Sort::PathAsc => (SortClass::Name, false, Some("path_asc is not supported yet; used name_asc")),
    };
    let lim = limit.min(10_000) as usize;
    let off = offset as usize;
    let ix = sh.index.read().unwrap();
    // The generation is read while holding the read lock, so it always matches the index
    // this scan is about to see (the writer bumps only after a mutation is done).
    let gen = sh.gen.load(Ordering::Relaxed);
    let cached = sh.qcache.lock().unwrap().get(gen, query, class);
    if lim == 0 {
        let total = cached.map_or_else(|| ix.count(&Query::parse(query)), |c| c.total());
        return (total as u64, Vec::new(), note);
    }
    // Can the cached result answer this window? (`Top` only covers its sorted prefix.)
    let usable = cached.filter(|c| match &**c {
        Cached::Top { items, .. } => !reverse && off + lim <= items.len(),
        _ => true,
    });
    let data = usable.unwrap_or_else(|| {
        let q = Query::parse(query);
        let fresh = Arc::new(match class {
            SortClass::Name => Cached::Names(ix.name_matches(&q)),
            SortClass::Mtime | SortClass::Size => {
                let key = if class == SortClass::Mtime { MetaKey::Mtime } else { MetaKey::Size };
                let slots = ix.match_slots(&q);
                // A size sort with folders in it ranks them by what they hold (computed for
                // exactly this index state; it also refreshes the table the rows read).
                let totals = (key == MetaKey::Size && slots.iter().any(|&i| ix.is_dir(i))).then(|| ix.folder_totals());
                if let Some(t) = &totals {
                    *sh.folder_sizes.lock().unwrap() = Some(Arc::new(FolderSizes::build(&ix, t, gen)));
                }
                let dirs = totals.as_deref();
                if slots.len() <= META_FULL_SORT || reverse || off + lim > META_TOP {
                    Cached::Sorted(ix.sort_slots(slots, key, dirs))
                } else {
                    Cached::Top { total: slots.len(), items: ix.top_slots(&slots, key, META_TOP, dirs) }
                }
            }
        });
        sh.qcache.lock().unwrap().put(gen, query, class, fresh.clone());
        fresh
    });
    let slots: Vec<u32> = match &*data {
        Cached::Names(set) => ix.name_page(set, off, lim, reverse),
        Cached::Sorted(v) | Cached::Top { items: v, .. } => {
            if reverse {
                let hi = v.len().saturating_sub(off);
                v[hi.saturating_sub(lim)..hi].iter().rev().copied().collect()
            } else {
                v[off.min(v.len())..(off + lim).min(v.len())].to_vec()
            }
        }
    };
    let has_dirs = slots.iter().any(|&i| ix.is_dir(i));
    let mut sizes = None;
    if has_dirs {
        sh.folders_seen.store(sh.started.elapsed().as_secs().max(1), Ordering::Relaxed);
        sizes = sh.folder_sizes.lock().unwrap().clone();
        if sizes.is_none() {
            // First folder row since start: compute once here so it never shows without a size.
            let fresh = Arc::new(FolderSizes::build(&ix, &ix.folder_totals(), gen));
            *sh.folder_sizes.lock().unwrap() = Some(fresh.clone());
            sizes = Some(fresh);
        }
    }
    let hits = slots.into_iter().map(|i| hit_sized(&ix, i, sizes.as_deref())).collect();
    (data.total() as u64, hits, note)
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
        assert!(search(&sh, "txt", 0, 10, Sort::SizeDesc).2.is_none(), "size sort is real now");
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
    fn meta_sorts_order_and_page_through_the_cache() {
        let sh = shared();
        // A folder-watch volume carries explicit sizes/mtimes (USN records get theirs from a later stat).
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'E', 5);
        b.volume_mut(b'E').unwrap().kind = VolKind::Folder;
        replace(&sh, b.finish());
        let up = |rel: &str, size: u64, mtime: i64| PathOp::Upsert { rel: rel.into(), is_dir: false, size, mtime };
        apply_paths(&sh, b'E', &[up(r"Ph\a.txt", 10, 100), up(r"Ph\b.txt", 900, 500), up(r"Ph\c.txt", 500, 300), up(r"Ph\d.txt", 20, 400)]);

        let names = |hits: Vec<Hit>| hits.iter().map(|h| h.name.as_str().to_string()).collect::<Vec<_>>();
        let (total, hits, note) = search(&sh, "txt", 0, 10, Sort::MtimeDesc);
        assert_eq!((total, note.is_none()), (4, true), "mtime sort is real now");
        assert_eq!(names(hits), ["b.txt", "d.txt", "c.txt", "a.txt"]);
        // the second page answers from the cache and tiles the same order exactly
        let (_, page, _) = search(&sh, "txt", 2, 2, Sort::MtimeDesc);
        assert_eq!(names(page), ["c.txt", "a.txt"]);
        let (_, hits, _) = search(&sh, "txt", 0, 10, Sort::SizeDesc);
        assert_eq!(names(hits), ["b.txt", "c.txt", "d.txt", "a.txt"]);

        // any mutation bumps the generation: the cached list is dropped, the new file leads
        apply_paths(&sh, b'E', &[up(r"Ph\e.txt", 1, 999)]);
        let (total, hits, _) = search(&sh, "txt", 0, 10, Sort::MtimeDesc);
        assert_eq!((total, hits[0].name.as_str()), (5, "e.txt"));
        // and the name orders still behave, PathAsc keeps its fallback note
        let (_, hits, _) = search(&sh, "txt", 0, 10, Sort::NameAsc);
        assert_eq!(names(hits), ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
        assert!(search(&sh, "txt", 0, 10, Sort::PathAsc).2.is_some());

        // direction flips read the same cached list backwards; name desc reads the same set from the end
        let (_, hits, _) = search(&sh, "txt", 0, 10, Sort::MtimeAsc);
        assert_eq!(names(hits), ["a.txt", "c.txt", "d.txt", "b.txt", "e.txt"]);
        let (_, hits, _) = search(&sh, "txt", 1, 2, Sort::SizeAsc);
        assert_eq!(names(hits), ["a.txt", "d.txt"]);
        let (_, hits, _) = search(&sh, "txt", 1, 3, Sort::NameDesc);
        assert_eq!(names(hits), ["d.txt", "c.txt", "b.txt"]);
        // limit 0 only counts: right total, no rows, nothing new in the cache
        let cached = sh.qcache.lock().unwrap().map.len();
        assert_eq!(search(&sh, "jpg", 0, 0, Sort::NameAsc), (0, vec![], None));
        let (total, hits, _) = search(&sh, "a.txt", 0, 0, Sort::SizeDesc);
        assert_eq!((total, hits.len()), (1, 0));
        assert_eq!(sh.qcache.lock().unwrap().map.len(), cached);
        // exact filters run in the engine: size bounds, date range (unix ms), both countable
        let (total, hits, _) = search(&sh, "txt size:>=500", 0, 10, Sort::NameAsc);
        assert_eq!((total, names(hits)), (2, vec!["b.txt".to_string(), "c.txt".to_string()]));
        assert_eq!(search(&sh, "txt size:<100", 0, 0, Sort::NameAsc).0, 3);
        // folders carry what they hold, and a size sort ranks them by it
        let (_, hits, _) = search(&sh, "Ph", 0, 10, Sort::NameAsc);
        assert_eq!((hits[0].name.as_str(), hits[0].is_dir, hits[0].size), ("Ph", true, Some(10 + 900 + 500 + 20 + 1)));
        let (_, hits, _) = search(&sh, "", 0, 3, Sort::SizeDesc);
        assert!(hits.iter().any(|h| h.name == "Ph" && h.size == Some(1431)), "{hits:?}");
        // a new file shows up in the folder's size once the table is refreshed
        apply_paths(&sh, b'E', &[up(r"Ph\f.txt", 69, 1)]);
        let (_, hits, _) = search(&sh, "folder:Ph", 0, 10, Sort::SizeDesc); // a size sort recomputes
        assert_eq!(hits[0].size, Some(1500));
        // a window past the end is empty, the total stays
        assert_eq!(search(&sh, "txt", 99, 5, Sort::NameAsc).0, 6);
        assert!(search(&sh, "txt", 99, 5, Sort::MtimeDesc).1.is_empty());
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
