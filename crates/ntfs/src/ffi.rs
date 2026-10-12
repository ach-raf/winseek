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
pub(crate) const FILE_FLAG_OVERLAPPED: DWORD = 0x4000_0000;
const ERROR_IO_PENDING: i32 = 997;
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
    fn CreateEventW(
        lpEventAttributes: *mut core::ffi::c_void,
        bManualReset: BOOL,
        bInitialState: BOOL,
        lpName: *const u16,
    ) -> HANDLE;
    fn GetOverlappedResult(
        hFile: HANDLE,
        lpOverlapped: *mut OVERLAPPED,
        lpNumberOfBytesTransferred: *mut DWORD,
        bWait: BOOL,
    ) -> BOOL;
    fn CancelIoEx(hFile: HANDLE, lpOverlapped: *mut OVERLAPPED) -> BOOL;
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

#[repr(C)]
struct OVERLAPPED {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: HANDLE,
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
        Self::open_path(&format!("\\\\.\\{}:", letter.to_ascii_uppercase()), access, flags)
    }

    /// Open an arbitrary path (volume device or file) with the given access and flags.
    pub(crate) fn open_path(path: &str, access: DWORD, flags: DWORD) -> io::Result<Handle> {
        let path = wide(path);
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

/// One in-flight `FILE_FLAG_OVERLAPPED` read. Owns its buffer, so the memory
/// stays valid for the kernel until the request completes; dropping a request
/// that was never collected cancels it and waits for the cancellation.
pub(crate) struct AsyncRead {
    vol: HANDLE,
    ov: Box<OVERLAPPED>,
    pending: bool,
    buf: Option<AlignedBuf>,
}

impl AsyncRead {
    /// Issue a read of `len` bytes at `offset` into the start of `buf`. `vol`
    /// must have been opened with `FILE_FLAG_OVERLAPPED` and outlive the read.
    pub(crate) fn start(vol: &Handle, mut buf: AlignedBuf, len: usize, offset: u64) -> io::Result<AsyncRead> {
        assert!(len <= buf.as_mut_slice().len() && len <= u32::MAX as usize);
        // SAFETY: plain event creation; manual-reset, unsignalled, unnamed.
        let ev = unsafe { CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
        if ev == 0 {
            return Err(io::Error::last_os_error());
        }
        let ov = Box::new(OVERLAPPED {
            internal: 0,
            internal_high: 0,
            offset: offset as u32,
            offset_high: (offset >> 32) as u32,
            h_event: ev,
        });
        let mut r = AsyncRead { vol: vol.0, ov, pending: false, buf: None };
        let ptr = buf.as_mut_slice().as_mut_ptr();
        r.buf = Some(buf);
        // SAFETY: `ptr` is valid for `len` bytes and kept alive (and unmoved: heap)
        // by `r.buf`; the boxed OVERLAPPED has a stable address until collected.
        let ok = unsafe {
            ReadFile(
                vol.0,
                ptr.cast(),
                len as DWORD,
                std::ptr::null_mut(),
                (&mut *r.ov as *mut OVERLAPPED).cast(),
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_IO_PENDING) {
                return Err(e);
            }
        }
        // Completed inline or pending: either way the result is read via GetOverlappedResult.
        r.pending = true;
        Ok(r)
    }

    /// Wait for completion; returns the byte count (or error) and gives the buffer back.
    pub(crate) fn finish(mut self) -> (io::Result<usize>, AlignedBuf) {
        let mut done: DWORD = 0;
        // SAFETY: the OVERLAPPED belongs to a request issued on `self.vol`.
        let ok = unsafe { GetOverlappedResult(self.vol, &mut *self.ov, &mut done, 1) };
        self.pending = false;
        let res = if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(done as usize) };
        let buf = self.buf.take().expect("buffer present until finish");
        (res, buf)
    }
}

impl Drop for AsyncRead {
    fn drop(&mut self) {
        if self.pending {
            let mut done: DWORD = 0;
            // SAFETY: cancel then wait so the kernel is done with the buffer before it is freed.
            unsafe {
                CancelIoEx(self.vol, &mut *self.ov);
                GetOverlappedResult(self.vol, &mut *self.ov, &mut done, 1);
            }
        }
        // SAFETY: event created in `start`, owned by this request.
        unsafe { CloseHandle(self.ov.h_event) };
    }
}

/// Page-aligned (4096) zeroed heap buffer (volume reads need sector alignment).
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
    #[cfg(test)]
    pub(crate) fn addr(&self) -> usize {
        self.ptr as usize
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_buf_is_page_aligned() {
        for sz in [1, 512, 4096, 1 << 20, (1 << 20) + 4096] {
            assert_eq!(AlignedBuf::new(sz).addr() % 4096, 0);
        }
    }

    /// Overlapped + unbuffered reads of a plain file, several in flight,
    /// collected out of issue order, must return the right bytes.
    #[test]
    fn async_reads_return_correct_bytes() {
        let path = std::env::temp_dir().join(format!("winseek-ffi-async-{}.bin", std::process::id()));
        let data: Vec<u8> = (0..(1usize << 20)).map(|i| (i * 31 % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let h = Handle::open_path(
            path.to_str().unwrap(),
            GENERIC_READ,
            FILE_FLAG_OVERLAPPED | FILE_FLAG_NO_BUFFERING,
        )
        .unwrap();
        let reqs = [(0u64, 65536usize), (65536, 131072), (8192, 4096), ((1 << 20) - 4096, 4096)];
        let inflight: Vec<_> = reqs
            .iter()
            .map(|&(off, len)| AsyncRead::start(&h, AlignedBuf::new(len), len, off).unwrap())
            .collect();
        for (r, &(off, len)) in inflight.into_iter().zip(&reqs).rev() {
            let (res, mut buf) = r.finish();
            assert_eq!(res.unwrap(), len);
            assert_eq!(&buf.as_mut_slice()[..len], &data[off as usize..off as usize + len]);
        }
        // Dropping an uncollected request must not crash or leak a live buffer.
        let r = AsyncRead::start(&h, AlignedBuf::new(1 << 20), 1 << 20, 0).unwrap();
        drop(r);
        drop(h);
        let _ = std::fs::remove_file(&path);
    }
}
