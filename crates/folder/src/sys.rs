//! Raw Win32 declarations shared by the modules (kernel32 only).

#![allow(non_snake_case, clippy::upper_case_acronyms, clippy::too_many_arguments)]

use std::ffi::{c_void, OsStr};
use std::io;
use std::os::windows::ffi::OsStrExt;

pub(crate) type HANDLE = isize;
pub(crate) type DWORD = u32;
pub(crate) type BOOL = i32;

pub(crate) const INVALID_HANDLE_VALUE: HANDLE = -1;
pub(crate) const OPEN_EXISTING: DWORD = 3;
pub(crate) const FILE_SHARE_ALL: DWORD = 0x7; // read | write | delete
pub(crate) const FILE_LIST_DIRECTORY: DWORD = 0x1;
pub(crate) const FILE_READ_ATTRIBUTES: DWORD = 0x80;
pub(crate) const SYNCHRONIZE: DWORD = 0x0010_0000;
pub(crate) const FILE_FLAG_BACKUP_SEMANTICS: DWORD = 0x0200_0000;
pub(crate) const FILE_FLAG_OVERLAPPED: DWORD = 0x4000_0000;
pub(crate) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

pub(crate) const ERROR_FILE_NOT_FOUND: i32 = 2;
pub(crate) const ERROR_NO_MORE_FILES: i32 = 18;
pub(crate) const ERROR_INVALID_PARAMETER: i32 = 87;
pub(crate) const ERROR_OPERATION_ABORTED: i32 = 995;
pub(crate) const ERROR_NOTIFY_ENUM_DIR: i32 = 1022;

pub(crate) const WAIT_OBJECT_0: DWORD = 0;
pub(crate) const WAIT_TIMEOUT: DWORD = 0x102;

/// `FIND_FIRST_EX_LARGE_FETCH`.
pub(crate) const FIND_FIRST_EX_LARGE_FETCH: DWORD = 2;
/// `FindExInfoBasic`.
pub(crate) const FIND_EX_INFO_BASIC: i32 = 1;
/// `FindExSearchNameMatch`.
pub(crate) const FIND_EX_SEARCH_NAME_MATCH: i32 = 0;

#[repr(C)]
pub(crate) struct OVERLAPPED {
    pub internal: usize,
    pub internal_high: usize,
    pub offset: u32,
    pub offset_high: u32,
    pub h_event: HANDLE,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct FILETIME {
    pub lo: u32,
    pub hi: u32,
}

impl FILETIME {
    pub fn ticks(self) -> i64 {
        (((self.hi as u64) << 32) | self.lo as u64) as i64
    }
}

#[repr(C)]
pub(crate) struct WIN32_FIND_DATAW {
    pub dwFileAttributes: u32,
    pub ftCreationTime: FILETIME,
    pub ftLastAccessTime: FILETIME,
    pub ftLastWriteTime: FILETIME,
    pub nFileSizeHigh: u32,
    pub nFileSizeLow: u32,
    pub dwReserved0: u32,
    pub dwReserved1: u32,
    pub cFileName: [u16; 260],
    pub cAlternateFileName: [u16; 14],
}

#[repr(C)]
pub(crate) struct WIN32_FILE_ATTRIBUTE_DATA {
    pub dwFileAttributes: u32,
    pub ftCreationTime: FILETIME,
    pub ftLastAccessTime: FILETIME,
    pub ftLastWriteTime: FILETIME,
    pub nFileSizeHigh: u32,
    pub nFileSizeLow: u32,
}

#[link(name = "kernel32")]
extern "system" {
    pub(crate) fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: *mut c_void,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    pub(crate) fn CloseHandle(hObject: HANDLE) -> BOOL;
    pub(crate) fn CreateEventW(
        lpEventAttributes: *mut c_void,
        bManualReset: BOOL,
        bInitialState: BOOL,
        lpName: *const u16,
    ) -> HANDLE;
    pub(crate) fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: DWORD) -> DWORD;
    pub(crate) fn GetOverlappedResult(
        hFile: HANDLE,
        lpOverlapped: *mut OVERLAPPED,
        lpNumberOfBytesTransferred: *mut DWORD,
        bWait: BOOL,
    ) -> BOOL;
    pub(crate) fn CancelIoEx(hFile: HANDLE, lpOverlapped: *mut OVERLAPPED) -> BOOL;
    pub(crate) fn ReadDirectoryChangesW(
        hDirectory: HANDLE,
        lpBuffer: *mut c_void,
        nBufferLength: DWORD,
        bWatchSubtree: BOOL,
        dwNotifyFilter: DWORD,
        lpBytesReturned: *mut DWORD,
        lpOverlapped: *mut OVERLAPPED,
        lpCompletionRoutine: *mut c_void,
    ) -> BOOL;
    pub(crate) fn GetFileInformationByHandleEx(
        hFile: HANDLE,
        FileInformationClass: i32,
        lpFileInformation: *mut c_void,
        dwBufferSize: DWORD,
    ) -> BOOL;
    pub(crate) fn FindFirstFileExW(
        lpFileName: *const u16,
        fInfoLevelId: i32,
        lpFindFileData: *mut c_void,
        fSearchOp: i32,
        lpSearchFilter: *mut c_void,
        dwAdditionalFlags: DWORD,
    ) -> HANDLE;
    pub(crate) fn FindNextFileW(hFindFile: HANDLE, lpFindFileData: *mut c_void) -> BOOL;
    pub(crate) fn FindClose(hFindFile: HANDLE) -> BOOL;
    pub(crate) fn GetFileAttributesExW(
        lpFileName: *const u16,
        fInfoLevelId: i32,
        lpFileInformation: *mut c_void,
    ) -> BOOL;
    pub(crate) fn GetLogicalDrives() -> DWORD;
    pub(crate) fn GetDriveTypeW(lpRootPathName: *const u16) -> DWORD;
    pub(crate) fn GetVolumeInformationW(
        lpRootPathName: *const u16,
        lpVolumeNameBuffer: *mut u16,
        nVolumeNameSize: DWORD,
        lpVolumeSerialNumber: *mut DWORD,
        lpMaximumComponentLength: *mut DWORD,
        lpFileSystemFlags: *mut DWORD,
        lpFileSystemNameBuffer: *mut u16,
        nFileSystemNameSize: DWORD,
    ) -> BOOL;
}

/// NUL-terminated UTF-16 for Win32 calls.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// UTF-16 up to the first NUL (or the end of the slice), lossy.
pub(crate) fn from_wide_nul(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// Apply the `\\?\` long-path prefix to absolute paths (`E:\` -> `\\?\E:\`, UNC -> `\\?\UNC\..`).
/// Relative / already-prefixed paths are returned unchanged (apart from `/` -> `\`).
pub(crate) fn long_path(p: &str) -> String {
    let p = p.replace('/', "\\");
    if p.starts_with("\\\\?\\") || p.starts_with("\\\\.\\") {
        p
    } else if let Some(rest) = p.strip_prefix("\\\\") {
        format!("\\\\?\\UNC\\{rest}")
    } else {
        let b = p.as_bytes();
        if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
            format!("\\\\?\\{p}")
        } else {
            p
        }
    }
}

/// `dir` + `\` + `name` without doubling the separator.
pub(crate) fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

pub(crate) fn last_code() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Owned HANDLE closed on drop.
pub(crate) struct OwnedHandle(pub HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// `FindClose` on drop.
pub(crate) struct FindHandle(pub HANDLE);

impl Drop for FindHandle {
    fn drop(&mut self) {
        // SAFETY: valid find handle from FindFirstFileExW, closed once.
        unsafe {
            FindClose(self.0);
        }
    }
}

/// Open a file/directory handle (`path` is used as given).
pub(crate) fn open_handle(path: &str, access: DWORD, flags: DWORD) -> io::Result<OwnedHandle> {
    let w = wide(path);
    // SAFETY: `w` is NUL-terminated; no security attributes or template file.
    let h = unsafe {
        CreateFileW(w.as_ptr(), access, FILE_SHARE_ALL, std::ptr::null_mut(), OPEN_EXISTING, flags, 0)
    };
    if h == INVALID_HANDLE_VALUE || h == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(OwnedHandle(h))
    }
}
