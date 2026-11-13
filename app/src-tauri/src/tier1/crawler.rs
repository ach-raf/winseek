//! Initial / re-scope crawl: walk the include roots and queue supported files (images, Office /
//! text documents and, when opted in, source code).
//!
//! Skips reparse points (junctions, symlinks, OneDrive placeholders - reading those could
//! trigger a cloud download), hidden+system entries and anything `in_scope_with` rejects.

use std::path::Path;
use std::sync::Mutex;

use winseek_store::{in_scope_with, Ignore, Scopes, Store};

use super::gitignore::Chain;
use super::guard;
use super::identity::{file_info, ATTR_DIRECTORY, ATTR_HIDDEN, ATTR_REPARSE_POINT, ATTR_SYSTEM};

pub const KIND_IMAGE: &str = "image";
/// Office / OpenDocument / plain text / RTF (and legacy Office, which is recorded as skipped).
pub const KIND_DOC: &str = "doc";
/// PDF documents (pdfium text layer, scans rendered + OCR-ed).
pub const KIND_PDF: &str = "pdf";
/// Source code; only crawled when the `index_code` setting is on.
pub const KIND_CODE: &str = "code";
/// Audio files: ~30 s windows embedded from the waveform (needs the ffmpeg sidecar).
pub const KIND_AUDIO: &str = "audio";
/// Video files: one frame per shot plus the soundtrack windows (needs the ffmpeg sidecar).
pub const KIND_VIDEO: &str = "video";
const AUDIO_EXTS: &[&str] = &["mp3", "wav", "flac", "m4a", "aac", "ogg", "oga", "opus", "wma"];
// No `ts` (TypeScript wins that extension).
const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mkv", "mov", "avi", "webm", "wmv", "flv", "mpg", "mpeg", "mts", "m2ts"];
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif", "heic", "heif"];
const DOC_EXTS: &[&str] = &[
    "docx", "docm", "dotx", "dotm", "xlsx", "xlsm", "xltx", "xltm", "pptx", "pptm", "ppsx", "potx", "odt", "ott", "ods", "ots", "odp", "otp",
    "doc", "dot", "xls", "xlt", "ppt", "pps", "pot", "rtf", "txt", "md", "markdown", "csv", "tsv", "log",
];
const CODE_EXTS: &[&str] = &[
    "rs", "py", "pyi", "js", "jsx", "mjs", "cjs", "ts", "tsx", "java", "kt", "kts", "scala", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "cs", "go",
    "rb", "php", "swift", "m", "mm", "dart", "lua", "r", "jl", "ex", "exs", "erl", "hs", "ml", "clj", "sh", "bash", "zsh", "ps1", "psm1", "bat",
    "cmd", "sql", "html", "htm", "css", "scss", "sass", "less", "vue", "svelte", "json", "toml", "yaml", "yml", "xml", "ini", "cfg", "gradle",
    "cmake", "proto", "graphql", "tf", "ipynb",
];
/// Prose that lives next to code; it joins the code index when its folder is a code project.
const CODE_PROSE_EXTS: &[&str] = &["md", "markdown", "txt", "rst"];

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

/// Which Tier 1 reader a path belongs to: `image`, `pdf`, `doc`, or `code` (only when `index_code`).
/// Source files that are minified / generated / vendored / secret-looking are never `code`.
pub fn kind_of(path: &str, index_code: bool) -> Option<&'static str> {
    if guard::is_temp_name(path) {
        None
    } else if is_supported_image(path) {
        Some(KIND_IMAGE)
    } else if ext_in(path, &["pdf"]) {
        Some(KIND_PDF)
    } else if ext_in(path, DOC_EXTS) {
        Some(KIND_DOC)
    } else if ext_in(path, VIDEO_EXTS) {
        Some(KIND_VIDEO)
    } else if ext_in(path, AUDIO_EXTS) {
        Some(KIND_AUDIO)
    } else if index_code && ext_in(path, CODE_EXTS) && !guard::skip_code_file(path) {
        Some(KIND_CODE)
    } else {
        None
    }
}

/// [`kind_of`] knowing whether the file sits inside a code project.
///
/// In a project only screenshots are read (their text is the point; photos and documents there
/// are build assets), and only with `index_code` is anything else read: source, notebooks and the
/// project's prose go to the separate code index. Outside a project nothing changes.
pub fn kind_in(path: &str, index_code: bool, in_project: bool) -> Option<&'static str> {
    if !in_project {
        return kind_of(path, index_code);
    }
    if is_supported_image(path) {
        return super::ocr::is_screenshot(path, None, None).then_some(KIND_IMAGE);
    }
    if !index_code || guard::skip_code_file(path) {
        return None;
    }
    (ext_in(path, CODE_EXTS) || ext_in(path, CODE_PROSE_EXTS)).then_some(KIND_CODE)
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
    // (directory, .gitignore chain, inside a code project, is an include root)
    let mut stack: Vec<(std::path::PathBuf, Chain, bool, bool)> = scopes
        .include
        .iter()
        .map(|r| {
            let mut p = std::path::PathBuf::from(r.trim_end_matches(['\\', '/']));
            // `C:` alone means the drive's current directory; keep the trailing slash for drive roots.
            if p.as_os_str().len() == 2 && p.to_string_lossy().ends_with(':') {
                p.push("\\");
            }
            // A root inside a project (D:\proj\docs) is project content too.
            let in_project = guard::in_code_project(&format!("{}\\x", p.to_string_lossy().trim_end_matches('\\')));
            (p, Chain::default(), in_project, true)
        })
        .collect();
    while let Some((dir, parent_chain, parent_in_project, is_root)) = stack.pop() {
        if stop() {
            break;
        }
        let chain = if index_code { parent_chain.descend(&dir) } else { parent_chain };
        let Ok(rd) = std::fs::read_dir(&dir) else {
            st.errors += 1;
            continue;
        };
        st.dirs += 1;
        use std::os::windows::fs::MetadataExt;
        // One pass over the directory: entry names first (project marker, dataset shape), then the work.
        let mut entries: Vec<(std::path::PathBuf, u32)> = Vec::new();
        for entry in rd.flatten() {
            if stop() {
                return st;
            }
            match entry.metadata() {
                Ok(md) => entries.push((entry.path(), md.file_attributes())),
                Err(_) => st.errors += 1,
            }
        }
        let name_of = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (mut files, mut dirs, mut all): (Vec<String>, Vec<String>, Vec<String>) = (vec![], vec![], vec![]);
        for (p, attrs) in &entries {
            let n = name_of(p);
            all.push(n.clone());
            if attrs & ATTR_DIRECTORY != 0 {
                dirs.push(n);
            } else {
                files.push(n);
            }
        }
        // Dataset folders are noise; a root the user chose explicitly is exempt (like the ignore list).
        if !is_root && guard::looks_like_dataset(&name_of(&dir), &files, &dirs) {
            continue;
        }
        let in_project = parent_in_project || guard::is_project_root(&all);
        for (path, attrs) in entries {
            if stop() {
                return st;
            }
            if skip_attrs(attrs) || guard::is_cloud_only(attrs) {
                continue;
            }
            let ps = path.to_string_lossy();
            if attrs & ATTR_DIRECTORY != 0 {
                if in_scope_with(scopes, ignore, &ps) {
                    stack.push((path, chain.clone(), in_project, false));
                }
                continue;
            }
            let Some(kind) = kind_in(&ps, index_code, in_project) else { continue };
            if !in_scope_with(scopes, ignore, &ps) || (kind == KIND_CODE && chain.ignored(&ps, false)) {
                continue;
            }
            st.files_seen += 1;
            match file_info(&path) {
                Ok(info) => {
                    let r = store.lock().unwrap_or_else(|e| e.into_inner()).upsert_file(&info.key, &ps, info.size, info.mtime_ms, kind);
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

/// Skip reasons worth a second look later: the file may have been hydrated / the codec installed.
pub const RETRY_SKIPS: &[&str] = &["cloud_only", "no_heic_codec", "still_writing"];

/// Remove rows indexed before the current rules existed: out of scope or hard-denied, not readable
/// under the code-project rules, or inside a dataset-looking folder. Returns the rows removed.
/// Nothing is purged when there is no include root (a transient empty scope must not wipe the index).
pub fn purge_stale(store: &Mutex<Store>, scopes: &Scopes, ignore: &Ignore, index_code: bool) -> usize {
    use std::collections::HashMap;
    if scopes.include.is_empty() {
        return 0;
    }
    let rows = store.lock().unwrap_or_else(|e| e.into_inner()).all_file_paths().unwrap_or_default();
    let parent_of = |p: &str| p.rsplit_once('\\').map(|x| x.0.to_string()).unwrap_or_default();
    let mut per_dir: HashMap<String, usize> = HashMap::new();
    for (p, _) in &rows {
        *per_dir.entry(parent_of(p).to_lowercase()).or_default() += 1;
    }
    let roots: Vec<String> = scopes.include.iter().map(|r| r.trim_end_matches(['\\', '/']).to_lowercase()).collect();
    let mut dataset: HashMap<String, bool> = HashMap::new();
    let mut project: HashMap<String, bool> = HashMap::new();
    let mut gone: Vec<String> = Vec::new();
    for (path, kind) in &rows {
        let dir = parent_of(path).to_lowercase();
        let in_project = *project.entry(dir.clone()).or_insert_with(|| guard::in_code_project(path));
        let mut drop_it = !in_scope_with(scopes, ignore, path) || (kind != KIND_CODE && kind_in(path, index_code, in_project).is_none());
        if !drop_it && kind == KIND_IMAGE && per_dir[&dir] >= 20 && !roots.contains(&dir) {
            drop_it = *dataset.entry(dir.clone()).or_insert_with(|| dir_is_dataset(&parent_of(path)));
        }
        if !drop_it && kind == KIND_CODE {
            drop_it = !index_code || kind_in(path, true, in_project) != Some(KIND_CODE);
        }
        if drop_it {
            gone.push(path.clone());
        }
    }
    let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
    gone.iter().filter(|p| s.remove_path(p).unwrap_or(false)).count()
}

fn dir_is_dataset(dir: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            dirs.push(n);
        } else {
            files.push(n);
        }
    }
    let name = dir.rsplit('\\').next().unwrap_or(dir);
    guard::looks_like_dataset(name, &files, &dirs)
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
        let mut p: Vec<String> = s.lock().unwrap().next_pending(5000).unwrap().into_iter().map(|f| f.path).collect();
        p.sort();
        p
    }

    #[test]
    fn supported_extensions() {
        for ok in ["a.JPG", "a.jpeg", "x.png", "x.WebP", "b.bmp", "c.gif", "p.HEIC", "p.heif", r"C:\d.e\f.Png"] {
            assert!(is_supported_image(ok), "{ok}");
        }
        for no in ["a.txt", "png", "a.jpg.bak", "a.heic.bak", "a."] {
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
            ..Default::default()
        };
        let s = store();
        let st = crawl(&s, &scopes, &Ignore::defaults(), false, &|| false);
        assert_eq!(st.upserted, 4, "{st:?}");
        let got: Vec<String> = paths(&s).into_iter().map(|p| p.strip_prefix(&*r.to_string_lossy()).unwrap().to_string()).collect();
        assert_eq!(got, [r"\a.jpg", r"\g.heic", r"\sub\c.PNG", r"\sub\deep\d.gif"]);
        // re-crawl is idempotent
        crawl(&s, &scopes, &Ignore::defaults(), false, &|| false);
        assert_eq!(s.lock().unwrap().stats().unwrap().files, 4);
    }

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_of("a.JPG", false), Some(KIND_IMAGE));
        assert_eq!(kind_of("IMG_1.HEIC", false), Some(KIND_IMAGE), "decoded through the Windows codec");
        for doc in ["a.docx", "a.XLSX", "a.pptx", "a.odt", "a.txt", "a.md", "a.csv", "a.log", "a.rtf", "a.doc", "a.xls", "a.ppt"] {
            assert_eq!(kind_of(doc, false), Some(KIND_DOC), "{doc}");
        }
        assert_eq!(kind_of("Invoice.PDF", false), Some(KIND_PDF));
        assert_eq!(kind_of(r"D:\Media\Trip.MKV", false), Some(KIND_VIDEO));
        assert_eq!(kind_of("memo.m4a", false), Some(KIND_AUDIO));
        assert_eq!(kind_of("app.ts", false), None, "ts stays TypeScript, never video");
        assert_eq!(kind_of("a.pdf", true), Some(KIND_PDF));
        for code in ["a.rs", "a.PY", "a.tsx", "a.json", "b.c"] {
            assert_eq!(kind_of(code, false), None, "{code} needs the opt-in");
            assert_eq!(kind_of(code, true), Some(KIND_CODE), "{code}");
        }
        for no in ["a.exe", "a.dll", "a", "app.min.js", "x.js.map", "package-lock.json", "vendor/x.go"] {
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
        let scopes = Scopes { include: vec![r.to_string_lossy().into_owned()], exclude: vec![], ..Default::default() };
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
    fn kind_in_a_code_project_reads_only_screenshots_unless_code_search_is_on() {
        let p = r"D:\w\proj";
        for off in [false, true] {
            assert_eq!(kind_in(&format!(r"{p}\Screenshot 2024-01-01.png"), off, true), Some(KIND_IMAGE), "screenshot, code {off}");
            assert_eq!(kind_in(&format!(r"{p}\assets\hero.jpg"), off, true), None, "photo, code {off}");
            assert_eq!(kind_in(&format!(r"{p}\spec.pdf"), off, true), None, "pdf, code {off}");
            assert_eq!(kind_in(&format!(r"{p}\clip.mp4"), off, true), None, "video, code {off}");
        }
        assert_eq!(kind_in(&format!(r"{p}\src\main.rs"), false, true), None);
        assert_eq!(kind_in(&format!(r"{p}\notes.docx"), false, true), None);
        assert_eq!(kind_in(&format!(r"{p}\src\main.rs"), true, true), Some(KIND_CODE));
        assert_eq!(kind_in(&format!(r"{p}\README.md"), true, true), Some(KIND_CODE), "project prose joins the code index");
        assert_eq!(kind_in(&format!(r"{p}\a.ipynb"), true, true), Some(KIND_CODE));
        assert_eq!(kind_in(&format!(r"{p}\notes.docx"), true, true), None);
        assert_eq!(kind_in(&format!(r"{p}\vendor\x.go"), true, true), None);
        assert_eq!(kind_in(&format!(r"{p}\.env"), true, true), None);
        // outside a project nothing changes
        assert_eq!(kind_in(r"D:\w\Photos\a.jpg", false, false), Some(KIND_IMAGE));
        assert_eq!(kind_in(r"D:\w\Docs\notes.docx", false, false), Some(KIND_DOC));
    }

    fn rel_paths(s: &Mutex<Store>, root: &Path) -> Vec<String> {
        paths(s).into_iter().map(|p| p.strip_prefix(&*root.to_string_lossy()).unwrap().trim_start_matches('\\').replace('\\', "/")).collect()
    }

    #[test]
    fn crawl_inside_a_project_skips_photos_and_docs_but_keeps_screenshots() {
        let d = crate::tier1::test_tmp();
        let r = d.path();
        for (f, c) in [
            ("proj/Cargo.toml", "[package]"),
            ("proj/src/main.rs", "fn main(){}"),
            ("proj/docs/Screenshot 1.png", "x"),
            ("proj/docs/photo.jpg", "x"),
            ("proj/docs/spec.docx", "x"),
            ("Photos/trip.jpg", "x"),
            ("Docs/spec.docx", "x"),
        ] {
            let p = r.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, c).unwrap();
        }
        let scopes = Scopes { include: vec![r.to_string_lossy().into_owned()], exclude: vec![], ..Default::default() };
        let off = store();
        crawl(&off, &scopes, &Ignore::defaults(), false, &|| false);
        assert_eq!(rel_paths(&off, r), ["Docs/spec.docx", "Photos/trip.jpg", "proj/docs/Screenshot 1.png"]);
        let on = store();
        crawl(&on, &scopes, &Ignore::defaults(), true, &|| false);
        assert_eq!(rel_paths(&on, r), ["Docs/spec.docx", "Photos/trip.jpg", "proj/Cargo.toml", "proj/docs/Screenshot 1.png", "proj/src/main.rs"]);
        // a root inside a project is project content too
        let inner = Scopes { include: vec![r.join("proj").join("docs").to_string_lossy().into_owned()], exclude: vec![], ..Default::default() };
        let s = store();
        crawl(&s, &inner, &Ignore::defaults(), false, &|| false);
        assert_eq!(rel_paths(&s, r), ["proj/docs/Screenshot 1.png"]);
    }

    #[test]
    fn crawl_skips_dataset_folders_and_secret_names_even_with_an_empty_ignore_list() {
        let d = crate::tier1::test_tmp();
        let r = d.path();
        let mut files: Vec<String> =
            ["Photos/trip.jpg", "keys/server.pem", "keys/notes.txt", ".ssh/readme.txt", "datasets/a.jpg", "faces/labels.csv"].iter().map(|s| s.to_string()).collect();
        files.extend((0..25).map(|i| format!("faces/p{i}.jpg")));
        files.extend((0..220).map(|i| format!("frames/{i:06}.jpg")));
        files.extend((0..220).map(|i| format!("camera/IMG_{i:04}.jpg")));
        for f in &files {
            let p = r.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"x").unwrap();
        }
        let scopes = Scopes { include: vec![r.to_string_lossy().into_owned()], exclude: vec![], ..Default::default() };
        let s = store();
        crawl(&s, &scopes, &Ignore::new::<&str>(&[]), false, &|| false);
        let got = rel_paths(&s, r);
        assert!(got.contains(&"Photos/trip.jpg".to_string()), "{} of {got:?}", got.len());
        assert!(got.contains(&"keys/notes.txt".to_string()), "only the key file is denied");
        assert!(
            !got.iter().any(|p| p.ends_with(".pem") || p.starts_with(".ssh") || p.starts_with("datasets/") || p.starts_with("faces/") || p.starts_with("frames/")),
            "{got:?}"
        );
        assert_eq!(got.iter().filter(|p| p.starts_with("camera/")).count(), 220, "a camera roll is not a dataset");
        // the user's own include root is exempt from the dataset rule
        let explicit = Scopes { include: vec![r.join("datasets").to_string_lossy().into_owned()], exclude: vec![], ..Default::default() };
        let e = store();
        crawl(&e, &explicit, &Ignore::new::<&str>(&[]), false, &|| false);
        assert_eq!(rel_paths(&e, r), ["datasets/a.jpg"]);
    }

    #[test]
    fn purge_removes_rows_that_break_todays_rules_and_keeps_the_rest() {
        let d = crate::tier1::test_tmp();
        let r = d.path();
        for (f, c) in [("Photos/trip.jpg", "x"), ("keys/server.pem", "x"), ("proj/package.json", "{}"), ("proj/img/photo.jpg", "x"), ("proj/img/Screenshot 1.png", "x")] {
            let p = r.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, c).unwrap();
        }
        let s = store();
        {
            // rows as an older build would have written them
            let mut g = s.lock().unwrap();
            for (n, f) in ["Photos/trip.jpg", "keys/server.pem", "proj/img/photo.jpg", "proj/img/Screenshot 1.png"].iter().enumerate() {
                let p = r.join(f).to_string_lossy().replace('/', "\\");
                let key = winseek_store::FileKey { vol_guid: "v".into(), frn: n as u64 + 1 };
                g.upsert_file(&key, &p, 1, 1, "image").unwrap();
            }
        }
        let scopes = Scopes { include: vec![r.to_string_lossy().into_owned()], exclude: vec![], ..Default::default() };
        assert_eq!(purge_stale(&s, &scopes, &Ignore::new::<&str>(&[]), false), 2);
        assert_eq!(rel_paths(&s, r), ["Photos/trip.jpg", "proj/img/Screenshot 1.png"]);
        let none = Scopes::default();
        assert_eq!(purge_stale(&s, &none, &Ignore::defaults(), false), 0, "an empty scope never wipes the index");
    }

    #[test]
    fn crawl_stops_on_request() {
        let d = crate::tier1::test_tmp();
        fs::write(d.path().join("a.jpg"), b"x").unwrap();
        let scopes = Scopes { include: vec![d.path().to_string_lossy().into_owned()], ..Default::default() };
        let s = store();
        assert_eq!(crawl(&s, &scopes, &Ignore::defaults(), false, &|| true).upserted, 0);
    }
}
