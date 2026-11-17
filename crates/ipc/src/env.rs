//! PATH-environment helpers shared by `winseek-cli path …` and the app's Settings toggle:
//! add / remove a directory from the user (HKCU) or machine (HKLM) PATH and announce the
//! change so running shells pick it up for new children. Registry access goes through the
//! `winreg` crate: the previous hand-rolled FFI opened keys with `RegCreateKeyExW` (so a
//! status *read* created the key) and rewrote every Path as REG_EXPAND_SZ (so a REG_SZ
//! Path holding a literal `%` changed meaning).
//!
//! The pure string half ([`path_contains`], [`path_with`]) is where the tests live; the
//! registry wrappers around it are thin. Reads never create anything; writes keep the
//! value's stored type and are retried with a re-read between attempts, so a concurrent
//! installer write converges instead of dropping an entry.

use std::ffi::c_void;
use std::io;
use std::time::Duration;

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ};
use winreg::types::{FromRegValue, ToRegValue};
use winreg::{RegKey, RegValue};

type Handle = *mut c_void;

const USER_SUBKEY: &str = "Environment";
const MACHINE_SUBKEY: &str = "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment";
const VALUE_NAME: &str = "Path";

/// Read-modify-write is not atomic in the registry: an installer and `winseek-cli path add`
/// racing one another can lose an entry. A few attempts that re-read between tries converge
/// (if the other side already added the same directory, the next attempt reports "already
/// there" instead of writing a duplicate).
const WRITE_ATTEMPTS: u32 = 3;
const RETRY_BACKOFF: Duration = Duration::from_millis(150);

const HWND_BROADCAST: Handle = handle(0xffff);
const WM_SETTINGCHANGE: u32 = 0x001a;
const SMTO_ABORTIFHUNG: u32 = 0x0002;

const fn handle(v: u64) -> Handle {
    // const-evaluated integer-to-pointer cast; the predefined registry handles are constants.
    v as usize as Handle
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[link(name = "kernel32")]
extern "system" {
    fn ExpandEnvironmentStringsW(src: *const u16, dst: *mut u16, size: u32) -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn SendMessageTimeoutW(
        hwnd: Handle,
        msg: u32,
        wparam: usize,
        lparam: *const u16,
        flags: u32,
        timeout: u32,
        result: *mut usize,
    ) -> isize;
}

// ---------------------------------------------------------------- pure string half

/// Case-insensitive directory comparison that ignores trailing separators, so `C:\x` and
/// `C:\x\` are the same entry.
pub fn dir_eq(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        let t = s.trim();
        let t = t.trim_end_matches(['/', '\\']);
        // A bare drive root ("C:") and "C:\" are the same directory.
        if t.chars().count() == 2 && t.ends_with(':') {
            return t.to_ascii_lowercase();
        }
        t.to_ascii_lowercase()
    }
    norm(a) == norm(b) && !norm(a).is_empty()
}

/// Does `raw` (a raw PATH string) contain `dir`? Matches the literal entry and, so a
/// `%ProgramFiles%\winseek`-style entry is not duplicated, its expanded form.
pub fn path_contains(raw: &str, dir: &str) -> bool {
    raw.split(';')
        .any(|e| dir_eq(e, dir) || dir_eq(&expand_env(e), dir))
}

/// Add `dir` to (or remove it from) the PATH string `raw`. Returns `None` when nothing would
/// change, so callers skip pointless registry writes. Removal collapses the leftover
/// separators (`a;;b` would re-introduce an empty entry).
pub fn path_with(raw: &str, dir: &str, add: bool) -> Option<String> {
    if add {
        if path_contains(raw, dir) {
            return None;
        }
        let mut out = raw.trim_end().to_string();
        if !out.is_empty() && !out.ends_with(';') {
            out.push(';');
        }
        out.push_str(dir);
        return Some(out);
    }
    if !path_contains(raw, dir) {
        return None;
    }
    let kept: Vec<&str> = raw
        .split(';')
        .filter(|e| !(dir_eq(e, dir) || dir_eq(&expand_env(e), dir)))
        .collect();
    Some(kept.join(";"))
}

/// `%VAR%` references expanded (kernel32); unknown references come back unchanged, which is
/// also the behaviour a shell sees when the variable is unset.
pub fn expand_env(s: &str) -> String {
    let src = wide(s);
    let need = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), std::ptr::null_mut(), 0) };
    if need == 0 {
        return s.to_string();
    }
    let mut dst = vec![0u16; need as usize];
    let n = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), dst.as_mut_ptr(), need) };
    if n == 0 {
        return s.to_string();
    }
    // n includes the terminating nul.
    String::from_utf16_lossy(&dst[..(n as usize).saturating_sub(1)])
}

// ---------------------------------------------------------------- registry half

fn subkey(machine: bool) -> &'static str {
    if machine {
        MACHINE_SUBKEY
    } else {
        USER_SUBKEY
    }
}

/// Open the Environment key read-only; `None` when it does not exist (reads never create).
fn open_read(machine: bool) -> io::Result<Option<RegKey>> {
    let hk = if machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    match RegKey::predef(hk).open_subkey(subkey(machine)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        r => r.map(Some),
    }
}

/// The raw (unexpanded) PATH string; `""` when the key or the Path value does not exist.
/// Opening is read-only, so a status read never creates anything.
fn read_path_raw(machine: bool) -> io::Result<String> {
    let Some(key) = open_read(machine)? else {
        return Ok(String::new());
    };
    let v = match key.get_raw_value(VALUE_NAME) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        r => r?,
    };
    if v.vtype != REG_SZ && v.vtype != REG_EXPAND_SZ {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("PATH value is {:?}, not a string", v.vtype),
        ));
    }
    String::from_reg_value(&v)
}

/// Write the whole PATH string, keeping the stored value type — rewriting a REG_SZ Path
/// as REG_EXPAND_SZ would turn every literal `%` into a variable reference. A missing
/// key or value is created REG_EXPAND_SZ, which is what Windows itself stores.
fn write_path_raw(machine: bool, raw: &str) -> io::Result<()> {
    let hk = if machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let key = match RegKey::predef(hk).open_subkey_with_flags(subkey(machine), KEY_SET_VALUE) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            RegKey::predef(hk).create_subkey(subkey(machine))?.0
        }
        r => r?,
    };
    let expand = match key.get_raw_value(VALUE_NAME) {
        Ok(v) => v.vtype != REG_SZ,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true, // fresh value
        Err(e) => return Err(e),
    };
    let value = if expand {
        expand_sz(raw)
    } else {
        raw.to_reg_value()
    };
    key.set_raw_value(VALUE_NAME, &value)
}

/// A REG_EXPAND_SZ [`RegValue`] holding `s` (NUL-terminated UTF-16, the registry convention).
fn expand_sz(s: &str) -> RegValue<'static> {
    let mut bytes = Vec::with_capacity((s.len() + 1) * 2);
    for u in s.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    RegValue {
        bytes: bytes.into(),
        vtype: REG_EXPAND_SZ,
    }
}

/// One read-modify-write attempt; `Ok(false)` means nothing would change.
fn try_update_path(dir: &str, machine: bool, add: bool) -> io::Result<bool> {
    let raw = read_path_raw(machine)?;
    let Some(next) = path_with(&raw, dir, add) else {
        return Ok(false);
    };
    write_path_raw(machine, &next)?;
    announce();
    Ok(true)
}

fn update_path(dir: &str, machine: bool, add: bool) -> io::Result<bool> {
    let mut last: Option<io::Error> = None;
    for attempt in 1..=WRITE_ATTEMPTS {
        match try_update_path(dir, machine, add) {
            Ok(changed) => return Ok(changed),
            Err(e) => {
                eprintln!("winseek: PATH update attempt {attempt}/{WRITE_ATTEMPTS} failed ({e})");
                last = Some(e);
                std::thread::sleep(RETRY_BACKOFF * attempt);
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("PATH update failed")))
}

/// Tell running shells (Explorer and friends) that the environment changed, so *new*
/// terminals pick the PATH up without a log-off. Best effort: no one is around to see an
/// error here.
fn announce() {
    let name = wide("Environment");
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            name.as_ptr(),
            SMTO_ABORTIFHUNG,
            2000,
            std::ptr::null_mut(),
        );
    }
}

/// Add `dir` to the PATH (user PATH unless `machine`). Returns whether it changed, so the
/// caller can report "already there" instead of a fake success. Idempotent by design.
pub fn add_to_path(dir: &str, machine: bool) -> io::Result<bool> {
    update_path(dir, machine, true)
}

/// Remove `dir` from the PATH. Returns whether it changed.
pub fn remove_from_path(dir: &str, machine: bool) -> io::Result<bool> {
    update_path(dir, machine, false)
}

/// The raw PATH string as stored (unexpanded), for status displays.
pub fn path_raw(machine: bool) -> io::Result<String> {
    read_path_raw(machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_eq_ignores_case_and_trailing_separators() {
        assert!(dir_eq(
            r"C:\Program Files\winseek",
            r"c:\program files\winseek"
        ));
        assert!(dir_eq(r"C:\x", r"C:\x\"));
        assert!(dir_eq(r"C:\", "C:"));
        assert!(!dir_eq(r"C:\x", r"C:\xy"));
        assert!(!dir_eq("", r"C:\x"));
    }

    #[test]
    fn path_contains_matches_entries() {
        let raw = r"C:\Windows;C:\Program Files\winseek;%ProgramFiles%\other";
        assert!(path_contains(raw, r"C:\PROGRAM FILES\WINSEEK"));
        assert!(path_contains(raw, r"C:\Program Files\winseek\"));
        assert!(!path_contains(raw, r"D:\elsewhere"));
        assert!(!path_contains("", r"C:\x"));
    }

    #[test]
    fn add_appends_once_and_is_idempotent() {
        assert_eq!(path_with("", r"C:\w", true).as_deref(), Some(r"C:\w"));
        assert_eq!(
            path_with(r"C:\Windows", r"C:\w", true).as_deref(),
            Some(r"C:\Windows;C:\w")
        );
        assert_eq!(
            path_with(r"C:\Windows;", r"C:\w", true).as_deref(),
            Some(r"C:\Windows;C:\w")
        );
        // already there: no change
        assert_eq!(path_with(r"C:\Windows;C:\w", r"C:\w", true), None);
        assert_eq!(path_with(r"C:\Windows;C:\w\", r"C:\w", true), None);
    }

    #[test]
    fn remove_drops_the_entry_without_doubles() {
        assert_eq!(
            path_with(r"a;C:\w;b", r"C:\w", false).as_deref(),
            Some("a;b")
        );
        assert_eq!(
            path_with(r"a;C:\w\;b", r"C:\w", false).as_deref(),
            Some("a;b")
        );
        assert_eq!(path_with(r"C:\w", r"C:\W", false).as_deref(), Some(""));
        // not there: no change
        assert_eq!(path_with("a;b", r"C:\w", false), None);
    }

    #[test]
    fn expand_leaves_unknown_references_alone() {
        assert_eq!(
            expand_env(r"%definitely_not_set_xyz%\x"),
            r"%definitely_not_set_xyz%\x"
        );
        assert_eq!(expand_env("plain"), "plain");
    }
}
