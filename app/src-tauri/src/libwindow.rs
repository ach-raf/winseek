//! The library window: a normal resizable window created on first use and reused afterwards.
//! It loads the same UI bundle as the popup; `?view=library` selects the library screen.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "library";

/// Show the library window, creating it the first time (or after the user closed it).
pub fn open(app: &AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(LABEL) {
        let hidden = !w.is_visible().unwrap_or(false);
        let _ = w.unminimize();
        let _ = w.show();
        // A window that comes back (was closed to the popup or minimized) opens centred again;
        // one already on screen stays where the user put it.
        if hidden {
            crate::popup::place_centered_on_cursor_monitor(&w);
        }
        let _ = w.set_focus();
        return Ok(());
    }
    // Built hidden, placed centred on the monitor under the cursor, then shown — so it never
    // flashes at an OS-default (top-left-ish) spot before the real position lands.
    let w = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html?view=library".into()))
        .title("winseek Library")
        .inner_size(1100.0, 750.0)
        .min_inner_size(760.0, 520.0)
        .resizable(true)
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;
    crate::popup::place_centered_on_cursor_monitor(&w);
    let _ = w.show();
    let _ = w.set_focus();
    Ok(())
}

#[tauri::command]
pub async fn open_library(app: AppHandle) -> Result<(), String> {
    // The popup loses focus (and hides) as the library takes the foreground.
    open(&app)
}
