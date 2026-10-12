//! Case folding.
//!
//! * ASCII: a 256-entry byte table (`ASCII_FOLD`), bytes >= 0x80 map to themselves.
//! * Non-ASCII: *simple* case folding = first char of `char::to_lowercase()`
//!   (so `É`->`é`, `Σ`->`σ`, `İ`->`i`), plus final sigma `ς`->`σ`. Scripts without case
//!   (Arabic, CJK, ...) pass through unchanged. This is not full Unicode case folding
//!   (no multi-char expansions such as `ß`->`ss`) and does no diacritic stripping.
//! * Fast path: if the (folded) needle is pure ASCII, haystack bytes are compared through
//!   the table without decoding UTF-8. Non-ASCII haystack bytes can never equal an ASCII
//!   byte, so this is exact except for the few non-ASCII chars that fold to ASCII
//!   (e.g. Kelvin sign U+212A -> `k`) which the fast path does not see in the haystack.

use std::cmp::Ordering;

pub static ASCII_FOLD: [u8; 256] = build_table();

const fn build_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = if i >= 65 && i <= 90 { (i + 32) as u8 } else { i as u8 };
        i += 1;
    }
    t
}

#[inline]
pub fn fold_char(c: char) -> char {
    if (c as u32) < 128 {
        ASCII_FOLD[c as usize] as char
    } else if c == 'ς' {
        'σ'
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

pub fn fold_into(s: &str, out: &mut String) {
    out.clear();
    for c in s.chars() {
        out.push(fold_char(c));
    }
}

pub fn fold_string(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    fold_into(s, &mut o);
    o
}

/// Case-insensitive substring test; `needle` must already be folded, ASCII.
#[inline]
pub fn find_ascii_ci(hay: &[u8], needle: &[u8]) -> bool {
    let n = needle.len();
    if n == 0 {
        return true;
    }
    if hay.len() < n {
        return false;
    }
    let first = needle[0];
    let last = hay.len() - n;
    let mut i = 0;
    while i <= last {
        if ASCII_FOLD[hay[i] as usize] == first {
            let mut j = 1;
            while j < n && ASCII_FOLD[hay[i + j] as usize] == needle[j] {
                j += 1;
            }
            if j == n {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Ordering of folded strings (by folded code point).
pub fn cmp_fold(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let n = ab.len().min(bb.len());
    let mut i = 0;
    while i < n {
        let (x, y) = (ab[i], bb[i]);
        if x >= 0x80 || y >= 0x80 {
            // everything before i was ASCII so i is a char boundary in both
            return a[i..].chars().map(fold_char).cmp(b[i..].chars().map(fold_char));
        }
        let (fx, fy) = (ASCII_FOLD[x as usize], ASCII_FOLD[y as usize]);
        if fx != fy {
            return fx.cmp(&fy);
        }
        i += 1;
    }
    ab.len().cmp(&bb.len())
}

/// Wildcard match (`*` any run, `?` any single char) over folded chars; whole-string match.
pub fn glob_match(pat: &[char], hay: &[char]) -> bool {
    let (mut p, mut h) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while h < hay.len() {
        if p < pat.len() && (pat[p] == '?' || (pat[p] != '*' && pat[p] == hay[h])) {
            p += 1;
            h += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = p;
            mark = h;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            mark += 1;
            h = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

/// Whole-string wildcard match on ASCII bytes; `pat` folded ASCII, `hay` folded via table.
pub fn glob_match_ascii_ci(pat: &[u8], hay: &[u8]) -> bool {
    let (mut p, mut h) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while h < hay.len() {
        if p < pat.len() && (pat[p] == b'?' || (pat[p] != b'*' && pat[p] == ASCII_FOLD[hay[h] as usize])) {
            p += 1;
            h += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = p;
            mark = h;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            mark += 1;
            h = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folds() {
        assert_eq!(fold_string("ÉTÉ"), "été");
        assert_eq!(fold_string("ΟΔΟΣ"), fold_string("οδος"));
        assert_eq!(fold_string("مرحبا 日本語"), "مرحبا 日本語");
        assert_eq!(cmp_fold("Abc", "abd"), Ordering::Less);
        assert_eq!(cmp_fold("ABC", "abc"), Ordering::Equal);
        assert_eq!(cmp_fold("a", "é"), Ordering::Less);
        assert_eq!(cmp_fold("ab", "abc"), Ordering::Less);
    }
    #[test]
    fn globs() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert!(glob_match(&c("*.log"), &c("a.log")));
        assert!(!glob_match(&c("*.log"), &c("a.logx")));
        assert!(glob_match(&c("a?c*"), &c("abcdef")));
        assert!(glob_match(&c("*"), &c("")));
        assert!(!glob_match(&c("a?"), &c("a")));
    }
}
