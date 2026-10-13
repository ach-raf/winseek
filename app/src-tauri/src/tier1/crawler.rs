//! Initial / re-scope crawl: walk the include roots and queue supported image files.
//!
//! Skips reparse points (junctions, symlinks, OneDrive placeholders - reading those could
//! trigger a cloud download), hidden+system entries and anything `in_scope` rejects.

use std::path::Path;
use std::sync::Mutex;

use winseek_store::{in_scope, Scopes, Store};

use super::identity::{file_info, ATTR_DIRECTORY, ATTR_HIDDEN, ATTR_REPARSE_POINT, ATTR_SYSTEM};

pub const KIND_IMAGE: &str = "image";
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif"];

/// True for the image formats the worker can decode.
pub fn is_supported_image(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
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
pub fn crawl(store: &Mutex<Store>, scopes: &Scopes, stop: &dyn Fn() -> bool) -> CrawlStats {
    let mut st = CrawlStats::default();
    let mut stack: Vec<std::path::PathBuf> = scopes.include.iter().map(|r| r.trim_end_matches(['\\', '/']).into()).collect();
    // `C:` alone means the drive's current directory; keep the trailing slash for drive roots.
    for r in &mut stack {
        if r.as_os_str().len() == 2 && r.to_string_lossy().ends_with(':') {
            r.push("\\");
        }
    }
    while let Some(dir) = stack.pop() {
        if stop() {
            break;
        }
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
                if in_scope(scopes, &ps) {
                    stack.push(path);
                }
                continue;
            }
            if !is_supported_image(&ps) || !in_scope(scopes, &ps) {
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
                        KIND_IMAGE,
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
        for f in ["a.jpg", "b.txt", "sub/c.PNG", "sub/deep/d.gif", "skip/e.jpg", "node_modules/f.jpg", "g.heic"] {
            let p = r.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"x").unwrap();
        }
        let scopes = Scopes {
            include: vec![r.to_string_lossy().into_owned()],
            exclude: vec![r.join("skip").to_string_lossy().into_owned()],
        };
        let s = store();
        let st = crawl(&s, &scopes, &|| false);
        assert_eq!(st.upserted, 3, "{st:?}");
        let got: Vec<String> = paths(&s).into_iter().map(|p| p.strip_prefix(&*r.to_string_lossy()).unwrap().to_string()).collect();
        assert_eq!(got, [r"\a.jpg", r"\sub\c.PNG", r"\sub\deep\d.gif"]);
        // re-crawl is idempotent
        crawl(&s, &scopes, &|| false);
        assert_eq!(s.lock().unwrap().stats().unwrap().files, 3);
    }

    #[test]
    fn crawl_stops_on_request() {
        let d = crate::tier1::test_tmp();
        fs::write(d.path().join("a.jpg"), b"x").unwrap();
        let scopes = Scopes { include: vec![d.path().to_string_lossy().into_owned()], exclude: vec![] };
        let s = store();
        assert_eq!(crawl(&s, &scopes, &|| true).upserted, 0);
    }
}
