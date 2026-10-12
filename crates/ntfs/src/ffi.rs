//! Minimal raw Win32 FFI (kernel32 only, no `windows` crate) plus the RAII
//! [`Handle`] and a page-aligned buffer.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

pub(crate) type HANDLE = isize;
pub(crate) type DWORD = u32;
pub(crate) type BOOL = i32;

pub(crate) const INVALID_HANDLE_VALUE: HANDLE = -1;
pub(crate) const GENERIC_READ: DWORD = 0x8000_0000;
pub(crate) const FILE_SHARE_READ: DWORD = 0x1;
pub(crate) const FILE_SHARE_WRITE: DWORD = 0x2;
pub(crate) const OPEN_EXISTING: DWORD = 3;
pub(crate) const FILE_FLAG_NO_BUFFERING: DWORD = 0x2000_0000;
pub(crate) const FILE_BEGIN: DWORD = 0;
pub(crate) const DRIVE_FIXED: DWORD = 3;
pub(crate) const DRIVE_REMOVABLE: DWORD = 2;

pub const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x90064;

extern "system" {
    fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: *mut core::ffi::c_void,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    fn CloseHandle(hObject: HANDLE) -> BOOL;
    fn ReadFile(
        hFile: HANDLE,
        lpBuffer: *mut core::ffi::c_void,
        nNumberOfBytesToRead: DWORD,
        lpNumberOfBytesRead: *mut DWORD,
        lpOverlapped: *mut core::ffi::c_void,
    ) -> BOOL;
    fn SetFilePointerEx(
        hFile: HANDLE,
        liDistanceToMove: i64,
        lpNewFilePointer: *mut i64,
        dwMoveMethod: DWORD,
    ) -> BOOL;
    fn DeviceIoControl(
        hDevice: HANDLE,
        dwIoControlCode: DWORD,
        lpInBuffer: *mut core::ffi::c_void,
        nInBufferSize: DWORD,
        lpOutBuffer: *mut core::ffi::c_void,
        nOutBufferSize: DWORD,
        lpBytesReturned: *mut DWORD,
        lpOverlapped: *mut core::ffi::c_void,
    ) -> BOOL;
    fn GetLogicalDrives() -> DWORD;
    fn GetDriveTypeW(lpRootPathName: *const u16) -> DWORD;
    fn GetVolumeInformationW(
        lpRootPathName: *const u16,
        lpVolumeNameBuffer: *mut u16,
        nVolumeNameSize: DWORD,
        lpVolumeSerialNumber: *mut DWORD,
        lpMaximumComponentLength: *mut DWORD,
        lpFileSystemFlags: *mut DWORD,
        lpFileSystemNameBuffer: *mut u16,
        nFileSystemNameSize: DWORD,
    ) -> BOOL;
    fn GetDiskFreeSpaceW(
        lpRootPathName: *const u16,
        lpSectorsPerCluster: *mut DWORD,
        lpBytesPerSector: *mut DWORD,
        lpNumberOfFreeClusters: *mut DWORD,
        lpTotalNumberOfClusters: *mut DWORD,
    ) -> BOOL;
}

/// NUL-terminated UTF-16 for Win32 calls.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Owned Win32 HANDLE, closed on drop.
#[derive(Debug)]
pub struct Handle(HANDLE);

// A kernel HANDLE may be used from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    /// Open `\\.\X:` with the given access and flags.
    pub(crate) fn open_volume(letter: char, access: DWORD, flags: DWORD) -> io::Result<Handle> {
        let path = wide(&format!("\\\\.\\{}:", letter.to_ascii_uppercase()));
        // SAFETY: path is NUL-terminated; no security attributes / template.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                flags,
                0,
            )
        };
        if h == INVALID_HANDLE_VALUE || h == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Handle(h))
        }
    }

    /// Raw HANDLE value (still owned by `self`).
    pub fn raw(&self) -> isize {
        self.0
    }

    /// Synchronous `DeviceIoControl`; returns bytes written to `output`.
    /// On failure the error carries the Win32 code in `raw_os_error()`.
    pub fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> io::Result<usize> {
        let mut returned: DWORD = 0;
        // SAFETY: in/out slices are valid for their stated lengths; the
        // kernel does not write through the input pointer.
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                code,
                if input.is_empty() { std::ptr::null_mut() } else { input.as_ptr() as *mut _ },
                input.len() as DWORD,
                if output.is_empty() { std::ptr::null_mut() } else { output.as_mut_ptr().cast() },
                output.len() as DWORD,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(returned as usize)
        }
    }

    /// Seek to an absolute offset and read once into `buf`.
    pub(crate) fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        // SAFETY: valid handle; null out-pointer for the new position is allowed.
        if unsafe { SetFilePointerEx(self.0, offset as i64, std::ptr::null_mut(), FILE_BEGIN) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut done: DWORD = 0;
        // SAFETY: buf is writable for buf.len() (<= u32::MAX by callers).
        let ok = unsafe {
            ReadFile(self.0, buf.as_mut_ptr().cast(), buf.len() as DWORD, &mut done, std::ptr::null_mut())
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if done == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "ReadFile returned 0 bytes"));
        }
        Ok(done as usize)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.0 != 0 && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: owned, valid handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Page-aligned zeroed heap buffer (volume reads need sector alignment).
pub(crate) struct AlignedBuf {
    ptr: *mut u8,
    layout: Layout,
}

// Plain bytes owned by exactly one place; moves between threads by value.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub(crate) fn new(size: usize) -> AlignedBuf {
        let layout = Layout::from_size_align(size.max(1), 4096).expect("AlignedBuf layout");
        // SAFETY: non-zero size, valid alignment.
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "AlignedBuf::new({size}) allocation failed");
        AlignedBuf { ptr, layout }
    }
    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, uniquely borrowed.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: allocated with this layout.
        unsafe { dealloc(self.ptr, self.layout) }
    }
}

/// Letters of every fixed or removable logical drive.
pub(crate) fn logical_drives() -> Vec<char> {
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        // SAFETY: NUL-terminated root path.
        let ty = unsafe { GetDriveTypeW(root.as_ptr()) };
        if ty == DRIVE_FIXED || ty == DRIVE_REMOVABLE {
            out.push(letter);
        }
    }
    out
}

/// (filesystem name, serial) via GetVolumeInformationW; None if unreadable.
pub(crate) fn volume_fs_and_serial(letter: char) -> Option<(String, u32)> {
    let root = wide(&format!("{letter}:\\"));
    let mut fs = [0u16; 64];
    let mut serial: DWORD = 0;
    // SAFETY: buffers sized as declared; unused outputs are null.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as DWORD,
        )
    };
    if ok == 0 {
        return None;
    }
    let n = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    Some((String::from_utf16_lossy(&fs[..n]), serial))
}

/// (bytes per sector, bytes per cluster) via GetDiskFreeSpaceW.
pub(crate) fn disk_geometry(letter: char) -> Option<(u32, u32)> {
    let root = wide(&format!("{letter}:\\"));
    let (mut spc, mut bps, mut fc, mut tc) = (0, 0, 0, 0);
    // SAFETY: all out params are locals.
    let ok = unsafe { GetDiskFreeSpaceW(root.as_ptr(), &mut spc, &mut bps, &mut fc, &mut tc) };
    if ok == 0 || spc == 0 || bps == 0 {
        None
    } else {
        Some((bps, bps.saturating_mul(spc)))
    }
}
