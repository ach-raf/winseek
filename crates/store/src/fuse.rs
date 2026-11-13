//! Hybrid ranking: exact-word hits (FTS5 / bm25) fused into the semantic list.
//!
//! The semantic list is already calibrated and gated ([`crate::relevance`]: `Z_MIN`, `ADJ_MIN`).
//! Those gates are about *meaning*; a literal match is a different kind of evidence and must not
//! be judged by them: an invoice number, an error code or a serial has no "meaning" for the
//! embedding model, so its vector can sit far below the gate while the text contains it verbatim.
//!
//! Weighted reciprocal-rank fusion over the two ranked lists, per file:
//! `score = 1/(K + sem_rank) + w_text/(K + text_rank)`, with `w_text` growing with how
//! "exact-like" the query is ([`exact_lean`]):
//!
//! - plain words (`lean` 0): `w_text` = [`W_TEXT_MIN`]. A file that the text search also found
//!   climbs a little; a text-only hit stays behind the semantic list, as before this module.
//! - code-like tokens (`lean` 1: `INV-2024-00817`, `0x80070005`, `ERR_FILE_NOT_FOUND`):
//!   `w_text` = [`W_TEXT_MAX`], so every literal hit leads the list and the semantic list follows.

/// RRF constant (the usual 60: a flat tail, no single list dominates the top by rank alone).
const K: f32 = 60.0;
/// Text weight for a query without code-like tokens.
pub const W_TEXT_MIN: f32 = 0.2;
/// Text weight for a query made of code-like tokens only.
pub const W_TEXT_MAX: f32 = 3.0;

/// Does this (already de-quoted) token look like something only a literal match can find?
/// Digits (numbers, ids, dates, hex), `_` joined identifiers, and `-`/`.`/`/` joined upper-case
/// codes. A bare year or short number is not enough on its own ("2024"), but any id with a
/// letter or separator next to its digits is.
fn code_like(tok: &str) -> bool {
    let digits = tok.chars().filter(char::is_ascii_digit).count();
    let letters = tok.chars().filter(|c| c.is_alphabetic()).count();
    let joined = tok.chars().any(|c| matches!(c, '_' | '-' | '/' | '\\' | '.' | ':' | '#'));
    if digits >= 1 && (letters >= 1 || joined) {
        return true; // INV-2024-00817, 0x80070005, 12.04.2023, A4
    }
    if digits >= 5 {
        return true; // 00817, 20240331, 4111111111
    }
    let upper = tok.chars().filter(|c| c.is_uppercase()).count();
    (tok.contains('_') && letters >= 3) || (joined && upper >= 3 && upper == letters && letters >= 3)
}

/// 0..=1: the share of query tokens that look like codes or numbers. 0 for plain words.
pub fn exact_lean(query: &str) -> f32 {
    let toks: Vec<&str> = query
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '?' | '!')))
        .filter(|t| !t.is_empty())
        .collect();
    if toks.is_empty() {
        return 0.0;
    }
    toks.iter().filter(|t| code_like(t)).count() as f32 / toks.len() as f32
}

/// Fuse two ranked lists of file keys (best first) into one order. A key in both lists is
/// scored by both. `lean` is [`exact_lean`] of the query. Ties keep the semantic order.
pub fn fuse_order(sem: &[String], text: &[String], lean: f32) -> Vec<String> {
    let w = W_TEXT_MIN + (W_TEXT_MAX - W_TEXT_MIN) * lean.clamp(0.0, 1.0);
    let mut score: std::collections::HashMap<&str, (f32, usize)> = std::collections::HashMap::new();
    let mut first_seen = 0usize;
    for (i, k) in sem.iter().enumerate() {
        score.entry(k).or_insert_with(|| {
            first_seen += 1;
            (0.0, first_seen)
        }).0 += 1.0 / (K + 1.0 + i as f32);
    }
    for (i, k) in text.iter().enumerate() {
        score.entry(k).or_insert_with(|| {
            first_seen += 1;
            (0.0, first_seen)
        }).0 += w / (K + 1.0 + i as f32);
    }
    let mut v: Vec<(&str, f32, usize)> = score.into_iter().map(|(k, (s, o))| (k, s, o)).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    v.into_iter().map(|(k, _, _)| k.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn lean_reads_codes_and_numbers_not_words() {
        for q in ["INV-2024-00817", "0x80070005", "ERR_FILE_NOT_FOUND", "\"00817\"", "12.04.2023", "KB5034441"] {
            assert_eq!(exact_lean(q), 1.0, "{q}");
        }
        for q in ["grocery shopping list", "chocolate cake recipe", "a red circle", "2024", "café", ""] {
            assert_eq!(exact_lean(q), 0.0, "{q}");
        }
        assert_eq!(exact_lean("invoice INV-2024-00817"), 0.5);
    }

    #[test]
    fn plain_words_keep_the_semantic_order_and_the_text_tail() {
        // Semantic list untouched by text-only hits: they come after, as before fusion.
        let out = fuse_order(&s(&["a", "b", "c"]), &s(&["x", "y"]), 0.0);
        assert_eq!(out, s(&["a", "b", "c", "x", "y"]));
        // No text hits at all: identical order.
        assert_eq!(fuse_order(&s(&["a", "b", "c"]), &[], 0.0), s(&["a", "b", "c"]));
    }

    #[test]
    fn a_file_found_by_both_climbs() {
        let out = fuse_order(&s(&["a", "b", "c", "d"]), &s(&["d"]), 0.0);
        assert_eq!(out[0], "d", "semantic rank 4 + exact hit beats rank 1 alone");
        assert_eq!(out.len(), 4, "no duplicates");
    }

    #[test]
    fn a_code_query_puts_the_literal_hit_first_even_when_meaning_missed_it() {
        // The invoice has no semantic standing (not in the gated list at all).
        let out = fuse_order(&s(&["a", "b", "c"]), &s(&["inv"]), exact_lean("INV-2024-00817"));
        assert_eq!(out, s(&["inv", "a", "b", "c"]));
        // Hit in both lists: still first.
        let out = fuse_order(&s(&["a", "b", "inv"]), &s(&["inv"]), 1.0);
        assert_eq!(out[0], "inv");
    }

    #[test]
    fn mixed_queries_lean_halfway() {
        let lean = exact_lean("invoice INV-2024-00817");
        let out = fuse_order(&s(&["a", "b", "c"]), &s(&["inv"]), lean);
        assert_eq!(out[0], "inv");
        let out = fuse_order(&s(&["a", "b", "c"]), &s(&["x", "y", "z"]), lean);
        assert_eq!(out[0], "x", "text rank 1 outweighs semantic rank 1 at lean 0.5; the rest interleave");
    }
}
