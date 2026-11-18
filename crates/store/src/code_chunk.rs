//! Code stretches: source chunked into stretches of whole lines (see [`code_stretches`]),
//! each carrying the 1-based line span a hit opens at.
//!
//! Ported from DigUp's `CodeExtractor.lines` in the measured configuration: 1800-character
//! stretches (1200 / 3600 measured worse), a single longer line is a stretch of its own cut at
//! 4000, a mid-block cut hands its last two lines to the next stretch so whatever spans the
//! cut is read whole once. Lives in the store crate so the eval harness indexes code with the
//! exact chunker the app runs; `tier1::chunk::chunk_code` wraps this for the app.

use serde_json::json;

/// Stretches hold at most this many characters.
pub const CODE_STRETCH: usize = 1800;
/// A single line longer than a stretch is its own stretch, cut here.
pub const CODE_MAX_PIECE: usize = 4000;
/// A code file contributes at most this many stretches — the same bound the rest of the index
/// keeps per file.
pub const CODE_MAX_CHUNKS: usize = 64;

/// Lines that close a block (shell, Ruby, Lua, Objective-C, the C preprocessor).
const CODE_CLOSERS: &[&str] = &[
    "end", "fi", "done", "esac", "@end", "#endif", "end;", "end)",
];

/// A stretch of whole lines: 0-based `first`/`last` (exclusive) indexes into `lines`, plus the
/// char range of the kept body.
pub struct Stretch {
    pub text: String,
    pub first: usize,
    pub last: usize,
    pub char_start: usize,
    pub char_end: usize,
}

fn code_blank(line: &str) -> bool {
    line.chars().all(char::is_whitespace)
}

/// A line that starts a new block at the left margin; a closing line belongs to the block above it.
fn code_starts_block(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    if first.is_whitespace() || "})]".contains(first) {
        return false;
    }
    let trimmed = line.trim();
    !CODE_CLOSERS.contains(&trimmed)
}

/// Stretches of whole lines, each at most `size` characters (a longer line is a stretch of its
/// own, cut at [`CODE_MAX_PIECE`]). A stretch ends, when it can, where the code does: before a
/// line that starts a new block after a blank line first, then after a blank line or at a new
/// block — both only in its last 60%. One that had to stop mid-block hands its last two lines on
/// to the next, so whatever spans the cut is read whole once. `text` is `\n`-normalised upstream.
pub fn code_stretches(text: &str, size: usize) -> Vec<Stretch> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop(); // the final newline
    }
    let count = lines.len();
    // Characters before each line, each line counted with its newline.
    let mut offsets = Vec::with_capacity(count + 1);
    offsets.push(0usize);
    for line in &lines {
        offsets.push(offsets.last().unwrap() + line.chars().count() + 1);
    }
    let mut pieces: Vec<Stretch> = Vec::new();
    let mut start = 0usize;
    while start < count && pieces.len() < CODE_MAX_CHUNKS {
        while start < count && code_blank(lines[start]) {
            start += 1;
        }
        if start >= count {
            break;
        }
        let mut end = start + 1; // the stretch is lines[start..<end]
        while end < count && offsets[end + 1] - offsets[start] <= size {
            end += 1;
        }
        let mut natural = end >= count;
        if !natural {
            let floor = offsets[start] + size * 2 / 5;
            let last_break = |pred: &dyn Fn(usize) -> bool| -> Option<usize> {
                let mut line = end;
                while line > start + 1 && offsets[line] >= floor {
                    if pred(line) {
                        return Some(line);
                    }
                    line -= 1;
                }
                None
            };
            if let Some(cut) =
                last_break(&|l| code_starts_block(lines[l]) && code_blank(lines[l - 1])).or_else(
                    || last_break(&|l| code_blank(lines[l - 1]) || code_starts_block(lines[l])),
                )
            {
                end = cut;
                natural = true;
            }
        }
        let mut last = end;
        while last > start + 1 && code_blank(lines[last - 1]) {
            last -= 1; // blank lines at its end don't count
        }
        let mut body = lines[start..last].join("\n");
        if body.chars().count() > CODE_MAX_PIECE {
            body = body.chars().take(CODE_MAX_PIECE).collect();
        }
        if !body.is_empty() {
            pieces.push(Stretch {
                char_start: offsets[start],
                char_end: offsets[last - 1] + lines[last - 1].chars().count(),
                text: body,
                first: start,
                last,
            });
        }
        if pieces.len() >= CODE_MAX_CHUNKS {
            break;
        }
        start = if natural {
            end
        } else {
            end.saturating_sub(2).max(start + 1)
        };
    }
    pieces
}

/// Locator of a code stretch: the 1-based lines it spans, so a hit opens there.
pub fn code_locator(first: usize, last: usize) -> String {
    json!({ "loc": first + 1, "loc_end": last }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_spans_are_lines_and_cover_the_file() {
        let cs = code_stretches("fn main() {\n    hi();\n}\n", CODE_STRETCH);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].first, 0);
        assert_eq!(cs[0].last, 3, "three lines, last exclusive");
        assert_eq!(cs[0].char_start, 0);
        assert_eq!(cs[0].text, "fn main() {\n    hi();\n}");
        assert_eq!(
            code_locator(cs[0].first, cs[0].last),
            r#"{"loc":1,"loc_end":3}"#
        );
    }

    #[test]
    fn blank_only_text_yields_nothing() {
        assert!(code_stretches("  \n\n", CODE_STRETCH).is_empty());
        assert!(code_stretches("", CODE_STRETCH).is_empty());
    }
}
