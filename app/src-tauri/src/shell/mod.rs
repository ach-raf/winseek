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
use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName};

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
pub async fn copy_files(app: AppHandle, paths: Vec<String>) -> Result<(), String> {
    on_main(&app.clone(), move || {
        let (hwnd, _) = popup_hwnd(&app)?;
        clipboard::copy_files(hwnd, &paths)
    })
}

#[tauri::command]
pub async fn recycle(app: AppHandle, paths: Vec<String>) -> Result<(), String> {
    on_main(&app.clone(), move || {
        let (hwnd, _) = popup_hwnd(&app)?;
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
pub async fn show_context_menu(app: AppHandle, path: String, x: f64, y: f64) -> Result<String, String> {
    on_main(&app.clone(), move || {
        let (hwnd, w) = popup_hwnd(&app)?;
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
