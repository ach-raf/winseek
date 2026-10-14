//! `FindFirstFileExW` worklist walker (no recursion) with synthetic folder ids.

use crate::sys::*;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct WalkEntry<'a> {
    pub id: u64,
    pub parent_id: u64,
    pub name: &'a str,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalkStats {
    pub folders: u64,
    pub files: u64,
    pub errors: u64,
}

/// Walk everything under `root` (e.g. `E:\` or any directory; trailing backslash optional).
///
/// The root itself is NOT emitted; its id is `root_id`. Every folder found gets
/// `id = *next_id; *next_id += 1` when its entry is emitted (discovery order). Files get id 0.
/// Reparse-point directories are emitted as folders but not descended. A directory that cannot
/// be opened increments `errors` and is skipped (only the root failing is returned as `Err`).
/// `stop` is checked between directories; if set, returns `Err(ErrorKind::Interrupted)`.
pub fn walk_names(
    root: &str,
    root_id: u64,
    next_id: &mut u64,
    stop: &AtomicBool,
    sink: &mut dyn FnMut(&WalkEntry),
) -> io::Result<WalkStats> {
    let mut stats = WalkStats::default();
    let mut work: Vec<(String, u64)> = vec![(long_path(root), root_id)];
    let mut is_root = true;
    while let Some((dir, dir_id)) = work.pop() {
        if stop.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let was_root = std::mem::replace(&mut is_root, false);
        let pat = wide(&join(&dir, "*"));
        // SAFETY: plain-old-data struct, all-zero is valid.
        let mut fd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        // SAFETY: `pat` is NUL-terminated and `fd` is a WIN32_FIND_DATAW as required by
        // FindExInfoBasic; no search filter.
        let h = unsafe {
            FindFirstFileExW(
                pat.as_ptr(),
                FIND_EX_INFO_BASIC,
                &mut fd as *mut _ as *mut _,
                FIND_EX_SEARCH_NAME_MATCH,
                std::ptr::null_mut(),
                FIND_FIRST_EX_LARGE_FETCH,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) {
                continue; // empty directory (e.g. a bare FAT root)
            }
            if was_root {
                return Err(e);
            }
            stats.errors += 1;
            continue;
        }
        let _guard = FindHandle(h);
        loop {
            let name = from_wide_nul(&fd.cFileName);
            if name != "." && name != ".." && !name.is_empty() {
                let attrs = fd.dwFileAttributes;
                let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                let size = if is_dir { 0 } else { ((fd.nFileSizeHigh as u64) << 32) | fd.nFileSizeLow as u64 };
                let mtime = fd.ftLastWriteTime.ticks();
                let id = if is_dir {
                    let id = *next_id;
                    *next_id += 1;
                    stats.folders += 1;
                    if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                        work.push((join(&dir, &name), id));
                    }
                    id
                } else {
                    stats.files += 1;
                    0
                };
                sink(&WalkEntry { id, parent_id: dir_id, name: &name, is_dir, size, mtime });
            }
            // SAFETY: `h` is a live find handle and `fd` the matching buffer.
            let ok = unsafe { FindNextFileW(h, &mut fd as *mut _ as *mut _) };
            if ok == 0 {
                if last_code() != ERROR_NO_MORE_FILES {
                    stats.errors += 1;
                }
                break;
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{make_tree, TempDir};
    use std::collections::HashMap;

    #[test]
    fn walks_tree_with_ids() {
        let t = TempDir::new("walknames");
        make_tree(t.path());
        let mut next = 16u64;
        let mut got: Vec<(u64, u64, String, bool, u64, i64)> = Vec::new();
        let stop = AtomicBool::new(false);
        let st = walk_names(t.s(), 5, &mut next, &stop, &mut |e| {
            got.push((e.id, e.parent_id, e.name.to_string(), e.is_dir, e.size, e.mtime));
        })
        .unwrap();
        assert_eq!(st, WalkStats { folders: 3, files: 3, errors: 0 });
        assert_eq!(next, 19);
        let by_name: HashMap<&str, _> = got.iter().map(|g| (g.2.as_str(), g)).collect();
        let a = by_name["a"];
        let b = by_name["b"];
        let uni = by_name["\u{fc}n\u{ef}-\u{2603}"];
        assert!(a.3 && b.3 && uni.3);
        assert_eq!(a.1, 5);
        assert_eq!(uni.1, 5);
        assert_eq!(b.1, a.0);
        assert_eq!(by_name["top.txt"].1, 5);
        assert_eq!(by_name["top.txt"].4, 5);
        assert_eq!(by_name["top.txt"].0, 0);
        assert_eq!(by_name["file.bin"].1, b.0);
        assert_eq!(by_name["file.bin"].4, 12345);
        assert!(by_name["file.bin"].5 > 0);
        assert_eq!(by_name["\u{65e5}\u{672c}.txt"].1, a.0);
        let mut ids: Vec<u64> = got.iter().filter(|g| g.3).map(|g| g.0).collect();
        ids.sort();
        assert_eq!(ids, vec![16, 17, 18]);
    }

    #[test]
    fn trailing_backslash_optional_and_missing_root() {
        let t = TempDir::new("walkroot");
        make_tree(t.path());
        let stop = AtomicBool::new(false);
        let mut n = 0;
        let with = format!("{}\\", t.s());
        let st = walk_names(&with, 5, &mut n, &stop, &mut |_| {}).unwrap();
        assert_eq!(st.folders, 3);
        let missing = format!("{}\\nope", t.s());
        assert!(walk_names(&missing, 5, &mut n, &stop, &mut |_| {}).is_err());
    }

    #[test]
    fn stop_flag_interrupts() {
        let t = TempDir::new("walkstop");
        make_tree(t.path());
        let stop = AtomicBool::new(true);
        let mut n = 16;
        let e = walk_names(t.s(), 5, &mut n, &stop, &mut |_| {}).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted);
    }
}
