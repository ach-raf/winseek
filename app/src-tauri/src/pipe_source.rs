//! `NameSource` backed by `searchd` over the named pipe. A small pool of connections: each call
//! takes an idle one (or opens one), so a slow request (a Space walk) never holds up a search.
//! searchd serves every connection on its own thread. On an I/O error the connection is dropped,
//! the idle pool is cleared (a restarted searchd kills them all) and the call retries once.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use winseek_ipc::pipe::{connect, PipeConn};
use winseek_ipc::{
    read_message, write_message, Monitor, Request, Response, Sort, VolumeState, PIPE_NAME,
};

use crate::source::{Hit, NameSource, SearchResult, Tier0Detail, VolumeDetail, VolumeStatus};

const RECONNECT_TIMEOUT: Duration = Duration::from_millis(300);
/// Idle connections kept for reuse. Concurrency is not capped here: it is bounded by the callers
/// (one per in-flight Tauri command).
const MAX_IDLE: usize = 4;
/// A call slower than this is logged (wall time vs searchd's own time tells queueing from work).
const SLOW_CALL: Duration = Duration::from_millis(150);

pub struct PipeSource {
    name: String,
    idle: Mutex<Vec<PipeConn>>,
    next_id: AtomicU64,
}

impl PipeSource {
    /// Connect once (used at startup to decide between searchd and demo data).
    pub fn connect(timeout: Duration) -> io::Result<Self> {
        Self::connect_to(PIPE_NAME, timeout)
    }

    /// Same as `connect`, to any pipe name (tests use a private one).
    pub fn connect_to(name: &str, timeout: Duration) -> io::Result<Self> {
        let c = connect(name, timeout)?;
        Ok(PipeSource {
            name: name.to_string(),
            idle: Mutex::new(vec![c]),
            next_id: AtomicU64::new(1),
        })
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn idle(&self) -> std::sync::MutexGuard<'_, Vec<PipeConn>> {
        self.idle.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Send one request on its own connection and read its response; on failure, retry once on a
    /// fresh connection. The pool lock is held only to pop/push, never across I/O.
    fn call(&self, req: &Request) -> io::Result<Response> {
        let t = Instant::now();
        let mut last = None;
        for attempt in 0..2 {
            let pooled = if attempt == 0 {
                self.idle().pop()
            } else {
                None
            };
            let mut c = match pooled {
                Some(c) => c,
                None => connect(&self.name, RECONNECT_TIMEOUT)?,
            };
            match write_message(&mut c, req).and_then(|_| read_message::<_, Response>(&mut c)) {
                Ok(resp) => {
                    {
                        let mut idle = self.idle();
                        if idle.len() < MAX_IDLE {
                            idle.push(c);
                        }
                    }
                    if t.elapsed() > SLOW_CALL {
                        eprintln!(
                            "winseek: slow searchd call {} took {:.0?}",
                            kind_of(req),
                            t.elapsed()
                        );
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    self.idle().clear();
                    if attempt == 0 {
                        eprintln!(
                            "winseek: searchd connection failed ({e}); retrying on a fresh one"
                        );
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap())
    }
}

/// The request's name for the slow-call log line.
fn kind_of(req: &Request) -> &'static str {
    match req {
        Request::Search { .. } => "search",
        Request::Status { .. } => "status",
        Request::Recent { .. } => "recent",
        Request::Rescan { .. } => "rescan",
        Request::Usage { .. } => "usage",
        Request::UsageDetail { .. } => "usage_detail",
        Request::KindCounts { .. } => "kind_counts",
        Request::Subscribe { .. } => "subscribe",
    }
}

/// UI filter chip -> query suffix (Tier 0 query syntax).
fn filter_clause(filter: &str) -> &'static str {
    match filter {
        "files" => "file:",
        "images" => "ext:jpg;jpeg;png;gif;heic;webp;bmp;svg;tif;tiff",
        "docs" => "ext:pdf;doc;docx;xls;xlsx;ppt;pptx;txt;md;rtf;odt;csv",
        "video" => "ext:mp4;mkv;mov;avi;webm;wmv;m4v",
        "audio" => "ext:mp3;flac;wav;m4a;ogg;aac;wma",
        "apps" => "ext:exe;msi;msix;appx",
        "folders" => "folder:",
        _ => "",
    }
}

fn sort_of(sort: &str) -> Sort {
    match sort {
        "size" => Sort::SizeDesc,
        "size_asc" => Sort::SizeAsc,
        "date_asc" => Sort::MtimeAsc,
        "name_desc" => Sort::NameDesc,
        "date" => Sort::MtimeDesc,
        "type" => Sort::TypeAsc,
        "type_desc" => Sort::TypeDesc,
        _ => Sort::NameAsc, // "name" and "rel" (no relevance ranking in Tier 0)
    }
}

fn hit(h: winseek_ipc::Hit) -> Hit {
    Hit {
        name: h.name,
        path: h.path,
        is_dir: h.is_dir,
        size: h.size,
        mtime: h.mtime,
    }
}

fn volume_detail(v: winseek_ipc::VolumeStatus) -> VolumeDetail {
    let (state, done, total, error) = match v.state {
        VolumeState::Live => ("live", None, None, None),
        VolumeState::Scanning { done, total } => ("scanning", Some(done), Some(total), None),
        VolumeState::Rescanning => ("rescanning", None, None, None),
        VolumeState::Error { msg } => ("error", None, None, Some(msg)),
    };
    // An older searchd sends no fs: show nothing rather than a guessed monitor.
    let monitor = match (v.fs.is_empty(), v.monitor) {
        (true, _) => "",
        (false, Monitor::Usn) => "usn",
        (false, Monitor::FolderWatch) => "folder_watch",
    };
    VolumeDetail {
        letter: v.letter.to_string(),
        files: v.files,
        folders: v.folders,
        state: state.into(),
        done,
        total,
        error,
        fs: v.fs,
        monitor: monitor.into(),
    }
}

fn offline() -> Vec<VolumeStatus> {
    vec![VolumeStatus {
        letter: "-".into(),
        files: 0,
        folders: 0,
        state: "offline".into(),
        done: None,
        total: None,
    }]
}

impl NameSource for PipeSource {
    fn search_excluding(
        &self,
        query: &str,
        filter: &str,
        offset: usize,
        limit: usize,
        sort: &str,
        exclude: &[String],
    ) -> SearchResult {
        let q = format!("{query} {}", filter_clause(filter))
            .trim()
            .to_string();
        let req = Request::Search {
            id: self.id(),
            query: q,
            offset: offset.min(u32::MAX as usize) as u32,
            limit: limit.min(u32::MAX as usize) as u32,
            sort: sort_of(sort),
            exclude: exclude.to_vec(),
        };
        match self.call(&req) {
            Ok(Response::SearchResult {
                total,
                hits,
                elapsed_us,
                ..
            }) => SearchResult {
                total,
                hits: hits.into_iter().map(hit).collect(),
                elapsed_us,
            },
            _ => SearchResult {
                total: 0,
                hits: vec![],
                elapsed_us: 0,
            },
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
            Ok(Response::StatusResult {
                volumes, uptime_s, ..
            }) => Tier0Detail {
                kind: "searchd",
                uptime_s: Some(uptime_s),
                volumes: volumes.into_iter().map(volume_detail).collect(),
            },
            _ => Tier0Detail {
                kind: "unavailable",
                uptime_s: None,
                volumes: vec![],
            },
        }
    }

    /// The pipe's DACL gives the interactive user read/write and searchd does not check elevation,
    /// so a normal (non-admin) app may ask for a rescan.
    fn rescan(&self, volume: char) -> Result<(), String> {
        match self.call(&Request::Rescan {
            id: self.id(),
            volume,
        }) {
            Ok(Response::Error { message, .. }) => Err(message),
            Ok(_) => Ok(()),
            Err(e) => Err(format!("The name service did not answer: {e}")),
        }
    }

    fn usage_detail(&self, path: &str, top_files: u32) -> Result<winseek_ipc::UsageDetail, String> {
        match self.call(&Request::UsageDetail {
            id: self.id(),
            path: path.into(),
            top_files,
        }) {
            Ok(Response::UsageDetailResult { detail, .. }) => Ok(detail),
            Ok(Response::Error { message, .. }) => Err(message),
            Ok(_) => Err("Unexpected answer from the name service.".into()),
            Err(e) => Err(format!("The name service did not answer: {e}")),
        }
    }

    fn usage(&self, path: &str, limit: u32) -> Result<winseek_ipc::UsageTree, String> {
        match self.call(&Request::Usage {
            id: self.id(),
            path: path.into(),
            limit,
        }) {
            Ok(Response::UsageResult { usage, .. }) => Ok(usage),
            Ok(Response::Error { message, .. }) => Err(message),
            Ok(_) => Err("Unexpected answer from the name service.".into()),
            Err(e) => Err(format!("The name service did not answer: {e}")),
        }
    }

    fn kind_counts(&self, query: &str, kinds: &[String]) -> Vec<u64> {
        let req = Request::KindCounts {
            id: self.id(),
            query: query.into(),
            kinds: kinds.iter().map(|k| filter_clause(k).to_string()).collect(),
        };
        match self.call(&req) {
            Ok(Response::KindCountsResult { counts, .. }) => counts,
            // An older searchd answers Error ("bad request"); so does a broken pipe. One count
            // at a time keeps the chips working.
            _ => kinds
                .iter()
                .map(|k| self.search(query, k, 0, 0, "rel").total)
                .collect(),
        }
    }

    fn recent(&self, limit: usize) -> Vec<Hit> {
        match self.call(&Request::Recent {
            id: self.id(),
            limit: limit.min(u32::MAX as usize) as u32,
        }) {
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
        let v = |state| winseek_ipc::VolumeStatus {
            letter: 'C',
            files: 7,
            folders: 2,
            state,
            fs: "NTFS".into(),
            monitor: Monitor::Usn,
        };
        let d = volume_detail(v(VolumeState::Scanning { done: 3, total: 9 }));
        assert_eq!(
            (d.state.as_str(), d.done, d.total, d.letter.as_str()),
            ("scanning", Some(3), Some(9), "C")
        );
        let d = volume_detail(v(VolumeState::Error {
            msg: "journal gone".into(),
        }));
        assert_eq!(
            (d.state.as_str(), d.error.as_deref()),
            ("error", Some("journal gone"))
        );
        assert_eq!(
            volume_detail(v(VolumeState::Rescanning)).state,
            "rescanning"
        );
        let d = volume_detail(v(VolumeState::Live));
        assert_eq!(
            (d.state.as_str(), d.files, d.folders, d.error),
            ("live", 7, 2, None)
        );
        assert_eq!((d.fs.as_str(), d.monitor.as_str()), ("NTFS", "usn"));
        let e = volume_detail(winseek_ipc::VolumeStatus {
            letter: 'E',
            files: 1,
            folders: 0,
            state: VolumeState::Live,
            fs: "exFAT".into(),
            monitor: Monitor::FolderWatch,
        });
        assert_eq!(
            (e.fs.as_str(), e.monitor.as_str()),
            ("exFAT", "folder_watch")
        );
        let old = volume_detail(winseek_ipc::VolumeStatus {
            fs: String::new(),
            ..winseek_ipc::VolumeStatus {
                letter: 'D',
                files: 0,
                folders: 0,
                state: VolumeState::Live,
                fs: String::new(),
                monitor: Monitor::Usn,
            }
        });
        assert_eq!((old.fs.as_str(), old.monitor.as_str()), ("", ""));
    }

    #[test]
    fn filter_clauses_map_to_index_terms() {
        // The Files chip is the bare file: filter (everything but folders); folders is its inverse.
        assert_eq!(filter_clause("files"), "file:");
        assert_eq!(filter_clause("folders"), "folder:");
        assert_eq!(filter_clause("all"), "");
        assert!(filter_clause("images").starts_with("ext:"));
    }

    /// A thread-per-connection accept loop shaped like searchd's `spawn_accept_loop`, answering
    /// `Status` at once and `Usage` after a slow walk (then an error, which `usage` maps to `Err`).
    fn spawn_fake_searchd(name: &str) {
        use winseek_ipc::pipe::PipeServer;
        let mut server = PipeServer::bind(name).unwrap();
        std::thread::spawn(move || loop {
            match server.accept() {
                Ok(mut conn) => {
                    std::thread::spawn(move || loop {
                        let req: Request = match read_message(&mut conn) {
                            Ok(r) => r,
                            Err(_) => return,
                        };
                        let resp = match req {
                            Request::Usage { id, .. } => {
                                std::thread::sleep(Duration::from_millis(600));
                                Response::Error {
                                    id,
                                    message: "slow walk".into(),
                                }
                            }
                            Request::Status { id } => Response::StatusResult {
                                id,
                                volumes: vec![],
                                uptime_s: 1,
                            },
                            _ => return,
                        };
                        if write_message(&mut conn, &resp).is_err() {
                            return;
                        }
                    });
                }
                Err(_) => return,
            }
        });
    }

    #[test]
    fn a_slow_call_does_not_hold_up_a_fast_one() {
        let name = format!(r"\\.\pipe\winseek-test-pool-{}", std::process::id());
        spawn_fake_searchd(&name);
        let src =
            std::sync::Arc::new(PipeSource::connect_to(&name, Duration::from_secs(1)).unwrap());
        let slow = {
            let src = src.clone();
            std::thread::spawn(move || src.usage("C:", 10))
        };
        std::thread::sleep(Duration::from_millis(50));
        let t = std::time::Instant::now();
        let volumes = src.status();
        assert!(
            t.elapsed() < Duration::from_millis(200),
            "status waited {:.0?} behind the slow usage call",
            t.elapsed()
        );
        assert!(volumes.is_empty(), "status answered {:?}", volumes);
        slow.join().unwrap().unwrap_err(); // the walk finished (with the fake error) and did not panic
    }

    #[test]
    fn concurrent_calls_each_get_their_own_connection() {
        let name = format!(r"\\.\pipe\winseek-test-pool4-{}", std::process::id());
        spawn_fake_searchd(&name);
        let src =
            std::sync::Arc::new(PipeSource::connect_to(&name, Duration::from_secs(1)).unwrap());
        let mut threads = Vec::new();
        for _ in 0..4 {
            let src = src.clone();
            threads.push(std::thread::spawn(move || {
                for _ in 0..20 {
                    assert!(src.status().is_empty());
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
    }
}
