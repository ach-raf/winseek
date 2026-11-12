//! Popup window lifecycle: pre-warmed hidden, shown on the monitor under the cursor.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

pub const LABEL: &str = "popup";
static SHOWN_AT_MS: AtomicU64 = AtomicU64::new(0);
static MODAL: AtomicBool = AtomicBool::new(false);
/// Where the user last dragged the popup (outer top-left), so it reopens there.
static USER_POS: Mutex<Option<(i32, i32)>> = Mutex::new(None);
/// When the current window drag was started, so the blur it triggers does not hide the popup.
static DRAG_AT_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// A reposition/resize we do ourselves (view switch) must not be remembered as a user drag.
pub fn mark_programmatic_move() {
    SHOWN_AT_MS.store(now_ms(), Ordering::Relaxed);
}

pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

/// Our own reposition right after `show` must not count as a user drag.
const SETTLE_MS: u64 = 500;

pub(crate) fn place_on_cursor_monitor(w: &WebviewWindow) {
    let Ok(cursor) = w.cursor_position() else { return };
    let mon = w
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .or_else(|| w.primary_monitor().ok().flatten());
    let (Some(mon), Ok(size)) = (mon, w.outer_size()) else { return };
    let (mp, ms) = (mon.position(), mon.size());
    let (mw, mh) = (ms.width as i32, ms.height as i32);
    let (ww, wh) = (size.width as i32, size.height as i32);
    // A dragged position counts while the window's centre is still on this monitor, clamped so a
    // resolution change cannot strand the popup off-screen. A different monitor re-centers Spotlight-style.
    let saved = USER_POS.lock().ok().and_then(|g| *g).filter(|(x, y)| {
        let (cx, cy) = (x + ww / 2, y + wh / 2);
        cx >= mp.x && cx < mp.x + mw && cy >= mp.y && cy < mp.y + mh
    });
    let (x, y) = saved
        .map(|(x, y)| {
            let vis = 120.min(ww);
            (
                x.clamp(mp.x + vis - ww, mp.x + mw - vis),
                y.clamp(mp.y, mp.y + mh - 60),
            )
        })
        .unwrap_or_else(|| {
            // Spotlight-style: centred horizontally, upper third rather than dead centre.
            (
                mp.x + (mw - ww) / 2,
                mp.y + ((mh - wh) as f64 * 0.28) as i32,
            )
        });
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

/// Dead centre of the work area on the monitor under the cursor — where workspace windows open
/// (the full-mode window, the library window), no matter which screen or taskbar. The compact
/// popup keeps its Spotlight upper third; a tall workspace anchored there would sit high.
pub(crate) fn place_centered_on_cursor_monitor(w: &WebviewWindow) {
    let Ok(cursor) = w.cursor_position() else { return };
    let mon = w
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .or_else(|| w.primary_monitor().ok().flatten());
    let (Some(mon), Ok(size)) = (mon, w.outer_size()) else { return };
    let area = mon.work_area();
    let (aw, ah) = (area.size.width as i32, area.size.height as i32);
    let (ww, wh) = (size.width as i32, size.height as i32);
    let x = area.position.x + ((aw - ww) / 2).max(0);
    let y = area.position.y + ((ah - wh) / 2).max(0);
    mark_programmatic_move(); // our own set_position must not be remembered as a user drag
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

/// Starts a native window drag from the search bar (UI command, invoked as `move_popup`). The
/// drag loop can fire a transient focus loss on Windows, so the time is armed for the blur guard.
#[tauri::command]
pub fn move_popup(w: WebviewWindow) {
    DRAG_AT_MS.store(now_ms(), Ordering::Relaxed);
    let _ = w.start_dragging();
}

/// UI command, invoked as `minimize_popup`: the full window's minimize button.
#[tauri::command]
pub fn minimize_popup(w: WebviewWindow) {
    let _ = w.minimize();
}

/// Records a drag the user made while the popup was visible.
pub fn on_moved(w: &tauri::Window, pos: PhysicalPosition<i32>) {
    if w.label() != LABEL {
        return; // the saved popup position belongs to the popup only (the library window moves too)
    }
    if now_ms().saturating_sub(SHOWN_AT_MS.load(Ordering::Relaxed)) <= SETTLE_MS || crate::view::is_full() {
        return;
    }
    if w.is_visible().unwrap_or(false) {
        if let Ok(mut g) = USER_POS.lock() {
            *g = Some((pos.x, pos.y));
        }
    }
}

pub fn show(app: &AppHandle) {
    let Some(w) = window(app) else { return };
    crate::view::begin_show(app, &w); // default view: size + placement
    let _ = w.unminimize(); // minimized (the full window's minimize button): SW_SHOW alone would leave it hidden
    SHOWN_AT_MS.store(now_ms(), Ordering::Relaxed);
    // Tell the UI first: the (already-running, hidden) page focuses its input while the OS is still
    // mapping the window, so the caret is live the moment the first frame appears.
    let _ = app.emit_to(LABEL, "popup-shown", ());
    let _ = w.show();
    let _ = w.set_focus();
}

pub fn hide(app: &AppHandle) {
    if let Some(w) = window(app) {
        let _ = w.hide();
    }
}

/// UI command, invoked as `open_popup`: the library's back button returns to the popup
/// (the library window then hides itself, mirroring how the popup hides on open_library).
#[tauri::command]
pub fn open_popup(app: AppHandle) {
    show(&app);
}

pub fn toggle(app: &AppHandle) {
    match window(app).map(|w| w.is_visible().unwrap_or(false)) {
        Some(true) => hide(app),
        Some(false) => show(app),
        None => {}
    }
}

/// Suppress hide-on-blur while a shell context menu or drag owns the foreground.
pub fn set_modal(on: bool) {
    MODAL.store(on, Ordering::Relaxed);
}

/// Clears the blur guard on drop (all exit paths).
pub struct ModalGuard;

impl ModalGuard {
    pub fn new() -> Self {
        set_modal(true);
        Self
    }
}

impl Drop for ModalGuard {
    fn drop(&mut self) {
        set_modal(false);
    }
}

/// Focus loss hides the popup, except for the brief window right after showing
/// (the OS may bounce focus while the window is being activated).
pub fn on_blur(app: &AppHandle) {
    if MODAL.load(Ordering::Relaxed) {
        return;
    }
    if now_ms().saturating_sub(DRAG_AT_MS.load(Ordering::Relaxed)) <= 400 {
        return;
    }
    if now_ms().saturating_sub(SHOWN_AT_MS.load(Ordering::Relaxed)) > 250 {
        hide(app);
    }
}
