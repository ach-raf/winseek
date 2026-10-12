//! searchd: Tier 0 daemon. MFT scan -> in-memory name index, USN journal follow, snapshot,
//! named-pipe search server. P3 scope: `--console` only (Windows service comes in P4).
//!
//! Threads: main (signals, periodic snapshot) | writer (sole index mutator) |
//! one worker per volume (scan / USN monitor) | pipe accept loop + one per connection.

mod index_ops;
mod log;
mod server;
mod state;
mod worker;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use winseek_index::Index;
use winseek_ipc::pipe::PipeServer;
use winseek_ipc::PIPE_NAME;
use winseek_ntfs::{list_ntfs_volumes, open_volume_for_usn, VolumeInfo};

use log::{error, info, warn};
use state::{Msg, Shared};

const USAGE: &str = "usage: searchd --console [--data <dir>] [--volumes C,D] [--log <file>]";
const SAVE_EVERY: Duration = Duration::from_secs(300);

struct Args {
    data: PathBuf,
    volumes: Option<Vec<char>>,
    log: PathBuf,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut console, mut data, mut volumes, mut log) = (false, None, None, None);
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--console" => console = true,
            "--data" => data = Some(PathBuf::from(val("--data")?)),
            "--log" => log = Some(PathBuf::from(val("--log")?)),
            "--volumes" => {
                let v = val("--volumes")?;
                let mut l = Vec::new();
                for p in v.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    let c = p.trim_end_matches([':', '\\']).chars().next().filter(|c| c.is_ascii_alphabetic());
                    l.push(c.ok_or_else(|| format!("bad volume '{p}'"))?.to_ascii_uppercase());
                }
                volumes = Some(l);
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    if !console {
        return Err("--console is required (service mode is not implemented yet)".into());
    }
    let data = data.unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into())).join("winseek")
    });
    let log = log.unwrap_or_else(|| data.join("searchd.log"));
    Ok(Args { data, volumes, log })
}

static CTRL_C: AtomicBool = AtomicBool::new(false);

extern "system" fn on_ctrl(_event: u32) -> i32 {
    CTRL_C.store(true, Ordering::SeqCst);
    1 // handled: let main shut down and save
}

#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("searchd: {e}\n{USAGE}");
            return 2;
        }
    };
    log::init(&args.log);
    info!("searchd starting (data dir {}, log {})", args.data.display(), args.log.display());
    let _ = std::fs::create_dir_all(&args.data);

    // Volumes to index.
    let all = list_ntfs_volumes();
    let vols: Vec<VolumeInfo> = match &args.volumes {
        None => all,
        Some(want) => {
            for w in want.iter().filter(|w| !all.iter().any(|v| v.letter == **w)) {
                warn!("{w}: is not an NTFS volume; skipped");
            }
            all.into_iter().filter(|v| want.contains(&v.letter)).collect()
        }
    };
    if vols.is_empty() {
        error!("no NTFS volumes to index");
        return 1;
    }

    // Fail fast without administrator rights (raw volume access needs them).
    if let Err(e) = open_volume_for_usn(vols[0].letter) {
        error!("searchd needs administrator to read $MFT; run elevated ({e})");
        return 1;
    }

    let pipe = match PipeServer::bind(PIPE_NAME) {
        Ok(p) => p,
        Err(e) => {
            error!("cannot create pipe {PIPE_NAME}: {e} (is another searchd running?)");
            return 1;
        }
    };

    let letters: Vec<char> = vols.iter().map(|v| v.letter).collect();
    let snap_path = args.data.join("index.wsix");
    let index = load_snapshot(&snap_path, &letters);
    let cursors: Vec<_> = vols
        .iter()
        .map(|v| {
            index
                .volumes()
                .iter()
                .find(|m| m.letter == v.letter as u8)
                .and_then(|m| worker::stored_cursor(v, m))
        })
        .collect();

    let sh = Arc::new(Shared::new(index, &letters));
    let (tx, rx) = mpsc::channel::<Msg>();
    let writer = {
        let sh = sh.clone();
        std::thread::spawn(move || state::writer_loop(&sh, rx))
    };
    let mut workers = Vec::new();
    for (v, cur) in vols.into_iter().zip(cursors) {
        match cur {
            Some(c) => info!("{}: snapshot valid, catching up from USN {}", v.letter, c.next_usn),
            None => info!("{}: no usable snapshot; full scan", v.letter),
        }
        if cur.is_some() {
            sh.set_state(v.letter, winseek_ipc::VolumeState::Live);
        }
        let (sh, tx) = (sh.clone(), tx.clone());
        workers.push(std::thread::spawn(move || worker::run(sh, tx, v, cur)));
    }
    server::spawn_accept_loop(sh.clone(), pipe);
    // SAFETY: registering a plain `extern "system"` handler.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    info!("ready; serving {PIPE_NAME} (Ctrl+C to stop)");

    let mut last_save = Instant::now();
    while !CTRL_C.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        if sh.save_now.swap(false, Ordering::Relaxed) || last_save.elapsed() >= SAVE_EVERY {
            save_snapshot(&sh, &snap_path);
            last_save = Instant::now();
        }
    }

    info!("shutting down");
    sh.shutdown.store(true, Ordering::SeqCst);
    for c in sh.ctl.values() {
        c.stop.store(true, Ordering::SeqCst);
    }
    // Monitors flush their last batch within ~100 ms; a worker stuck in a scan is abandoned.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !workers.iter().all(|w| w.is_finished()) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = tx.send(Msg::Shutdown);
    let _ = writer.join();
    save_snapshot(&sh, &snap_path);
    info!("bye");
    0
}

/// Load the snapshot if it is usable for exactly these volumes; otherwise start empty.
fn load_snapshot(path: &std::path::Path, letters: &[char]) -> Index {
    if path.exists() {
        match Index::load(path) {
            Ok(ix) if !ix.has_meta() => warn!("snapshot has no size/mtime columns; ignoring"),
            Ok(ix) if ix.volumes().iter().any(|v| !letters.contains(&(v.letter as char))) => {
                warn!("snapshot contains volumes that are not selected; ignoring it")
            }
            Ok(ix) => {
                info!("loaded snapshot: {} records", ix.live_count());
                return ix;
            }
            Err(e) => warn!("cannot load snapshot {}: {e}; rescanning", path.display()),
        }
    }
    Index::new(true)
}

fn save_snapshot(sh: &Shared, path: &std::path::Path) {
    let ix = sh.index.read().unwrap();
    if ix.volumes().is_empty() {
        return; // nothing scanned yet: never overwrite a good snapshot with an empty one
    }
    let t = Instant::now();
    match ix.save(path) {
        Ok(()) => info!("snapshot saved ({} records, {:.1?})", ix.live_count(), t.elapsed()),
        Err(e) => error!("snapshot save failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Args, String> {
        parse_args(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn args() {
        assert!(p(&[]).is_err());
        assert!(p(&["--console", "--bogus"]).is_err());
        assert!(p(&["--console", "--data"]).is_err());
        let a = p(&["--console", "--data", "D:\\x", "--volumes", "c, d:"]).unwrap();
        assert_eq!(a.volumes, Some(vec!['C', 'D']));
        assert_eq!(a.log, PathBuf::from("D:\\x").join("searchd.log"));
        assert!(p(&["--console", "--volumes", "1"]).is_err());
    }
}
