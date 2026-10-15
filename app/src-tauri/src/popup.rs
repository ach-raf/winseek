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

pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

/// Our own reposition right after `show` must not count as a user drag.
const SETTLE_MS: u64 = 500;

fn place_on_cursor_monitor(w: &WebviewWindow) {
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

/// Starts a native window drag from the search bar (UI command, invoked as `move_popup`). The
/// drag loop can fire a transient focus loss on Windows, so the time is armed for the blur guard.
#[tauri::command]
pub fn move_popup(w: WebviewWindow) {
    DRAG_AT_MS.store(now_ms(), Ordering::Relaxed);
    let _ = w.start_dragging();
}

/// Records a drag the user made while the popup was visible.
pub fn on_moved(w: &tauri::Window, pos: PhysicalPosition<i32>) {
    if now_ms().saturating_sub(SHOWN_AT_MS.load(Ordering::Relaxed)) <= SETTLE_MS {
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
    place_on_cursor_monitor(&w);
    SHOWN_AT_MS.store(now_ms(), Ordering::Relaxed);
    let _ = w.show();
    let _ = w.set_focus();
    let _ = app.emit("popup-shown", ());
}

pub fn hide(app: &AppHandle) {
    if let Some(w) = window(app) {
        let _ = w.hide();
    }
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

#[cfg(windows)]
fn windows_build() -> u32 {
    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }
    let mut v = OsVersionInfo {
        size: std::mem::size_of::<OsVersionInfo>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        csd: [0; 128],
    };
    // SAFETY: struct is correctly sized/initialised for RtlGetVersion.
    unsafe { RtlGetVersion(&mut v) };
    v.build
}

/// Mica on Win11 (build >= 22000), acrylic on Win10; failures are ignored.
pub fn apply_backdrop(w: &WebviewWindow) {
    #[cfg(windows)]
    {
        if windows_build() >= 22000 {
            let _ = window_vibrancy::apply_mica(w, None);
        } else {
            let _ = window_vibrancy::apply_acrylic(w, Some((24, 24, 28, 120)));
        }
    }
    #[cfg(not(windows))]
    let _ = w;
}
