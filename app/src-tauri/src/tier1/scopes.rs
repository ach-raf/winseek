//! Default Tier 1 scopes: the user's Pictures, Documents, Desktop and Downloads folders.

use std::path::PathBuf;

use windows::core::GUID;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Pictures, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};
use winseek_store::Scopes;

fn known_folder(id: &GUID) -> Option<PathBuf> {
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

/// Saved scopes, or the defaults when nothing is saved (empty `include`).
pub fn effective(saved: Scopes) -> (Scopes, bool) {
    if saved.include.is_empty() {
        (Scopes { include: default_include(), exclude: saved.exclude }, true)
    } else {
        (saved, false)
    }
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
        let s = Scopes { include: vec![r"D:\x".into()], exclude: vec![r"D:\x\y".into()] };
        assert_eq!(effective(s.clone()), (s, false));
        let (e, used_default) = effective(Scopes { include: vec![], exclude: vec![r"E:\z".into()] });
        assert!(used_default);
        assert_eq!(e.exclude, vec![r"E:\z".to_string()]);
    }
}
