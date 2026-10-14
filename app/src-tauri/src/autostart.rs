//! "Start with Windows": the `winseek` value under HKCU\...\Run, pointing at this exe with
//! `--autostart` (which starts in the tray without showing the popup).
//!
//! Default on: the first run for a user registers it and writes `%LOCALAPPDATA%\winseek\autostart`
//! ("on"); while the marker says "on" the value is kept present and current (the uninstaller deletes
//! it). The Settings toggle writes "off" and nothing re-adds it.

use std::path::Path;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
    REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};

use crate::tier1;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("winseek");
pub const ARG: &str = "--autostart";
const MARKER: &str = "autostart";

/// The Run command line for `exe`.
pub fn command_line(exe: &Path) -> String {
    format!("\"{}\" {ARG}", exe.display())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn read_value() -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut len))
    };
    if r.0 != 0 {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

fn write_value(on: bool) -> Result<(), String> {
    let mut key = HKEY::default();
    unsafe {
        let r = RegCreateKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None);
        if r.0 != 0 {
            return Err(format!("open Run key: error {}", r.0));
        }
        let r = if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let data = wide(&command_line(&exe));
            let bytes = std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2);
            RegSetValueExW(key, VALUE, None, REG_SZ, Some(bytes))
        } else {
            match RegDeleteValueW(key, VALUE) {
                r if r == ERROR_FILE_NOT_FOUND => windows::Win32::Foundation::WIN32_ERROR(0),
                r => r,
            }
        };
        let _ = RegCloseKey(key);
        if r.0 != 0 {
            return Err(format!("write Run value: error {}", r.0));
        }
    }
    Ok(())
}

/// True when the Run value points at this exe.
fn enabled() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    read_value().is_some_and(|v| v.eq_ignore_ascii_case(&command_line(&exe)))
}

/// First run for this user: turn autostart on once (debug builds never register themselves).
pub fn init_default() {
    if cfg!(debug_assertions) {
        return;
    }
    let Some(root) = tier1::data_root() else { return };
    let marker = root.join(MARKER);
    match std::fs::read(&marker) {
        // the user turned it off in Settings
        Ok(m) if m == b"off" => {}
        // on: keep the value present and pointing at this exe (the uninstaller deletes it and a
        // reinstall may use another folder)
        Ok(_) => {
            if !enabled() {
                let _ = write_value(true);
            }
        }
        Err(_) => {
            if write_value(true).is_ok() {
                let _ = std::fs::create_dir_all(&root);
                let _ = std::fs::write(marker, b"on");
            }
        }
    }
}

#[tauri::command]
pub fn autostart_get() -> bool {
    enabled()
}

#[tauri::command]
pub fn autostart_set(on: bool) -> Result<bool, String> {
    write_value(on)?;
    if let Some(root) = tier1::data_root() {
        let _ = std::fs::create_dir_all(&root);
        let _ = std::fs::write(root.join(MARKER), if on { &b"on"[..] } else { &b"off"[..] });
    }
    Ok(enabled())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_is_quoted() {
        assert_eq!(
            command_line(Path::new(r"C:\Program Files\winseek\winseek.exe")),
            r#""C:\Program Files\winseek\winseek.exe" --autostart"#
        );
    }
}
