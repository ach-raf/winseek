//! Ranking for `code:` searches: two kinds of evidence, one score.
//!
//! A code hit stands out by *meaning* (its best stretch's z among the code vectors, the
//! [`crate::relevance`] scale) and by *words* (how much of the query the file contains). DigUp's
//! measured shape (Searcher.swift, the 2026-10 real-code and synthetic evals): candidates are the
//! files with `z >= Z_MIN` **or** at least half the query's words; the score is
//! `weight_mean * max(z, 0) + weight_words * coverage^1.5`, with the words weight at
//! [`CODE_WORD_WEIGHT`] — words are everywhere in code, so they count for less than in general
//! search. A query that *looks up* something exact (an identifier, an invoice-like token) halves
//! the meaning weight and doubles the words weight, so the literal hit leads.
//!
//! An identifier-shaped query (`quantize_row_q8_0_ref`, `CodeExtractor.lines`) is searched as one
//! quoted phrase, so it matches only side by side, as written — FTS5 tokenizes `_` away, and the
//! phrase preserves the order.

/// The words weight for code (DigUp `codeWordWeight`; 1.5 measured no different, 0.5 lost the top 5).
pub const CODE_WORD_WEIGHT: f32 = 1.0;
/// A file qualifies by word coverage alone when it contains at least this share of the query's words.
pub const COVERAGE_CANDIDATE: f32 = 0.5;

/// Is `c` an identifier character (ASCII letters, digits, `_`, `$)?
fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// The name a query looks up, as its words (`"quantize_row_q8_0_ref"` -> `["quantize", "row",
/// "q8", "0", "ref"]`), when the whole query is one: snake_case, camelCase, dotted, `::`, `->`,
/// `#`, or with digits. `None` for words anyone would say ("retry backoff"), which are searched
/// as words and by meaning. Port of DigUp `CodeLookup.identifier`.
pub fn identifier(query: &str) -> Option<Vec<String>> {
    let name = query.trim();
    let name = name.strip_suffix("()").unwrap_or(name);
    if name.is_empty() {
        return None;
    }
    // The shape: ident words joined by `.`, `::`, `->` or `#`, nothing else.
    let b = name.as_bytes();
    let mut i = 0usize;
    let mut word_start = true;
    let mut has_sep = false;
    while i < b.len() {
        let c = name[i..].chars().next().unwrap();
        let group = if i + 1 < b.len()
            && ((c == ':' && b[i + 1] == b':') || (c == '-' && b[i + 1] == b'>'))
        {
            2
        } else if c == '.' || c == '#' {
            1
        } else {
            0
        };
        if group > 0 {
            if word_start {
                return None; // a separator must follow a word ("a..b", ".a")
            }
            has_sep = true;
            word_start = true;
            i += group;
            continue;
        }
        if !ident_char(c) {
            return None;
        }
        // A word starts with a letter, `_` or `$` — never a digit.
        if word_start && !c.is_ascii_alphabetic() && c != '_' && c != '$' {
            return None;
        }
        word_start = false;
        i += c.len_utf8();
    }
    if word_start {
        return None; // trailing separator
    }
    let camel =
        name.chars().skip(1).any(char::is_uppercase) && name.chars().any(char::is_lowercase);
    // DigUp counts `_` and `$` inside a word as name marks too, not just the joining separators.
    let marked = has_sep
        || name
            .chars()
            .any(|c| matches!(c, '_' | '$' | '.' | ':' | '>' | '#'));
    let digit = name.chars().any(|c| c.is_ascii_digit());
    if !marked && !camel && !digit {
        return None;
    }
    Some(
        name.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// The content words of a query as keyword search keeps text: runs of letters / digits, two
/// characters or more, lowercased, in order, without repeats.
pub fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
    {
        if !out.iter().any(|t| t == w) {
            out.push(w.to_owned());
        }
    }
    out
}

/// The FTS5 pattern for an identifier's words: one quoted phrase, so they match only side by side.
pub fn phrase(words: &[String]) -> String {
    format!("\"{}\"", words.join(" "))
}

/// FTS5 patterns for the query's words: quoted. A word of three characters or more that is in no
/// code file as typed also matches as the start of longer words (`"grok"*` finds `Grok4`), so a
/// file shown while typing does not drop out once the word is complete. `indexed` says whether a
/// pattern matches anything.
pub fn word_patterns(terms: &[String], indexed: &dyn Fn(&str) -> bool) -> Vec<String> {
    terms
        .iter()
        .map(|t| {
            let p = format!("\"{t}\"");
            if t.chars().count() >= 3 && !indexed(&p) {
                format!("{p}*")
            } else {
                p
            }
        })
        .collect()
}

/// A query that looks up something exact: every word is a token with digits and (letters or four
/// characters) — an invoice number, an error code. Port of DigUp `isLookup`.
pub fn is_lookup(terms: &[String]) -> bool {
    !terms.is_empty()
        && terms.iter().all(|t| {
            t.chars().any(|c| c.is_ascii_digit())
                && (t.chars().any(|c| c.is_ascii_alphabetic()) || t.chars().count() >= 4)
        })
}

/// One code file's final score: how far it stands out in meaning (its best stretch's z, floored at
/// 0 so a below-gate file is not pulled under) plus the words bonus that grows with coverage.
/// A lookup query counts the words double and meaning half.
pub fn score(z: Option<f32>, coverage: f32, lookup: bool) -> f32 {
    let (wm, ww) = if lookup {
        (0.5, CODE_WORD_WEIGHT * 2.0)
    } else {
        (1.0, CODE_WORD_WEIGHT)
    };
    wm * z.unwrap_or(0.0).max(0.0) + ww * coverage.powf(1.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_reads_names_not_sentences() {
        let words = |q: &str| identifier(q).map(|w| w.join(" "));
        assert_eq!(
            words("quantize_row_q8_0_ref").as_deref(),
            Some("quantize row q8 0 ref")
        );
        assert_eq!(
            words("retryWithBackoff").as_deref(),
            Some("retrywithbackoff")
        );
        assert_eq!(
            words("CodeExtractor.lines").as_deref(),
            Some("codeextractor lines")
        );
        assert_eq!(words("std::vec::Vec").as_deref(), Some("std vec vec"));
        assert_eq!(
            words("useLocalStorage()").as_deref(),
            Some("uselocalstorage")
        );
        assert_eq!(
            words("v1_2").as_deref(),
            Some("v1 2"),
            "digits qualify a bare word"
        );
        assert_eq!(
            identifier("retry backoff"),
            None,
            "plain words are not a name"
        );
        assert_eq!(identifier("Renamer"), None);
        assert_eq!(identifier("a..b"), None);
        assert_eq!(identifier("fn main"), None, "two words, no name shape");
        assert_eq!(identifier("42"), None, "no letter, no name");
    }

    #[test]
    fn terms_are_the_content_words() {
        assert_eq!(terms("Retry with Backoff!"), ["retry", "with", "backoff"]);
        assert_eq!(
            terms("a I"),
            Vec::<String>::new(),
            "one-letter words are noise"
        );
        assert_eq!(terms("code code"), ["code"], "no repeats");
    }

    #[test]
    fn identifier_phrase_matches_side_by_side() {
        assert_eq!(
            phrase(&identifier("retry_with_backoff").unwrap()),
            "\"retry with backoff\""
        );
    }

    #[test]
    fn words_not_indexed_as_typed_become_prefixes() {
        let terms = terms("quantize grok cat");
        let indexed = |p: &str| p.contains("quantize") || p.contains("cat");
        assert_eq!(
            word_patterns(&terms, &indexed),
            ["\"quantize\"", "\"grok\"*", "\"cat\""],
            "grok matches as a prefix only"
        );
        let none = |_: &str| true;
        assert_eq!(word_patterns(&terms, &none).len(), 3);
    }

    #[test]
    fn lookups_are_digit_tokens() {
        // Every word needs a digit of its own, so a dashed number is three words and no lookup —
        // its words still match as three quoted patterns.
        assert!(!is_lookup(&terms("INV-2024-00817")));
        assert!(is_lookup(&terms("0x80070005")));
        assert!(is_lookup(&terms("KB5034441")));
        assert!(
            is_lookup(&terms("2024")),
            "a word of four digits is exact enough"
        );
        assert!(!is_lookup(&terms("42")), "a short bare number is not");
        assert!(!is_lookup(&terms("retry with backoff")));
        assert!(!is_lookup(&[]));
    }

    #[test]
    fn scoring_follows_the_measured_shape() {
        // Meaning alone: a z of 4 scores 4.
        assert!((score(Some(4.0), 0.0, false) - 4.0).abs() < 1e-5);
        // Words alone: full coverage scores the words weight.
        assert!((score(None, 1.0, false) - CODE_WORD_WEIGHT).abs() < 1e-5);
        // Half coverage grows less than double the coverage (the ^1.5 curve).
        assert!(score(None, 0.5, false) * 2.0 < score(None, 1.0, false) * 1.01);
        // A lookup: meaning halves (z 4 -> 2) and the words weigh double (2.0 at full coverage):
        // z 4 + full coverage scores the same as before, z alone drops under a full-coverage file.
        assert!((score(Some(4.0), 1.0, true) - 4.0).abs() < 1e-5);
        assert!(
            (score(Some(4.0), 0.0, true) - 2.0).abs() < 1e-5,
            "meaning halves"
        );
        assert!(
            score(Some(5.0), 0.0, true) > score(None, 1.0, true),
            "a strong meaning match still beats full word coverage"
        );
        // Below-gate meaning does not drag the score under the words bonus.
        assert!(score(Some(-1.0), 1.0, false) > 0.0);
    }
}
