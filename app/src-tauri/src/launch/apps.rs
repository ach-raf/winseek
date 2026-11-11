//! App enumeration for the launcher (Win32, runs on its own STA thread):
//! 1. the shell's AppsFolder (Start menu shortcuts + Store apps in one walk),
//! 2. the App Paths registry (HKLM + HKCU),
//! 3. user-added program folders (depth <= 4),
//! 4. PATH dirs (opt-in, non-recursive).
//!
//! All rules live in `appsrules.rs`; this file is the thin adapter.

use std::path::Path;
use std::time::Instant;

use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{PROPERTYKEY, WIN32_ERROR};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, IPersistFile, STGM_READ};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ,
    RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{
    IEnumShellItems, IShellItem, IShellItem2, IShellLinkW, BHID_EnumItems, FOLDERID_AppsFolder, KF_FLAG_DEFAULT,
    ShellLink, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING, SLGP_RAWPATH, SHGetKnownFolderItem,
};

use super::appsrules;
use super::item::{Kind, LaunchItem, Target};
use super::settings::expand_root;
use crate::shell::{wide, ComGuard};

/// PKEY_Link_TargetParsingPath (shell link target). Inlined to avoid the EnhancedStorage feature.
const PKEY_LINK_TARGET_PARSING_PATH: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xb9b4b3fc_2b51_4a42_b5d8_324146afcf25),
    pid: 2,
};

const APP_PATHS_KEY: PCWSTR = windows::core::w!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths");

/// Sources for one catalog build. Mirrors `LaunchCfg` in the launcher state.
#[derive(Clone, Debug)]
pub struct AppOpts {
    pub hide_uninstallers: bool,
    pub folders: Vec<String>,
    pub path: bool,
}

/// Enumerate all enabled sources (COM initialised here; call from a dedicated thread).
pub fn enumerate(opts: &AppOpts) -> Vec<LaunchItem> {
    let _com = ComGuard::init();
    let started = Instant::now();
    let mut items = apps_folder(opts);
    let from_folder = items.len();
    items.extend(app_paths());
    for folder in &opts.folders {
        let found = folder_items(folder, opts);
        eprintln!("winseek: app catalog: {} items from folder {folder}", found.len());
        items.extend(found);
    }
    if opts.path {
        let found = path_items();
        eprintln!("winseek: app catalog: {} items from PATH", found.len());
        items.extend(found);
    }
    let out = appsrules::dedupe(items);
    eprintln!(
        "winseek: app catalog: {} items ({} from AppsFolder) in {:.1?}",
        out.len(),
        from_folder,
        started.elapsed()
    );
    out
}

/// shell:AppsFolder — one enumeration for Start menu shortcuts AND Store/UWP apps.
fn apps_folder(opts: &AppOpts) -> Vec<LaunchItem> {
    // SAFETY: known-folder GUID is static; the item is COM-owned and freed on drop.
    let folder: IShellItem = match unsafe { SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) } {
        Ok(f) => f,
        Err(e) => {
            eprintln!("winseek: AppsFolder unavailable: {e}");
            return Vec::new();
        }
    };
    // SAFETY: valid item + static BHID GUID.
    let items: IEnumShellItems = match unsafe { folder.BindToHandler(None, &BHID_EnumItems) } {
        Ok(i) => i,
        Err(e) => {
            eprintln!("winseek: AppsFolder enumeration unavailable: {e}");
            return Vec::new();
        }
    };
    let startup = startup_dirs();
    let mut out = Vec::new();
    let mut buf: [Option<IShellItem>; 64] = std::array::from_fn(|_| None);
    loop {
        let mut fetched = 0u32;
        // SAFETY: `buf` is the output array, `fetched` the count out-param.
        if unsafe { items.Next(&mut buf, Some(&mut fetched)) }.is_err() || fetched == 0 {
            break;
        }
        for slot in buf[..fetched as usize].iter_mut() {
            let Some(item) = slot.take() else { continue };
            if let Some(launch) = apps_folder_item(&item, opts, &startup) {
                out.push(launch);
            }
        }
    }
    out
}

fn apps_folder_item(item: &IShellItem, opts: &AppOpts, startup: &[String]) -> Option<LaunchItem> {
    let title = display_name(item, SIGDN_NORMALDISPLAY)?;
    let aumid = display_name(item, SIGDN_PARENTRELATIVEPARSING)?;
    if title.is_empty() || aumid.is_empty() {
        return None;
    }
    // Link targets (Win32 shortcuts) resolve to their exe; Store apps keep `None`.
    let mut path: Option<String> = None;
    if let Ok(item2) = item.cast::<IShellItem2>() {
        // SAFETY: valid item; the returned PWSTR is CoTaskMem-owned, freed in pwstr_string.
        if let Ok(pw) = unsafe { item2.GetString(&PKEY_LINK_TARGET_PARSING_PATH) } {
            let s = pwstr_string(pw);
            if !s.is_empty() {
                path = Some(s);
            }
        }
    }
    let store = aumid.contains('!');
    let admin_ok = path.is_some();

    // Launch-by-URL entries: only the store-run protocols Flow allows survive.
    if path.is_none() && !store && aumid.contains("://") && !aumid.starts_with('\\') && !appsrules::allowed_url(&aumid) {
        return None;
    }
    let exe = path.as_deref();
    if exe.is_some_and(|p| startup.iter().any(|s| p.to_lowercase().starts_with(s))) {
        return None; // autolaunch entries, not apps
    }
    if opts.hide_uninstallers
        && (appsrules::is_uninstaller(&title, exe) || exe.is_some_and(|p| appsrules::is_helper_exe(&appsrules::path_stem(p))))
    {
        return None;
    }
    // Shortcut apps show where the exe really lives (the UI middle-truncates); Store apps have none.
    let subtitle = match exe {
        Some(p) if !store => format!("Start menu · {p}"),
        _ if store => "Store app".into(),
        _ => "Start menu".into(),
    };
    let keywords = exe.map(|p| vec![appsrules::path_stem(p)]).unwrap_or_default();
    Some(LaunchItem {
        id: format!("app:{}", aumid.to_lowercase()),
        kind: Kind::App,
        title,
        subtitle,
        keywords,
        target: Target::Aumid { aumid, path },
        icon: None,
        admin_ok,
    })
}

/// Registry App Paths: default value = exe path; key name (`chrome.exe`) stem as keyword.
fn app_paths() -> Vec<LaunchItem> {
    let mut out = Vec::new();
    for (root, label) in [(HKEY_LOCAL_MACHINE, "HKLM"), (HKEY_CURRENT_USER, "HKCU")] {
        let found = app_paths_hive(root);
        eprintln!("winseek: app catalog: {} entries from {label} App Paths", found.len());
        out.extend(found);
    }
    out
}

fn app_paths_hive(root: HKEY) -> Vec<LaunchItem> {
    let mut out = Vec::new();
    let mut key = HKEY::default();
    // SAFETY: static key path; handle closed before returning.
    if unsafe { RegOpenKeyExW(root, APP_PATHS_KEY, None, KEY_READ, &mut key) }.0 != 0 {
        return out;
    }
    let mut index = 0u32;
    loop {
        let mut name_buf = [0u16; 256];
        let mut name_len = name_buf.len() as u32;
        // SAFETY: out buffers sized above; this is the documented enumeration loop.
        let r: WIN32_ERROR = unsafe {
            RegEnumKeyExW(key, index, Some(PWSTR(name_buf.as_mut_ptr())), &mut name_len, None, None, None, None)
        };
        if r.0 != 0 {
            break; // ERROR_NO_MORE_ITEMS or a real error: stop this hive
        }
        index += 1;
        let key_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        if let Some(item) = app_paths_entry(key, &key_name) {
            out.push(item);
        }
    }
    // SAFETY: opened above.
    unsafe {
        let _ = RegCloseKey(key);
    }
    out
}

/// One App Paths subkey -> item, or None (with the reason logged).
fn app_paths_entry(key: HKEY, key_name: &str) -> Option<LaunchItem> {
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let value_w = wide(key_name);
    // SAFETY: NUL-terminated value name; opened parent key; out buffer sized above.
    let r = unsafe {
        RegGetValueW(
            key,
            PCWSTR::null(),
            PCWSTR(value_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if r.0 != 0 {
        eprintln!("winseek: app catalog: App Paths entry {key_name} has no default value (error {})", r.0);
        return None;
    }
    let mut raw = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1).min(buf.len())]);
    // Trim quotes/whitespace, expand env vars; `"C:\a b\x.exe" /flag` keeps only the path.
    raw = raw.trim().trim_matches('"').trim().to_string();
    if raw.contains('"') {
        raw = raw.split('"').next().unwrap_or("").to_string();
    }
    let exe = expand_root(raw.trim());
    if !Path::new(&exe).is_file() {
        eprintln!("winseek: app catalog: App Paths entry {key_name} target missing ({exe})");
        return None;
    }
    let stem = appsrules::path_stem(&exe);
    let key_stem = appsrules::path_stem(key_name);
    Some(LaunchItem {
        id: format!("app:{}", exe.to_lowercase()),
        kind: Kind::App,
        title: capitalize(&stem),
        subtitle: "App Paths".into(),
        keywords: [stem, key_stem].into_iter().filter(|k| !k.is_empty()).collect(),
        target: Target::Path { path: exe, args: None },
        icon: None,
        admin_ok: true,
    })
}

/// Extra program folders: exe/lnk/appref-ms up to depth 4.
fn folder_items(root: &str, opts: &AppOpts) -> Vec<LaunchItem> {
    let mut out = Vec::new();
    walk_folder(Path::new(root), 0, opts, &mut out);
    out
}

fn walk_folder(dir: &Path, depth: u8, opts: &AppOpts, out: &mut Vec<LaunchItem>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_folder(&path, depth + 1, opts, out);
            continue;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !matches!(ext.as_str(), "exe" | "lnk" | "appref-ms") {
            continue;
        }
        let stem = appsrules::path_stem(&name);
        if opts.hide_uninstallers && (appsrules::is_uninstaller(&stem, path.to_str()) || appsrules::is_helper_exe(&stem)) {
            continue;
        }
        let (target_path, args) = if ext == "lnk" {
            match path.to_str().and_then(lnk_target) {
                Some(t) => (t, None),
                None => {
                    eprintln!("winseek: app catalog: .lnk target unreadable: {}", path.display());
                    continue;
                }
            }
        } else {
            (path.to_string_lossy().into_owned(), None)
        };
        out.push(LaunchItem {
            id: format!("app:{}", target_path.to_lowercase()),
            kind: Kind::App,
            title: capitalize(&stem),
            subtitle: path.to_string_lossy().into_owned(),
            keywords: vec![stem],
            target: Target::Path { path: target_path, args },
            icon: None,
            admin_ok: true,
        });
    }
}

/// PATH dirs, non-recursive, exes only (off by default: noisy).
fn path_items() -> Vec<LaunchItem> {
    let mut out = Vec::new();
    let Some(path_var) = std::env::var_os("PATH") else { return out };
    for dir in std::env::split_paths(&path_var) {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| !e.eq_ignore_ascii_case("exe")).unwrap_or(true) {
                continue;
            }
            let stem = appsrules::path_stem(&path.to_string_lossy());
            if appsrules::is_helper_exe(&stem) {
                continue;
            }
            out.push(LaunchItem {
                id: format!("app:{}", path.to_string_lossy().to_lowercase()),
                kind: Kind::App,
                title: capitalize(&stem),
                subtitle: "PATH".into(),
                keywords: vec![stem],
                target: Target::Path { path: path.to_string_lossy().into_owned(), args: None },
                icon: None,
                admin_ok: true,
            });
        }
    }
    out
}

/// Resolve a .lnk's target path. Needs COM STA (the enumerator thread has it).
fn lnk_target(path: &str) -> Option<String> {
    // SAFETY: CoCreateInstance with the shell-link CLSID; interfaces released on drop.
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_ALL) }.ok()?;
    let persist: IPersistFile = link.cast().ok()?;
    let w = wide(path);
    // SAFETY: `w` outlives the call; Load takes the path by PCWSTR.
    unsafe { persist.Load(PCWSTR(w.as_ptr()), STGM_READ) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut fd = WIN32_FIND_DATAW::default();
    // SAFETY: buffer + find-data out structs sized above.
    unsafe { link.GetPath(&mut buf, &mut fd, SLGP_RAWPATH.0 as u32) }.ok()?;
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let target = String::from_utf16_lossy(&buf[..end]);
    (!target.is_empty()).then_some(target)
}

/// %APPDATA% / %ProgramData% Startup folders — entries here autostart, they are not apps.
fn startup_dirs() -> Vec<String> {
    let mut out = Vec::new();
    let suffix = r"Microsoft\Windows\Start Menu\Programs\Startup";
    for var in ["APPDATA", "ProgramData"] {
        if let Some(base) = std::env::var_os(var) {
            out.push(format!("{}\\{suffix}\\", base.to_string_lossy().to_lowercase()));
        }
    }
    out
}

fn display_name(item: &IShellItem, sigdn: windows::Win32::UI::Shell::SIGDN) -> Option<String> {
    // SAFETY: valid item; returned PWSTR is CoTaskMem-owned and freed right after the read.
    let pw = unsafe { item.GetDisplayName(sigdn) }.ok()?;
    let s = pwstr_string(pw);
    let trimmed = s.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Read + free a CoTaskMem-allocated PWSTR.
fn pwstr_string(pw: PWSTR) -> String {
    let s = unsafe { pw.to_string() }.unwrap_or_default();
    // SAFETY: the pointer came from a shell allocator call and is freed exactly once.
    unsafe { CoTaskMemFree(Some(pw.0.cast())) };
    s
}

fn capitalize(stem: &str) -> String {
    let mut chars = stem.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live check (run manually on Windows): `cargo test -p winseek-app apps:: -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn enumerate_lists_some_apps() {
        let opts = AppOpts { hide_uninstallers: true, folders: vec![], path: false };
        let items = enumerate(&opts);
        eprintln!("app catalog: {} items", items.len());
        for i in items.iter().take(20) {
            eprintln!("  {} | {} | {}", i.title, i.subtitle, i.id);
        }
        assert!(items.len() > 10, "expected real apps, got {}", items.len());
    }
}
