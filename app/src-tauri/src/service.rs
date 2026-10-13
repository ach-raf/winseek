//! Windows service helpers: query/start/install the `winseek-searchd` service, and read the
//! system accent palette.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED, ERROR_SERVICE_DOES_NOT_EXIST};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY};
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, StartServiceW, SC_HANDLE,
    SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STOPPED, SERVICE_STOP_PENDING,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

const SERVICE_NAME: PCWSTR = w!("winseek-searchd");

/// Unix-ms until which the source watcher should poll every 250 ms.
static FAST_POLL_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Ask the watcher to poll quickly for the next `secs` seconds.
pub fn kick_watcher(secs: u64) {
    FAST_POLL_UNTIL_MS.store(now_ms() + secs * 1000, Ordering::Relaxed);
}

pub fn fast_polling() -> bool {
    now_ms() < FAST_POLL_UNTIL_MS.load(Ordering::Relaxed)
}

struct ScHandle(SC_HANDLE);
impl Drop for ScHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

fn open_service(access: u32) -> Result<Option<ScHandle>, String> {
    unsafe {
        let scm = ScHandle(
            OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT).map_err(|e| e.to_string())?,
        );
        match OpenServiceW(scm.0, SERVICE_NAME, access) {
            Ok(h) => Ok(Some(ScHandle(h))),
            Err(e) if e.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Map a SERVICE_STATUS `dwCurrentState` to the UI string.
fn state_name(current: u32) -> &'static str {
    match current {
        x if x == SERVICE_STOPPED.0 => "stopped",
        x if x == SERVICE_START_PENDING.0 => "starting",
        x if x == SERVICE_RUNNING.0 => "running",
        x if x == SERVICE_STOP_PENDING.0 => "stopping",
        _ => "unknown",
    }
}

fn query_state() -> &'static str {
    let h = match open_service(SERVICE_QUERY_STATUS) {
        Ok(Some(h)) => h,
        Ok(None) => return "not_installed",
        Err(_) => return "unknown",
    };
    let mut st = SERVICE_STATUS::default();
    match unsafe { QueryServiceStatus(h.0, &mut st) } {
        Ok(()) => state_name(st.dwCurrentState.0),
        Err(_) => "unknown",
    }
}

/// `"not_installed" | "stopped" | "starting" | "running" | "stopping" | "unknown"`.
#[tauri::command]
pub fn service_state() -> String {
    query_state().to_string()
}

#[tauri::command]
pub fn start_service() -> Result<(), String> {
    let h = open_service(SERVICE_START)?.ok_or("Name service is not installed")?;
    match unsafe { StartServiceW(h.0, None) } {
        Ok(()) => {}
        // ERROR_SERVICE_ALREADY_RUNNING (1056)
        Err(e) if e.code() == windows::core::HRESULT::from_win32(1056) => {}
        Err(e) => return Err(format!("Could not start the name service: {e}")),
    }
    kick_watcher(10);
    Ok(())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Run `searchd.exe --install` elevated (one UAC prompt) and wait for it.
fn install_blocking() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let searchd = exe.parent().map(|p| p.join("searchd.exe")).filter(|p| p.is_file());
    let Some(searchd) = searchd else {
        return Err("searchd.exe not found next to winseek-app.exe".into());
    };
    let file = wide(&searchd.to_string_lossy());
    let dir = searchd.parent().map(|p| wide(&p.to_string_lossy()));
    let params = wide("--install");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        lpDirectory: dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        if ShellExecuteExW(&mut info).is_err() {
            let err = GetLastError();
            return Err(if err == ERROR_CANCELLED {
                "Installation cancelled".to_string()
            } else {
                format!("Could not start the installer (error {})", err.0)
            });
        }
        let proc = info.hProcess;
        if proc.is_invalid() {
            return Err("Installer did not start".into());
        }
        WaitForSingleObject(proc, INFINITE);
        let mut code = 0u32;
        let got = GetExitCodeProcess(proc, &mut code);
        let _ = CloseHandle(proc);
        got.map_err(|e| e.to_string())?;
        if code != 0 {
            return Err(format!(
                "searchd --install failed (exit {code}); see %ProgramData%\\winseek\\searchd.log"
            ));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn install_service() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(install_blocking).await.map_err(|e| e.to_string())??;
    kick_watcher(10);
    Ok(())
}

/// Parse the 32-byte AccentPalette blob (8 x RGBA) into 8 `#rrggbb` strings.
pub fn parse_accent_palette(b: &[u8]) -> Option<Vec<String>> {
    if b.len() != 32 {
        return None;
    }
    Some(b.as_chunks::<4>().0.iter().map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])).collect())
}

fn read_accent_bytes() -> Option<Vec<u8>> {
    let mut buf = [0u8; 64];
    let mut len = buf.len() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if r.0 != 0 {
        return None;
    }
    Some(buf[..len as usize].to_vec())
}

#[tauri::command]
pub fn accent_palette() -> Option<Vec<String>> {
    parse_accent_palette(&read_accent_bytes()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_parses() {
        let mut b = Vec::new();
        for i in 0..8u8 {
            b.extend_from_slice(&[i, 0x80, 0xff, 0x00]);
        }
        let p = parse_accent_palette(&b).unwrap();
        assert_eq!(p.len(), 8);
        assert_eq!(p[0], "#0080ff");
        assert_eq!(p[7], "#0780ff");
    }

    #[test]
    fn palette_wrong_length() {
        assert!(parse_accent_palette(&[0; 31]).is_none());
        assert!(parse_accent_palette(&[0; 36]).is_none());
        assert!(parse_accent_palette(&[]).is_none());
    }

    #[test]
    fn state_is_allowed() {
        let s = service_state();
        assert!(["not_installed", "stopped", "starting", "running", "stopping", "unknown"].contains(&s.as_str()), "{s}");
    }
}
