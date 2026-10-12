//! winseek-ntfs: raw `$MFT` scan engine (zero dependencies, raw Win32 FFI).
//!
//! Ported from the verified wiztree-rs scanner. Windows only. Scanning needs
//! an elevated process. See [`scan_volume`], [`list_ntfs_volumes`] and
//! [`open_volume_for_usn`].
//!
//! Handle flag rule: the scan handle's flags come from [`ScanOptions`]
//! (default: GENERIC_READ, no flags); the USN-monitor handle always has
//! `FILE_FLAG_NO_BUFFERING`.

pub mod ffi;
pub mod record;
mod scan;

pub use ffi::Handle;
pub use scan::{
    list_ntfs_volumes, open_volume_for_usn, scan_volume, scan_volume_with, MftEntry, ScanError,
    ScanOptions, ScanStats, VolumeInfo,
};
#[cfg(feature = "testutil")]
#[doc(hidden)]
pub use scan::bench_decode;
