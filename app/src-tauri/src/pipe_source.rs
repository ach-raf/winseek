//! `NameSource` backed by `searchd` over the named pipe. One connection behind a mutex;
//! on any I/O error the connection is dropped and re-established (one retry per call).

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use winseek_ipc::pipe::{connect, PipeConn};
use winseek_ipc::{read_message, write_message, Request, Response, Sort, VolumeState, PIPE_NAME};

use crate::source::{Hit, NameSource, SearchResult, Tier0Detail, VolumeDetail, VolumeStatus};

const RECONNECT_TIMEOUT: Duration = Duration::from_millis(300);

pub struct PipeSource {
    conn: Mutex<Option<PipeConn>>,
    next_id: AtomicU64,
}

impl PipeSource {
    /// Connect once (used at startup to decide between searchd and demo data).
    pub fn connect(timeout: Duration) -> io::Result<Self> {
        let c = connect(PIPE_NAME, timeout)?;
        Ok(PipeSource { conn: Mutex::new(Some(c)), next_id: AtomicU64::new(1) })
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send one request and read its response; reconnect and retry once on failure.
    fn call(&self, req: &Request) -> io::Result<Response> {
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut last = None;
        for _ in 0..2 {
            if guard.is_none() {
                *guard = Some(connect(PIPE_NAME, RECONNECT_TIMEOUT)?);
            }
            let c = guard.as_mut().unwrap();
            let r = write_message(c, req).and_then(|_| read_message::<_, Response>(c));
            match r {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    *guard = None;
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap())
    }
}

/// UI filter chip -> query suffix (Tier 0 query syntax).
fn filter_clause(filter: &str) -> &'static str {
    match filter {
        "images" => "ext:jpg;jpeg;png;gif;heic;webp;bmp;svg;tif;tiff",
        "docs" => "ext:pdf;doc;docx;xls;xlsx;ppt;pptx;txt;md;rtf;odt;csv",
        "video" => "ext:mp4;mkv;mov;avi;webm;wmv;m4v",
        "audio" => "ext:mp3;flac;wav;m4a;ogg;aac;wma",
        "folders" => "folder:",
        _ => "",
    }
}

fn sort_of(sort: &str) -> Sort {
    match sort {
        "size" => Sort::SizeDesc,
        "date" => Sort::MtimeDesc,
        _ => Sort::NameAsc, // "name" and "rel" (no relevance ranking in Tier 0)
    }
}

fn hit(h: winseek_ipc::Hit) -> Hit {
    Hit { name: h.name, path: h.path, is_dir: h.is_dir, size: h.size, mtime: h.mtime }
}

fn volume_detail(v: winseek_ipc::VolumeStatus) -> VolumeDetail {
    let (state, done, total, error) = match v.state {
        VolumeState::Live => ("live", None, None, None),
        VolumeState::Scanning { done, total } => ("scanning", Some(done), Some(total), None),
        VolumeState::Rescanning => ("rescanning", None, None, None),
        VolumeState::Error { msg } => ("error", None, None, Some(msg)),
    };
    VolumeDetail { letter: v.letter.to_string(), files: v.files, folders: v.folders, state: state.into(), done, total, error }
}

fn offline() -> Vec<VolumeStatus> {
    vec![VolumeStatus { letter: "-".into(), files: 0, folders: 0, state: "offline".into(), done: None, total: None }]
}

impl NameSource for PipeSource {
    fn search(&self, query: &str, filter: &str, offset: usize, limit: usize, sort: &str) -> SearchResult {
        let q = format!("{query} {}", filter_clause(filter)).trim().to_string();
        let req = Request::Search {
            id: self.id(),
            query: q,
            offset: offset.min(u32::MAX as usize) as u32,
            limit: limit.min(u32::MAX as usize) as u32,
            sort: sort_of(sort),
        };
        match self.call(&req) {
            Ok(Response::SearchResult { total, hits, elapsed_us, .. }) => {
                SearchResult { total, hits: hits.into_iter().map(hit).collect(), elapsed_us }
            }
            _ => SearchResult { total: 0, hits: vec![], elapsed_us: 0 },
        }
    }

    fn status(&self) -> Vec<VolumeStatus> {
        match self.call(&Request::Status { id: self.id() }) {
            Ok(Response::StatusResult { volumes, .. }) => volumes
                .into_iter()
                .map(|v| VolumeStatus {
                    letter: v.letter.to_string(),
                    files: v.files,
                    folders: v.folders,
                    done: match v.state {
                        VolumeState::Scanning { done, .. } => Some(done),
                        _ => None,
                    },
                    total: match v.state {
                        VolumeState::Scanning { total, .. } => Some(total),
                        _ => None,
                    },
                    state: match v.state {
                        VolumeState::Live => "live",
                        VolumeState::Scanning { .. } | VolumeState::Rescanning => "scanning",
                        VolumeState::Error { .. } => "offline",
                    }
                    .into(),
                })
                .collect(),
            _ => offline(),
        }
    }

    fn detail(&self) -> Tier0Detail {
        match self.call(&Request::Status { id: self.id() }) {
            Ok(Response::StatusResult { volumes, uptime_s, .. }) => {
                Tier0Detail { kind: "searchd", uptime_s: Some(uptime_s), volumes: volumes.into_iter().map(volume_detail).collect() }
            }
            _ => Tier0Detail { kind: "unavailable", uptime_s: None, volumes: vec![] },
        }
    }

    /// The pipe's DACL gives the interactive user read/write and searchd does not check elevation,
    /// so a normal (non-admin) app may ask for a rescan.
    fn rescan(&self, volume: char) -> Result<(), String> {
        match self.call(&Request::Rescan { id: self.id(), volume }) {
            Ok(Response::Error { message, .. }) => Err(message),
            Ok(_) => Ok(()),
            Err(e) => Err(format!("The name service did not answer: {e}")),
        }
    }

    fn recent(&self, limit: usize) -> Vec<Hit> {
        match self.call(&Request::Recent { id: self.id(), limit: limit.min(u32::MAX as usize) as u32 }) {
            Ok(Response::RecentResult { hits, .. }) => hits.into_iter().map(hit).collect(),
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_detail_keeps_state_progress_and_error_text() {
        let v = |state| winseek_ipc::VolumeStatus { letter: 'C', files: 7, folders: 2, state };
        let d = volume_detail(v(VolumeState::Scanning { done: 3, total: 9 }));
        assert_eq!((d.state.as_str(), d.done, d.total, d.letter.as_str()), ("scanning", Some(3), Some(9), "C"));
        let d = volume_detail(v(VolumeState::Error { msg: "journal gone".into() }));
        assert_eq!((d.state.as_str(), d.error.as_deref()), ("error", Some("journal gone")));
        assert_eq!(volume_detail(v(VolumeState::Rescanning)).state, "rescanning");
        let d = volume_detail(v(VolumeState::Live));
        assert_eq!((d.state.as_str(), d.files, d.folders, d.error), ("live", 7, 2, None));
    }
}
