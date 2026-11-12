//! Name data source abstraction. `MockSource` generates fake entries; the real
//! implementation is `pipe_source::PipeSource` (searchd over the named pipe); the mock
//! is the fallback when searchd is not running and reports state "demo".

use serde::Serialize;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Serialize, Clone, Debug)]
pub struct Hit {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    /// unix milliseconds
    pub mtime: Option<i64>,
}

#[derive(Serialize, Debug)]
pub struct SearchResult {
    pub total: u64,
    pub hits: Vec<Hit>,
    pub elapsed_us: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct VolumeStatus {
    pub letter: String,
    pub files: u64,
    pub folders: u64,
    /// "live" | "scanning" | "paused" | "offline" | "demo" (MockSource: searchd not running)
    pub state: String,
    /// Scan progress (only while "scanning").
    pub done: Option<u64>,
    pub total: Option<u64>,
}

/// One volume as the status page shows it (keeps the error text that `VolumeStatus` drops).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct VolumeDetail {
    pub letter: String,
    pub files: u64,
    pub folders: u64,
    /// "live" | "scanning" | "rescanning" | "error" | "demo"
    pub state: String,
    pub done: Option<u64>,
    pub total: Option<u64>,
    pub error: Option<String>,
    /// Filesystem ("NTFS", "FAT32", "exFAT", "ReFS"); empty when unknown (demo, old searchd).
    pub fs: String,
    /// "usn" | "folder_watch" | "" (unknown)
    pub monitor: String,
}

/// Tier 0 as seen by the status page.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Tier0Detail {
    /// "searchd" | "demo" (searchd not running) | "unavailable" (searchd did not answer)
    pub kind: &'static str,
    pub uptime_s: Option<u64>,
    pub volumes: Vec<VolumeDetail>,
}

pub trait NameSource: Send + Sync {
    fn search(&self, query: &str, filter: &str, offset: usize, limit: usize, sort: &str) -> SearchResult;
    fn status(&self) -> Vec<VolumeStatus>;
    fn recent(&self, limit: usize) -> Vec<Hit>;
    /// Detailed Tier 0 state for the status page.
    fn detail(&self) -> Tier0Detail;
    /// Ask searchd to rebuild one volume's index from the MFT.
    fn rescan(&self, volume: char) -> Result<(), String>;
}

struct Entry {
    name: String,
    lower: String,
    dir: u32, // index into dirs
    is_dir: bool,
    size: u64,
    mtime: i64,
    kind: u8, // 0 other 1 image 2 doc 3 video 4 audio 5 folder
}

pub struct MockSource {
    entries: Vec<Entry>,
    dirs: Vec<String>,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a, T>(&mut self, s: &'a [T]) -> &'a T {
        &s[(self.next() % s.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "invoice", "report", "budget", "holiday", "photo", "screenshot", "meeting", "notes", "draft", "final",
    "contract", "resume", "backup", "project", "design", "summer", "winter", "family", "tax", "receipt",
    "presentation", "analysis", "readme", "config", "archive", "setup", "installer", "demo", "logo", "icon",
    "paris", "tokyo", "berlin", "wedding", "birthday", "lecture", "thesis", "model", "dataset", "export",
];
const EXTS: &[(&str, u8)] = &[
    ("jpg", 1), ("png", 1), ("heic", 1), ("gif", 1), ("pdf", 2), ("docx", 2), ("xlsx", 2), ("txt", 2),
    ("md", 2), ("pptx", 2), ("mp4", 3), ("mkv", 3), ("mov", 3), ("mp3", 4), ("flac", 4), ("wav", 4),
    ("zip", 0), ("exe", 0), ("rs", 0), ("py", 0), ("json", 0), ("dll", 0), ("log", 0),
];
const ROOTS: &[&str] = &[
    "C:\\Users\\User\\Documents", "C:\\Users\\User\\Pictures", "C:\\Users\\User\\Downloads",
    "C:\\Users\\User\\Desktop", "D:\\Projects", "D:\\Media\\Videos", "D:\\Media\\Music", "D:\\Archive",
    "C:\\Program Files\\Common Files", "C:\\Windows\\System32",
];
const SUBS: &[&str] = &["2023", "2024", "2025", "Work", "Personal", "Old", "Misc", "Clients", "Scans", "Camera Roll", "src", "assets"];

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl MockSource {
    pub fn generate(n: usize) -> Self {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let mut dirs = Vec::new();
        for r in ROOTS {
            dirs.push((*r).to_string());
            for s in SUBS {
                dirs.push(format!("{r}\\{s}"));
                dirs.push(format!("{r}\\{s}\\{}", rng.pick(WORDS)));
            }
        }
        let now = now_ms();
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let dir = (rng.next() % dirs.len() as u64) as u32;
            if i % 40 == 0 {
                let name = format!("{} {}", rng.pick(WORDS), rng.pick(SUBS));
                let age = (rng.next() % 31_536_000_000) as i64;
                entries.push(Entry { lower: name.to_lowercase(), name, dir, is_dir: true, size: 0, mtime: now - age, kind: 5 });
                continue;
            }
            let (ext, kind) = *rng.pick(EXTS);
            let name = match kind {
                1 => format!("IMG_{:04}.{ext}", rng.next() % 9999),
                _ => format!("{}_{}_{}.{ext}", rng.pick(WORDS), rng.pick(WORDS), rng.next() % 100),
            };
            let size = (rng.next() % 4_000_000) * if kind == 3 { 300 } else { 1 } + 200;
            // skew toward recent: cubic distribution over ~1 year
            let u = (rng.next() % 1_000_000) as f64 / 1_000_000.0;
            let age = (u * u * u * 31_536_000_000.0) as i64;
            entries.push(Entry { lower: name.to_lowercase(), name, dir, is_dir: false, size, mtime: now - age, kind });
        }
        MockSource { entries, dirs }
    }

    fn hit(&self, e: &Entry) -> Hit {
        let d = &self.dirs[e.dir as usize];
        Hit {
            name: e.name.clone(),
            path: format!("{d}\\{}", e.name),
            is_dir: e.is_dir,
            size: if e.is_dir { None } else { Some(e.size) },
            mtime: Some(e.mtime),
        }
    }
}

fn matches_filter(e: &Entry, filter: &str) -> bool {
    match filter {
        "images" => e.kind == 1,
        "docs" => e.kind == 2,
        "video" => e.kind == 3,
        "audio" => e.kind == 4,
        "folders" => e.is_dir,
        "apps" => e.lower.ends_with(".exe") || e.lower.ends_with(".msi"),
        _ => true,
    }
}

impl NameSource for MockSource {
    fn search(&self, query: &str, filter: &str, offset: usize, limit: usize, sort: &str) -> SearchResult {
        let t = Instant::now();
        let all: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
        // Tier 0 `ext:pdf;docx` terms (the UI sends one for "pdf invoice").
        let exts: Vec<String> = all
            .iter()
            .filter_map(|t| t.strip_prefix("ext:"))
            .flat_map(|x| x.split([';', ',']).map(|e| format!(".{e}")).collect::<Vec<_>>())
            .collect();
        let terms: Vec<String> = all.into_iter().filter(|t| !t.starts_with("ext:")).collect();
        let mut idx: Vec<u32> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if matches_filter(e, filter)
                && (exts.is_empty() || exts.iter().any(|x| e.lower.ends_with(x.as_str())))
                && terms.iter().all(|t| e.lower.contains(t.as_str()))
            {
                idx.push(i as u32);
            }
        }
        let total = idx.len() as u64;
        let e = &self.entries;
        // Empty query has no relevance: fall back to newest first.
        let sort = if sort == "rel" && terms.is_empty() { "date" } else { sort };
        match sort {
            "name" => idx.sort_unstable_by(|a, b| e[*a as usize].lower.cmp(&e[*b as usize].lower)),
            "size" => idx.sort_unstable_by(|a, b| e[*b as usize].size.cmp(&e[*a as usize].size)),
            "date" => idx.sort_unstable_by(|a, b| e[*b as usize].mtime.cmp(&e[*a as usize].mtime)),
            "name_desc" => idx.sort_unstable_by(|a, b| e[*b as usize].lower.cmp(&e[*a as usize].lower)),
            "size_asc" => idx.sort_unstable_by(|a, b| e[*a as usize].size.cmp(&e[*b as usize].size)),
            "date_asc" => idx.sort_unstable_by(|a, b| e[*a as usize].mtime.cmp(&e[*b as usize].mtime)),
            _ => {
                // relevance: prefix match first, then shorter names
                let q = terms.first().cloned().unwrap_or_default();
                idx.sort_by_key(|i| {
                    let n = &e[*i as usize];
                    (!n.lower.starts_with(&q), n.lower.len())
                });
            }
        }
        let hits = idx.iter().skip(offset).take(limit).map(|i| self.hit(&e[*i as usize])).collect();
        SearchResult { total, hits, elapsed_us: t.elapsed().as_micros() as u64 }
    }

    fn status(&self) -> Vec<VolumeStatus> {
        let folders = self.entries.iter().filter(|e| e.is_dir).count() as u64;
        vec![
            VolumeStatus { letter: "C".into(), files: 12_580_243, folders: 1_204_511, state: "demo".into(), done: None, total: None },
            VolumeStatus { letter: "D".into(), files: self.entries.len() as u64 - folders, folders, state: "demo".into(), done: None, total: None },
        ]
    }

    fn detail(&self) -> Tier0Detail {
        let volumes = self
            .status()
            .into_iter()
            .map(|v| VolumeDetail {
                letter: v.letter,
                files: v.files,
                folders: v.folders,
                state: v.state,
                done: None,
                total: None,
                error: None,
                fs: String::new(),
                monitor: String::new(),
            })
            .collect();
        Tier0Detail { kind: "demo", uptime_s: None, volumes }
    }

    fn rescan(&self, _volume: char) -> Result<(), String> {
        Err("The name service is not running".into())
    }

    fn recent(&self, limit: usize) -> Vec<Hit> {
        let mut idx: Vec<&Entry> = self.entries.iter().filter(|e| !e.is_dir).collect();
        let n = limit.min(idx.len());
        if n == 0 {
            return vec![];
        }
        idx.select_nth_unstable_by(n - 1, |a, b| b.mtime.cmp(&a.mtime));
        idx.truncate(n);
        idx.sort_by_key(|a| std::cmp::Reverse(a.mtime));
        idx.into_iter().map(|e| self.hit(e)).collect()
    }
}
