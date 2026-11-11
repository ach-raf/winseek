//! View mode of the popup window: the compact Spotlight popup or a borderless full-screen
//! layout (the monitor's work area under the cursor). The user's default lives in
//! `%LOCALAPPDATA%\winseek\ui.json` (`{ "default_view": "compact" | "full" }`); the mode in use
//! can be flipped at any time (F11 in the UI) without touching the saved default.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, LogicalSize, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::{popup, tier1};

pub const COMPACT_W: f64 = 840.0;
pub const COMPACT_H: f64 = 540.0;

/// What is on screen right now (true = full screen).
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

/// Position + size (physical px) of the full-screen window: the whole work area of the monitor.
pub fn full_rect(work: (i32, i32, u32, u32)) -> (i32, i32, u32, u32) {
    work
}

/// Resizes (and, for full, repositions) the window for `full`; compact is centred by the popup.
fn apply(w: &WebviewWindow, full: bool) {
    if full {
        let mon = w
            .cursor_position()
            .ok()
            .and_then(|c| w.monitor_from_point(c.x, c.y).ok().flatten())
            .or_else(|| w.current_monitor().ok().flatten())
            .or_else(|| w.primary_monitor().ok().flatten());
        if let Some(m) = mon {
            let wa = m.work_area();
            let (x, y, ww, hh) = full_rect((wa.position.x, wa.position.y, wa.size.width, wa.size.height));
            let _ = w.set_position(PhysicalPosition::new(x, y));
            let _ = w.set_size(PhysicalSize::new(ww, hh));
        }
    } else {
        let _ = w.set_size(LogicalSize::new(COMPACT_W, COMPACT_H));
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
    fn full_rect_is_the_work_area() {
        assert_eq!(full_rect((-1920, 0, 1920, 1040)), (-1920, 0, 1920, 1040));
    }
}
