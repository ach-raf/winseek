//! Overlapped `ReadDirectoryChangesW` on one directory tree.

use crate::notify::{parse_notify, NotifyPairer, PathEvent};
use crate::sys::*;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// FILE_NAME | DIR_NAME | ATTRIBUTES | SIZE | LAST_WRITE | CREATION (Everything's mask).
pub const NOTIFY_FILTER: u32 = 0x5f;

const MAX_BUF: usize = 64 * 1024;
const MIN_BUF: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    Events(Vec<PathEvent>),
    /// Zero-byte completion, `ERROR_NOTIFY_ENUM_DIR`, or an unparsable buffer: caller must rescan.
    Overflow,
    Timeout,
}

pub struct DirWatch {
    dir: HANDLE,
    event: HANDLE,
    /// Boxed so its address stays stable while the kernel owns it.
    ov: Box<OVERLAPPED>,
    /// u64 backing store: DWORD (indeed 8-byte) aligned, never reallocated.
    buf: Vec<u64>,
    buf_size: usize,
    pairer: NotifyPairer,
    pending: bool,
    cancelled: Arc<AtomicBool>,
}

// SAFETY: the raw handles are kernel objects usable from any thread, and the OVERLAPPED/buffer
// are owned exclusively by this struct (all access goes through `&mut self`).
unsafe impl Send for DirWatch {}

/// Cancels the pending request of a [`DirWatch`] from another thread. The `DirWatch` must
/// outlive every call to [`WatchCanceller::cancel`] (the handle value is not reference counted).
#[derive(Clone)]
pub struct WatchCanceller {
    handle: HANDLE,
    flag: Arc<AtomicBool>,
}

impl WatchCanceller {
    /// `CancelIoEx` on the directory handle; the next/pending `poll` returns `Err`.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        // SAFETY: handle is a directory handle owned by a live DirWatch; null OVERLAPPED
        // cancels every request on it.
        unsafe {
            CancelIoEx(self.handle, std::ptr::null_mut());
        }
    }
}

impl DirWatch {
    /// Open `root` and immediately issue the first request, so changes from now on are captured.
    /// No `FILE_FLAG_NO_BUFFERING` (meaningless on a directory handle).
    pub fn open(root: &str) -> io::Result<DirWatch> {
        let dir = open_handle(
            &long_path(root),
            FILE_LIST_DIRECTORY,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
        )?;
        // SAFETY: no security attributes / name; manual-reset, initially non-signaled.
        let ev = unsafe { CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
        if ev == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut w = DirWatch {
            dir: dir.0,
            event: ev,
            ov: Box::new(OVERLAPPED { internal: 0, internal_high: 0, offset: 0, offset_high: 0, h_event: ev }),
            buf: vec![0u64; MAX_BUF / 8],
            buf_size: MAX_BUF,
            pairer: NotifyPairer::new(),
            pending: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        // From here on `w`'s Drop owns both handles.
        std::mem::forget(dir);
        w.issue()?;
        Ok(w)
    }

    /// Issue a request, halving the buffer on `ERROR_INVALID_PARAMETER` (down to 4 KiB).
    fn issue(&mut self) -> io::Result<()> {
        loop {
            self.ov.internal = 0;
            self.ov.internal_high = 0;
            self.ov.offset = 0;
            self.ov.offset_high = 0;
            self.ov.h_event = self.event;
            // SAFETY: `buf` is at least `buf_size` bytes, DWORD aligned and not moved while
            // pending; `ov` is boxed (stable address) and outlives the request because Drop
            // cancels and waits; bytes-returned is null as required for overlapped use.
            let ok = unsafe {
                ReadDirectoryChangesW(
                    self.dir,
                    self.buf.as_mut_ptr() as *mut _,
                    self.buf_size as u32,
                    1,
                    NOTIFY_FILTER,
                    std::ptr::null_mut(),
                    &mut *self.ov,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 {
                self.pending = true;
                return Ok(());
            }
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(ERROR_INVALID_PARAMETER) && self.buf_size > MIN_BUF {
                self.buf_size = (self.buf_size / 2).max(MIN_BUF);
                continue;
            }
            return Err(e);
        }
    }

    /// Wait up to `timeout_ms` for the pending request. On data: parse + pair and re-issue the
    /// next request before returning. See [`Poll`]. `Err` after `cancel()` or on other errors.
    pub fn poll(&mut self, timeout_ms: u32) -> io::Result<Poll> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED));
        }
        if !self.pending {
            self.issue()?;
        }
        // SAFETY: `event` is a live event handle owned by self.
        let w = unsafe { WaitForSingleObject(self.event, timeout_ms) };
        if w == WAIT_TIMEOUT {
            return Ok(Poll::Timeout);
        }
        if w != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut n: u32 = 0;
        // SAFETY: `ov` is the OVERLAPPED of the pending request on `dir`; the event is
        // signaled so the request is complete (bWait = FALSE).
        let ok = unsafe { GetOverlappedResult(self.dir, &mut *self.ov, &mut n, 0) };
        self.pending = false;
        if ok == 0 {
            let e = io::Error::last_os_error();
            return match e.raw_os_error() {
                Some(ERROR_NOTIFY_ENUM_DIR) => {
                    self.issue()?;
                    Ok(Poll::Overflow)
                }
                Some(ERROR_INVALID_PARAMETER) => {
                    self.buf_size = (self.buf_size / 2).max(MIN_BUF);
                    self.issue()?;
                    Ok(Poll::Timeout)
                }
                _ => Err(e),
            };
        }
        if n == 0 {
            self.issue()?;
            return Ok(Poll::Overflow);
        }
        let len = (n as usize).min(self.buf_size);
        // SAFETY: the first `len` bytes of the initialised u64 buffer were written by the kernel;
        // no request is pending so nothing else touches the buffer.
        let bytes = unsafe { std::slice::from_raw_parts(self.buf.as_ptr() as *const u8, len) };
        let parsed = parse_notify(bytes);
        // Re-arm before returning; if this fails the next poll retries `issue` and reports it.
        let _ = self.issue();
        match parsed {
            Ok(raw) => {
                let mut out = Vec::new();
                self.pairer.push_all(raw, &mut out);
                Ok(Poll::Events(out))
            }
            Err(_) => Ok(Poll::Overflow),
        }
    }

    /// The raw directory handle (for `CM_Register_Notification`).
    pub fn raw_handle(&self) -> isize {
        self.dir
    }

    pub fn canceller(&self) -> WatchCanceller {
        WatchCanceller { handle: self.dir, flag: self.cancelled.clone() }
    }
}

impl Drop for DirWatch {
    fn drop(&mut self) {
        if self.pending {
            let mut n: u32 = 0;
            // SAFETY: cancel our request, then block until the kernel is done with `buf` and
            // `ov`; only then are they freed (field drop happens after this function).
            unsafe {
                CancelIoEx(self.dir, &mut *self.ov);
                GetOverlappedResult(self.dir, &mut *self.ov, &mut n, 1);
            }
        }
        // SAFETY: both handles are owned by self and closed exactly once.
        unsafe {
            CloseHandle(self.dir);
            CloseHandle(self.event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::time::{Duration, Instant};

    fn collect(w: &mut DirWatch, want: &[PathEvent], secs: u64) -> Vec<PathEvent> {
        let mut all = Vec::new();
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end && !want.iter().all(|e| all.contains(e)) {
            if let Poll::Events(ev) = w.poll(200).unwrap() {
                all.extend(ev);
            }
        }
        all
    }

    #[test]
    fn real_events() {
        let t = TempDir::new("watch");
        std::fs::write(t.path().join("gone.txt"), b"x").unwrap();
        std::fs::write(t.path().join("mod.txt"), b"x").unwrap();
        let mut w = DirWatch::open(t.s()).unwrap();
        std::fs::write(t.path().join("new.txt"), b"hello").unwrap();
        std::fs::create_dir(t.path().join("sub")).unwrap();
        std::fs::write(t.path().join("sub").join("inner.txt"), b"i").unwrap();
        std::fs::rename(t.path().join("new.txt"), t.path().join("renamed.txt")).unwrap();
        std::fs::remove_file(t.path().join("gone.txt")).unwrap();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(t.path().join("mod.txt")).unwrap();
            f.write_all(b"more").unwrap();
        }
        let want = vec![
            PathEvent::Added("new.txt".into()),
            PathEvent::Added("sub".into()),
            PathEvent::Added("sub\\inner.txt".into()),
            PathEvent::Renamed { old: "new.txt".into(), new: "renamed.txt".into() },
            PathEvent::Removed("gone.txt".into()),
            PathEvent::Modified("mod.txt".into()),
        ];
        let got = collect(&mut w, &want, 5);
        for e in &want {
            assert!(got.contains(e), "missing {e:?} in {got:?}");
        }
    }

    #[test]
    fn cancel_makes_poll_fail() {
        let t = TempDir::new("watchcancel");
        let mut w = DirWatch::open(t.s()).unwrap();
        let c = w.canceller();
        let c2 = c.clone();
        let th = std::thread::spawn(move || c2.cancel());
        th.join().unwrap();
        assert!(w.poll(2000).is_err());
        drop(c);
    }

    #[test]
    fn cancel_while_blocked_in_poll() {
        let t = TempDir::new("watchcancel2");
        let mut w = DirWatch::open(t.s()).unwrap();
        let c = w.canceller();
        let th = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.cancel();
        });
        let r = w.poll(5000);
        th.join().unwrap();
        assert!(r.is_err());
    }

    #[test]
    fn drop_with_pending_request() {
        let t = TempDir::new("watchdrop");
        let w = DirWatch::open(t.s()).unwrap();
        std::fs::write(t.path().join("a"), b"1").unwrap();
        drop(w);
        let w = DirWatch::open(t.s()).unwrap();
        drop(w);
    }

    #[test]
    fn open_missing_fails() {
        let t = TempDir::new("watchmissing");
        assert!(DirWatch::open(t.path().join("nope").to_str().unwrap()).is_err());
    }
}
