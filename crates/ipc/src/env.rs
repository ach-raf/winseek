//! PATH-environment helpers shared by `winseek-cli path …` and the app's Settings toggle:
//! add / remove a directory from the user (HKCU) or machine (HKLM) PATH and announce the
//! change so running shells pick it up for new children. Registry access is hand-rolled FFI
//! like the rest of this crate (no `windows` crate, no new dependencies).
//!
//! The pure string half ([`path_contains`], [`path_with`]) is where the tests live; the
//! registry wrappers around it are thin.

use std::ffi::c_void;
use std::io;

type Handle = *mut c_void;

const HKEY_CURRENT_USER: Handle = handle(0x8000_0001u64);
const HKEY_LOCAL_MACHINE: Handle = handle(0x8000_0002u64);
const USER_SUBKEY: &str = "Environment";
const MACHINE_SUBKEY: &str = "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment";
const VALUE_NAME: &str = "Path";

const KEY_READ: u32 = 0x0002_0019;
const KEY_WRITE: u32 = 0x0002_0006;
const REG_EXPAND_SZ: u32 = 2;
const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;

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

#[link(name = "advapi32")]
extern "system" {
    fn RegCreateKeyExW(
        key: Handle,
        subkey: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        access: u32,
        security: *mut c_void,
        out: *mut Handle,
        disposition: *mut u32,
    ) -> i32;
    fn RegQueryValueExW(
        key: Handle,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        len: *mut u32,
    ) -> i32;
    fn RegSetValueExW(
        key: Handle,
        name: *const u16,
        reserved: u32,
        kind: u32,
        data: *const u8,
        len: u32,
    ) -> i32;
    fn RegCloseKey(key: Handle) -> i32;
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

fn err(op: &str, code: i32) -> io::Error {
    io::Error::other(format!("{op} failed (winerror {code})"))
}

fn read_path_raw(machine: bool) -> io::Result<String> {
    let (root, subkey) = root_and_key(machine);
    let subkey = wide(subkey);
    let name = wide(VALUE_NAME);
    let mut key: Handle = std::ptr::null_mut();
    let r = unsafe {
        RegCreateKeyExW(
            root,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_READ,
            std::ptr::null_mut(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if r != ERROR_SUCCESS {
        return Err(err("open the PATH key", r));
    }
    let out = (|| {
        let mut kind = 0u32;
        let mut len = 0u32;
        let r = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                std::ptr::null_mut(),
                &mut len,
            )
        };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(String::new()); // no Path value yet: an empty PATH
        }
        if r != ERROR_SUCCESS {
            return Err(err("size the Path value", r));
        }
        let mut buf = vec![0u8; len as usize];
        let r = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        if r != ERROR_SUCCESS {
            return Err(err("read the Path value", r));
        }
        let bytes = &buf[..len as usize];
        let units: Vec<u16> = (0..bytes.len() / 2)
            .map(|i| u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]))
            .collect();
        let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
        Ok(String::from_utf16_lossy(&units[..end]))
    })();
    unsafe { RegCloseKey(key) };
    out
}

fn write_path_raw(machine: bool, raw: &str) -> io::Result<()> {
    let (root, subkey) = root_and_key(machine);
    let subkey = wide(subkey);
    let name = wide(VALUE_NAME);
    let mut key: Handle = std::ptr::null_mut();
    let r = unsafe {
        RegCreateKeyExW(
            root,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_WRITE,
            std::ptr::null_mut(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if r != ERROR_SUCCESS {
        return Err(err("open the PATH key for write", r));
    }
    // REG_EXPAND_SZ keeps %VAR% entries working; for a plain PATH it behaves like REG_SZ.
    let mut data = wide(raw);
    data.pop(); // RegSetValueExW takes the byte count without the terminating nul.
    let r = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_EXPAND_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    unsafe { RegCloseKey(key) };
    if r != ERROR_SUCCESS {
        return Err(err("write the Path value", r));
    }
    Ok(())
}

fn root_and_key(machine: bool) -> (Handle, &'static str) {
    if machine {
        (HKEY_LOCAL_MACHINE, MACHINE_SUBKEY)
    } else {
        (HKEY_CURRENT_USER, USER_SUBKEY)
    }
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
    let raw = read_path_raw(machine)?;
    let Some(next) = path_with(&raw, dir, true) else {
        return Ok(false);
    };
    write_path_raw(machine, &next)?;
    announce();
    Ok(true)
}

/// Remove `dir` from the PATH. Returns whether it changed.
pub fn remove_from_path(dir: &str, machine: bool) -> io::Result<bool> {
    let raw = read_path_raw(machine)?;
    let Some(next) = path_with(&raw, dir, false) else {
        return Ok(false);
    };
    write_path_raw(machine, &next)?;
    announce();
    Ok(true)
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
