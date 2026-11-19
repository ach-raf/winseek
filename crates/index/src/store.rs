//! Struct-of-arrays record store, live mutation, name-sorted order array.
//!
//! # Memory layout (bytes per record slot)
//! `parent` 4, `name_off` 4, `name_len` 2, `flags` 1, `vol` 1, `frn` 8, sorted order 4,
//! `rank` 4, name arena ~avg name length (UTF-8, no terminator), optional `size` 8 +
//! `mtime` 8. That is ~28 B + names without meta.
//! The `HashMap<(vol,frn),idx>` holds **folders only** (~25 B per folder entry).
//! The *lookup structure* `kids` is built lazily on the first mutation: every record in
//! one chunked array sorted by (parent, exact name, slot), 4 B per record. A query-only
//! index never pays for it.
//!
//! # Lookup of files
//! Files are not in the FRN map. `(parent, exact name)` -> idx is a binary search in
//! `kids` (blocks, then inside one block); children enumeration (recursive delete) is the
//! run of entries with that parent. Inserts and removals move one block.
//!
//! # Sort maintenance
//! `sorted` (a chunked list, see [`crate::chunks`]) is sorted by (folded name, then full
//! path, then idx). Mutations
//! never touch it directly: a renamed/moved record is flagged `STALE` (still in `order`
//! at its old position) and queued in `pending`; new records are queued in `pending`;
//! removed records get `DELETED`. [`Index::refresh`] drops stale/deleted entries from the
//! blocks that hold them and binary-searches the sorted pending list into place (or does a
//! full parallel sort if pending is > 1/8 of the index). Readers on a dirty index
//! (`search(&self)`) get the same merge computed on the fly, so results are always
//! correct, just slower.
//! Record slots of deleted records are recycled via a free list *after* refresh has
//! dropped them from `order`. Caveat: renaming a folder does not re-sort descendants;
//! only the path tie-break between equal names may be stale until the next full rebuild.
//!
//! # Stubs / orphans
//! A record whose parent FRN is unknown gets a lazy *stub folder* (name `<hexfrn>`,
//! attached to the volume root, in the FRN map, not searchable). When the real folder
//! record for that FRN arrives (`insert`), the stub is filled in place, so children keep
//! their parent index. Builder entries whose parent FRN is never seen keep the stub.

use crate::chunks::Chunks;
use crate::fold::cmp_fold;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

pub const NONE: u32 = u32::MAX;
pub const F_DIR: u8 = 1;
pub const F_DELETED: u8 = 2;
pub const F_STUB: u8 = 4;
pub const F_ROOT: u8 = 8;
pub(crate) const F_STALE: u8 = 16;
pub(crate) const F_PENDING: u8 = 32;
pub(crate) const F_INORDER: u8 = 64;
pub(crate) const F_FREED: u8 = 128;
pub(crate) const F_PERSIST: u8 = F_DIR | F_DELETED | F_STUB | F_ROOT;

/// NTFS FRNs carry a 16-bit sequence number in the top bits; USN parents have it, MFT scans
/// don't. All public APIs mask to the 48-bit record number.
pub const FRN_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
#[inline]
pub fn norm_frn(f: u64) -> u64 {
    f & FRN_MASK
}

pub(crate) const MAX_DEPTH: usize = 128;

#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);
impl FxHasher {
    #[inline]
    fn add(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}
impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.add(b as u64);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
}
pub type FolderMap = HashMap<(u8, u64), u32, BuildHasherDefault<FxHasher>>;
/// ReFS 128-bit FRN -> the u64 id used as that folder's `frn` (see [`Index::intern128`]).
pub type Ids128 = HashMap<(u8, u128), u64, BuildHasherDefault<FxHasher>>;

/// How a volume's records are identified and kept live. The numbers match Everything's
/// ESDb volume types (0 = NTFS, 1 = FAT folder index, 2 = ReFS).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum VolKind {
    /// 48-bit MFT FRNs, USN V2.
    #[default]
    Ntfs = 0,
    /// No FRNs (FAT/exFAT, ReFS without a journal): synthetic folder ids from `next_id`,
    /// path-based updates.
    Folder = 1,
    /// 128-bit FRNs interned to u64 ids, USN V3.
    Refs = 2,
}

impl VolKind {
    pub fn from_u8(v: u8) -> Option<VolKind> {
        match v {
            0 => Some(VolKind::Ntfs),
            1 => Some(VolKind::Folder),
            2 => Some(VolKind::Refs),
            _ => None,
        }
    }
}

/// First synthetic id (`Folder` / `Refs` volumes). The root is always 5, like NTFS.
pub const FIRST_SYNTHETIC_ID: u64 = 16;

/// Per-volume state. `serial`, `journal_id`, `next_usn` are opaque, set by the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeMeta {
    pub letter: u8,
    pub serial: u64,
    pub journal_id: u64,
    pub next_usn: i64,
    /// Record index of the volume root.
    pub root: u32,
    pub kind: VolKind,
    /// Next synthetic folder id (`Folder` / `Refs`); ids are never reused.
    pub next_id: u64,
}

#[derive(Default)]
pub struct Index {
    pub(crate) parent: Vec<u32>,
    pub(crate) name_off: Vec<u32>,
    pub(crate) name_len: Vec<u16>,
    pub(crate) flags: Vec<u8>,
    pub(crate) vol: Vec<u8>,
    pub(crate) frn: Vec<u64>,
    pub(crate) size: Vec<u64>,
    pub(crate) mtime: Vec<i64>,
    pub(crate) with_meta: bool,
    pub(crate) arena: Vec<u8>,
    pub(crate) garbage: usize,
    pub(crate) folders: FolderMap,
    /// ReFS twin of the folder map key space (Everything db+0x4a0): 128-bit FRN -> u64 id.
    pub(crate) ids128: Ids128,
    pub(crate) vols: Vec<VolumeMeta>,
    /// The name-sorted order of the searchable records, chunked (see [`crate::chunks`]).
    pub(crate) sorted: Chunks,
    /// slot -> code of its place in `sorted` (NONE if not searchable); lets scans run in
    /// storage order. [`OrderView::rank`] turns a code into the dense position.
    pub(crate) rank: Vec<u32>,
    pub(crate) pending: Vec<u32>,
    /// Records still in `order` that were flagged `STALE` / `DELETED` since the last refresh
    /// (may hold duplicates); lets [`Index::refresh`] skip whole-index flag passes.
    pub(crate) touched: Vec<u32>,
    pub(crate) free: Vec<u32>,
    pub(crate) dirty: bool,
    pub(crate) live: usize,
    /// Per-volume live (files, folders), indexed by volume letter byte. Excludes deleted
    /// slots, stubs and volume roots. Maintained incrementally; never recounted.
    pub(crate) vcount: Vec<(u64, u64)>,
    // lazily built mutation structures
    links_on: bool,
    /// Every linked record sorted by (parent, exact name, slot): file lookup by name and
    /// children enumeration (built on the first mutation).
    kids: Chunks,
}

/// What a reader scans: the sorted order and every slot's dense position in it.
pub(crate) enum OrderView<'a> {
    Live { ch: &'a Chunks, rank: &'a [u32] },
    Flat { order: Vec<u32>, rank: Vec<u32> },
}

impl OrderView<'_> {
    pub(crate) fn len(&self) -> usize {
        match self {
            OrderView::Live { ch, .. } => ch.len(),
            OrderView::Flat { order, .. } => order.len(),
        }
    }
    /// Number of record slots covered.
    pub(crate) fn slots(&self) -> usize {
        match self {
            OrderView::Live { rank, .. } => rank.len(),
            OrderView::Flat { rank, .. } => rank.len(),
        }
    }
    /// Dense position of slot `i` in the order, NONE when it is not searchable.
    #[inline]
    pub(crate) fn rank(&self, i: usize) -> u32 {
        match self {
            OrderView::Live { ch, rank } => match rank[i] {
                NONE => NONE,
                c => ch.dense(c),
            },
            OrderView::Flat { rank, .. } => rank[i],
        }
    }
    /// The record at dense position `pos`.
    pub(crate) fn at(&self, pos: usize) -> Option<u32> {
        match self {
            OrderView::Live { ch, .. } => ch.at(pos),
            OrderView::Flat { order, .. } => order.get(pos).copied(),
        }
    }
}

/// Make room for `more` items in `v`, growing by a sixteenth of its length (at least 4096
/// items) instead of `Vec`'s doubling: the columns hold ~14M entries, and one doubling
/// after a snapshot load would commit hundreds of MB that are never touched.
fn room<T>(v: &mut Vec<T>, more: usize) {
    if v.capacity() - v.len() < more {
        v.reserve_exact((v.len() / 16).max(4096).max(more));
    }
}

fn clamp_name(name: &str) -> &str {
    if name.len() <= u16::MAX as usize {
        return name;
    }
    let mut e = u16::MAX as usize;
    while !name.is_char_boundary(e) {
        e -= 1;
    }
    &name[..e]
}

impl Index {
    pub fn new(with_meta: bool) -> Self {
        Index {
            with_meta,
            ..Default::default()
        }
    }

    // ---------------------------------------------------------------- accessors
    /// Number of record slots (including tombstones awaiting reuse).
    pub fn slots(&self) -> usize {
        self.parent.len()
    }
    /// Live records (volume roots and stubs included).
    pub fn live_count(&self) -> usize {
        self.live
    }
    /// Searchable records currently in the sorted order (after pending merge).
    pub fn order_len(&self) -> usize {
        if self.dirty {
            self.merged().len()
        } else {
            self.sorted.len()
        }
    }
    pub fn has_meta(&self) -> bool {
        self.with_meta
    }
    #[inline]
    pub(crate) fn name_bytes(&self, i: u32) -> &[u8] {
        let i = i as usize;
        let o = self.name_off[i] as usize;
        &self.arena[o..o + self.name_len[i] as usize]
    }
    #[inline]
    pub fn name(&self, i: u32) -> &str {
        // SAFETY: the arena only ever receives bytes of valid `&str` (insert/rename clamp at
        // char boundaries) or snapshot names validated with `from_utf8` on load.
        unsafe { std::str::from_utf8_unchecked(self.name_bytes(i)) }
    }
    #[inline]
    pub fn parent(&self, i: u32) -> u32 {
        self.parent[i as usize]
    }
    #[inline]
    pub fn is_dir(&self, i: u32) -> bool {
        self.flags[i as usize] & F_DIR != 0
    }
    #[inline]
    pub fn is_deleted(&self, i: u32) -> bool {
        self.flags[i as usize] & F_DELETED != 0
    }
    #[inline]
    pub fn is_stub(&self, i: u32) -> bool {
        self.flags[i as usize] & F_STUB != 0
    }
    pub fn frn(&self, i: u32) -> u64 {
        self.frn[i as usize]
    }
    pub fn vol(&self, i: u32) -> u8 {
        self.vol[i as usize]
    }
    pub fn size(&self, i: u32) -> Option<u64> {
        self.with_meta.then(|| self.size[i as usize])
    }
    pub fn mtime(&self, i: u32) -> Option<i64> {
        self.with_meta.then(|| self.mtime[i as usize])
    }
    pub fn volumes(&self) -> &[VolumeMeta] {
        &self.vols
    }
    pub fn volume_mut(&mut self, letter: u8) -> Option<&mut VolumeMeta> {
        self.vols.iter_mut().find(|v| v.letter == letter)
    }
    pub fn folder_idx(&self, vol: u8, frn: u64) -> Option<u32> {
        self.folders.get(&(vol, norm_frn(frn))).copied()
    }
    pub fn root_of(&self, vol: u8) -> Option<u32> {
        self.vols.iter().find(|v| v.letter == vol).map(|v| v.root)
    }
    pub fn kind_of(&self, vol: u8) -> Option<VolKind> {
        self.vols.iter().find(|v| v.letter == vol).map(|v| v.kind)
    }

    // ---------------------------------------------------------------- synthetic ids
    /// Next synthetic id of `vol` (`Folder` volumes: a new folder; `Refs`: a new FRN).
    /// Adds the volume (root 5) if needed. Ids start at [`FIRST_SYNTHETIC_ID`].
    pub fn alloc_id(&mut self, vol: u8) -> u64 {
        self.add_volume_raw(vol, 5);
        let m = self.volume_mut(vol).expect("volume just added");
        let id = m.next_id.max(FIRST_SYNTHETIC_ID);
        m.next_id = id + 1;
        id
    }
    /// The u64 id of a 128-bit (ReFS) FRN, if known.
    pub fn lookup128(&self, vol: u8, frn: u128) -> Option<u64> {
        self.ids128.get(&(vol, frn)).copied()
    }
    /// Find-or-assign the u64 id of a 128-bit FRN. One folder map serves both id widths:
    /// this interner is our stand-in for Everything's twin 128-bit folder tree.
    pub fn intern128(&mut self, vol: u8, frn: u128) -> u64 {
        if let Some(id) = self.lookup128(vol, frn) {
            return id;
        }
        let id = self.alloc_id(vol);
        self.ids128.insert((vol, frn), id);
        id
    }
    /// Bind a 128-bit FRN to a fixed id (the volume root -> 5).
    pub fn set_id128(&mut self, vol: u8, frn: u128, id: u64) {
        self.ids128.insert((vol, frn), id);
    }
    /// Interned FRNs whose id is still a live folder (what a snapshot keeps).
    pub fn live_ids128(&self) -> impl Iterator<Item = (u8, u128, u64)> + '_ {
        self.ids128
            .iter()
            .filter(|(&(v, _), &id)| self.folders.contains_key(&(v, id)))
            .map(|(&(v, f), &id)| (v, f, id))
    }

    /// Resolve `rel` (backslash-separated, relative to the volume root, exact names) to a
    /// record. Empty `rel` is the root.
    pub fn find_path(&mut self, vol: u8, rel: &str) -> Option<u32> {
        let mut cur = self.root_of(vol)?;
        for part in rel.split('\\').filter(|p| !p.is_empty()) {
            cur = self.find_child(cur, part)?;
        }
        Some(cur)
    }
    /// Approximate heap bytes used (capacities).
    pub fn mem_bytes(&self) -> usize {
        self.parent.capacity() * 4
            + self.name_off.capacity() * 4
            + self.name_len.capacity() * 2
            + self.flags.capacity()
            + self.vol.capacity()
            + self.frn.capacity() * 8
            + self.size.capacity() * 8
            + self.mtime.capacity() * 8
            + self.arena.capacity()
            + self.sorted.mem_bytes()
            + self.rank.capacity() * 4
            + self.folders.capacity() * 25
            + self.ids128.capacity() * 33
            + self.kids.mem_bytes() * 4
    }

    // ---------------------------------------------------------------- paths
    /// Fill `buf` with the ancestor chain, leaf first. Returns its length.
    pub(crate) fn chain(&self, mut i: u32, buf: &mut [u32; MAX_DEPTH]) -> usize {
        let mut n = 0;
        while i != NONE && n < MAX_DEPTH {
            buf[n] = i;
            n += 1;
            i = self.parent[i as usize];
        }
        n
    }

    pub(crate) fn write_path(&self, i: u32, out: &mut Vec<u8>) {
        let mut buf = [0u32; MAX_DEPTH];
        let n = self.chain(i, &mut buf);
        out.clear();
        for k in (0..n).rev() {
            if k + 1 != n {
                out.push(b'\\');
            }
            out.extend_from_slice(self.name_bytes(buf[k]));
        }
        if n == 1 && self.flags[buf[0] as usize] & F_ROOT != 0 {
            out.push(b'\\');
        }
    }

    /// `C:\a\b\c`; a volume root is `C:\`.
    pub fn full_path(&self, i: u32) -> String {
        let mut v = Vec::new();
        self.write_path(i, &mut v);
        String::from_utf8(v).unwrap_or_default()
    }

    /// O(1) live (files, folders) of a volume (no deleted slots, stubs or the root).
    pub fn counts(&self, vol: u8) -> (u64, u64) {
        self.vcount.get(vol as usize).copied().unwrap_or_default()
    }

    /// Adjust a volume's counter for a record with `flags` (no-op for stubs/roots/deleted).
    #[inline]
    fn count_adj(&mut self, vol: u8, flags: u8, add: bool) {
        if flags & (F_STUB | F_ROOT | F_DELETED) != 0 {
            return;
        }
        if self.vcount.is_empty() {
            self.vcount = vec![(0, 0); 256];
        }
        let e = &mut self.vcount[vol as usize];
        let c = if flags & F_DIR != 0 {
            &mut e.1
        } else {
            &mut e.0
        };
        if add {
            *c += 1;
        } else {
            *c -= 1;
        }
    }

    /// One-time recount (snapshot load).
    pub(crate) fn recount(&mut self) {
        self.vcount.clear();
        for i in 0..self.parent.len() {
            let (v, f) = (self.vol[i], self.flags[i]);
            self.count_adj(v, f, true);
        }
    }

    // ---------------------------------------------------------------- alloc
    fn push_name(&mut self, name: &str) -> (u32, u16) {
        let name = clamp_name(name);
        assert!(
            self.arena.len() + name.len() <= u32::MAX as usize,
            "name arena overflow"
        );
        let off = self.arena.len() as u32;
        room(&mut self.arena, name.len());
        self.arena.extend_from_slice(name.as_bytes());
        (off, name.len() as u16)
    }

    fn set_name(&mut self, i: u32, name: &str) {
        let name = clamp_name(name);
        let iu = i as usize;
        let old = self.name_len[iu] as usize;
        if name.len() <= old {
            let o = self.name_off[iu] as usize;
            self.arena[o..o + name.len()].copy_from_slice(name.as_bytes());
            self.name_len[iu] = name.len() as u16;
            self.garbage += old - name.len();
        } else {
            let (o, l) = self.push_name(name);
            self.name_off[iu] = o;
            self.name_len[iu] = l;
            self.garbage += old;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn alloc(
        &mut self,
        vol: u8,
        frn: u64,
        parent: u32,
        name: &str,
        flags: u8,
        size: u64,
        mtime: i64,
    ) -> u32 {
        let (off, len) = self.push_name(name);
        self.live += 1;
        self.count_adj(vol, flags, true);
        if let Some(r) = self.free.pop() {
            let i = r as usize;
            self.parent[i] = parent;
            self.name_off[i] = off;
            self.name_len[i] = len;
            self.flags[i] = flags;
            self.vol[i] = vol;
            self.frn[i] = frn;
            if self.with_meta {
                self.size[i] = size;
                self.mtime[i] = mtime;
            }
            return r;
        }
        let i = self.parent.len();
        assert!(i < (u32::MAX - 2) as usize, "too many records");
        if self.parent.len() == self.parent.capacity() {
            room(&mut self.parent, 1);
            room(&mut self.name_off, 1);
            room(&mut self.name_len, 1);
            room(&mut self.flags, 1);
            room(&mut self.vol, 1);
            room(&mut self.frn, 1);
            if self.with_meta {
                room(&mut self.size, 1);
                room(&mut self.mtime, 1);
            }
        }
        self.parent.push(parent);
        self.name_off.push(off);
        self.name_len.push(len);
        self.flags.push(flags);
        self.vol.push(vol);
        self.frn.push(frn);
        if self.with_meta {
            self.size.push(size);
            self.mtime.push(mtime);
        }
        i as u32
    }

    pub(crate) fn add_volume_raw(&mut self, letter: u8, root_frn: u64) -> u32 {
        if let Some(r) = self.root_of(letter) {
            return r;
        }
        let name = format!("{}:", letter as char);
        let root = self.alloc(
            letter,
            norm_frn(root_frn),
            NONE,
            &name,
            F_DIR | F_ROOT,
            0,
            0,
        );
        self.folders.insert((letter, norm_frn(root_frn)), root);
        self.vols.push(VolumeMeta {
            letter,
            root,
            ..Default::default()
        });
        root
    }

    /// Add a volume root (if not present). `root_frn` is 5 on NTFS.
    pub fn add_volume(&mut self, letter: u8, root_frn: u64) -> u32 {
        self.add_volume_raw(letter, root_frn)
    }

    /// Find-or-insert a folder by FRN; unknown -> stub folder attached to the volume root.
    pub fn ensure_folder(&mut self, vol: u8, frn: u64) -> u32 {
        let frn = norm_frn(frn);
        if let Some(&i) = self.folders.get(&(vol, frn)) {
            return i;
        }
        let root = self.add_volume_raw(vol, 5);
        let stub = format!("<{:x}>", frn);
        let i = self.alloc(vol, frn, root, &stub, F_DIR | F_STUB, 0, 0);
        self.folders.insert((vol, frn), i);
        if self.links_on {
            self.kid_insert(i);
        }
        i
    }

    // ---------------------------------------------------------------- (parent, name) lookup
    fn valid_linked(&self, r: usize) -> bool {
        self.flags[r] & F_DELETED == 0 && self.parent[r] != NONE
    }

    /// Order of the lookup structure: parent, then exact name bytes, then slot.
    #[inline]
    fn kid_cmp(&self, x: u32, parent: u32, name: &[u8], idx: u32) -> Ordering {
        self.parent[x as usize]
            .cmp(&parent)
            .then_with(|| self.name_bytes(x).cmp(name))
            .then_with(|| x.cmp(&idx))
    }

    /// Build the lookup structure on the first mutation: every linked record sorted by
    /// (parent, name, slot) in one chunked array, 4 B a record (Everything keys its file tree
    /// db+0xb0 the same way). A counting sort by parent, then the names inside each folder.
    fn ensure_links(&mut self) {
        if self.links_on {
            return;
        }
        let n = self.parent.len();
        let mut start = vec![0u32; n + 1];
        let mut count = 0usize;
        for r in 0..n {
            if self.valid_linked(r) {
                start[self.parent[r] as usize + 1] += 1;
                count += 1;
            }
        }
        for k in 0..n {
            start[k + 1] += start[k];
        }
        let mut v = vec![0u32; count];
        for r in 0..n {
            if self.valid_linked(r) {
                let p = self.parent[r] as usize;
                v[start[p] as usize] = r as u32;
                start[p] += 1;
            }
        }
        drop(start);
        // Sort every folder's run by name, the runs split over threads at folder boundaries.
        let hw = std::thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(1);
        let nt = hw.min(v.len() / (1 << 16)).max(1);
        let mut parts: Vec<&mut [u32]> = Vec::with_capacity(nt);
        let mut rest: &mut [u32] = &mut v;
        for k in (1..=nt).rev() {
            let mut cut = rest.len() / k;
            while cut > 0
                && cut < rest.len()
                && self.parent[rest[cut] as usize] == self.parent[rest[cut - 1] as usize]
            {
                cut += 1;
            }
            let cut = cut.min(rest.len());
            let (a, b) = std::mem::take(&mut rest).split_at_mut(cut);
            parts.push(a);
            rest = b;
        }
        let sort_runs = |s: &mut [u32]| {
            let mut a = 0;
            while a < s.len() {
                let p = self.parent[s[a] as usize];
                let mut b = a + 1;
                while b < s.len() && self.parent[s[b] as usize] == p {
                    b += 1;
                }
                if b - a > 1 {
                    s[a..b].sort_unstable_by(|&x, &y| {
                        self.name_bytes(x).cmp(self.name_bytes(y)).then(x.cmp(&y))
                    });
                }
                a = b;
            }
        };
        std::thread::scope(|sc| {
            for part in parts {
                sc.spawn(move || sort_runs(part));
            }
        });
        self.kids = Chunks::from_sorted_plain(&v);
        self.links_on = true;
    }

    /// Record `r` must already have its final parent/name.
    fn kid_insert(&mut self, r: u32) {
        let mut kids = std::mem::take(&mut self.kids);
        let (p, name) = (self.parent[r as usize], self.name_bytes(r));
        kids.insert(
            r,
            |x| self.kid_cmp(x, p, name, r) == Ordering::Less,
            &mut Vec::new(),
        );
        self.kids = kids;
    }

    /// Call BEFORE changing parent/name/deleting `r`.
    fn kid_remove(&mut self, r: u32) {
        let (p, name) = (self.parent[r as usize], self.name_bytes(r));
        let at = self
            .kids
            .lower_bound(|x| self.kid_cmp(x, p, name, r) == Ordering::Less);
        if self.kids.get(at) == Some(r) {
            self.kids.remove_at(at);
        }
    }

    /// The children of folder `p`.
    fn kids_of(&self, p: u32, out: &mut Vec<u32>) {
        let mut at = self.kids.lower_bound(|x| self.parent[x as usize] < p);
        while let Some(x) = self.kids.get(at) {
            if self.parent[x as usize] != p {
                break;
            }
            out.push(x);
            self.kids.advance(&mut at);
        }
    }

    fn tbl_find(&self, parent: u32, name: &[u8], kind: Option<bool>) -> Option<u32> {
        let mut at = self
            .kids
            .lower_bound(|x| self.kid_cmp(x, parent, name, 0) == Ordering::Less);
        while let Some(x) = self.kids.get(at) {
            if self.parent[x as usize] != parent || self.name_bytes(x) != name {
                return None;
            }
            if kind.is_none_or(|k| (self.flags[x as usize] & F_DIR != 0) == k) {
                return Some(x);
            }
            self.kids.advance(&mut at);
        }
        None
    }

    /// Exact-name child lookup (builds the mutation structures on first use).
    pub fn find_child(&mut self, parent: u32, name: &str) -> Option<u32> {
        self.ensure_links();
        self.tbl_find(parent, name.as_bytes(), None)
    }
    /// File lookup by (volume, parent FRN, exact name) for USN delete/rename.
    pub fn find_file(&mut self, vol: u8, parent_frn: u64, name: &str) -> Option<u32> {
        let p = self.folder_idx(vol, parent_frn)?;
        self.ensure_links();
        self.tbl_find(p, name.as_bytes(), Some(false))
    }

    // ---------------------------------------------------------------- order bookkeeping
    fn add_pending(&mut self, r: u32) {
        let f = &mut self.flags[r as usize];
        if *f & (F_PENDING | F_STUB | F_ROOT) == 0 {
            *f |= F_PENDING;
            self.pending.push(r);
        }
        self.dirty = true;
    }
    fn mark_moved(&mut self, r: u32) {
        if self.flags[r as usize] & F_INORDER != 0 {
            self.flags[r as usize] |= F_STALE;
            self.touched.push(r);
        }
        self.add_pending(r);
    }

    // ---------------------------------------------------------------- mutation API
    /// Insert (or idempotently update) a record. Unknown parent FRN -> lazy stub.
    /// A folder whose FRN is already known (e.g. a stub) is filled/moved in place.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        vol: u8,
        frn: u64,
        parent_frn: u64,
        name: &str,
        is_dir: bool,
        size: u64,
        mtime: i64,
    ) -> u32 {
        let (frn, pf) = (norm_frn(frn), norm_frn(parent_frn));
        self.ensure_links();
        let parent = if pf == frn {
            self.add_volume_raw(vol, 5)
        } else {
            self.ensure_folder(vol, pf)
        };
        if is_dir {
            if let Some(&e) = self.folders.get(&(vol, frn)) {
                if self.flags[e as usize] & F_ROOT != 0 {
                    return e;
                }
                if self.flags[e as usize] & F_STUB != 0 {
                    self.kid_remove(e);
                    self.parent[e as usize] = parent;
                    self.set_name(e, name);
                    self.flags[e as usize] &= !F_STUB;
                    self.count_adj(vol, F_DIR, true);
                    self.kid_insert(e);
                    self.update_meta(e, size, mtime);
                    self.add_pending(e);
                } else {
                    if self.parent[e as usize] != parent || self.name(e) != name {
                        self.rename(e, parent, name);
                    }
                    self.update_meta(e, size, mtime);
                }
                return e;
            }
        } else if let Some(e) = self.tbl_find(parent, name.as_bytes(), Some(false)) {
            self.update_meta(e, size, mtime);
            return e;
        }
        let flags = if is_dir { F_DIR } else { 0 };
        let i = self.alloc(vol, frn, parent, name, flags, size, mtime);
        self.kid_insert(i);
        if is_dir {
            self.folders.insert((vol, frn), i);
        }
        self.add_pending(i);
        i
    }

    /// Remove a record; folders are removed recursively. Returns the number of records removed.
    pub fn remove(&mut self, idx: u32) -> usize {
        let iu = idx as usize;
        if iu >= self.parent.len() || self.flags[iu] & (F_DELETED | F_ROOT) != 0 {
            return 0;
        }
        self.ensure_links();
        let mut stack = vec![idx];
        let mut all = Vec::new();
        while let Some(r) = stack.pop() {
            all.push(r);
            if self.flags[r as usize] & F_DIR != 0 {
                self.kids_of(r, &mut stack);
            }
        }
        // A big subtree leaves in one pass over the lookup structure, a small one entry by entry.
        let bulk = all.len() > 4096;
        for &r in &all {
            let ru = r as usize;
            if !bulk {
                self.kid_remove(r);
            }
            if self.flags[ru] & F_DIR != 0 {
                let key = (self.vol[ru], self.frn[ru]);
                if self.folders.get(&key) == Some(&r) {
                    self.folders.remove(&key);
                }
            }
            self.garbage += self.name_len[ru] as usize;
            self.name_len[ru] = 0;
            self.parent[ru] = NONE;
            self.count_adj(self.vol[ru], self.flags[ru], false);
            self.flags[ru] = (self.flags[ru] & (F_INORDER | F_PENDING)) | F_DELETED;
            if self.flags[ru] & F_INORDER != 0 {
                self.touched.push(r);
            }
            self.live -= 1;
            if self.flags[ru] & (F_INORDER | F_PENDING) == 0 {
                self.flags[ru] |= F_FREED;
                self.free.push(r);
            } else {
                self.dirty = true;
            }
        }
        if bulk {
            let flags = &self.flags;
            self.kids.retain_all(|x| flags[x as usize] & F_DELETED == 0);
        }
        all.len()
    }

    pub fn remove_file(&mut self, vol: u8, parent_frn: u64, name: &str) -> usize {
        match self.find_file(vol, parent_frn, name) {
            Some(i) => self.remove(i),
            None => 0,
        }
    }
    pub fn remove_folder(&mut self, vol: u8, frn: u64) -> usize {
        match self.folder_idx(vol, frn) {
            Some(i) => self.remove(i),
            None => 0,
        }
    }

    /// Move and/or rename. Returns false if `idx` is invalid, a root, or the move would
    /// create a cycle.
    pub fn rename(&mut self, idx: u32, new_parent: u32, new_name: &str) -> bool {
        let iu = idx as usize;
        if iu >= self.parent.len()
            || new_parent as usize >= self.parent.len()
            || self.flags[iu] & (F_DELETED | F_ROOT) != 0
            || self.flags[new_parent as usize] & (F_DELETED | F_DIR) != F_DIR
        {
            return false;
        }
        let mut p = new_parent;
        let mut guard = 0;
        while p != NONE && guard < 100_000 {
            if p == idx {
                return false;
            }
            p = self.parent[p as usize];
            guard += 1;
        }
        self.ensure_links();
        self.kid_remove(idx);
        self.parent[iu] = new_parent;
        self.set_name(idx, new_name);
        self.kid_insert(idx);
        if self.flags[iu] & F_STUB == 0 {
            self.mark_moved(idx);
        }
        true
    }

    pub fn update_meta(&mut self, idx: u32, size: u64, mtime: i64) {
        if self.with_meta && (idx as usize) < self.parent.len() && !self.is_deleted(idx) {
            self.size[idx as usize] = size;
            self.mtime[idx as usize] = mtime;
        }
    }

    // ---------------------------------------------------------------- ordering
    pub(crate) fn cmp_idx(&self, a: u32, b: u32) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let o = cmp_fold(self.name(a), self.name(b));
        if o != Ordering::Equal {
            return o;
        }
        let (pa, pb) = (self.parent[a as usize], self.parent[b as usize]);
        if pa != pb {
            let (mut ba, mut bb) = ([0u32; MAX_DEPTH], [0u32; MAX_DEPTH]);
            let (na, nb) = (self.chain(pa, &mut ba), self.chain(pb, &mut bb));
            let mut k = 0;
            while k < na && k < nb {
                let (x, y) = (ba[na - 1 - k], bb[nb - 1 - k]);
                if x != y {
                    let o = cmp_fold(self.name(x), self.name(y));
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                k += 1;
            }
            let o = na.cmp(&nb);
            if o != Ordering::Equal {
                return o;
            }
        }
        a.cmp(&b)
    }

    fn merge(&self, a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if self.cmp_idx(b[j], a[i]) == Ordering::Less {
                out.push(b[j]);
                j += 1;
            } else {
                out.push(a[i]);
                i += 1;
            }
        }
        out.extend_from_slice(&a[i..]);
        out.extend_from_slice(&b[j..]);
        out
    }

    pub(crate) fn par_sort(&self, v: &mut Vec<u32>) {
        let n = v.len();
        let hw = std::thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(1);
        let nt = hw.min(n / 32768).max(1);
        if nt <= 1 {
            v.sort_unstable_by(|&a, &b| self.cmp_idx(a, b));
            return;
        }
        let chunk = n.div_ceil(nt);
        std::thread::scope(|s| {
            for c in v.chunks_mut(chunk) {
                s.spawn(move || c.sort_unstable_by(|&a, &b| self.cmp_idx(a, b)));
            }
        });
        let mut runs: Vec<Vec<u32>> = v.chunks(chunk).map(|c| c.to_vec()).collect();
        while runs.len() > 1 {
            let mut pairs: Vec<(Vec<u32>, Option<Vec<u32>>)> = Vec::new();
            let mut it = runs.into_iter();
            while let Some(a) = it.next() {
                pairs.push((a, it.next()));
            }
            runs = std::thread::scope(|s| {
                let hs: Vec<_> = pairs
                    .iter()
                    .map(|(a, b)| {
                        s.spawn(move || match b {
                            Some(b) => self.merge(a, b),
                            None => a.clone(),
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap()).collect()
            });
        }
        *v = runs.pop().unwrap_or_default();
    }

    fn merged(&self) -> Vec<u32> {
        let add: Vec<u32> = self
            .pending
            .iter()
            .copied()
            .filter(|&r| self.flags[r as usize] & F_DELETED == 0)
            .collect();
        let mut old: Vec<u32> = self
            .sorted
            .iter()
            .filter(|&r| self.flags[r as usize] & (F_DELETED | F_STALE) == 0)
            .collect();
        if add.len() * 8 > old.len() + add.len() + 64 {
            old.extend(add);
            self.par_sort(&mut old);
            old
        } else {
            let mut add = add;
            add.sort_unstable_by(|&a, &b| self.cmp_idx(a, b));
            self.merge(&old, &add)
        }
    }

    /// The up-to-date sorted order as one list (a copy; merged when dirty).
    pub fn order_vec(&self) -> Vec<u32> {
        if self.dirty {
            self.merged()
        } else {
            self.sorted.to_vec()
        }
    }

    /// The up-to-date sorted order for readers: the live chunks when clean, a merged copy
    /// with dense ranks when dirty (a reader never mutates; the writer refreshes before it
    /// releases the lock, so this fallback is rare).
    pub(crate) fn order_view(&self) -> OrderView<'_> {
        if self.dirty {
            let order = self.merged();
            let rank = self.compute_rank(&order);
            OrderView::Flat { order, rank }
        } else {
            OrderView::Live {
                ch: &self.sorted,
                rank: &self.rank,
            }
        }
    }

    /// Install a sorted list as the order and code every rank from it.
    pub(crate) fn set_sorted(&mut self, v: Vec<u32>) {
        self.sorted = Chunks::from_sorted(&v, &mut self.rank, self.parent.len());
    }

    /// Fold pending mutations into the sorted order and recycle freed slots.
    ///
    /// Small change sets (the USN steady state) touch only the blocks they land in: leavers
    /// are dropped from the blocks their codes name, newcomers are binary-searched into
    /// place, and only those blocks get their codes rewritten (Everything's chunked arrays:
    /// an insert moves one 64 KB chunk). Nothing walks every record or copies the order, so
    /// a batch costs microseconds whatever the index size.
    pub fn refresh(&mut self) {
        if !self.dirty {
            return;
        }
        let n = self.sorted.len();
        if (self.pending.len() + self.touched.len()) * 8 > n + 64 {
            self.refresh_full();
            return;
        }
        if self.rank.len() < self.parent.len() {
            let more = self.parent.len() - self.rank.len();
            room(&mut self.rank, more);
            self.rank.resize(self.parent.len(), NONE);
        }
        let leaving = |f: u8| f & F_INORDER != 0 && f & (F_DELETED | F_STALE) != 0;
        let mut ch = std::mem::take(&mut self.sorted);
        let mut blocks: Vec<u32> = Vec::new();
        // Leavers (renamed/moved: re-added from `pending`; deleted: gone) leave their blocks.
        let codes: Vec<u32> = self
            .touched
            .iter()
            .filter(|&&r| leaving(self.flags[r as usize]))
            .map(|&r| self.rank[r as usize])
            .collect();
        ch.remove_where(&codes, |r| leaving(self.flags[r as usize]), &mut blocks);
        let mut add: Vec<u32> = self
            .pending
            .iter()
            .copied()
            .filter(|&r| self.flags[r as usize] & F_DELETED == 0)
            .collect();
        add.sort_unstable_by(|&a, &b| self.cmp_idx(a, b));
        add.dedup();
        for &a in &add {
            ch.insert(a, |x| self.cmp_idx(x, a) == Ordering::Less, &mut blocks);
        }

        // Flags, ranks of leavers and the free list: only the records that changed.
        for i in 0..self.touched.len() {
            let r = self.touched[i] as usize;
            self.flags[r] &= !(F_INORDER | F_STALE);
            self.rank[r] = NONE; // a re-added (moved) record gets its new code below
        }
        for i in 0..self.touched.len().max(self.pending.len()) {
            for r in [self.touched.get(i), self.pending.get(i)]
                .into_iter()
                .flatten()
                .map(|&r| r as usize)
            {
                if self.flags[r] & F_DELETED != 0 && self.flags[r] & F_FREED == 0 {
                    self.flags[r] |= F_FREED;
                    self.free.push(r as u32);
                }
            }
        }
        for i in 0..self.pending.len() {
            let r = self.pending[i] as usize;
            self.flags[r] &= !F_PENDING;
            if self.flags[r] & F_DELETED == 0 {
                self.flags[r] |= F_INORDER;
            } else {
                self.rank[r] = NONE;
            }
        }
        ch.recode(&mut blocks, &mut self.rank);
        self.sorted = ch;
        self.pending.clear();
        self.touched.clear();
        self.dirty = false;
        if self.garbage > (self.arena.len() / 4).max(1 << 20) {
            self.compact_names();
        }
    }

    /// Whole-index refresh, for large change sets.
    fn refresh_full(&mut self) {
        let new = self.merged();
        let old = self.sorted.to_vec();
        for &r in old.iter().chain(self.pending.iter()) {
            let f = &mut self.flags[r as usize];
            if *f & F_DELETED != 0 && *f & F_FREED == 0 {
                *f |= F_FREED;
                self.free.push(r);
            }
        }
        for &r in &old {
            self.flags[r as usize] &= !(F_INORDER | F_STALE);
        }
        drop(old);
        for &r in &self.pending {
            self.flags[r as usize] &= !F_PENDING;
        }
        for &r in &new {
            self.flags[r as usize] |= F_INORDER;
        }
        self.set_sorted(new);
        self.pending.clear();
        self.touched.clear();
        self.dirty = false;
        if self.garbage > (self.arena.len() / 4).max(1 << 20) {
            self.compact_names();
        }
    }

    /// Full rebuild of the sorted order from all records (parallel sort).
    pub fn rebuild_order(&mut self) {
        let n = self.parent.len();
        let mut v = Vec::with_capacity(n);
        for r in 0..n {
            let f = &mut self.flags[r];
            *f &= !(F_INORDER | F_STALE | F_PENDING);
            if *f & F_DELETED != 0 {
                if *f & F_FREED == 0 {
                    *f |= F_FREED;
                    self.free.push(r as u32);
                }
            } else if *f & (F_ROOT | F_STUB) == 0 {
                v.push(r as u32);
            }
        }
        self.par_sort(&mut v);
        for &r in &v {
            self.flags[r as usize] |= F_INORDER;
        }
        self.set_sorted(v);
        self.pending.clear();
        self.touched.clear();
        self.dirty = false;
    }

    pub(crate) fn compute_rank(&self, order: &[u32]) -> Vec<u32> {
        let mut r = vec![NONE; self.parent.len()];
        for (k, &i) in order.iter().enumerate() {
            r[i as usize] = k as u32;
        }
        r
    }

    /// Rewrite the name arena without garbage.
    pub fn compact_names(&mut self) {
        let mut arena = Vec::with_capacity(self.arena.len() - self.garbage.min(self.arena.len()));
        for r in 0..self.parent.len() {
            let (o, l) = (self.name_off[r] as usize, self.name_len[r] as usize);
            self.name_off[r] = arena.len() as u32;
            arena.extend_from_slice(&self.arena[o..o + l]);
        }
        self.arena = arena;
        self.garbage = 0;
    }
}

/// Bulk builder. `push` is O(1) per record; parents are resolved in `finish`.
pub struct IndexBuilder {
    idx: Index,
    parent_frn: Vec<u64>,
}

impl IndexBuilder {
    pub fn new(with_meta: bool) -> Self {
        IndexBuilder {
            idx: Index::new(with_meta),
            parent_frn: Vec::new(),
        }
    }
    pub fn with_capacity(with_meta: bool, records: usize) -> Self {
        let mut b = Self::new(with_meta);
        let i = &mut b.idx;
        i.parent.reserve(records);
        i.name_off.reserve(records);
        i.name_len.reserve(records);
        i.flags.reserve(records);
        i.vol.reserve(records);
        i.frn.reserve(records);
        if with_meta {
            i.size.reserve(records);
            i.mtime.reserve(records);
        }
        i.arena.reserve(records * 20);
        b.parent_frn.reserve(records);
        b
    }
    /// Register a volume (root FRN is 5 on NTFS). Called automatically by `push` if missing.
    pub fn add_volume(&mut self, letter: u8, root_frn: u64) -> u32 {
        let r = self.idx.add_volume_raw(letter, root_frn);
        while self.parent_frn.len() < self.idx.parent.len() {
            self.parent_frn.push(0);
        }
        r
    }
    pub fn volume_mut(&mut self, letter: u8) -> Option<&mut VolumeMeta> {
        self.idx.volume_mut(letter)
    }
    /// See [`Index::alloc_id`].
    pub fn alloc_id(&mut self, vol: u8) -> u64 {
        self.add_volume(vol, 5);
        self.idx.alloc_id(vol)
    }
    /// See [`Index::intern128`].
    pub fn intern128(&mut self, vol: u8, frn: u128) -> u64 {
        self.add_volume(vol, 5);
        self.idx.intern128(vol, frn)
    }
    /// See [`Index::set_id128`].
    pub fn set_id128(&mut self, vol: u8, frn: u128, id: u64) {
        self.idx.set_id128(vol, frn, id);
    }
    pub fn len(&self) -> usize {
        self.idx.parent.len()
    }
    pub fn is_empty(&self) -> bool {
        self.idx.parent.is_empty()
    }
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        vol: u8,
        frn: u64,
        parent_frn: u64,
        name: &str,
        is_dir: bool,
        size: u64,
        mtime: i64,
    ) {
        let frn = norm_frn(frn);
        if self.idx.root_of(vol).is_none() {
            self.add_volume(vol, 5);
        }
        if let Some(&e) = self.idx.folders.get(&(vol, frn)) {
            if self.idx.flags[e as usize] & F_ROOT != 0 {
                return; // MFT root entry "." (parent = itself)
            }
        }
        let flags = if is_dir { F_DIR } else { 0 };
        let i = self.idx.alloc(vol, frn, NONE, name, flags, size, mtime);
        debug_assert_eq!(i as usize, self.parent_frn.len());
        self.parent_frn.push(norm_frn(parent_frn));
        if is_dir {
            self.idx.folders.insert((vol, frn), i);
        }
    }

    /// Resolve parents but do not sort (for timing).
    pub fn finish_unsorted(mut self) -> Index {
        let idx = &mut self.idx;
        let n = self.parent_frn.len();
        for r in 0..n {
            if idx.flags[r] & F_ROOT != 0 {
                continue;
            }
            let (vol, frn, pf) = (idx.vol[r], idx.frn[r], self.parent_frn[r]);
            let p = if pf == frn {
                idx.root_of(vol).unwrap_or(NONE)
            } else {
                idx.ensure_folder(vol, pf) // find-or-insert: unseen parent => stub
            };
            idx.parent[r] = p;
        }
        self.idx
    }

    pub fn finish(self) -> Index {
        let mut i = self.finish_unsorted();
        i.rebuild_order();
        i
    }
}
