//! Launcher state: the in-memory catalog (apps + settings pages + system commands), selection
//! history with prefix learning, config persistence, and the Tauri commands the UI calls.
//! Spec: docs/specs/2026-10-20-launcher-prd.md (R1, R5-R8).

pub mod aliases;
pub mod apps;
pub mod appsrules;
pub mod commands;
pub mod history;
pub mod item;
pub mod matcher;
pub mod portable;
pub mod run;
pub mod settings;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::tier1;
use apps::AppOpts;
use commands as syscmd;
use history::History;
use item::{Kind, LaunchItem, Target};

/// Launcher sources + exclusions. All fields default (PRD §4.3): everything on except PATH
/// search, uninstallers hidden, no extra folders, nothing hidden.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchCfg {
    pub apps: bool,
    pub settings: bool,
    pub commands: bool,
    pub portable: bool,
    pub path: bool,
    pub hide_uninstallers: bool,
    pub folders: Vec<String>,
    pub hidden: Vec<String>,
    /// Your own shortcuts: alias typed -> text found in an app's title / exe name / id
    /// (`{"ps": "photoshop"}`). Added to the built-in alias table.
    pub aliases: BTreeMap<String, String>,
}

impl Default for LaunchCfg {
    fn default() -> Self {
        LaunchCfg {
            apps: true,
            settings: true,
            commands: true,
            portable: true,
            path: false,
            hide_uninstallers: true,
            folders: Vec::new(),
            hidden: Vec::new(),
            aliases: BTreeMap::new(),
        }
    }
}

/// One scored result row.
#[derive(Clone, Serialize)]
pub struct LaunchHit {
    pub item: LaunchItem,
    pub score: u32,
    /// Char ranges into the title for highlighting.
    pub ranges: Vec<(usize, usize)>,
    /// Needs the second-Enter confirmation (shutdown/restart/sign out/empty bin).
    pub confirm: bool,
}

#[derive(Clone, Serialize)]
pub struct LaunchCounts {
    pub apps: usize,
    pub settings: usize,
    pub commands: usize,
}

pub struct Launcher {
    cat: RwLock<Arc<Vec<LaunchItem>>>,
    hist: Mutex<History>,
    cfg: RwLock<LaunchCfg>,
    hist_dirty: Arc<AtomicBool>,
    hist_path: PathBuf,
    cfg_path: PathBuf,
    cache_path: PathBuf,
    rebuilding: AtomicBool,
    /// Portable programs found by the index sweep (`portable.rs`): raw list + the copy that
    /// search uses (deduped against the catalog, aliases applied).
    portable_raw: Mutex<Vec<LaunchItem>>,
    portable: RwLock<Arc<Vec<LaunchItem>>>,
    portable_cache_path: PathBuf,
    portable_wake: AtomicBool,
}

fn data_dir() -> PathBuf {
    tier1::data_root().unwrap_or_else(std::env::temp_dir)
}

impl Launcher {
    /// Synchronous construction: cached apps (if any) + settings + commands are ready at once;
    /// the fresh app enumeration happens on the background thread started by `start_background`.
    pub fn new() -> Self {
        let dir = data_dir();
        let cfg_path = dir.join("launch_config.json");
        let cfg = match std::fs::read(&cfg_path).map(|b| serde_json::from_slice::<LaunchCfg>(&b)) {
            Ok(Ok(cfg)) => cfg,
            Ok(Err(e)) => {
                eprintln!("winseek: launch config is not valid JSON ({e}), using defaults");
                LaunchCfg::default()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => LaunchCfg::default(), // first run
            Err(e) => {
                eprintln!("winseek: launch config unreadable ({e}), using defaults");
                LaunchCfg::default()
            }
        };
        let hist_path = dir.join("launch_history.json");
        let hist = History::load(&hist_path);
        let cache_path = dir.join("launch_catalog.json");
        let mut items: Vec<LaunchItem> = std::fs::read(&cache_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<LaunchItem>>(&b).ok())
            .map(|cached| {
                eprintln!("winseek: launch catalog: {} apps restored from cache", cached.len());
                cached.into_iter().filter(|i| i.kind == Kind::App).collect()
            })
            .unwrap_or_default();
        let (s, c) = (settings::catalog(settings::os_build()), syscmd::catalog(syscmd::hibernate_available()));
        eprintln!("winseek: launch catalog starts with {} apps + {} settings + {} commands", items.len(), s.len(), c.len());
        aliases::apply(&mut items, &cfg.aliases);
        items.extend(s);
        items.extend(c);
        let portable_cache_path = dir.join("launch_portable.json");
        let portable_raw: Vec<LaunchItem> =
            std::fs::read(&portable_cache_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        eprintln!("winseek: launch catalog: {} portable programs restored from cache", portable_raw.len());
        let launcher = Launcher {
            cat: RwLock::new(Arc::new(items)),
            hist: Mutex::new(hist),
            cfg: RwLock::new(cfg),
            hist_dirty: Arc::new(AtomicBool::new(false)),
            hist_path,
            cfg_path,
            cache_path,
            rebuilding: AtomicBool::new(false),
            portable_raw: Mutex::new(portable_raw),
            portable: RwLock::new(Arc::new(Vec::new())),
            portable_cache_path,
            portable_wake: AtomicBool::new(true),
        };
        launcher.refresh_portable();
        launcher
    }

    /// Replace the portable list (fresh index sweep), persist it, rebuild the search copy.
    pub fn set_portable(&self, items: Vec<LaunchItem>) {
        match serde_json::to_vec(&items) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&self.portable_cache_path, bytes) {
                    eprintln!("winseek: writing portable cache: {e}");
                }
            }
            Err(e) => eprintln!("winseek: serializing portable cache: {e}"),
        }
        *self.portable_raw.lock().unwrap_or_else(|e| e.into_inner()) = items;
        self.refresh_portable();
    }

    /// Search copy of the portable list: drops what a catalog app already covers (same exe),
    /// applies aliases. Cheap; runs after every catalog or portable change.
    fn refresh_portable(&self) {
        let raw = self.portable_raw.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let cat = self.cat.read().unwrap_or_else(|e| e.into_inner()).clone();
        let aliases = self.cfg.read().unwrap_or_else(|e| e.into_inner()).aliases.clone();
        let covered: HashSet<String> =
            cat.iter().filter(|i| i.kind == Kind::App).filter_map(appsrules::dedupe_key).collect();
        let mut keep: Vec<LaunchItem> =
            raw.into_iter().filter(|i| appsrules::dedupe_key(i).is_none_or(|k| !covered.contains(&k))).collect();
        aliases::apply(&mut keep, &aliases);
        *self.portable.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(keep);
    }

    /// Background threads: initial app re-scan, debounced history saver, Start-menu watchers.
    pub fn start_background(app: AppHandle) {
        spawn_rebuild(&app);
        let portable_app = app.clone();
        std::thread::Builder::new()
            .name("launch-portable".into())
            .spawn(move || portable_loop(portable_app))
            .map_err(|e| eprintln!("winseek: spawning portable scanner: {e}"))
            .ok();
        let saver = app.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(2));
            let st = saver.state::<Launcher>();
            if st.hist_dirty.swap(false, Ordering::Relaxed) {
                let snap = st.hist.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Err(e) = snap.save(&st.hist_path) {
                    eprintln!("winseek: saving launch history: {e}");
                }
            }
        });
        for dir in start_menu_dirs() {
            let watcher = app.clone();
            std::thread::Builder::new()
                .name("launch-watch".into())
                .spawn(move || watch_start_menu_dir(dir, watcher))
                .map_err(|e| eprintln!("winseek: spawning start-menu watcher: {e}"))
                .ok();
        }
    }

    /// Score every enabled catalog item + history boost. Pure memory work, no I/O.
    pub fn search(&self, query: &str, limit: usize) -> Vec<LaunchHit> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let started = Instant::now();
        let cat = self.cat.read().unwrap_or_else(|e| e.into_inner()).clone();
        let cfg = self.cfg.read().unwrap_or_else(|e| e.into_inner()).clone();
        let port: Arc<Vec<LaunchItem>> = if cfg.portable && cfg.apps {
            self.portable.read().unwrap_or_else(|e| e.into_inner()).clone()
        } else {
            Arc::default()
        };
        let item_at = |idx: usize| if idx < cat.len() { &cat[idx] } else { &port[idx - cat.len()] };
        let hist = self.hist.lock().unwrap_or_else(|e| e.into_inner());
        let qtext = q.to_string();
        let q = matcher::prepare(q);
        let mut scorer = matcher::Scorer::default();
        // Score first, clone later: only the rows that survive the sort get their LaunchItem
        // cloned (a 1-char query can match every item in the catalog).
        struct Scored {
            score: u32,
            ranges: Vec<(usize, usize)>,
            confirm: bool,
            idx: usize,
        }
        let mut scored: Vec<Scored> = Vec::new();
        for (idx, item) in cat.iter().chain(port.iter()).enumerate() {
            let enabled = match item.kind {
                Kind::App => cfg.apps,
                Kind::Setting => cfg.settings,
                Kind::Command => cfg.commands,
            };
            if !enabled || cfg.hidden.iter().any(|h| h == &item.id) {
                continue;
            }
            let Some(mut m) = matcher::score_item_with(&mut scorer, &q, item) else { continue };
            m.score = m.score.saturating_add(hist.boost(&qtext, &item.id));
            // Apps are what a launcher is for: they win ties against settings and commands, and a
            // Start-menu app beats an unzipped portable copy of the same name.
            if item.kind == Kind::App {
                m.score += if item.subtitle.starts_with("Portable") { 8 } else { 15 };
            }
            let confirm = matches!(item.target, Target::Cmd { .. }) && syscmd::needs_confirm(&item.id);
            scored.push(Scored { score: m.score, ranges: m.ranges, confirm, idx });
        }
        scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| item_at(a.idx).title.cmp(&item_at(b.idx).title)));
        scored.truncate(limit);
        if started.elapsed() > Duration::from_millis(2) {
            eprintln!("winseek: launch search '{qtext}' took {:.1?} over {} items", started.elapsed(), cat.len() + port.len());
        }
        scored
            .into_iter()
            .map(|h| LaunchHit { item: item_at(h.idx).clone(), score: h.score, ranges: h.ranges, confirm: h.confirm })
            .collect()
    }

    pub fn counts(&self) -> LaunchCounts {
        let cat = self.cat.read().unwrap_or_else(|e| e.into_inner());
        let portable = if self.cfg.read().unwrap_or_else(|e| e.into_inner()).portable {
            self.portable.read().unwrap_or_else(|e| e.into_inner()).len()
        } else {
            0
        };
        LaunchCounts {
            apps: cat.iter().filter(|i| i.kind == Kind::App).count() + portable,
            settings: cat.iter().filter(|i| i.kind == Kind::Setting).count(),
            commands: cat.iter().filter(|i| i.kind == Kind::Command).count(),
        }
    }

    pub fn get_item(&self, id: &str) -> Option<LaunchItem> {
        if let Some(i) = self.cat.read().unwrap_or_else(|e| e.into_inner()).iter().find(|i| i.id == id) {
            return Some(i.clone());
        }
        self.portable.read().unwrap_or_else(|e| e.into_inner()).iter().find(|i| i.id == id).cloned()
    }

    pub fn get_config(&self) -> LaunchCfg {
        self.cfg.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the config, persist it; returns true when a catalog source changed.
    pub fn set_config(&self, cfg: LaunchCfg) -> bool {
        let sources_changed = {
            let mut c = self.cfg.write().unwrap_or_else(|e| e.into_inner());
            let changed = c.apps != cfg.apps
                || c.path != cfg.path
                || c.hide_uninstallers != cfg.hide_uninstallers
                || c.folders != cfg.folders
                || c.aliases != cfg.aliases;
            if !c.portable && cfg.portable {
                self.portable_wake.store(true, Ordering::Relaxed); // just switched on: sweep now
            }
            *c = cfg;
            changed
        };
        self.persist_config();
        if sources_changed {
            eprintln!("winseek: launch config changed a source, re-enumerating");
        }
        sources_changed
    }

    pub fn hide(&self, id: &str) {
        let mut c = self.cfg.write().unwrap_or_else(|e| e.into_inner());
        if !c.hidden.iter().any(|h| h == id) {
            c.hidden.push(id.to_string());
        }
        drop(c);
        self.persist_config();
    }

    pub fn unhide(&self, id: &str) {
        let mut c = self.cfg.write().unwrap_or_else(|e| e.into_inner());
        c.hidden.retain(|h| h != id);
        drop(c);
        self.persist_config();
    }

    pub fn record(&self, query: &str, id: &str) {
        self.hist.lock().unwrap_or_else(|e| e.into_inner()).record(query, id);
        self.hist_dirty.store(true, Ordering::Relaxed);
    }

    pub fn clear_history(&self) -> Result<(), String> {
        self.hist.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let snap = self.hist.lock().unwrap_or_else(|e| e.into_inner()).clone();
        snap.save(&self.hist_path)
    }

    fn persist_config(&self) {
        let c = self.cfg.read().unwrap_or_else(|e| e.into_inner()).clone();
        match serde_json::to_vec(&c) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&self.cfg_path, bytes) {
                    eprintln!("winseek: writing launch config: {e}");
                }
            }
            Err(e) => eprintln!("winseek: serializing launch config: {e}"),
        }
    }

    /// Full re-enumeration (blocking; runs on a worker thread). Swaps the catalog only when
    /// the new one is complete, so searching continues against the old one meanwhile.
    fn rebuild_blocking(&self) {
        let cfg = self.cfg.read().unwrap_or_else(|e| e.into_inner()).clone();
        let opts = AppOpts { hide_uninstallers: cfg.hide_uninstallers, folders: cfg.folders.clone(), path: cfg.path };
        let mut items = apps::enumerate(&opts);
        aliases::apply(&mut items, &cfg.aliases);
        items.extend(settings::catalog(settings::os_build()));
        items.extend(syscmd::catalog(syscmd::hibernate_available()));
        let cache: Vec<LaunchItem> = items.iter().filter(|i| i.kind == Kind::App).cloned().collect();
        *self.cat.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(items);
        self.refresh_portable(); // a portable exe that just got a Start-menu entry stops showing twice
        match serde_json::to_vec(&cache) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&self.cache_path, bytes) {
                    eprintln!("winseek: writing launch catalog cache: {e}");
                }
            }
            Err(e) => eprintln!("winseek: serializing launch catalog cache: {e}"),
        }
    }
}

/// Sweeps the Tier 0 name index for portable programs: shortly after start, again every ten
/// minutes, and on demand (toggle switched on, Re-index). Waits quietly while searchd is down
/// or the index is still empty.
fn portable_loop(app: AppHandle) {
    let mut last: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_secs(5));
        let st = app.state::<Launcher>();
        let wake = st.portable_wake.swap(false, Ordering::Relaxed);
        let due = last.is_none_or(|t| t.elapsed() > Duration::from_secs(600));
        if !(wake || due) || !st.get_config().portable {
            continue;
        }
        let src = app.state::<crate::commands::AppState>().source();
        match portable::scan(&*src) {
            Some(items) => {
                st.set_portable(items);
                last = Some(Instant::now());
                let _ = app.emit("launch-catalog-changed", ());
            }
            None => std::thread::sleep(Duration::from_secs(25)), // searchd not ready: retry soon
        }
    }
}

/// One re-enumeration at a time; watchers can fire repeatedly, extra calls are dropped.
pub fn spawn_rebuild(app: &AppHandle) {
    let st = app.state::<Launcher>();
    if st.rebuilding.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::Builder::new()
        .name("launch-rebuild".into())
        .spawn(move || {
            let started = Instant::now();
            let st = app.state::<Launcher>();
            st.rebuild_blocking();
            st.rebuilding.store(false, Ordering::SeqCst);
            eprintln!("winseek: launch catalog rebuilt in {:.1?}", started.elapsed());
            let _ = app.emit("launch-catalog-changed", ());
        })
        .map_err(|e| eprintln!("winseek: spawning launch rebuild: {e}"))
        .ok();
}

fn start_menu_dirs() -> Vec<String> {
    let mut out = Vec::new();
    let suffix = r"Microsoft\Windows\Start Menu\Programs";
    for var in ["APPDATA", "ProgramData"] {
        if let Some(base) = std::env::var_os(var) {
            out.push(format!("{}\\{suffix}", base.to_string_lossy()));
        }
    }
    out
}

/// Blocking ReadDirectoryChangesW on one Start-menu tree; any change re-enumerates after 1 s.
fn watch_start_menu_dir(dir: String, app: AppHandle) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
        FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    loop {
        let w = crate::shell::wide(&dir);
        // SAFETY: NUL-terminated path; the directory handle is closed on every loop exit.
        let handle = match unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        } {
            Ok(h) => h,
            Err(e) => {
                eprintln!("winseek: cannot watch {dir} ({e}); retrying in 10 s");
                std::thread::sleep(Duration::from_secs(10));
                continue;
            }
        };
        eprintln!("winseek: watching start menu {dir}");
        let mut buf = vec![0u8; 64 * 1024];
        let mut bytes = 0u32;
        loop {
            // SAFETY: valid directory handle, buffer sized above; synchronous (no OVERLAPPED).
            let ok = unsafe {
                ReadDirectoryChangesW(
                    handle,
                    buf.as_mut_ptr().cast(),
                    buf.len() as u32,
                    true,
                    FILE_NOTIFY_CHANGE_FILE_NAME
                        | FILE_NOTIFY_CHANGE_DIR_NAME
                        | FILE_NOTIFY_CHANGE_LAST_WRITE
                        | FILE_NOTIFY_CHANGE_SIZE
                        | FILE_NOTIFY_CHANGE_CREATION
                        | FILE_NOTIFY_CHANGE_ATTRIBUTES,
                    Some(&mut bytes),
                    None,
                    None,
                )
            };
            if ok.is_err() || bytes == 0 {
                break; // watch broken or buffer overflow: reopen and rebuild anyway
            }
            // Debounce: installers drop dozens of files; let the tree go quiet first.
            std::thread::sleep(Duration::from_secs(1));
            spawn_rebuild(&app);
        }
        // SAFETY: owned handle from CreateFileW above.
        unsafe {
            let _ = CloseHandle(handle);
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

// ---------- Tauri commands ----------

#[tauri::command]
pub fn launch_search(state: tauri::State<'_, Launcher>, query: String, limit: usize) -> Vec<LaunchHit> {
    state.search(&query, limit)
}

#[tauri::command]
pub async fn launch_run(app: AppHandle, id: String, query: String, admin: bool) -> Result<(), String> {
    crate::commands::blocking(move || {
        let st = app.state::<Launcher>();
        let item = st.get_item(&id).ok_or_else(|| format!("unknown launch id {id}"))?;
        // A refused UAC comes back as Ok with nothing launched; recording the attempt is what
        // Flow does too (the user did ask for it), so any Ok records the selection.
        let launched = run::run_item(&item, admin);
        if launched.is_ok() {
            st.record(&query, &id);
        }
        launched
    })
    .await
    .and_then(|r| r)
}

/// Record a pick the UI handled itself (winseek Settings / Library).
#[tauri::command]
pub fn launch_record(state: tauri::State<'_, Launcher>, id: String, query: String) {
    state.record(&query, &id);
}

#[tauri::command]
pub fn launch_reindex(app: AppHandle) {
    app.state::<Launcher>().portable_wake.store(true, Ordering::Relaxed);
    spawn_rebuild(&app);
}

#[tauri::command]
pub fn launch_get_config(state: tauri::State<'_, Launcher>) -> LaunchCfg {
    state.get_config()
}

#[tauri::command]
pub async fn launch_set_config(app: AppHandle, cfg: LaunchCfg) -> Result<LaunchCfg, String> {
    crate::commands::blocking(move || {
        let st = app.state::<Launcher>();
        let changed = st.set_config(cfg);
        if changed {
            spawn_rebuild(&app);
        }
        Ok(st.get_config())
    })
    .await
    .and_then(|r| r)
}

#[tauri::command]
pub fn launch_hide(state: tauri::State<'_, Launcher>, id: String) {
    state.hide(&id);
}

#[tauri::command]
pub fn launch_unhide(state: tauri::State<'_, Launcher>, id: String) {
    state.unhide(&id);
}

#[tauri::command]
pub async fn launch_clear_history(app: AppHandle) -> Result<(), String> {
    crate::commands::blocking(move || app.state::<Launcher>().clear_history())
        .await
        .and_then(|r| r)
}

#[tauri::command]
pub fn launch_counts(state: tauri::State<'_, Launcher>) -> LaunchCounts {
    state.counts()
}

/// Copy the item's target: exe path, AUMID, URI or the joined command line.
#[tauri::command]
pub async fn launch_copy(app: AppHandle, id: String) -> Result<(), String> {
    crate::commands::blocking(move || {
        let st = app.state::<Launcher>();
        let item = st.get_item(&id).ok_or_else(|| format!("unknown launch id {id}"))?;
        let target = match &item.target {
            Target::Aumid { aumid, path } => path.clone().unwrap_or_else(|| aumid.clone()),
            Target::Path { path, .. } => path.clone(),
            Target::Uri { uri } => uri.clone(),
            Target::Exec { exe, args } => {
                let mut s = exe.clone();
                for a in args {
                    s.push(' ');
                    s.push_str(a);
                }
                s
            }
            Target::Cmd { .. } => item.id.clone(),
        };
        crate::commands::copy_path(target)
    })
    .await
    .and_then(|r| r)
}

/// Select the item's real file in Explorer (the resolved exe for shortcuts — PRD §4.2).
#[tauri::command]
pub async fn launch_reveal(app: AppHandle, id: String) -> Result<(), String> {
    crate::commands::blocking(move || {
        let st = app.state::<Launcher>();
        let item = st.get_item(&id).ok_or_else(|| format!("unknown launch id {id}"))?;
        let target = match &item.target {
            Target::Aumid { path, .. } => path.clone(),
            Target::Path { path, .. } => Some(path.clone()),
            Target::Exec { exe, .. } => Some(exe.clone()),
            _ => None,
        };
        let path = target.ok_or("this item has no file location")?;
        crate::commands::reveal_path(path)
    })
    .await
    .and_then(|r| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, kind: Kind, title: &str, keywords: &[&str]) -> LaunchItem {
        LaunchItem {
            id: id.into(),
            kind,
            title: title.into(),
            subtitle: "test".into(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            target: Target::Path { path: format!("C:\\{id}.exe"), args: None },
            icon: None,
            admin_ok: true,
        }
    }

    fn launcher_with(items: Vec<LaunchItem>, cfg: LaunchCfg) -> Launcher {
        let dir = std::env::temp_dir().join(format!("winseek-launch-test-{}", std::process::id()));
        Launcher {
            cat: RwLock::new(Arc::new(items)),
            hist: Mutex::new(History::default()),
            cfg: RwLock::new(cfg),
            hist_dirty: Arc::new(AtomicBool::new(false)),
            hist_path: dir.join("h.json"),
            cfg_path: dir.join("c.json"),
            cache_path: dir.join("cache.json"),
            rebuilding: AtomicBool::new(false),
            portable_raw: Mutex::new(Vec::new()),
            portable: RwLock::new(Arc::new(Vec::new())),
            portable_cache_path: dir.join("p.json"),
            portable_wake: AtomicBool::new(false),
        }
    }

    #[test]
    fn exact_title_beats_fuzzy_and_hidden_and_disabled_are_dropped() {
        let items = vec![
            item("app:mpv-net", Kind::App, "mpv.net", &[]),
            item("app:mpv", Kind::App, "mpv", &[]),
            item("app:hidden", Kind::App, "mpv exact hidden", &[]),
            item("app:disabled", Kind::Command, "mpv command disabled", &[]),
        ];
        let cfg = LaunchCfg { commands: false, hidden: vec!["app:hidden".into()], ..Default::default() };
        let l = launcher_with(items, cfg);
        let hits = l.search("mpv", 10);
        assert_eq!(hits.first().map(|h| h.item.id.as_str()), Some("app:mpv"));
        assert!(hits.iter().all(|h| h.item.id != "app:hidden" && h.item.id != "app:disabled"));
        assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn history_boost_lifts_a_picked_item() {
        let items = vec![item("app:a", Kind::App, "alpha", &[]), item("app:b", Kind::App, "alphanumeric tool", &[])];
        let l = launcher_with(items, LaunchCfg::default());
        l.record("al", "app:b");
        l.record("al", "app:b");
        let hits = l.search("al", 10);
        assert_eq!(hits.first().map(|h| h.item.id.as_str()), Some("app:b"));
    }

    #[test]
    fn confirm_flag_follows_the_command() {
        use crate::launch::item::Target;
        let mut shutdown = item("cmd:shutdown", Kind::Command, "Shut down", &[]);
        shutdown.target = Target::Cmd { name: "shutdown".into() };
        let l = launcher_with(vec![shutdown], LaunchCfg::default());
        let hits = l.search("shut", 10);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].confirm);
    }

    #[test]
    fn portable_items_are_searchable_and_deduped_against_the_catalog() {
        let mut portable_foo = item(r"app:d:\tools\foo.exe", Kind::App, "Foo", &[]);
        portable_foo.subtitle = r"Portable · D:\Tools".into();
        portable_foo.target = Target::Path { path: r"D:\Tools\foo.exe".into(), args: None };
        let mut dup = item("app:dup", Kind::App, "Foo Start Menu", &[]);
        dup.target = Target::Path { path: r"D:\Tools\foo.exe".into(), args: None };
        let l = launcher_with(vec![dup], LaunchCfg::default());
        *l.portable_raw.lock().unwrap() = vec![portable_foo.clone()];
        l.refresh_portable();
        assert!(l.search("foo", 10).iter().all(|h| h.item.id != portable_foo.id), "covered exe must not show twice");

        let l = launcher_with(vec![], LaunchCfg::default());
        *l.portable_raw.lock().unwrap() = vec![portable_foo.clone()];
        l.refresh_portable();
        assert_eq!(l.search("foo", 10).first().map(|h| h.item.id.clone()), Some(portable_foo.id.clone()));
        assert!(l.get_item(&portable_foo.id).is_some());
        // The settings toggle turns the whole source off.
        let off = LaunchCfg { portable: false, ..Default::default() };
        *l.cfg.write().unwrap() = off;
        assert!(l.search("foo", 10).is_empty());
    }

    /// Ranking regression set: real-world queries against a mini catalog, asserting the top hit
    /// (and, for junk-prone queries, that nothing scattered matches at all).
    #[test]
    fn ranking_regressions() {
        let mut items = vec![
            item("app:ron", Kind::App, "Rise of Nations", &["RiseOfNations"]),
            item("app:ps", Kind::App, "Adobe Photoshop 2025", &["Photoshop"]),
            item("app:chrome", Kind::App, "Google Chrome", &["chrome"]),
            item("app:vsc", Kind::App, "Visual Studio Code", &["Code"]),
            item("app:term", Kind::App, "Windows Terminal", &["wt"]),
            item("app:macro", Kind::App, "Macro Buttons", &[]),
            item("set:hide", Kind::Setting, "Auto hide the taskbar", &["taskbar"]),
        ];
        aliases::apply(&mut items, &Default::default());
        let l = launcher_with(items, LaunchCfg::default());
        let top = |q: &str| l.search(q, 5).first().map(|h| h.item.id.clone());
        assert_eq!(top("atos"), None);
        assert_eq!(top("chrome").as_deref(), Some("app:chrome"));
        assert_eq!(top("chrm").as_deref(), Some("app:chrome"));
        assert_eq!(top("chrmoe").as_deref(), Some("app:chrome"));
        assert_eq!(top("vsc").as_deref(), Some("app:vsc"));
        assert_eq!(top("ps").as_deref(), Some("app:ps"));
        assert_eq!(top("wt").as_deref(), Some("app:term"));
        assert_eq!(top("term").as_deref(), Some("app:term"));
    }

    #[test]
    fn apps_win_ties_over_settings() {
        let items = vec![item("set:display", Kind::Setting, "Display", &[]), item("app:display", Kind::App, "Display", &[])];
        let l = launcher_with(items, LaunchCfg::default());
        assert_eq!(l.search("display", 10).first().map(|h| h.item.id.as_str()), Some("app:display"));
    }

    /// The p99 budget: 1000 searches over a 1500-item catalog must stay well under 2 s.
    #[test]
    fn search_is_fast_over_a_realistic_catalog() {
        let mut items = Vec::new();
        for i in 0..500 {
            items.push(item(&format!("app:a{i}"), Kind::App, &format!("App Number {i}"), &["shortcut"]));
            items.push(item(&format!("set:s{i}"), Kind::Setting, &format!("Setting Page {i}"), &["area", "keywords"]));
            items.push(item(&format!("cmd:c{i}"), Kind::Command, &format!("Command {i}"), &["power", "action"]));
        }
        let l = launcher_with(items, LaunchCfg::default());
        // Wall-clock under `cargo test` is noisy (other tests run in parallel), so take the
        // best of three runs — contention only ever adds time.
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let started = Instant::now();
            for n in 0..1000 {
                let q = ["mpv", "app", "set", "com", "x"][n % 5];
                let _ = l.search(q, 30);
            }
            best = best.min(started.elapsed());
        }
        eprintln!("1000 launch searches over 1500 items: {best:?} (best of 3)");
        // The PRD budget (< 2 ms p99) applies to the shipped release build. Debug builds run
        // ~5x slower and share the CPU with the rest of the suite, so the debug gate is
        // looser — a real regression (e.g. an accidental O(n^2)) blows far past it anyway.
        let budget = if cfg!(debug_assertions) { Duration::from_secs(6) } else { Duration::from_secs(2) };
        assert!(best < budget, "searches took {best:?}");
    }
}
