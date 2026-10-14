//! Dev convenience: take a pinned file from a local folder instead of downloading it.
//!
//! Import folders come from `WINSEEK_MODELS_IMPORT` (`;`-separated) and from
//! `<root>\models-import.txt` (one folder per line, `#` comments). A candidate must have the
//! manifest's file name, size (when pinned) and SHA-256; it is hard-linked into place when the
//! folders share a volume, copied otherwise (via `.part` + rename, so a crash never leaves a
//! truncated file).

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::fetch::sha256_file;

pub const ENV_IMPORT: &str = "WINSEEK_MODELS_IMPORT";
pub const IMPORT_FILE: &str = "models-import.txt";

/// How deep below an import folder to look (`D:\Tools\models\<repo>\<file>` is depth 1).
const MAX_DEPTH: u32 = 3;

/// Import folders from the env value and `<root>\models-import.txt`, existing dirs only, deduplicated.
pub fn import_dirs(env: Option<&OsStr>, root: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |s: &str| {
        let s = s.trim().trim_matches('"');
        if s.is_empty() || s.starts_with('#') {
            return;
        }
        let p = PathBuf::from(s);
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(e) = env {
        e.to_string_lossy().split(';').for_each(&mut add);
    }
    if let Some(r) = root {
        if let Ok(t) = fs::read_to_string(r.join(IMPORT_FILE)) {
            t.lines().for_each(&mut add);
        }
    }
    out
}

/// The folders configured for this process (env + `<root>\models-import.txt`).
pub fn configured_dirs(root: &Path) -> Vec<PathBuf> {
    import_dirs(std::env::var_os(ENV_IMPORT).as_deref(), Some(root))
}

/// Files named `name` (ASCII case-insensitive) under `dir`, up to [`MAX_DEPTH`] levels down.
fn find_named(dir: &Path, name: &str, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_file() && e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            out.push(e.path());
        } else if ft.is_dir() && depth < MAX_DEPTH {
            find_named(&e.path(), name, depth + 1, out);
        }
    }
}

/// Put a verified copy of `name` at `dest` from one of `dirs`. Returns true when `dest` now holds
/// the pinned file (also when it already did). Never errors: on any problem the caller downloads.
pub fn import_file(dirs: &[PathBuf], name: &str, size: Option<u64>, sha256: &str, dest: &Path) -> bool {
    if dirs.is_empty() || crate::manifest::is_unpinned(sha256) {
        return false;
    }
    let ok_hash = |p: &Path| sha256_file(p).is_ok_and(|h| h.eq_ignore_ascii_case(sha256));
    if dest.is_file() && size.is_none_or(|s| fs::metadata(dest).is_ok_and(|m| m.len() == s)) && ok_hash(dest) {
        return true;
    }
    let mut cands = Vec::new();
    for d in dirs {
        find_named(d, name, 0, &mut cands);
    }
    for c in cands {
        if size.is_some_and(|s| fs::metadata(&c).map_or(true, |m| m.len() != s)) || !ok_hash(&c) {
            continue;
        }
        if place(&c, dest).is_ok() {
            return true;
        }
    }
    false
}

/// Hard link `src` at `dest`, or copy it (through `dest.part`) when linking is not possible.
fn place(src: &Path, dest: &Path) -> std::io::Result<()> {
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let _ = fs::remove_file(dest);
    if fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    if let Err(e) = fs::copy(src, &part) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::sha256_hex;

    fn tmp(tag: &str) -> PathBuf {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("winseek-import-{tag}-{}-{t}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dirs_from_env_and_file() {
        let (a, b, root) = (tmp("a"), tmp("b"), tmp("root"));
        fs::write(root.join(IMPORT_FILE), format!("# dev models\n{}\n\n\"{}\"\nC:\\does\\not\\exist\n", b.display(), a.display())).unwrap();
        let env = format!("{};;{}", a.display(), "Z:\\nope");
        let d = import_dirs(Some(OsStr::new(&env)), Some(&root));
        assert_eq!(d, [a.clone(), b.clone()], "existing dirs, env first, no duplicates");
        assert!(import_dirs(None, None).is_empty());
        assert_eq!(import_dirs(None, Some(&root)), [b, a]);
    }

    #[test]
    fn imports_matching_file_only() {
        let src = tmp("src");
        let body = b"pinned model bytes".to_vec();
        let sha = sha256_hex(&body);
        // a same-named file with other content (like the BF16 repo's mmproj) must be skipped
        fs::create_dir_all(src.join("other")).unwrap();
        fs::write(src.join("other").join("m.gguf"), b"different bytes!!!").unwrap();
        fs::create_dir_all(src.join("repo")).unwrap();
        fs::write(src.join("repo").join("M.GGUF"), &body).unwrap();
        let dest = tmp("dest").join("models").join("m.gguf");
        let dirs = vec![src.clone()];
        assert!(import_file(&dirs, "m.gguf", Some(body.len() as u64), &sha, &dest));
        assert_eq!(fs::read(&dest).unwrap(), body);
        // already in place -> true without touching anything
        assert!(import_file(&dirs, "m.gguf", None, &sha, &dest));
        // wrong size, unknown name, unpinned hash, no dirs -> false
        let d2 = tmp("dest2").join("x.gguf");
        assert!(!import_file(&dirs, "m.gguf", Some(3), &sha, &d2));
        assert!(!import_file(&dirs, "nope.gguf", None, &sha, &d2));
        assert!(!import_file(&dirs, "m.gguf", None, "TODO-pin-in-CI", &d2));
        assert!(!import_file(&[], "m.gguf", None, &sha, &d2));
        assert!(!d2.exists());
    }

    #[test]
    fn too_deep_is_ignored() {
        let src = tmp("deep");
        let deep = src.join("1").join("2").join("3").join("4");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("m.gguf"), b"x").unwrap();
        let dest = tmp("dd").join("m.gguf");
        assert!(!import_file(&[src], "m.gguf", None, &sha256_hex(b"x"), &dest));
    }
}
