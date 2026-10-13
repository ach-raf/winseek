//! The library window: a normal resizable window created on first use and reused afterwards.
//! It loads the same UI bundle as the popup; `?view=library` selects the library screen.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "library";

/// Show the library window, creating it the first time (or after the user closed it).
pub fn open(app: &AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html?view=library".into()))
        .title("winseek Library")
        .inner_size(1100.0, 750.0)
        .min_inner_size(760.0, 520.0)
        .resizable(true)
        .center()
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn open_library(app: AppHandle) -> Result<(), String> {
    // The popup loses focus (and hides) as the library takes the foreground.
    open(&app)
}
