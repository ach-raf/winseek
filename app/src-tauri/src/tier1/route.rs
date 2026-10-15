//! Which items go to the document parser (spec 2026-10-14-p3-parser, section C): the OCR-quality
//! gate, the screenshot rule and the zero-shot "is this image a document?" check on the stored
//! image vector. All pure; the passes that use them live in `parse_pass`.

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
/// Stored / fresh OCR text shorter than this is treated as sparse (the parser should try the page).
pub const MIN_OCR_CHARS: usize = 200;
/// Share of non-space characters that must be alphanumeric for OCR text to count as readable.
const ALNUM_MIN_SHARE: f32 = 0.6;
/// Image vectors looked at per route pass.
pub const ROUTE_BATCH: usize = 500;

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

/// Would the document parser plausibly beat this OCR text? Script-agnostic — Infinity-Parser2
/// reads Latin, CJK and Arabic alike (verified live 2026-10-15 on a Moroccan ID card), so the old
/// Latin/CJK language gate only blocked good parses. Text that is sparse (little came out of a
/// page) or junk-heavy (mostly punctuation and symbols) is worth replacing; anything else keeps
/// its OCR text and saves the GPU.
pub fn ocr_looks_poor(text: &str) -> bool {
    if text.chars().count() < MIN_OCR_CHARS {
        return true;
    }
    let (mut alnum, mut total) = (0usize, 0usize);
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        total += 1;
        alnum += usize::from(c.is_alphanumeric());
    }
    total == 0 || (alnum as f32) / (total as f32) < ALNUM_MIN_SHARE
}

/// Screenshots go to the parser when Windows OCR read a lot of text but it looks poor; short UI
/// captures and cleanly-read ones stay with Windows OCR.
pub fn screenshot_wants_parser(ocr_text: &str) -> bool {
    ocr_text.chars().count() >= SCREENSHOT_MIN_CHARS && ocr_looks_poor(ocr_text)
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
    fn quality_gate_is_script_agnostic() {
        // 220 chars of clean text, Arabic first: the parser is not needed, whatever the script.
        let arabic = "هذه فاتورة صادرة عن الشركة المغربية الشرقية بتاريخ أمس بطاقة إلكترونية ".repeat(4);
        assert!(arabic.chars().count() >= MIN_OCR_CHARS);
        assert!(!ocr_looks_poor(&arabic), "clean Arabic reads fine");
        assert!(ocr_looks_poor("مرحبا بالعالم"), "sparse text goes to the parser");
        assert!(ocr_looks_poor(""), "no text at all: let the parser try");
        assert!(ocr_looks_poor(&"·-\\|/ *_—".repeat(40)), "symbol junk goes to the parser");
        let clean = "Invoice number 4711 for the customer, total due 1,869.00 EUR, payable within 30 days of the invoice date. ".repeat(2);
        assert!(!ocr_looks_poor(&clean), "clean Latin is kept");
        // digits count as content: a mostly-numbers page (forms, IDs) still reads fine
        let ids = "U1234567 05/12/1983 22/07/2029 CAH 123457 ".repeat(6);
        assert!(!ocr_looks_poor(&ids));
    }

    #[test]
    fn screenshots_want_the_parser_only_for_poor_text() {
        let clean = "word ".repeat(40);
        assert!(!screenshot_wants_parser(&clean), "cleanly read: nothing to add");
        assert!(!screenshot_wants_parser(&"word ".repeat(20)), "short capture stays with Windows OCR");
        let junk = "·-\\|/ *_—".repeat(30);
        assert!(screenshot_wants_parser(&junk), "plenty of chars but garbage: parse it");
        let arabic = "بسم الله الرحمن الرحيم هذه فاتورة ".repeat(8);
        assert!(!screenshot_wants_parser(&arabic), "clean Arabic stays, like clean Latin");
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
