//! Struct-of-arrays record store, live mutation, name-sorted order array.
//!
//! # Memory layout (bytes per record slot)
//! `parent` 4, `name_off` 4, `name_len` 2, `flags` 1, `vol` 1, `frn` 8, `order` 4,
//! name arena ~avg name length (UTF-8, no terminator), optional `size` 8 + `mtime` 8.
//! That is ~24 B + names without meta.
//! The `HashMap<(vol,frn),idx>` holds **folders only** (~25 B per folder entry).
//! The *mutation structures* (child links + (parent,name) hash table) are built lazily
//! on the first mutation: `first_child/next/prev` 12 B + open-addressing `u32` table
//! (load 0.25-0.7, ~6-16 B) = ~20-28 B per record. A query-only index never pays for them.
//!
//! # Lookup of files
//! Files are not in the FRN map. `(parent, exact name)` -> idx goes through the open
//! addressing table keyed by `hash(parent, name bytes)`; O(1) expected, tombstones on
//! delete, rebuilt when load > 0.7. Children enumeration (recursive delete) uses the
//! doubly linked sibling list.
//!
//! # Sort maintenance
//! `order: Vec<u32>` is sorted by (folded name, then full path, then idx). Mutations
//! never touch it directly: a renamed/moved record is flagged `STALE` (still in `order`
//! at its old position) and queued in `pending`; new records are queued in `pending`;
//! removed records get `DELETED`. [`Index::refresh`] drops stale/deleted entries,
//! sorts the (small) pending list and does one linear merge (or a full parallel sort if
//! pending is > 1/8 of the index). Readers on a dirty index (`search(&self)`) get the
//! same merge computed on the fly, so results are always correct, just slower.
//! Record slots of deleted records are recycled via a free list *after* refresh has
//! dropped them from `order`. Caveat: renaming a folder does not re-sort descendants;
//! only the path tie-break between equal names may be stale until the next full rebuild.
//!
//! # Stubs / orphans
//! A record whose parent FRN is unknown gets a lazy *stub folder* (name `<hexfrn>`,
//! attached to the volume root, in the FRN map, not searchable). When the real folder
//! record for that FRN arrives (`insert`), the stub is filled in place, so children keep
//! their parent index. Builder entries whose parent FRN is never seen keep the stub.

use crate::fold::cmp_fold;
use std::borrow::Cow;
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
const EMPTY: u32 = u32::MAX;
const TOMB: u32 = u32::MAX - 1;

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

/// Per-volume state. `serial`, `journal_id`, `next_usn` are opaque, set by the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeMeta {
    pub letter: u8,
    pub serial: u64,
    pub journal_id: u64,
    pub next_usn: i64,
    /// Record index of the volume root.
    pub root: u32,
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
    pub(crate) vols: Vec<VolumeMeta>,
    pub(crate) order: Vec<u32>,
    /// slot -> position in `order` (NONE if not searchable); lets scans run in storage order
    pub(crate) rank: Vec<u32>,
    pub(crate) pending: Vec<u32>,
    pub(crate) free: Vec<u32>,
    pub(crate) dirty: bool,
    pub(crate) live: usize,
    // lazily built mutation structures
    links_on: bool,
    first: Vec<u32>,
    next: Vec<u32>,
    prev: Vec<u32>,
    tbl: Vec<u32>,
    tbl_used: usize,
    tbl_tomb: usize,
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

fn hash_key(parent: u32, name: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ (parent as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for &b in name {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^ (h >> 32)
}

impl Index {
    pub fn new(with_meta: bool) -> Self {
        Index { with_meta, ..Default::default() }
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
        self.clean_order().len()
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
            + self.order.capacity() * 4
            + self.rank.capacity() * 4
            + self.folders.capacity() * 25
            + (self.first.capacity() + self.next.capacity() + self.prev.capacity() + self.tbl.capacity()) * 4
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

    // ---------------------------------------------------------------- alloc
    fn push_name(&mut self, name: &str) -> (u32, u16) {
        let name = clamp_name(name);
        assert!(self.arena.len() + name.len() <= u32::MAX as usize, "name arena overflow");
        let off = self.arena.len() as u32;
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
    pub(crate) fn alloc(&mut self, vol: u8, frn: u64, parent: u32, name: &str, flags: u8, size: u64, mtime: i64) -> u32 {
        let (off, len) = self.push_name(name);
        self.live += 1;
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
            if self.links_on {
                self.first[i] = NONE;
                self.next[i] = NONE;
                self.prev[i] = NONE;
            }
            return r;
        }
        let i = self.parent.len();
        assert!(i < (u32::MAX - 2) as usize, "too many records");
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
        if self.links_on {
            self.first.push(NONE);
            self.next.push(NONE);
            self.prev.push(NONE);
        }
        i as u32
    }

    pub(crate) fn add_volume_raw(&mut self, letter: u8, root_frn: u64) -> u32 {
        if let Some(r) = self.root_of(letter) {
            return r;
        }
        let name = format!("{}:", letter as char);
        let root = self.alloc(letter, norm_frn(root_frn), NONE, &name, F_DIR | F_ROOT, 0, 0);
        self.folders.insert((letter, norm_frn(root_frn)), root);
        self.vols.push(VolumeMeta { letter, root, ..Default::default() });
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
            self.link(i);
            self.tbl_insert(i);
        }
        i
    }

    // ---------------------------------------------------------------- links + table
    fn valid_linked(&self, r: usize) -> bool {
        self.flags[r] & F_DELETED == 0 && self.parent[r] != NONE
    }

    fn ensure_links(&mut self) {
        if self.links_on {
            return;
        }
        let n = self.parent.len();
        self.first = vec![NONE; n];
        self.next = vec![NONE; n];
        self.prev = vec![NONE; n];
        self.links_on = true;
        for r in 0..n {
            if self.valid_linked(r) {
                self.link(r as u32);
            }
        }
        self.tbl_rebuild();
    }

    fn link(&mut self, r: u32) {
        let p = self.parent[r as usize] as usize;
        let f = self.first[p];
        self.next[r as usize] = f;
        self.prev[r as usize] = NONE;
        if f != NONE {
            self.prev[f as usize] = r;
        }
        self.first[p] = r;
    }

    fn unlink(&mut self, r: u32) {
        let (ru, p) = (r as usize, self.parent[r as usize]);
        let (pr, nx) = (self.prev[ru], self.next[ru]);
        if pr != NONE {
            self.next[pr as usize] = nx;
        } else if p != NONE {
            self.first[p as usize] = nx;
        }
        if nx != NONE {
            self.prev[nx as usize] = pr;
        }
        self.prev[ru] = NONE;
        self.next[ru] = NONE;
    }

    fn tbl_rebuild(&mut self) {
        let n = self.parent.len();
        let count = (0..n).filter(|&r| self.valid_linked(r)).count();
        let cap = (count * 2 + 2).next_power_of_two().max(16);
        self.tbl = vec![EMPTY; cap];
        self.tbl_used = 0;
        self.tbl_tomb = 0;
        for r in 0..n {
            if self.valid_linked(r) {
                self.tbl_put(r as u32);
            }
        }
    }

    fn tbl_put(&mut self, r: u32) {
        let h = hash_key(self.parent[r as usize], self.name_bytes(r)) as usize;
        let mask = self.tbl.len() - 1;
        let mut s = h & mask;
        loop {
            let v = self.tbl[s];
            if v == EMPTY || v == TOMB {
                if v == TOMB {
                    self.tbl_tomb -= 1;
                }
                self.tbl[s] = r;
                self.tbl_used += 1;
                return;
            }
            s = (s + 1) & mask;
        }
    }

    /// Record `r` must already have its final parent/name.
    fn tbl_insert(&mut self, r: u32) {
        if (self.tbl_used + self.tbl_tomb + 1) * 10 > self.tbl.len() * 7 {
            self.tbl_rebuild(); // includes r
            return;
        }
        self.tbl_put(r);
    }

    /// Call BEFORE changing parent/name/deleting `r`.
    fn tbl_remove(&mut self, r: u32) {
        let h = hash_key(self.parent[r as usize], self.name_bytes(r)) as usize;
        let mask = self.tbl.len() - 1;
        let mut s = h & mask;
        loop {
            let v = self.tbl[s];
            if v == EMPTY {
                return;
            }
            if v == r {
                self.tbl[s] = TOMB;
                self.tbl_used -= 1;
                self.tbl_tomb += 1;
                return;
            }
            s = (s + 1) & mask;
        }
    }

    fn tbl_find(&self, parent: u32, name: &[u8], kind: Option<bool>) -> Option<u32> {
        if self.tbl.is_empty() {
            return None;
        }
        let mask = self.tbl.len() - 1;
        let mut s = (hash_key(parent, name) as usize) & mask;
        loop {
            let r = self.tbl[s];
            if r == EMPTY {
                return None;
            }
            if r != TOMB {
                let ri = r as usize;
                if self.parent[ri] == parent
                    && self.name_bytes(r) == name
                    && kind.is_none_or(|k| (self.flags[ri] & F_DIR != 0) == k)
                {
                    return Some(r);
                }
            }
            s = (s + 1) & mask;
        }
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
        }
        self.add_pending(r);
    }

    // ---------------------------------------------------------------- mutation API
    /// Insert (or idempotently update) a record. Unknown parent FRN -> lazy stub.
    /// A folder whose FRN is already known (e.g. a stub) is filled/moved in place.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(&mut self, vol: u8, frn: u64, parent_frn: u64, name: &str, is_dir: bool, size: u64, mtime: i64) -> u32 {
        let (frn, pf) = (norm_frn(frn), norm_frn(parent_frn));
        self.ensure_links();
        let parent = if pf == frn { self.add_volume_raw(vol, 5) } else { self.ensure_folder(vol, pf) };
        if is_dir {
            if let Some(&e) = self.folders.get(&(vol, frn)) {
                if self.flags[e as usize] & F_ROOT != 0 {
                    return e;
                }
                if self.flags[e as usize] & F_STUB != 0 {
                    self.tbl_remove(e);
                    self.unlink(e);
                    self.parent[e as usize] = parent;
                    self.set_name(e, name);
                    self.flags[e as usize] &= !F_STUB;
                    self.link(e);
                    self.tbl_insert(e);
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
        self.link(i);
        self.tbl_insert(i);
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
                let mut c = self.first[r as usize];
                while c != NONE {
                    stack.push(c);
                    c = self.next[c as usize];
                }
            }
        }
        self.unlink(idx);
        for &r in &all {
            let ru = r as usize;
            self.tbl_remove(r);
            if self.flags[ru] & F_DIR != 0 {
                let key = (self.vol[ru], self.frn[ru]);
                if self.folders.get(&key) == Some(&r) {
                    self.folders.remove(&key);
                }
            }
            self.garbage += self.name_len[ru] as usize;
            self.name_len[ru] = 0;
            self.parent[ru] = NONE;
            self.first[ru] = NONE;
            self.next[ru] = NONE;
            self.prev[ru] = NONE;
            self.flags[ru] = (self.flags[ru] & (F_INORDER | F_PENDING)) | F_DELETED;
            self.live -= 1;
            if self.flags[ru] & (F_INORDER | F_PENDING) == 0 {
                self.flags[ru] |= F_FREED;
                self.free.push(r);
            } else {
                self.dirty = true;
            }
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
        self.tbl_remove(idx);
        self.unlink(idx);
        self.parent[iu] = new_parent;
        self.set_name(idx, new_name);
        self.link(idx);
        self.tbl_insert(idx);
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
        let hw = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(1);
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
        let add: Vec<u32> = self.pending.iter().copied().filter(|&r| self.flags[r as usize] & F_DELETED == 0).collect();
        let mut old: Vec<u32> = self
            .order
            .iter()
            .copied()
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

    /// The up-to-date sorted order (borrowed when clean, computed when dirty).
    pub fn clean_order(&self) -> Cow<'_, [u32]> {
        if self.dirty {
            Cow::Owned(self.merged())
        } else {
            Cow::Borrowed(&self.order)
        }
    }

    /// Fold pending mutations into the sorted order and recycle freed slots.
    pub fn refresh(&mut self) {
        if !self.dirty {
            return;
        }
        let new = self.merged();
        for &r in self.order.iter().chain(self.pending.iter()) {
            let f = &mut self.flags[r as usize];
            if *f & F_DELETED != 0 && *f & F_FREED == 0 {
                *f |= F_FREED;
                self.free.push(r);
            }
        }
        for &r in &self.order {
            self.flags[r as usize] &= !(F_INORDER | F_STALE);
        }
        for &r in &self.pending {
            self.flags[r as usize] &= !F_PENDING;
        }
        for &r in &new {
            self.flags[r as usize] |= F_INORDER;
        }
        self.rank = self.compute_rank(&new);
        self.order = new;
        self.pending.clear();
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
        self.rank = self.compute_rank(&v);
        self.order = v;
        self.pending.clear();
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
        IndexBuilder { idx: Index::new(with_meta), parent_frn: Vec::new() }
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
    pub fn len(&self) -> usize {
        self.idx.parent.len()
    }
    pub fn is_empty(&self) -> bool {
        self.idx.parent.is_empty()
    }
    #[allow(clippy::too_many_arguments)]
    pub fn push(&mut self, vol: u8, frn: u64, parent_frn: u64, name: &str, is_dir: bool, size: u64, mtime: i64) {
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
