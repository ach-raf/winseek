//! Split extracted text into ~1500-character chunks for FTS rows and text vectors.
//!
//! Chunks end on a paragraph break when one is near the target size, else on a line break, else on
//! whitespace, else hard. Consecutive chunks overlap a little (snapped to a word start). Each chunk
//! carries a locator JSON `{"slide"|"sheet": .., "chars": [start, end]}` (character offsets into the
//! extracted text; the slide / sheet is the one active at the chunk's start, with `slide_end` /
//! `sheet_end` added when the chunk runs into the next one).

use serde_json::{json, Map, Value};

use super::extract::{Extracted, Loc, Mark};

pub const TARGET_CHARS: usize = 1500;
pub const OVERLAP_CHARS: usize = 150;
pub const MAX_CHUNKS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// Character range `[start, end)` in the extracted text.
    pub start: usize,
    pub end: usize,
    pub locator: String,
}

/// Where to end a chunk that starts at `pos` (exclusive end index into `chars`).
fn break_at(chars: &[char], pos: usize, target: usize) -> usize {
    let n = chars.len();
    let hard = (pos + target).min(n);
    if hard >= n {
        return n;
    }
    let lo = pos + target / 2;
    // paragraph break: the chunk ends right after a blank line
    if let Some(b) = (lo..=hard).rev().find(|&b| b >= 2 && chars[b - 1] == '\n' && chars[b - 2] == '\n') {
        return b;
    }
    if let Some(b) = (lo..=hard).rev().find(|&b| chars[b - 1] == '\n') {
        return b;
    }
    if let Some(b) = (lo..=hard).rev().find(|&b| chars[b - 1].is_whitespace() || chars.get(b).is_some_and(|c| c.is_whitespace())) {
        return b;
    }
    hard
}

fn loc_json(m: &Mark, suffix: &str, map: &mut Map<String, Value>) {
    match &m.loc {
        Loc::Slide(n) => map.insert(format!("slide{suffix}"), json!(n)),
        Loc::Sheet(s) => map.insert(format!("sheet{suffix}"), json!(s)),
    };
}

fn locator(marks: &[Mark], start: usize, end: usize) -> String {
    let mut map = Map::new();
    // last mark at or before `start`
    let first = marks.iter().rposition(|m| m.at <= start);
    if let Some(i) = first {
        loc_json(&marks[i], "", &mut map);
    }
    // a later mark that begins inside the chunk
    if let Some(m) = marks.iter().rev().find(|m| m.at > start && m.at < end) {
        if first.is_none_or(|i| marks[i].loc != m.loc) {
            loc_json(m, "_end", &mut map);
        }
    }
    map.insert("chars".into(), json!([start, end]));
    Value::Object(map).to_string()
}

/// Chunk `chars` with the given sizes; at most `max_chunks` chunks.
pub fn chunk_chars(chars: &[char], target: usize, overlap: usize, max_chunks: usize) -> Vec<(usize, usize)> {
    let n = chars.len();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < n && out.len() < max_chunks {
        let end = break_at(chars, pos, target);
        if chars[pos..end].iter().any(|c| !c.is_whitespace()) {
            out.push((pos, end));
        }
        if end >= n {
            break;
        }
        let cand = end.saturating_sub(overlap);
        let mut next = if cand > pos { cand } else { end };
        // start the overlap at a word boundary
        while next < end && next > 0 && !chars[next - 1].is_whitespace() {
            next += 1;
        }
        pos = next.max(pos + 1);
    }
    out
}

/// Chunk a document: ~1500 chars, small overlap, at most [`MAX_CHUNKS`].
pub fn chunk(doc: &Extracted) -> Vec<Chunk> {
    chunk_with(doc, TARGET_CHARS, OVERLAP_CHARS, MAX_CHUNKS)
}

pub fn chunk_with(doc: &Extracted, target: usize, overlap: usize, max_chunks: usize) -> Vec<Chunk> {
    let chars: Vec<char> = doc.text.chars().collect();
    chunk_chars(&chars, target, overlap, max_chunks)
        .into_iter()
        .map(|(s, e)| Chunk {
            text: chars[s..e].iter().collect::<String>().trim().to_owned(),
            start: s,
            end: e,
            locator: locator(&doc.marks, s, e),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str, marks: Vec<Mark>) -> Extracted {
        Extracted { text: text.into(), marks, truncated: false }
    }

    #[test]
    fn short_text_is_one_chunk_with_chars_locator() {
        let c = chunk(&doc("hello world", vec![]));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "hello world");
        assert_eq!(serde_json::from_str::<Value>(&c[0].locator).unwrap(), json!({"chars": [0, 11]}));
    }

    #[test]
    fn prefers_paragraph_then_line_then_space_boundaries() {
        // paragraphs of 40 chars: target 100 -> break after the blank line nearest the target
        let para = format!("{}\n\n", "p".repeat(38));
        let text = para.repeat(6);
        let chars: Vec<char> = text.chars().collect();
        let spans = chunk_chars(&chars, 100, 0, 64);
        assert_eq!(spans[0], (0, 80), "two paragraphs fit, the third crosses the target");
        for (s, e) in &spans {
            assert!(chars[*e - 1] == '\n' && chars[*e - 2] == '\n', "ends on paragraph break: {s}..{e}");
        }
        // lines without blank lines
        let text = format!("{}\n", "l".repeat(29)).repeat(10);
        let chars: Vec<char> = text.chars().collect();
        let spans = chunk_chars(&chars, 100, 0, 64);
        assert_eq!(spans[0], (0, 90));
        assert!(spans.iter().all(|(_, e)| chars[*e - 1] == '\n'));
        // words only
        let text = "word ".repeat(100);
        let chars: Vec<char> = text.chars().collect();
        let spans = chunk_chars(&chars, 52, 0, 64);
        assert!(spans.iter().all(|(_, e)| chars[*e - 1] == ' '));
        // no break opportunity at all: hard cut
        let text = "x".repeat(250);
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(chunk_chars(&chars, 100, 0, 64), [(0, 100), (100, 200), (200, 250)]);
    }

    #[test]
    fn chunks_overlap_cover_everything_and_make_progress() {
        let text: String = (0..400).map(|i| format!("word{i} ")).collect();
        let d = doc(&text, vec![]);
        let cs = chunk_with(&d, 300, 60, 64);
        assert!(cs.len() > 5);
        assert_eq!(cs[0].start, 0);
        assert_eq!(cs.last().unwrap().end, text.chars().count());
        for w in cs.windows(2) {
            assert!(w[1].start > w[0].start, "progress");
            assert!(w[1].start < w[0].end, "overlap: {} < {}", w[1].start, w[0].end);
            assert!(w[0].end - w[1].start <= 60, "overlap is small");
            let prev = text.chars().nth(w[1].start - 1).unwrap();
            assert!(prev.is_whitespace(), "next chunk starts at a word");
        }
        assert!(cs.iter().all(|c| c.end - c.start <= 300 && !c.text.is_empty()));
    }

    #[test]
    fn cap_on_chunk_count() {
        let text = "a ".repeat(MAX_CHUNKS * TARGET_CHARS);
        let cs = chunk(&doc(&text, vec![]));
        assert_eq!(cs.len(), MAX_CHUNKS);
    }

    #[test]
    fn blank_text_gives_no_chunks() {
        assert!(chunk(&doc("  \n\n \n", vec![])).is_empty());
        assert!(chunk(&doc("", vec![])).is_empty());
    }

    fn lj(marks: &[Mark], s: usize, e: usize) -> Value {
        serde_json::from_str(&locator(marks, s, e)).unwrap()
    }

    #[test]
    fn locators_carry_slide_and_sheet() {
        let marks = vec![
            Mark { at: 0, loc: Loc::Slide(1) },
            Mark { at: 10, loc: Loc::Slide(2) },
            Mark { at: 30, loc: Loc::Slide(3) },
        ];
        assert_eq!(lj(&marks, 0, 5), json!({"slide": 1, "chars": [0, 5]}));
        assert_eq!(lj(&marks, 12, 25), json!({"slide": 2, "chars": [12, 25]}));
        assert_eq!(lj(&marks, 5, 40), json!({"slide": 1, "slide_end": 3, "chars": [5, 40]}));
        let sheets = vec![Mark { at: 0, loc: Loc::Sheet("Budget".into()) }];
        assert_eq!(lj(&sheets, 3, 9), json!({"sheet": "Budget", "chars": [3, 9]}));
        assert_eq!(lj(&[], 3, 9), json!({"chars": [3, 9]}));
    }

    #[test]
    fn chunk_locators_follow_marks() {
        let mut text = String::from("Slide 1\n");
        text.push_str(&"alpha ".repeat(40));
        let at2 = text.chars().count();
        text.push_str("\n\nSlide 2\n");
        text.push_str(&"beta ".repeat(40));
        let d = doc(&text, vec![Mark { at: 0, loc: Loc::Slide(1) }, Mark { at: at2 + 2, loc: Loc::Slide(2) }]);
        let cs = chunk_with(&d, 120, 20, 64);
        assert!(cs[0].locator.contains(r#""slide":1"#));
        assert!(cs.last().unwrap().locator.contains(r#""slide":2"#));
    }
}
