//! Crash-safe small-file writes: temp file in the target's directory, flushed, then renamed
//! over the target. A crash mid-save leaves the old file intact (and at most a `.tmp`
//! leftover) instead of a truncated config that loads back as defaults — which for
//! `agent_config.json` silently meant agent access OFF.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;

/// Write `bytes` to `path` atomically: write to a sibling temp file, sync it to disk, then
/// rename over `path` — a rename is the one step a concurrent reader never sees half of.
/// Creates `path`'s parent directory when missing. Every config-style save goes through
/// this one function (single mutation path for the atomic-write pattern).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent dir"))?;
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_else(|| OsStr::new("file"));
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let done = (|| -> io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?; // the bytes are on disk before the name flips
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp); // do not litter on a failed save
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier1::test_tmp;

    #[test]
    fn writes_and_overwrites_without_leftovers() {
        let tmp = test_tmp();
        let p = tmp.path().join("sub").join("cfg.json");
        write_atomic(&p, b"one").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("sub")).unwrap().collect();
        assert_eq!(entries.len(), 1, "only the target file should remain");
    }
}
