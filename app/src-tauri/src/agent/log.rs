//! Agent activity log: `%LOCALAPPDATA%\winseek\agent_log.jsonl`, one JSON line per call
//! (PRD RA6). Argument strings are cut so the file stays scannable; file *contents* never
//! land here, only paths, filters and counts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Long strings (query text, filters) are cut here; full paths stay (they are what the user
/// needs to see to know what an agent touched — read-only).
pub const MAX_ARG_CHARS: usize = 200;
/// The file is trimmed to its last N lines on app start.
pub const MAX_LINES: usize = 5000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub ts_ms: i64,
    pub client: String,
    pub tool: String,
    /// The call's arguments, strings over [`MAX_ARG_CHARS`] cut with "…".
    pub args: Value,
    pub ok: bool,
    /// Hit / result count when the tool returns a list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ms: u64,
}

pub fn path(root: &Path) -> PathBuf {
    root.join("agent_log.jsonl")
}

/// Cut every string in `v` (recursively) at [`MAX_ARG_CHARS`].
pub fn truncate_args(v: &Value) -> Value {
    const ELLIPSIS: &str = "…";
    match v {
        Value::String(s) if s.chars().count() > MAX_ARG_CHARS => {
            Value::String(s.chars().take(MAX_ARG_CHARS).collect::<String>() + ELLIPSIS)
        }
        Value::Array(a) => Value::Array(a.iter().map(truncate_args).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), truncate_args(v))).collect()),
        other => other.clone(),
    }
}

pub struct Log {
    path: PathBuf,
    /// One writer at a time; lines must not interleave.
    lock: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Log {
    pub fn new(root: &Path) -> Self {
        Log { path: path(root), lock: Mutex::new(()) }
    }

    /// Append one entry. A failed write must not fail the tool call it describes; the error is
    /// logged to stderr instead.
    pub fn append(&self, e: &Entry) {
        let line = match serde_json::to_string(e) {
            Ok(l) => l,
            Err(err) => {
                eprintln!("winseek: agent log: cannot serialize entry: {err}");
                return;
            }
        };
        let _g = lock(&self.lock);
        let r = std::fs::create_dir_all(self.path.parent().unwrap_or(Path::new(".")))
            .and_then(|_| OpenOptions::new().create(true).append(true).open(&self.path))
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(err) = r {
            eprintln!("winseek: agent log: append failed: {err}");
        }
    }

    /// The last `n` entries, newest last.
    pub fn tail(&self, n: usize) -> Vec<Entry> {
        let _g = lock(&self.lock);
        read_all(&self.path).into_iter().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect()
    }

    pub fn clear(&self) -> Result<(), String> {
        let _g = lock(&self.lock);
        std::fs::write(&self.path, b"").map_err(|e| e.to_string())
    }
}

fn read_all(path: &Path) -> Vec<Entry> {
    let Ok(f) = std::fs::File::open(path) else { return vec![] };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

/// Keep only the last `max` lines. Called on app start so the file cannot grow forever.
pub fn trim(path: &Path, max: usize) -> Result<(), String> {
    let entries = read_all(path);
    if entries.len() <= max {
        return Ok(());
    }
    let kept = &entries[entries.len() - max..];
    let mut out = String::new();
    for e in kept {
        out.push_str(&serde_json::to_string(e).map_err(|e| e.to_string())?);
        out.push('\n');
    }
    std::fs::write(path, out).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(tool: &str, args: Value) -> Entry {
        Entry { ts_ms: 1, client: "tester".into(), tool: tool.into(), args, ok: true, count: None, error: None, ms: 1 }
    }

    #[test]
    fn long_strings_are_cut_paths_shape_kept() {
        let v = json!({"query": "x".repeat(500), "path": r"C:\short.txt", "deep": ["y".repeat(300)], "n": 5});
        let t = truncate_args(&v);
        let s = t["query"].as_str().unwrap();
        assert_eq!(s.chars().count(), MAX_ARG_CHARS + 1); // + the ellipsis
        assert!(s.ends_with('…'));
        assert_eq!(t["path"], r"C:\short.txt");
        assert_eq!(t["deep"][0].as_str().unwrap().chars().count(), MAX_ARG_CHARS + 1);
        assert_eq!(t["n"], 5);
        // short strings pass through unchanged
        let small = truncate_args(&json!("tiny"));
        assert_eq!(small, json!("tiny"));
    }

    #[test]
    fn append_tail_and_clear() {
        let tmp = crate::tier1::test_tmp();
        let log = Log::new(tmp.path());
        for i in 0..3 {
            log.append(&entry(&format!("tool{i}"), json!({})));
        }
        let tail = log.tail(2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].tool, "tool1", "newest last");
        assert_eq!(tail[1].tool, "tool2");
        assert_eq!(log.tail(10).len(), 3);
        log.clear().unwrap();
        assert!(log.tail(10).is_empty());
    }

    #[test]
    fn trim_keeps_the_last_lines() {
        let tmp = crate::tier1::test_tmp();
        let p = tmp.path().join("trim.jsonl");
        {
            let mut f = std::fs::File::create(&p).unwrap();
            for i in 0..10 {
                writeln!(f, "{}", serde_json::to_string(&entry(&format!("t{i}"), json!({}))).unwrap()).unwrap();
            }
        }
        trim(&p, 3).unwrap();
        let kept = read_all(&p);
        assert_eq!(kept.iter().map(|e| e.tool.as_str()).collect::<Vec<_>>(), ["t7", "t8", "t9"]);
        // A partially written last line is dropped, not fatal.
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{{broken").unwrap();
        assert_eq!(read_all(&p).len(), 3);
    }
}
