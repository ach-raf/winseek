//! Query parsing and parallel execution.
//!
//! Syntax (terms are ANDed):
//! * whitespace separates terms; `"quoted phrase"` is one term
//! * `!term` negates; `!"a b"` too
//! * `*` / `?` in a term: whole-name wildcard match (whole *path* if the term has `\`)
//! * a term containing `\` (or `/`) is matched against the full path (substring)
//! * `ws:cat` the name has a word that starts with `cat` (`my cat.jpg`, `cat_food.png`, `Cats.png`;
//!   not `intricate.png`); see `fold::word_start` for what starts a word
//! * `ext:jpg;png` (also `,`), `file:`, `folder:` (alone = filter, `folder:foo` = filter + term)
//! * `size:>100mb`, `size:<=4kb`, `size:1mb..2gb`, `size:0` (files only; b/kb/mb/gb/tb, 1024-based)
//! * `dm:A..B` date modified in unix milliseconds, `[A, B)`; either end may be left out
//!   (`dm:1700000000000..`). The app turns "last week" / "summer 2023" into this.
//!
//! Matching is case-insensitive (see `fold`). Execution scans the sorted order array in
//! parallel chunks (so hits are already in name order), cheapest terms first.
//!
//! [`Order::Relevance`] (Best match) then orders the hits by how useful they are for what was
//! typed ([`crate::rank`]): the whole name or its start before a word inside it, that before the
//! letters inside a word; then folders, recent, shallow, outside build and cache folders.

use crate::fold::*;
use crate::rank::{
    age_days, is_noise_dir, now_filetime, rank_key, rank_score, MatchClass, RankFacts,
};
use crate::store::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Cond {
    /// Substring; `needle` is folded. `ascii` = needle is pure ASCII (fast path).
    Text {
        needle: String,
        ascii: bool,
        path: bool,
    },
    /// Like `Text` on the name, but the match must begin a word. `needle` is folded.
    Word {
        needle: String,
        chars: Vec<char>,
        ascii: bool,
    },
    /// Whole match with `*`/`?`; pattern is folded.
    Glob {
        pat: Vec<char>,
        pat8: Option<Vec<u8>>,
        path: bool,
    },
    Ext(Vec<String>),
    File,
    Folder,
    /// File size in bytes, inclusive bounds (folders never match).
    Size {
        min: u64,
        max: u64,
    },
    /// Modified time as stored (FILETIME), `[from, to)`.
    Mtime {
        from: i64,
        to: i64,
    },
}

/// 1601 -> 1970 in 100 ns units (the store keeps raw FILETIMEs).
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

fn parse_bytes(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let cut = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
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
        (
            if a.is_empty() { 0 } else { parse_bytes(a)? },
            if b.is_empty() {
                u64::MAX
            } else {
                parse_bytes(b)?
            },
        )
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
    let ft = |ms: &str| {
        ms.parse::<i64>().ok().map(|ms| {
            ms.saturating_mul(10_000)
                .saturating_add(FILETIME_UNIX_EPOCH)
        })
    };
    Some(Cond::Mtime {
        from: if a.is_empty() { i64::MIN } else { ft(a)? },
        to: if b.is_empty() { i64::MAX } else { ft(b)? },
    })
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
    /// Best match first ([`crate::rank`]). A query with no typed word to rank by (filters, a
    /// path, wildcards only) is a name search: it answers in `NameAsc` order.
    Relevance,
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

/// Matches by storage slot. A slot keeps its number while other records come and go (ranks
/// shift on every insert), so a cached query stays current by re-testing only the records a
/// batch touched — Everything patches its open queries the same way instead of re-running
/// them — and rebuilds its [`RankSet`] from this with [`Index::ranks_of`] when paged again.
#[derive(Debug, Clone, Default)]
pub struct SlotSet {
    bits: Vec<u64>,
}

impl SlotSet {
    pub fn get(&self, i: u32) -> bool {
        self.bits
            .get((i >> 6) as usize)
            .is_some_and(|w| (w >> (i & 63)) & 1 == 1)
    }

    pub fn set(&mut self, i: u32, on: bool) {
        let w = (i >> 6) as usize;
        if w >= self.bits.len() {
            if !on {
                return;
            }
            self.bits.resize(w + 1, 0);
        }
        let m = 1u64 << (i & 63);
        if on {
            self.bits[w] |= m;
        } else {
            self.bits[w] &= !m;
        }
    }

    /// Heap bytes held (for cache accounting).
    pub fn mem_bytes(&self) -> usize {
        self.bits.len() * 8
    }
}

impl RankSet {
    fn all(n: usize) -> Self {
        RankSet {
            bits: None,
            dir: Vec::new(),
            n,
            total: n,
        }
    }

    fn from_bits(bits: Vec<u64>, n: usize) -> Self {
        let mut dir = Vec::with_capacity(bits.len() / BLOCK + 1);
        let mut acc = 0u32;
        for b in bits.chunks(BLOCK) {
            dir.push(acc);
            acc += b.iter().map(|w| w.count_ones()).sum::<u32>();
        }
        RankSet {
            bits: Some(bits),
            dir,
            n,
            total: acc as usize,
        }
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
        SearchOptions {
            offset: 0,
            limit: usize::MAX,
            order: Order::NameAsc,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    /// Total number of matches (ignoring offset/limit).
    pub total: usize,
    /// Record indices of the requested window.
    pub items: Vec<u32>,
}

/// Query-side cleanup before a term becomes a match condition. A path copied out of JSON
/// (`E:\\a\\b`), a console/ffmpeg arg (`'C:/v/clip.mp4'`), or a `file:///E:/a%20b.mp4` URL
/// must land on the same needle the stored single-backslash paths use. Syntax the user
/// asked for on purpose (wildcards, operators, negation — stripped by the tokenizer) is
/// untouched; only URL clothes, wrapping quotes, and separator runs are removed.
fn normalize_term_text(text: &str) -> String {
    let mut t = text.to_string();
    let lower = t.to_ascii_lowercase();
    let mut url = false;
    if lower.starts_with("file:///") {
        t.drain(..8);
        url = true;
    } else if lower.starts_with("file://") {
        t.drain(..7);
        url = true;
    }
    if url {
        t = percent_decode(&t);
    }
    // one wrapping quote pair, e.g. 'C:\v\clip.mp4' glued into the term by a console line
    let ch: Vec<char> = t.chars().collect();
    if ch.len() >= 2 {
        let f = ch[0];
        if (f == '"' || f == '\'') && f == ch[ch.len() - 1] {
            t = ch[1..ch.len() - 1].iter().collect();
        }
    }
    t
}

/// Runs of `\` and `/`, doubled or mixed, become one `\` (`E:\\a\\b` from JSON text and
/// `E:/a/b` both land on the stored form). A leading single `\` survives: `\Windows` stays
/// an any-drive fragment.
fn collapse_separators(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_sep = false;
    for c in text.chars() {
        let sep = c == '\\' || c == '/';
        if sep {
            if !prev_sep {
                out.push('\\');
            }
        } else {
            out.push(c);
        }
        prev_sep = sep;
    }
    out
}

/// `%XX` -> byte, best effort; a `%` that is not a valid escape stays literal.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let hex = |c: u8| (c as char).to_digit(16);
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(hi), Some(lo)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn text_term(text: &str, negate: bool) -> Option<Term> {
    let text = normalize_term_text(text);
    if text.is_empty() {
        return None;
    }
    let path = text.contains('\\') || text.contains('/');
    let text = if path {
        collapse_separators(&text)
    } else {
        text
    };
    let folded = fold_string(&text);
    let cond = if folded.contains('*') || folded.contains('?') {
        Cond::Glob {
            pat8: folded.is_ascii().then(|| folded.as_bytes().to_vec()),
            pat: folded.chars().collect(),
            path,
        }
    } else {
        let ascii = folded.is_ascii();
        Cond::Text {
            needle: folded,
            ascii,
            path,
        }
    };
    Some(Term { cond, negate })
}

impl Query {
    /// Whether a record's match can change without the record itself changing: a path term
    /// sees its ancestors, so a folder rename or move can flip every record below it.
    pub fn path_sensitive(&self) -> bool {
        self.terms.iter().any(|t| {
            matches!(
                t.cond,
                Cond::Text { path: true, .. } | Cond::Glob { path: true, .. }
            )
        })
    }

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
                if matches!(w.as_str(), "ext" | "file" | "folder" | "size" | "dm" | "ws") {
                    // `file://…` is a pasted URL, not the file-only operator
                    let is_url = w == "file" && j + 2 < n && ch[j + 1] == '/' && ch[j + 2] == '/';
                    if !is_url {
                        prefix = Some(w);
                        i = j + 1;
                    }
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
                        terms.push(Term {
                            cond: Cond::Ext(list),
                            negate,
                        });
                    }
                }
                Some("ws") => {
                    // A path fragment or a wildcard has no word to start: ordinary term.
                    let folded = fold_string(&text);
                    if folded.is_empty() {
                    } else if folded.contains(['\\', '/', '*', '?']) {
                        terms.extend(text_term(&text, negate));
                    } else {
                        let cond = Cond::Word {
                            ascii: folded.is_ascii(),
                            chars: folded.chars().collect(),
                            needle: folded,
                        };
                        terms.push(Term { cond, negate });
                    }
                }
                Some(p @ ("size" | "dm")) => {
                    // A value that does not parse is ordinary text ("size:huge" finds that name).
                    match if p == "size" {
                        parse_size(&text)
                    } else {
                        parse_mtime(&text)
                    } {
                        Some(cond) => terms.push(Term { cond, negate }),
                        None => terms.extend(text_term(&format!("{p}:{text}"), negate)),
                    }
                }
                Some(p) => {
                    let cond = if p == "file" {
                        Cond::File
                    } else {
                        Cond::Folder
                    };
                    if text.is_empty() {
                        terms.push(Term { cond, negate });
                    } else {
                        terms.push(Term {
                            cond,
                            negate: false,
                        });
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
            Cond::Text {
                ascii: true,
                path: false,
                ..
            }
            | Cond::Word { ascii: true, .. } => 2,
            Cond::Text { path: false, .. } | Cond::Word { .. } => 3,
            Cond::Glob { path: false, .. } => 4,
            Cond::Text { .. } => 5,
            Cond::Glob { .. } => 6,
        });
        Query { terms }
    }

    /// Has a typed word that [`MatchClass`] can place in a name, so Best match has something to
    /// rank by? Filters, wildcards, path terms and negations only decide *whether* a record matches.
    pub fn ranks_by_name(&self) -> bool {
        self.terms.iter().any(|t| {
            !t.negate && matches!(t.cond, Cond::Text { path: false, .. } | Cond::Word { .. })
        })
    }

    /// The terms that give a match its class (those of [`Query::ranks_by_name`]), and their
    /// typed letters in bytes together.
    fn classed_terms(&self) -> (Vec<usize>, usize) {
        let mut idx = Vec::new();
        let mut typed = 0;
        for (k, t) in self.terms.iter().enumerate() {
            if t.negate {
                continue;
            }
            if let Cond::Text {
                needle,
                path: false,
                ..
            }
            | Cond::Word { needle, .. } = &t.cond
            {
                idx.push(k);
                typed += needle.len();
            }
        }
        (idx, typed)
    }
}

#[derive(Default)]
struct Scratch {
    path: Vec<u8>,
    fold: String,
    chars: Vec<char>,
    chain: Vec<u32>,
    up: Vec<u32>,
}

/// Per-query folder verdicts of one path substring term, indexed by slot: 0 unknown, 1 the
/// folder's path does not hold the needle, 2 it does. Everything matches folders first and
/// lets files inherit (`FUN_1400ae4b0` before `FUN_1400af3b0`); here a file's path holds the
/// needle iff its folder's does or the needle runs into `"\" + name`, so a scan never builds
/// whole paths. Threads race benignly: every writer stores the same value.
type PathMemo = Vec<std::sync::atomic::AtomicU8>;

/// A [`PathMemo`] of `n` unknown verdicts, from zeroed pages (no per-slot writes).
fn zeroed_memo(n: usize) -> PathMemo {
    let mut v = std::mem::ManuallyDrop::new(vec![0u8; n]);
    // SAFETY: `AtomicU8` has the same size, alignment and bit validity as `u8`, and the
    // allocation is handed over whole (pointer, length and capacity unchanged).
    unsafe {
        Vec::from_raw_parts(
            v.as_mut_ptr() as *mut std::sync::atomic::AtomicU8,
            v.len(),
            v.capacity(),
        )
    }
}

/// Does folded `needle` occur in `hay` (as `Cond::Text` matches)?
fn text_has(hay: &[u8], needle: &str, ascii: bool, fold: &mut String) -> bool {
    if ascii {
        find_ascii_ci(hay, needle.as_bytes())
    } else if hay.is_ascii() {
        false // a non-ASCII folded needle cannot occur in pure-ASCII text
    } else {
        match std::str::from_utf8(hay) {
            Ok(s) => {
                fold_into(s, fold);
                fold.contains(needle)
            }
            Err(_) => false,
        }
    }
}

const SEP: u8 = b'\\';
const M_HAS_KNOWN: u8 = 1;
const M_HAS: u8 = 2;
const M_ENDS_KNOWN: u8 = 4;
const M_ENDS: u8 = 8;

/// A folded path needle split at its last separator: `head` + `\` + `rest`.
struct PathNeedle<'a> {
    whole: &'a str,
    head: &'a str,
    rest: &'a str,
    ascii: bool,
}

impl<'a> PathNeedle<'a> {
    /// `whole` must hold a separator.
    fn new(whole: &'a str, ascii: bool) -> Self {
        let k = whole.rfind('\\').unwrap_or(0);
        PathNeedle {
            whole,
            head: &whole[..k],
            rest: whole.get(k + 1..).unwrap_or(""),
            ascii,
        }
    }
}

/// Does folded `pre` start `hay` (folding `hay`)?
fn starts_folded(hay: &[u8], pre: &str, ascii: bool, fold: &mut String) -> bool {
    if pre.is_empty() {
        return true;
    }
    if ascii && hay.is_ascii() {
        return hay.len() >= pre.len() && hay[..pre.len()].eq_ignore_ascii_case(pre.as_bytes());
    }
    match std::str::from_utf8(hay) {
        Ok(s) => {
            fold_into(s, fold);
            fold.starts_with(pre)
        }
        Err(_) => false,
    }
}

/// Does folded `suf` end `hay` (folding `hay`)?
fn ends_folded(hay: &[u8], suf: &str, ascii: bool, fold: &mut String) -> bool {
    if ascii && hay.is_ascii() {
        return hay.len() >= suf.len()
            && hay[hay.len() - suf.len()..].eq_ignore_ascii_case(suf.as_bytes());
    }
    match std::str::from_utf8(hay) {
        Ok(s) => {
            fold_into(s, fold);
            fold.ends_with(suf)
        }
        Err(_) => false,
    }
}

/// Is folded `hay` exactly `needle` (folding `hay`)?
fn eq_folded(hay: &[u8], needle: &str, ascii: bool, fold: &mut String) -> bool {
    if ascii && hay.is_ascii() {
        return hay.eq_ignore_ascii_case(needle.as_bytes());
    }
    match std::str::from_utf8(hay) {
        Ok(s) => {
            fold_into(s, fold);
            fold == needle
        }
        Err(_) => false,
    }
}

/// `name` without its last extension, when it has one that is not the whole name (`.gitignore`
/// and `Makefile` keep theirs; `awb.xlsx` is `awb`).
fn stem_of(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b'.') {
        Some(d) if d > 0 && d + 1 < name.len() => &name[..d],
        _ => name,
    }
}

/// Can a whole-path glob match a record named `name`? Its literal tail (after the last `*`
/// or `?`) ends the path, so it ends the name, or, holding a separator, its last segment is
/// the whole name. Cheap enough to run before building the path.
fn glob_tail_fits(pat: &[char], name: &[u8], sc: &mut Scratch) -> bool {
    let k = pat
        .iter()
        .rposition(|&c| c == '*' || c == '?')
        .map_or(0, |k| k + 1);
    let lit = &pat[k..];
    if lit.is_empty() {
        return true;
    }
    sc.chars.clear();
    sc.chars.extend(
        std::str::from_utf8(name)
            .unwrap_or("")
            .chars()
            .map(fold_char),
    );
    match lit.iter().rposition(|&c| c == '\\') {
        Some(j) => sc.chars[..] == lit[j + 1..],
        None => sc.chars.ends_with(lit),
    }
}

fn char_len(b: &[u8]) -> usize {
    if b.is_ascii() {
        b.len()
    } else {
        std::str::from_utf8(b).map_or(b.len(), |s| s.chars().count())
    }
}

fn ext_of(name: &str) -> Option<&str> {
    let p = name.rfind('.')?;
    let e = &name[p + 1..];
    (!e.is_empty()).then_some(e)
}

impl Index {
    /// `sc.path` = the last `want` (or more) chars of `p`'s path as its children see it (a
    /// volume root contributes its bare name, `C:`).
    fn write_suffix(&self, p: u32, want: usize, sc: &mut Scratch) {
        sc.chain.clear();
        let (mut x, mut got) = (p, 0usize);
        while x != NONE && sc.chain.len() < MAX_DEPTH {
            sc.chain.push(x);
            got += char_len(self.name_bytes(x)) + 1;
            if got > want {
                break;
            }
            x = self.parent[x as usize];
        }
        sc.path.clear();
        for k in (0..sc.chain.len()).rev() {
            if k + 1 != sc.chain.len() {
                sc.path.push(SEP);
            }
            sc.path.extend_from_slice(self.name_bytes(sc.chain[k]));
        }
    }

    /// Does folded `head` end `f`'s path (as its children see it)? Memoized in the `M_ENDS` bits.
    fn folder_ends(&self, head: &str, ascii: bool, f: u32, m: &PathMemo, sc: &mut Scratch) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        let s = m[f as usize].load(Relaxed);
        if s & M_ENDS_KNOWN != 0 {
            return s & M_ENDS != 0;
        }
        let v = head.is_empty() || {
            self.write_suffix(f, head.chars().count(), sc);
            ends_folded(&sc.path, head, ascii, &mut sc.fold)
        };
        m[f as usize].fetch_or(M_ENDS_KNOWN | if v { M_ENDS } else { 0 }, Relaxed);
        v
    }

    /// Does the needle run over the separator in front of `name(i)` (`i`'s parent is `p`)?
    /// Such a match is `head` ending `p`'s path and `rest` starting the name, with the needle
    /// split at its last separator (a name holds none).
    fn crosses(&self, nd: &PathNeedle, p: u32, i: u32, m: &PathMemo, sc: &mut Scratch) -> bool {
        starts_folded(self.name_bytes(i), nd.rest, nd.ascii, &mut sc.fold)
            && self.folder_ends(nd.head, nd.ascii, p, m, sc)
    }

    /// Whether `f`'s path, as a prefix of its children's paths, holds the needle (memoized).
    fn folder_has(&self, nd: &PathNeedle, f: u32, m: &PathMemo, sc: &mut Scratch) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        // Climb to the first known (or root) folder, then settle the chain downwards.
        let mut chain = std::mem::take(&mut sc.up);
        chain.clear();
        let mut x = f;
        let mut known = loop {
            let s = m[x as usize].load(Relaxed);
            if s & M_HAS_KNOWN != 0 {
                break s & M_HAS != 0;
            }
            let p = self.parent[x as usize];
            if p == NONE || chain.len() >= MAX_DEPTH {
                // A root: its bare name (`C:`) is what its children's paths start with.
                let v = text_has(self.name_bytes(x), nd.whole, nd.ascii, &mut sc.fold);
                m[x as usize].fetch_or(M_HAS_KNOWN | if v { M_HAS } else { 0 }, Relaxed);
                break v;
            }
            chain.push(x);
            x = p;
        };
        for &c in chain.iter().rev() {
            known = known || self.crosses(nd, self.parent[c as usize], c, m, sc);
            m[c as usize].fetch_or(M_HAS_KNOWN | if known { M_HAS } else { 0 }, Relaxed);
        }
        sc.up = chain;
        known
    }

    fn eval(&self, t: &Term, i: u32, sc: &mut Scratch, memo: Option<&PathMemo>) -> bool {
        let m = match &t.cond {
            Cond::File => !self.is_dir(i),
            Cond::Folder => self.is_dir(i),
            Cond::Size { min, max } => {
                !self.is_dir(i) && self.size(i).is_some_and(|s| s >= *min && s <= *max)
            }
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
            Cond::Text {
                needle,
                ascii,
                path,
            } => {
                let p = self.parent[i as usize];
                match memo {
                    Some(m) if *path && p != NONE && needle.as_bytes().contains(&SEP) => {
                        let nd = PathNeedle::new(needle, *ascii);
                        self.folder_has(&nd, p, m, sc) || self.crosses(&nd, p, i, m, sc)
                    }
                    _ => {
                        if *path {
                            self.write_path(i, &mut sc.path);
                            text_has(&sc.path, needle, *ascii, &mut sc.fold)
                        } else {
                            text_has(self.name_bytes(i), needle, *ascii, &mut sc.fold)
                        }
                    }
                }
            }
            Cond::Word {
                needle,
                chars,
                ascii,
            } => {
                let hay = self.name_bytes(i);
                if hay.is_ascii() {
                    *ascii && find_word_ascii_ci(hay, needle.as_bytes())
                } else {
                    sc.chars.clear();
                    sc.chars
                        .extend(std::str::from_utf8(hay).unwrap_or("").chars());
                    find_word(&sc.chars, chars)
                }
            }
            Cond::Glob {
                pat, path: true, ..
            } if !glob_tail_fits(pat, self.name_bytes(i), sc) => false,
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
                        sc.chars.extend(
                            std::str::from_utf8(hay)
                                .unwrap_or("")
                                .chars()
                                .map(fold_char),
                        );
                        glob_match(pat, &sc.chars)
                    }
                }
            }
        };
        m != t.negate
    }

    /// Scan storage slots `lo..hi` sequentially and hand every match to `hit(slot, rank)`.
    fn scan_each(
        &self,
        q: &Query,
        memos: &[Option<PathMemo>],
        view: &OrderView,
        lo: usize,
        hi: usize,
        mut hit: impl FnMut(u32, u32),
    ) {
        let mut sc = Scratch::default();
        for i in lo..hi {
            let r = view.rank(i);
            if r != NONE
                && q.terms
                    .iter()
                    .zip(memos.iter())
                    .all(|(t, m)| self.eval(t, i as u32, &mut sc, m.as_ref()))
            {
                hit(i as u32, r);
            }
        }
    }

    /// One [`PathMemo`] per path substring term of `q` (`None` for every other term), shared
    /// by the threads of one scan of this index state.
    fn path_memos(&self, q: &Query) -> Vec<Option<PathMemo>> {
        let n = self.parent.len();
        q.terms
            .iter()
            .map(|t| matches!(t.cond, Cond::Text { path: true, .. }).then(|| zeroed_memo(n)))
            .collect()
    }

    /// Split `0..slots` into `available_parallelism` chunks and run `scan` over them; the
    /// per-chunk results come back in slot order.
    fn parallel_scan<T: Send>(
        &self,
        slots: usize,
        scan: impl Fn(usize, usize) -> T + Send + Sync,
    ) -> Vec<T> {
        let hw = std::thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(1);
        const MIN_CHUNK: usize = 8192;
        let nt = hw.min(slots / MIN_CHUNK).max(1);
        if nt == 1 {
            return vec![scan(0, slots)];
        }
        let chunk = slots.div_ceil(nt);
        let scan = &scan;
        std::thread::scope(|s| {
            let hs: Vec<_> = (0..nt)
                .map(|k| {
                    s.spawn(move || scan((k * chunk).min(slots), ((k + 1) * chunk).min(slots)))
                })
                .collect();
            hs.into_iter()
                .map(|h| h.join().expect("scan thread"))
                .collect()
        })
    }

    /// Number of matches, without materializing them (the type-chip counters).
    pub fn count(&self, q: &Query) -> usize {
        let view = self.order_view();
        if q.terms.is_empty() {
            return view.len();
        }
        let memos = self.path_memos(q);
        self.parallel_scan(view.slots(), |lo, hi| {
            let mut n = 0usize;
            self.scan_each(q, &memos, &view, lo, hi, |_, _| n += 1);
            n
        })
        .into_iter()
        .sum()
    }

    /// How many matches of `q` also match each of `kinds` (the type-chip counters), in ONE pass:
    /// `out[k] == self.count(q AND kinds[k])`. A kind with a path term falls back to its own count.
    pub fn count_kinds(&self, q: &Query, kinds: &[Query]) -> Vec<usize> {
        if kinds.iter().any(|k| k.path_sensitive()) {
            return kinds
                .iter()
                .map(|k| {
                    let mut both = q.clone();
                    both.terms.extend(k.terms.iter().cloned());
                    self.count(&both)
                })
                .collect();
        }
        let view = self.order_view();
        let memos = self.path_memos(q);
        self.parallel_scan(view.slots(), |lo, hi| {
            let mut n = vec![0usize; kinds.len()];
            let mut sc = Scratch::default();
            self.scan_each(q, &memos, &view, lo, hi, |slot, _| {
                for (k, kq) in kinds.iter().enumerate() {
                    if kq.terms.iter().all(|t| self.eval(t, slot, &mut sc, None)) {
                        n[k] += 1;
                    }
                }
            });
            n
        })
        .into_iter()
        .fold(vec![0usize; kinds.len()], |mut a, b| {
            for (x, y) in a.iter_mut().zip(b) {
                *x += y;
            }
            a
        })
    }

    /// Every match as a [`RankSet`] (name order). One parallel pass over the names; the
    /// scan threads set their bits directly, so nothing is collected or sorted.
    pub fn name_matches(&self, q: &Query) -> RankSet {
        use std::sync::atomic::{AtomicU64, Ordering};
        let view = self.order_view();
        let n = view.len();
        if q.terms.is_empty() {
            return RankSet::all(n);
        }
        let bits: Vec<AtomicU64> = (0..n.div_ceil(64)).map(|_| AtomicU64::new(0)).collect();
        let bits_ref = &bits;
        let memos = self.path_memos(q);
        self.parallel_scan(view.slots(), |lo, hi| {
            self.scan_each(q, &memos, &view, lo, hi, |_, r| {
                bits_ref[(r >> 6) as usize].fetch_or(1u64 << (r & 63), Ordering::Relaxed);
            })
        });
        RankSet::from_bits(bits.into_iter().map(AtomicU64::into_inner).collect(), n)
    }

    /// [`name_matches`](Self::name_matches) plus the same matches by storage slot, from one scan:
    /// the [`SlotSet`] is what a cached query keeps current as the index changes.
    pub fn name_matches_live(&self, q: &Query) -> (RankSet, SlotSet) {
        use std::sync::atomic::{AtomicU64, Ordering};
        let view = self.order_view();
        let (n, slots) = (view.len(), view.slots());
        let words = |len: usize| -> Vec<AtomicU64> {
            (0..len.div_ceil(64)).map(|_| AtomicU64::new(0)).collect()
        };
        let (ranks, by_slot) = (words(n), words(slots));
        let (ranks_ref, slot_ref) = (&ranks, &by_slot);
        let memos = self.path_memos(q);
        self.parallel_scan(slots, |lo, hi| {
            self.scan_each(q, &memos, &view, lo, hi, |i, r| {
                ranks_ref[(r >> 6) as usize].fetch_or(1u64 << (r & 63), Ordering::Relaxed);
                slot_ref[(i >> 6) as usize].fetch_or(1u64 << (i & 63), Ordering::Relaxed);
            })
        });
        let take = |v: Vec<AtomicU64>| v.into_iter().map(AtomicU64::into_inner).collect();
        (
            RankSet::from_bits(take(ranks), n),
            SlotSet {
                bits: take(by_slot),
            },
        )
    }

    /// Whether record `i` is live and matches `q`: one record, no memo (a path term builds its
    /// path), for patching a cached [`SlotSet`] with the records a batch touched.
    pub fn slot_matches(&self, q: &Query, i: u32) -> bool {
        if i as usize >= self.parent.len() || self.is_deleted(i) {
            return false;
        }
        let mut sc = Scratch::default();
        q.terms.iter().all(|t| self.eval(t, i, &mut sc, None))
    }

    /// The name-order [`RankSet`] of `set` under the current order: one parallel pass of bit
    /// lookups, no matching. Slots that are no longer live drop out here.
    pub fn ranks_of(&self, set: &SlotSet) -> RankSet {
        use std::sync::atomic::{AtomicU64, Ordering};
        let view = self.order_view();
        let n = view.len();
        let bits: Vec<AtomicU64> = (0..n.div_ceil(64)).map(|_| AtomicU64::new(0)).collect();
        let bits_ref = &bits;
        self.parallel_scan(view.slots(), |lo, hi| {
            // Walk the set's words, not every slot: a typical cached query holds a small share
            // of the index, so most words are empty and cost one load.
            let mut i = lo;
            while i < hi {
                let word = set.bits.get(i >> 6).copied().unwrap_or(0) >> (i & 63);
                if word == 0 {
                    i = ((i >> 6) + 1) << 6;
                    continue;
                }
                i += word.trailing_zeros() as usize;
                if i >= hi {
                    break;
                }
                let r = view.rank(i);
                if r != NONE {
                    bits_ref[(r >> 6) as usize].fetch_or(1u64 << (r & 63), Ordering::Relaxed);
                }
                i += 1;
            }
        });
        RankSet::from_bits(bits.into_iter().map(AtomicU64::into_inner).collect(), n)
    }

    /// Record indices of one window of a [`RankSet`] built on this same index state.
    pub fn name_page(&self, set: &RankSet, offset: usize, limit: usize, desc: bool) -> Vec<u32> {
        let view = self.order_view();
        debug_assert_eq!(view.len(), set.n, "RankSet used on a different index state");
        set.window(offset, limit, desc)
            .into_iter()
            .filter_map(|r| view.at(r as usize))
            .collect()
    }

    /// Every matching record index, in storage order (input for the meta orders).
    pub fn match_slots(&self, q: &Query) -> Vec<u32> {
        let view = self.order_view();
        if q.terms.is_empty() {
            return (0..view.slots() as u32)
                .filter(|&i| view.rank(i as usize) != NONE)
                .collect();
        }
        let memos = self.path_memos(q);
        let parts = self.parallel_scan(view.slots(), |lo, hi| {
            let mut out = Vec::new();
            self.scan_each(q, &memos, &view, lo, hi, |i, _| out.push(i));
            out
        });
        let mut all = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
        for p in parts {
            all.extend(p);
        }
        all
    }

    // ------------------------------------------------------------------ Best match
    /// How `cond`, a typed word the name of record `i` holds, sits in that name. `None` for every
    /// other condition: they decide whether a record matches, not how good it is.
    fn name_class(&self, i: u32, cond: &Cond, sc: &mut Scratch) -> Option<MatchClass> {
        let (needle, ascii) = match cond {
            Cond::Text {
                needle,
                ascii,
                path: false,
            } => (needle, *ascii),
            Cond::Word { needle, ascii, .. } => (needle, *ascii),
            _ => return None,
        };
        let hay = self.name_bytes(i);
        if eq_folded(hay, needle, ascii, &mut sc.fold) {
            return Some(MatchClass::Exact);
        }
        let stem = stem_of(hay);
        if stem.len() != hay.len()
            && !self.is_dir(i)
            && eq_folded(stem, needle, ascii, &mut sc.fold)
        {
            return Some(MatchClass::Exact);
        }
        if starts_folded(hay, needle, ascii, &mut sc.fold) {
            return Some(MatchClass::Prefix);
        }
        // It holds the word somewhere (it matched): at the start of a word, or inside one.
        let word = if hay.is_ascii() {
            ascii && find_word_ascii_ci(hay, needle.as_bytes())
        } else {
            sc.chars.clear();
            sc.chars
                .extend(std::str::from_utf8(hay).unwrap_or("").chars());
            let typed: Vec<char> = needle.chars().collect();
            find_word(&sc.chars, &typed)
        };
        Some(if word {
            MatchClass::Word
        } else {
            MatchClass::Part
        })
    }

    /// Where folder `f` sits: bit 0 = known, bit 1 = a noise folder is among its ancestors or it
    /// is one, bits 2.. = its depth below the volume root (0..=63). Memoized in `m` (one
    /// [`PathMemo`] per query); threads race benignly: every writer stores the same value.
    fn folder_loc(&self, f: u32, m: &PathMemo, chain: &mut Vec<u32>) -> u8 {
        use std::sync::atomic::Ordering::Relaxed;
        const KNOWN: u8 = 1;
        const NOISE: u8 = 2;
        chain.clear();
        let mut x = f;
        // The verdict above the chain: a folder already settled, or nothing (the volume root).
        let mut cur = 0u8;
        loop {
            let s = m[x as usize].load(Relaxed);
            if s & KNOWN != 0 {
                cur = s;
                break;
            }
            chain.push(x);
            let p = self.parent[x as usize];
            if self.flags[x as usize] & F_ROOT != 0
                || p == NONE
                || p == x
                || chain.len() >= MAX_DEPTH
            {
                break;
            }
            x = p;
        }
        for &c in chain.iter().rev() {
            let root = self.flags[c as usize] & F_ROOT != 0;
            let d = if root { 0 } else { (cur >> 2).min(62) + 1 };
            let noise = !root && (cur & NOISE != 0 || is_noise_dir(self.name_bytes(c)));
            cur = KNOWN | if noise { NOISE } else { 0 } | (d << 2);
            m[c as usize].store(cur, Relaxed);
        }
        cur
    }

    /// Best-match sort keys for the matches `slots` of `q`, in `slots` order: the packed
    /// [`rank_key`] (score, then name, then slot), so the biggest key is the best result. One
    /// pass over the matches on all cores; `now_ft` is the clock as FILETIME (the recency weight).
    pub fn rank_keys(&self, q: &Query, slots: &[u32], now_ft: i64) -> Vec<(i64, u32)> {
        // Threads get at least this many matches each: a handful of matches stay on one thread.
        const MIN_CHUNK: usize = 4096;
        let view = self.order_view();
        let memo = zeroed_memo(self.parent.len());
        let (classed, typed_len) = q.classed_terms();
        let mut out: Vec<(i64, u32)> = slots.iter().map(|&i| (0, i)).collect();
        let hw = std::thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(1);
        let nt = hw.min(out.len() / MIN_CHUNK).max(1);
        let chunk = out.len().div_ceil(nt).max(1); // no matches: one empty pass, not a zero-size chunk
                                                   // shared by every thread: the verdicts and the view are read-only, the atomics race benignly
        let (memo, classed, view) = (&memo, &classed, &view);
        std::thread::scope(|s| {
            for part in out.chunks_mut(chunk) {
                s.spawn(move || {
                    let (memo, classed, view) = (memo, classed, view);
                    let mut sc = Scratch::default();
                    let mut classes: Vec<MatchClass> = Vec::with_capacity(classed.len());
                    for entry in part.iter_mut() {
                        let i = entry.1;
                        classes.clear();
                        for &t in classed.iter() {
                            if let Some(c) = self.name_class(i, &q.terms[t].cond, &mut sc) {
                                classes.push(c);
                            }
                        }
                        let p = self.parent[i as usize];
                        let loc = if p == NONE {
                            0
                        } else {
                            self.folder_loc(p, memo, &mut sc.up)
                        };
                        let score = rank_score(&RankFacts {
                            classes: &classes,
                            name_len: self.name_bytes(i).len(),
                            typed_len,
                            is_dir: self.is_dir(i),
                            depth: u16::from(loc >> 2),
                            noisy: loc & 2 != 0,
                            age_days: self.mtime(i).map(|m| age_days(now_ft, m)),
                        });
                        entry.0 = rank_key(score, view.rank(i as usize), i);
                    }
                });
            }
        });
        out
    }

    /// The matches `slots` of `q`, best first.
    pub fn ranked_slots(&self, q: &Query, slots: &[u32], now_ft: i64) -> Vec<u32> {
        Self::sort_pairs(self.rank_keys(q, slots, now_ft))
    }

    /// The first `k` of the matches `slots` of `q`, best first, without ordering the rest.
    pub fn top_ranked_slots(&self, q: &Query, slots: &[u32], now_ft: i64, k: usize) -> Vec<u32> {
        Self::top_pairs(self.rank_keys(q, slots, now_ft), k)
    }

    /// Sort key of one record for a meta order. Records without metadata sort as size 0 /
    /// oldest. With `dirs` (see [`Index::folder_totals`]) a folder sorts by what it holds.
    fn meta_key(&self, key: MetaKey, i: u32, dirs: Option<&[u64]>) -> i64 {
        match key {
            MetaKey::Mtime => self.mtime(i).unwrap_or(i64::MIN),
            MetaKey::Size => match dirs {
                Some(t) if self.is_dir(i) => {
                    t.get(i as usize).copied().unwrap_or(0).min(i64::MAX as u64) as i64
                }
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
            return slots
                .iter()
                .map(|&i| (self.meta_key(key, i, dirs), i))
                .collect();
        }
        let view = self.order_view();
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
                let r = (view.rank(i as usize) as u64).min((1 << 31) - 1);
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

    /// `(key, slot)` pairs ([`Index::sort_keys`], [`Index::rank_keys`]) by key, biggest first.
    /// Chunks sort on all cores; the final stable sort only merges those runs.
    pub fn sort_pairs(mut pairs: Vec<(i64, u32)>) -> Vec<u32> {
        let cmp = |a: &(i64, u32), b: &(i64, u32)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
        let hw = std::thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(1);
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

    /// The first `k` of `(key, slot)` pairs by key, biggest first, without sorting the rest.
    pub fn top_pairs(mut pairs: Vec<(i64, u32)>, k: usize) -> Vec<u32> {
        let cmp = |a: &(i64, u32), b: &(i64, u32)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
        let k = k.min(pairs.len());
        if k < pairs.len() && k > 0 {
            pairs.select_nth_unstable_by(k - 1, cmp);
        }
        pairs.truncate(k);
        pairs.sort_unstable_by(cmp);
        pairs.into_iter().map(|(_, i)| i).collect()
    }

    /// Sort matches newest / largest first (ties by slot, so the order is total).
    pub fn sort_slots(&self, slots: Vec<u32>, key: MetaKey, dirs: Option<&[u64]>) -> Vec<u32> {
        Self::sort_pairs(self.sort_keys(&slots, key, dirs))
    }

    /// The first `k` rows of [`Index::sort_slots`] without sorting the rest.
    pub fn top_slots(
        &self,
        slots: &[u32],
        key: MetaKey,
        k: usize,
        dirs: Option<&[u64]>,
    ) -> Vec<u32> {
        Self::top_pairs(self.sort_keys(slots, key, dirs), k)
    }

    /// [`Self::search`] with [`Order::Relevance`], at the clock `now_ft` (FILETIME).
    pub fn search_ranked(
        &self,
        q: &Query,
        offset: usize,
        limit: usize,
        now_ft: i64,
    ) -> SearchResult {
        let slots = self.match_slots(q);
        let total = slots.len();
        let end = offset.saturating_add(limit).min(total);
        let pairs = self.rank_keys(q, &slots, now_ft);
        let mut items = if end < total {
            Self::top_pairs(pairs, end)
        } else {
            Self::sort_pairs(pairs)
        };
        items.drain(..offset.min(end));
        SearchResult { total, items }
    }

    /// Execute a query for one window. The name orders give name-sorted results (ties by
    /// path, `NameDesc` reversed); the meta orders give newest / largest first (or the exact
    /// reverse); [`Order::Relevance`] gives Best match first. The scan runs in storage order
    /// (sequential memory) over `available_parallelism` chunks of slots. Works on a dirty
    /// index (merges pending changes on the fly); call [`Index::refresh`] first for best latency.
    ///
    /// A caller that pages through one query should keep [`Index::name_matches`] /
    /// [`Index::sort_slots`] / [`Index::rank_keys`] instead: every further window is then free.
    pub fn search(&self, q: &Query, o: &SearchOptions) -> SearchResult {
        let (key, asc) = match o.order {
            Order::Relevance if q.ranks_by_name() => {
                return self.search_ranked(q, o.offset, o.limit, now_filetime());
            }
            // Relevance lands here with nothing typed to rank by (see `Query::ranks_by_name`).
            Order::NameAsc | Order::NameDesc | Order::Relevance => {
                let set = self.name_matches(q);
                let items = self.name_page(&set, o.offset, o.limit, o.order == Order::NameDesc);
                return SearchResult {
                    total: set.total(),
                    items,
                };
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
