//! Rust port of Flow Launcher's StringMatcher (dev branch, MIT — see THIRD_PARTY_NOTICES.md):
//! acronym match first, else an in-order per-token fuzzy match, with Flow's scoring formula.
//!
//! Divergences from Flow, both pinned by the task list's hand-worked numbers:
//! - acronym score rounds to nearest (`vs`/`Visual Studio Code` = 67; Flow truncates to 66),
//! - the word-start tie-break subtracts ALL spaces of the candidate (Flow only counts spaces
//!   seen while matching the first token — `bluetooth`/`Bluetooth & devices` = 178 needs 2).
//!
//! Char folding is 1:1, so folded indices stay valid for the original string (highlight ranges).

use super::item::{Kind, LaunchItem, Target};

/// Accept threshold, Flow's default QuerySearchPrecision.
pub const PRECISION: u32 = 50;

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub score: u32,
    /// (start, end) char ranges into the matched candidate string.
    pub ranges: Vec<(usize, usize)>,
}

/// Score one query against one candidate string: acronym pass first, then fuzzy.
/// The catalog search uses `score_item_with`; this standalone form backs the tests and the
/// task-list contract (worked scoring numbers are pinned against it).
#[allow(dead_code)]
pub fn score(query: &str, candidate: &str) -> Option<Match> {
    score_inner(query, candidate).map(|(m, _)| m)
}

/// A query folded once per keystroke — `search()` scores ~1500 items per call, so the query
/// must not be re-folded per item.
#[derive(Clone)]
pub struct Prepared {
    pub chars: Vec<char>,
}

pub fn prepare(query: &str) -> Prepared {
    Prepared { chars: query.trim().chars().map(fold_char).collect() }
}

/// Like `score`, but also reports whether an acronym match covered every group (title bonus).
fn score_inner(query: &str, candidate: &str) -> Option<(Match, bool)> {
    if query.trim().is_empty() || candidate.is_empty() {
        return None;
    }
    score_prepared(&prepare(query).chars, candidate)
}

fn score_prepared(q: &[char], candidate: &str) -> Option<(Match, bool)> {
    let mut scorer = Scorer::default();
    scorer.score(q, candidate)
}

/// Reusable per-candidate scratch so a catalog search does not allocate per item.
#[derive(Default)]
pub(crate) struct Scorer {
    orig: Vec<char>,
    folded: Vec<char>,
}

impl Scorer {
    pub(crate) fn score(&mut self, q: &[char], candidate: &str) -> Option<(Match, bool)> {
        if q.is_empty() || candidate.is_empty() {
            return None;
        }
        self.orig.clear();
        self.orig.extend(candidate.chars());
        self.folded.clear();
        self.folded.extend(self.orig.iter().map(|&ch| fold_char(ch)));
        // Prune before scoring: neither pass can match without the query's first char in the
        // candidate (fuzzy needs it to start the first token, acronym needs it on a group).
        let q0 = q[0];
        if !self.folded.contains(&q0) {
            return None;
        }
        if let Some(m) = acronym(q, &self.folded, &self.orig) {
            return Some(m);
        }
        fuzzy(q, &self.folded)
    }
}

/// Lowercase + strip Latin diacritics, one char in / one char out.
pub fn fold(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

fn fold_char(ch: char) -> char {
    if ch.is_ascii() {
        return ch.to_ascii_lowercase();
    }
    match ch {
        'A' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' | 'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' | 'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'Ď' | 'ď' => 'd',
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' | 'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' | 'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'Ĥ' | 'ĥ' => 'h',
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' | 'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' => 'i',
        'Ĵ' | 'ĵ' => 'j',
        'Ķ' | 'ķ' => 'k',
        'Ĺ' | 'Ļ' | 'Ľ' | 'Ŀ' | 'Ł' | 'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' | 'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ŏ' | 'Ő' | 'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'Ŕ' | 'Ŗ' | 'Ř' | 'ŕ' | 'ŗ' | 'ř' => 'r',
        'Ś' | 'Ŝ' | 'Ş' | 'Š' | 'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'Ţ' | 'Ť' | 'Ŧ' | 'ţ' | 'ť' | 'ŧ' => 't',
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' | 'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'Ŵ' | 'ŵ' => 'w',
        'Ý' | 'Ÿ' | 'Ŷ' | 'ý' | 'ÿ' | 'ŷ' => 'y',
        'Ź' | 'Ż' | 'Ž' | 'ź' | 'ż' | 'ž' => 'z',
        _ => ch,
    }
}

/// Acronym positions on the ORIGINAL candidate: uppercase, index 0, after a space, or a digit
/// (a digit run counts once). `-`, `_`, `.` also start a group (small improvement over Flow).
fn is_acronym_char(s: &[char], i: usize) -> bool {
    s[i].is_uppercase()
        || i == 0
        || s[i - 1].is_whitespace()
        || s[i - 1] == '-'
        || s[i - 1] == '_'
        || s[i - 1] == '.'
}

fn is_acronym_number(s: &[char], i: usize) -> bool {
    s[i].is_ascii_digit()
}

fn is_acronym(s: &[char], i: usize) -> bool {
    is_acronym_char(s, i) || is_acronym_number(s, i)
}

/// Total-group counting: like `is_acronym` but a digit run only counts at its first digit.
fn is_acronym_count(s: &[char], i: usize) -> bool {
    if is_acronym_char(s, i) {
        return true;
    }
    is_acronym_number(s, i) && (i == 0 || !is_acronym_number(s, i - 1))
}

/// All query chars must match distinct acronym groups in order; score = matched groups of all.
fn acronym(q: &[char], c: &[char], orig: &[char]) -> Option<(Match, bool)> {
    let mut acr_q = 0usize;
    let mut matched: Vec<usize> = Vec::new();
    let mut total = 0u32;
    for (i, ch) in c.iter().enumerate() {
        if acr_q >= q.len() {
            // Query exhausted: keep counting the remaining groups for the score.
            if is_acronym_count(orig, i) {
                total += 1;
            }
            continue;
        }
        if is_acronym(orig, i) && *ch == q[acr_q] {
            matched.push(i);
            acr_q += 1;
        }
        if is_acronym_count(orig, i) {
            total += 1;
        }
    }
    if matched.len() != q.len() || matched.is_empty() {
        return None;
    }
    let groups = count_distinct_groups(&matched, orig);
    if total == 0 {
        return None;
    }
    let full = groups == total;
    let score = (groups * 200 + total) / (total * 2); // round to nearest (see module doc)
    (score >= PRECISION).then(|| (Match { score, ranges: ranges(matched) }, full))
}

/// Groups among matched indices: each non-digit index is one group; a digit run is one group.
fn count_distinct_groups(matched: &[usize], orig: &[char]) -> u32 {
    let mut groups = 0u32;
    let mut seen = std::collections::HashSet::new();
    for &i in matched {
        if !seen.insert(i) {
            continue;
        }
        groups += 1;
        if !is_acronym_number(orig, i) {
            continue;
        }
        let mut end = i;
        while end + 1 < orig.len() && is_acronym_number(orig, end + 1) {
            end += 1;
            seen.insert(end);
        }
    }
    groups
}

/// Flow's FuzzyMatching: split the query on spaces; every token must match IN ORDER (each token
/// is an in-order char scan over the candidate, greedy re-anchor when a contiguous run exists).
fn fuzzy(q: &[char], c: &[char]) -> Option<(Match, bool)> {
    let subs: Vec<&[char]> = q.split(|&ch| ch == ' ').filter(|s| !s.is_empty()).collect();
    if subs.len() == 1 {
        return single_token(subs[0], c);
    }
    let mut sub = subs[0];
    let mut sub_i = 0usize;
    let mut sub_ci = 0usize;
    let mut first_match: Option<usize> = None;
    let mut first_in_word = 0usize;
    let mut last_match = 0usize;
    let mut all_matched = false;
    let mut prev_matched = false;
    let mut all_contained = true;
    let mut index_list: Vec<usize> = Vec::new();

    for (i, ch) in c.iter().enumerate() {
        if all_matched || *ch != sub[sub_ci] {
            prev_matched = false;
            continue;
        }
        if first_match.is_none() {
            first_match = Some(i);
        }
        if sub_ci == 0 {
            // First letter of the current token matched: a fresh, better anchor.
            prev_matched = true;
            first_in_word = i;
        } else if !prev_matched {
            // Verify the token is contiguous right up to here, then re-anchor onto that run.
            let start = i - sub_ci;
            if (0..sub_ci).all(|k| c[start + k] == sub[k]) {
                prev_matched = true;
                if sub_i == 0 {
                    first_match = Some(start);
                }
                let kept: Vec<usize> = index_list.iter().copied().filter(|&x| x < first_in_word).collect();
                index_list = kept;
                index_list.extend(start..start + sub_ci);
            }
        }
        last_match = i + 1;
        index_list.push(i);
        sub_ci += 1;
        if sub_ci == sub.len() {
            all_contained = prev_matched && all_contained;
            sub_i += 1;
            all_matched = sub_i >= subs.len();
            if all_matched {
                continue;
            }
            sub = subs[sub_i];
            sub_ci = 0;
        }
    }
    if !all_matched {
        return None;
    }

    let fm = first_match.unwrap_or(0);
    // A multi-token query must still anchor on a word start: "of nat" matching mid-word junk
    // is how a loose in-order scan buries real hits.
    if !is_word_start(c, fm) {
        return None;
    }
    Some((finish(q, c, fm, last_match, all_contained, index_list), false))
}

fn is_word_start(c: &[char], i: usize) -> bool {
    i == 0 || !c[i - 1].is_alphanumeric()
}

/// Single-token query (the overwhelmingly common case). Unlike a loose in-order scan this only
/// accepts what a human would call a match, tried best-first:
/// 1. the query as a contiguous substring (a word-start occurrence wins over a mid-word one;
///    mid-word needs >= 3 chars, so `at` does not hit every "Format");
/// 2. otherwise a typo/abbreviation match: >= 3 chars, anchored on a WORD START, inside one word
///    run (no spaces crossed) with at most `len/4` (min 1) skipped letters — `chrm` -> Chrome,
///    `firefx` -> Firefox, but `atos` NOT -> "Rise of Nations".
fn single_token(q: &[char], c: &[char]) -> Option<(Match, bool)> {
    let n = q.len();
    let mut sub: Option<usize> = None;
    if c.len() >= n {
        for i in 0..=c.len() - n {
            if c[i..i + n] == *q {
                if is_word_start(c, i) {
                    sub = Some(i);
                    break;
                }
                sub.get_or_insert(i);
            }
        }
    }
    if let Some(s) = sub.filter(|&s| is_word_start(c, s) || n >= 3) {
        return Some((finish(q, c, s, s + n, true, (s..s + n).collect()), false));
    }
    if n < 3 {
        return None;
    }
    // Swapped neighbours ("chrmoe" -> Chrome): a word-start hit on the corrected query, a bit
    // below a clean match.
    if n >= 4 {
        for i in 0..n - 1 {
            if q[i] == q[i + 1] {
                continue;
            }
            let mut fixed = q.to_vec();
            fixed.swap(i, i + 1);
            if let Some(s) = (0..=c.len().saturating_sub(n)).find(|&s| c.len() >= n && c[s..s + n] == fixed[..] && is_word_start(c, s)) {
                let mut m = finish(&fixed, c, s, s + n, true, (s..s + n).collect());
                m.score = m.score.saturating_sub(15);
                return Some((m, false));
            }
        }
    }
    let mut best: Option<Vec<usize>> = None;
    for i in 0..c.len() {
        if c[i] != q[0] || !is_word_start(c, i) {
            continue;
        }
        let mut idx = vec![i];
        let mut qi = 1;
        let mut j = i + 1;
        while qi < n && j < c.len() {
            if c[j] == q[qi] {
                idx.push(j);
                qi += 1;
            }
            j += 1;
        }
        if qi < n {
            continue;
        }
        if best.as_ref().is_none_or(|b| idx[n - 1] - i < b[n - 1] - b[0]) {
            best = Some(idx);
        }
    }
    let idx = best?;
    let (fm, last) = (idx[0], idx[n - 1] + 1);
    if c[fm..last].iter().any(|ch| ch.is_whitespace()) || last - fm - n > (n / 4).max(1) {
        return None;
    }
    Some((finish(q, c, fm, last, false, idx), false))
}

/// Flow's scoring formula over a finished match.
fn finish(q: &[char], c: &[char], fm: usize, last_match: usize, all_contained: bool, index_list: Vec<usize>) -> Match {
    // Measure from the word that contains the match, not the string start: 'world' in
    // 'hello world' scores like a match at index 0 (Flow's nearest-space adjustment).
    let first_rel = fm - space_before(c, fm);
    let match_len = last_match - fm;
    let mut sc = (100 * (q.len() + 1)) as i32 / ((1 + first_rel) + (match_len + 1)) as i32;
    if first_rel == 0 && all_contained {
        sc -= c.iter().filter(|&&ch| ch == ' ').count() as i32;
    }
    let diff = c.len() as i32 - q.len() as i32;
    if diff < 5 {
        sc += 20;
    } else if diff < 10 {
        sc += 10;
    }
    if all_contained {
        let count = q.iter().filter(|&&ch| ch != ' ').count();
        const THRESHOLD: usize = 4;
        if count <= THRESHOLD {
            sc += (count * 10) as i32;
        } else {
            sc += (THRESHOLD * 10 + (count - THRESHOLD) * 5) as i32;
        }
    }
    Match { score: sc.max(0) as u32, ranges: ranges(index_list) }
}

/// Index just after the last space at or before `at` (0 when none) — Flow's closest-space shift.
fn space_before(c: &[char], at: usize) -> usize {
    (0..at).rev().find(|&i| c[i] == ' ').map_or(0, |i| i + 1)
}

/// Sorted, deduped char indices -> merged (start, end) ranges.
fn ranges(mut idx: Vec<usize>) -> Vec<(usize, usize)> {
    idx.sort_unstable();
    idx.dedup();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for i in idx {
        match out.last_mut() {
            Some(r) if r.1 == i => r.1 = i + 1,
            _ => out.push((i, i + 1)),
        }
    }
    out
}

/// Best of title (x1.0) and keywords (x0.9, ranges cleared), plus the title bonuses the
/// Top-Hit threshold depends on: exact +50, title prefix +25, full-title acronym +60.
/// Task-list tests pin their worked numbers against this entry point.
#[allow(dead_code)]
pub fn score_item(query: &str, item: &LaunchItem) -> Option<Match> {
    score_item_prepared(&prepare(query), item)
}

/// The hot path from `search`: same scoring, query folded once for the whole catalog.
#[allow(dead_code)]
pub fn score_item_prepared(q: &Prepared, item: &LaunchItem) -> Option<Match> {
    score_item_with(&mut Scorer::default(), q, item)
}

/// `search` passes one shared Scorer through the whole catalog (no per-item allocation).
pub(crate) fn score_item_with(s: &mut Scorer, q: &Prepared, item: &LaunchItem) -> Option<Match> {
    let mut best = s.score(&q.chars, &item.title);
    for kw in &item.keywords {
        if let Some((mut m, _)) = s.score(&q.chars, kw) {
            m.score *= 9; // keywords weigh 0.9
            m.score /= 10;
            m.ranges.clear(); // keyword hits must not highlight the title
            if best.as_ref().is_none_or(|b| m.score > b.0.score) {
                best = Some((m, false));
            }
        }
    }
    let (mut m, full_acronym) = best?;
    let fq = &q.chars;
    // An alias / exe name typed in full (`code`, `wt`, `ps`) is as good as the exact title.
    if m.ranges.is_empty()
        && item.keywords.iter().any(|kw| kw.chars().count() == fq.len() && kw.chars().map(fold_char).eq(fq.iter().copied()))
    {
        m.score += 40;
    }
    let ft: Vec<char> = item.title.chars().map(fold_char).collect();
    if ft == *fq {
        m.score += 50;
    } else if ft.starts_with(fq.as_slice()) {
        m.score += 25;
    }
    if full_acronym {
        m.score += 60;
    }
    // Legacy Control Panel / Task pages rank below the modern ms-settings page for the same
    // query: the Settings page is the destination, the .cpl is the fallback row (PRD §1).
    if item.kind == Kind::Setting && matches!(item.target, Target::Exec { .. }) {
        m.score = m.score.saturating_sub(15);
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::item::{Kind, Target};

    fn item(title: &str, keywords: &[&str]) -> LaunchItem {
        LaunchItem {
            id: format!("app:{title}"),
            kind: Kind::App,
            title: title.into(),
            subtitle: "test".into(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            target: Target::Path { path: format!("C:\\{title}.exe"), args: None },
            icon: None,
            admin_ok: true,
        }
    }

    #[test]
    fn flow_worked_example_wor() {
        // hello world: base 80 (400/5), word-start penalty -1, length diff 8 -> +10, query +30.
        assert_eq!(score("wor", "hello world").unwrap().score, 119);
    }

    #[test]
    fn acronym_matches() {
        let m = score("vsc", "Visual Studio Code").unwrap();
        assert_eq!(m.score, 100);
        assert_eq!(m.ranges, vec![(0, 1), (7, 8), (14, 15)]);
        assert_eq!(score("vs2", "Visual Studio 2019").unwrap().score, 100);
        // 2 of 3 groups = 66.7 rounds to 67.
        assert_eq!(score("vs", "Visual Studio Code").unwrap().score, 67);
    }

    #[test]
    fn worked_top_hit_numbers() {
        let mpv = item("mpv", &[]);
        assert_eq!(score_item("mpv", &mpv).unwrap().score, 180); // 80 + 20 + 30 + 50

        let bt = item("Bluetooth & devices", &["devices", "bt"]);
        assert_eq!(score_item("bluetooth", &bt).unwrap().score, 178); // 90 - 2 + 0 + 65 + 25

        let vsc = item("Visual Studio Code", &["code"]);
        assert_eq!(score_item("vsc", &vsc).unwrap().score, 160); // 100 + 60

        // Partial acronym, no bonus -> far below the Top Hit threshold.
        assert_eq!(score_item("vs", &vsc).unwrap().score, 67);
    }

    #[test]
    fn mpv_ordering() {
        let exact = item("mpv", &[]);
        let dotnet = item("mpv.net", &[]);
        let other = item("SMPlayer", &[]);
        let a = score_item("mpv", &exact).unwrap().score;
        let b = score_item("mpv", &dotnet).unwrap().score;
        assert!(a > b);
        assert!(score_item("mpv", &other).is_none());
    }

    #[test]
    fn blue_matches_bluetooth() {
        assert!(score("blue", "Bluetooth").is_some());
    }

    #[test]
    fn keyword_match_has_cleared_ranges() {
        let vsc = item("Visual Studio Code", &["code"]);
        let m = score_item("code", &vsc).unwrap();
        assert!(m.score > 0);
        assert!(m.ranges.is_empty()); // came from the keyword, not the title
    }

    #[test]
    fn tokens_must_match_in_order() {
        assert!(score("studio visual", "Visual Studio Code").is_none());
        assert!(score("vi co", "Visual Studio Code").is_some());
    }

    #[test]
    fn scattered_subsequences_are_not_matches() {
        // The "atos" bug: a loose in-order scan hit all of these.
        for junk in ["Rise of Nations", "AtomicParsley Documentation", "Adobe Photoshop 2025", "Macro Buttons", "Change font settings"] {
            assert!(score("atos", junk).is_none(), "{junk}");
        }
        assert!(score("atos", "MyAtos Portal").is_some());
        assert!(score("shop", "Adobe Photoshop 2025").is_some()); // mid-word substring, >= 3 chars
        assert!(score("ph", "Adobe Photoshop").is_some()); // word-start prefix
        assert!(score("ot", "Photoshop").is_none()); // 2-char mid-word is noise
    }

    #[test]
    fn typos_and_abbreviations_still_match() {
        assert!(score("chrm", "Google Chrome").is_some());
        assert!(score("firefx", "Firefox").is_some());
        assert!(score("chrome", "Google Chrome").unwrap().score > score("chrm", "Google Chrome").unwrap().score);
    }

    #[test]
    fn swapped_letters_and_exact_aliases() {
        assert!(score("chrmoe", "Google Chrome").is_some());
        let ps = item("Adobe Photoshop 2025", &["Photoshop", "ps"]);
        let plain = item("Psi Messenger", &[]);
        assert!(score_item("ps", &ps).unwrap().score > score_item("ps", &plain).unwrap().score);
    }

    #[test]
    fn word_start_beats_mid_word() {
        let a = score("studio", "Visual Studio Code").unwrap().score;
        let b = score("studio", "Photostudio").unwrap().score;
        assert!(a > b);
    }

    #[test]
    fn fold_strips_diacritics() {
        assert!(score("café", "cafe").is_some());
        assert!(score("cafe", "Café").is_some());
        assert_eq!(fold("Café"), "cafe");
    }

    #[test]
    fn empty_query_is_no_match() {
        assert!(score("", "hello").is_none());
        assert!(score("   ", "hello").is_none());
        assert!(score("x", "").is_none());
        assert!(score_item("", &item("hello", &[])).is_none());
    }
}
