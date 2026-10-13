//! File identity: `(volume GUID, FRN)` from an open handle, plus size / mtime / attributes.
//!
//! Opened with `FILE_READ_ATTRIBUTES` only (no data access, so cloud placeholders are not
//! recalled) and `FILE_FLAG_BACKUP_SEMANTICS` (works for directories too).

use std::collections::HashMap;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use winseek_store::FileKey;

pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;
pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_REPARSE_POINT: u32 = 0x400;

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub key: FileKey,
    pub size: u64,
    /// Last write time, Unix milliseconds.
    pub mtime_ms: i64,
    pub attrs: u32,
}

/// 100 ns ticks between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

fn filetime_to_unix_ms(low: u32, high: u32) -> i64 {
    let t = (u64::from(high) << 32) | u64::from(low);
    (t.saturating_sub(FILETIME_UNIX_EPOCH) / 10_000) as i64
}

/// NUL-terminated UTF-16; absolute drive paths of ~MAX_PATH or longer get the `\\?\` prefix.
fn wide_path(p: &Path) -> Vec<u16> {
    let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
    let is_drive_abs = v.len() >= 3 && v[1] == u16::from(b':') && (v[2] == u16::from(b'\\') || v[2] == u16::from(b'/'));
    if v.len() >= 240 && is_drive_abs {
        let mut pre: Vec<u16> = r"\\?\".encode_utf16().collect();
        pre.extend(v.iter().map(|&c| if c == u16::from(b'/') { u16::from(b'\\') } else { c }));
        v = pre;
    }
    v.push(0);
    v
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: handle was returned by CreateFileW and is owned by us.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn volume_cache() -> &'static Mutex<HashMap<String, String>> {
    static C: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// `\\?\Volume{guid}\` of the volume holding `wide` (a NUL-terminated path), cached per volume root.
fn volume_guid(wide: &[u16]) -> io::Result<String> {
    let mut root = [0u16; 1024];
    // SAFETY: `wide` is NUL-terminated; `root` is a valid output buffer.
    unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut root) }.map_err(io::Error::from)?;
    let root_s = from_wide(&root);
    let cache_key = root_s.to_lowercase();
    if let Some(g) = volume_cache().lock().unwrap_or_else(|e| e.into_inner()).get(&cache_key) {
        return Ok(g.clone());
    }
    let mut name = [0u16; 64];
    // SAFETY: `root` is NUL-terminated (zero-initialised buffer).
    unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(root.as_ptr()), &mut name) }.map_err(io::Error::from)?;
    let guid = from_wide(&name);
    volume_cache().lock().unwrap_or_else(|e| e.into_inner()).insert(cache_key, guid.clone());
    Ok(guid)
}

/// Identity + basic attributes of `path` (file or directory).
pub fn file_info(path: &Path) -> io::Result<FileInfo> {
    let w = wide_path(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let h = unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(io::Error::from)?;
    let h = Handle(h);
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: valid handle and output struct.
    unsafe { GetFileInformationByHandle(h.0, &mut info) }.map_err(io::Error::from)?;
    let vol_guid = volume_guid(&w)?;
    Ok(FileInfo {
        key: FileKey { vol_guid, frn: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow) },
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        mtime_ms: filetime_to_unix_ms(info.ftLastWriteTime.dwLowDateTime, info.ftLastWriteTime.dwHighDateTime),
        attrs: info.dwFileAttributes,
    })
}

/// `(volume GUID, FRN)` of `path`.
#[allow(dead_code)] // the identity-only API; the crawler and feed need size/mtime too
pub fn file_key(path: &Path) -> io::Result<FileKey> {
    file_info(path).map(|i| i.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frn_stable_across_rename_and_distinct_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        std::fs::write(&a, b"one").unwrap();
        std::fs::write(&b, b"two").unwrap();
        let ka = file_key(&a).unwrap();
        let kb = file_key(&b).unwrap();
        assert_ne!(ka, kb);
        assert!(ka.vol_guid.starts_with(r"\\?\Volume{"), "{}", ka.vol_guid);
        assert_eq!(ka.vol_guid, kb.vol_guid);

        let a2 = dir.path().join("renamed.jpg");
        std::fs::rename(&a, &a2).unwrap();
        assert_eq!(file_key(&a2).unwrap(), ka);
        assert!(file_key(&a).is_err());
    }

    #[test]
    fn info_size_mtime_and_dir_attr() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.png");
        std::fs::write(&f, b"12345").unwrap();
        let i = file_info(&f).unwrap();
        assert_eq!(i.size, 5);
        assert_eq!(i.attrs & ATTR_DIRECTORY, 0);
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        assert!((now_ms - i.mtime_ms).abs() < 120_000, "mtime {} vs now {now_ms}", i.mtime_ms);
        assert_ne!(file_info(dir.path()).unwrap().attrs & ATTR_DIRECTORY, 0);
    }

    #[test]
    fn long_paths_get_extended_prefix() {
        let long = format!(r"C:\{}\f.jpg", "d".repeat(250));
        let w = wide_path(Path::new(&long));
        assert!(String::from_utf16_lossy(&w).starts_with(r"\\?\C:\"));
        let short = wide_path(Path::new(r"C:\a\f.jpg"));
        assert!(String::from_utf16_lossy(&short).starts_with(r"C:\a"));
    }

    #[test]
    fn filetime_conversion() {
        // 1970-01-01T00:00:01Z
        let t = FILETIME_UNIX_EPOCH + 10_000_000;
        assert_eq!(filetime_to_unix_ms(t as u32, (t >> 32) as u32), 1000);
    }
}
