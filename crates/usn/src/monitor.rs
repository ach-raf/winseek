//! USN journal monitor (Windows ioctls via `winseek_ntfs::Handle`; compile-only,
//! not unit-testable without a volume).
//!
//! Use a handle from `winseek_ntfs::open_volume_for_usn` (GENERIC_READ +
//! NO_BUFFERING). The monitor reads non-blocking (Timeout 0, BytesToWaitFor 0),
//! asking for all reasons with ReturnOnlyOnClose = 0, and polls.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use winseek_ntfs::Handle;

use crate::pairer::{Change, RenamePairer};
use crate::parse::{parse_usn_buffer, UsnError, UsnRecord};

pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00f4;
pub const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00bb;
/// Everything uses 0xfff8; 64 KiB is fine.
pub const DEFAULT_BUF: usize = 64 * 1024;

const ERROR_JOURNAL_ENTRY_DELETED: i32 = 1181;

/// Where to resume. Persist this (e.g. per volume) between runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalCursor {
    pub journal_id: u64,
    pub next_usn: i64,
}

/// Selected fields of USN_JOURNAL_DATA_V0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalInfo {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

#[derive(Debug)]
pub enum MonitorError {
    Io(io::Error),
    Parse(UsnError),
    /// Query returned fewer bytes than USN_JOURNAL_DATA_V0.
    ShortReply(usize),
}

impl std::fmt::Display for MonitorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MonitorError::Io(e) => write!(f, "usn ioctl failed: {e}"),
            MonitorError::Parse(e) => write!(f, "usn parse failed: {e}"),
            MonitorError::ShortReply(n) => write!(f, "short USN query reply ({n} bytes)"),
        }
    }
}

impl std::error::Error for MonitorError {}

#[derive(Debug)]
pub enum ReadOutcome {
    /// Parsed records (possibly empty) and the cursor to continue from.
    Records(Vec<UsnRecord>, JournalCursor),
    /// Journal ID changed or the cursor fell off the tail: rescan the volume.
    JournalReset,
    Error(MonitorError),
}

fn le_u64(b: &[u8], o: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(v)
}

/// FSCTL_QUERY_USN_JOURNAL.
pub fn query_journal(handle: &Handle) -> Result<JournalInfo, MonitorError> {
    let mut out = [0u8; 64];
    let n = handle
        .ioctl(FSCTL_QUERY_USN_JOURNAL, &[], &mut out)
        .map_err(MonitorError::Io)?;
    if n < 32 {
        return Err(MonitorError::ShortReply(n));
    }
    Ok(JournalInfo {
        journal_id: le_u64(&out, 0),
        first_usn: le_u64(&out, 8) as i64,
        next_usn: le_u64(&out, 16) as i64,
        lowest_valid_usn: le_u64(&out, 24) as i64,
    })
}

/// Which `READ_USN_JOURNAL_DATA` / record version to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordVersion {
    /// READ_USN_JOURNAL_DATA_V0 (0x28 bytes) -> USN_RECORD_V2 (NTFS, 64-bit FRNs).
    #[default]
    V2,
    /// READ_USN_JOURNAL_DATA_V1 (0x30 bytes, Min/MaxMajorVersion = 3) -> USN_RECORD_V3
    /// (ReFS, 128-bit FRNs). Everything builds exactly this request for ReFS
    /// (constants 0xffffffff and 0x00030003, pass 4 `FUN_14004ca10`).
    V3,
}

/// READ_USN_JOURNAL_DATA_V0 (0x28 bytes) or _V1 (0x30 bytes).
fn read_request(cursor: &JournalCursor, version: RecordVersion) -> Vec<u8> {
    let mut r = vec![0u8; if version == RecordVersion::V3 { 0x30 } else { 0x28 }];
    r[0..8].copy_from_slice(&cursor.next_usn.to_le_bytes());
    r[8..12].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // ReasonMask: all
    // ReturnOnlyOnClose = 0, Timeout = 0, BytesToWaitFor = 0 (non-blocking)
    r[32..40].copy_from_slice(&cursor.journal_id.to_le_bytes());
    if version == RecordVersion::V3 {
        r[40..42].copy_from_slice(&3u16.to_le_bytes()); // MinMajorVersion
        r[42..44].copy_from_slice(&3u16.to_le_bytes()); // MaxMajorVersion
    }
    r
}

/// One FSCTL_READ_USN_JOURNAL call into `buf` (use >= 4 KiB; `DEFAULT_BUF`).
pub fn read_journal(handle: &Handle, cursor: JournalCursor, version: RecordVersion, buf: &mut [u8]) -> ReadOutcome {
    let req = read_request(&cursor, version);
    match handle.ioctl(FSCTL_READ_USN_JOURNAL, &req, buf) {
        Ok(n) => match parse_usn_buffer(&buf[..n]) {
            Ok((next, recs)) => {
                ReadOutcome::Records(recs, JournalCursor { journal_id: cursor.journal_id, next_usn: next })
            }
            Err(e) => ReadOutcome::Error(MonitorError::Parse(e)),
        },
        Err(e) => match e.raw_os_error() {
            Some(ERROR_JOURNAL_ENTRY_DELETED) => ReadOutcome::JournalReset,
            _ => ReadOutcome::Error(MonitorError::Io(e)),
        },
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MonitorConfig {
    /// Max time changes are held before `on_batch` is called (~1 s).
    pub batch_interval: Duration,
    /// Sleep after a read error before retrying (30 s).
    pub retry_delay: Duration,
    /// Poll sleep when the journal is drained.
    pub poll_interval: Duration,
    pub buffer_size: usize,
    /// RenamePairer staleness window, in records.
    pub rename_stale_after: u64,
    /// V2 for NTFS, V3 for ReFS.
    pub version: RecordVersion,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            batch_interval: Duration::from_secs(1),
            retry_delay: Duration::from_secs(30),
            poll_interval: Duration::from_millis(250),
            buffer_size: DEFAULT_BUF,
            rename_stale_after: 10_000,
            version: RecordVersion::V2,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MonitorExit {
    Stopped,
    /// Caller must rescan the volume and restart with a fresh cursor.
    JournalReset,
}

/// Sleep up to `d`, waking early when `stop` is set.
fn sleep_stoppable(d: Duration, stop: &AtomicBool) {
    let end = Instant::now() + d;
    while !stop.load(Ordering::Relaxed) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
}

/// Blocking monitor loop. Reads until drained, pairs renames, and calls
/// `on_batch(changes, cursor)` at most once per `batch_interval` (and when
/// stopping) with the cursor to persist. On any read error it waits
/// `retry_delay`, then re-queries the journal: a different journal ID or a
/// cursor below `lowest_valid_usn` returns `JournalReset`. Parked rename-olds
/// are in-memory only (lost on restart, which at worst loses one rename).
///
/// Run it on a dedicated thread (lower its priority if desired).
pub fn run_monitor(
    handle: &Handle,
    mut cursor: JournalCursor,
    cfg: &MonitorConfig,
    stop: &AtomicBool,
    on_batch: &mut impl FnMut(Vec<Change>, JournalCursor),
) -> MonitorExit {
    let mut buf = vec![0u8; cfg.buffer_size.max(4096)];
    let mut pairer = RenamePairer::new(cfg.rename_stale_after);
    let mut pending: Vec<Change> = Vec::new();
    let mut last_flush = Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            if !pending.is_empty() {
                on_batch(std::mem::take(&mut pending), cursor);
            }
            return MonitorExit::Stopped;
        }
        match read_journal(handle, cursor, cfg.version, &mut buf) {
            ReadOutcome::Records(recs, next) => {
                let drained = recs.is_empty();
                for r in recs {
                    pending.extend(pairer.push(r));
                }
                cursor = next;
                if !pending.is_empty() && (drained || last_flush.elapsed() >= cfg.batch_interval) {
                    on_batch(std::mem::take(&mut pending), cursor);
                    last_flush = Instant::now();
                }
                if drained {
                    sleep_stoppable(cfg.poll_interval, stop);
                }
            }
            ReadOutcome::JournalReset => return MonitorExit::JournalReset,
            ReadOutcome::Error(_) => {
                sleep_stoppable(cfg.retry_delay, stop);
                match query_journal(handle) {
                    Ok(info) if info.journal_id != cursor.journal_id || cursor.next_usn < info.lowest_valid_usn => {
                        return MonitorExit::JournalReset;
                    }
                    _ => {} // transient; loop retries
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_request_layout() {
        let cur = JournalCursor { journal_id: 0x1122_3344_5566_7788, next_usn: 4096 };
        let r = read_request(&cur, RecordVersion::V2);
        assert_eq!(r.len(), 0x28);
        assert_eq!(&r[0..8], &4096i64.to_le_bytes());
        assert_eq!(&r[8..12], &0xFFFF_FFFFu32.to_le_bytes());
        assert_eq!(&r[12..32], &[0u8; 20]);
        assert_eq!(&r[32..40], &0x1122_3344_5566_7788u64.to_le_bytes());
        // V1 request for ReFS: same prefix, then MinMajorVersion = MaxMajorVersion = 3
        // (Everything's 0x00030003), padded to 0x30.
        let r3 = read_request(&cur, RecordVersion::V3);
        assert_eq!(r3.len(), 0x30);
        assert_eq!(&r3[..0x28], &r[..]);
        assert_eq!(&r3[40..44], &0x0003_0003u32.to_le_bytes());
        assert_eq!(&r3[44..48], &[0u8; 4]);
    }
}
