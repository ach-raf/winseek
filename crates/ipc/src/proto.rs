//! Message types. Top-level enums are `#[serde(tag = "type", rename_all = "snake_case")]`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    NameAsc,
    NameDesc,
    PathAsc,
    SizeDesc,
    MtimeDesc,
    /// Smallest first (exact reverse of `SizeDesc`).
    SizeAsc,
    /// Oldest first (exact reverse of `MtimeDesc`).
    MtimeAsc,
    /// File type A to Z (folders first, ties by name).
    TypeAsc,
    /// Exact reverse of `TypeAsc`.
    TypeDesc,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Search {
        id: u64,
        query: String,
        offset: u32,
        limit: u32,
        sort: Sort,
        /// Full paths left out of the stream before `offset`/`limit` and `total` apply, so
        /// offsets stay exact over what remains (the picture grid shows its meaning matches
        /// first and pages the name matches without them). Compared case-insensitively, `/` or
        /// `\`. An older daemon ignores the field.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exclude: Vec<String>,
    },
    Status {
        id: u64,
    },
    /// Files changed recently (from the USN journal).
    Recent {
        id: u64,
        limit: u32,
    },
    /// Stream `Response::Change` for paths under these folders (Tier 1 worker).
    Subscribe {
        id: u64,
        roots: Vec<String>,
    },
    Rescan {
        id: u64,
        volume: char,
    },
    /// What each child of a folder holds (WizTree-style). Empty `path` = every indexed volume.
    Usage {
        id: u64,
        path: String,
        limit: u32,
    },
    /// File types and biggest files under a folder (walks the subtree, so asked for separately).
    UsageDetail {
        id: u64,
        path: String,
        top_files: u32,
    },
    /// Totals of `query` per type chip, in the order of `kinds` (index clauses: "", "file:",
    /// "ext:jpg;png", ...). One index pass answers all of them.
    KindCounts {
        id: u64,
        query: String,
        kinds: Vec<String>,
    },
}

impl Request {
    pub fn id(&self) -> u64 {
        match self {
            Request::Search { id, .. }
            | Request::Status { id }
            | Request::Recent { id, .. }
            | Request::Subscribe { id, .. }
            | Request::Rescan { id, .. }
            | Request::Usage { id, .. }
            | Request::UsageDetail { id, .. }
            | Request::KindCounts { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    /// Unix milliseconds.
    pub mtime: Option<i64>,
}

/// One child of a folder in a disk-usage answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// File category (0 other, 1 pictures, 2 video, 3 audio, 4 documents, 5 archives, 6 code, 7 programs and system); a folder: what most of its bytes are.
    #[serde(default)]
    pub kind: u8,
    /// Bytes it holds (a file: its own size).
    pub size: u64,
    pub files: u64,
    pub folders: u64,
    /// Unix milliseconds.
    pub mtime: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageType {
    pub ext: String,
    #[serde(default)]
    pub kind: u8,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageTree {
    pub path: String,
    pub size: u64,
    pub files: u64,
    pub folders: u64,
    /// Biggest first; capped (see `more`).
    pub entries: Vec<UsageEntry>,
    /// Children left out of `entries`, and what they hold together.
    pub more: u64,
    pub more_size: u64,
    /// Bytes per file category under the folder (index = `UsageEntry::kind`).
    #[serde(default)]
    pub cats: Vec<u64>,
    /// False while the roll-up is known to sit behind the index (a change landed but the next
    /// rebuild has not run yet); the UI reads again until it is true.
    #[serde(default)]
    pub fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageDetail {
    pub path: String,
    pub types: Vec<UsageType>,
    pub biggest: Vec<Hit>,
    /// See `UsageTree::fresh`.
    #[serde(default)]
    pub fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolumeState {
    Scanning { done: u64, total: u64 },
    Live,
    Rescanning,
    Error { msg: String },
}

/// How a volume's index is kept live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Monitor {
    /// USN journal (NTFS V2 records, ReFS V3 records).
    #[default]
    Usn,
    /// `ReadDirectoryChangesW` plus rescans on overflow (FAT, exFAT, ReFS without a journal).
    FolderWatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeStatus {
    pub letter: char,
    pub files: u64,
    pub folders: u64,
    pub state: VolumeState,
    /// Filesystem name as Windows reports it ("NTFS", "FAT32", "exFAT", "ReFS"); empty from
    /// an older searchd.
    #[serde(default)]
    pub fs: String,
    #[serde(default)]
    pub monitor: Monitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Deleted,
    Renamed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    SearchResult {
        id: u64,
        total: u64,
        hits: Vec<Hit>,
        elapsed_us: u64,
    },
    StatusResult {
        id: u64,
        volumes: Vec<VolumeStatus>,
        uptime_s: u64,
    },
    RecentResult {
        id: u64,
        hits: Vec<Hit>,
    },
    UsageResult {
        id: u64,
        usage: UsageTree,
    },
    UsageDetailResult {
        id: u64,
        detail: UsageDetail,
    },
    Change {
        kind: ChangeKind,
        path: String,
        old_path: Option<String>,
        is_dir: bool,
    },
    /// Streamed to every subscriber when the index changed WITHOUT per-file events (a volume was
    /// rescanned and swapped in, or a drive was dropped): whatever a client cached about files
    /// may be stale and must be read again.
    Resync,
    /// Totals of the query per type chip, in the request's `kinds` order.
    KindCountsResult {
        id: u64,
        counts: Vec<u64>,
    },
    Error {
        id: u64,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_shape() {
        let r = Request::Search {
            id: 1,
            query: "foo".into(),
            offset: 0,
            limit: 10,
            sort: Sort::MtimeDesc,
            exclude: Vec::new(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""type":"search""#), "{s}");
        assert!(s.contains(r#""sort":"mtime_desc""#), "{s}");
        // An empty exclude list is not sent, so an older daemon sees the old shape.
        assert!(!s.contains("exclude"), "{s}");
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
    }

    #[test]
    fn search_exclude_is_optional_both_ways() {
        // A request from an older app (no field) still parses.
        let old = r#"{"type":"search","id":3,"query":"a","offset":0,"limit":5,"sort":"name_asc"}"#;
        match serde_json::from_str::<Request>(old).unwrap() {
            Request::Search { exclude, .. } => assert!(exclude.is_empty()),
            r => panic!("{r:?}"),
        }
        let r = Request::Search {
            id: 4,
            query: "a".into(),
            offset: 500,
            limit: 500,
            sort: Sort::NameAsc,
            exclude: vec![r"C:\Pics\a.jpg".into()],
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""exclude":["C:\\Pics\\a.jpg"]"#), "{s}");
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
    }

    #[test]
    fn status_and_change_roundtrip() {
        let r = Response::StatusResult {
            id: 2,
            volumes: vec![
                VolumeStatus {
                    letter: 'C',
                    files: 1,
                    folders: 2,
                    state: VolumeState::Scanning { done: 1, total: 9 },
                    fs: "NTFS".into(),
                    monitor: Monitor::Usn,
                },
                VolumeStatus {
                    letter: 'D',
                    files: 0,
                    folders: 0,
                    state: VolumeState::Error { msg: "x".into() },
                    fs: "ReFS".into(),
                    monitor: Monitor::Usn,
                },
                VolumeStatus {
                    letter: 'E',
                    files: 0,
                    folders: 0,
                    state: VolumeState::Live,
                    fs: "exFAT".into(),
                    monitor: Monitor::FolderWatch,
                },
            ],
            uptime_s: 5,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&s).unwrap(), r);
        let c = Response::Change {
            kind: ChangeKind::Renamed,
            path: "a".into(),
            old_path: Some("b".into()),
            is_dir: false,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(
            s.contains(r#""type":"change""#) && s.contains(r#""kind":"renamed""#),
            "{s}"
        );
        assert_eq!(serde_json::from_str::<Response>(&s).unwrap(), c);
    }

    #[test]
    fn volume_status_fs_fields_are_optional_on_the_wire() {
        let old = r#"{"letter":"C","files":1,"folders":2,"state":{"kind":"live"}}"#;
        let v: VolumeStatus = serde_json::from_str(old).unwrap();
        assert_eq!((v.fs.as_str(), v.monitor), ("", Monitor::Usn));
        let s = serde_json::to_string(&VolumeStatus {
            monitor: Monitor::FolderWatch,
            ..v
        })
        .unwrap();
        assert!(s.contains(r#""monitor":"folder_watch""#), "{s}");
    }

    #[test]
    fn request_ids() {
        assert_eq!(Request::Rescan { id: 7, volume: 'C' }.id(), 7);
        assert_eq!(
            Request::Usage {
                id: 8,
                path: String::new(),
                limit: 0
            }
            .id(),
            8
        );
    }

    #[test]
    fn kind_counts_roundtrip() {
        let r = Request::KindCounts {
            id: 9,
            query: "report".into(),
            kinds: vec![String::new(), "file:".into(), "ext:jpg;png".into()],
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""type":"kind_counts""#), "{s}");
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
        let p = Response::KindCountsResult {
            id: 9,
            counts: vec![14, 9, 1],
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains(r#""type":"kind_counts_result""#), "{s}");
        assert_eq!(serde_json::from_str::<Response>(&s).unwrap(), p);
    }
}
