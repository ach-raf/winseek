//! Windows shell integration for the popup: icons/thumbnails, clipboard, file ops, context menu, drag.
//! Menu/drag/clipboard/file-op COM work runs on the main (STA) thread; icon work never does.

mod clipboard;
mod drag;
mod fileop;
pub mod icons;
mod menu;

use std::sync::mpsc;
use tauri::{AppHandle, WebviewWindow};
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SHObjectProperties, SHOP_FILEPATH};

/// Per-thread COM STA init; uninitialises on drop only if this call initialised it.
pub(crate) struct ComGuard(bool);

impl ComGuard {
    pub fn init() -> Self {
        // SAFETY: plain COM init; a failure (e.g. changed mode) just means we don't uninit.
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx in `init`.
            unsafe { CoUninitialize() };
        }
    }
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub(crate) fn shell_item(path: &str) -> Result<IShellItem, String> {
    let w = wide(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) }.map_err(|e| format!("{path}: {e}"))
}

pub(crate) fn popup_hwnd(app: &AppHandle) -> Result<(HWND, WebviewWindow), String> {
    let w = crate::popup::window(app).ok_or("popup window missing")?;
    let h = w.hwnd().map_err(|e| e.to_string())?;
    Ok((h, w))
}

/// HWND of the window that invoked a command (popup or library).
pub(crate) fn window_hwnd(w: &WebviewWindow) -> Result<HWND, String> {
    w.hwnd().map_err(|e| e.to_string())
}

/// Run `f` on the main thread (COM STA, message pump) and wait for its result.
pub(crate) fn on_main<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let _com = ComGuard::init();
        let _ = tx.send(f());
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn copy_files(app: AppHandle, window: WebviewWindow, paths: Vec<String>) -> Result<(), String> {
    on_main(&app.clone(), move || {
        let hwnd = window_hwnd(&window)?;
        clipboard::copy_files(hwnd, &paths)
    })
}

/// Text (and optionally an HTML twin, e.g. a table) to the clipboard.
#[tauri::command]
pub async fn copy_text_html(app: AppHandle, window: WebviewWindow, text: String, html: Option<String>) -> Result<(), String> {
    on_main(&app.clone(), move || {
        let hwnd = window_hwnd(&window)?;
        clipboard::copy_text_html(hwnd, &text, html.as_deref())
    })
}

#[tauri::command]
pub async fn recycle(app: AppHandle, window: WebviewWindow, paths: Vec<String>) -> Result<(), String> {
    on_main(&app.clone(), move || {
        let hwnd = window_hwnd(&window)?;
        fileop::recycle(hwnd, &paths)
    })
}

#[tauri::command]
pub async fn restore_recycled(app: AppHandle, paths: Vec<String>) -> Result<usize, String> {
    on_main(&app, move || fileop::restore_recycled(&paths))
}

#[tauri::command]
pub async fn rename_path(app: AppHandle, path: String, new_name: String) -> Result<String, String> {
    on_main(&app.clone(), move || {
        let (hwnd, _) = popup_hwnd(&app)?;
        fileop::rename(hwnd, &path, &new_name)
    })
}

#[tauri::command]
pub async fn show_context_menu(app: AppHandle, window: WebviewWindow, path: String, x: f64, y: f64) -> Result<String, String> {
    on_main(&app.clone(), move || {
        let hwnd = window_hwnd(&window)?;
        let w = window;
        let scale = w.scale_factor().map_err(|e| e.to_string())?;
        let _modal = crate::popup::ModalGuard::new();
        menu::show(hwnd, &path, x * scale, y * scale)
    })
}

#[tauri::command]
pub async fn start_drag(app: AppHandle, paths: Vec<String>) -> Result<String, String> {
    on_main(&app.clone(), move || {
        let (hwnd, _) = popup_hwnd(&app)?;
        let _modal = crate::popup::ModalGuard::new();
        drag::start(hwnd, &paths)
    })
}

/// The Windows Properties sheet for `path`. It is modeless and lives on the calling thread's
/// message loop, so it must be opened on the main thread — a worker thread would close it
/// the moment the call returns (PRD B3).
/// The plain-text clipboard contents, if any (for the clipboard hotkey).
pub fn clipboard_text(hwnd: HWND) -> Option<String> {
    clipboard::read_text(hwnd)
}

#[tauri::command]
pub async fn show_properties(app: AppHandle, window: WebviewWindow, path: String) -> Result<(), String> {
    on_main(&app, move || {
        let hwnd = window_hwnd(&window)?;
        let w = crate::shell::wide(&path);
        // SAFETY: NUL-terminated name; the sheet stays up on this thread's message pump.
        let ok = unsafe { SHObjectProperties(Some(hwnd), SHOP_FILEPATH, PCWSTR(w.as_ptr()), PCWSTR::null()) };
        if ok.as_bool() {
            Ok(())
        } else {
            Err(format!("Cannot open Properties for {path}"))
        }
    })
}

/// Destroys an HMENU on drop.
pub(crate) struct MenuGuard(pub windows::Win32::UI::WindowsAndMessaging::HMENU);

impl Drop for MenuGuard {
    fn drop(&mut self) {
        // SAFETY: owned menu handle, destroyed once.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyMenu(self.0);
        }
    }
}
