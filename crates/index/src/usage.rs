//! Disk-usage roll-up. One pass over the record arrays builds, for every folder, what lies under
//! it (bytes, files, folders, bytes per file category) plus child lists, so listing any folder
//! afterwards costs only its own children. The roll-up is a snapshot: the daemon rebuilds it now
//! and then and every read re-checks the slots it hands out against the live index.

use crate::store::*;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hash::BuildHasherDefault;
use std::sync::OnceLock;

type Fx<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

pub const CATEGORIES: usize = 8;
const EXT_MAX: usize = 8;
/// Category bytes are kept in units of 4 KiB (`u32` holds 16 TiB per category per folder).
const CAT_SHIFT: u32 = 12;

const CATEGORY_EXTS: [(&str, u8); 7] = [
    ("jpg jpeg png gif bmp webp heic heif tif tiff svg ico raw cr2 nef arw dng psd avif jxl", 1),
    ("mp4 mkv avi mov wmv webm m4v flv mpg mpeg ts m2ts vob 3gp", 2),
    ("mp3 wav flac aac ogg m4a wma opus aiff mid", 3),
    ("pdf doc docx xls xlsx ppt pptx txt md rtf odt ods csv epub mobi djvu log", 4),
    ("zip rar 7z tar gz bz2 xz iso img cab wim vhd vhdx vmdk zst esd", 5),
    ("rs py js jsx tsx java c h cpp hpp cs go rb php html css json xml yml yaml toml sql sh ps1 bat kt swift map pyc o obj rlib pdb lock", 6),
    ("exe dll sys msi msix appx dat bin lib mui mum manifest cat drv ocx efi etl db sqlite", 7),
];

/// Lower-case extension (up to 8 bytes) packed into a `u64`; 0 = none.
pub fn ext_key(name: &[u8]) -> u64 {
    let Some(dot) = name.iter().rposition(|&b| b == b'.') else {
        return 0;
    };
    let e = &name[dot + 1..];
    if e.is_empty() || e.len() > EXT_MAX {
        return 0;
    }
    let mut k = 0u64;
    for (j, &b) in e.iter().enumerate() {
        k |= (b.to_ascii_lowercase() as u64) << (8 * j);
    }
    k
}

pub fn unpack_ext(k: u64) -> String {
    (0..EXT_MAX)
        .map(|j| ((k >> (8 * j)) & 0xFF) as u8)
        .take_while(|&b| b != 0)
        .map(|b| b as char)
        .collect()
}

fn cat_table() -> &'static Fx<u64, u8> {
    static T: OnceLock<Fx<u64, u8>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m: Fx<u64, u8> = HashMap::default();
        for (list, k) in CATEGORY_EXTS {
            for e in list.split(' ') {
                m.insert(ext_key(format!("x.{e}").as_bytes()), k);
            }
        }
        m
    })
}

/// 0 other, 1 pictures, 2 video, 3 audio, 4 documents, 5 archives, 6 code, 7 programs and system.
pub fn category_of_key(k: u64) -> u8 {
    cat_table().get(&k).copied().unwrap_or(0)
}

pub fn category(ext: &str) -> u8 {
    category_of_key(ext_key(format!("x.{ext}").as_bytes()))
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeUsage {
    /// Lower-case extension without the dot; empty = no extension.
    pub ext: String,
    pub kind: u8,
    pub bytes: u64,
    pub files: u64,
}

/// One child of a folder.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildUsage {
    pub slot: u32,
    pub is_dir: bool,
    pub bytes: u64,
    pub files: u64,
    pub folders: u64,
    /// File category (a folder: the one holding most of its bytes).
    pub kind: u8,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DirList {
    pub bytes: u64,
    pub files: u64,
    pub folders: u64,
    /// Bytes per category under the folder.
    pub cats: [u64; CATEGORIES],
    /// Biggest children first.
    pub children: Vec<ChildUsage>,
    /// Children left out of `children` and what they hold.
    pub more: u64,
    pub more_bytes: u64,
}

/// Snapshot of every folder's totals and child lists, plus a small overlay of what changed
/// since. See the module docs.
#[derive(Clone)]
pub struct Rollup {
    /// Slot of each folder, ascending.
    dirs: Vec<u32>,
    bytes: Vec<u64>,
    files: Vec<u32>,
    folders: Vec<u32>,
    cats: Vec<[u32; CATEGORIES]>,
    kd_start: Vec<u32>,
    kd: Vec<u32>,
    kf_start: Vec<u32>,
    kf: Vec<u32>,
    ov: Overlay,
}

/// Signed change of one folder's totals (`cats` in the same 4 KiB units as the snapshot).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Delta {
    bytes: i64,
    files: i64,
    folders: i64,
    cats: [i64; CATEGORIES],
}

impl Delta {
    fn add(&mut self, o: &Delta, sign: i64) {
        self.bytes += sign * o.bytes;
        self.files += sign * o.files;
        self.folders += sign * o.folders;
        for k in 0..CATEGORIES {
            self.cats[k] += sign * o.cats[k];
        }
    }
}

/// What changed under each folder since the snapshot was built, keyed by folder *slot* (so
/// folders created since have totals too). Everything's way: every add/delete/move pushes its
/// size up the parent chain, O(depth), instead of recomputing anything.
#[derive(Clone, Default)]
struct Overlay {
    by_slot: Fx<u32, Delta>,
    /// Something was added or moved that the snapshot's child lists do not show yet.
    dirty: bool,
    /// A change could not be tracked: totals are no longer exact until the next build.
    broken: bool,
}

/// What one record (with everything under it) adds to the folders above it, and which they are.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Contrib {
    chain: Vec<u32>,
    d: Delta,
}

/// The state of the records a mutation may touch, taken just before it
/// ([`Index::usage_before`]); hand it to [`Index::usage_after`] right after.
pub struct UsageTouch {
    live: usize,
    vols: usize,
    pre: Vec<(u32, Contrib)>,
}

fn argmax(c: &[u32; CATEGORIES]) -> u8 {
    let mut best = 0usize;
    for k in 1..CATEGORIES {
        if c[k] > c[best] {
            best = k;
        }
    }
    best as u8
}

impl Rollup {
    pub fn dir_count(&self) -> usize {
        self.dirs.len()
    }
    pub fn mem_bytes(&self) -> usize {
        self.dirs.len() * (4 + 8 + 4 + 4 + 32 + 4 + 4 + 4)
            + (self.kd.len() + self.kf.len()) * 4
            + self.ov.by_slot.len() * (4 + std::mem::size_of::<Delta>())
    }
    /// Folder id of a record slot (None: not a live folder when the roll-up was built).
    pub fn dir_of_slot(&self, slot: u32) -> Option<u32> {
        self.dirs.binary_search(&slot).ok().map(|d| d as u32)
    }
    /// Snapshot totals of folder id `d`.
    fn base(&self, d: u32) -> Delta {
        let du = d as usize;
        Delta {
            bytes: self.bytes[du] as i64,
            files: self.files[du] as i64,
            folders: self.folders[du] as i64,
            cats: std::array::from_fn(|k| self.cats[du][k] as i64),
        }
    }
    /// Current totals of the folder at `slot`: snapshot (if it has the folder) plus overlay.
    fn slot_sum(&self, slot: u32) -> Delta {
        let mut t = self
            .dir_of_slot(slot)
            .map(|d| self.base(d))
            .unwrap_or_default();
        if let Some(o) = self.ov.by_slot.get(&slot) {
            t.add(o, 1);
        }
        t
    }
    fn sum(&self, d: u32) -> Delta {
        let mut t = self.base(d);
        if let Some(o) = self.ov.by_slot.get(&self.dirs[d as usize]) {
            t.add(o, 1);
        }
        t
    }
    /// `(bytes, files, folders)` of a folder, changes since the build included.
    pub fn totals(&self, d: u32) -> (u64, u64, u64) {
        let t = self.sum(d);
        (
            t.bytes.max(0) as u64,
            t.files.max(0) as u64,
            t.folders.max(0) as u64,
        )
    }
    /// Bytes per category under a folder (changes since the build included).
    pub fn cats_of(&self, d: u32) -> [u64; CATEGORIES] {
        let c = self.sum(d).cats;
        std::array::from_fn(|k| (c[k].max(0) as u64) << CAT_SHIFT)
    }
    /// `(bytes, files, folders, cats)` of the live folder at record `slot`, also one created
    /// after the build (not in the child lists yet).
    pub fn totals_at(&self, slot: u32) -> (u64, u64, u64, [u64; CATEGORIES]) {
        let t = self.slot_sum(slot);
        (
            t.bytes.max(0) as u64,
            t.files.max(0) as u64,
            t.folders.max(0) as u64,
            std::array::from_fn(|k| (t.cats[k].max(0) as u64) << CAT_SHIFT),
        )
    }
    fn kind_of(&self, d: u32) -> u8 {
        let c = self.sum(d).cats;
        argmax(&std::array::from_fn(|k| {
            c[k].clamp(0, u32::MAX as i64) as u32
        }))
    }
    /// Totals reflect every change pushed through [`Index::usage_after`] since the build.
    pub fn exact(&self) -> bool {
        !self.ov.broken
    }
    /// Records were added or moved since the build: child lists lag until the next one.
    pub fn structure_dirty(&self) -> bool {
        self.ov.dirty
    }
    /// Give up exact tracking (a change went around [`Index::usage_after`]).
    pub fn mark_inexact(&mut self) {
        self.ov.broken = true;
    }
    /// Folders with overlay entries.
    pub fn overlay_len(&self) -> usize {
        self.ov.by_slot.len()
    }
    fn push(&mut self, c: &Contrib, sign: i64) {
        for &p in &c.chain {
            self.ov.by_slot.entry(p).or_default().add(&c.d, sign);
        }
    }
}

impl Index {
    #[allow(clippy::needless_range_loop)]
    pub fn rollup(&self) -> Rollup {
        let n = self.parent.len();
        let live = |i: usize| self.flags[i] & F_DELETED == 0;
        let mut dirs: Vec<u32> = Vec::new();
        let mut dir_id = vec![NONE; n];
        for i in 0..n {
            if live(i) && self.flags[i] & F_DIR != 0 {
                dir_id[i] = dirs.len() as u32;
                dirs.push(i as u32);
            }
        }
        let nd = dirs.len();
        let mut bytes = vec![0u64; nd];
        let mut files = vec![0u32; nd];
        let mut folders = vec![0u32; nd];
        let mut cats = vec![[0u32; CATEGORIES]; nd];
        let mut parent_dir = vec![NONE; nd];
        let mut kd_start = vec![0u32; nd + 1];
        let mut kf_start = vec![0u32; nd + 1];
        let pid_of = |i: usize| -> u32 {
            let p = self.parent[i] as usize;
            if p < n {
                dir_id[p]
            } else {
                NONE
            }
        };
        for i in 0..n {
            if !live(i) {
                continue;
            }
            let pid = pid_of(i);
            if self.flags[i] & F_DIR != 0 {
                let d = dir_id[i];
                if pid != NONE && pid != d {
                    parent_dir[d as usize] = pid;
                    kd_start[pid as usize + 1] += 1;
                    folders[pid as usize] += 1;
                }
            } else if pid != NONE {
                let p = pid as usize;
                kf_start[p + 1] += 1;
                files[p] += 1;
                if self.with_meta {
                    let sz = self.size[i];
                    bytes[p] = bytes[p].saturating_add(sz);
                    if sz > 0 {
                        let k = category_of_key(ext_key(self.name_bytes(i as u32))) as usize;
                        cats[p][k] = cats[p][k].saturating_add((sz >> CAT_SHIFT) as u32);
                    }
                }
            }
        }
        for d in 0..nd {
            kd_start[d + 1] += kd_start[d];
            kf_start[d + 1] += kf_start[d];
        }
        let mut kd = vec![0u32; kd_start[nd] as usize];
        let mut kf = vec![0u32; kf_start[nd] as usize];
        let mut pd = kd_start.clone();
        let mut pf = kf_start.clone();
        for i in 0..n {
            if !live(i) {
                continue;
            }
            let pid = pid_of(i);
            if pid == NONE {
                continue;
            }
            if self.flags[i] & F_DIR != 0 {
                let d = dir_id[i];
                if pid != d {
                    kd[pd[pid as usize] as usize] = d;
                    pd[pid as usize] += 1;
                }
            } else {
                kf[pf[pid as usize] as usize] = i as u32;
                pf[pid as usize] += 1;
            }
        }
        // Fold folders into their parents: pre-order from the roots, then in reverse.
        let mut order: Vec<u32> = Vec::with_capacity(nd);
        let mut stack: Vec<u32> = (0..nd as u32)
            .filter(|&d| parent_dir[d as usize] == NONE)
            .collect();
        while let Some(d) = stack.pop() {
            order.push(d);
            stack.extend_from_slice(
                &kd[kd_start[d as usize] as usize..kd_start[d as usize + 1] as usize],
            );
        }
        for &d in order.iter().rev() {
            let p = parent_dir[d as usize];
            if p == NONE {
                continue;
            }
            let (d, p) = (d as usize, p as usize);
            bytes[p] = bytes[p].saturating_add(bytes[d]);
            files[p] = files[p].saturating_add(files[d]);
            folders[p] = folders[p].saturating_add(folders[d]);
            let cd = cats[d];
            for k in 0..CATEGORIES {
                cats[p][k] = cats[p][k].saturating_add(cd[k]);
            }
        }
        Rollup {
            dirs,
            bytes,
            files,
            folders,
            cats,
            kd_start,
            kd,
            kf_start,
            kf,
            ov: Overlay::default(),
        }
    }

    fn live_dir(&self, p: u32) -> bool {
        (p as usize) < self.parent.len() && self.flags[p as usize] & (F_DELETED | F_DIR) == F_DIR
    }

    /// What live record `s` adds to the live folders above it (`rollup` semantics: a record
    /// counts towards a parent only while that parent is a live folder). `fresh`: `s` was just
    /// created, so a folder holds nothing yet whatever its slot held before.
    fn contrib(&self, ru: &Rollup, s: u32, fresh: bool) -> Contrib {
        let su = s as usize;
        let mut d = Delta::default();
        if self.flags[su] & F_DIR != 0 {
            if !fresh {
                d = ru.slot_sum(s);
            }
            d.folders += 1;
        } else {
            d.files = 1;
            if self.with_meta {
                let sz = self.size[su];
                d.bytes = sz.min(i64::MAX as u64) as i64;
                if sz > 0 {
                    let k = category_of_key(ext_key(self.name_bytes(s))) as usize;
                    d.cats[k] = (sz >> CAT_SHIFT).min(u32::MAX as u64) as i64;
                }
            }
        }
        let mut chain = Vec::new();
        let mut c = s;
        while chain.len() < 100_000 {
            let p = self.parent[c as usize];
            if p == c || !self.live_dir(p) {
                break;
            }
            chain.push(p);
            c = p;
        }
        Contrib { chain, d }
    }

    /// Before a mutation: note what the live records among `slots` (the ones it may move,
    /// resize or remove) add to the folders above them.
    pub fn usage_before(&self, ru: &Rollup, slots: &[u32]) -> UsageTouch {
        let mut pre: Vec<(u32, Contrib)> = Vec::with_capacity(slots.len());
        for &s in slots {
            let live = (s as usize) < self.parent.len() && self.flags[s as usize] & F_DELETED == 0;
            if live && !pre.iter().any(|e| e.0 == s) {
                pre.push((s, self.contrib(ru, s, false)));
            }
        }
        UsageTouch {
            live: self.live_count(),
            vols: self.volumes().len(),
            pre,
        }
    }

    /// After the mutation: push the difference up the old and the new parent chains. `maybe_new`
    /// are records the mutation may have created (anything live there that `usage_before` did
    /// not see counts as new). If the live-record count does not add up, the change touched
    /// something else: the overlay gives up exactness rather than guess.
    pub fn usage_after(&self, ru: &mut Rollup, t: UsageTouch, maybe_new: &[u32]) {
        if ru.ov.broken {
            return;
        }
        let live =
            |s: u32| (s as usize) < self.parent.len() && self.flags[s as usize] & F_DELETED == 0;
        let mut created: Vec<u32> = Vec::new();
        for &s in maybe_new {
            if live(s) && !t.pre.iter().any(|e| e.0 == s) && !created.contains(&s) {
                created.push(s);
            }
        }
        // Every "after" is measured before the overlay changes.
        let post: Vec<Option<Contrib>> = t
            .pre
            .iter()
            .map(|(s, _)| live(*s).then(|| self.contrib(ru, *s, false)))
            .collect();
        let added: Vec<Contrib> = created.iter().map(|&s| self.contrib(ru, s, true)).collect();
        let removed: i64 = t
            .pre
            .iter()
            .zip(&post)
            .filter(|(_, p)| p.is_none())
            .map(|((_, c), _)| c.d.files + c.d.folders)
            .sum();
        if self.live_count() as i64 != t.live as i64 + created.len() as i64 - removed
            || self.volumes().len() != t.vols
        {
            ru.ov.broken = true;
            return;
        }
        // A new folder may sit in a slot an old one (still in the snapshot) had: start it at 0.
        for &s in &created {
            if self.flags[s as usize] & F_DIR != 0 {
                match ru.dir_of_slot(s) {
                    Some(d) => {
                        let mut z = Delta::default();
                        z.add(&ru.base(d), -1);
                        ru.ov.by_slot.insert(s, z);
                    }
                    None => {
                        ru.ov.by_slot.remove(&s);
                    }
                }
            }
        }
        for ((_, pre), post) in t.pre.iter().zip(post) {
            match post {
                None => ru.push(pre, -1),
                Some(post) if post != *pre => {
                    ru.push(pre, -1);
                    ru.push(&post, 1);
                    if post.chain != pre.chain {
                        ru.ov.dirty = true;
                    }
                }
                Some(_) => {}
            }
        }
        for c in &added {
            ru.push(c, 1);
            ru.ov.dirty = true;
        }
    }

    /// Resolve `C:\a\b` to a folder id of `ru` (case-insensitive); `C:` / `C:\` is the volume root.
    pub fn resolve_dir(&self, ru: &Rollup, path: &str) -> Option<u32> {
        let mut parts = path.split(['\\', '/']).filter(|p| !p.is_empty());
        let first = parts.next()?;
        let letter = first.chars().next()?.to_ascii_uppercase();
        if !letter.is_ascii_alphabetic() || !first[1..].starts_with(':') {
            return None;
        }
        let mut cur = ru.dir_of_slot(self.root_of(letter as u8)?)?;
        for part in parts {
            let lo = ru.kd_start[cur as usize] as usize;
            let hi = ru.kd_start[cur as usize + 1] as usize;
            let want = part.to_lowercase();
            cur = ru.kd[lo..hi].iter().copied().find(|&k| {
                let s = ru.dirs[k as usize];
                self.live_dir(s) && {
                    let nm = self.name(s);
                    nm.eq_ignore_ascii_case(part) || nm.to_lowercase() == want
                }
            })?;
        }
        Some(cur)
    }

    /// Slot of a folder id.
    pub fn dir_slot(&self, ru: &Rollup, d: u32) -> u32 {
        ru.dirs[d as usize]
    }

    /// Totals of one folder and its biggest `limit` children.
    pub fn list_dir(&self, ru: &Rollup, d: u32, limit: usize) -> DirList {
        let du = d as usize;
        let slot = ru.dirs[du];
        let (bytes, files, folders) = ru.totals(d);
        let mut out = DirList {
            bytes,
            files,
            folders,
            cats: ru.cats_of(d),
            ..Default::default()
        };
        let n = self.slots();
        let mut all: Vec<(u64, u32, u32)> = Vec::new(); // bytes, slot, folder id (NONE = file)
        for &k in &ru.kd[ru.kd_start[du] as usize..ru.kd_start[du + 1] as usize] {
            let s = ru.dirs[k as usize];
            if self.live_dir(s) && self.parent(s) == slot {
                all.push((ru.totals(k).0, s, k));
            }
        }
        for &s in &ru.kf[ru.kf_start[du] as usize..ru.kf_start[du + 1] as usize] {
            if (s as usize) < n && !self.is_deleted(s) && self.parent(s) == slot && !self.is_dir(s)
            {
                all.push((self.size(s).unwrap_or(0), s, NONE));
            }
        }
        let total: u64 = all.iter().map(|e| e.0).sum();
        let cmp = |a: &(u64, u32, u32), b: &(u64, u32, u32)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
        if all.len() > limit {
            all.select_nth_unstable_by(limit, cmp);
            out.more = (all.len() - limit) as u64;
            all.truncate(limit);
        }
        all.sort_unstable_by(cmp);
        let listed: u64 = all.iter().map(|e| e.0).sum();
        out.more_bytes = total.saturating_sub(listed);
        out.children = all
            .into_iter()
            .map(|(bytes, s, k)| {
                if k == NONE {
                    ChildUsage {
                        slot: s,
                        is_dir: false,
                        bytes,
                        files: 1,
                        folders: 0,
                        kind: category_of_key(ext_key(self.name_bytes(s))),
                    }
                } else {
                    let (_, files, folders) = ru.totals(k);
                    ChildUsage {
                        slot: s,
                        is_dir: true,
                        bytes,
                        files,
                        folders,
                        kind: ru.kind_of(k),
                    }
                }
            })
            .collect();
        out
    }

    /// File types and the biggest files under a folder (walks the subtree).
    pub fn type_detail(
        &self,
        ru: &Rollup,
        d: u32,
        top_files: usize,
    ) -> (Vec<TypeUsage>, Vec<(u32, u64)>) {
        let n = self.slots();
        let mut types: Fx<u64, (u64, u64)> = HashMap::default();
        let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
        let mut stack = vec![d];
        while let Some(x) = stack.pop() {
            let xu = x as usize;
            stack.extend_from_slice(&ru.kd[ru.kd_start[xu] as usize..ru.kd_start[xu + 1] as usize]);
            for &s in &ru.kf[ru.kf_start[xu] as usize..ru.kf_start[xu + 1] as usize] {
                if s as usize >= n || self.is_deleted(s) || self.is_dir(s) {
                    continue;
                }
                let sz = self.size(s).unwrap_or(0);
                let t = types.entry(ext_key(self.name_bytes(s))).or_default();
                t.0 = t.0.saturating_add(sz);
                t.1 += 1;
                if top_files > 0
                    && sz > 0
                    && (heap.len() < top_files || heap.peek().is_some_and(|r| sz > r.0 .0))
                {
                    if heap.len() == top_files {
                        heap.pop();
                    }
                    heap.push(Reverse((sz, s)));
                }
            }
        }
        let mut tv: Vec<TypeUsage> = types
            .into_iter()
            .map(|(k, (bytes, files))| TypeUsage {
                ext: unpack_ext(k),
                kind: category_of_key(k),
                bytes,
                files,
            })
            .collect();
        tv.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then(a.ext.cmp(&b.ext)));
        let mut bf: Vec<(u32, u64)> = heap.into_iter().map(|Reverse((sz, s))| (s, sz)).collect();
        bf.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        (tv, bf)
    }

    /// `(volume letter, folder id)` of every volume root in the roll-up.
    pub fn volume_dirs(&self, ru: &Rollup) -> Vec<(u8, u32)> {
        self.volumes()
            .iter()
            .filter_map(|v| ru.dir_of_slot(v.root).map(|d| (v.letter, d)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Index {
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'C', 5);
        b.push(b'C', 10, 5, "Users", true, 0, 0);
        b.push(b'C', 11, 10, "Pics", true, 0, 0);
        b.push(b'C', 12, 11, "a.JPG", false, 300, 0);
        b.push(b'C', 13, 11, "b.jpg", false, 200, 0);
        b.push(b'C', 14, 10, "notes.txt", false, 50, 0);
        b.push(b'C', 15, 5, "pagefile.sys", false, 1000, 0);
        b.push(b'C', 16, 10, "Empty", true, 0, 0);
        b.finish()
    }

    #[test]
    fn rolls_up_children_and_totals() {
        let ix = sample();
        let ru = ix.rollup();
        let users = ix.resolve_dir(&ru, r"c:\users").unwrap();
        let l = ix.list_dir(&ru, users, 10);
        assert_eq!((l.bytes, l.files, l.folders), (550, 3, 2));
        let names: Vec<_> = l
            .children
            .iter()
            .map(|c| (ix.name(c.slot).to_string(), c.bytes, c.files, c.folders))
            .collect();
        assert_eq!(names[0], ("Pics".into(), 500, 2, 0));
        assert_eq!(names[1], ("notes.txt".into(), 50, 1, 0));
        assert!(names.iter().any(|n| n.0 == "Empty" && n.1 == 0));
    }

    #[test]
    fn folder_kind_is_its_dominant_file_type() {
        let mut b = IndexBuilder::new(true);
        b.add_volume(b'C', 5);
        b.push(b'C', 10, 5, "Vids", true, 0, 0);
        b.push(b'C', 11, 10, "a.mp4", false, 50_000_000, 0);
        b.push(b'C', 12, 10, "b.txt", false, 90_000, 0);
        let ix = b.finish();
        let ru = ix.rollup();
        let root = ix.resolve_dir(&ru, "C:\\").unwrap();
        let l = ix.list_dir(&ru, root, 5);
        assert_eq!(l.children[0].kind, 2);
        assert_eq!(l.cats[2] / 4096, 50_000_000 / 4096);
    }

    #[test]
    fn volume_root_unknown_paths_and_cap() {
        let ix = sample();
        let ru = ix.rollup();
        let root = ix.resolve_dir(&ru, "C:\\").unwrap();
        let l = ix.list_dir(&ru, root, 1);
        assert_eq!((l.bytes, l.files), (1550, 4));
        assert_eq!(ix.name(l.children[0].slot), "pagefile.sys");
        assert_eq!((l.children.len(), l.more, l.more_bytes), (1, 1, 550));
        assert!(ix.resolve_dir(&ru, r"C:\nope").is_none());
        assert!(ix.resolve_dir(&ru, r"C:\Users\notes.txt").is_none());
        assert!(ix.resolve_dir(&ru, r"Z:\").is_none());
        assert_eq!(ix.volume_dirs(&ru).len(), 1);
    }

    #[test]
    fn types_and_biggest() {
        let ix = sample();
        let ru = ix.rollup();
        let users = ix.resolve_dir(&ru, r"C:\Users").unwrap();
        let (t, big) = ix.type_detail(&ru, users, 5);
        assert_eq!(
            t[0],
            TypeUsage {
                ext: "jpg".into(),
                kind: 1,
                bytes: 500,
                files: 2
            }
        );
        assert_eq!(ix.name(big[0].0), "a.JPG");
        assert_eq!(category("mp4"), 2);
        assert_eq!(category("zzz"), 0);
    }

    /// Timing on a synthetic index shaped like a big disk (run with `--ignored --nocapture`).
    #[test]
    #[ignore]
    fn bench_big_index() {
        let mut b = IndexBuilder::with_capacity(true, 6_000_000);
        b.add_volume(b'C', 5);
        let mut frn = 100u64;
        let mut top = Vec::new();
        for t in 0..40 {
            frn += 1;
            b.push(b'C', frn, 5, &format!("top{t}"), true, 0, 0);
            top.push(frn);
        }
        let mut dirs = top.clone();
        for i in 0..400_000u64 {
            frn += 1;
            let p = dirs[(i * 2654435761 % dirs.len() as u64) as usize];
            b.push(b'C', frn, p, &format!("dir{i}"), true, 0, 0);
            dirs.push(frn);
        }
        for i in 0..5_000_000u64 {
            frn += 1;
            let p = dirs[(i * 40503 % dirs.len() as u64) as usize];
            b.push(
                b'C',
                frn,
                p,
                &format!("file{i}.{}", ["jpg", "mp4", "dll", "txt"][(i % 4) as usize]),
                false,
                1000 + i % 90000,
                0,
            );
        }
        let ix = b.finish();
        let t = std::time::Instant::now();
        let ru = ix.rollup();
        println!(
            "rollup of {} records: {:?} ({} MB)",
            ix.slots(),
            t.elapsed(),
            ru.mem_bytes() >> 20
        );
        let t = std::time::Instant::now();
        let root = ix.resolve_dir(&ru, "C:\\").unwrap();
        let l = ix.list_dir(&ru, root, 400);
        println!(
            "resolve + list root ({} kids): {:?}",
            l.children.len(),
            t.elapsed()
        );
        let t = std::time::Instant::now();
        let (ty, big) = ix.type_detail(&ru, root, 100);
        println!(
            "type detail: {:?} ({} types, {} big)",
            t.elapsed(),
            ty.len(),
            big.len()
        );
        let (mut ix, mut ru) = (ix, ru);
        let n = ix.slots() as u32;
        let mut spent = std::time::Duration::ZERO;
        for j in 0..20_000u32 {
            let s = n - 1 - j * 13;
            let t = std::time::Instant::now();
            let tch = ix.usage_before(&ru, &[s]);
            spent += t.elapsed();
            if j % 2 == 0 {
                let sz = ix.size(s).unwrap_or(0) + 4096;
                ix.update_meta(s, sz, 0);
            } else {
                ix.remove(s);
            }
            let t = std::time::Instant::now();
            ix.usage_after(&mut ru, tch, &[]);
            spent += t.elapsed();
        }
        println!(
            "overlay bookkeeping, 20k resize/delete: {:?} per change, {} overlay folders, exact {}",
            spent / 20_000,
            ru.overlay_len(),
            ru.exact()
        );
    }

    /// Fresh `rollup()` of `ix` and `ru` (snapshot + overlay) agree on every live folder; the
    /// children `ru` lists are listed by the fresh one too, with the same numbers.
    fn assert_same(ix: &Index, ru: &Rollup) {
        let fresh = ix.rollup();
        assert!(ru.exact());
        for s in 0..ix.slots() as u32 {
            if !ix.live_dir(s) {
                continue;
            }
            assert_eq!(
                ru.totals_at(s),
                fresh.totals_at(s),
                "folder {}",
                ix.full_path(s)
            );
            let (Some(d), Some(fd)) = (ru.dir_of_slot(s), fresh.dir_of_slot(s)) else {
                continue;
            };
            let a = ix.list_dir(ru, d, usize::MAX);
            let b = ix.list_dir(&fresh, fd, usize::MAX);
            assert_eq!(
                (a.bytes, a.files, a.folders, a.cats),
                (b.bytes, b.files, b.folders, b.cats)
            );
            for ch in &a.children {
                assert!(b.children.contains(ch), "{:?} in {}", ch, ix.full_path(s));
            }
            if !ru.structure_dirty() {
                assert_eq!(a.children.len(), b.children.len());
            }
        }
    }

    #[test]
    fn overlay_tracks_delete_resize_and_move() {
        let mut ix = sample();
        let mut ru = ix.rollup();
        let pics = ix.folder_idx(b'C', 11).unwrap();
        let empty = ix.folder_idx(b'C', 16).unwrap();
        let users = ix.resolve_dir(&ru, r"C:\Users").unwrap();
        // Recycle the pictures folder: Users drops 500 bytes and 2 files at once.
        let t = ix.usage_before(&ru, &[pics]);
        ix.remove(pics);
        ix.usage_after(&mut ru, t, &[]);
        assert_eq!(ru.totals(users), (50, 1, 1));
        assert!(!ru.structure_dirty());
        assert_same(&ix, &ru);
        // Grow a file, move it into Empty, add a new file there.
        let notes = ix.find_file(b'C', 10, "notes.txt").unwrap();
        let t = ix.usage_before(&ru, &[notes]);
        ix.update_meta(notes, 9000, 0);
        ix.usage_after(&mut ru, t, &[]);
        let t = ix.usage_before(&ru, &[notes]);
        ix.rename(notes, empty, "notes.md");
        ix.usage_after(&mut ru, t, &[]);
        let t = ix.usage_before(&ru, &[]);
        let f = ix.insert(b'C', 0, 16, "v.mp4", false, 1 << 20, 0);
        ix.usage_after(&mut ru, t, &[f]);
        assert!(ru.structure_dirty());
        let e = ix.resolve_dir(&ru, r"C:\Users\Empty").unwrap();
        assert_eq!(ru.totals(e), (9000 + (1 << 20), 2, 0));
        assert_eq!(ru.cats_of(e)[2], 1 << 20);
        assert_same(&ix, &ru);
    }

    #[test]
    fn overlay_gives_up_on_untracked_changes() {
        let mut ix = sample();
        let mut ru = ix.rollup();
        // A create the caller did not announce as possibly new: the live count does not add up.
        let t = ix.usage_before(&ru, &[]);
        ix.insert(b'C', 0, 10, "x.txt", false, 5, 0);
        ix.usage_after(&mut ru, t, &[]);
        assert!(!ru.exact());
    }

    /// Hundreds of random creates/deletes/moves/resizes (with slot reuse) keep every folder's
    /// overlaid totals equal to a fresh roll-up.
    #[test]
    fn overlay_matches_fresh_rollup_randomized() {
        for seed in 1..=4u64 {
            let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut next = move |m: u64| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng % m.max(1)
            };
            let exts = ["jpg", "mp4", "txt", "zip", "rs", "exe", "bin", "none"];
            let mut b = IndexBuilder::new(true);
            b.add_volume(b'C', 5);
            let mut tree = vec![5u64];
            let mut frn = 100u64;
            for i in 0..300 {
                frn += 1;
                let p = tree[next(tree.len() as u64) as usize];
                if i % 6 == 0 {
                    b.push(b'C', frn, p, &format!("d{i}"), true, 0, 0);
                    tree.push(frn);
                } else {
                    let name = format!("f{i}.{}", exts[next(8) as usize]);
                    b.push(b'C', frn, p, &name, false, next(1 << 22), 0);
                }
            }
            let mut ix = b.finish();
            let mut ru = ix.rollup();
            for step in 0..400 {
                let n = ix.slots() as u64;
                let live: Vec<u32> = (0..n as u32)
                    .filter(|&s| !ix.is_deleted(s) && ix.parent(s) != NONE)
                    .collect();
                let dirs: Vec<u32> = (0..n as u32).filter(|&s| ix.live_dir(s)).collect();
                let dir = dirs[next(dirs.len() as u64) as usize];
                let Some(&pick) = live.get(next(live.len() as u64) as usize) else {
                    let t = ix.usage_before(&ru, &[]);
                    frn += 1;
                    let i = ix.insert(b'C', frn, ix.frn(dir), "re", true, 0, 0);
                    ix.usage_after(&mut ru, t, &[i]);
                    continue;
                };
                match next(10) {
                    0..=3 => {
                        let is_dir = next(3) == 0;
                        frn += 1;
                        let name = format!("n{}.{}", next(6), exts[next(8) as usize]);
                        let pf = ix.frn(dir);
                        let pre: Vec<u32> = if is_dir {
                            vec![]
                        } else {
                            ix.find_file(b'C', pf, &name).into_iter().collect()
                        };
                        let t = ix.usage_before(&ru, &pre);
                        let i = ix.insert(b'C', frn, pf, &name, is_dir, next(1 << 22), 0);
                        ix.usage_after(&mut ru, t, &[i]);
                    }
                    4..=5 if !ix.is_dir(pick) || next(4) == 0 => {
                        let t = ix.usage_before(&ru, &[pick]);
                        ix.remove(pick);
                        ix.usage_after(&mut ru, t, &[]);
                    }
                    6..=7 => {
                        let t = ix.usage_before(&ru, &[pick]);
                        let nm = format!("m{}.{}", next(6), exts[next(8) as usize]);
                        ix.rename(pick, dir, &nm);
                        ix.usage_after(&mut ru, t, &[]);
                    }
                    _ => {
                        let t = ix.usage_before(&ru, &[pick]);
                        ix.update_meta(pick, next(1 << 24), 0);
                        ix.usage_after(&mut ru, t, &[]);
                    }
                }
                if step % 50 == 0 {
                    ix.refresh(); // frees deleted slots for reuse
                }
                if step % 40 == 0 {
                    assert_same(&ix, &ru);
                }
            }
            assert_same(&ix, &ru);
            // The run really moved things around (not an index that died early).
            assert!(
                ix.live_count() > 80 && ru.overlay_len() > 10,
                "{}",
                ix.live_count()
            );
        }
    }
}
