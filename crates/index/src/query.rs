//! Query parsing and parallel execution.
//!
//! Syntax (terms are ANDed):
//! * whitespace separates terms; `"quoted phrase"` is one term
//! * `!term` negates; `!"a b"` too
//! * `*` / `?` in a term: whole-name wildcard match (whole *path* if the term has `\`)
//! * a term containing `\` (or `/`) is matched against the full path (substring)
//! * `ext:jpg;png` (also `,`), `file:`, `folder:` (alone = filter, `folder:foo` = filter + term)
//! * `size:>100mb`, `size:<=4kb`, `size:1mb..2gb`, `size:0` (files only; b/kb/mb/gb/tb, 1024-based)
//! * `dm:A..B` date modified in unix milliseconds, `[A, B)`; either end may be left out
//!   (`dm:1700000000000..`). The app turns "last week" / "summer 2023" into this.
//!
//! Matching is case-insensitive (see `fold`). Execution scans the sorted order array in
//! parallel chunks (so hits are already in name order), cheapest terms first.

use crate::fold::*;
use crate::store::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Cond {
    /// Substring; `needle` is folded. `ascii` = needle is pure ASCII (fast path).
    Text { needle: String, ascii: bool, path: bool },
    /// Whole match with `*`/`?`; pattern is folded.
    Glob { pat: Vec<char>, pat8: Option<Vec<u8>>, path: bool },
    Ext(Vec<String>),
    File,
    Folder,
    /// File size in bytes, inclusive bounds (folders never match).
    Size { min: u64, max: u64 },
    /// Modified time as stored (FILETIME), `[from, to)`.
    Mtime { from: i64, to: i64 },
}

/// 1601 -> 1970 in 100 ns units (the store keeps raw FILETIMEs).
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

fn parse_bytes(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let cut = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(cut);
    let mul: u64 = match unit {
        "" | "b" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        _ => return None,
    };
    let v: f64 = num.parse().ok()?;
    (v >= 0.0 && v.is_finite()).then(|| (v * mul as f64).min(u64::MAX as f64) as u64)
}

/// `>N`, `>=N`, `<N`, `<=N`, `A..B`, `A..`, `..B`, `N` -> inclusive (min, max).
fn parse_size(text: &str) -> Option<Cond> {
    let (min, max) = if let Some(r) = text.strip_prefix(">=") {
        (parse_bytes(r)?, u64::MAX)
    } else if let Some(r) = text.strip_prefix('>') {
        (parse_bytes(r)?.checked_add(1)?, u64::MAX)
    } else if let Some(r) = text.strip_prefix("<=") {
        (0, parse_bytes(r)?)
    } else if let Some(r) = text.strip_prefix('<') {
        (0, parse_bytes(r)?.checked_sub(1)?)
    } else if let Some((a, b)) = text.split_once("..") {
        if a.is_empty() && b.is_empty() {
            return None;
        }
        (if a.is_empty() { 0 } else { parse_bytes(a)? }, if b.is_empty() { u64::MAX } else { parse_bytes(b)? })
    } else {
        let v = parse_bytes(text)?;
        (v, v)
    };
    Some(Cond::Size { min, max })
}

/// `A..B` in unix milliseconds (either end optional) -> FILETIME `[from, to)`.
fn parse_mtime(text: &str) -> Option<Cond> {
    let (a, b) = text.split_once("..")?;
    if a.is_empty() && b.is_empty() {
        return None;
    }
    let ft = |ms: &str| ms.parse::<i64>().ok().map(|ms| ms.saturating_mul(10_000).saturating_add(FILETIME_UNIX_EPOCH));
    Some(Cond::Mtime { from: if a.is_empty() { i64::MIN } else { ft(a)? }, to: if b.is_empty() { i64::MAX } else { ft(b)? } })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub cond: Cond,
    pub negate: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<Term>,
}

/// Result order. The name orders walk the sorted order array (ties by path); the meta
/// orders keep the best window of matches by the stored size / mtime, ties by slot.
/// Records without metadata (a volume scanned `with_meta = false`) sort as size 0 /
/// oldest, so they land at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    NameAsc,
    NameDesc,
    MtimeDesc,
    SizeDesc,
    /// Exact reverse of `MtimeDesc` (oldest first).
    MtimeAsc,
    /// Exact reverse of `SizeDesc` (smallest first).
    SizeAsc,
    /// File type A to Z (folders first, then by extension, ties by name).
    TypeAsc,
    /// Exact reverse of `TypeAsc`.
    TypeDesc,
}

/// Which stored column a meta order sorts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetaKey {
    Mtime,
    Size,
    /// File type then name; the list reads A to Z (its reverse is Z to A).
    Ext,
}

/// Every match of a query as a set of positions in the name-sorted order array.
///
/// A bitmap (one bit per sort rank) with a popcount directory: 14M records cost 1.8 MB no
/// matter how many match, nothing is sorted (walking the bits *is* name order), and any
/// window - the first rows or row 8,000,000 - is found in microseconds. This is what lets a
/// scrollbar jump land instantly on a list of millions.
#[derive(Debug, Clone)]
pub struct RankSet {
    /// `None` = every rank `0..n` matches (the empty query).
    bits: Option<Vec<u64>>,
    /// Matches before each `BLOCK`-word block of `bits`.
    dir: Vec<u32>,
    n: usize,
    total: usize,
}

const BLOCK: usize = 8; // words per directory entry (512 ranks)

impl RankSet {
    fn all(n: usize) -> Self {
        RankSet { bits: None, dir: Vec::new(), n, total: n }
    }

    fn from_bits(bits: Vec<u64>, n: usize) -> Self {
        let mut dir = Vec::with_capacity(bits.len() / BLOCK + 1);
        let mut acc = 0u32;
        for b in bits.chunks(BLOCK) {
            dir.push(acc);
            acc += b.iter().map(|w| w.count_ones()).sum::<u32>();
        }
        RankSet { bits: Some(bits), dir, n, total: acc as usize }
    }

    /// Number of matches.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Heap bytes held (for cache accounting).
    pub fn mem_bytes(&self) -> usize {
        self.bits.as_ref().map_or(0, |b| b.len() * 8) + self.dir.len() * 4
    }

    /// The ranks of matches `offset..offset+limit` in ascending rank order.
    fn asc(&self, offset: usize, limit: usize) -> Vec<u32> {
        let end = offset.saturating_add(limit).min(self.total);
        if offset >= end {
            return Vec::new();
        }
        let Some(bits) = &self.bits else {
            return (offset as u32..end as u32).collect();
        };
        // the last directory block that starts at or before match number `offset`
        let b = self.dir.partition_point(|&c| c as usize <= offset) - 1;
        let mut skip = offset - self.dir[b] as usize;
        let mut out = Vec::with_capacity(end - offset);
        for (wi, &word) in bits.iter().enumerate().skip(b * BLOCK) {
            let mut w = word;
            let ones = w.count_ones() as usize;
            if skip >= ones {
                skip -= ones;
                continue;
            }
            while skip > 0 {
                w &= w - 1;
                skip -= 1;
            }
            while w != 0 {
                out.push((wi * 64) as u32 + w.trailing_zeros());
                if out.len() == end - offset {
                    return out;
                }
                w &= w - 1;
            }
        }
        out
    }

    /// The ranks of the requested window: ascending, or counted from the end when `desc`.
    pub fn window(&self, offset: usize, limit: usize, desc: bool) -> Vec<u32> {
        if !desc {
            return self.asc(offset, limit);
        }
        let hi = self.total.saturating_sub(offset);
        let lo = hi.saturating_sub(limit);
        let mut v = self.asc(lo, hi - lo);
        v.reverse();
        v
    }
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub offset: usize,
    pub limit: usize,
    pub order: Order,
}
impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions { offset: 0, limit: usize::MAX, order: Order::NameAsc }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    /// Total number of matches (ignoring offset/limit).
    pub total: usize,
    /// Record indices of the requested window.
    pub items: Vec<u32>,
}

fn text_term(text: &str, negate: bool) -> Option<Term> {
    if text.is_empty() {
        return None;
    }
    let path = text.contains('\\') || text.contains('/');
    let text = if path { text.replace('/', "\\") } else { text.to_string() };
    let folded = fold_string(&text);
    let cond = if folded.contains('*') || folded.contains('?') {
        Cond::Glob { pat8: folded.is_ascii().then(|| folded.as_bytes().to_vec()), pat: folded.chars().collect(), path }
    } else {
        let ascii = folded.is_ascii();
        Cond::Text { needle: folded, ascii, path }
    };
    Some(Term { cond, negate })
}

impl Query {
    pub fn parse(s: &str) -> Query {
        let ch: Vec<char> = s.chars().collect();
        let n = ch.len();
        let mut i = 0;
        let mut terms = Vec::new();
        while i < n {
            if ch[i].is_whitespace() {
                i += 1;
                continue;
            }
            let mut negate = false;
            while i < n && ch[i] == '!' {
                negate = !negate;
                i += 1;
            }
            if i >= n || ch[i].is_whitespace() {
                continue;
            }
            let start = i;
            let mut j = i;
            while j < n && ch[j].is_ascii_alphabetic() {
                j += 1;
            }
            let mut prefix = None;
            if j < n && ch[j] == ':' && j > start {
                let w: String = ch[start..j].iter().collect::<String>().to_ascii_lowercase();
                if matches!(w.as_str(), "ext" | "file" | "folder" | "size" | "dm") {
                    prefix = Some(w);
                    i = j + 1;
                }
            }
            let mut text = String::new();
            if i < n && ch[i] == '"' {
                i += 1;
                while i < n && ch[i] != '"' {
                    text.push(ch[i]);
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
            } else {
                while i < n && !ch[i].is_whitespace() {
                    text.push(ch[i]);
                    i += 1;
                }
            }
            match prefix.as_deref() {
                Some("ext") => {
                    let list: Vec<String> = text
                        .split([';', ','])
                        .map(|e| fold_string(e.trim().trim_start_matches('.')))
                        .filter(|e| !e.is_empty())
                        .collect();
                    if !list.is_empty() {
                        terms.push(Term { cond: Cond::Ext(list), negate });
                    }
                }
                Some(p @ ("size" | "dm")) => {
                    // A value that does not parse is ordinary text ("size:huge" finds that name).
                    match if p == "size" { parse_size(&text) } else { parse_mtime(&text) } {
                        Some(cond) => terms.push(Term { cond, negate }),
                        None => terms.extend(text_term(&format!("{p}:{text}"), negate)),
                    }
                }
                Some(p) => {
                    let cond = if p == "file" { Cond::File } else { Cond::Folder };
                    if text.is_empty() {
                        terms.push(Term { cond, negate });
                    } else {
                        terms.push(Term { cond, negate: false });
                        terms.extend(text_term(&text, negate));
                    }
                }
                None => terms.extend(text_term(&text, negate)),
            }
        }
        // cheapest first
        terms.sort_by_key(|t| match &t.cond {
            Cond::File | Cond::Folder | Cond::Size { .. } | Cond::Mtime { .. } => 0,
            Cond::Ext(_) => 1,
            Cond::Text { ascii: true, path: false, .. } => 2,
            Cond::Text { path: false, .. } => 3,
            Cond::Glob { path: false, .. } => 4,
            Cond::Text { .. } => 5,
            Cond::Glob { .. } => 6,
        });
        Query { terms }
    }
}

#[derive(Default)]
struct Scratch {
    path: Vec<u8>,
    fold: String,
    chars: Vec<char>,
}

fn ext_of(name: &str) -> Option<&str> {
    let p = name.rfind('.')?;
    let e = &name[p + 1..];
    (!e.is_empty()).then_some(e)
}

impl Index {
    fn eval(&self, t: &Term, i: u32, sc: &mut Scratch) -> bool {
        let m = match &t.cond {
            Cond::File => !self.is_dir(i),
            Cond::Folder => self.is_dir(i),
            Cond::Size { min, max } => !self.is_dir(i) && self.size(i).is_some_and(|s| s >= *min && s <= *max),
            Cond::Mtime { from, to } => self.mtime(i).is_some_and(|m| m >= *from && m < *to),
            Cond::Ext(list) => {
                !self.is_dir(i)
                    && ext_of(self.name(i)).is_some_and(|e| {
                        list.iter().any(|x| {
                            if x.is_ascii() && e.is_ascii() {
                                x.eq_ignore_ascii_case(e)
                            } else {
                                fold_string(e) == *x
                            }
                        })
                    })
            }
            Cond::Text { needle, ascii, path } => {
                let hay: &[u8] = if *path {
                    self.write_path(i, &mut sc.path);
                    &sc.path
                } else {
                    self.name_bytes(i)
                };
                if *ascii {
                    find_ascii_ci(hay, needle.as_bytes())
                } else if hay.is_ascii() {
                    false // a non-ASCII folded needle cannot occur in pure-ASCII text
                } else {
                    match std::str::from_utf8(hay) {
                        Ok(s) => {
                            fold_into(s, &mut sc.fold);
                            sc.fold.contains(needle.as_str())
                        }
                        Err(_) => false,
                    }
                }
            }
            Cond::Glob { pat, pat8, path } => {
                let hay: &[u8] = if *path {
                    self.write_path(i, &mut sc.path);
                    &sc.path
                } else {
                    self.name_bytes(i)
                };
                match pat8 {
                    Some(p8) if hay.is_ascii() => glob_match_ascii_ci(p8, hay),
                    _ => {
                        sc.chars.clear();
                        sc.chars.extend(std::str::from_utf8(hay).unwrap_or("").chars().map(fold_char));
                        glob_match(pat, &sc.chars)
                    }
                }
            }
        };
        m != t.negate
    }

    /// Scan storage slots `lo..hi` sequentially and hand every match to `hit(slot, rank)`.
    fn scan_each(&self, q: &Query, rank: &[u32], lo: usize, hi: usize, mut hit: impl FnMut(u32, u32)) {
        let mut sc = Scratch::default();
        for (i, &r) in rank.iter().enumerate().take(hi).skip(lo) {
            if r != NONE && q.terms.iter().all(|t| self.eval(t, i as u32, &mut sc)) {
                hit(i as u32, r);
            }
        }
    }

    /// Split `0..slots` into `available_parallelism` chunks and run `scan` over them; the
    /// per-chunk results come back in slot order.
    fn parallel_scan<T: Send>(&self, slots: usize, scan: impl Fn(usize, usize) -> T + Send + Sync) -> Vec<T> {
        let hw = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(1);
        const MIN_CHUNK: usize = 8192;
        let nt = hw.min(slots / MIN_CHUNK).max(1);
        if nt == 1 {
            return vec![scan(0, slots)];
        }
        let chunk = slots.div_ceil(nt);
        let scan = &scan;
        std::thread::scope(|s| {
            let hs: Vec<_> = (0..nt)
                .map(|k| s.spawn(move || scan((k * chunk).min(slots), ((k + 1) * chunk).min(slots))))
                .collect();
            hs.into_iter().map(|h| h.join().expect("scan thread")).collect()
        })
    }

    fn rank_view(&self, order_arr: &[u32]) -> std::borrow::Cow<'_, [u32]> {
        if self.dirty {
            std::borrow::Cow::Owned(self.compute_rank(order_arr))
        } else {
            std::borrow::Cow::Borrowed(&self.rank[..])
        }
    }

    /// Number of matches, without materializing them (the type-chip counters).
    pub fn count(&self, q: &Query) -> usize {
        let order_arr = self.clean_order();
        if q.terms.is_empty() {
            return order_arr.len();
        }
        let rank = self.rank_view(&order_arr);
        let rank = &rank[..];
        self.parallel_scan(rank.len(), |lo, hi| {
            let mut n = 0usize;
            self.scan_each(q, rank, lo, hi, |_, _| n += 1);
            n
        })
        .into_iter()
        .sum()
    }

    /// Every match as a [`RankSet`] (name order). One parallel pass over the names; the
    /// scan threads set their bits directly, so nothing is collected or sorted.
    pub fn name_matches(&self, q: &Query) -> RankSet {
        use std::sync::atomic::{AtomicU64, Ordering};
        let order_arr = self.clean_order();
        let n = order_arr.len();
        if q.terms.is_empty() {
            return RankSet::all(n);
        }
        let rank = self.rank_view(&order_arr);
        let rank = &rank[..];
        let bits: Vec<AtomicU64> = (0..n.div_ceil(64)).map(|_| AtomicU64::new(0)).collect();
        let bits_ref = &bits;
        self.parallel_scan(rank.len(), |lo, hi| {
            self.scan_each(q, rank, lo, hi, |_, r| {
                bits_ref[(r >> 6) as usize].fetch_or(1u64 << (r & 63), Ordering::Relaxed);
            })
        });
        RankSet::from_bits(bits.into_iter().map(AtomicU64::into_inner).collect(), n)
    }

    /// Record indices of one window of a [`RankSet`] built on this same index state.
    pub fn name_page(&self, set: &RankSet, offset: usize, limit: usize, desc: bool) -> Vec<u32> {
        let order_arr = self.clean_order();
        debug_assert_eq!(order_arr.len(), set.n, "RankSet used on a different index state");
        set.window(offset, limit, desc).into_iter().filter_map(|r| order_arr.get(r as usize).copied()).collect()
    }

    /// Every matching record index, in storage order (input for the meta orders).
    pub fn match_slots(&self, q: &Query) -> Vec<u32> {
        let order_arr = self.clean_order();
        let rank = self.rank_view(&order_arr);
        let rank = &rank[..];
        if q.terms.is_empty() {
            return (0..rank.len() as u32).filter(|&i| rank[i as usize] != NONE).collect();
        }
        let parts = self.parallel_scan(rank.len(), |lo, hi| {
            let mut out = Vec::new();
            self.scan_each(q, rank, lo, hi, |i, _| out.push(i));
            out
        });
        let mut all = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
        for p in parts {
            all.extend(p);
        }
        all
    }

    /// Sort key of one record for a meta order. Records without metadata sort as size 0 /
    /// oldest. With `dirs` (see [`Index::folder_totals`]) a folder sorts by what it holds.
    fn meta_key(&self, key: MetaKey, i: u32, dirs: Option<&[u64]>) -> i64 {
        match key {
            MetaKey::Mtime => self.mtime(i).unwrap_or(i64::MIN),
            MetaKey::Size => match dirs {
                Some(t) if self.is_dir(i) => t.get(i as usize).copied().unwrap_or(0).min(i64::MAX as u64) as i64,
                _ => self.size(i).unwrap_or(0).min(i64::MAX as u64) as i64,
            },
            // `Ext` needs the name ranks; `sort_keys` builds those itself.
            MetaKey::Ext => 0,
        }
    }

    /// (key, slot) for every slot. The list sorts keys high to low, so the `Ext` key is
    /// inverted: the first four bytes of the lower-cased extension (folders have none and
    /// lead), then the name rank, packed into 63 bits.
    fn sort_keys(&self, slots: &[u32], key: MetaKey, dirs: Option<&[u64]>) -> Vec<(i64, u32)> {
        if key != MetaKey::Ext {
            return slots.iter().map(|&i| (self.meta_key(key, i, dirs), i)).collect();
        }
        let order_arr = self.clean_order();
        let rank = self.rank_view(&order_arr);
        slots
            .iter()
            .map(|&i| {
                let mut ext = 0u64;
                if !self.is_dir(i) {
                    if let Some(e) = ext_of(self.name(i)) {
                        for (k, b) in e.bytes().take(4).enumerate() {
                            ext |= (b.to_ascii_lowercase() as u64) << (8 * (3 - k));
                        }
                    }
                }
                let r = (rank.get(i as usize).copied().unwrap_or(0) as u64).min((1 << 31) - 1);
                (i64::MAX - ((ext << 31) | r) as i64, i)
            })
            .collect()
    }

    /// Total bytes under every folder (all descendants), indexed by record slot; 0 for
    /// files. One pass adds each file to its parent, then folders are folded into their
    /// parents deepest first. ~14M records take tens of milliseconds; nothing is kept.
    pub fn folder_totals(&self) -> Vec<u64> {
        let n = self.parent.len();
        let mut tot = vec![0u64; n];
        if !self.with_meta {
            return tot;
        }
        let live = |i: usize| self.flags[i] & F_DELETED == 0;
        for i in 0..n {
            if live(i) && self.flags[i] & F_DIR == 0 {
                let p = self.parent[i] as usize;
                if p < n {
                    tot[p] = tot[p].saturating_add(self.size[i]);
                }
            }
        }
        // Depth of every folder (0 = no parent), memoized; a broken chain is cut at MAX_DEPTH.
        const MAX_DEPTH: u16 = 4096;
        const UNSET: u16 = u16::MAX;
        let mut depth = vec![UNSET; n];
        let mut chain: Vec<u32> = Vec::new();
        let mut by_depth: Vec<Vec<u32>> = Vec::new();
        for i in 0..n {
            if !live(i) || self.flags[i] & F_DIR == 0 || depth[i] != UNSET {
                continue;
            }
            chain.clear();
            let mut c = i;
            let mut base = 0u16;
            loop {
                chain.push(c as u32);
                let p = self.parent[c] as usize;
                if p >= n || chain.len() as u16 >= MAX_DEPTH {
                    break;
                }
                if depth[p] != UNSET {
                    base = depth[p].saturating_add(1);
                    break;
                }
                c = p;
            }
            for (k, &c) in chain.iter().rev().enumerate() {
                let d = base.saturating_add(k as u16).min(MAX_DEPTH);
                depth[c as usize] = d;
                if by_depth.len() <= d as usize {
                    by_depth.resize_with(d as usize + 1, Vec::new);
                }
                by_depth[d as usize].push(c);
            }
        }
        for level in by_depth.iter().rev() {
            for &c in level {
                let p = self.parent[c as usize] as usize;
                if p < n && p != c as usize {
                    tot[p] = tot[p].saturating_add(tot[c as usize]);
                }
            }
        }
        tot
    }

    /// Sort matches newest / largest first (ties by slot, so the order is total). The
    /// ascending orders are this list read backwards. Chunks sort on all cores; the final
    /// stable sort only merges those runs.
    pub fn sort_slots(&self, slots: Vec<u32>, key: MetaKey, dirs: Option<&[u64]>) -> Vec<u32> {
        let mut pairs = self.sort_keys(&slots, key, dirs);
        let cmp = |a: &(i64, u32), b: &(i64, u32)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
        let hw = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(1);
        let nt = hw.min(pairs.len() / 65536).max(1);
        if nt > 1 {
            let chunk = pairs.len().div_ceil(nt);
            std::thread::scope(|s| {
                for c in pairs.chunks_mut(chunk) {
                    s.spawn(move || c.sort_unstable_by(cmp));
                }
            });
            pairs.sort_by(cmp); // run-aware merge of the sorted chunks
        } else {
            pairs.sort_unstable_by(cmp);
        }
        pairs.into_iter().map(|(_, i)| i).collect()
    }

    /// The first `k` rows of [`Index::sort_slots`] without sorting the rest.
    pub fn top_slots(&self, slots: &[u32], key: MetaKey, k: usize, dirs: Option<&[u64]>) -> Vec<u32> {
        let mut pairs = self.sort_keys(slots, key, dirs);
        let cmp = |a: &(i64, u32), b: &(i64, u32)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
        let k = k.min(pairs.len());
        if k < pairs.len() && k > 0 {
            pairs.select_nth_unstable_by(k - 1, cmp);
        }
        pairs.truncate(k);
        pairs.sort_unstable_by(cmp);
        pairs.into_iter().map(|(_, i)| i).collect()
    }

    /// Execute a query for one window. The name orders give name-sorted results (ties by
    /// path, `NameDesc` reversed); the meta orders give newest / largest first (or the exact
    /// reverse). The scan runs in storage order (sequential memory) over
    /// `available_parallelism` chunks of slots. Works on a dirty index (merges pending
    /// changes on the fly); call [`Index::refresh`] first for best latency.
    ///
    /// A caller that pages through one query should keep [`Index::name_matches`] /
    /// [`Index::sort_slots`] instead: every further window is then free.
    pub fn search(&self, q: &Query, o: &SearchOptions) -> SearchResult {
        let (key, asc) = match o.order {
            Order::NameAsc | Order::NameDesc => {
                let set = self.name_matches(q);
                let items = self.name_page(&set, o.offset, o.limit, o.order == Order::NameDesc);
                return SearchResult { total: set.total(), items };
            }
            Order::MtimeDesc => (MetaKey::Mtime, false),
            Order::MtimeAsc => (MetaKey::Mtime, true),
            Order::SizeDesc => (MetaKey::Size, false),
            Order::SizeAsc => (MetaKey::Size, true),
            Order::TypeAsc => (MetaKey::Ext, false),
            Order::TypeDesc => (MetaKey::Ext, true),
        };
        let slots = self.match_slots(q);
        let total = slots.len();
        let items = if asc {
            let sorted = self.sort_slots(slots, key, None);
            let hi = total.saturating_sub(o.offset);
            let lo = hi.saturating_sub(o.limit);
            sorted[lo..hi].iter().rev().copied().collect()
        } else {
            let k = o.offset.saturating_add(o.limit).min(total);
            let top = self.top_slots(&slots, key, k, None);
            top[o.offset.min(k)..].to_vec()
        };
        SearchResult { total, items }
    }

    pub fn search_str(&self, q: &str, o: &SearchOptions) -> SearchResult {
        self.search(&Query::parse(q), o)
    }
}
