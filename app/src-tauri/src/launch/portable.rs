//! Portable programs: `.exe` files that have no Start-menu entry or registry record (unzipped
//! tools, `D:\Tools\foo\foo.exe`, game launchers, scoop apps...). The Tier 0 name index already
//! knows every exe on every volume, so one `ext:exe` sweep feeds this module, which keeps only
//! what looks like a *program a person launches* and turns it into catalog items. They then
//! rank through the same matcher / frecency / alias pipeline as Start-menu apps.
//!
//! `select` is pure (unit-tested); `scan` is the thin searchd adapter.

use std::collections::HashMap;

use super::appsrules::{is_helper_exe, is_uninstaller, path_stem};
use super::item::{Kind, LaunchItem, Target};
use crate::source::{Hit, NameSource};

/// Hard cap on catalog growth: a machine with 100k stray exes must not slow every keystroke.
const MAX_ITEMS: usize = 6000;
/// A folder with more exes than this is a tool suite / bin dir: keep only its namesake + largest.
const SUITE: usize = 8;
/// Below this size an exe is a stub/helper unless it carries its folder's name.
const MIN_SIZE: u64 = 100 * 1024;
const PAGE: usize = 10_000;
const MAX_PAGES: usize = 60;

/// Path components that never hold a launchable program (lower-case compare).
const SKIP_DIRS: &[&str] = &[
    "windows", "winsxs", "system32", "syswow64", "$recycle.bin", "$windows.~bt", "windows.old", "system volume information",
    "node_modules", ".git", ".hg", ".svn", "site-packages", "__pycache__", "venv", ".venv", "temp", "tmp", "cache",
    "caches", "driverstore", "windowsapps", "programdata", "installer", "$sysreset", "deps", "incremental", ".cargo",
    ".rustup", ".gradle", ".nuget", ".vs", "wine",
];
/// `target\debug`-style build output (cargo/cmake/dotnet) is not a "program" the user installed.
const BUILD_PAIRS: &[(&str, &str)] = &[("target", "debug"), ("target", "release"), ("bin", "debug"), ("bin", "release"), ("obj", "debug"), ("obj", "release")];
/// Folder names that say nothing about the app; the real name is one level up.
const GENERIC_DIRS: &[&str] = &["bin", "x64", "x86", "win64", "win32", "app", "release", "portable", "program", "application", "binaries", "windows"];

/// Program Files / Start-menu territory is already covered by the catalog sources.
fn in_installed_tree(lower: &str) -> bool {
    lower.contains("\\program files") || lower.contains("\\appdata\\local\\programs\\") || lower.contains("\\appdata\\local\\microsoft\\")
}

fn skip_path(lower: &str) -> bool {
    if in_installed_tree(lower) {
        return true;
    }
    let comps: Vec<&str> = lower.split('\\').collect();
    let dirs = &comps[..comps.len().saturating_sub(1)];
    if dirs.iter().skip(1).any(|c| SKIP_DIRS.contains(c)) {
        return true;
    }
    dirs.windows(2).any(|w| BUILD_PAIRS.iter().any(|(a, b)| w[0] == *a && w[1] == *b))
}

/// Folder names of toolchain / SDK / build internals: their exes are kept (they are real) but
/// must never outrank what a person actually launches. Compared per path component, lower-case.
const JUNK_COMPS: &[&str] = &[
    "appxpackage", "msbuild", "winsdk", "windows kits", "visualstudio", "vs_studio", "_backups", "backup", "backups", "redist",
    "sdk", "sdks", "toolchains", "buildtools", "build tools",
];

/// True when any directory component of `path` (any case) is toolchain / SDK / backup territory.
pub fn is_junk_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.split('\\').any(|c| JUNK_COMPS.contains(&c) || c.starts_with("microsoft.visualstudio") || c.starts_with("windows kits"))
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn parent_names(path: &str) -> (String, Option<String>) {
    let mut it = path.rsplit('\\').skip(1);
    let parent = it.next().unwrap_or("").to_string();
    // Skip generic folders (`bin`, `x64`) to find the app's real folder name.
    let mut name = parent.clone();
    let mut up = None;
    if GENERIC_DIRS.contains(&parent.to_lowercase().as_str()) {
        if let Some(g) = it.next() {
            up = Some(g.to_string());
            name = g.to_string();
        }
    }
    let _ = parent;
    (name, up)
}

/// The exe is "the" program of its folder when its stem and the folder name contain each other.
fn namesake(stem: &str, folder: &str) -> bool {
    let (a, b) = (norm(stem), norm(folder));
    a.len() >= 3 && b.len() >= 3 && (a.contains(&b) || b.contains(&a))
}

struct Cand<'a> {
    hit: &'a Hit,
    stem: String,
    folder: String,
    extra: Option<String>,
    lower_dir: String,
    namesake: bool,
}

/// Index hits (any mix of files) -> launchable portable programs, best first.
pub fn select(hits: &[Hit]) -> Vec<LaunchItem> {
    let mut by_dir: HashMap<String, Vec<Cand>> = HashMap::new();
    for h in hits {
        if h.is_dir || !h.name.to_lowercase().ends_with(".exe") {
            continue;
        }
        let lower = h.path.to_lowercase();
        if skip_path(&lower) {
            continue;
        }
        let stem = path_stem(&h.name);
        if stem.is_empty() || is_helper_exe(&stem) || is_uninstaller(&stem, Some(&h.path)) {
            continue;
        }
        let (folder, extra) = parent_names(&h.path);
        let ns = namesake(&stem, &folder) || extra.as_deref().is_some_and(|e| namesake(&stem, e));
        // Tiny exes are stubs and helpers unless they are their folder's program.
        if !ns && h.size.is_some_and(|s| s < MIN_SIZE) {
            continue;
        }
        let dir = lower.rsplit_once('\\').map(|(d, _)| d.to_string()).unwrap_or_default();
        by_dir.entry(dir.clone()).or_default().push(Cand { hit: h, stem, folder, extra, lower_dir: dir, namesake: ns });
    }

    let mut keep: Vec<Cand> = Vec::new();
    for (_, mut group) in by_dir {
        if group.len() > SUITE {
            group.sort_by_key(|c| std::cmp::Reverse(c.hit.size));
            let mut largest = 0;
            group.retain(|c| {
                if c.namesake {
                    return true;
                }
                largest += 1;
                largest <= 2
            });
        }
        keep.extend(group);
    }

    // Every copy of a program is a real program: duplicates are kept (the subtitle carries the
    // full path to tell them apart). Only the sanity cap trims: namesakes first (the real
    // programs), non-junk before tool/SDK junk, then newest, so the cap never eats those.
    keep.sort_by(|a, b| {
        b.namesake
            .cmp(&a.namesake)
            .then(is_junk_path(&a.lower_dir).cmp(&is_junk_path(&b.lower_dir)))
            .then(b.hit.mtime.cmp(&a.hit.mtime))
            .then(a.lower_dir.len().cmp(&b.lower_dir.len()))
    });
    keep.truncate(MAX_ITEMS);

    keep.into_iter().map(|c| to_item(&c)).collect()
}

fn to_item(c: &Cand) -> LaunchItem {
    let mut keywords = vec![c.stem.clone()];
    for k in [Some(&c.folder), c.extra.as_ref()].into_iter().flatten() {
        if !k.is_empty() && !k.eq_ignore_ascii_case(&c.stem) && !keywords.iter().any(|x| x.eq_ignore_ascii_case(k)) {
            keywords.push(k.clone());
        }
    }
    LaunchItem {
        id: format!("app:{}", c.hit.path.to_lowercase()),
        kind: Kind::App,
        title: title_of(&c.stem),
        subtitle: format!("Portable · {}", c.hit.path),
        keywords,
        target: Target::Path { path: c.hit.path.clone(), args: None },
        icon: None,
        admin_ok: true,
    }
}

/// `my_tool-x64` -> "My tool x64"; names that already mix case ("WinMerge") are left alone.
fn title_of(stem: &str) -> String {
    let spaced: String = stem.chars().map(|c| if c == '_' || c == '-' { ' ' } else { c }).collect();
    let spaced = spaced.trim().to_string();
    if spaced.chars().any(|c| c.is_uppercase()) {
        return spaced;
    }
    let mut ch = spaced.chars();
    ch.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + ch.as_str())
}

/// Sweep every `.exe` in the name index (paged). `None` when searchd is not the live source.
pub fn scan(src: &dyn NameSource) -> Option<Vec<LaunchItem>> {
    if src.detail().kind != "searchd" {
        return None;
    }
    let mut all: Vec<Hit> = Vec::new();
    for page in 0..MAX_PAGES {
        let r = src.search("ext:exe", "all", page * PAGE, PAGE, "name");
        let got = r.hits.len();
        all.extend(r.hits);
        if got < PAGE || all.len() as u64 >= r.total {
            break;
        }
    }
    if all.is_empty() {
        return None; // index still scanning or unreachable: keep whatever we had
    }
    let out = select(&all);
    eprintln!("winseek: portable scan: {} exes in the index -> {} programs", all.len(), out.len());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(path: &str, size: u64, mtime: i64) -> Hit {
        let name = path.rsplit('\\').next().unwrap().to_string();
        Hit { name, path: path.into(), is_dir: false, size: Some(size), mtime: Some(mtime) }
    }

    fn titles(items: &[LaunchItem]) -> Vec<String> {
        items.iter().map(|i| i.title.clone()).collect()
    }

    #[test]
    fn keeps_portable_programs_drops_system_and_helpers() {
        let hits = vec![
            exe(r"D:\Tools\mpv\mpv.exe", 30_000_000, 5),
            exe(r"D:\Tools\mpv\updater.exe", 500_000, 5),
            exe(r"D:\Tools\mpv\unins000.exe", 900_000, 5),
            exe(r"C:\Windows\System32\notepad.exe", 200_000, 5),
            exe(r"C:\Program Files\Git\bin\git.exe", 3_000_000, 5),
            exe(r"C:\Users\u\AppData\Local\Temp\x\setup.exe", 3_000_000, 5),
            exe(r"D:\proj\app\target\debug\app.exe", 9_000_000, 5),
            exe(r"D:\proj\node_modules\esbuild\esbuild.exe", 9_000_000, 5),
            exe(r"D:\Tools\tiny\stub.exe", 4_000, 5),
        ];
        assert_eq!(titles(&select(&hits)), vec!["Mpv"]);
    }

    #[test]
    fn generic_folder_uses_the_grandparent_and_namesake_survives_small_size() {
        let hits = vec![exe(r"D:\Portable\Everything\x64\Everything.exe", 60_000, 5), exe(r"D:\Portable\Foo\bin\foo.exe", 80_000, 5)];
        let out = select(&hits);
        assert_eq!(out.len(), 2); // both are their folder's namesake, so the size floor does not apply
        let ev = out.iter().find(|i| i.title == "Everything").unwrap();
        assert!(ev.keywords.iter().any(|k| k == "Everything"));
        assert_eq!(ev.subtitle, r"Portable · D:\Portable\Everything\x64\Everything.exe");
        assert_eq!(ev.id, r"app:d:\portable\everything\x64\everything.exe");
    }

    #[test]
    fn suite_folders_keep_namesake_and_the_two_largest() {
        let mut hits = vec![exe(r"D:\Tools\suite\suite.exe", 5_000_000, 5)];
        for i in 0..12 {
            hits.push(exe(&format!(r"D:\Tools\suite\tool{i}.exe"), 1_000_000 + i * 10_000, 5));
        }
        let out = select(&hits);
        assert_eq!(out.len(), 3); // namesake + 2 largest of the rest
        assert!(titles(&out).contains(&"Suite".to_string()));
    }

    #[test]
    fn duplicate_copies_are_all_kept_and_tellable_apart() {
        let hits = vec![
            exe(r"D:\a\foo\foo.exe", 1_000_000, 1),
            exe(r"D:\b\foo\foo.exe", 1_000_000, 3),
            exe(r"D:\c\foo\foo.exe", 1_000_000, 2),
        ];
        let out = select(&hits);
        assert_eq!(out.len(), 3);
        let mut subs: Vec<_> = out.iter().map(|i| i.subtitle.clone()).collect();
        subs.sort();
        subs.dedup();
        assert_eq!(subs.len(), 3, "each copy shows its own full path");
        assert!(subs.iter().all(|s| s.ends_with("foo.exe")));
    }

    #[test]
    fn junk_paths_are_flagged() {
        assert!(is_junk_path(r"E:\vs_studio\Msbuild\Microsoft\VisualStudio\v17.0\AppxPackage\x86\MsPdbCmf.exe"));
        assert!(is_junk_path(r"C:\tools\mpv\_backups\mpv-v0.41\mpv.exe"));
        assert!(!is_junk_path(r"C:\tools\mpv\mpv.exe"));
    }

    #[test]
    fn titles_are_prettified() {
        assert_eq!(title_of("my_tool-x64"), "My tool x64");
        assert_eq!(title_of("WinMerge"), "WinMerge");
    }
}
