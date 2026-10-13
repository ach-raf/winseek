//! Shared text decoding (preview pane and the Tier 1 text extractor).

/// Decode a text prefix: UTF-8 / UTF-16 BOM, else UTF-8, else Latin-1. `None` = binary (NUL bytes without a
/// UTF-16 BOM). A multi-byte character cut by the read cap is dropped, not turned into U+FFFD.
pub fn decode_text(bytes: &[u8], truncated: bool) -> Option<(String, &'static str)> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Some((utf8_prefix(rest, truncated).0, "UTF-8 BOM"));
    }
    for (bom, le, name) in [([0xFF, 0xFE], true, "UTF-16 LE"), ([0xFE, 0xFF], false, "UTF-16 BE")] {
        if let Some(rest) = bytes.strip_prefix(&bom) {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) })
                .collect();
            return Some((String::from_utf16_lossy(&units), name));
        }
    }
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    let (s, valid) = utf8_prefix(bytes, truncated);
    if valid {
        Some((s, "UTF-8"))
    } else {
        Some((bytes.iter().map(|&b| b as char).collect(), "Latin-1"))
    }
}

/// (text, was valid UTF-8). With `truncated`, an incomplete character at the very end is not an error.
fn utf8_prefix(bytes: &[u8], truncated: bool) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_owned(), true),
        Err(e) if truncated && e.error_len().is_none() => {
            (String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned(), true)
        }
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_boms_and_fallbacks() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFhi", false), Some(("hi".into(), "UTF-8 BOM")));
        assert_eq!(decode_text(b"\xFF\xFEh\0i\0", false), Some(("hi".into(), "UTF-16 LE")));
        assert_eq!(decode_text(b"\xFE\xFF\0h\0i", false), Some(("hi".into(), "UTF-16 BE")));
        assert_eq!(decode_text("h\u{e9}llo".as_bytes(), false), Some(("h\u{e9}llo".into(), "UTF-8")));
        // invalid UTF-8 -> Latin-1
        assert_eq!(decode_text(b"caf\xE9", false), Some(("caf\u{e9}".into(), "Latin-1")));
        // binary
        assert_eq!(decode_text(b"MZ\0\0\x03", false), None);
        // a character cut by the read cap is dropped when truncated, an error otherwise
        let cut = &"a\u{e9}".as_bytes()[..2];
        assert_eq!(decode_text(cut, true), Some(("a".into(), "UTF-8")));
        assert_eq!(decode_text(cut, false).map(|d| d.1), Some("Latin-1"));
    }
}
