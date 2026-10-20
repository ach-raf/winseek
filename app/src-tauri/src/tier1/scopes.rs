//! Default Tier 1 scopes: the user's Pictures, Documents, Desktop and Downloads folders.

use std::collections::HashSet;
use std::path::PathBuf;

use windows::core::GUID;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Pictures, FOLDERID_Screenshots, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};
use winseek_store::Scopes;

pub(crate) fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: on success the returned PWSTR is a CoTaskMem allocation we free below.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// Existing known folders, in the order Pictures, Documents, Desktop, Downloads.
pub fn default_include() -> Vec<String> {
    [&FOLDERID_Pictures, &FOLDERID_Documents, &FOLDERID_Desktop, &FOLDERID_Downloads]
        .into_iter()
        .filter_map(known_folder)
        .filter(|p| p.is_dir())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// A scope root as it is stored and compared: backslash separators, no trailing separator.
/// Case is kept (paths are user-visible); comparisons fold it.
pub fn normalize_root(path: &str) -> String {
    path.trim().trim_matches('"').replace('/', "\\").trim_end_matches('\\').trim_end_matches('/').to_string()
}

/// Trim every root and drop empty or duplicate (case-insensitive) ones, keeping the first
/// occurrence. Duplicate roots would otherwise be crawled (and re-embedded) twice.
pub fn normalize(mut s: Scopes) -> Scopes {
    let clean = |list: &mut Vec<String>| {
        let mut seen: HashSet<String> = HashSet::new();
        list.retain_mut(|p| {
            *p = normalize_root(p);
            !p.is_empty() && seen.insert(p.to_ascii_lowercase())
        });
    };
    clean(&mut s.include);
    clean(&mut s.exclude);
    s
}

/// Saved scopes, or the defaults when nothing is saved (empty, never-configured `include`).
/// A configured empty list — every folder removed — is respected as "nothing is in scope".
pub fn effective(saved: Scopes) -> (Scopes, bool) {
    if saved.include.is_empty() && !saved.configured {
        (Scopes { include: default_include(), exclude: saved.exclude, configured: false }, true)
    } else {
        (saved, false)
    }
}

/// The user's known Screenshots folder (`Pictures\Screenshots` by default), if it resolves.
pub fn screenshots_dir() -> Option<String> {
    known_folder(&FOLDERID_Screenshots).map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_existing_dirs() {
        for p in default_include() {
            assert!(std::path::Path::new(&p).is_dir(), "{p}");
        }
    }

    #[test]
    fn saved_scopes_win() {
        let s = Scopes { include: vec![r"D:\x".into()], exclude: vec![r"D:\x\y".into()], configured: true };
        assert_eq!(effective(s.clone()), (s, false));
        let (e, used_default) = effective(Scopes { include: vec![], exclude: vec![r"E:\z".into()], configured: false });
        assert!(used_default);
        assert_eq!(e.exclude, vec![r"E:\z".to_string()]);
    }

    /// The 2026-10-19 live bug: after removing every folder the defaults resurrected on
    /// restart, because an empty list looked like "never configured".
    #[test]
    fn an_empty_list_that_was_configured_is_respected() {
        let s = Scopes { include: vec![], exclude: vec![], configured: true };
        assert_eq!(effective(s.clone()), (s, false));
    }

    #[test]
    fn roots_are_trimmed_and_deduplicated() {
        let s = normalize(Scopes {
            include: vec![r#" "D:\Photos\ "#.into(), r"D:/photos".into(), r"E:\\".into(), String::new()],
            exclude: vec![r"D:\Photos\raw/".into(), r"d:\photos\RAW".into()],
            ..Default::default()
        });
        assert_eq!(s.include, vec![r"D:\Photos".to_string(), r"E:".to_string()]);
        assert_eq!(s.exclude, vec![r"D:\Photos\raw".to_string()]);
    }
}
