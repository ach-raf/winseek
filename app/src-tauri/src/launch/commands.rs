//! System commands in the launcher: lock, sleep/hibernate, restart, shutdown, sign out,
//! recycle bin, and winseek's own panels (PRD R4). Win32 calls live here; the catalog itself
//! is plain data. Win32 APIs first, `shutdown.exe` only as the documented fallback.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, ERROR_NOT_ALL_ASSIGNED};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY, LUID_AND_ATTRIBUTES,
};
use windows::Win32::System::Power::{GetPwrCapabilities, SetSuspendState, SYSTEM_POWER_CAPABILITIES};
use windows::Win32::System::Shutdown::{
    ExitWindowsEx, LockWorkStation, EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EXIT_WINDOWS_FLAGS,
    SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI};

use super::item::{Kind, LaunchItem, Target};

/// The command catalog. `hibernate_available` gates the hibernate row (no hiberfile -> no item).
pub fn catalog(hibernate_available: bool) -> Vec<LaunchItem> {
    let mk = |name: &str, title: &str, subtitle: &str, keywords: &[&str]| LaunchItem {
        id: format!("cmd:{name}"),
        kind: Kind::Command,
        title: title.into(),
        subtitle: subtitle.into(),
        keywords: keywords.iter().map(|k| k.to_string()).collect(),
        target: Target::Cmd { name: name.into() },
        icon: None,
        admin_ok: false,
    };
    let mut items = vec![
        mk("lock", "Lock", "Locks this PC", &["lock screen", "lock pc"]),
        mk("sleep", "Sleep", "Puts this PC to sleep", &["standby"]),
        mk("restart", "Restart", "Restarts this PC", &["reboot", "reboot pc"]),
        mk("shutdown", "Shut down", "Shuts this PC down", &["shutdown", "power off", "turn off"]),
        mk("signout", "Sign out", "Signs you out", &["log off", "logout", "logoff"]),
        mk("empty_bin", "Empty Recycle Bin", "Empties the Recycle Bin", &["trash", "recycle", "clear bin", "empty bin"]),
        mk("open_bin", "Open Recycle Bin", "Opens the Recycle Bin", &["trash", "recycle", "bin"]),
        mk("winseek_settings", "winseek Settings", "Opens winseek's settings", &["preferences", "options", "configure"]),
        mk("winseek_library", "winseek Library", "Opens winseek's library", &["photos", "images", "collections"]),
    ];
    if hibernate_available {
        items.insert(2, mk("hibernate", "Hibernate", "Hibernates this PC", &["hibernate pc", "deep sleep"]));
    }
    items
}

/// Commands that must not fire on a single Enter (PRD §4.2: second Enter confirms).
pub fn needs_confirm(id: &str) -> bool {
    matches!(id, "cmd:restart" | "cmd:shutdown" | "cmd:signout" | "cmd:empty_bin")
}

/// True when Windows can hibernate (hiberfile present and S4 supported).
pub fn hibernate_available() -> bool {
    let mut caps = SYSTEM_POWER_CAPABILITIES::default();
    // SAFETY: plain out-struct call.
    let ok = unsafe { GetPwrCapabilities(&mut caps) };
    ok && caps.HiberFilePresent && caps.SystemS4
}

/// Run one system command by its catalog id. `winseek_settings`/`winseek_library` are handled
/// by the UI itself and never reach here; if they do, report it instead of acting.
pub fn run(id: &str) -> Result<(), String> {
    eprintln!("winseek: launch command {id}");
    match id.strip_prefix("cmd:").unwrap_or(id) {
        "lock" => unsafe { LockWorkStation() }.map_err(|e| format!("lock: {e}")),
        "sleep" => suspend(false),
        "hibernate" => suspend(true),
        "restart" => exit_windows(EWX_REBOOT).or_else(|e| fallback_shutdown("/r", e)),
        "shutdown" => exit_windows(EWX_POWEROFF).or_else(|e| fallback_shutdown("/s", e)),
        "signout" => exit_windows(EWX_LOGOFF).or_else(|e| fallback_shutdown("/l", e)),
        "empty_bin" => empty_bin(),
        "open_bin" => std::process::Command::new("explorer.exe")
            .arg("shell:RecycleBinFolder")
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("open recycle bin: {e}")),
        "winseek_settings" | "winseek_library" => Err(format!("{id} is handled by the UI, not commands::run")),
        other => Err(format!("unknown command id {other}")),
    }
}

fn suspend(hibernate: bool) -> Result<(), String> {
    enable_shutdown_privilege("sleep/hibernate")?;
    // SAFETY: documented power call; `force=false` keeps the app-friendly graceful path.
    // Returns false when the request is refused (no privilege, wake-source rules).
    if unsafe { SetSuspendState(hibernate, false, false) } {
        Ok(())
    } else {
        Err(format!("SetSuspendState(hibernate={hibernate}) was refused"))
    }
}

fn exit_windows(flags: EXIT_WINDOWS_FLAGS) -> Result<(), String> {
    enable_shutdown_privilege("exit windows")?;
    // SAFETY: planned shutdown/restart/logoff with an explicit reason code.
    unsafe { ExitWindowsEx(flags, SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED) }
        .map_err(|e| format!("ExitWindowsEx: {e}"))
}

/// `shutdown.exe` fallback when the API path is refused (e.g. missing privilege on odd setups).
fn fallback_shutdown(switch: &str, api_error: String) -> Result<(), String> {
    eprintln!("winseek: ExitWindowsEx failed ({api_error}); falling back to shutdown.exe {switch}");
    std::process::Command::new("shutdown.exe")
        .args([switch, "/t", "0"])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("shutdown.exe fallback: {e} (api: {api_error})"))
}

fn empty_bin() -> Result<(), String> {
    // SAFETY: no hwnd, no root path: empties all recycle bins without confirmation UI.
    let r = unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI) };
    match r {
        Ok(()) => Ok(()),
        Err(e) if e.code() == windows::core::HRESULT(0x8000_FFFFu32 as i32) => {
            Ok(()) // E_UNEXPECTED = already empty (documented shell behaviour)
        }
        Err(e) => Err(format!("empty recycle bin: {e}")),
    }
}

/// `SeShutdownPrivilege` is required for SetSuspendState/ExitWindowsEx on standard tokens.
/// AdjustTokenPrivileges returns success even when the privilege was NOT granted, so the
/// not-all-assigned case is logged but not fatal — the caller's real API call is the test.
fn enable_shutdown_privilege(why: &str) -> Result<(), String> {
    // SAFETY: all calls take valid handles/out structs; the token is closed on drop.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)
            .map_err(|e| format!("{why}: open process token: {e}"))?;
        let _token = TokenGuard(token);
        let mut tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: Default::default(),
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut tp.Privileges[0].Luid)
            .map_err(|e| format!("{why}: lookup SeShutdownPrivilege: {e}"))?;
        AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None)
            .map_err(|e| format!("{why}: adjust token: {e}"))?;
        if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
            eprintln!("winseek: {why}: SeShutdownPrivilege not granted; the system call will decide");
        }
    }
    Ok(())
}

struct TokenGuard(HANDLE);
impl Drop for TokenGuard {
    fn drop(&mut self) {
        // SAFETY: owned handle from OpenProcessToken, released once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::matcher;

    #[test]
    fn catalog_ids_are_unique_and_complete() {
        let items = catalog(true);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        let expected = [
            "cmd:lock",
            "cmd:sleep",
            "cmd:hibernate",
            "cmd:restart",
            "cmd:shutdown",
            "cmd:signout",
            "cmd:empty_bin",
            "cmd:open_bin",
            "cmd:winseek_settings",
            "cmd:winseek_library",
        ];
        assert_eq!(ids, expected);
        assert_eq!(catalog(false).iter().filter(|i| i.id == "cmd:hibernate").count(), 0);
    }

    #[test]
    fn confirm_gated_commands() {
        for id in ["cmd:restart", "cmd:shutdown", "cmd:signout", "cmd:empty_bin"] {
            assert!(needs_confirm(id), "{id} must confirm");
        }
        for id in ["cmd:lock", "cmd:sleep", "cmd:open_bin", "cmd:winseek_settings"] {
            assert!(!needs_confirm(id), "{id} must not confirm");
        }
    }

    fn best_for(q: &str) -> Option<String> {
        let items = catalog(true);
        items
            .iter()
            .filter_map(|i| matcher::score_item(q, i).map(|m| (m.score, i.id.clone())))
            .max_by_key(|(s, _)| *s)
            .map(|(_, id)| id)
    }

    #[test]
    fn shut_and_lock_resolve() {
        assert_eq!(best_for("shut").as_deref(), Some("cmd:shutdown"));
        assert_eq!(best_for("lock").as_deref(), Some("cmd:lock"));
        assert_eq!(best_for("empty recycle").as_deref(), Some("cmd:empty_bin"));
        assert_eq!(best_for("log off").as_deref(), Some("cmd:signout"));
    }
}
