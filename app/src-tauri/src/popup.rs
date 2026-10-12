//! Popup window lifecycle: pre-warmed hidden, shown on the monitor under the cursor.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

pub const LABEL: &str = "popup";
static SHOWN_AT_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

fn center_on_cursor_monitor(w: &WebviewWindow) {
    let Ok(cursor) = w.cursor_position() else { return };
    let mon = w
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .or_else(|| w.primary_monitor().ok().flatten());
    let (Some(mon), Ok(size)) = (mon, w.outer_size()) else { return };
    let (mp, ms) = (mon.position(), mon.size());
    let x = mp.x + (ms.width as i32 - size.width as i32) / 2;
    // Spotlight-style: upper third rather than dead center.
    let y = mp.y + ((ms.height as i32 - size.height as i32) as f64 * 0.28) as i32;
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

pub fn show(app: &AppHandle) {
    let Some(w) = window(app) else { return };
    center_on_cursor_monitor(&w);
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

/// Focus loss hides the popup, except for the brief window right after showing
/// (the OS may bounce focus while the window is being activated).
pub fn on_blur(app: &AppHandle) {
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
