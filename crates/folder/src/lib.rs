//! Non-NTFS (and ReFS) support primitives for winseek, zero dependencies, raw Win32 FFI.
//!
//! * [`volumes`]  - enumerate fixed/removable drive letters with their filesystem name.
//! * [`stat`]     - `GetFileAttributesExW` wrapper.
//! * [`walk`]     - `FindFirstFileExW` worklist walker with synthetic u64 folder ids (FAT/exFAT/any).
//! * [`walk_ids`] - handle based walker yielding 128-bit file ids (ReFS; also works on NTFS).
//! * [`notify`]   - pure `FILE_NOTIFY_INFORMATION` parser + rename pairer (no Win32).
//! * [`watch`]    - overlapped `ReadDirectoryChangesW` on one directory tree.
//! * [`device`]   - `CM_Register_Notification` volume arrival/removal and per-handle events,
//!   usable from a service or console process without a window.
//!
//! Nothing here needs elevation.

#![cfg(windows)]
// Keep `% n != 0` / `chunks_exact(2)` so the crate also builds on toolchains older than
// the `is_multiple_of` / `as_chunks` stabilisations.
#![allow(clippy::manual_is_multiple_of, clippy::chunks_exact_to_as_chunks)]

mod sys;

pub mod device;
pub mod notify;
pub mod stat;
pub mod volumes;
pub mod walk;
pub mod walk_ids;
pub mod watch;

pub use device::{watch_handle, watch_volume_interfaces, HandleEvent, Registration};
pub use notify::{
    parse_notify, NotifyError, NotifyPairer, PathEvent, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED,
    FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
};
pub use stat::{stat, Stat};
pub use volumes::{list_volumes, logical_drive_mask, VolumeDesc};
pub use walk::{walk_names, WalkEntry, WalkStats};
pub use walk_ids::{file_id_128, walk_file_ids, IdEntry};
pub use watch::{DirWatch, Poll, WatchCanceller, NOTIFY_FILTER};

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};

    /// Fresh temp directory removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(name: &str) -> TempDir {
            let p = std::env::temp_dir().join(format!("winseek-folder-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
        pub fn s(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    /// Tree used by the walker tests: 3 folders (a, a\b, unicode), 3 files.
    pub fn make_tree(root: &Path) {
        let b = root.join("a").join("b");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir_all(root.join("\u{fc}n\u{ef}-\u{2603}")).unwrap();
        std::fs::write(b.join("file.bin"), vec![7u8; 12345]).unwrap();
        std::fs::write(root.join("top.txt"), b"hello").unwrap();
        std::fs::write(root.join("a").join("\u{65e5}\u{672c}.txt"), b"x").unwrap();
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
