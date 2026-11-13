//! Named-pipe request handling. One thread per connection.
//!
//! Pipe handles are synchronous (reads/writes on one handle serialise), so a `Subscribe`
//! connection stops reading requests and only streams `Change` responses until a write
//! fails (client closed) or the daemon shuts down.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winseek_ipc::pipe::{PipeConn, PipeServer};
use winseek_ipc::{read_message, write_message, Request, Response, VolumeState};

use crate::log::{info, warn};
use crate::state::{self, Shared};

/// Accept loop on its own thread. The pipe must already be bound (so a second searchd fails fast).
pub fn spawn_accept_loop(sh: Arc<Shared>, mut server: PipeServer) {
    std::thread::spawn(move || loop {
        match server.accept() {
            Ok(conn) => {
                let sh = sh.clone();
                std::thread::spawn(move || handle(conn, &sh));
            }
            Err(e) => {
                warn!("pipe accept failed: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    });
}

fn handle(mut conn: PipeConn, sh: &Shared) {
    loop {
        let req: Request = match read_message(&mut conn) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let msg = Response::Error { id: 0, message: format!("bad request: {e}") };
                if write_message(&mut conn, &msg).is_err() {
                    return;
                }
                continue;
            }
            Err(_) => return, // EOF or broken pipe
        };
        let resp = match req {
            Request::Subscribe { roots, .. } => {
                stream_changes(&mut conn, sh, &roots);
                return;
            }
            other => respond(sh, other),
        };
        if write_message(&mut conn, &resp).is_err() {
            return;
        }
    }
}

fn respond(sh: &Shared, req: Request) -> Response {
    match req {
        Request::Search { id, query, offset, limit, sort } => {
            let t = Instant::now();
            let (total, hits, note) = state::search(sh, &query, offset, limit, sort);
            if let Some(n) = note {
                static ONCE: AtomicBool = AtomicBool::new(false);
                if !ONCE.swap(true, Ordering::Relaxed) {
                    warn!("search sort {sort:?}: {n}");
                }
            }
            Response::SearchResult { id, total, hits, elapsed_us: t.elapsed().as_micros() as u64 }
        }
        Request::Status { id } => status(sh, id),
        Request::Recent { id, limit } => Response::RecentResult { id, hits: state::recent(sh, limit) },
        Request::Rescan { id, volume } => {
            let v = volume.to_ascii_uppercase();
            match sh.has_ctl(v).then(|| sh.ctl(v)) {
                Some(c) => {
                    info!("{v}: rescan requested over IPC");
                    sh.set_state(v, VolumeState::Rescanning);
                    c.rescan.store(true, Ordering::Relaxed);
                    c.stop.store(true, Ordering::Relaxed);
                    status(sh, id)
                }
                None => Response::Error { id, message: format!("volume {v}: is not indexed") },
            }
        }
        Request::Usage { id, path, limit } => match state::usage(sh, &path, limit) {
            Ok(usage) => Response::UsageResult { id, usage },
            Err(message) => Response::Error { id, message },
        },
        Request::UsageDetail { id, path, top_files } => match state::usage_detail(sh, &path, top_files) {
            Ok(detail) => Response::UsageDetailResult { id, detail },
            Err(message) => Response::Error { id, message },
        },
        Request::Subscribe { id, .. } => Response::Error { id, message: "subscribe handled elsewhere".into() },
    }
}

fn status(sh: &Shared, id: u64) -> Response {
    Response::StatusResult { id, volumes: state::status(sh), uptime_s: sh.started.elapsed().as_secs() }
}

fn stream_changes(conn: &mut PipeConn, sh: &Shared, roots: &[String]) {
    let rx = sh.subscribe(roots);
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(msg) => {
                if write_message(conn, &msg).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if sh.shutdown.load(Ordering::Relaxed) {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return, // dropped by the writer (overflow)
        }
    }
}
