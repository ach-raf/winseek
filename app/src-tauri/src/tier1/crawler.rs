//! Initial / re-scope crawl: walk the include roots and queue supported files (images, Office /
//! text documents and, when opted in, source code).
//!
//! Skips reparse points (junctions, symlinks, OneDrive placeholders - reading those could
//! trigger a cloud download), hidden+system entries and anything `in_scope_with` rejects.

use std::path::Path;
use std::sync::Mutex;

use winseek_store::{in_scope_with, Ignore, Scopes, Store};

use super::gitignore::Chain;
use super::identity::{file_info, ATTR_DIRECTORY, ATTR_HIDDEN, ATTR_REPARSE_POINT, ATTR_SYSTEM};

pub const KIND_IMAGE: &str = "image";
/// Office / OpenDocument / plain text / RTF (and legacy Office, which is recorded as skipped).
pub const KIND_DOC: &str = "doc";
/// Source code; only crawled when the `index_code` setting is on.
pub const KIND_CODE: &str = "code";
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif"];
const DOC_EXTS: &[&str] = &[
    "docx", "docm", "dotx", "dotm", "xlsx", "xlsm", "xltx", "xltm", "pptx", "pptm", "ppsx", "potx", "odt", "ott", "ods", "ots", "odp", "otp",
    "doc", "dot", "xls", "xlt", "ppt", "pps", "pot", "rtf", "txt", "md", "markdown", "csv", "tsv", "log",
];
const CODE_EXTS: &[&str] = &[
    "rs", "py", "pyi", "js", "jsx", "mjs", "cjs", "ts", "tsx", "java", "kt", "kts", "scala", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "cs", "go",
    "rb", "php", "swift", "m", "mm", "dart", "lua", "r", "jl", "ex", "exs", "erl", "hs", "ml", "clj", "sh", "bash", "zsh", "ps1", "psm1", "bat",
    "cmd", "sql", "html", "htm", "css", "scss", "sass", "less", "vue", "svelte", "json", "toml", "yaml", "yml", "xml", "ini", "cfg", "gradle",
    "cmake", "proto", "graphql", "tf",
];

/// True for the image formats the worker can decode.
pub fn is_supported_image(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn ext_in(path: &str, list: &[&str]) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| list.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Minified / generated artefacts are noise even in a source tree.
fn is_generated(path: &str) -> bool {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase();
    name.contains(".min.") || name.ends_with(".map") || name.ends_with("-lock.json") || name == "package-lock.json"
}

/// Which Tier 1 reader a path belongs to: `image`, `doc`, or `code` (only when `index_code`).
pub fn kind_of(path: &str, index_code: bool) -> Option<&'static str> {
    if is_supported_image(path) {
        Some(KIND_IMAGE)
    } else if ext_in(path, DOC_EXTS) {
        Some(KIND_DOC)
    } else if index_code && ext_in(path, CODE_EXTS) && !is_generated(path) {
        Some(KIND_CODE)
    } else {
        None
    }
}

/// Entries the crawler never looks at.
pub fn skip_attrs(attrs: u32) -> bool {
    attrs & ATTR_REPARSE_POINT != 0 || attrs & (ATTR_HIDDEN | ATTR_SYSTEM) == (ATTR_HIDDEN | ATTR_SYSTEM)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CrawlStats {
    pub dirs: u64,
    pub files_seen: u64,
    pub upserted: u64,
    pub errors: u64,
}

/// Walk every include root. `stop` is polled per entry (return true to abort).
///
/// With `index_code`, source files are queued too, minus whatever the `.gitignore` files between
/// the include root and the file ignore.
pub fn crawl(store: &Mutex<Store>, scopes: &Scopes, ignore: &Ignore, index_code: bool, stop: &dyn Fn() -> bool) -> CrawlStats {
    let mut st = CrawlStats::default();
    let mut stack: Vec<(std::path::PathBuf, Chain)> =
        scopes.include.iter().map(|r| (std::path::PathBuf::from(r.trim_end_matches(['\\', '/'])), Chain::default())).collect();
    // `C:` alone means the drive's current directory; keep the trailing slash for drive roots.
    for (r, _) in &mut stack {
        if r.as_os_str().len() == 2 && r.to_string_lossy().ends_with(':') {
            r.push("\\");
        }
    }
    while let Some((dir, parent_chain)) = stack.pop() {
        if stop() {
            break;
        }
        let chain = if index_code { parent_chain.descend(&dir) } else { parent_chain };
        let Ok(rd) = std::fs::read_dir(&dir) else {
            st.errors += 1;
            continue;
        };
        st.dirs += 1;
        for entry in rd.flatten() {
            if stop() {
                return st;
            }
            let path = entry.path();
            let Ok(md) = entry.metadata() else {
                st.errors += 1;
                continue;
            };
            use std::os::windows::fs::MetadataExt;
            let attrs = md.file_attributes();
            if skip_attrs(attrs) {
                continue;
            }
            let ps = path.to_string_lossy();
            if attrs & ATTR_DIRECTORY != 0 {
                if in_scope_with(scopes, ignore, &ps) {
                    stack.push((path, chain.clone()));
                }
                continue;
            }
            let Some(kind) = kind_of(&ps, index_code) else { continue };
            if !in_scope_with(scopes, ignore, &ps) || (kind == KIND_CODE && chain.ignored(&ps, false)) {
                continue;
            }
            st.files_seen += 1;
            match file_info(&path) {
                Ok(info) => {
                    let r = store.lock().unwrap_or_else(|e| e.into_inner()).upsert_file(
                        &info.key,
                        &ps,
                        info.size,
                        info.mtime_ms,
                        kind,
                    );
                    match r {
                        Ok(_) => st.upserted += 1,
                        Err(_) => st.errors += 1,
                    }
                }
                Err(_) => st.errors += 1,
            }
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use winseek_store::IndexMeta;

    fn store() -> Mutex<Store> {
        let m = IndexMeta { model_id: "t".into(), llama_build: "b".into(), quant: "q".into(), image_budget: 280, dims: 4 };
        Mutex::new(Store::open_in_memory(&m).unwrap())
    }

    fn paths(s: &Mutex<Store>) -> Vec<String> {
        let mut p: Vec<String> = s.lock().unwrap().next_pending(100).unwrap().into_iter().map(|f| f.path).collect();
        p.sort();
        p
    }

    #[test]
    fn supported_extensions() {
        for ok in ["a.JPG", "a.jpeg", "x.png", "x.WebP", "b.bmp", "c.gif", r"C:\d.e\f.Png"] {
            assert!(is_supported_image(ok), "{ok}");
        }
        for no in ["a.heic", "a.txt", "png", "a.jpg.bak", "a."] {
            assert!(!is_supported_image(no), "{no}");
        }
    }

    #[test]
    fn attr_skipping() {
        assert!(skip_attrs(ATTR_REPARSE_POINT));
        assert!(skip_attrs(ATTR_HIDDEN | ATTR_SYSTEM));
        assert!(!skip_attrs(ATTR_HIDDEN));
        assert!(!skip_attrs(ATTR_SYSTEM));
        assert!(!skip_attrs(0x20));
    }

    #[test]
    fn crawl_respects_scope_and_extensions() {
        let d = crate::tier1::test_tmp();
        let r = d.path();
        for f in ["a.jpg", "b.exe", "sub/c.PNG", "sub/deep/d.gif", "skip/e.jpg", "node_modules/f.jpg", "g.heic"] {
            let p = r.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"x").unwrap();
        }
        let scopes = Scopes {
            include: vec![r.to_string_lossy().into_owned()],
            exclude: vec![r.join("skip").to_string_lossy().into_owned()],
        };
        let s = store();
        let st = crawl(&s, &scopes, &Ignore::defaults(), false, &|| false);
        assert_eq!(st.upserted, 3, "{st:?}");
        let got: Vec<String> = paths(&s).into_iter().map(|p| p.strip_prefix(&*r.to_string_lossy()).unwrap().to_string()).collect();
        assert_eq!(got, [r"\a.jpg", r"\sub\c.PNG", r"\sub\deep\d.gif"]);
        // re-crawl is idempotent
        crawl(&s, &scopes, &Ignore::defaults(), false, &|| false);
        assert_eq!(s.lock().unwrap().stats().unwrap().files, 3);
    }

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_of("a.JPG", false), Some(KIND_IMAGE));
        for doc in ["a.docx", "a.XLSX", "a.pptx", "a.odt", "a.txt", "a.md", "a.csv", "a.log", "a.rtf", "a.doc", "a.xls", "a.ppt"] {
            assert_eq!(kind_of(doc, false), Some(KIND_DOC), "{doc}");
        }
        for code in ["a.rs", "a.PY", "a.tsx", "a.json", "b.c"] {
            assert_eq!(kind_of(code, false), None, "{code} needs the opt-in");
            assert_eq!(kind_of(code, true), Some(KIND_CODE), "{code}");
        }
        for no in ["a.exe", "a.dll", "a", "app.min.js", "x.js.map", "package-lock.json", "a.heic"] {
            assert_eq!(kind_of(no, true), None, "{no}");
        }
    }

    #[test]
    fn crawl_picks_documents_always_and_code_only_when_opted_in_honouring_gitignore() {
        let d = crate::tier1::test_tmp();
        let r = d.path();
        let files = [
            ("notes.txt", "n"),
            ("deck.pptx", "n"),
            ("main.rs", "n"),
            ("gen/out.rs", "n"),
            ("gen/readme.md", "n"),
            ("sub/lib.rs", "n"),
            ("sub/skipme.rs", "n"),
            ("sub/keep.log", "n"),
            (".gitignore", "gen/\n*.log\n"),
            ("sub/.gitignore", "skipme.rs\n!keep.log\n"),
        ];
        for (f, c) in files {
            let p = r.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, c).unwrap();
        }
        let scopes = Scopes { include: vec![r.to_string_lossy().into_owned()], exclude: vec![] };
        let rel = |s: &Mutex<Store>| -> Vec<String> {
            paths(s).into_iter().map(|p| p.strip_prefix(&*r.to_string_lossy()).unwrap().trim_start_matches('\\').to_string()).collect()
        };
        let off = store();
        crawl(&off, &scopes, &Ignore::defaults(), false, &|| false);
        assert_eq!(rel(&off), [r"deck.pptx", r"gen\readme.md", r"notes.txt", r"sub\keep.log"]);

        let on = store();
        crawl(&on, &scopes, &Ignore::defaults(), true, &|| false);
        assert_eq!(rel(&on), [r"deck.pptx", r"gen\readme.md", r"main.rs", r"notes.txt", r"sub\keep.log", r"sub\lib.rs"]);
        // documents are never filtered by .gitignore; kinds are recorded
        let kinds: Vec<(String, String)> =
            on.lock().unwrap().next_pending(100).unwrap().into_iter().map(|f| (f.path.rsplit('\\').next().unwrap().to_string(), f.kind)).collect();
        assert!(kinds.contains(&("main.rs".into(), "code".into())));
        assert!(kinds.contains(&("notes.txt".into(), "doc".into())));
    }

    #[test]
    fn crawl_stops_on_request() {
        let d = crate::tier1::test_tmp();
        fs::write(d.path().join("a.jpg"), b"x").unwrap();
        let scopes = Scopes { include: vec![d.path().to_string_lossy().into_owned()], exclude: vec![] };
        let s = store();
        assert_eq!(crawl(&s, &scopes, &Ignore::defaults(), false, &|| true).upserted, 0);
    }
}
