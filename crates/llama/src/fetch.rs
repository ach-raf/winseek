//! Resumable HTTP download with SHA-256 verification and atomic rename.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::manifest::is_unpinned;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

fn verify(path: &Path, expected: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(Error::Sha256Mismatch { expected: expected.to_ascii_lowercase(), actual })
    }
}

/// Retry / stall policy for [`fetch_with`].
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// Abort an attempt if fewer than this many bytes/s arrive over `stall_window`.
    pub min_bytes_per_sec: u64,
    pub stall_window: Duration,
    pub max_attempts: u32,
    /// Upper bound for the exponential backoff.
    pub backoff_cap: Duration,
    /// First backoff step (doubles each attempt: 1, 2, 4, 8, 16 s ...). Tests shrink it.
    pub backoff_base: Duration,
}

impl Default for FetchOptions {
    fn default() -> Self {
        FetchOptions {
            min_bytes_per_sec: 64 * 1024,
            stall_window: Duration::from_secs(20),
            max_attempts: 8,
            backoff_cap: Duration::from_secs(30),
            backoff_base: Duration::from_secs(1),
        }
    }
}

impl FetchOptions {
    fn backoff(&self, attempt: u32) -> Duration {
        let mult = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
        self.backoff_base.saturating_mul(mult).min(self.backoff_cap)
    }
}

/// Replace a leading `https://huggingface.co` with `endpoint` (like `HF_ENDPOINT`).
pub fn rewrite_hf(url: &str, endpoint: Option<&str>) -> String {
    const HF: &str = "https://huggingface.co";
    if let (Some(ep), Some(rest)) = (endpoint, url.strip_prefix(HF)) {
        let ep = ep.trim().trim_end_matches('/');
        if !ep.is_empty() && (rest.is_empty() || rest.starts_with('/')) {
            return format!("{ep}{rest}");
        }
    }
    url.to_string()
}

/// Download `url` to `dest` with default [`FetchOptions`] (see [`fetch_with`]).
/// `WINSEEK_HF_ENDPOINT` rewrites a Hugging Face prefix.
pub fn fetch(
    url: &str,
    dest: &Path,
    expected_sha256: &str,
    progress_cb: &mut dyn FnMut(Progress),
) -> Result<()> {
    let ep = std::env::var("WINSEEK_HF_ENDPOINT").ok();
    let urls = [rewrite_hf(url, ep.as_deref())];
    fetch_with(&urls, dest, expected_sha256, &FetchOptions::default(), progress_cb)
}

/// Download to `dest` (resuming `dest.part` via Range), verify SHA-256, then
/// atomically rename. No-op if `dest` already has the right hash.
///
/// Attempts rotate across `urls`. Stalls, truncation and transient HTTP/io errors
/// are retried with exponential backoff, keeping `.part` for Range resume. A hash
/// mismatch deletes `.part` and counts as an attempt. The last error is returned
/// once `max_attempts` is exhausted.
pub fn fetch_with(
    urls: &[String],
    dest: &Path,
    expected_sha256: &str,
    opts: &FetchOptions,
    progress_cb: &mut dyn FnMut(Progress),
) -> Result<()> {
    let first = urls.first().ok_or_else(|| Error::NotFound("no download urls".into()))?;
    if is_unpinned(expected_sha256) {
        return Err(Error::UnpinnedHash(first.clone()));
    }
    if dest.is_file() && verify(dest, expected_sha256).is_ok() {
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    let part = part_path(dest);
    let attempts = opts.max_attempts.max(1);
    let mut last_err = Error::Http("no attempts made".into());
    for attempt in 0..attempts {
        if attempt > 0 {
            std::thread::sleep(opts.backoff(attempt - 1));
        }
        let url = &urls[attempt as usize % urls.len()];
        match download_into(url, &part, opts, progress_cb) {
            // 416: `.part` is already complete (or stale); verify it below.
            Ok(Outcome::Done) | Ok(Outcome::RangeNotSatisfiable) => match verify(&part, expected_sha256) {
                Ok(()) => {
                    fs::rename(&part, dest)?; // MoveFileEx(REPLACE_EXISTING) on Windows
                    return Ok(());
                }
                Err(e) => {
                    let _ = fs::remove_file(&part);
                    last_err = e;
                }
            },
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

enum Outcome {
    Done,
    RangeNotSatisfiable,
}

fn download_into(
    url: &str,
    part: &Path,
    opts: &FetchOptions,
    progress_cb: &mut dyn FnMut(Progress),
) -> Result<Outcome> {
    let existing = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    // A read that blocks for a whole stall window is a stall by definition.
    let read_timeout = opts.stall_window.min(Duration::from_secs(30)).max(Duration::from_millis(1));
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(read_timeout)
        .build();
    let mut req = agent.get(url);
    if existing > 0 {
        req = req.set("Range", &format!("bytes={existing}-"));
    }
    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Status(416, _)) => return Ok(Outcome::RangeNotSatisfiable),
        Err(e) => return Err(Error::Http(e.to_string())),
    };
    let (mut offset, total) = if resp.status() == 206 {
        let total = resp
            .header("Content-Range")
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.trim().parse::<u64>().ok());
        (existing, total)
    } else {
        // 200: server ignored Range (or fresh download); restart from scratch.
        let total = resp.header("Content-Length").and_then(|v| v.trim().parse::<u64>().ok());
        (0, total)
    };
    let mut file = if offset > 0 {
        OpenOptions::new().append(true).open(part)?
    } else {
        File::create(part)?
    };
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    progress_cb(Progress { downloaded: offset, total });
    let min_in_window = (opts.min_bytes_per_sec as f64 * opts.stall_window.as_secs_f64()) as u64;
    let mut win_start = Instant::now();
    let mut win_bytes = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                file.flush()?;
                return Err(Error::Stalled(format!("no data for {:?} at {offset} bytes", opts.stall_window)));
            }
            Err(e) => {
                file.flush()?;
                return Err(Error::Io(e));
            }
        };
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        offset += n as u64;
        win_bytes += n as u64;
        progress_cb(Progress { downloaded: offset, total });
        if win_start.elapsed() >= opts.stall_window {
            if win_bytes < min_in_window {
                file.flush()?;
                return Err(Error::Stalled(format!(
                    "{win_bytes} bytes in {:?} (< {min_in_window}) at {offset} bytes",
                    opts.stall_window
                )));
            }
            win_start = Instant::now();
            win_bytes = 0;
        }
    }
    file.flush()?;
    if let Some(t) = total {
        if offset < t {
            // Connection dropped; leave `.part` for the next resume attempt.
            return Err(Error::Http(format!("truncated download: {offset} of {t} bytes")));
        }
    }
    Ok(Outcome::Done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    pub fn temp_dir(tag: &str) -> PathBuf {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("winseek-llama-{tag}-{}-{t}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Serves `body` for `conns` connections, honouring `Range: bytes=N-`. Records Range headers.
    fn serve(body: Vec<u8>, conns: usize, honour_range: bool) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for _ in 0..conns {
                let (mut s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = Some(v.trim().trim_end_matches('-').to_string());
                    }
                }
                seen2.lock().unwrap().push(range.clone());
                let total = body.len();
                let start: usize = range.as_deref().and_then(|r| r.parse().ok()).unwrap_or(0);
                let head = if honour_range && range.is_some() {
                    if start >= total {
                        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    } else {
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            total - 1,
                            total - start
                        )
                    }
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n")
                };
                s.write_all(head.as_bytes()).unwrap();
                if head.starts_with("HTTP/1.1 206") {
                    s.write_all(&body[start..]).unwrap();
                } else if head.starts_with("HTTP/1.1 200") {
                    s.write_all(&body).unwrap();
                }
            }
        });
        (url, seen)
    }

    fn body() -> Vec<u8> {
        (0..20_000u32).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let d = temp_dir("sha");
        let p = d.join("f");
        fs::write(&p, b"abc").unwrap();
        assert_eq!(sha256_file(&p).unwrap(), sha256_hex(b"abc"));
    }

    #[test]
    fn full_download_and_verify() {
        let b = body();
        let (url, seen) = serve(b.clone(), 1, true);
        let d = temp_dir("full");
        let dest = d.join("out").join("f.bin");
        let mut last = None;
        fetch(&url, &dest, &sha256_hex(&b), &mut |p| last = Some(p)).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b);
        assert!(!part_path(&dest).exists());
        assert_eq!(last.unwrap().downloaded, b.len() as u64);
        assert_eq!(seen.lock().unwrap()[0], None);
        // already present + valid => no network (server only accepts 1 conn)
        fetch(&url, &dest, &sha256_hex(&b), &mut |_| {}).unwrap();
    }

    #[test]
    fn resumes_from_part() {
        let b = body();
        let (url, seen) = serve(b.clone(), 1, true);
        let d = temp_dir("resume");
        let dest = d.join("f.bin");
        fs::write(part_path(&dest), &b[..7_000]).unwrap();
        fetch(&url, &dest, &sha256_hex(&b), &mut |_| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b);
        assert_eq!(seen.lock().unwrap()[0].as_deref(), Some("7000"));
    }

    #[test]
    fn restarts_when_server_ignores_range() {
        let b = body();
        let (url, _) = serve(b.clone(), 1, false);
        let d = temp_dir("norange");
        let dest = d.join("f.bin");
        fs::write(part_path(&dest), &b[..5_000]).unwrap();
        fetch(&url, &dest, &sha256_hex(&b), &mut |_| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b);
    }

    #[test]
    fn complete_part_gets_416_then_verified() {
        let b = body();
        let (url, _) = serve(b.clone(), 1, true);
        let d = temp_dir("complete");
        let dest = d.join("f.bin");
        fs::write(part_path(&dest), &b).unwrap();
        fetch(&url, &dest, &sha256_hex(&b), &mut |_| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b);
    }

    #[test]
    fn mismatch_deletes_part_and_leaves_no_dest() {
        let b = body();
        let (url, seen) = serve_script(b.clone(), vec![Beh::Full]);
        let d = temp_dir("bad");
        let dest = d.join("f.bin");
        let opts = FetchOptions { max_attempts: 2, ..fast_opts() };
        let err = fetch_with(&[url], &dest, &sha256_hex(b"other"), &opts, &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::Sha256Mismatch { .. }), "{err}");
        assert!(!dest.exists() && !part_path(&dest).exists());
        // each attempt restarted from scratch (no Range), since .part was deleted
        assert_eq!(*seen.lock().unwrap(), vec![None, None]);
    }

    #[test]
    fn unpinned_hash_refused_without_network() {
        let d = temp_dir("todo");
        let err = fetch("http://127.0.0.1:1/x", &d.join("f"), "TODO-pin-in-CI", &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::UnpinnedHash(_)));
    }

    #[test]
    fn verify_is_case_insensitive() {
        let d = temp_dir("case");
        let p = d.join("f");
        fs::write(&p, b"abc").unwrap();
        verify(&p, &sha256_hex(b"abc").to_ascii_uppercase()).unwrap();
    }

    #[derive(Clone, Copy)]
    enum Beh {
        Full,
        Status500,
        /// Declare the full length, send `n` body bytes, then close.
        Truncate(usize),
        /// Send `n` body bytes, then keep the connection open and silent.
        Hang(usize),
    }

    /// One thread per connection; behaviour `i` for the i-th connection (last one repeats).
    fn serve_script(body: Vec<u8>, script: Vec<Beh>) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for i in 0.. {
                let Ok((mut s, _)) = l.accept() else { return };
                let beh = script[i.min(script.len() - 1)];
                let body = body.clone();
                let seen = seen2.clone();
                std::thread::spawn(move || {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut range = None;
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                            range = Some(v.trim().trim_end_matches('-').to_string());
                        }
                    }
                    seen.lock().unwrap().push(range.clone());
                    let total = body.len();
                    let start: usize = range.as_deref().and_then(|r| r.parse().ok()).unwrap_or(0);
                    if let Beh::Status500 = beh {
                        let _ = s.write_all(
                            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        return;
                    }
                    let head = if range.is_some() {
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            total - 1,
                            total - start
                        )
                    } else {
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n")
                    };
                    let _ = s.write_all(head.as_bytes());
                    let rest = &body[start..];
                    match beh {
                        Beh::Full => {
                            let _ = s.write_all(rest);
                        }
                        Beh::Truncate(n) => {
                            let _ = s.write_all(&rest[..n]);
                        }
                        Beh::Hang(n) => {
                            let _ = s.write_all(&rest[..n]);
                            let _ = s.flush();
                            std::thread::sleep(Duration::from_secs(5));
                        }
                        Beh::Status500 => unreachable!(),
                    }
                });
            }
        });
        (url, seen)
    }

    fn fast_opts() -> FetchOptions {
        FetchOptions {
            backoff_base: Duration::from_millis(1),
            stall_window: Duration::from_secs(5),
            min_bytes_per_sec: 0,
            ..Default::default()
        }
    }

    #[test]
    fn stall_detected_then_range_resume() {
        let b = body();
        let (url, seen) = serve_script(b.clone(), vec![Beh::Hang(8_000), Beh::Full]);
        let d = temp_dir("stall");
        let dest = d.join("f.bin");
        let opts = FetchOptions {
            min_bytes_per_sec: 1 << 20,
            stall_window: Duration::from_millis(200),
            ..fast_opts()
        };
        let t = Instant::now();
        fetch_with(&[url], &dest, &sha256_hex(&b), &opts, &mut |_| {}).unwrap();
        assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
        assert_eq!(fs::read(&dest).unwrap(), b);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], None);
        assert_eq!(seen[1].as_deref(), Some("8000"));
    }

    #[test]
    fn stall_error_when_attempts_exhausted() {
        let b = body();
        let (url, _) = serve_script(b.clone(), vec![Beh::Hang(100)]);
        let d = temp_dir("stall-exhaust");
        let opts = FetchOptions { stall_window: Duration::from_millis(100), max_attempts: 2, ..fast_opts() };
        let err = fetch_with(&[url], &d.join("f.bin"), &sha256_hex(&b), &opts, &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::Stalled(_)), "{err}");
    }

    #[test]
    fn failing_url_rotates_to_mirror() {
        let b = body();
        let (bad, bad_seen) = serve_script(b.clone(), vec![Beh::Status500]);
        let (good, _) = serve_script(b.clone(), vec![Beh::Full]);
        let d = temp_dir("mirror");
        let dest = d.join("f.bin");
        fetch_with(&[bad, good], &dest, &sha256_hex(&b), &fast_opts(), &mut |_| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b);
        assert_eq!(bad_seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn truncated_body_retries_with_range() {
        let b = body();
        let (url, seen) = serve_script(b.clone(), vec![Beh::Truncate(6_000), Beh::Full]);
        let d = temp_dir("trunc");
        let dest = d.join("f.bin");
        fetch_with(&[url], &dest, &sha256_hex(&b), &fast_opts(), &mut |_| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b);
        assert_eq!(seen.lock().unwrap()[1].as_deref(), Some("6000"));
    }

    #[test]
    fn sha_mismatch_counts_as_attempt_and_next_mirror_wins() {
        let b = body();
        let mut evil = b.clone();
        evil[0] ^= 0xff;
        let (bad, _) = serve_script(evil, vec![Beh::Full]);
        let (good, _) = serve_script(b.clone(), vec![Beh::Full]);
        let d = temp_dir("badmirror");
        let dest = d.join("f.bin");
        fetch_with(&[bad, good], &dest, &sha256_hex(&b), &fast_opts(), &mut |_| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b);
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let b = body();
        let (url, seen) = serve_script(b.clone(), vec![Beh::Status500]);
        let d = temp_dir("giveup");
        let opts = FetchOptions { max_attempts: 3, ..fast_opts() };
        let err = fetch_with(&[url], &d.join("f.bin"), &sha256_hex(&b), &opts, &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::Http(_)), "{err}");
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[test]
    fn backoff_schedule() {
        let o = FetchOptions::default();
        let secs: Vec<u64> = (0..7).map(|i| o.backoff(i).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(o.backoff(200).as_secs(), 30);
    }

    #[test]
    fn rewrite_hf_cases() {
        let u = "https://huggingface.co/ggml-org/x/resolve/main/m.gguf";
        let want = "https://hf-mirror.com/ggml-org/x/resolve/main/m.gguf";
        assert_eq!(rewrite_hf(u, None), u);
        assert_eq!(rewrite_hf(u, Some("https://hf-mirror.com")), want);
        assert_eq!(rewrite_hf(u, Some("https://hf-mirror.com/")), want);
        assert_eq!(rewrite_hf(u, Some("")), u);
        let other = "https://github.com/ggml-org/llama.cpp/releases/x.zip";
        assert_eq!(rewrite_hf(other, Some("https://m")), other);
        let lookalike = "https://huggingface.co.evil/x";
        assert_eq!(rewrite_hf(lookalike, Some("https://m")), lookalike);
    }
}
