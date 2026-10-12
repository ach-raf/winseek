//! winseek-ntfs: raw `$MFT` scan engine (zero dependencies, raw Win32 FFI).
//!
//! Ported from the verified wiztree-rs scanner. Windows only. Scanning needs
//! an elevated process. See [`scan_volume`], [`list_ntfs_volumes`] and
//! [`open_volume_for_usn`].
//!
//! Handle flag rule: the scan handle is GENERIC_READ without
//! `FILE_FLAG_NO_BUFFERING`; the USN-monitor handle has it.

pub mod ffi;
pub mod record;
mod scan;

pub use ffi::Handle;
pub use scan::{
    list_ntfs_volumes, open_volume_for_usn, scan_volume, MftEntry, ScanError, ScanStats,
    VolumeInfo,
};
#[cfg(feature = "testutil")]
#[doc(hidden)]
pub use scan::bench_decode;
