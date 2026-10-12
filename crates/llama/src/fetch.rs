//! Resumable HTTP download with SHA-256 verification and atomic rename.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// Download `url` to `dest` (resuming `dest.part` via a Range request), verify
/// SHA-256, then atomically rename. If `dest` already exists with the right
/// hash this is a no-op. A hash mismatch deletes the partial file.
pub fn fetch(
    url: &str,
    dest: &Path,
    expected_sha256: &str,
    progress_cb: &mut dyn FnMut(Progress),
) -> Result<()> {
    if is_unpinned(expected_sha256) {
        return Err(Error::UnpinnedHash(url.to_string()));
    }
    if dest.is_file() && verify(dest, expected_sha256).is_ok() {
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    let part = part_path(dest);
    let mut restarted = false;
    loop {
        match download_into(url, &part, progress_cb)? {
            Outcome::Done => break,
            Outcome::RangeNotSatisfiable => {
                // `.part` is already complete (or stale). Verify it; otherwise start over once.
                if verify(&part, expected_sha256).is_ok() {
                    break;
                }
                let _ = fs::remove_file(&part);
                if restarted {
                    return Err(Error::Http("server rejected range after restart".into()));
                }
                restarted = true;
            }
        }
    }
    if let Err(e) = verify(&part, expected_sha256) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, dest)?; // MoveFileEx(REPLACE_EXISTING) on Windows
    Ok(())
}

enum Outcome {
    Done,
    RangeNotSatisfiable,
}

fn download_into(url: &str, part: &Path, progress_cb: &mut dyn FnMut(Progress)) -> Result<Outcome> {
    let existing = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
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
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        offset += n as u64;
        progress_cb(Progress { downloaded: offset, total });
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
        let (url, _) = serve(b.clone(), 1, true);
        let d = temp_dir("bad");
        let dest = d.join("f.bin");
        let err = fetch(&url, &dest, &sha256_hex(b"other"), &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::Sha256Mismatch { .. }), "{err}");
        assert!(!dest.exists() && !part_path(&dest).exists());
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
}
