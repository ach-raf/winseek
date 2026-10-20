//! Rust port of Flow Launcher's StringMatcher (dev branch, MIT — see THIRD_PARTY_NOTICES.md):
//! acronym match first, else an in-order per-token fuzzy match, with Flow's scoring formula.
//!
//! Divergences from Flow, both pinned by the task list's hand-worked numbers:
//! - acronym score rounds to nearest (`vs`/`Visual Studio Code` = 67; Flow truncates to 66),
//! - the word-start tie-break subtracts ALL spaces of the candidate (Flow only counts spaces
//!   seen while matching the first token — `bluetooth`/`Bluetooth & devices` = 178 needs 2).
//!
//! Char folding is 1:1, so folded indices stay valid for the original string (highlight ranges).

use super::item::LaunchItem;

/// Accept threshold, Flow's default QuerySearchPrecision.
pub const PRECISION: u32 = 50;

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub score: u32,
    /// (start, end) char ranges into the matched candidate string.
    pub ranges: Vec<(usize, usize)>,
}

/// Score one query against one candidate string: acronym pass first, then fuzzy.
pub fn score(query: &str, candidate: &str) -> Option<Match> {
    score_inner(query, candidate).map(|(m, _)| m)
}

/// Like `score`, but also reports whether an acronym match covered every group (title bonus).
fn score_inner(query: &str, candidate: &str) -> Option<(Match, bool)> {
    let query = query.trim();
    if query.is_empty() || candidate.is_empty() {
        return None;
    }
    let q: Vec<char> = fold(query).chars().collect();
    let orig: Vec<char> = candidate.chars().collect();
    let c: Vec<char> = fold(candidate).chars().collect();

    if let Some(m) = acronym(&q, &c, &orig) {
        return Some(m);
    }
    fuzzy(&q, &c)
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
    Some((Match { score: sc.max(0) as u32, ranges: ranges(index_list) }, false))
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
pub fn score_item(query: &str, item: &LaunchItem) -> Option<Match> {
    let mut best = score_inner(query, &item.title);
    for kw in &item.keywords {
        if let Some((mut m, _)) = score_inner(query, kw) {
            m.score *= 9; // keywords weigh 0.9
            m.score /= 10;
            m.ranges.clear(); // keyword hits must not highlight the title
            if best.as_ref().is_none_or(|b| m.score > b.0.score) {
                best = Some((m, false));
            }
        }
    }
    let (mut m, full_acronym) = best?;
    let fq = fold(query);
    let ft = fold(&item.title);
    if ft == fq {
        m.score += 50;
    } else if ft.starts_with(&fq) {
        m.score += 25;
    }
    if full_acronym {
        m.score += 60;
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
