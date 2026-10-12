//! Query parsing and parallel execution.
//!
//! Syntax (terms are ANDed):
//! * whitespace separates terms; `"quoted phrase"` is one term
//! * `!term` negates; `!"a b"` too
//! * `*` / `?` in a term: whole-name wildcard match (whole *path* if the term has `\`)
//! * a term containing `\` (or `/`) is matched against the full path (substring)
//! * `ext:jpg;png` (also `,`), `file:`, `folder:` (alone = filter, `folder:foo` = filter + term)
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

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub offset: usize,
    pub limit: usize,
    /// Reverse the (name) order, Everything's flip-pair: iterate the same array backwards.
    pub descending: bool,
}
impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions { offset: 0, limit: usize::MAX, descending: false }
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
                if matches!(w.as_str(), "ext" | "file" | "folder") {
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
            Cond::File | Cond::Folder => 0,
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

    /// Scan storage slots `lo..hi` sequentially; returns the sort ranks of the matches.
    fn scan(&self, q: &Query, rank: &[u32], lo: usize, hi: usize) -> Vec<u32> {
        let mut sc = Scratch::default();
        let mut out = Vec::new();
        for (i, &r) in rank.iter().enumerate().take(hi).skip(lo) {
            if r != NONE && q.terms.iter().all(|t| self.eval(t, i as u32, &mut sc)) {
                out.push(r);
            }
        }
        out
    }

    /// Execute a query. Results are in name order (ties by path); `descending` reverses
    /// the same order. The scan runs in storage order (sequential memory) over
    /// `available_parallelism` chunks of slots and yields each hit's rank in the sorted
    /// order array; only the requested window is selected/sorted (`select_nth_unstable`).
    /// Works on a dirty index (merges pending changes on the fly); call
    /// [`Index::refresh`] first for best latency.
    pub fn search(&self, q: &Query, o: &SearchOptions) -> SearchResult {
        let order = self.clean_order();
        let rank = if self.dirty { std::borrow::Cow::Owned(self.compute_rank(&order)) } else { std::borrow::Cow::Borrowed(&self.rank[..]) };
        let slots = rank.len();
        let mut ranks: Vec<u32> = if q.terms.is_empty() {
            (0..order.len() as u32).collect()
        } else {
            let hw = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(1);
            const MIN_CHUNK: usize = 8192;
            let nt = hw.min(slots / MIN_CHUNK).max(1);
            if nt == 1 {
                self.scan(q, &rank, 0, slots)
            } else {
                let chunk = slots.div_ceil(nt);
                let rank = &rank[..];
                let parts: Vec<Vec<u32>> = std::thread::scope(|s| {
                    let hs: Vec<_> = (0..nt)
                        .map(|k| s.spawn(move || self.scan(q, rank, (k * chunk).min(slots), ((k + 1) * chunk).min(slots))))
                        .collect();
                    hs.into_iter().map(|h| h.join().expect("scan thread")).collect()
                });
                let mut all = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
                for p in parts {
                    all.extend(p);
                }
                all
            }
        };
        let total = ranks.len();
        let k = o.offset.saturating_add(o.limit).min(total);
        let window: &[u32] = if o.descending {
            // largest `k` ranks, sorted descending
            let k = k.min(total);
            if k < total {
                ranks.select_nth_unstable(total - k);
            }
            ranks[total - k..].sort_unstable_by(|a, b| b.cmp(a));
            &ranks[total - k..]
        } else {
            if k < total && k > 0 {
                ranks.select_nth_unstable(k - 1);
            }
            ranks[..k].sort_unstable();
            &ranks[..k]
        };
        let start = o.offset.min(window.len());
        let items = window[start..].iter().map(|&r| order[r as usize]).collect();
        SearchResult { total, items }
    }

    pub fn search_str(&self, q: &str, o: &SearchOptions) -> SearchResult {
        self.search(&Query::parse(q), o)
    }
}
