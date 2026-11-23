//! Shared daemon state, the single writer thread, and the read-side queries.
//!
//! Locking: `index` is an `RwLock<Index>`. Exactly one thread (the writer) ever takes the
//! write lock, so readers (pipe connections, the snapshot saver) only contend with short
//! batches. A volume replace builds the new index under a *read* lock and takes the write
//! lock only for the pointer swap.
//!
//! Locks here are taken with `unwrap()` on purpose (fail-fast): a panicked writer would leave
//! the index inconsistent, so poisoning the lock — every later query panics too, the process
//! exits, the next start reloads the snapshot — beats silently answering from broken state.
//! Deliberate; do not add poison recovery or swap in `parking_lot`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use winseek_index::{now_filetime, Index, MetaKey, Query, RankSet, Rollup, SlotSet, VolKind};
use winseek_ipc::{
    ChangeKind, Hit, Monitor, Response, Sort, UsageDetail, UsageEntry, UsageTree, UsageType,
    VolumeState, VolumeStatus,
};
use winseek_usn::{Change, JournalCursor};

use crate::index_ops::{
    apply_change_usage, apply_path_op, filetime_to_unix_ms, map_refs_change, replace_volume,
    retain_volumes, Event, PathOp,
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
/// A Best match order slower than this is logged (a one-letter query over millions of names).
const SLOW_RANK: Duration = Duration::from_millis(50);

/// Folder sizes (bytes under each folder, all levels) are recomputed at most this often while
/// the index keeps changing, and only while somebody is looking at folders.
/// Folder-size hints on search rows are approximate: a full pass over the index is only worth it
/// every couple of minutes, and only while folder rows were shown lately.
const FOLDER_SIZES_EVERY: Duration = Duration::from_secs(120);
const FOLDER_SIZES_WANTED: Duration = Duration::from_secs(180);

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
        FolderSizes {
            gen,
            at: Instant::now(),
            table,
        }
    }
    fn get(&self, ix: &Index, i: u32) -> Option<u64> {
        let k = folder_key(ix, i);
        self.table
            .binary_search_by_key(&k, |e| e.0)
            .ok()
            .map(|p| self.table[p].1)
    }
}

/// What a query's result looks like once computed.
enum Cached {
    /// Name orders: asc and desc read the same set from either end.
    Names(RankSet),
    /// Meta orders and Best match: every match in its order (asc reads a meta order backwards).
    Sorted(Vec<u32>),
    /// Meta orders and Best match, huge result: only the leading rows are ordered so far.
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
    Type,
    /// Best match (`winseek_index::rank`): not kept live, it is ordered again on the next
    /// request after the index changes (recency and folders move).
    Relevance,
}

type CacheKey = (String, SortClass);

/// The paths a search leaves out, ready to test records against. Most records are rejected on
/// the name's byte length alone; only a record whose lowercase name is one of the excluded
/// names has its full path built and compared.
struct ExcludeSet {
    /// Normalised full paths (lowercase, `\`, no trailing separator), sorted: also the cache key.
    paths: Vec<String>,
    names: HashSet<String>,
    /// Byte lengths of the excluded names as written and lowercased (they differ only for a
    /// few non-ASCII letters), so a name of any other length is skipped without folding.
    lens: HashSet<usize>,
}

impl ExcludeSet {
    fn new(raw: &[String]) -> Self {
        let mut paths: Vec<String> = raw
            .iter()
            .map(|p| normalize_root(p))
            .filter(|p| !p.is_empty())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        let mut names = HashSet::new();
        let mut lens = HashSet::new();
        for p in raw {
            let p = p.trim_end_matches(['\\', '/']);
            let name = p.rsplit(['\\', '/']).next().unwrap_or(p);
            lens.insert(name.len());
            let lower = name.to_lowercase();
            lens.insert(lower.len());
            names.insert(lower);
        }
        ExcludeSet { paths, names, lens }
    }

    fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    fn holds(&self, ix: &Index, i: u32) -> bool {
        let name = ix.name(i);
        self.lens.contains(&name.len())
            && self.names.contains(&name.to_lowercase())
            && self
                .paths
                .binary_search(&normalize_root(&ix.full_path(i)))
                .is_ok()
    }
}

/// Positions (ascending, in the stream read forwards) of the excluded records in one cached
/// result, kept for the next window: the picture grid pages chunk after chunk of the same
/// stream with the same exclude list, and finding them is a pass over every match.
struct Excluded {
    gen: u64,
    query: String,
    class: SortClass,
    paths: Vec<String>,
    at: Arc<Vec<usize>>,
}

/// Rows read per step while scanning a name result for excluded records (bounds the scratch).
const EXCLUDE_SCAN_PAGE: usize = 1 << 16;

fn excluded_positions(ix: &Index, data: &Cached, ex: &ExcludeSet) -> Vec<usize> {
    let mut at = Vec::new();
    match data {
        Cached::Names(set) => {
            let mut base = 0;
            while base < set.total() {
                let page = ix.name_page(set, base, EXCLUDE_SCAN_PAGE, false);
                if page.is_empty() {
                    break;
                }
                at.extend(
                    page.iter()
                        .enumerate()
                        .filter(|&(_, &i)| ex.holds(ix, i))
                        .map(|(p, _)| base + p),
                );
                base += page.len();
            }
        }
        Cached::Sorted(v) | Cached::Top { items: v, .. } => {
            at.extend(
                v.iter()
                    .enumerate()
                    .filter(|&(_, &i)| ex.holds(ix, i))
                    .map(|(p, _)| p),
            );
        }
    }
    at
}

struct CacheEntry {
    /// Index generation the result answers for: any mutation bumps the generation, so a
    /// stale entry never answers. A name-order entry with `live` is carried to the new
    /// generation by [`QCache::patch`] instead of being dropped.
    gen: u64,
    data: Arc<Cached>,
    live: Option<Live>,
    used: Instant,
}

/// A name-order result kept current across USN batches, as Everything keeps its open
/// queries: the matches by slot are re-tested only for the records a batch touched, and the
/// name-order set is rebuilt from them (bit lookups, no matching) when next paged. Without it
/// every page request after a change re-ran the query over the whole index on every core.
struct Live {
    q: Query,
    set: SlotSet,
    /// `data` was derived from `set` under the current generation.
    fresh: bool,
}

#[derive(Default)]
struct QCache {
    map: HashMap<CacheKey, CacheEntry>,
}

impl QCache {
    fn get(&mut self, gen: u64, query: &str, class: SortClass, ix: &Index) -> Option<Arc<Cached>> {
        let e = self.map.get_mut(&(query.to_string(), class))?;
        if e.gen != gen {
            return None;
        }
        if let Some(l) = e.live.as_mut().filter(|l| !l.fresh) {
            e.data = Arc::new(Cached::Names(ix.ranks_of(&l.set)));
            l.fresh = true;
        }
        e.used = Instant::now();
        Some(e.data.clone())
    }
    fn put(
        &mut self,
        gen: u64,
        query: &str,
        class: SortClass,
        data: Arc<Cached>,
        live: Option<Live>,
    ) {
        self.map.retain(|_, e| e.gen == gen); // a new generation invalidates everything older
        self.map.insert(
            (query.to_string(), class),
            CacheEntry {
                gen,
                data,
                live,
                used: Instant::now(),
            },
        );
        // Evict least-recently-used entries (never the only one left) over either budget.
        while self.map.len() > 1
            && (self.map.len() > QUERY_CACHE_ENTRIES
                || self.map.values().map(CacheEntry::bytes).sum::<usize>() > QUERY_CACHE_BYTES)
        {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone())
                .expect("non-empty");
            self.map.remove(&oldest);
        }
    }
    fn trim_idle(&mut self) {
        self.map.retain(|_, e| e.used.elapsed() < QUERY_CACHE_IDLE);
    }
    /// A batch moved the index from `from` to `to` and touched `touched`: carry the live
    /// entries over (re-test just those records), drop the rest. A renamed or moved folder
    /// changes the path of everything below it, so entries with a path term go then too.
    fn patch(&mut self, from: u64, to: u64, ix: &Index, touched: &[u32], folder_renamed: bool) {
        self.map.retain(|_, e| {
            let Some(l) = e.live.as_mut() else {
                return false;
            };
            if e.gen != from || (folder_renamed && l.q.path_sensitive()) {
                return false;
            }
            for &i in touched {
                l.set.set(i, ix.slot_matches(&l.q, i));
            }
            l.fresh = false;
            e.gen = to;
            true
        });
    }
}

impl CacheEntry {
    fn bytes(&self) -> usize {
        self.data.bytes() + self.live.as_ref().map_or(0, |l| l.set.mem_bytes())
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
    usage_cache: Mutex<Option<Arc<UsageCache>>>,
    usage_build: Mutex<()>,
    usage_seen: AtomicU64,
    /// Where the last exclude list sits in its result stream (see [`search_excluding`]).
    excluded: Mutex<Option<Excluded>>,
    /// Per-task work counters, logged and reset by the main loop every minute.
    pub stats: Stats,
}

/// What the daemon spent its time on since the last report: one line per minute in the log, so a
/// CPU complaint can be answered from numbers (writer batches, searches, roll-up and
/// folder-size rebuilds).
#[derive(Default)]
pub struct Stats {
    batches: AtomicU64,
    changes: AtomicU64,
    batch_us: AtomicU64,
    searches: AtomicU64,
    search_us: AtomicU64,
    /// Best-match orders built (one per query and index change), and their time.
    ranked: AtomicU64,
    rank_ms: AtomicU64,
    usage_builds: AtomicU64,
    usage_ms: AtomicU64,
    folder_builds: AtomicU64,
    folder_ms: AtomicU64,
}

impl Stats {
    pub fn search(&self, d: Duration) {
        self.searches.fetch_add(1, Ordering::Relaxed);
        self.search_us
            .fetch_add(d.as_micros() as u64, Ordering::Relaxed);
    }
    fn ranked(&self, d: Duration) {
        self.ranked.fetch_add(1, Ordering::Relaxed);
        self.rank_ms
            .fetch_add(d.as_millis() as u64, Ordering::Relaxed);
    }
    fn batch(&self, changes: usize, d: Duration) {
        self.batches.fetch_add(1, Ordering::Relaxed);
        self.changes.fetch_add(changes as u64, Ordering::Relaxed);
        self.batch_us
            .fetch_add(d.as_micros() as u64, Ordering::Relaxed);
    }
    fn usage_build(&self, d: Duration) {
        self.usage_builds.fetch_add(1, Ordering::Relaxed);
        self.usage_ms
            .fetch_add(d.as_millis() as u64, Ordering::Relaxed);
    }
    fn folder_build(&self, d: Duration) {
        self.folder_builds.fetch_add(1, Ordering::Relaxed);
        self.folder_ms
            .fetch_add(d.as_millis() as u64, Ordering::Relaxed);
    }
    /// A one-line summary of the work since the last call (counters reset); `None` when idle.
    pub fn take_line(&self) -> Option<String> {
        let t = |c: &AtomicU64| c.swap(0, Ordering::Relaxed);
        let (b, ch, bus) = (t(&self.batches), t(&self.changes), t(&self.batch_us));
        let (s, sus) = (t(&self.searches), t(&self.search_us));
        let (ub, ums) = (t(&self.usage_builds), t(&self.usage_ms));
        let (fb, fms) = (t(&self.folder_builds), t(&self.folder_ms));
        let (rk, rms) = (t(&self.ranked), t(&self.rank_ms));
        if b + s + ub + fb == 0 {
            return None;
        }
        Some(format!(
            "writer: {b} batches / {ch} changes / {} ms; searches: {s} / {} ms; usage roll-ups: {ub} / {ums} ms; folder sizes: {fb} / {fms} ms; best-match orders: {rk} / {rms} ms",
            bus / 1000,
            sus / 1000
        ))
    }
}

impl Shared {
    pub fn new(index: Index, letters: &[char]) -> Self {
        Shared {
            index: RwLock::new(index),
            recent: Mutex::new(VecDeque::with_capacity(RECENT_CAP)),
            vols: Mutex::new(
                letters
                    .iter()
                    .map(|&l| (l, VolumeState::Scanning { done: 0, total: 0 }))
                    .collect(),
            ),
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
            usage_cache: Mutex::new(None),
            usage_build: Mutex::new(()),
            usage_seen: AtomicU64::new(0),
            excluded: Mutex::new(None),
            stats: Stats::default(),
        }
    }

    /// One pass over every lock a request needs, in the order requests take them. `stage` says which
    /// one the probe is waiting on (1 vols, 2 info, 3 index read, 4 query cache, 5 usage cache,
    /// 6 usage build, 7 folder sizes, 8 ctl), 0 once the pass is through. The watchdog reads it to
    /// name the culprit when a probe never finishes.
    pub fn probe_locks(&self, stage: &std::sync::atomic::AtomicU8) {
        stage.store(1, Ordering::SeqCst);
        drop(self.vols.lock());
        stage.store(2, Ordering::SeqCst);
        drop(self.info.lock());
        stage.store(3, Ordering::SeqCst);
        drop(self.index.read());
        stage.store(4, Ordering::SeqCst);
        drop(self.qcache.lock());
        stage.store(5, Ordering::SeqCst);
        drop(self.usage_cache.lock());
        stage.store(6, Ordering::SeqCst);
        drop(self.usage_build.lock());
        stage.store(7, Ordering::SeqCst);
        drop(self.folder_sizes.lock());
        stage.store(8, Ordering::SeqCst);
        drop(self.ctl.lock());
        stage.store(0, Ordering::SeqCst);
    }

    pub fn set_state(&self, letter: char, st: VolumeState) {
        self.vols.lock().unwrap().insert(letter, st);
    }

    pub fn set_info(&self, letter: char, fs: &str, monitor: Monitor) {
        self.info.lock().unwrap().insert(
            letter,
            VolInfo {
                fs: fs.to_string(),
                monitor,
            },
        );
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
        if seen == 0
            || self.started.elapsed().as_secs().saturating_sub(seen) > FOLDER_SIZES_WANTED.as_secs()
        {
            return;
        }
        let cur = self.folder_sizes.lock().unwrap().clone();
        let gen = self.gen.load(Ordering::Relaxed);
        if cur
            .as_ref()
            .is_some_and(|c| c.gen == gen || c.at.elapsed() < FOLDER_SIZES_EVERY)
        {
            return;
        }
        let ix = self.index.read().unwrap();
        let gen = self.gen.load(Ordering::Relaxed);
        let t = Instant::now();
        let fresh = FolderSizes::build(&ix, &ix.folder_totals(), gen);
        drop(ix);
        self.stats.folder_build(t.elapsed());
        *self.folder_sizes.lock().unwrap() = Some(Arc::new(fresh));
    }

    /// Index generation: moves on every change to the index (the snapshot saver compares it).
    pub fn generation(&self) -> u64 {
        self.gen.load(Ordering::Relaxed)
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

    /// Tell every subscriber (whatever its roots) that the index changed without per-file events:
    /// a rescan swapped a volume in, or a drive was dropped. A full queue drops the subscriber,
    /// which resyncs on reconnect anyway.
    pub fn notify_resync(&self) {
        self.subs
            .lock()
            .unwrap()
            .retain(|s| match s.tx.try_send(Response::Resync) {
                Ok(()) => true,
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    warn!("dropping a subscriber whose queue overflowed");
                    false
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => false,
            });
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
                || s.roots
                    .iter()
                    .any(|r| under(&path, r) || old.as_deref().is_some_and(|o| under(o, r)));
            if !hit {
                return true;
            }
            let msg = Response::Change {
                kind: ev.kind,
                path: ev.path.clone(),
                old_path: ev.old_path.clone(),
                is_dir: ev.is_dir,
            };
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
    r.trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}

fn under(path_lower: &str, root: &str) -> bool {
    path_lower
        .strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
}

pub enum Msg {
    /// Changes for one volume plus the journal cursor reached after them. ReFS changes
    /// carry 128-bit FRNs and are interned by the writer.
    Changes {
        vol: u8,
        changes: Vec<Change>,
        cursor: JournalCursor,
    },
    /// Folder-watch changes (FAT/exFAT/ReFS without a journal), already stat'ed.
    Paths {
        vol: u8,
        ops: Vec<PathOp>,
    },
    /// A finished fresh scan (journal cursor stored in its volume meta) replacing its volume.
    Replace {
        fresh: Box<Index>,
    },
    /// The volume went away: drop its records.
    Drop {
        vol: u8,
    },
    Shutdown,
}

/// The one thread that mutates the index.
pub fn writer_loop(sh: &Shared, rx: Receiver<Msg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Changes {
                vol,
                changes,
                cursor,
            } => apply_batch(sh, vol, &changes, cursor),
            Msg::Paths { vol, ops } => apply_paths(sh, vol, &ops),
            Msg::Replace { fresh } => replace(sh, *fresh),
            Msg::Drop { vol } => drop_volume(sh, vol),
            Msg::Shutdown => break,
        }
    }
}

/// How long a writer polls before it falls back to a blocking (queueing) write. A long read is a
/// few seconds at most; USN changes simply land that much later.
const WRITE_PATIENCE: Duration = Duration::from_secs(10);

/// Take the index write lock *without queueing behind a long reader*. std's RwLock prefers
/// writers: a writer blocked in `write()` makes every new `read()` wait, so a USN batch that
/// arrives during a multi-second read (usage roll-up, folder totals) would freeze all searches
/// until that read ends. Polling `try_write` never marks a writer as waiting; searches keep
/// running. After `WRITE_PATIENCE` it blocks normally so the writer cannot starve.
pub(crate) fn write_index(sh: &Shared) -> std::sync::RwLockWriteGuard<'_, Index> {
    use std::sync::TryLockError;
    let t = Instant::now();
    loop {
        match sh.index.try_write() {
            Ok(g) => return g,
            Err(TryLockError::WouldBlock) if t.elapsed() < WRITE_PATIENCE => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(TryLockError::WouldBlock) => {
                warn!("index writer waited {:.0?}; queueing", t.elapsed());
                return sh.index.write().unwrap();
            }
            Err(TryLockError::Poisoned(_)) => return sh.index.write().unwrap(), // same panic as before
        }
    }
}

fn apply_batch(sh: &Shared, vol: u8, changes: &[Change], cursor: JournalCursor) {
    let started = Instant::now();
    let mut events: Vec<Event> = Vec::new();
    // Created / modified files whose size and mtime are refined by a stat() *after* the write
    // lock is released: disk (or antivirus) latency must never stall searches.
    let mut to_stat: Vec<(u32, usize)> = Vec::new();
    {
        let mut ix = write_index(sh);
        // Lock order everywhere: index, then the roll-up. Builds install under the index read
        // lock, so no change can fall between a build and its overlay.
        let usage = sh.usage_cache.lock().unwrap().clone();
        let mut ru = usage.as_ref().map(|c| c.ru.write().unwrap());
        let refs = ix.kind_of(vol) == Some(VolKind::Refs);
        for c in changes {
            let mapped;
            let c = if refs {
                mapped = map_refs_change(&mut ix, vol, c);
                &mapped
            } else {
                c
            };
            let Some(ev) = apply_change_usage(&mut ix, vol, c, ru.as_deref_mut()) else {
                continue;
            };
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
            (
                i,
                md.map(|md| {
                    let mt = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                    (
                        md.len(),
                        mt.map_or(0, |d| {
                            crate::index_ops::filetime_from_unix_ms(d.as_millis() as i64)
                        }),
                    )
                }),
            )
        })
        .collect();
    let mut recent: Vec<Hit> = Vec::new();
    {
        let mut ix = write_index(sh);
        let usage = sh.usage_cache.lock().unwrap().clone();
        let mut ru = usage.as_ref().map(|c| c.ru.write().unwrap());
        for (i, st) in stats {
            if let Some((len, ft)) = st {
                let ft = if ft != 0 {
                    ft
                } else {
                    ix.mtime(i).unwrap_or(0)
                };
                let t = ru.as_deref().map(|ru| ix.usage_before(ru, &[i]));
                ix.update_meta(i, len, ft);
                if let (Some(ru), Some(t)) = (ru.as_deref_mut(), t) {
                    ix.usage_after(ru, t, &[]);
                }
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
        // Still under the write lock, so no search sees the new index under the old
        // generation: carry the cached name results over (re-test only what this batch
        // touched), then bump, so the query cache never serves this batch's old results.
        let touched: Vec<u32> = events.iter().filter_map(|e| e.idx).collect();
        let folder_renamed = events
            .iter()
            .any(|e| e.is_dir && e.kind == ChangeKind::Renamed);
        let g = sh.gen.load(Ordering::Relaxed);
        sh.qcache
            .lock()
            .unwrap()
            .patch(g, g + 1, &ix, &touched, folder_renamed);
        sh.gen.fetch_add(1, Ordering::Relaxed);
        // The roll-up's overlay saw every change of this batch: it is still in step with the index.
        // (Any other writer bumps `gen` without this, which leaves the roll-up behind.)
        // `usage` is the cache this batch just updated through `ru` (it cannot change while the
        // index write lock is held: builds install under the read lock). Do NOT lock the
        // `usage_cache` mutex here: `ru` is held, and readers take that mutex before `ru`.
        if let Some(c) = usage.as_ref() {
            let _ = c
                .synced
                .compare_exchange(g, g + 1, Ordering::Relaxed, Ordering::Relaxed);
        }
    }
    publish(sh, recent, &events);
    sh.stats.batch(changes.len(), started.elapsed());
}

/// Apply stat'ed folder-watch ops (no I/O under the lock).
fn apply_paths(sh: &Shared, vol: u8, ops: &[PathOp]) {
    let mut recent: Vec<Hit> = Vec::new();
    let mut events: Vec<Event> = Vec::new();
    {
        let mut ix = write_index(sh);
        if ix.root_of(vol).is_none() {
            return; // not scanned yet (or dropped): the scan/rescan covers these
        }
        for op in ops {
            let Some(ev) = apply_path_op(&mut ix, vol, op) else {
                continue;
            };
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
        let mut ix = write_index(sh);
        let old = std::mem::take(&mut *ix);
        *ix = retain_volumes(old, |v| v.letter != vol);
    }
    sh.gen.fetch_add(1, Ordering::Relaxed);
    sh.notify_resync();
    let prefix = format!("{}:\\", vol as char).to_lowercase();
    sh.recent
        .lock()
        .unwrap()
        .retain(|h| !h.path.to_lowercase().starts_with(&prefix));
    sh.forget(vol as char);
    sh.save_now.store(true, Ordering::Relaxed);
    info!(
        "{}: volume dropped from the index in {:.1?}",
        vol as char,
        t.elapsed()
    );
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
    let old = std::mem::replace(&mut *write_index(sh), new);
    drop(old);
    sh.gen.fetch_add(1, Ordering::Relaxed);
    sh.notify_resync();
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

/// Totals of `query` per type chip, in the order of `kinds` (index clauses). One index pass
/// answers all of them; `Query::parse` accepts the empty clause ("" counts everything).
pub fn kind_counts(sh: &Shared, query: &str, kinds: &[String]) -> Vec<u64> {
    let ix = sh.index.read().unwrap();
    let q = Query::parse(query);
    let parsed: Vec<Query> = kinds.iter().map(|k| Query::parse(k)).collect();
    ix.count_kinds(&q, &parsed)
        .into_iter()
        .map(|n| n as u64)
        .collect()
}

/// [`search_excluding`] with nothing excluded (the tests' entry point).
#[cfg(test)]
pub fn search(
    sh: &Shared,
    query: &str,
    offset: u32,
    limit: u32,
    sort: Sort,
) -> (u64, Vec<Hit>, Option<&'static str>) {
    search_excluding(sh, query, offset, limit, sort, &[])
}

/// Returns (total, hits, note). Every protocol sort is a real order; `PathAsc` is the one
/// gap and answers in name order with a note (logged server-side, once).
///
/// The first request of a query computes its match set once and caches it; every further
/// window - the next page or a scrollbar jump to row 8,000,000 - is a lookup, whatever the
/// size of the result. `limit == 0` only counts (the type-chip counters) and caches nothing.
///
/// The `exclude` paths are taken out of the stream first: `total` and every offset count only
/// what remains, so a client can show some records elsewhere (the picture grid's meaning
/// matches) and still address the rest by exact position.
///
/// `Sort::Relevance` is Best match (`winseek_index::rank`). A query with nothing typed to rank
/// by (a filter tab with an empty box, a pasted path, wildcards only) is the name search it
/// always was.
pub fn search_excluding(
    sh: &Shared,
    query: &str,
    offset: u32,
    limit: u32,
    sort: Sort,
    exclude: &[String],
) -> (u64, Vec<Hit>, Option<&'static str>) {
    let ex = ExcludeSet::new(exclude);
    let q = Query::parse(query);
    let (class, reverse, note) = match sort {
        Sort::Relevance if q.ranks_by_name() => (SortClass::Relevance, false, None),
        Sort::Relevance => (SortClass::Name, false, None),
        Sort::NameAsc => (SortClass::Name, false, None),
        Sort::NameDesc => (SortClass::Name, true, None),
        Sort::MtimeDesc => (SortClass::Mtime, false, None),
        Sort::MtimeAsc => (SortClass::Mtime, true, None),
        Sort::SizeDesc => (SortClass::Size, false, None),
        Sort::SizeAsc => (SortClass::Size, true, None),
        Sort::TypeAsc => (SortClass::Type, false, None),
        Sort::TypeDesc => (SortClass::Type, true, None),
        Sort::PathAsc => (
            SortClass::Name,
            false,
            Some("path_asc is not supported yet; used name_asc"),
        ),
    };
    let lim = limit.min(10_000) as usize;
    let off = offset as usize;
    let ix = sh.index.read().unwrap();
    // The generation is read while holding the read lock, so it always matches the index
    // this scan is about to see (the writer bumps only after a mutation is done).
    let gen = sh.gen.load(Ordering::Relaxed);
    let cached = sh.qcache.lock().unwrap().get(gen, query, class, &ix);
    if lim == 0 && ex.is_empty() {
        let total = cached.map_or_else(|| ix.count(&q), |c| c.total());
        return (total as u64, Vec::new(), note);
    }
    // Can the cached result answer this window? (`Top` only covers its sorted prefix.)
    let usable = cached.filter(|c| match &**c {
        // Excluded records may sit past the sorted prefix, so an exclude needs the full order.
        Cached::Top { items, .. } => ex.is_empty() && !reverse && off + lim <= items.len(),
        _ => true,
    });
    let data = usable.unwrap_or_else(|| {
        let mut live = None;
        let fresh = Arc::new(match class {
            // Every query but the empty one (all ranks, nothing to scan) is kept live.
            SortClass::Name if !q.terms.is_empty() => {
                let (ranks, set) = ix.name_matches_live(&q);
                live = Some(Live {
                    q: q.clone(),
                    set,
                    fresh: true,
                });
                Cached::Names(ranks)
            }
            SortClass::Name => Cached::Names(ix.name_matches(&q)),
            SortClass::Relevance => {
                let t = Instant::now();
                let slots = ix.match_slots(&q);
                let keys = ix.rank_keys(&q, &slots, now_filetime());
                let full = slots.len() <= META_FULL_SORT || off + lim > META_TOP || !ex.is_empty();
                let data = if full {
                    Cached::Sorted(Index::sort_pairs(keys))
                } else {
                    Cached::Top {
                        total: slots.len(),
                        items: Index::top_pairs(keys, META_TOP),
                    }
                };
                let d = t.elapsed();
                sh.stats.ranked(d);
                if d > SLOW_RANK {
                    info!(
                        "best match: ordered {} matches for {query:?} in {} ms",
                        slots.len(),
                        d.as_millis()
                    );
                }
                data
            }
            SortClass::Mtime | SortClass::Size | SortClass::Type => {
                let key = match class {
                    SortClass::Mtime => MetaKey::Mtime,
                    SortClass::Size => MetaKey::Size,
                    _ => MetaKey::Ext,
                };
                let slots = ix.match_slots(&q);
                // A size sort with folders in it ranks them by what they hold (computed for
                // exactly this index state; it also refreshes the table the rows read).
                let totals = (key == MetaKey::Size && slots.iter().any(|&i| ix.is_dir(i)))
                    .then(|| ix.folder_totals());
                if let Some(t) = &totals {
                    *sh.folder_sizes.lock().unwrap() =
                        Some(Arc::new(FolderSizes::build(&ix, t, gen)));
                }
                let dirs = totals.as_deref();
                if slots.len() <= META_FULL_SORT
                    || reverse
                    || off + lim > META_TOP
                    || !ex.is_empty()
                {
                    Cached::Sorted(ix.sort_slots(slots, key, dirs))
                } else {
                    Cached::Top {
                        total: slots.len(),
                        items: ix.top_slots(&slots, key, META_TOP, dirs),
                    }
                }
            }
        });
        sh.qcache
            .lock()
            .unwrap()
            .put(gen, query, class, fresh.clone(), live);
        fresh
    });
    let page = |off: usize, lim: usize| -> Vec<u32> {
        match &*data {
            Cached::Names(set) => ix.name_page(set, off, lim, reverse),
            Cached::Sorted(v) | Cached::Top { items: v, .. } => {
                if reverse {
                    let hi = v.len().saturating_sub(off);
                    v[hi.saturating_sub(lim)..hi]
                        .iter()
                        .rev()
                        .copied()
                        .collect()
                } else {
                    v[off.min(v.len())..(off + lim).min(v.len())].to_vec()
                }
            }
        }
    };
    let (total, slots) = if ex.is_empty() {
        (data.total(), page(off, lim))
    } else {
        let n = data.total();
        let at = exclusion_at(sh, &ix, gen, query, class, &data, &ex);
        // Positions in the direction being read, ascending.
        let at: Vec<usize> = if reverse {
            at.iter().rev().map(|&p| n - 1 - p).collect()
        } else {
            at.to_vec()
        };
        // The raw position of the `off`-th kept row: every excluded row at or before it
        // pushes it one further.
        let mut raw = off;
        let mut k = 0;
        while k < at.len() && at[k] <= raw {
            raw += 1;
            k += 1;
        }
        let ahead = &at[k..];
        let slots = if lim == 0 {
            Vec::new()
        } else {
            // Read enough raw rows to cover the excluded ones still ahead, then drop them.
            page(raw, lim + ahead.len())
                .into_iter()
                .enumerate()
                .filter(|&(j, _)| ahead.binary_search(&(raw + j)).is_err())
                .map(|(_, i)| i)
                .take(lim)
                .collect()
        };
        (n.saturating_sub(at.len()), slots)
    };
    let has_dirs = slots.iter().any(|&i| ix.is_dir(i));
    let mut sizes = None;
    if has_dirs {
        sh.folders_seen
            .store(sh.started.elapsed().as_secs().max(1), Ordering::Relaxed);
        sizes = sh.folder_sizes.lock().unwrap().clone();
        if sizes.is_none() {
            // First folder row since start: compute once here so it never shows without a size.
            let fresh = Arc::new(FolderSizes::build(&ix, &ix.folder_totals(), gen));
            *sh.folder_sizes.lock().unwrap() = Some(fresh.clone());
            sizes = Some(fresh);
        }
    }
    let hits = slots
        .into_iter()
        .map(|i| hit_sized(&ix, i, sizes.as_deref()))
        .collect();
    (total as u64, hits, note)
}

/// The excluded records' positions in `data`: from the last request when it asked the same
/// thing of the same index state, else one scan of the result (kept for the next window).
fn exclusion_at(
    sh: &Shared,
    ix: &Index,
    gen: u64,
    query: &str,
    class: SortClass,
    data: &Cached,
    ex: &ExcludeSet,
) -> Arc<Vec<usize>> {
    {
        let last = sh.excluded.lock().unwrap();
        if let Some(e) = &*last {
            if e.gen == gen && e.class == class && e.query == query && e.paths == ex.paths {
                return e.at.clone();
            }
        }
    }
    let at = Arc::new(excluded_positions(ix, data, ex));
    *sh.excluded.lock().unwrap() = Some(Excluded {
        gen,
        query: query.to_string(),
        class,
        paths: ex.paths.clone(),
        at: at.clone(),
    });
    at
}

pub fn status(sh: &Shared) -> Vec<VolumeStatus> {
    let states = sh.vols.lock().unwrap().clone();
    let info = sh.info.lock().unwrap().clone();
    let ix = sh.index.read().unwrap();
    states
        .into_iter()
        .map(|(letter, state)| {
            let (files, folders) = ix.counts(letter as u8);
            let (fs, monitor) = info
                .get(&letter)
                .map_or((String::new(), Monitor::Usn), |i| (i.fs.clone(), i.monitor));
            VolumeStatus {
                letter,
                files,
                folders,
                state,
                fs,
                monitor,
            }
        })
        .collect()
}

pub fn recent(sh: &Shared, limit: u32) -> Vec<Hit> {
    sh.recent
        .lock()
        .unwrap()
        .iter()
        .take(limit.min(500) as usize)
        .cloned()
        .collect()
}

/// Biggest children listed per folder when the caller names no limit; the rest is summed into `more`.
const USAGE_ENTRIES: usize = 400;
/// How long a roll-up stays good even while the index changes: the floor between rebuilds, so a
/// churn burst (saves, builds, browser caches) cannot trigger one per index batch. Once the index
/// moved on and this floor has passed, the very next look rebuilds — a recycle shows up within
/// seconds, not minutes.
const USAGE_FRESH: Duration = Duration::from_secs(5);
/// The roll-up is only kept (and rebuilt) while somebody asked for it this recently.
const USAGE_WANTED: Duration = Duration::from_secs(120);
/// While the totals are exact (the overlay tracks every change) a rebuild is only for the child
/// lists, which merely lack records added since: new files from constant churn (logs, queues)
/// must not trigger a full pass over the index every few seconds.
const USAGE_LISTS_EVERY: Duration = Duration::from_secs(60);

/// Folder totals for the disk-usage view, built once from the index and reused for every click.
/// The writer keeps its totals exact as the index changes (`Rollup`'s overlay, fed by
/// `apply_batch`); only its child lists wait for the next build to show new records.
pub struct UsageCache {
    /// Index generation the snapshot was built at.
    gen: u64,
    /// Generation the overlay is in step with: `apply_batch` moves it along with `gen`, any
    /// other writer (folder-watch ops, volume replace/drop) leaves it behind.
    synced: AtomicU64,
    at: Instant,
    ru: RwLock<Rollup>,
}

impl UsageCache {
    fn new(gen: u64, ru: Rollup) -> Self {
        UsageCache {
            gen,
            synced: AtomicU64::new(gen),
            at: Instant::now(),
            ru: RwLock::new(ru),
        }
    }
    /// Totals are exact for index generation `gen`.
    fn exact(&self, gen: u64) -> bool {
        self.synced.load(Ordering::Relaxed) == gen && self.ru.read().unwrap().exact()
    }
}

impl Shared {
    fn note_usage_use(&self) {
        self.usage_seen
            .store(self.started.elapsed().as_secs().max(1), Ordering::Relaxed);
    }

    /// A roll-up may be served as is: its totals are exact (child lists may lag additions until
    /// the background rebuild), or the build is young enough that the next rebuild is not due
    /// yet (see `USAGE_FRESH`).
    fn usage_usable(c: &UsageCache, gen: u64) -> bool {
        c.gen == gen || c.exact(gen) || c.at.elapsed() < USAGE_FRESH
    }

    /// Build a roll-up and install it while still holding the index read lock: the writer
    /// cannot slip a change in between, so nothing is lost from or doubled in the overlay.
    fn usage_build_now(&self) -> Result<Arc<UsageCache>, String> {
        let ix = self.index.read().unwrap();
        if !ix.has_meta() {
            return Err("The name index carries no sizes yet.".into());
        }
        let t = Instant::now();
        let gen = self.gen.load(Ordering::Relaxed);
        let ru = ix.rollup();
        self.stats.usage_build(t.elapsed());
        info!(
            "disk usage roll-up: {} folders, {} MB, {:.0?}",
            ru.dir_count(),
            ru.mem_bytes() >> 20,
            t.elapsed()
        );
        let c = Arc::new(UsageCache::new(gen, ru));
        *self.usage_cache.lock().unwrap() = Some(c.clone());
        drop(ix);
        Ok(c)
    }

    /// The roll-up: the cached one, or a fresh build when the cache fell behind (an untracked
    /// change) past the floor, so the request that asked after a change gets a right answer.
    /// Concurrent builds share one pass.
    fn usage_cache(&self) -> Result<Arc<UsageCache>, String> {
        self.note_usage_use();
        // The mutex guard must be gone before `usage_usable` runs: it takes the roll-up's own lock
        // (`ru`), and the writer holds `ru` while it wants this mutex. (A guard in an `if let`
        // header lives for the whole body: that was the deadlock that wedged searchd.)
        let cur = self.usage_cache.lock().unwrap().clone();
        if let Some(c) = cur {
            if Self::usage_usable(&c, self.gen.load(Ordering::Relaxed)) {
                return Ok(c);
            }
        }
        let _one = self.usage_build.lock().unwrap();
        let cur = self.usage_cache.lock().unwrap().clone();
        if let Some(c) = cur {
            if Self::usage_usable(&c, self.gen.load(Ordering::Relaxed)) {
                return Ok(c);
            }
        }
        self.usage_build_now()
    }

    /// Main loop tick: rebuild a roll-up that fell behind, or whose child lists miss records
    /// added since (throttled by `USAGE_FRESH`), while somebody is looking; drop it when nobody is.
    pub fn refresh_usage(&self) {
        let Some(cur) = self.usage_cache.lock().unwrap().clone() else {
            return;
        };
        let seen = self.usage_seen.load(Ordering::Relaxed);
        if self.started.elapsed().as_secs().saturating_sub(seen) > USAGE_WANTED.as_secs() {
            *self.usage_cache.lock().unwrap() = None;
            return;
        }
        let gen = self.gen.load(Ordering::Relaxed);
        let lists_lag = cur.ru.read().unwrap().structure_dirty();
        let floor = if cur.exact(gen) {
            USAGE_LISTS_EVERY
        } else {
            USAGE_FRESH
        };
        if cur.at.elapsed() < floor || (cur.exact(gen) && !lists_lag) {
            return;
        }
        let Ok(_one) = self.usage_build.try_lock() else {
            return;
        };
        let _ = self.usage_build_now();
    }
}

fn drive_label(letter: u8) -> String {
    format!("{}:", (letter as char).to_ascii_uppercase())
}

/// What the folder `path` holds, child by child; an empty `path` lists the drives.
pub fn usage(sh: &Shared, path: &str, limit: u32) -> Result<UsageTree, String> {
    let c = sh.usage_cache()?;
    let ix = sh.index.read().unwrap();
    // (under the index lock: the overlay and the index are in step here)
    let gen = sh.gen.load(Ordering::Relaxed);
    let fresh = c.gen == gen || c.exact(gen);
    let ru = c.ru.read().unwrap();
    let ru = &*ru;
    if path.trim().is_empty() {
        let mut t = UsageTree {
            path: String::new(),
            size: 0,
            files: 0,
            folders: 0,
            entries: vec![],
            more: 0,
            more_size: 0,
            cats: vec![0; winseek_index::CATEGORIES],
            fresh,
        };
        for (letter, d) in ix.volume_dirs(ru) {
            let (bytes, files, folders) = ru.totals(d);
            let cats = ru.cats_of(d);
            let kind = (0..cats.len())
                .max_by_key(|&k| (cats[k], std::cmp::Reverse(k)))
                .unwrap_or(0) as u8;
            for (k, b) in cats.iter().enumerate() {
                t.cats[k] += b;
            }
            t.size += bytes;
            t.files += files;
            t.folders += folders;
            let name = drive_label(letter);
            t.entries.push(UsageEntry {
                path: format!("{name}\\"),
                name,
                is_dir: true,
                kind,
                size: bytes,
                files,
                folders,
                mtime: None,
            });
        }
        t.entries.sort_by(|a, b| a.name.cmp(&b.name));
        return Ok(t);
    }
    let d = ix
        .resolve_dir(ru, path)
        .ok_or_else(|| format!("{path} is not in the index."))?;
    let cap = if limit == 0 {
        USAGE_ENTRIES
    } else {
        (limit as usize).min(5000)
    };
    let l = ix.list_dir(ru, d, cap);
    let entries = l
        .children
        .iter()
        .map(|ch| UsageEntry {
            name: ix.name(ch.slot).to_string(),
            path: ix.full_path(ch.slot),
            is_dir: ch.is_dir,
            kind: ch.kind,
            size: ch.bytes,
            files: ch.files,
            folders: ch.folders,
            mtime: ix.mtime(ch.slot).and_then(filetime_to_unix_ms),
        })
        .collect();
    Ok(UsageTree {
        path: ix.full_path(ix.dir_slot(ru, d)),
        size: l.bytes,
        files: l.files,
        folders: l.folders,
        entries,
        more: l.more,
        more_size: l.more_bytes,
        cats: l.cats.to_vec(),
        fresh,
    })
}

/// File types and the biggest files under a folder (or under every drive for an empty path).
pub fn usage_detail(sh: &Shared, path: &str, top_files: u32) -> Result<UsageDetail, String> {
    let c = sh.usage_cache()?;
    let ix = sh.index.read().unwrap();
    let gen = sh.gen.load(Ordering::Relaxed);
    let ru = c.ru.read().unwrap();
    let ru = &*ru;
    // Types and biggest files come from walking the child lists: deletes and size changes
    // show at once, records added or moved since the build only after the next one.
    let fresh = c.gen == gen || (c.exact(gen) && !ru.structure_dirty());
    let top = top_files.min(500) as usize;
    let (types, big) = if path.trim().is_empty() {
        let mut all: std::collections::HashMap<String, UsageType> =
            std::collections::HashMap::new();
        let mut big: Vec<(u32, u64)> = Vec::new();
        for (_, d) in ix.volume_dirs(ru) {
            let (t, b) = ix.type_detail(ru, d, top);
            for x in t {
                let e = all.entry(x.ext.clone()).or_insert(UsageType {
                    ext: x.ext,
                    kind: x.kind,
                    bytes: 0,
                    files: 0,
                });
                e.bytes += x.bytes;
                e.files += x.files;
            }
            big.extend(b);
        }
        big.sort_by_key(|b| std::cmp::Reverse(b.1));
        big.truncate(top);
        let mut t: Vec<UsageType> = all.into_values().collect();
        t.sort_by_key(|x| std::cmp::Reverse(x.bytes));
        (t, big)
    } else {
        let d = ix
            .resolve_dir(ru, path)
            .ok_or_else(|| format!("{path} is not in the index."))?;
        let (t, b) = ix.type_detail(ru, d, top);
        (
            t.into_iter()
                .map(|x| UsageType {
                    ext: x.ext,
                    kind: x.kind,
                    bytes: x.bytes,
                    files: x.files,
                })
                .collect(),
            b,
        )
    };
    let biggest = big
        .iter()
        .map(|&(i, sz)| {
            let mut h = hit_at(&ix, i);
            h.size = Some(sz);
            h
        })
        .collect();
    Ok(UsageDetail {
        path: path.to_string(),
        types: types.into_iter().take(60).collect(),
        biggest,
        fresh,
    })
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

    /// std's RwLock prefers writers: a *waiting* writer blocks new readers. This is why a long read
    /// (Space roll-up, folder totals) plus one USN batch used to freeze every search.
    #[test]
    fn std_rwlock_waiting_writer_blocks_new_readers() {
        use std::sync::{Arc, RwLock, TryLockError};
        let l = Arc::new(RwLock::new(0u32));
        let r = l.read().unwrap();
        let l2 = l.clone();
        let w = std::thread::spawn(move || {
            *l2.write().unwrap() += 1;
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let blocked = matches!(l.try_read(), Err(TryLockError::WouldBlock));
        drop(r);
        w.join().unwrap();
        assert!(blocked, "std RwLock let a reader past a waiting writer");
    }

    /// `write_index` polls instead of queueing, so a USN batch arriving during a long read (the
    /// roll-up stand-in below) no longer freezes searches that start while the read is running.
    #[test]
    fn a_waiting_writer_no_longer_blocks_searches() {
        let sh = Arc::new(shared());
        // Thread R: a long read holds the index (a roll-up / folder-totals pass).
        let reader = {
            let sh = Arc::clone(&sh);
            std::thread::spawn(move || {
                let _r = sh.index.read().unwrap();
                std::thread::sleep(Duration::from_millis(400));
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        // Thread W: a USN batch wants in and polls (`write_index`) instead of queueing.
        let writer = {
            let sh = Arc::clone(&sh);
            std::thread::spawn(move || {
                drop(write_index(&sh));
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        // The search must run now, not after R releases the lock (~300 ms from here on the old code).
        let t = Instant::now();
        let _ = search(&sh, "a", 0, 10, Sort::NameAsc);
        assert!(
            t.elapsed() < Duration::from_millis(150),
            "search waited {:.0?} behind the waiting writer",
            t.elapsed()
        );
        reader.join().unwrap();
        // W must have got the lock only after R released it: joining proves it completed.
        writer.join().unwrap();
    }
    fn create(frn: u128, parent: u128, name: &str) -> Change {
        Change::Create(UsnRecord {
            frn,
            parent_frn: parent,
            usn: 0,
            reason: 0,
            attrs: 0x20,
            name: name.into(),
        })
    }
    const CUR: JournalCursor = JournalCursor {
        journal_id: 7,
        next_usn: 99,
    };

    #[test]
    fn usage_lists_children_and_volumes() {
        let sh = shared();
        apply_batch(&sh, b'C', &[create(100, 10, "a.txt")], CUR);
        let u = usage(&sh, r"C:\Docs", 5).unwrap();
        assert!(u.entries.len() <= 1);
        let all = usage(&sh, "", 0).unwrap();
        assert_eq!(
            (all.entries[0].name.as_str(), all.entries[0].path.as_str()),
            ("C:", "C:\\")
        );
        assert!(usage(&sh, r"C:\nope", 0).is_err());
        let d = usage_detail(&sh, "", 5).unwrap();
        assert!(d.biggest.len() <= 1);
    }

    /// Regression: searchd wedged for good (every search and `status` hung) when a Disk-space request
    /// held the `usage_cache` mutex while waiting for the roll-up's `ru` lock, and the writer held `ru`
    /// while waiting for that mutex. Requests and batches must be able to run side by side forever.
    #[test]
    fn usage_requests_and_batches_do_not_deadlock() {
        let sh = Arc::new(shared());
        apply_batch(&sh, b'C', &[create(100, 10, "a.txt")], CUR);
        usage(&sh, "", 0).unwrap(); // a roll-up exists, so every batch updates it through `ru`
        let stop = Arc::new(AtomicBool::new(false));
        let work = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::new();
        {
            let (sh, stop, work) = (sh.clone(), stop.clone(), work.clone());
            threads.push(std::thread::spawn(move || {
                let mut n = 1000u128;
                while !stop.load(Ordering::Relaxed) {
                    n += 1;
                    apply_batch(&sh, b'C', &[create(n, 10, &format!("f{n}.txt"))], CUR);
                    work.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for _ in 0..3 {
            let (sh, stop, work) = (sh.clone(), stop.clone(), work.clone());
            threads.push(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = sh.usage_cache();
                    let _ = usage(&sh, "", 0);
                    work.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        // The cycle needs an unlucky interleaving: give it a few seconds, and fail (instead of
        // hanging the test run) if the work counter ever stops moving.
        let mut last = 0;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(100));
            let now = work.load(Ordering::Relaxed);
            if now == last && now > 0 && last > 0 {
                std::thread::sleep(Duration::from_secs(3));
                assert!(
                    work.load(Ordering::Relaxed) != now,
                    "writer and usage requests deadlocked"
                );
            }
            last = now;
        }
        stop.store(true, Ordering::Relaxed);
        for t in threads {
            t.join().unwrap();
        }
    }

    /// The cached roll-up, aged past the rebuild floor.
    fn age_usage(sh: &Shared) {
        let c = sh.usage_cache.lock().unwrap().clone().unwrap();
        let old = UsageCache {
            gen: c.gen,
            synced: AtomicU64::new(c.synced.load(Ordering::Relaxed)),
            at: Instant::now().checked_sub(USAGE_LISTS_EVERY).unwrap(),
            ru: RwLock::new(c.ru.read().unwrap().clone()),
        };
        *sh.usage_cache.lock().unwrap() = Some(Arc::new(old));
    }
    fn built_at(sh: &Shared) -> Instant {
        sh.usage_cache.lock().unwrap().as_ref().unwrap().at
    }

    #[test]
    fn usage_freshness_tracks_the_index() {
        let sh = shared();
        apply_batch(&sh, b'C', &[create(100, 10, "a.txt")], CUR);
        assert!(
            usage(&sh, r"C:\Docs", 5).unwrap().fresh,
            "a just-built roll-up is fresh"
        );
        let built = built_at(&sh);
        // a change the writer tracked: totals follow at once, no rebuild, still fresh
        apply_batch(&sh, b'C', &[create(101, 10, "b.txt")], CUR);
        let u = usage(&sh, r"C:\Docs", 5).unwrap();
        assert!(u.fresh, "tracked change: totals exact");
        assert_eq!(u.files, 2);
        assert_eq!(built_at(&sh), built, "no rebuild for a tracked change");
        // ... but the type walk uses child lists, which miss b.txt until the next build
        assert!(!usage_detail(&sh, r"C:\Docs", 5).unwrap().fresh);
        // the main loop rebuilds lagging lists once past the floor; b.txt is listed then
        sh.refresh_usage();
        assert_eq!(built_at(&sh), built, "inside the floor: no rebuild");
        age_usage(&sh);
        sh.refresh_usage();
        let u = usage(&sh, r"C:\Docs", 5).unwrap();
        assert!(u.fresh && u.entries.iter().any(|e| e.name == "b.txt"));
        assert!(usage_detail(&sh, r"C:\Docs", 5).unwrap().fresh);
        // a change around the overlay (here: a bare generation bump): behind inside the floor,
        // rebuilt by the request itself past it
        sh.gen.fetch_add(1, Ordering::Relaxed);
        assert!(!usage(&sh, r"C:\Docs", 5).unwrap().fresh);
        age_usage(&sh);
        assert!(
            usage(&sh, r"C:\Docs", 5).unwrap().fresh,
            "past the floor the request rebuilds"
        );
    }

    #[test]
    fn usage_shows_a_recycled_folder_at_once() {
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'C', 5);
        b.push(b'C', 10, 5, "Docs", true, 0, 0);
        b.push(b'C', 11, 10, "Old", true, 0, 0);
        b.push(b'C', 12, 11, "big.iso", false, 7 << 30, 0);
        b.push(b'C', 13, 10, "keep.txt", false, 100, 0);
        let sh = Shared::new(b.finish(), &['C']);
        let u = usage(&sh, r"C:\Docs", 5).unwrap();
        assert_eq!(
            (u.size, u.files, u.folders, u.entries.len()),
            ((7 << 30) + 100, 2, 1, 2)
        );
        let del = Change::Delete(UsnRecord {
            frn: 11,
            parent_frn: 10,
            usn: 0,
            reason: 0,
            attrs: 0x10,
            name: "Old".into(),
        });
        apply_batch(&sh, b'C', &[del], CUR);
        let u = usage(&sh, r"C:\Docs", 5).unwrap();
        assert!(u.fresh);
        assert_eq!((u.size, u.files, u.folders), (100, 1, 0));
        assert_eq!(u.entries.len(), 1);
        let all = usage(&sh, "", 0).unwrap();
        assert_eq!((all.size, all.files, all.folders), (100, 1, 1));
        assert_eq!(all.cats[5], 0, "the archive bytes left the drive total");
        let d = usage_detail(&sh, r"C:\Docs", 5).unwrap();
        assert!(d.fresh && d.types.iter().all(|t| t.ext != "iso"));
    }

    #[test]
    fn resync_reaches_every_subscriber_whatever_its_roots() {
        let sh = shared();
        let rx_docs = sh.subscribe(&["C:\\Docs".into()]);
        let rx_all = sh.subscribe(&[]);
        sh.notify_resync();
        assert!(matches!(rx_docs.try_recv(), Ok(Response::Resync)));
        assert!(matches!(rx_all.try_recv(), Ok(Response::Resync)));
        // A volume swap (rescan) announces itself the same way.
        replace(&sh, Index::default());
        assert!(matches!(rx_docs.try_recv(), Ok(Response::Resync)));
    }

    #[test]
    fn batch_updates_index_cursor_recent_and_subscribers() {
        let sh = shared();
        let rx_docs = sh.subscribe(&["C:\\Docs".into()]);
        let rx_other = sh.subscribe(&["C:\\Other\\".into()]);
        apply_batch(
            &sh,
            b'C',
            &[
                create(100, 10, "a.txt"),
                create(101, 10, "b.txt"),
                create(100, 10, "a.txt"),
            ],
            CUR,
        );
        let (total, hits, note) = search(&sh, "txt", 0, 10, Sort::NameDesc);
        assert_eq!((total, hits[0].name.as_str(), note), (2, "b.txt", None));
        assert!(
            search(&sh, "txt", 0, 10, Sort::SizeDesc).2.is_none(),
            "size sort is real now"
        );
        let m = &sh.index.read().unwrap().volumes()[0].clone();
        assert_eq!((m.journal_id, m.next_usn), (7, 99));
        let r = recent(&sh, 10);
        assert_eq!(
            r.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            vec!["C:\\Docs\\a.txt", "C:\\Docs\\b.txt"]
        );
        assert!(matches!(
            rx_docs.try_recv(),
            Ok(Response::Change {
                kind: ChangeKind::Created,
                ..
            })
        ));
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
            PathOp::Upsert {
                rel: r"Photos\a.jpg".into(),
                is_dir: false,
                size: 7,
                mtime: 0,
            },
            PathOp::Upsert {
                rel: r"Photos\b.jpg".into(),
                is_dir: false,
                size: 8,
                mtime: 0,
            },
            PathOp::Remove {
                rel: r"Photos\b.jpg".into(),
            },
        ];
        apply_paths(&sh, b'E', &ops);
        let (total, hits, _) = search(&sh, "jpg", 0, 10, Sort::NameAsc);
        assert_eq!(
            (total, hits[0].path.as_str(), hits[0].size),
            (1, r"E:\Photos\a.jpg", Some(7))
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(Response::Change {
                kind: ChangeKind::Created,
                ..
            })
        ));
        let st = status(&sh);
        let e = st.iter().find(|v| v.letter == 'E').unwrap();
        assert_eq!(
            (e.fs.as_str(), e.monitor, e.files, e.folders),
            ("exFAT", Monitor::FolderWatch, 1, 1)
        );
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
        assert_eq!(
            search(&sh, "Docs", 0, 10, Sort::NameAsc).0,
            1,
            "other volumes kept"
        );
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
            Change::Create(UsnRecord {
                frn,
                parent_frn: parent,
                usn: 0,
                reason: 0,
                attrs,
                name: name.into(),
            })
        };
        apply_batch(
            &sh,
            b'R',
            &[
                mk(dir, 1 << 100, 0x10, "Big"),
                mk(dir + 1, dir, 0x20, "x.txt"),
            ],
            CUR,
        );
        let (_, hits, _) = search(&sh, "x.txt", 0, 10, Sort::NameAsc);
        assert_eq!(hits[0].path, r"R:\Big\x.txt");
    }

    #[test]
    fn excluded_paths_leave_the_stream_before_paging() {
        let sh = shared();
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'E', 5);
        b.volume_mut(b'E').unwrap().kind = VolKind::Folder;
        replace(&sh, b.finish());
        let up = |rel: &str, size: u64, mtime: i64| PathOp::Upsert {
            rel: rel.into(),
            is_dir: false,
            size,
            mtime,
        };
        let files: Vec<PathOp> = (0..10)
            .map(|k| up(&format!(r"Ph\p{k}.jpg"), 10 + k, 100 + k as i64))
            .chain([up(r"Other\p3.jpg", 1, 1)]) // same name as an excluded one, other folder
            .collect();
        apply_paths(&sh, b'E', &files);
        let names = |hits: Vec<Hit>| {
            hits.iter()
                .map(|h| h.path.as_str().to_string())
                .collect::<Vec<_>>()
        };
        let (total, all, _) = search(&sh, "jpg", 0, 100, Sort::NameAsc);
        assert_eq!(total, 11);
        let all = names(all);
        let path_of = |n: &str| all.iter().find(|p| p.ends_with(n)).unwrap().clone();
        // Case and slash direction do not matter; a path that matches nothing is harmless.
        let ex: Vec<String> = vec![
            path_of(r"Ph\p3.jpg").to_uppercase().replace('\\', "/"),
            path_of(r"Ph\p7.jpg"),
            path_of(r"Ph\p0.jpg") + "\\",
            r"E:\nowhere\p5.jpg".into(),
        ];
        let kept: Vec<String> = all
            .iter()
            .filter(|p| {
                !p.ends_with(r"Ph\p3.jpg") && !p.ends_with("p7.jpg") && !p.ends_with("p0.jpg")
            })
            .cloned()
            .collect();
        assert_eq!(kept.len(), 8);
        assert!(kept.iter().any(|p| p.ends_with(r"Other\p3.jpg")));

        for sort in [
            Sort::NameAsc,
            Sort::NameDesc,
            Sort::MtimeDesc,
            Sort::SizeAsc,
        ] {
            let (_, full, _) = search(&sh, "jpg", 0, 100, sort);
            let expect: Vec<String> = names(full)
                .into_iter()
                .filter(|p| kept.contains(p))
                .collect();
            let (total, hits, _) = search_excluding(&sh, "jpg", 0, 100, sort, &ex);
            assert_eq!((total, names(hits)), (8, expect.clone()), "{sort:?}");
            // Every window of 3 tiles the remaining stream exactly, whatever offset.
            for off in 0..9 {
                let (t, page, _) = search_excluding(&sh, "jpg", off, 3, sort, &ex);
                let want: Vec<String> = expect.iter().skip(off as usize).take(3).cloned().collect();
                assert_eq!((t, names(page)), (8, want), "{sort:?} @{off}");
            }
            // A count-only request applies the exclude too.
            assert_eq!(search_excluding(&sh, "jpg", 0, 0, sort, &ex).0, 8);
        }
        // A change to the index re-finds the excluded rows (the remembered positions are per state).
        apply_paths(&sh, b'E', &[up(r"Ph\a.jpg", 5, 5)]);
        let (total, hits, _) = search_excluding(&sh, "jpg", 0, 2, Sort::NameAsc, &ex);
        assert_eq!(total, 9);
        assert!(names(hits)[0].ends_with(r"Ph\a.jpg"));
    }

    #[test]
    fn meta_sorts_order_and_page_through_the_cache() {
        let sh = shared();
        // A folder-watch volume carries explicit sizes/mtimes (USN records get theirs from a later stat).
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'E', 5);
        b.volume_mut(b'E').unwrap().kind = VolKind::Folder;
        replace(&sh, b.finish());
        let up = |rel: &str, size: u64, mtime: i64| PathOp::Upsert {
            rel: rel.into(),
            is_dir: false,
            size,
            mtime,
        };
        apply_paths(
            &sh,
            b'E',
            &[
                up(r"Ph\a.txt", 10, 100),
                up(r"Ph\b.txt", 900, 500),
                up(r"Ph\c.txt", 500, 300),
                up(r"Ph\d.txt", 20, 400),
            ],
        );

        let names = |hits: Vec<Hit>| {
            hits.iter()
                .map(|h| h.name.as_str().to_string())
                .collect::<Vec<_>>()
        };
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
        assert_eq!(
            (total, names(hits)),
            (2, vec!["b.txt".to_string(), "c.txt".to_string()])
        );
        assert_eq!(search(&sh, "txt size:<100", 0, 0, Sort::NameAsc).0, 3);
        // folders carry what they hold, and a size sort ranks them by it
        let (_, hits, _) = search(&sh, "Ph", 0, 10, Sort::NameAsc);
        assert_eq!(
            (hits[0].name.as_str(), hits[0].is_dir, hits[0].size),
            ("Ph", true, Some(10 + 900 + 500 + 20 + 1))
        );
        let (_, hits, _) = search(&sh, "", 0, 3, Sort::SizeDesc);
        assert!(
            hits.iter().any(|h| h.name == "Ph" && h.size == Some(1431)),
            "{hits:?}"
        );
        // a new file shows up in the folder's size once the table is refreshed
        apply_paths(&sh, b'E', &[up(r"Ph\f.txt", 69, 1)]);
        let (_, hits, _) = search(&sh, "folder:Ph", 0, 10, Sort::SizeDesc); // a size sort recomputes
        assert_eq!(hits[0].size, Some(1500));
        // a window past the end is empty, the total stays
        assert_eq!(search(&sh, "txt", 99, 5, Sort::NameAsc).0, 6);
        assert!(search(&sh, "txt", 99, 5, Sort::MtimeDesc).1.is_empty());
    }

    /// Best match over the pipe protocol: the whole name first, then the names starting with
    /// it, then a word inside, and the letters inside a word last; pages tile through the
    /// cache, and the order is built again once the index changes.
    #[test]
    fn best_match_orders_pages_and_refreshes_through_the_cache() {
        let sh = shared();
        // A folder-watch volume carries explicit sizes/mtimes (USN records get theirs from a later stat).
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'E', 5);
        b.volume_mut(b'E').unwrap().kind = VolKind::Folder;
        replace(&sh, b.finish());
        // 2001-01-01 (unix ms): nothing is recent, so the class and the location decide.
        let old = 978_307_200_000i64;
        let up = |rel: &str, is_dir: bool| PathOp::Upsert {
            rel: rel.into(),
            is_dir,
            size: if is_dir { 0 } else { 7 },
            mtime: old,
        };
        apply_paths(
            &sh,
            b'E',
            &[
                up(r"Ph\017 drawbar organ.fst", false),
                up(r"Ph\awb.txt", false),
                up(r"Ph\awb", true),
                up(r"Ph\target", true),
                up(r"Ph\target\build_awb.log", false),
                up(r"Ph\my awb notes.docx", false),
            ],
        );
        let paths = |hits: &[Hit]| {
            hits.iter()
                .map(|h| format!("{}{}", h.path, if h.is_dir { "\\" } else { "" }))
                .collect::<Vec<_>>()
        };
        let run = |q: &str, off: u32, lim: u32, sort: Sort| {
            let (_, hits, note) = search(&sh, q, off, lim, sort);
            (paths(&hits), note.is_none())
        };
        let best = [
            r"E:\Ph\awb\",
            r"E:\Ph\awb.txt",
            r"E:\Ph\my awb notes.docx",
            r"E:\Ph\target\build_awb.log",
            r"E:\Ph\017 drawbar organ.fst",
        ];
        assert_eq!(run("awb", 0, 10, Sort::Relevance).0, best);
        assert!(run("awb", 0, 10, Sort::Relevance).1, "no fallback note");
        assert_eq!(search(&sh, "awb", 0, 10, Sort::Relevance).0, 5);
        // Alphabetical put the digit-led name on top; that order is still one menu pick away.
        assert_eq!(
            run("awb", 0, 10, Sort::NameAsc).0[0],
            r"E:\Ph\017 drawbar organ.fst"
        );
        // later windows answer from the cache and tile the same order
        let mut tiled = Vec::new();
        for off in [0, 2, 4, 6] {
            tiled.extend(run("awb", off, 2, Sort::Relevance).0);
        }
        assert_eq!(tiled, best);
        // Best match and the name order are two cached answers of the same query
        assert_eq!(sh.qcache.lock().unwrap().map.len(), 2);
        // a filter, a pasted path or wildcards only: nothing typed to rank by, so it is the
        // name search (a typed word next to a path still ranks: `run` above)
        for q in ["awb folder:", r"Ph\", "awb*"] {
            assert_eq!(
                run(q, 0, 10, Sort::Relevance),
                run(q, 0, 10, Sort::NameAsc),
                "{q}"
            );
        }
        // limit 0 only counts
        let (_, hits, _) = search(&sh, "awb", 0, 0, Sort::Relevance);
        assert_eq!(search(&sh, "awb", 0, 0, Sort::Relevance).0, 5);
        assert!(hits.is_empty());
        // the index changes: the order is built again and the new file sits with its class
        apply_paths(&sh, b'E', &[up(r"Ph\awb-old.txt", false)]);
        let (hits, _) = run("awb", 0, 10, Sort::Relevance);
        assert_eq!(hits.len(), 6);
        assert_eq!(
            hits[..4].to_vec(),
            [best[0], best[1], r"E:\Ph\awb-old.txt", best[2]]
        );
        // an excluded path leaves the stream before the offsets apply
        let (total, hits, _) = search_excluding(
            &sh,
            "awb",
            0,
            10,
            Sort::Relevance,
            &[r"E:\Ph\awb".to_string(), r"E:\Ph\awb.txt".to_string()],
        );
        assert_eq!(
            (total, paths(&hits)),
            (
                4,
                vec![
                    r"E:\Ph\awb-old.txt".to_string(),
                    best[2].to_string(),
                    best[3].to_string(),
                    best[4].to_string()
                ]
            )
        );
    }

    #[test]
    fn status_counts_follow_live_changes() {
        let sh = shared();
        assert_eq!(
            {
                let s = status(&sh);
                (s[0].files, s[0].folders)
            },
            (0, 1)
        ); // primes the cache
        apply_batch(
            &sh,
            b'C',
            &[
                create(100, 10, "a.txt"),
                Change::Create(UsnRecord {
                    frn: 20,
                    parent_frn: 10,
                    usn: 0,
                    reason: 0,
                    attrs: 0x10,
                    name: "Sub".into(),
                }),
                create(101, 20, "b.txt"),
            ],
            CUR,
        );
        assert_eq!(
            {
                let s = status(&sh);
                (s[0].files, s[0].folders)
            },
            (2, 2)
        );
        // rename: no change
        let ren = Change::Rename {
            frn: 100,
            old_parent: 10,
            old_name: "a.txt".into(),
            new_parent: 10,
            new_name: "c.txt".into(),
        };
        apply_batch(&sh, b'C', &[ren], CUR);
        assert_eq!(
            {
                let s = status(&sh);
                (s[0].files, s[0].folders)
            },
            (2, 2)
        );
        // recursive folder delete removes the folder and its child
        let del = Change::Delete(UsnRecord {
            frn: 20,
            parent_frn: 10,
            usn: 0,
            reason: 0,
            attrs: 0x10,
            name: "Sub".into(),
        });
        apply_batch(&sh, b'C', &[del], CUR);
        assert_eq!(
            {
                let s = status(&sh);
                (s[0].files, s[0].folders)
            },
            (1, 1)
        );
    }

    #[test]
    fn root_matching() {
        assert!(under("c:\\docs\\a", &normalize_root("C:\\Docs\\")));
        assert!(under("c:\\docs", &normalize_root("C:/Docs")));
        assert!(!under("c:\\docsx\\a", &normalize_root("C:\\Docs")));
        assert!(under("c:\\a", &normalize_root("C:\\")));
    }

    fn names_of(sh: &Shared, q: &str) -> Vec<String> {
        let (total, hits, _) = search(sh, q, 0, 100, Sort::NameAsc);
        assert_eq!(total as usize, hits.len());
        hits.into_iter().map(|h| h.name).collect()
    }
    fn entry_gen(sh: &Shared, q: &str) -> Option<u64> {
        let c = sh.qcache.lock().unwrap();
        c.map.get(&(q.to_string(), SortClass::Name)).map(|e| e.gen)
    }

    /// A paged name query survives USN batches: the batch patches it (no re-scan), and what
    /// it then serves equals the truth. A folder rename drops entries with a path term.
    #[test]
    fn cached_name_results_are_patched_by_batches() {
        let sh = shared();
        let files = [
            create(100, 10, "a.jpg"),
            create(101, 10, "b.jpg"),
            create(102, 10, "n.txt"),
        ];
        apply_batch(&sh, b'C', &files, CUR);
        assert_eq!(names_of(&sh, "ext:jpg"), ["a.jpg", "b.jpg"]);
        assert_eq!(names_of(&sh, r"docs\"), ["a.jpg", "b.jpg", "n.txt"]);
        let del = Change::Delete(UsnRecord {
            frn: 101,
            parent_frn: 10,
            usn: 0,
            reason: 0,
            attrs: 0x20,
            name: "b.jpg".into(),
        });
        let ren = Change::Rename {
            frn: 100,
            old_parent: 10,
            old_name: "a.jpg".into(),
            new_parent: 10,
            new_name: "a.txt".into(),
        };
        apply_batch(&sh, b'C', &[create(103, 10, "c.jpg"), ren, del], CUR);
        let now = sh.gen.load(Ordering::Relaxed);
        assert_eq!(entry_gen(&sh, "ext:jpg"), Some(now), "patched, not dropped");
        assert_eq!(
            entry_gen(&sh, r"docs\"),
            Some(now),
            "no folder moved: path entry kept"
        );
        assert_eq!(names_of(&sh, "ext:jpg"), ["c.jpg"]);
        assert_eq!(names_of(&sh, r"docs\"), ["a.txt", "c.jpg", "n.txt"]);
        let moved = Change::Rename {
            frn: 10,
            old_parent: 5,
            old_name: "Docs".into(),
            new_parent: 5,
            new_name: "Papers".into(),
        };
        apply_batch(&sh, b'C', &[moved], CUR);
        assert_eq!(
            entry_gen(&sh, r"docs\"),
            None,
            "a folder rename drops path entries"
        );
        assert!(
            entry_gen(&sh, "ext:jpg").is_some(),
            "name-only entries live on"
        );
        assert!(names_of(&sh, r"docs\").is_empty());
        assert_eq!(names_of(&sh, r"papers\"), ["a.txt", "c.jpg", "n.txt"]);
        assert_eq!(names_of(&sh, "ext:jpg"), ["c.jpg"]);
    }
}
