//! Daily "is there a newer release?" check. It only notifies (tray item + Settings line) and opens
//! the GitHub release page in the browser; nothing is downloaded or installed (releases are not
//! signed, so there is no auto-update).
//!
//! The request is a plain anonymous GET of the public releases list: no version, no machine or
//! user id, a generic User-Agent. GitHub of course sees the IP address, like any web request.
//! The user can turn the check off; while off, nothing is sent. State: `%LOCALAPPDATA%\winseek\updates.json`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::menu::MenuItem;
use tauri::{AppHandle, Emitter, Manager};

const API: &str = "https://api.github.com/repos/ach-raf/winseek/releases?per_page=10";
const PAGE_PREFIX: &str = "https://github.com/ach-raf/winseek/";
const RELEASES_PAGE: &str = "https://github.com/ach-raf/winseek/releases";
const FILE: &str = "updates.json";
const DAY_SECS: u64 = 24 * 3600;
/// The first check waits so it never competes with start-up.
const FIRST_DELAY: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_secs(3600);
const IDLE_LABEL: &str = "Check for updates";

/// Tray menu item showing the update state; kept in managed state.
pub struct TrayItem(pub MenuItem<tauri::Wry>);
/// Serialises check runs (the daily thread and a manual check).
struct Gate(Mutex<()>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Prefs {
    pub enabled: bool,
    /// Unix seconds of the last successful check.
    pub last_check: u64,
    /// Newest release seen and its page, when it is newer than this build.
    pub latest: Option<String>,
    pub url: Option<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { enabled: true, last_check: 0, latest: None, url: None }
    }
}

fn path() -> Option<PathBuf> {
    crate::tier1::data_root().map(|d| d.join(FILE))
}

fn load() -> Prefs {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(p: &Prefs) -> Result<(), String> {
    let f = path().ok_or("no %LOCALAPPDATA%")?;
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(f, serde_json::to_string_pretty(p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// `v0.2.19`, `0.2.19-beta.1` -> (0, 2, 19). The pre-release suffix is ignored.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut it = core.split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next().map_or(Some(0), |x| x.parse().ok())?;
    let c = it.next().map_or(Some(0), |x| x.parse().ok())?;
    it.next().is_none().then_some((a, b, c))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub tag: String,
    pub url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
}

/// Highest-versioned non-draft release in a GitHub releases list. Its page URL is only trusted
/// when it points at this project; otherwise the generic releases page is used.
pub fn pick_release(body: &str) -> Option<Release> {
    let list: Vec<GhRelease> = serde_json::from_str(body).ok()?;
    let best = list.into_iter().filter(|r| !r.draft).filter_map(|r| parse_version(&r.tag_name).map(|v| (v, r))).max_by_key(|(v, _)| *v)?.1;
    let url = if best.html_url.starts_with(PAGE_PREFIX) { best.html_url } else { RELEASES_PAGE.to_string() };
    Some(Release { tag: best.tag_name, url })
}

pub fn due(last_check: u64, now: u64) -> bool {
    last_check == 0 || now.saturating_sub(last_check) >= DAY_SECS
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn fetch() -> Result<Option<Release>, String> {
    let body = ureq::get(API)
        .set("User-Agent", "winseek-update-check")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    Ok(pick_release(&body))
}

fn tray_label(p: &Prefs) -> String {
    match &p.latest {
        Some(tag) => format!("Update available: {tag} (open release page)"),
        None => IDLE_LABEL.to_string(),
    }
}

fn refresh_tray(app: &AppHandle, p: &Prefs) {
    if let Some(i) = app.try_state::<TrayItem>() {
        let _ = i.0.set_text(tray_label(p));
    }
}

/// Ask GitHub now (ignores the 24 h spacing, honours nothing but the caller's intent), record the
/// result and tell the UI. Returns the refreshed state.
fn check(app: &AppHandle) -> Result<Prefs, String> {
    let gate = app.state::<Gate>();
    let _g = gate.0.lock().unwrap_or_else(|e| e.into_inner());
    let current = app.package_info().version.to_string();
    let found = fetch()?;
    let mut p = load();
    p.last_check = now_secs();
    match found.filter(|r| is_newer(&r.tag, &current)) {
        Some(r) => {
            p.latest = Some(r.tag);
            p.url = Some(r.url);
        }
        None => {
            p.latest = None;
            p.url = None;
        }
    }
    save(&p)?;
    refresh_tray(app, &p);
    let _ = app.emit("update-state", ());
    Ok(p)
}

/// Open the release page of the known newer version (or the releases page).
pub fn open_release_page(app: &AppHandle) {
    let p = load();
    let url = p.url.filter(|u| u.starts_with(PAGE_PREFIX)).unwrap_or_else(|| RELEASES_PAGE.to_string());
    let _ = std::process::Command::new("explorer.exe").arg(url).spawn();
    let _ = app; // kept for symmetry with the tray handler
}

/// Tray click on the update item: open the page when an update is known, else check now.
pub fn tray_clicked(app: &AppHandle) {
    if load().latest.is_some() {
        open_release_page(app);
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || match check(&app) {
        Ok(p) if p.latest.is_some() => open_release_page(&app),
        Ok(_) => {
            if let Some(i) = app.try_state::<TrayItem>() {
                let _ = i.0.set_text("You are up to date");
            }
        }
        Err(e) => eprintln!("winseek: update check failed: {e}"),
    });
}

/// Start the daily checker. Does nothing on the network while the check is off.
pub fn spawn(app: AppHandle) {
    app.manage(Gate(Mutex::new(())));
    refresh_tray(&app, &load());
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_DELAY);
        loop {
            let p = load();
            if p.enabled && due(p.last_check, now_secs()) {
                if let Err(e) = check(&app) {
                    eprintln!("winseek: update check failed: {e}"); // retried at the next poll
                }
            }
            std::thread::sleep(POLL);
        }
    });
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateState {
    pub enabled: bool,
    pub current: String,
    pub latest: Option<String>,
    pub url: Option<String>,
    pub last_check: u64,
}

fn state(app: &AppHandle, p: Prefs) -> UpdateState {
    UpdateState { enabled: p.enabled, current: app.package_info().version.to_string(), latest: p.latest, url: p.url, last_check: p.last_check }
}

#[tauri::command]
pub fn updates_get(app: AppHandle) -> UpdateState {
    state(&app, load())
}

#[tauri::command]
pub fn updates_set_enabled(app: AppHandle, enabled: bool) -> Result<UpdateState, String> {
    let mut p = load();
    p.enabled = enabled;
    save(&p)?;
    Ok(state(&app, p))
}

/// "Check now": works even while the daily check is off, because the user asked for it.
#[tauri::command]
pub async fn updates_check_now(app: AppHandle) -> Result<UpdateState, String> {
    let a = app.clone();
    let p = crate::commands::blocking(move || check(&a)).await??;
    Ok(state(&app, p))
}

#[tauri::command]
pub fn updates_open_page(app: AppHandle) {
    open_release_page(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(parse_version("v0.2.19"), Some((0, 2, 19)));
        assert_eq!(parse_version("0.3"), Some((0, 3, 0)));
        assert_eq!(parse_version("1.0.0-rc.1"), Some((1, 0, 0)));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("v0.2.19", "0.2.18"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.2.18", "0.2.18"));
        assert!(!is_newer("0.2.17", "0.2.18"));
        assert!(!is_newer("junk", "0.2.18"));
    }

    #[test]
    fn picks_the_highest_non_draft_release() {
        let body = r#"[
          {"tag_name":"v0.2.18","html_url":"https://github.com/ach-raf/winseek/releases/tag/v0.2.18","draft":false},
          {"tag_name":"v0.3.0","html_url":"https://github.com/ach-raf/winseek/releases/tag/v0.3.0","draft":true},
          {"tag_name":"v0.2.19","html_url":"https://github.com/ach-raf/winseek/releases/tag/v0.2.19","draft":false,"prerelease":true},
          {"tag_name":"nightly","html_url":"https://github.com/x","draft":false}
        ]"#;
        let r = pick_release(body).unwrap();
        assert_eq!(r.tag, "v0.2.19");
        assert_eq!(r.url, "https://github.com/ach-raf/winseek/releases/tag/v0.2.19");
    }

    #[test]
    fn a_foreign_release_url_is_replaced() {
        let body = r#"[{"tag_name":"v9.0.0","html_url":"https://evil.example/x","draft":false}]"#;
        assert_eq!(pick_release(body).unwrap().url, RELEASES_PAGE);
        assert_eq!(pick_release("[]"), None);
        assert_eq!(pick_release("not json"), None);
    }

    #[test]
    fn daily_spacing_and_defaults() {
        assert!(due(0, 100));
        assert!(!due(1000, 1000 + DAY_SECS - 1));
        assert!(due(1000, 1000 + DAY_SECS));
        assert!(Prefs::default().enabled);
        // an old file without the new fields still loads
        let p: Prefs = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!p.enabled && p.latest.is_none());
        assert_eq!(tray_label(&p), IDLE_LABEL);
        let q = Prefs { latest: Some("v0.2.19".into()), ..Prefs::default() };
        assert!(tray_label(&q).contains("v0.2.19"));
    }
}
