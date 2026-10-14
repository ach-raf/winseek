//! Preview bbox highlight: which boxes stored in a chunk locator contain a query word.
//!
//! Two locator shapes carry boxes: Windows OCR (`{"ocr":{"w":..,"h":..,"lines":[{"t":..,"r":[x,y,w,h]}]}}`,
//! pixels) and the document parser (`{"parser":{"boxes":[{"b":[x1,y1,x2,y2],"c":..,"t":..}]}}`, 0-1000).
//! Both are returned as fractions 0-1 of the image / page.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HitBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Lower-case and drop the diacritics of the common Latin letters (`É` -> `e`, `ß` -> `ss`).
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => out.push('c'),
            'ď' | 'đ' => out.push('d'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
            'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
            'ĺ' | 'ļ' | 'ľ' | 'ł' => out.push('l'),
            'ñ' | 'ń' | 'ņ' | 'ň' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
            'ŕ' | 'ŗ' | 'ř' => out.push('r'),
            'ś' | 'ŝ' | 'ş' | 'š' => out.push('s'),
            'ţ' | 'ť' => out.push('t'),
            'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            c => out.push(c),
        }
    }
    out
}

/// The words of a free-text query worth matching: folded, at least 2 characters, no duplicates.
pub fn query_terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in query.split(|c: char| !c.is_alphanumeric()) {
        let f = fold(w);
        if f.chars().count() >= 2 && !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// Remove `<...>` tags (parser tables are stored as HTML).
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn frac(v: f64, total: f64) -> f32 {
    if total <= 0.0 {
        return 0.0;
    }
    (v / total).clamp(0.0, 1.0) as f32
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|f| f.is_finite())
}

/// The boxes of the chunk `locator` whose text contains a word of `query`; `None` when the
/// locator has no boxes or none of them match.
pub fn hit_boxes(locator: Option<&str>, query: &str) -> Option<Vec<HitBox>> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(locator?).ok()?;
    let matches = |text: &str| {
        let t = fold(text);
        terms.iter().any(|w| t.contains(w.as_str()))
    };
    let mut out: Vec<HitBox> = Vec::new();
    if let Some(ocr) = v.get("ocr") {
        let (w, h) = (num(&ocr["w"])?, num(&ocr["h"])?);
        for line in ocr.get("lines")?.as_array()? {
            let (Some(t), Some(r)) = (line["t"].as_str(), line["r"].as_array()) else { continue };
            let r: Vec<f64> = r.iter().filter_map(num).collect();
            if r.len() != 4 || !matches(t) || r[2] <= 0.0 || r[3] <= 0.0 {
                continue;
            }
            out.push(HitBox { x: frac(r[0], w), y: frac(r[1], h), w: frac(r[2], w), h: frac(r[3], h) });
        }
    }
    if let Some(boxes) = v.pointer("/parser/boxes").and_then(Value::as_array) {
        for b in boxes {
            let (Some(t), Some(r)) = (b["t"].as_str(), b["b"].as_array()) else { continue };
            let r: Vec<f64> = r.iter().filter_map(num).collect();
            let text = if b["c"] == "table" { strip_tags(t) } else { t.to_string() };
            if r.len() != 4 || !matches(&text) || r[2] <= r[0] || r[3] <= r[1] {
                continue;
            }
            out.push(HitBox {
                x: frac(r[0], 1000.0),
                y: frac(r[1], 1000.0),
                w: frac(r[2] - r[0], 1000.0),
                h: frac(r[3] - r[1], 1000.0),
            });
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OCR: &str = r#"{"ocr":{"w":200,"h":100,"lines":[
        {"t":"Invoice 4711","r":[20,10,100,20]},
        {"t":"Café total due","r":[20,40,160,20]},
        {"t":"unrelated","r":[0,80,50,10]}]}}"#;

    #[test]
    fn fold_lowercases_and_drops_diacritics() {
        assert_eq!(fold("Café ÉCOLE Straße Œuvre"), "cafe ecole strasse oeuvre");
        assert_eq!(fold("日本語"), "日本語");
    }

    #[test]
    fn terms_are_folded_words_of_two_or_more_chars() {
        assert_eq!(query_terms("Invoice a  café, INVOICE x-ray"), ["invoice", "cafe", "ray"]);
        assert!(query_terms("a b").is_empty());
        assert!(query_terms("").is_empty());
    }

    #[test]
    fn ocr_lines_that_contain_a_term_become_fractions() {
        let b = hit_boxes(Some(OCR), "CAFE").unwrap();
        assert_eq!(b, vec![HitBox { x: 0.1, y: 0.4, w: 0.8, h: 0.2 }]);
        let b = hit_boxes(Some(OCR), "invoice due").unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0], HitBox { x: 0.1, y: 0.1, w: 0.5, h: 0.2 });
        assert!(hit_boxes(Some(OCR), "zebra").is_none());
    }

    #[test]
    fn parser_boxes_are_normalised_0_1000_and_tables_match_cell_text_only() {
        let loc = r#"{"page":2,"parser":{"v":"doc2json-v1","boxes":[
            {"b":[100,200,600,300],"c":"text","t":"Total due: 1,869.00"},
            {"b":[100,400,900,500],"c":"table","t":"<table><tr><td>Item</td></tr></table>"},
            {"b":[0,0,0,0],"c":"text","t":"total degenerate"}]}}"#;
        assert_eq!(hit_boxes(Some(loc), "total").unwrap(), vec![HitBox { x: 0.1, y: 0.2, w: 0.5, h: 0.1 }]);
        assert!(hit_boxes(Some(loc), "table").is_none(), "tag names are not text");
        let t = hit_boxes(Some(loc), "item").unwrap();
        assert_eq!(t, vec![HitBox { x: 0.1, y: 0.4, w: 0.8, h: 0.1 }]);
    }

    #[test]
    fn missing_or_unusable_locators_give_none() {
        assert!(hit_boxes(None, "invoice").is_none());
        assert!(hit_boxes(Some("not json"), "invoice").is_none());
        assert!(hit_boxes(Some(r#"{"page":3,"chars":[0,9]}"#), "invoice").is_none());
        assert!(hit_boxes(Some(OCR), "  ").is_none());
        // out-of-range rectangles are clamped, empty ones dropped
        let loc = r#"{"ocr":{"w":10,"h":10,"lines":[{"t":"abc","r":[8,8,10,10]},{"t":"abc","r":[0,0,0,5]}]}}"#;
        assert_eq!(hit_boxes(Some(loc), "abc").unwrap(), vec![HitBox { x: 0.8, y: 0.8, w: 1.0, h: 1.0 }]);
    }
}
