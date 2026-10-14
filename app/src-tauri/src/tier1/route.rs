//! Which items go to the document parser (spec 2026-10-14-p3-parser, section C): the language gate,
//! the screenshot rule and the zero-shot "is this image a document?" check on the stored image
//! vector. All pure; the passes that use them live in `parse_pass`.

use super::embedder::{EmbedError, Embedder};

/// Zero-shot document check on the image vector: the best document prototype must reach `DOC_MIN`
/// and beat the best non-document prototype by `DOC_MARGIN`.
///
/// Measured 2026-10-14 with `winseek-eval route` (EmbeddingGemma-2 Q8_0, llama.cpp b11588) on 418
/// labelled images: 138 documents (synthetic receipts / invoices / forms / contracts / slides / book
/// pages / whiteboards, flat and "photographed", plus first pages of 40 real PDFs) and 280 others
/// (real phone photos, wallpapers, game and film stills, screenshots, memes). Cosines live in
/// 0.53..0.76: every document scored best_doc >= 0.61 but so did many photos (up to 0.66), so the
/// margin does the separating. Documents: margin >= 0.042; photos: margin <= 0.034. At 0.60 / 0.04:
/// recall 138/138, no photo routed, 2 false positives (both web-page screenshots). Margin 0.05 would
/// lose a real PDF page for no photo gain on this set; 0.03 (the old guess) routed 3 photos.
pub const DOC_MIN: f32 = 0.60;
pub const DOC_MARGIN: f32 = 0.04;
/// A screenshot goes to the parser when Windows OCR read at least this many characters.
pub const SCREENSHOT_MIN_CHARS: usize = 200;
/// Image vectors looked at per route pass.
pub const ROUTE_BATCH: usize = 500;
/// Share of alphabetic characters that must be Latin / CJK for the parser to take the item.
pub const SCRIPT_MIN_SHARE: f32 = 0.8;

pub const DOC_PROMPTS: [&str; 6] = [
    "a scanned document page",
    "a receipt",
    "a whiteboard with writing",
    "a printed form",
    "a presentation slide",
    "a page of a book",
];
pub const OTHER_PROMPTS: [&str; 6] = [
    "a photo of people",
    "a landscape photo",
    "a photo of an animal",
    "a photo of food",
    "a meme",
    "a product photo",
];

/// Is `c` in a script the parser reads (Latin incl. extensions, Japanese kana, CJK ideographs)?
/// Hangul is deliberately not: the model does not support it.
fn supported_script(c: char) -> bool {
    matches!(c as u32,
        0x0000..=0x024F | 0x1E00..=0x1EFF
        | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF)
}

/// Language gate: the text is empty, or mostly (>= [`SCRIPT_MIN_SHARE`] of its letters) Latin / CJK.
pub fn script_ok(text: &str) -> bool {
    let (mut letters, mut ok) = (0usize, 0usize);
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        letters += 1;
        ok += usize::from(supported_script(c));
    }
    letters == 0 || ok as f32 / letters as f32 >= SCRIPT_MIN_SHARE
}

/// Screenshots with a lot of readable text go to the parser; short UI captures stay with Windows OCR.
pub fn screenshot_wants_parser(ocr_text: &str) -> bool {
    ocr_text.chars().count() >= SCREENSHOT_MIN_CHARS && script_ok(ocr_text)
}

/// The embedded prototype queries (L2-normalised, like stored image vectors).
#[derive(Debug, Clone, PartialEq)]
pub struct Prototypes {
    pub doc: Vec<Vec<f32>>,
    pub other: Vec<Vec<f32>>,
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn best(protos: &[Vec<f32>], v: &[f32]) -> f32 {
    protos.iter().map(|p| dot(p, v)).fold(f32::NEG_INFINITY, f32::max)
}

impl Prototypes {
    /// Embed the prompts once (the caller caches the result for the session).
    pub fn embed(e: &mut dyn Embedder) -> Result<Prototypes, EmbedError> {
        let mut all = |prompts: &[&str]| prompts.iter().map(|p| e.embed_query(p)).collect::<Result<Vec<_>, _>>();
        Ok(Prototypes { doc: all(&DOC_PROMPTS)?, other: all(&OTHER_PROMPTS)? })
    }

    /// Document-like image vector (pure cosine against the prototypes).
    pub fn is_doc(&self, image: &[f32]) -> bool {
        let d = best(&self.doc, image);
        d >= DOC_MIN && d - best(&self.other, image) >= DOC_MARGIN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_gate_latin_cjk_and_the_rest() {
        assert!(script_ok(""), "no letters: nothing to object to");
        assert!(script_ok("  123 -- 456 "));
        assert!(script_ok("Total due: 1,869.00 EUR"));
        assert!(script_ok("Zażółć gęślą jaźń, café naïve"));
        assert!(script_ok("Ạ Ế Ỳ"), "Latin Extended Additional");
        assert!(script_ok("請求書 合計 ひらがな カタカナ"), "CJK + kana");
        assert!(!script_ok("مرحبا بالعالم"), "Arabic");
        assert!(!script_ok("Привет мир"), "Cyrillic");
        assert!(!script_ok("안녕하세요 세계"), "Hangul is not supported");
        assert!(!script_ok("สวัสดี"), "Thai");
    }

    #[test]
    fn script_gate_threshold_is_80_percent_of_letters() {
        // 8 Latin + 2 Cyrillic letters = exactly 0.8; digits and spaces do not count
        assert!(script_ok("abcdefgh ЖЖ 12345"));
        assert!(!script_ok("abcdefg ЖЖЖ 12345"));
        // mixed with a little Arabic stays supported
        assert!(script_ok("Invoice number 4711 for the customer مرحبا"));
    }

    #[test]
    fn screenshots_need_200_chars_of_supported_text() {
        let long = "word ".repeat(40);
        assert!(screenshot_wants_parser(&long));
        assert!(!screenshot_wants_parser(&long[..150]));
        assert!(!screenshot_wants_parser(&"слово ".repeat(40)), "Cyrillic stays with Windows OCR");
        assert!(screenshot_wants_parser(&"字".repeat(200)), "characters, not bytes");
        assert!(!screenshot_wants_parser(&"字".repeat(199)));
    }

    fn unit(i: usize) -> Vec<f32> {
        let mut v = vec![0f32; 8];
        v[i] = 1.0;
        v
    }

    /// Prototypes on axes 0 (documents) and 1 (everything else).
    fn protos() -> Prototypes {
        Prototypes { doc: vec![unit(0), vec![0.6, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0, 0.0]], other: vec![unit(1)] }
    }

    fn mix(doc: f32, other: f32) -> Vec<f32> {
        // doc on axis 0, other on axis 1, the remainder on an unrelated axis; unit length
        let rest = (1.0 - doc * doc - other * other).max(0.0).sqrt();
        let mut v = vec![0f32; 8];
        v[0] = doc;
        v[1] = other;
        v[7] = rest;
        v
    }

    #[test]
    fn doc_needs_min_score_and_margin() {
        let p = protos();
        assert!(p.is_doc(&mix(0.7, 0.6)));
        assert!(!p.is_doc(&mix(0.59, 0.0)), "below DOC_MIN");
        assert!(p.is_doc(&mix(0.61, 0.0)));
        assert!(!p.is_doc(&mix(0.66, 0.63)), "margin 0.03 < DOC_MARGIN");
        assert!(p.is_doc(&mix(0.66, 0.615)), "margin 0.045");
        assert!(!p.is_doc(&mix(0.0, 0.9)), "a photo");
    }

    /// Guard: these are measured (see the constants' doc comment), not free parameters. Re-run
    /// `winseek-eval route` before changing them, and keep the status page's `provisional` in step.
    #[test]
    fn routing_thresholds_are_the_measured_ones() {
        assert_eq!((DOC_MIN, DOC_MARGIN), (0.60, 0.04));
        // real scores from the eval set: worst document vs. best-scoring photos
        let p = protos();
        assert!(p.is_doc(&mix(0.647, 0.605)), "real PDF page, margin 0.042");
        assert!(!p.is_doc(&mix(0.647, 0.613)), "photo, margin 0.034");
        assert!(!p.is_doc(&mix(0.622, 0.589)), "photo, margin 0.033");
    }

    struct Q;
    impl Embedder for Q {
        fn embed_images(&mut self, _: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Err(EmbedError::ModelsMissing)
        }
        fn embed_query(&mut self, q: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(unit(if DOC_PROMPTS.contains(&q) { 0 } else { 1 }))
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            super::super::embedder::index_meta("test")
        }
    }

    struct Down;
    impl Embedder for Down {
        fn embed_images(&mut self, _: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Err(EmbedError::ModelsMissing)
        }
        fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
            Err(EmbedError::Other("down".into()))
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            super::super::embedder::index_meta("test")
        }
    }

    #[test]
    fn prototypes_embed_every_prompt_once_and_surface_errors() {
        let p = Prototypes::embed(&mut Q).unwrap();
        assert_eq!((p.doc.len(), p.other.len()), (6, 6));
        assert!(p.doc.iter().all(|v| *v == unit(0)) && p.other.iter().all(|v| *v == unit(1)));
        assert!(p.is_doc(&unit(0)) && !p.is_doc(&unit(1)));
        assert!(matches!(Prototypes::embed(&mut Down), Err(EmbedError::Other(_))));
    }
}
