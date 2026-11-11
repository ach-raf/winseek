//! Pure rules over candidate app entries: uninstaller/helper detection, URL allowlist and
//! dedupe. No Win32 here so the rules stay unit-testable (apps.rs is the thin adapter).

use std::collections::HashMap;

use super::item::{LaunchItem, Target};

/// Exe stems that are never launch targets. Shared with the UI's portable-app rule in
/// `app/ui/src/launchLogic.ts` (`isHelperExe`) — keep both lists identical.
pub const HELPER_PATTERNS: &[&str] = &["setup", "-installer", "updater", "crashpad", "vc_redist"];

/// True when this candidate is an uninstaller, by title or by target path (PRD R2):
/// "uninstall" wording, `unins*` / `uninstall*` stems, or an .msi target.
pub fn is_uninstaller(title: &str, path: Option<&str>) -> bool {
    if title.to_lowercase().contains("uninstall") {
        return true;
    }
    let Some(p) = path else { return false };
    if p.to_lowercase().ends_with(".msi") {
        return true;
    }
    let stem = path_stem(p).to_lowercase();
    stem.starts_with("unins") || stem.starts_with("uninstall")
}

/// True when an exe stem is an installer/update/crash-helper program, never an app (PRD R9).
pub fn is_helper_exe(stem: &str) -> bool {
    let s = stem.to_lowercase();
    if s.is_empty() {
        return false;
    }
    HELPER_PATTERNS.iter().any(|p| match *p {
        "setup" => s.starts_with(p) || s.contains(" setup"),
        "crashpad" | "vc_redist" => s.starts_with(p),
        "-installer" => s.ends_with(p),
        "updater" => s.contains(p),
        _ => false,
    })
}

/// `.url`/link entries are only kept for these store-run protocols (Flow's Program plugin rule).
pub fn allowed_url(uri: &str) -> bool {
    let u = uri.to_lowercase();
    u.starts_with("steam://run/") || u.starts_with("steam://rungameid/") || u.starts_with("com.epicgames.launcher://apps/")
}

/// Dedupe by lower-case resolved target (+ args), else AUMID. Input order is the source
/// priority (AppsFolder > App Paths > folders > PATH), so the first entry per key wins;
/// within one source the shorter title replaces the earlier one (keeping the first id).
pub fn dedupe(items: Vec<LaunchItem>) -> Vec<LaunchItem> {
    let mut out: Vec<LaunchItem> = Vec::new();
    let mut first_at: HashMap<String, usize> = HashMap::new();
    for mut item in items {
        let Some(key) = dedupe_key(&item) else {
            out.push(item);
            continue;
        };
        match first_at.get(&key).copied() {
            None => {
                first_at.insert(key, out.len());
                out.push(item);
            }
            Some(i) => {
                // Same source (same subtitle) and a shorter label: swap the text, keep the id.
                if out[i].subtitle == item.subtitle
                    && item.title.chars().count() < out[i].title.chars().count()
                {
                    item.id = out[i].id.clone();
                    out[i] = item;
                }
            }
        }
    }
    out
}

pub(crate) fn dedupe_key(item: &LaunchItem) -> Option<String> {
    match &item.target {
        Target::Path { path, args } => Some(format!("p:{}|{}", path.to_lowercase(), args.as_deref().unwrap_or(""))),
        Target::Aumid { aumid, path } => match path {
            Some(p) => Some(format!("p:{}|", p.to_lowercase())),
            None => Some(format!("aumid:{}", aumid.to_lowercase())),
        },
        _ => None,
    }
}

pub(crate) fn path_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::item::Kind;

    fn app(id: &str, title: &str, subtitle: &str, path: Option<&str>) -> LaunchItem {
        LaunchItem {
            id: id.into(),
            kind: Kind::App,
            title: title.into(),
            subtitle: subtitle.into(),
            keywords: vec![],
            target: match path {
                Some(p) => Target::Path { path: p.into(), args: None },
                None => Target::Aumid { aumid: id.into(), path: None },
            },
            icon: None,
            admin_ok: path.is_some(),
        }
    }

    #[test]
    fn uninstaller_rules() {
        assert!(is_uninstaller("Uninstall Firefox", Some(r"C:\x\helper.exe")));
        assert!(is_uninstaller("Firefox", Some(r"C:\x\unins000.exe")));
        assert!(is_uninstaller("Firefox", Some(r"C:\x\uninstall.exe")));
        assert!(is_uninstaller("Some app", Some(r"C:\x\setup.msi")));
        assert!(!is_uninstaller("Firefox", Some(r"C:\x\firefox.exe")));
        assert!(!is_uninstaller("Firefox", None));
    }

    #[test]
    fn helper_exe_rules() {
        assert!(is_helper_exe("crashpad_handler"));
        assert!(is_helper_exe("vc_redist")); // vc_redist.x64.exe
        assert!(is_helper_exe("myapp-updater"));
        assert!(is_helper_exe("SomeApp-installer"));
        assert!(is_helper_exe("Firefox Setup")); // "Firefox Setup.exe"
        assert!(is_helper_exe("setup64"));
        assert!(!is_helper_exe("mpv"));
        assert!(!is_helper_exe("chrome"));
    }

    #[test]
    fn url_allowlist() {
        assert!(allowed_url("steam://run/570"));
        assert!(allowed_url("steam://rungameid/730"));
        assert!(allowed_url("com.epicgames.launcher://apps/Fortnite?action=play"));
        assert!(!allowed_url("https://example.com/app.url"));
        assert!(!allowed_url("msedge://some"));
    }

    #[test]
    fn dedupe_keeps_source_priority_and_first_id() {
        let start_menu = app("app:chrome-lnk", "Google Chrome", "Start menu", Some(r"C:\Program Files\Google\chrome.exe"));
        let app_paths = app("app:c:\\program files\\google\\chrome.exe", "Chrome", "App Paths", Some(r"C:\Program Files\Google\chrome.exe"));
        let store = app("app:store-app!app", "Store App", "Store app", None);
        let dup_store = app("app:store-app!app", "Store App Alias", "Store app", None);
        let out = dedupe(vec![start_menu, app_paths, store, dup_store]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "app:chrome-lnk"); // Start menu wins over App Paths
        assert_eq!(out[0].title, "Google Chrome");
        assert_eq!(out[1].id, "app:store-app!app"); // same AUMID: first id + first text kept
        assert_eq!(out[1].title, "Store App");
    }

    #[test]
    fn same_source_shorter_title_replaces() {
        let long = app("app:mpv-a", "mpv media player portable build", "D:\\tools", Some(r"D:\tools\mpv.exe"));
        let short = app("app:mpv-b", "mpv", "D:\\tools", Some(r"D:\tools\mpv.exe"));
        let out = dedupe(vec![long, short]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "mpv");
        assert_eq!(out[0].id, "app:mpv-a"); // first id survives the swap
    }
}
