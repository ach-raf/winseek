//! The tool table: the exact set of read-only tools the agent pipe serves (PRD A5). Nothing
//! else exists — [`all`] is the single source of truth, and a test asserts the 11 names so an
//! action tool cannot be added quietly.

use serde_json::{json, Value};

pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    /// Written for the model: what it returns, limits, when to use it.
    pub description: &'static str,
    pub schema: fn() -> Value,
}

/// MCP annotations every tool shares (built per call: `json!` is not const).
fn annotations() -> Value {
    serde_json::json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
    })
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

fn n(desc: &str, min: i64, max: i64) -> Value {
    json!({"type": "integer", "description": desc, "minimum": min, "maximum": max})
}

const SEARCH_FILES_DESC: &str = "Find files anywhere on this PC by name, in milliseconds (indexed, updated live). \
Query syntax (Everything-style): terms are ANDed; \"quoted phrase\" is one term; !term negates; \
* and ? are wildcards (whole-name match; whole path if the term contains a backslash); a term containing \\ or / matches the full path; \
ext:jpg;png filters by extension. `type` filters All/Folders/Images/Docs/Video/Audio/Apps. \
`sort` is relevance|name|date|size. `modified_after`/`modified_before` take an ISO date (2025-01-31) or \
today|yesterday|this_week|last_week|this_month|this_year. `min_size`/`max_size` take bytes or strings like \"10MB\", \"1.5GB\". \
With a date or size filter the scan walks at most 30000 rows and reports truncated:true when it stopped early.";

const SEARCH_BY_MEANING_DESC: &str = "Find documents, screenshots and photos by what they contain, not by file name \
(\"the invoice with VAT 19%\", \"screenshot of the login error\"). Covers only the folders chosen in winseek's \
\"Search by meaning\" settings. Returns path, score, snippet with the query words in [brackets], PDF page, and \
source (meaning = vector match, text = found in extracted text). Also set `state` (ready|indexing|models_missing) — \
explain it to the user when hits are missing.";

const RECENT_FILES_DESC: &str = "Files changed most recently (from the USN journal), newest first. Use it for \
\"what was I just working on\" questions without a search term.";

const FILE_INFO_DESC: &str = "Metadata for one file: kind (image/video/audio/text/document/pdf/folder/other), size, \
modified and created times, image dimensions and EXIF (date, camera, GPS flag), PDF page count, extracted text preview \
for text and Office files. Read-only; the file is never opened by another program.";

const READ_TEXT_DESC: &str = "Get the text content of one file: plain-text and code files, Office/ODF/RTF documents, \
the text layer of PDFs (`page` for one page, whole document otherwise), or the OCR text winseek has for screenshots. \
Refuses binary files (\"no text\"). Caps output at `max_chars` (default 20000) and reports truncated:true. \
Subject to the user's content scope: indexed folders only, or any file.";

const PARSE_DOCUMENT_DESC: &str = "Run the document parser (Infinity-Parser) on a scanned PDF or document image and \
return markdown with tables. Slow: 10-60 s on GPU, longer on CPU; the parser models must be installed \
(error \"parser not installed\" otherwise). Use read_text first — it is instant when the text layer is good.";

const FIND_APPS_DESC: &str = "Find installed applications, Windows Settings pages and built-in system commands by name \
(\"terminal\", \"bluetooth\", \"shutdown\"). Each hit has a `target`: an exe path (with args), a store-app ID, an \
ms-settings: URI, or a command name — launch it with your own tools if the user asks. winseek itself never runs anything.";

const INDEX_STATUS_DESC: &str = "winseek's health: indexed volumes (drive, filesystem, file/folder counts, scan state), \
semantic-index state and counts, whether the models and the document parser are installed, and whether agent access is on. \
Call this first when another tool says access is off or returns an index error.";

const LIBRARY_BIG_FILES_DESC: &str = "The biggest files under the user's profile folder, largest first. Good for \
\"what is eating my disk\". Returns name, path, size, modified.";

const LIBRARY_DUPLICATES_DESC: &str = "Groups of byte-identical files (same size, same SHA-256), biggest reclaimable \
space first. Each group: {size, members:[{path,...}], reclaimable}. Read-only — deleting duplicates stays with you and the user.";

const LIBRARY_COLLECTIONS_DESC: &str = "Without `name`: the user's smart collections (semantic saved searches like \
\"screenshots\", \"documents\"). With `name`: the photos/files of that collection (path, score, page), ranked by relevance.";

const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "search_files",
        title: "Search files by name",
        description: SEARCH_FILES_DESC,
        schema: || obj(
            json!({
                "query": s("Name search query (syntax in the description)"),
                "type": s("all (default) | folders | images | docs | video | audio | apps"),
                "sort": s("relevance (default) | name | date | size"),
                "modified_after": s("ISO date or today|yesterday|this_week|last_week|this_month|this_year"),
                "modified_before": s("ISO date or a relative word, same as modified_after"),
                "min_size": s("bytes, or \"10MB\", \"1.5GB\""),
                "max_size": s("bytes, or \"10MB\", \"1.5GB\""),
                "limit": n("Results to return (default 50)", 1, 200),
                "offset": n("Skip this many results after filtering", 0, i64::MAX),
            }),
            &["query"],
        ),
    },
    ToolDef {
        name: "search_by_meaning",
        title: "Search files by content",
        description: SEARCH_BY_MEANING_DESC,
        schema: || obj(
            json!({
                "query": s("What the files contain"),
                "limit": n("Results (default 20)", 1, 50),
            }),
            &["query"],
        ),
    },
    ToolDef {
        name: "recent_files",
        title: "Recently changed files",
        description: RECENT_FILES_DESC,
        schema: || obj(json!({"limit": n("Results (default 30)", 1, 200)}), &[]),
    },
    ToolDef {
        name: "file_info",
        title: "File metadata",
        description: FILE_INFO_DESC,
        schema: || obj(json!({"path": s("Full path of the file or folder")}), &["path"]),
    },
    ToolDef {
        name: "read_text",
        title: "Read text from a file",
        description: READ_TEXT_DESC,
        schema: || obj(
            json!({
                "path": s("Full path of the file"),
                "page": n("PDF page to read (1-based); omit for the whole document", 1, 1_000_000),
                "max_chars": n("Cap on returned text (default 20000)", 1, 200_000),
            }),
            &["path"],
        ),
    },
    ToolDef {
        name: "parse_document",
        title: "Parse a scanned document",
        description: PARSE_DOCUMENT_DESC,
        schema: || obj(
            json!({
                "path": s("Full path of the scanned PDF or image"),
                "page": n("Parse only this PDF page (1-based)", 1, 1_000_000),
            }),
            &["path"],
        ),
    },
    ToolDef {
        name: "find_apps",
        title: "Find apps and settings",
        description: FIND_APPS_DESC,
        schema: || obj(
            json!({
                "query": s("Name of the app, setting or command"),
                "kind": s("app | setting | command (omit for all)"),
                "limit": n("Results (default 10)", 1, 50),
            }),
            &["query"],
        ),
    },
    ToolDef {
        name: "index_status",
        title: "winseek index status",
        description: INDEX_STATUS_DESC,
        schema: || obj(json!({}), &[]),
    },
    ToolDef {
        name: "library_big_files",
        title: "Biggest files",
        description: LIBRARY_BIG_FILES_DESC,
        schema: || obj(json!({"limit": n("Results (default 50)", 1, 500)}), &[]),
    },
    ToolDef {
        name: "library_duplicates",
        title: "Duplicate files",
        description: LIBRARY_DUPLICATES_DESC,
        schema: || obj(json!({"limit": n("Groups to return (default 50)", 1, 500)}), &[]),
    },
    ToolDef {
        name: "library_collections",
        title: "Smart collections",
        description: LIBRARY_COLLECTIONS_DESC,
        schema: || obj(json!({"name": s("Collection name; omit to list the collections")}), &[]),
    },
];

pub fn all() -> &'static [ToolDef] {
    TOOLS
}

/// The MCP `tools` array. Disabled -> only `index_status`, which answers `{access:"off"}` so
/// the agent can tell the user what to flip (PRD A4).
pub fn list_for(enabled: bool) -> Value {
    Value::Array(
        all()
            .iter()
            .filter(|t| enabled || t.name == "index_status")
            .map(|t| {
                json!({
                    "name": t.name,
                    "title": t.title,
                    "description": t.description,
                    "inputSchema": (t.schema)(),
                    "annotations": annotations(),
                })
            })
            .collect(),
    )
}

/// Does `path` sit under one of `include` (and not under `exclude`)? Pure, case-insensitive,
/// separator-normalised, prefix-trap safe (`C:\a\Docs2` is not under `C:\a\Docs`).
pub fn content_allowed(path: &str, indexed_only: bool, include: &[String], exclude: &[String]) -> bool {
    if !indexed_only {
        return true;
    }
    let norm = |p: &str| p.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let p = norm(path);
    if p.contains("..\\") {
        return false;
    }
    let under = |root: &str| p == root || p.starts_with(&format!("{root}\\"));
    include.iter().map(|r| norm(r)).any(|r| under(&r)) && !exclude.iter().map(|r| norm(r)).any(|r| under(&r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_exactly_the_eleven() {
        // The 11 names of PRD A5, spelled out here: a new entry in the table fails this test,
        // so an action tool cannot be added quietly.
        const PRD: [&str; 11] = [
            "search_files",
            "search_by_meaning",
            "recent_files",
            "file_info",
            "read_text",
            "parse_document",
            "find_apps",
            "index_status",
            "library_big_files",
            "library_duplicates",
            "library_collections",
        ];
        let table: Vec<&str> = all().iter().map(|t| t.name).collect();
        assert_eq!(table, PRD);
    }

    #[test]
    fn every_schema_is_an_object_and_names_unique() {
        let mut seen = std::collections::HashSet::new();
        for t in all() {
            let schema = (t.schema)();
            assert_eq!(schema["type"], "object", "{}", t.name);
            assert!(schema["properties"].is_object());
            assert!(seen.insert(t.name), "duplicate {}", t.name);
            assert!(!t.description.is_empty());
            assert!(t.title.len() < 40);
        }
    }

    #[test]
    fn disabled_list_is_only_index_status() {
        let off = list_for(false);
        let arr = off.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "index_status");
        let on = list_for(true);
        assert_eq!(on.as_array().unwrap().len(), 11);
        for t in on.as_array().unwrap() {
            assert_eq!(t["annotations"]["readOnlyHint"], true);
            assert_eq!(t["annotations"]["destructiveHint"], false);
            assert_eq!(t["annotations"]["idempotentHint"], true);
            assert_eq!(t["annotations"]["openWorldHint"], false);
            assert!(t["inputSchema"]["type"] == "object");
        }
    }

    #[test]
    fn content_scope_rules() {
        let inc = vec!["C:\\Users\\A\\Docs".to_string(), "D:/Media".to_string()];
        let exc = vec!["C:\\Users\\A\\Docs\\secret".to_string()];
        assert!(content_allowed(r"C:\Users\a\Docs\report.docx", true, &inc, &exc));
        assert!(content_allowed(r"C:\Users\A\DOCS", true, &inc, &exc), "root itself is in scope");
        assert!(!content_allowed(r"C:\Users\a\Docs2\report.docx", true, &inc, &exc), "prefix trap");
        assert!(!content_allowed(r"C:\Users\a\Docs\secret\k.pdf", true, &inc, &exc), "excluded");
        assert!(content_allowed(r"D:\Media\video/x.mp4", true, &inc, &exc), "mixed slashes");
        assert!(!content_allowed(r"E:\other.docx", true, &inc, &exc));
        assert!(!content_allowed(r"C:\Users\a\Docs\..\Docs\secret\x", true, &inc, &exc), ".. escape");
        // "any" scope reads everywhere
        assert!(content_allowed(r"E:\anywhere.bin", false, &inc, &exc));
    }
}
