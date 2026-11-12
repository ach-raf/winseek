//! searchd: Tier 0 daemon. MFT scan -> in-memory name index, USN journal follow, snapshot,
//! named-pipe search server. Runs as `--console` (dev) or a Windows service (`--service`,
//! managed with `--install` / `--uninstall`).
//!
//! Threads: main (signals, periodic snapshot, volume arrival/removal) | writer (sole index
//! mutator) | one worker per volume (NTFS/ReFS: scan + USN monitor; FAT/exFAT: scan +
//! ReadDirectoryChangesW) | pipe accept loop + one per connection.

mod folder_worker;
mod index_ops;
mod log;
mod server;
mod service;
mod state;
mod worker;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use winseek_folder::{list_volumes, watch_volume_interfaces, VolumeDesc};
use winseek_index::{Index, VolKind};
use winseek_ipc::pipe::PipeServer;
use winseek_ipc::PIPE_NAME;
use winseek_ntfs::{list_ntfs_volumes, open_volume_for_usn, VolumeInfo};
use winseek_usn::{query_journal, JournalCursor};

use log::{error, info, warn};
use state::{Msg, Shared};
use worker::Vol;

/// How often the drive letters are re-checked without a device notification (the letter can
/// be assigned after the volume-interface arrival event).
const VOLUME_POLL: Duration = Duration::from_secs(10);

const USAGE: &str = "usage: searchd --console [--data <dir>] [--volumes C,D] [--log <file>]
       searchd --service [--data <dir>] [--volumes C,D] [--log <file>]   (started by the SCM)
       searchd --install [--volumes C,D]     (elevated)
       searchd --uninstall                   (elevated)";
const SAVE_EVERY: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Console,
    Service,
    Install,
    Uninstall,
}

#[derive(Debug)]
struct Args {
    mode: Mode,
    data: PathBuf,
    volumes: Option<Vec<char>>,
    log: PathBuf,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut modes, mut data, mut volumes, mut log) = (Vec::new(), None, None, None);
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--console" => modes.push(Mode::Console),
            "--service" => modes.push(Mode::Service),
            "--install" => modes.push(Mode::Install),
            "--uninstall" => modes.push(Mode::Uninstall),
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
    let mode = match modes.as_slice() {
        [] => return Err("exactly one of --console, --service, --install, --uninstall is required".into()),
        [m] => *m,
        _ => return Err("--console, --service, --install and --uninstall are mutually exclusive".into()),
    };
    let allowed = |opt: bool, name: &str| {
        if opt && matches!(mode, Mode::Install | Mode::Uninstall) {
            Err(format!("{name} is not valid with --install/--uninstall"))
        } else {
            Ok(())
        }
    };
    allowed(data.is_some(), "--data")?;
    allowed(log.is_some(), "--log")?;
    if mode == Mode::Uninstall && volumes.is_some() {
        return Err("--volumes is not valid with --uninstall".into());
    }
    let data = data.unwrap_or_else(default_data_dir);
    let log = log.unwrap_or_else(|| data.join("searchd.log"));
    Ok(Args { mode, data, volumes, log })
}

fn default_data_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into())).join("winseek")
}

/// The one stop flag: set by console Ctrl+C or by the service control handler.
static STOP: AtomicBool = AtomicBool::new(false);

extern "system" fn on_ctrl(_event: u32) -> i32 {
    STOP.store(true, Ordering::SeqCst);
    1 // handled: let main shut down and save
}

#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("searchd: {e}
{USAGE}");
            std::process::exit(2);
        }
    };
    std::process::exit(match args.mode {
        Mode::Console => {
            log::init(&args.log);
            // SAFETY: registering a plain `extern "system"` handler.
            unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
            run(&args, &STOP, &|| {})
        }
        Mode::Service => {
            log::init(&args.log);
            service::run_dispatcher(args)
        }
        Mode::Install | Mode::Uninstall => service::manage(&args),
    });
}

/// Run the daemon until `stop` is set. `ready` is called once the pipe is bound and the
/// workers are running (the service reports RUNNING from it). Returns the process exit code.
fn run(args: &Args, stop: &AtomicBool, ready: &dyn Fn()) -> i32 {
    info!("searchd starting (data dir {}, log {})", args.data.display(), args.log.display());
    let _ = std::fs::create_dir_all(&args.data);
    // Volumes to index: NTFS, FAT/FAT32/exFAT, ReFS on fixed and removable drives.
    let want = args.volumes.as_deref();
    let vols = detect_volumes(want, &[]);
    if let Some(want) = want {
        for w in want.iter().filter(|w| !vols.iter().any(|v| v.letter == **w)) {
            warn!("{w}: is not a supported volume (NTFS, FAT32, exFAT, ReFS); skipped");
        }
        if vols.is_empty() {
            error!("none of the selected volumes can be indexed");
            return 1;
        }
    }
    if vols.is_empty() {
        warn!("no volumes to index yet; waiting for one to arrive");
    }
    for v in &vols {
        info!("{}: {} ({:?})", v.letter, v.fs, v.kind);
    }

    // Fail fast without administrator rights (raw $MFT / USN access needs them).
    if let Some(v) = vols.iter().find(|v| v.kind != VolKind::Folder) {
        if let Err(e) = open_volume_for_usn(v.letter) {
            error!("searchd needs administrator to read $MFT / the USN journal; run elevated ({e})");
            return 1;
        }
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
    let index = load_snapshot(&snap_path, &vols);
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
    let mut workers: BTreeMap<char, JoinHandle<()>> = BTreeMap::new();
    for (v, cur) in vols.into_iter().zip(cursors) {
        match (v.kind, cur) {
            (VolKind::Folder, _) => info!("{}: folder-watch volume; scanning", v.letter),
            (_, Some(c)) => info!("{}: snapshot valid, catching up from USN {}", v.letter, c.next_usn),
            (_, None) => info!("{}: no usable snapshot; full scan", v.letter),
        }
        if cur.is_some() {
            sh.set_state(v.letter, winseek_ipc::VolumeState::Live);
        }
        workers.insert(v.letter, spawn_worker(&sh, &tx, v, cur));
    }
    server::spawn_accept_loop(sh.clone(), pipe);

    // Volume arrival/removal: CM_Register_Notification on the volume device interface (no
    // window needed, works as a service and in --console), plus a slow poll as a backstop.
    let drives_changed = Arc::new(AtomicBool::new(false));
    let _arrivals = {
        let flag = drives_changed.clone();
        watch_volume_interfaces(Box::new(move || flag.store(true, Ordering::SeqCst)))
            .map_err(|e| warn!("no volume arrival notifications ({e}); polling every {}s", VOLUME_POLL.as_secs()))
            .ok()
    };
    info!("ready; serving {PIPE_NAME} (Ctrl+C to stop)");
    ready();

    let mut last_save = Instant::now();
    let mut last_poll = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        sh.trim_query_cache();
        if sh.save_now.swap(false, Ordering::Relaxed) || last_save.elapsed() >= SAVE_EVERY {
            save_snapshot(&sh, &snap_path);
            last_save = Instant::now();
        }
        let notified = drives_changed.swap(false, Ordering::SeqCst);
        if notified || last_poll.elapsed() >= VOLUME_POLL {
            reconcile_volumes(&sh, &tx, want, &mut workers);
            // after a notification, look again shortly: the letter may not be mounted yet
            last_poll = if notified { Instant::now() - VOLUME_POLL + Duration::from_secs(2) } else { Instant::now() };
        }
    }

    info!("shutting down");
    sh.shutdown.store(true, Ordering::SeqCst);
    sh.stop_all();
    // Monitors flush their last batch within ~250 ms; a worker stuck in a scan is abandoned.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !workers.values().all(|w| w.is_finished()) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = tx.send(Msg::Shutdown);
    let _ = writer.join();
    save_snapshot(&sh, &snap_path);
    info!("bye");
    0
}

fn spawn_worker(sh: &Arc<Shared>, tx: &Sender<Msg>, v: Vol, cur: Option<JournalCursor>) -> JoinHandle<()> {
    let (sh, tx) = (sh.clone(), tx.clone());
    std::thread::spawn(move || match v.kind {
        VolKind::Folder => folder_worker::run(sh, tx, v),
        _ => worker::run(sh, tx, v, cur),
    })
}

/// Index a newly mounted volume; make the worker of a vanished one drop it.
fn reconcile_volumes(sh: &Arc<Shared>, tx: &Sender<Msg>, want: Option<&[char]>, workers: &mut BTreeMap<char, JoinHandle<()>>) {
    workers.retain(|_, h| !h.is_finished());
    for &l in workers.keys() {
        if !worker::present(l) {
            let c = sh.ctl(l);
            if !c.removing.swap(true, Ordering::SeqCst) {
                info!("{l}: volume disappeared");
                c.stop.store(true, Ordering::SeqCst);
            }
        }
    }
    let running: Vec<char> = workers.keys().copied().collect();
    for v in detect_volumes(want, &running) {
        info!("{}: new {} volume ({:?}); indexing", v.letter, v.fs, v.kind);
        sh.set_state(v.letter, winseek_ipc::VolumeState::Scanning { done: 0, total: 0 });
        workers.insert(v.letter, spawn_worker(sh, tx, v, None));
    }
}

/// The volumes Tier 0 can index now, minus `skip`, restricted to `want` when given.
fn detect_volumes(want: Option<&[char]>, skip: &[char]) -> Vec<Vol> {
    let descs: Vec<VolumeDesc> = list_volumes()
        .into_iter()
        .filter(|d| !skip.contains(&d.letter) && want.is_none_or(|w| w.contains(&d.letter)))
        .collect();
    let ntfs = if descs.iter().any(|d| d.fs.eq_ignore_ascii_case("NTFS")) { list_ntfs_volumes() } else { Vec::new() };
    descs.into_iter().filter_map(|d| classify(d, &ntfs, refs_has_journal)).collect()
}

/// Pick the volume kind from the filesystem name (Everything's volume types: NTFS / FAT
/// folder index / ReFS; ReFS without a usable journal runs as a folder index, like
/// Everything's RDC mode).
fn classify(d: VolumeDesc, ntfs: &[VolumeInfo], refs_journal: impl Fn(char) -> bool) -> Option<Vol> {
    let kind = match d.fs.to_ascii_uppercase().as_str() {
        "NTFS" => VolKind::Ntfs,
        "FAT" | "FAT12" | "FAT16" | "FAT32" | "EXFAT" => VolKind::Folder,
        "REFS" if refs_journal(d.letter) => VolKind::Refs,
        "REFS" => VolKind::Folder,
        _ => return None,
    };
    let info = ntfs.iter().find(|i| i.letter == d.letter).cloned();
    if kind == VolKind::Ntfs && info.is_none() {
        return None;
    }
    let serial = info.as_ref().map_or(d.serial as u64, |i| i.serial);
    Some(Vol { letter: d.letter, fs: d.fs, serial, kind, ntfs: info })
}

fn refs_has_journal(letter: char) -> bool {
    match open_volume_for_usn(letter).map_err(|e| e.to_string()).and_then(|h| query_journal(&h).map_err(|e| e.to_string())) {
        Ok(_) => true,
        Err(e) => {
            warn!("{letter}: ReFS USN journal unavailable ({e}); using folder watch");
            false
        }
    }
}

/// Load the snapshot and keep the volumes that still match what is mounted (same letter,
/// kind and serial); the others are dropped and rescanned. v1 snapshots load as NTFS.
fn load_snapshot(path: &std::path::Path, vols: &[Vol]) -> Index {
    if path.exists() {
        match Index::load(path) {
            Ok(ix) if !ix.has_meta() => warn!("snapshot has no size/mtime columns; ignoring"),
            Ok(ix) => {
                let matches = |m: &winseek_index::VolumeMeta| {
                    vols.iter().any(|v| {
                        v.letter as u8 == m.letter && v.kind == m.kind && (m.serial == 0 || v.serial == 0 || m.serial == v.serial)
                    })
                };
                for m in ix.volumes().iter().filter(|m| !matches(m)) {
                    warn!("{}: snapshot volume ({:?}) is not mounted or changed; dropping it", m.letter as char, m.kind);
                }
                let ix = index_ops::retain_volumes(ix, matches);
                info!("loaded snapshot: {} records", ix.live_count());
                return ix;
            }
            Err(e) => warn!("cannot load snapshot {}: {e}; rescanning", path.display()),
        }
    } else {
        info!("no snapshot at {}", path.display());
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
        assert_eq!(a.mode, Mode::Console);
        assert_eq!(a.volumes, Some(vec!['C', 'D']));
        assert_eq!(a.log, PathBuf::from("D:\\x").join("searchd.log"));
        assert!(p(&["--console", "--volumes", "1"]).is_err());
    }

    #[test]
    fn classify_volume_kinds() {
        let d = |l: char, fs: &str| VolumeDesc { letter: l, fs: fs.into(), serial: 0xABCD, removable: true };
        let ntfs = [VolumeInfo { letter: 'C', serial: 0x1122_3344_5566, bytes_per_cluster: 4096, record_size: 1024 }];
        let k = |v: Option<Vol>| v.map(|v| (v.kind, v.serial));
        assert_eq!(k(classify(d('C', "NTFS"), &ntfs, |_| true)), Some((VolKind::Ntfs, 0x1122_3344_5566)));
        assert_eq!(k(classify(d('N', "NTFS"), &ntfs, |_| true)), None, "NTFS without geometry is skipped");
        for fs in ["FAT32", "exFAT", "FAT"] {
            assert_eq!(k(classify(d('E', fs), &ntfs, |_| true)), Some((VolKind::Folder, 0xABCD)), "{fs}");
        }
        assert_eq!(k(classify(d('R', "ReFS"), &ntfs, |_| true)), Some((VolKind::Refs, 0xABCD)));
        assert_eq!(k(classify(d('R', "ReFS"), &ntfs, |_| false)), Some((VolKind::Folder, 0xABCD)), "RDC fallback");
        assert_eq!(k(classify(d('U', "UDF"), &ntfs, |_| true)), None);
        let v = classify(d('E', "exFAT"), &ntfs, |_| true).unwrap();
        assert_eq!((v.monitor(), v.root().as_str()), (winseek_ipc::Monitor::FolderWatch, r"E:\"));
    }

    #[test]
    fn modes() {
        assert_eq!(p(&["--service"]).unwrap().mode, Mode::Service);
        let s = p(&["--service", "--data", "D:\\x", "--log", "L.txt", "--volumes", "C"]).unwrap();
        assert_eq!(s.log, PathBuf::from("L.txt"));
        assert_eq!(p(&["--install"]).unwrap().mode, Mode::Install);
        assert_eq!(p(&["--install", "--volumes", "c,d"]).unwrap().volumes, Some(vec!['C', 'D']));
        assert_eq!(p(&["--uninstall"]).unwrap().mode, Mode::Uninstall);
        // mutual exclusion
        for pair in [
            ["--console", "--service"],
            ["--console", "--install"],
            ["--service", "--install"],
            ["--install", "--uninstall"],
            ["--service", "--uninstall"],
            ["--console", "--uninstall"],
        ] {
            assert!(p(&pair).is_err(), "{pair:?}");
        }
        // options that make no sense for management modes
        assert!(p(&["--install", "--data", "x"]).is_err());
        assert!(p(&["--install", "--log", "x"]).is_err());
        assert!(p(&["--uninstall", "--volumes", "C"]).is_err());
        assert!(p(&["--bogus"]).is_err());
    }
}
