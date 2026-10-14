//! `GetFileAttributesExW` wrapper.

use crate::sys::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub is_dir: bool,
    pub is_reparse: bool,
    pub size: u64,
    /// `ftLastWriteTime` as FILETIME ticks.
    pub mtime: i64,
}

/// Attributes of `path`; `None` if the call fails (missing, access denied, ...).
pub fn stat(path: &str) -> Option<Stat> {
    let w = wide(&long_path(path));
    // SAFETY: all-integer POD struct, zero is a valid bit pattern.
    let mut d: WIN32_FILE_ATTRIBUTE_DATA = unsafe { std::mem::zeroed() };
    // SAFETY: `w` NUL-terminated; level 0 = GetFileExInfoStandard fills WIN32_FILE_ATTRIBUTE_DATA.
    let ok = unsafe { GetFileAttributesExW(w.as_ptr(), 0, &mut d as *mut _ as *mut _) };
    if ok == 0 {
        return None;
    }
    let is_dir = d.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    Some(Stat {
        is_dir,
        is_reparse: d.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        size: if is_dir { 0 } else { ((d.nFileSizeHigh as u64) << 32) | d.nFileSizeLow as u64 },
        mtime: d.ftLastWriteTime.ticks(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn stat_file_dir_missing() {
        let t = TempDir::new("stat");
        let f = t.path().join("f.bin");
        std::fs::write(&f, vec![1u8; 777]).unwrap();
        let s = stat(f.to_str().unwrap()).unwrap();
        assert!(!s.is_dir && !s.is_reparse);
        assert_eq!(s.size, 777);
        assert!(s.mtime > 0);
        let d = stat(t.s()).unwrap();
        assert!(d.is_dir);
        assert_eq!(d.size, 0);
        assert!(stat(t.path().join("nope").to_str().unwrap()).is_none());
    }
}
