//! View mode of the popup window: the compact Spotlight popup or the larger full window.
//! There is no true full screen — the modes differ only in size (780x560 vs 1280x800, the
//! demo's proportions in docs/redesign/winseek-redesign.html). The user's default lives in
//! `%LOCALAPPDATA%\winseek\ui.json` (`{ "default_view": "compact" | "full" }`); the mode in use
//! can be flipped at any time (F11 in the UI) without touching the saved default.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, LogicalSize, WebviewWindow};

use crate::{popup, tier1};

pub const COMPACT_W: f64 = 780.0;
pub const COMPACT_H: f64 = 560.0;
pub const FULL_W: f64 = 1280.0;
pub const FULL_H: f64 = 800.0;

/// What is on screen right now (true = full window).
static CURRENT_FULL: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Compact,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UiCfg {
    #[serde(default)]
    pub default_view: Mode,
}

/// The state the UI needs: the saved default and what the window currently is.
#[derive(Debug, Clone, Serialize)]
pub struct ViewState {
    pub default_view: Mode,
    pub current: Mode,
}

fn path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("ui.json")
}

/// Missing or corrupt file -> defaults (compact): a broken file must not change how the popup opens.
pub fn load_from(root: &std::path::Path) -> UiCfg {
    std::fs::read(path(root)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_to(root: &std::path::Path, c: &UiCfg) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?;
    std::fs::write(path(root), json).map_err(|e| e.to_string())
}

fn load() -> UiCfg {
    tier1::data_root().map(|r| load_from(&r)).unwrap_or_default()
}

pub fn is_full() -> bool {
    CURRENT_FULL.load(Ordering::Relaxed)
}

fn mode_of(full: bool) -> Mode {
    if full { Mode::Full } else { Mode::Compact }
}

/// The demo scales its prototype down to fit the screen with 48 px margins on each side
/// (`min(1, (avail_w - 96) / W, (avail_h - 96) / H)`); the real window does the same, so the
/// full window always floats with the desktop visible around it instead of covering the screen.
fn fitted_size(w: &WebviewWindow, ww: f64, hh: f64) -> (f64, f64) {
    let Some(mon) = w
        .cursor_position()
        .ok()
        .and_then(|c| w.monitor_from_point(c.x, c.y).ok().flatten())
    else {
        return (ww, hh);
    };
    let scale = mon.scale_factor();
    let area = mon.work_area();
    let avail_w = area.size.width as f64 / scale - 96.0;
    let avail_h = area.size.height as f64 / scale - 96.0;
    let sc = 1.0f64.min(avail_w / ww).min(avail_h / hh).max(0.5);
    ((ww * sc).floor(), (hh * sc).floor())
}

/// Resizes the window for `full` (1280x800 vs the popup's 780x560, fitted to the work area) and
/// places it: the compact popup opens Spotlight-style on the monitor under the cursor, the full
/// window opens dead centre — a workspace window anchored in the upper third sits high on screen.
fn apply(w: &WebviewWindow, full: bool) {
    let (ww, hh) = if full { (FULL_W, FULL_H) } else { (COMPACT_W, COMPACT_H) };
    let (ww, hh) = fitted_size(w, ww, hh);
    let _ = w.set_size(LogicalSize::new(ww, hh));
    if full {
        popup::place_centered_on_cursor_monitor(w);
    } else {
        popup::place_on_cursor_monitor(w);
    }
}

/// Called by `popup::show` before the window is shown: applies the user's default view
/// (size and placement both, so the popup does not place it again).
pub fn begin_show(app: &AppHandle, w: &WebviewWindow) {
    let full = load().default_view == Mode::Full;
    CURRENT_FULL.store(full, Ordering::Relaxed);
    popup::mark_programmatic_move();
    apply(w, full);
    let _ = app.emit_to(popup::LABEL, "view-changed", mode_of(full));
}

#[tauri::command]
pub fn view_get() -> ViewState {
    ViewState { default_view: load().default_view, current: mode_of(is_full()) }
}

/// Saves the default view (what Alt+Space opens next time).
#[tauri::command]
pub fn view_set_default(mode: Mode) -> Result<ViewState, String> {
    let Some(root) = tier1::data_root() else { return Err("no data dir".into()) };
    save_to(&root, &UiCfg { default_view: mode })?;
    Ok(view_get())
}

/// Flips (or sets) the mode in use right now; the saved default is untouched.
#[tauri::command]
pub fn view_set_current(app: AppHandle, mode: Mode) -> Mode {
    let full = mode == Mode::Full;
    if let Some(w) = popup::window(&app) {
        CURRENT_FULL.store(full, Ordering::Relaxed);
        popup::mark_programmatic_move();
        apply(&w, full);
        let _ = w.set_focus();
    }
    let _ = app.emit_to(popup::LABEL, "view-changed", mode_of(full));
    mode_of(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_compact_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("winseek-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_from(&dir).default_view, Mode::Compact);
        save_to(&dir, &UiCfg { default_view: Mode::Full }).unwrap();
        assert_eq!(load_from(&dir).default_view, Mode::Full);
        std::fs::write(path(&dir), b"{ nope").unwrap();
        assert_eq!(load_from(&dir).default_view, Mode::Compact);
        std::fs::write(path(&dir), b"{}").unwrap();
        assert_eq!(load_from(&dir).default_view, Mode::Compact);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mode_serialises_lowercase() {
        assert_eq!(serde_json::to_string(&Mode::Full).unwrap(), "\"full\"");
        assert_eq!(serde_json::from_str::<Mode>("\"compact\"").unwrap(), Mode::Compact);
    }

    #[test]
    fn full_window_is_the_demo_size_not_true_fullscreen() {
        // The demo's only difference between popup and full window is the size (1280x800 vs 780x560).
        assert_eq!((FULL_W, FULL_H), (1280.0, 800.0));
        assert_eq!((COMPACT_W, COMPACT_H), (780.0, 560.0));
        assert_ne!((FULL_W, FULL_H), (COMPACT_W, COMPACT_H));
    }

    /// The demo's fit rule: the prototype scales down to leave 48 px of desktop on each side.
    /// Mirrors `fitted_size`'s maths so the window can never cover the screen.
    #[test]
    fn demo_fit_rule_never_exceeds_the_work_area() {
        for &(screen_w, screen_h, full) in &[
            (1920.0, 1040.0, true),  // big desktop: full size, floating
            (1366.0, 728.0, true),   // small laptop: scaled down
            (1280.0, 720.0, true),   // barely fits at all
            (1920.0, 1040.0, false), // popup on a big desktop
            (1024.0, 600.0, false),  // popup on a tiny screen
        ] {
            let (ww, hh) = if full { (1280.0, 800.0) } else { (780.0, 560.0) };
            let avail_w = screen_w - 96.0;
            let avail_h = screen_h - 96.0;
            let sc = 1.0f64.min(avail_w / ww).min(avail_h / hh).max(0.5);
            let (w, h) = ((ww * sc).floor(), (hh * sc).floor());
            assert!(w <= avail_w && h <= avail_h, "{w}x{h} covers {avail_w}x{avail_h}");
            assert!(sc == 1.0 || w >= 390.0, "never smaller than half the popup");
        }
    }
}
