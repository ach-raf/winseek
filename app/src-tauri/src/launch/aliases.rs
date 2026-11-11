//! Alias keywords: the names people TYPE for an app that its Start-menu title does not contain
//! (`word` -> Microsoft Word, `ps` -> Photoshop, `taskmgr` -> Task Manager). Built-in table +
//! user aliases from `LaunchCfg.aliases`. Applied once per catalog build, so search stays a pure
//! in-memory scan; an alias equal to the whole query ranks like an exact title (matcher.rs).

use std::collections::BTreeMap;

use super::item::{Kind, LaunchItem, Target};

/// (alias, needle): the alias is added to every item whose title or exe stem CONTAINS the needle
/// (both lower-cased). Keep needles specific enough not to catch unrelated apps.
const BUILTIN: &[(&str, &str)] = &[
    ("word", "microsoft word"),
    ("word", "winword"),
    ("excel", "microsoft excel"),
    ("powerpoint", "microsoft powerpoint"),
    ("ppt", "powerpnt"),
    ("outlook", "microsoft outlook"),
    ("onenote", "microsoft onenote"),
    ("code", "visual studio code"),
    ("vscode", "visual studio code"),
    ("vs", "visual studio"),
    ("ps", "photoshop"),
    ("ai", "illustrator"),
    ("pr", "premiere pro"),
    ("ae", "after effects"),
    ("lr", "lightroom"),
    ("id", "indesign"),
    ("chrome", "google chrome"),
    ("edge", "microsoft edge"),
    ("ff", "firefox"),
    ("browser", "chrome"),
    ("browser", "firefox"),
    ("browser", "msedge"),
    ("terminal", "windows terminal"),
    ("wt", "windows terminal"),
    ("cmd", "command prompt"),
    ("powershell", "windows powershell"),
    ("task manager", "taskmgr"),
    ("taskmgr", "task manager"),
    ("calc", "calculator"),
    ("paint", "mspaint"),
    ("snip", "snipping tool"),
    ("regedit", "registry editor"),
    ("services", "services"),
    ("devmgmt", "device manager"),
    ("diskmgmt", "disk management"),
    ("obs", "obs studio"),
    ("vlc", "vlc media player"),
    ("7z", "7-zip"),
    ("zip", "7-zip"),
    ("zip", "winrar"),
    ("git", "git bash"),
    ("pycharm", "pycharm"),
    ("idea", "intellij idea"),
    ("discord", "discord"),
    ("teams", "microsoft teams"),
    ("store", "microsoft store"),
    ("mail", "outlook"),
];

/// Add alias keywords to App items (settings/commands keep their curated keywords).
pub fn apply(items: &mut [LaunchItem], user: &BTreeMap<String, String>) {
    for item in items.iter_mut().filter(|i| i.kind == Kind::App) {
        let hay = haystack(item);
        for (alias, needle) in BUILTIN {
            if hay.contains(needle) && !has(item, alias) {
                item.keywords.push((*alias).to_string());
            }
        }
        for (alias, needle) in user {
            let (alias, needle) = (alias.trim(), needle.trim().to_lowercase());
            if !alias.is_empty() && !needle.is_empty() && (hay.contains(&needle) || item.id.to_lowercase() == needle) && !has(item, alias) {
                item.keywords.push(alias.to_string());
            }
        }
    }
}

fn has(item: &LaunchItem, kw: &str) -> bool {
    item.keywords.iter().any(|k| k.eq_ignore_ascii_case(kw))
}

/// Lower-cased title + exe stem (when the target has a file path).
fn haystack(item: &LaunchItem) -> String {
    let mut s = item.title.to_lowercase();
    let path = match &item.target {
        Target::Aumid { path, .. } => path.as_deref(),
        Target::Path { path, .. } => Some(path.as_str()),
        _ => None,
    };
    if let Some(p) = path {
        s.push('\u{1}');
        s.push_str(&super::appsrules::path_stem(p).to_lowercase());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(title: &str, path: &str) -> LaunchItem {
        LaunchItem {
            id: format!("app:{}", path.to_lowercase()),
            kind: Kind::App,
            title: title.into(),
            subtitle: "Start menu".into(),
            keywords: vec![],
            target: Target::Path { path: path.into(), args: None },
            icon: None,
            admin_ok: true,
        }
    }

    #[test]
    fn builtin_and_user_aliases() {
        let mut items = vec![app("Adobe Photoshop 2025", r"C:\A\Photoshop.exe"), app("Word", r"C:\O\WINWORD.EXE")];
        let mut user = BTreeMap::new();
        user.insert("photo".to_string(), "photoshop".to_string());
        apply(&mut items, &user);
        assert!(items[0].keywords.contains(&"ps".to_string()));
        assert!(items[0].keywords.contains(&"photo".to_string()));
        assert!(items[1].keywords.contains(&"word".to_string())); // via the exe stem
        assert!(!items[1].keywords.contains(&"ps".to_string()));
    }
}
