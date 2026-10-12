//! Windows named-pipe server/client (raw FFI, no `windows` crate).

use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

type Handle = *mut c_void;
type Bool = i32;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const PIPE_ACCESS_DUPLEX: u32 = 3;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const PIPE_TYPE_BYTE: u32 = 0;
const PIPE_READMODE_BYTE: u32 = 0;
const PIPE_WAIT: u32 = 0;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 8;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_BROKEN_PIPE: i32 = 109;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_PIPE_CONNECTED: i32 = 535;
const SDDL_REVISION_1: u32 = 1;

/// SYSTEM: full; interactive user: read/write. Nobody else (protected DACL).
pub const DEFAULT_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)";

#[repr(C)]
struct SecurityAttributes {
    n_length: u32,
    lp_security_descriptor: *mut c_void,
    b_inherit_handle: Bool,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buf: u32,
        in_buf: u32,
        default_timeout: u32,
        sa: *mut SecurityAttributes,
    ) -> Handle;
    fn ConnectNamedPipe(h: Handle, overlapped: *mut c_void) -> Bool;
    fn DisconnectNamedPipe(h: Handle) -> Bool;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        sa: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn WaitNamedPipeW(name: *const u16, timeout_ms: u32) -> Bool;
    fn ReadFile(h: Handle, buf: *mut u8, n: u32, read: *mut u32, overlapped: *mut c_void) -> Bool;
    fn WriteFile(h: Handle, buf: *const u8, n: u32, written: *mut u32, overlapped: *mut c_void) -> Bool;
    fn FlushFileBuffers(h: Handle) -> Bool;
    fn CloseHandle(h: Handle) -> Bool;
    fn LocalFree(p: *mut c_void) -> *mut c_void;
}

#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl: *const u16,
        revision: u32,
        sd: *mut *mut c_void,
        size: *mut u32,
    ) -> Bool;
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// A connected pipe end (server or client). Closed on drop.
pub struct PipeConn {
    h: Handle,
    server: bool,
}

// SAFETY: a pipe HANDLE may be used from any thread.
unsafe impl Send for PipeConn {}

impl Read for PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0u32;
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: valid handle and buffer.
        let ok = unsafe { ReadFile(self.h, buf.as_mut_ptr(), len, &mut n, null_mut()) };
        if ok == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(ERROR_BROKEN_PIPE) {
                return Ok(0);
            }
            return Err(e);
        }
        Ok(n as usize)
    }
}

impl Write for PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut n = 0u32;
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: valid handle and buffer.
        let ok = unsafe { WriteFile(self.h, buf.as_ptr(), len, &mut n, null_mut()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(()) // FlushFileBuffers would block until the peer drained; done in Drop instead.
    }
}

impl Drop for PipeConn {
    fn drop(&mut self) {
        // SAFETY: handle owned by us.
        unsafe {
            if self.server {
                FlushFileBuffers(self.h); // let the client read pending replies
                DisconnectNamedPipe(self.h);
            }
            CloseHandle(self.h);
        }
    }
}

/// Named-pipe server. Always keeps one listening instance so clients never
/// see "file not found" between accepts.
pub struct PipeServer {
    name: Vec<u16>,
    sd: *mut c_void,
    next: Handle,
}

// SAFETY: raw pointers are only owned resources (SD from LocalAlloc, a pipe handle).
unsafe impl Send for PipeServer {}

impl PipeServer {
    /// Create with [`DEFAULT_SDDL`]. Fails if another process already owns the pipe name.
    pub fn bind(name: &str) -> io::Result<Self> {
        Self::bind_with_sddl(name, DEFAULT_SDDL)
    }

    pub fn bind_with_sddl(name: &str, sddl: &str) -> io::Result<Self> {
        let mut sd: *mut c_void = null_mut();
        let w = wide(sddl);
        // SAFETY: out-pointer valid; result freed in Drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut())
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut s = PipeServer { name: wide(name), sd, next: INVALID_HANDLE_VALUE };
        s.next = s.create_instance(true)?; // Drop frees sd on error
        Ok(s)
    }

    fn create_instance(&self, first: bool) -> io::Result<Handle> {
        let mut sa = SecurityAttributes {
            n_length: std::mem::size_of::<SecurityAttributes>() as u32,
            lp_security_descriptor: self.sd,
            b_inherit_handle: 0,
        };
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: all pointers valid for the call.
        let h = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &mut sa,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(h)
    }

    /// Block until a client connects.
    pub fn accept(&mut self) -> io::Result<PipeConn> {
        // SAFETY: valid handle.
        let ok = unsafe { ConnectNamedPipe(self.next, null_mut()) };
        if ok == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_PIPE_CONNECTED) {
                return Err(e);
            }
        }
        let fresh = self.create_instance(false)?;
        let h = std::mem::replace(&mut self.next, fresh);
        Ok(PipeConn { h, server: true })
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        // SAFETY: owned resources.
        unsafe {
            if self.next != INVALID_HANDLE_VALUE {
                CloseHandle(self.next);
            }
            if !self.sd.is_null() {
                LocalFree(self.sd);
            }
        }
    }
}

/// Connect to a pipe, retrying on "not found"/"busy" until `timeout` elapses.
pub fn connect(name: &str, timeout: Duration) -> io::Result<PipeConn> {
    let w = wide(name);
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: valid NUL-terminated name. Identification level: server cannot impersonate us.
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null_mut(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            return Ok(PipeConn { h, server: false });
        }
        let e = io::Error::last_os_error();
        let left = deadline.saturating_duration_since(Instant::now());
        match e.raw_os_error() {
            Some(ERROR_PIPE_BUSY) if !left.is_zero() => {
                let ms = left.as_millis().clamp(1, 250) as u32;
                // SAFETY: valid name.
                unsafe { WaitNamedPipeW(w.as_ptr(), ms) };
            }
            Some(ERROR_FILE_NOT_FOUND) if !left.is_zero() => {
                std::thread::sleep(left.min(Duration::from_millis(50)));
            }
            _ if left.is_zero() => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("pipe connect timed out: {e}")));
            }
            _ => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{read_message, write_message, Hit, Request, Response, Sort};

    fn unique() -> String {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        format!(r"\\.\pipe\winseek-test-{}-{t}", std::process::id())
    }

    #[test]
    fn search_roundtrip_over_pipe() {
        let name = unique();
        let mut server = PipeServer::bind(&name).expect("bind");
        let t = std::thread::spawn(move || {
            for _ in 0..2 {
                let mut c = server.accept().unwrap();
                let req: Request = read_message(&mut c).unwrap();
                let resp = match req {
                    Request::Search { id, query, .. } => Response::SearchResult {
                        id,
                        total: 1,
                        hits: vec![Hit { name: query, path: r"C:\x".into(), is_dir: false, size: Some(3), mtime: None }],
                        elapsed_us: 7,
                    },
                    other => Response::Error { id: other.id(), message: "unsupported".into() },
                };
                write_message(&mut c, &resp).unwrap();
            }
        });
        for i in 0..2u64 {
            let mut c = connect(&name, Duration::from_secs(5)).unwrap();
            let q = Request::Search { id: i, query: "a.txt".into(), offset: 0, limit: 10, sort: Sort::NameAsc };
            write_message(&mut c, &q).unwrap();
            let r: Response = read_message(&mut c).unwrap();
            match r {
                Response::SearchResult { id, total, hits, .. } => {
                    assert_eq!((id, total), (i, 1));
                    assert_eq!(hits[0].name, "a.txt");
                }
                other => panic!("{other:?}"),
            }
        }
        t.join().unwrap();
    }

    #[test]
    fn connect_times_out_when_absent() {
        let e = connect(&unique(), Duration::from_millis(150)).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn second_bind_same_name_fails() {
        let name = unique();
        let _a = PipeServer::bind(&name).unwrap();
        assert!(PipeServer::bind(&name).is_err());
    }
}
