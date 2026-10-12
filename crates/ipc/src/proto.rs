//! Message types. Top-level enums are `#[serde(tag = "type", rename_all = "snake_case")]`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    NameAsc,
    NameDesc,
    PathAsc,
    SizeDesc,
    MtimeDesc,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Search { id: u64, query: String, offset: u32, limit: u32, sort: Sort },
    Status { id: u64 },
    /// Files changed recently (from the USN journal).
    Recent { id: u64, limit: u32 },
    /// Stream `Response::Change` for paths under these folders (Tier 1 worker).
    Subscribe { id: u64, roots: Vec<String> },
    Rescan { id: u64, volume: char },
}

impl Request {
    pub fn id(&self) -> u64 {
        match self {
            Request::Search { id, .. }
            | Request::Status { id }
            | Request::Recent { id, .. }
            | Request::Subscribe { id, .. }
            | Request::Rescan { id, .. } => *id,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolumeState {
    Scanning { done: u64, total: u64 },
    Live,
    Rescanning,
    Error { msg: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeStatus {
    pub letter: char,
    pub files: u64,
    pub folders: u64,
    pub state: VolumeState,
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
    SearchResult { id: u64, total: u64, hits: Vec<Hit>, elapsed_us: u64 },
    StatusResult { id: u64, volumes: Vec<VolumeStatus>, uptime_s: u64 },
    RecentResult { id: u64, hits: Vec<Hit> },
    Change { kind: ChangeKind, path: String, old_path: Option<String>, is_dir: bool },
    Error { id: u64, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_shape() {
        let r = Request::Search { id: 1, query: "foo".into(), offset: 0, limit: 10, sort: Sort::MtimeDesc };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""type":"search""#), "{s}");
        assert!(s.contains(r#""sort":"mtime_desc""#), "{s}");
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
    }

    #[test]
    fn status_and_change_roundtrip() {
        let r = Response::StatusResult {
            id: 2,
            volumes: vec![
                VolumeStatus { letter: 'C', files: 1, folders: 2, state: VolumeState::Scanning { done: 1, total: 9 } },
                VolumeStatus { letter: 'D', files: 0, folders: 0, state: VolumeState::Error { msg: "x".into() } },
                VolumeStatus { letter: 'E', files: 0, folders: 0, state: VolumeState::Live },
            ],
            uptime_s: 5,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&s).unwrap(), r);
        let c = Response::Change { kind: ChangeKind::Renamed, path: "a".into(), old_path: Some("b".into()), is_dir: false };
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains(r#""type":"change""#) && s.contains(r#""kind":"renamed""#), "{s}");
        assert_eq!(serde_json::from_str::<Response>(&s).unwrap(), c);
    }

    #[test]
    fn request_ids() {
        assert_eq!(Request::Rescan { id: 7, volume: 'C' }.id(), 7);
    }
}
