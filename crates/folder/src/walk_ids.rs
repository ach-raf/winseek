//! Handle based walker yielding 128-bit file ids (ReFS; also works on NTFS).

use crate::sys::*;
use crate::walk::WalkStats;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct IdEntry<'a> {
    pub id: u128,
    pub parent: u128,
    pub name: &'a str,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

const FILE_ID_INFO: i32 = 18;
const FILE_ID_EXTD_DIRECTORY_INFO: i32 = 19;
const FILE_ID_EXTD_DIRECTORY_RESTART_INFO: i32 = 20;
const BUF_BYTES: usize = 64 * 1024;
/// Offset of `FileName` in `FILE_ID_EXTD_DIR_INFO`.
const NAME_OFF: usize = 88;

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn rd_i64(b: &[u8], off: usize) -> Option<i64> {
    Some(i64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

/// `FILE_ID_128` of a file or directory (`FileIdInfo`), little-endian interpreted.
pub fn file_id_128(path: &str) -> io::Result<u128> {
    let h = open_handle(&long_path(path), FILE_READ_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS)?;
    id_of_handle(&h)
}

fn id_of_handle(h: &OwnedHandle) -> io::Result<u128> {
    // FILE_ID_INFO { u64 VolumeSerialNumber; u8 FileId[16] }
    let mut buf = [0u8; 24];
    // SAFETY: `buf` is 24 bytes, the size of FILE_ID_INFO, and `h` is a live handle.
    let ok = unsafe { GetFileInformationByHandleEx(h.0, FILE_ID_INFO, buf.as_mut_ptr() as *mut _, 24) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(&buf[8..24]);
    Ok(u128::from_le_bytes(id))
}

/// Worklist walk like [`crate::walk_names`] but yielding 128-bit ids. The root is not emitted;
/// its children have `parent == file_id_128(root)`. Reparse directories are emitted but not
/// descended. `stop` set -> `Err(Interrupted)`.
pub fn walk_file_ids(root: &str, stop: &AtomicBool, sink: &mut dyn FnMut(&IdEntry)) -> io::Result<WalkStats> {
    let mut stats = WalkStats::default();
    let root_long = long_path(root);
    let root_id = file_id_128(&root_long)?;
    let mut work: Vec<(String, u128)> = vec![(root_long, root_id)];
    // u64 backing store guarantees the 8-byte alignment the API expects.
    let mut store = vec![0u64; BUF_BYTES / 8];
    while let Some((dir, dir_id)) = work.pop() {
        if stop.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let h = match open_handle(&dir, FILE_LIST_DIRECTORY | SYNCHRONIZE, FILE_FLAG_BACKUP_SEMANTICS) {
            Ok(h) => h,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        let mut class = FILE_ID_EXTD_DIRECTORY_RESTART_INFO;
        loop {
            // SAFETY: `store` is BUF_BYTES long and 8-aligned; `h` is a live directory handle.
            let ok = unsafe { GetFileInformationByHandleEx(h.0, class, store.as_mut_ptr() as *mut _, BUF_BYTES as u32) };
            class = FILE_ID_EXTD_DIRECTORY_INFO;
            if ok == 0 {
                if last_code() != ERROR_NO_MORE_FILES {
                    stats.errors += 1;
                }
                break;
            }
            // SAFETY: reinterpreting the initialised u64 buffer as bytes (same length).
            let bytes: &[u8] = unsafe { std::slice::from_raw_parts(store.as_ptr() as *const u8, BUF_BYTES) };
            if !parse_dir_buffer(bytes, &dir, dir_id, &mut work, &mut stats, sink) {
                stats.errors += 1;
            }
        }
    }
    Ok(stats)
}

/// Walk one `FILE_ID_EXTD_DIR_INFO` chain. Returns false if the chain was malformed (parsing of
/// this buffer stops there).
fn parse_dir_buffer(
    b: &[u8],
    dir: &str,
    dir_id: u128,
    work: &mut Vec<(String, u128)>,
    stats: &mut WalkStats,
    sink: &mut dyn FnMut(&IdEntry),
) -> bool {
    let mut off = 0usize;
    loop {
        if off + NAME_OFF > b.len() {
            return false;
        }
        let (Some(next), Some(attrs), Some(name_len), Some(mtime), Some(eof)) = (
            rd_u32(b, off),
            rd_u32(b, off + 56),
            rd_u32(b, off + 60),
            rd_i64(b, off + 24),
            rd_i64(b, off + 40),
        ) else {
            return false;
        };
        let name_len = name_len as usize;
        if name_len % 2 != 0 || off + NAME_OFF + name_len > b.len() {
            return false;
        }
        let Some(id_bytes) = b.get(off + 72..off + 88) else { return false };
        let mut idb = [0u8; 16];
        idb.copy_from_slice(id_bytes);
        let id = u128::from_le_bytes(idb);
        let units: Vec<u16> = b[off + NAME_OFF..off + NAME_OFF + name_len]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let name = String::from_utf16_lossy(&units);
        if name != "." && name != ".." && !name.is_empty() {
            let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
            if is_dir {
                stats.folders += 1;
                if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                    work.push((join(dir, &name), id));
                }
            } else {
                stats.files += 1;
            }
            let size = if is_dir { 0 } else { eof.max(0) as u64 };
            sink(&IdEntry { id, parent: dir_id, name: &name, is_dir, size, mtime });
        }
        if next == 0 {
            return true;
        }
        let next = next as usize;
        if next < NAME_OFF || next % 8 != 0 || off + next >= b.len() {
            return false;
        }
        off += next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{make_tree, TempDir};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn walks_tree_with_file_ids() {
        let t = TempDir::new("walkids");
        make_tree(t.path());
        let stop = AtomicBool::new(false);
        let mut got: Vec<(u128, u128, String, bool, u64)> = Vec::new();
        let st = walk_file_ids(t.s(), &stop, &mut |e| {
            got.push((e.id, e.parent, e.name.to_string(), e.is_dir, e.size));
        })
        .unwrap();
        assert_eq!(st, WalkStats { folders: 3, files: 3, errors: 0 });
        let root_id = file_id_128(t.s()).unwrap();
        let by_name: HashMap<&str, _> = got.iter().map(|g| (g.2.as_str(), g)).collect();
        let a = by_name["a"];
        let b = by_name["b"];
        let uni = by_name["\u{fc}n\u{ef}-\u{2603}"];
        assert_eq!(a.1, root_id);
        assert_eq!(uni.1, root_id);
        assert_eq!(by_name["top.txt"].1, root_id);
        assert_eq!(b.1, a.0);
        assert_eq!(by_name["file.bin"].1, b.0);
        assert_eq!(by_name["file.bin"].4, 12345);
        assert_eq!(by_name["\u{65e5}\u{672c}.txt"].1, a.0);
        assert_eq!(a.0, file_id_128(t.path().join("a").to_str().unwrap()).unwrap());
        assert_eq!(b.0, file_id_128(t.path().join("a").join("b").to_str().unwrap()).unwrap());
        assert_eq!(uni.0, file_id_128(t.path().join("\u{fc}n\u{ef}-\u{2603}").to_str().unwrap()).unwrap());
        let uniq: HashSet<u128> = got.iter().map(|g| g.0).collect();
        assert_eq!(uniq.len(), got.len());
    }

    #[test]
    fn stop_flag_interrupts() {
        let t = TempDir::new("walkidsstop");
        let stop = AtomicBool::new(true);
        let e = walk_file_ids(t.s(), &stop, &mut |_| {}).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn malformed_buffers_do_not_panic() {
        let mut stats = WalkStats::default();
        let mut work = Vec::new();
        let mut n = 0;
        for len in [0usize, 10, 87, 88, 100] {
            let mut b = vec![0xffu8; len];
            if len >= 64 {
                b[60..64].copy_from_slice(&1000u32.to_le_bytes());
            }
            parse_dir_buffer(&b, "x", 1, &mut work, &mut stats, &mut |_| n += 1);
        }
        assert_eq!(n, 0);
    }
}
