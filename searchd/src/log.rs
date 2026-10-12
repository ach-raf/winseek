//! Tiny logger: every line goes to stderr AND the log file (elevated runs lose stdout).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static FILE: Mutex<Option<File>> = Mutex::new(None);

/// Open (append) the log file. Failure is reported on stderr; logging then goes to stderr only.
pub fn init(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => *FILE.lock().unwrap() = Some(f),
        Err(e) => eprintln!("searchd: cannot open log file {}: {e} (logging to stderr only)", path.display()),
    }
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC) for a unix time in milliseconds.
pub fn timestamp(unix_ms: u64) -> String {
    let secs = unix_ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil-from-days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        unix_ms % 1000
    )
}

pub fn write_line(level: &str, msg: &str) {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let line = format!("{} {level:5} {msg}\n", timestamp(ms));
    let _ = std::io::stderr().write_all(line.as_bytes());
    if let Some(f) = FILE.lock().unwrap().as_mut() {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

macro_rules! log_info { ($($a:tt)*) => { $crate::log::write_line("INFO", &format!($($a)*)) } }
macro_rules! log_warn { ($($a:tt)*) => { $crate::log::write_line("WARN", &format!($($a)*)) } }
macro_rules! log_error { ($($a:tt)*) => { $crate::log::write_line("ERROR", &format!($($a)*)) } }
pub(crate) use {log_error as error, log_info as info, log_warn as warn};

#[cfg(test)]
mod tests {
    #[test]
    fn timestamp_format() {
        assert_eq!(super::timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(super::timestamp(1_709_164_800_123), "2024-02-29T00:00:00.123Z");
    }
}
