//! One worker thread per volume. USN volumes (NTFS, ReFS with a journal): validate/obtain a
//! journal cursor (rescanning when needed), then follow the journal and forward batches to
//! the writer. Folder-watch volumes live in `folder_worker`.

use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winseek_folder::{file_id_128, logical_drive_mask, walk_file_ids, watch_handle, HandleEvent, Registration, WatchCanceller};
use winseek_index::{Index, IndexBuilder, VolKind, VolumeMeta};
use winseek_ipc::{Monitor, VolumeState};
use winseek_ntfs::{open_volume_for_usn, scan_volume, VolumeInfo};
use winseek_usn::{query_journal, run_monitor, JournalCursor, MonitorConfig, MonitorExit, RecordVersion};

use crate::log::{error, info, warn};
use crate::state::{Msg, Shared, VolCtl};

const RETRY: Duration = Duration::from_secs(60);
/// How long a query-remove callback waits for the worker to close its handles.
const RELEASE_WAIT: Duration = Duration::from_secs(5);
/// How long a worker waits, after releasing for a query-remove, to see the volume go away.
const GONE_WAIT: Duration = Duration::from_secs(30);

/// One volume to index, as detected at startup or on arrival.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vol {
    pub letter: char,
    /// Filesystem name as Windows reports it.
    pub fs: String,
    /// 64-bit NTFS serial when known, else the 32-bit `GetVolumeInformationW` serial.
    pub serial: u64,
    pub kind: VolKind,
    /// NTFS geometry (only for `VolKind::Ntfs`).
    pub ntfs: Option<VolumeInfo>,
}

impl Vol {
    pub fn monitor(&self) -> Monitor {
        if self.kind == VolKind::Folder {
            Monitor::FolderWatch
        } else {
            Monitor::Usn
        }
    }
    pub fn root(&self) -> String {
        format!("{}:\\", self.letter)
    }
}

/// Is the drive letter still mounted?
pub fn present(letter: char) -> bool {
    let bit = (letter.to_ascii_uppercase() as u32).wrapping_sub('A' as u32);
    bit < 26 && logical_drive_mask() & (1 << bit) != 0
}

/// Is the cursor stored in the snapshot still usable for this volume?
pub fn stored_cursor(vol: &Vol, meta: &VolumeMeta) -> Option<JournalCursor> {
    let l = vol.letter;
    if meta.journal_id == 0 || meta.kind != vol.kind || vol.kind == VolKind::Folder {
        return None;
    }
    if meta.serial != 0 && vol.serial != 0 && meta.serial != vol.serial {
        warn!("{l}: volume serial changed ({:x} -> {:x}); full scan", meta.serial, vol.serial);
        return None;
    }
    let h = open_volume_for_usn(l).map_err(|e| warn!("{l}: {e}")).ok()?;
    let j = query_journal(&h).map_err(|e| warn!("{l}: {e}")).ok()?;
    if j.journal_id != meta.journal_id {
        warn!("{l}: journal id changed; full scan");
        None
    } else if meta.next_usn < j.lowest_valid_usn || meta.next_usn > j.next_usn {
        warn!("{l}: stored USN {} outside journal [{}, {}]; full scan", meta.next_usn, j.lowest_valid_usn, j.next_usn);
        None
    } else {
        Some(JournalCursor { journal_id: j.journal_id, next_usn: meta.next_usn })
    }
}

/// Take the journal cursor FIRST (changes made during the scan are replayed afterwards;
/// replay is idempotent), then sweep the volume into a fresh single-volume index:
/// the raw $MFT on NTFS, a `FILE_ID_128` directory walk on ReFS.
fn scan(sh: &Shared, vol: &Vol, ctl: &VolCtl) -> Result<(Index, JournalCursor), String> {
    let l = vol.letter;
    let h = open_volume_for_usn(l).map_err(|e| e.to_string())?;
    let j = query_journal(&h).map_err(|e| format!("{e} (is the USN journal enabled?)"))?;
    let cursor = JournalCursor { journal_id: j.journal_id, next_usn: j.next_usn };
    drop(h);
    let mut ix = match vol.kind {
        VolKind::Refs => scan_refs(sh, vol, ctl)?,
        _ => scan_ntfs(sh, vol)?,
    };
    if let Some(m) = ix.volume_mut(l as u8) {
        m.serial = vol.serial;
        m.kind = vol.kind;
        m.journal_id = cursor.journal_id;
        m.next_usn = cursor.next_usn;
    }
    Ok((ix, cursor))
}

fn scan_ntfs(sh: &Shared, vol: &Vol) -> Result<Index, String> {
    let l = vol.letter;
    let v = l as u8;
    let t = Instant::now();
    let mut b = IndexBuilder::new(true);
    b.add_volume(v, 5);
    let mut count = 0u64;
    let mut last_log = Instant::now();
    let mut last_pct = 0u64;
    let stats = scan_volume(
        l,
        &mut |e| {
            count += 1;
            b.push(v, e.frn, e.parent_frn, &e.name, e.is_dir, e.size, e.mtime);
        },
        &mut |done, total| {
            sh.set_state(l, VolumeState::Scanning { done, total });
            let pct = (done * 100).checked_div(total).unwrap_or(0);
            if last_log.elapsed() >= Duration::from_secs(2) || pct >= last_pct + 10 {
                let secs = t.elapsed().as_secs_f64();
                info!(
                    "scan {l}: {pct}% ({:.1}M/{:.1}M records, {secs:.1} s, {:.0}k rec/s)",
                    done as f64 / 1e6,
                    total as f64 / 1e6,
                    done as f64 / secs.max(0.001) / 1e3
                );
                last_log = Instant::now();
                last_pct = pct;
            }
        },
    )
    .map_err(|e| e.to_string())?;
    info!("{l}: scanned {count} entries in {:.1?}; stats: {stats:?}", t.elapsed());
    let t_sort = Instant::now();
    let ix = b.finish();
    info!("{l}: index sorted in {:.1?}; built (scan + sort) in {:.1?}", t_sort.elapsed(), t.elapsed());
    Ok(ix)
}

/// ReFS: walk directories with `FileIdExtdDirectoryInfo` (128-bit ids); the root's FRN maps
/// to 5, every folder FRN is interned to a synthetic u64 id (files keep 0).
fn scan_refs(sh: &Shared, vol: &Vol, ctl: &VolCtl) -> Result<Index, String> {
    let l = vol.letter;
    let v = l as u8;
    let root = vol.root();
    let t = Instant::now();
    let root128 = file_id_128(&root).map_err(|e| format!("cannot read the root file id: {e}"))?;
    let mut b = IndexBuilder::new(true);
    b.add_volume(v, 5);
    b.set_id128(v, root128, 5);
    let mut count = 0u64;
    let mut last = Instant::now();
    let stats = walk_file_ids(&root, &ctl.stop, &mut |e| {
        count += 1;
        let id = if e.is_dir { b.intern128(v, e.id) } else { 0 };
        let parent = b.intern128(v, e.parent);
        b.push(v, id, parent, e.name, e.is_dir, e.size, e.mtime);
        if last.elapsed() >= Duration::from_secs(1) {
            sh.set_state(l, VolumeState::Scanning { done: count, total: 0 });
            last = Instant::now();
        }
    })
    .map_err(|e| format!("ReFS walk failed: {e}"))?;
    info!("{l}: ReFS walk: {count} entries in {:.1?}; {stats:?}", t.elapsed());
    Ok(b.finish())
}

pub(crate) fn sleep_stoppable(sh: &Shared, d: Duration) {
    let end = Instant::now() + d;
    while !sh.shutdown.load(Ordering::Relaxed) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Close-on-query-remove for a long-lived handle (`CM_Register_Notification` on the handle,
/// our stand-in for Everything's per-volume `RegisterDeviceNotification`). The callback asks
/// the worker to stop (and cancels a pending folder watch), then waits until the worker has
/// closed the handle so Windows can eject the drive.
pub(crate) fn guard_removal(raw: isize, ctl: Arc<VolCtl>, cancel: Option<WatchCanceller>, letter: char) -> Option<Registration> {
    let cb = move |ev: HandleEvent| match ev {
        HandleEvent::QueryRemove => {
            info!("{letter}: device removal requested; closing handles");
            ctl.released.store(false, Ordering::SeqCst);
            ctl.removing.store(true, Ordering::SeqCst);
            ctl.stop.store(true, Ordering::SeqCst);
            if let Some(c) = &cancel {
                c.cancel();
            }
            let end = Instant::now() + RELEASE_WAIT;
            while !ctl.released.load(Ordering::SeqCst) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        HandleEvent::QueryRemoveFailed => {
            info!("{letter}: device removal was cancelled");
            ctl.removing.store(false, Ordering::SeqCst);
        }
        _ => {}
    };
    watch_handle(raw, Box::new(cb)).map_err(|e| warn!("{letter}: no removal notifications: {e}")).ok()
}

/// After the worker closed its handles because of a removal: wait for the drive to vanish
/// (true: tell the writer to drop it) or for the removal to be cancelled / time out (false:
/// carry on). `reg` stays registered meanwhile so a `QueryRemoveFailed` can arrive.
pub(crate) fn settle_removal(sh: &Shared, tx: &Sender<Msg>, ctl: &VolCtl, letter: char, reg: Option<Registration>) -> bool {
    ctl.released.store(true, Ordering::SeqCst);
    let end = Instant::now() + GONE_WAIT;
    let gone = loop {
        if !present(letter) {
            break true;
        }
        if !ctl.removing.load(Ordering::SeqCst) || sh.shutdown.load(Ordering::Relaxed) || Instant::now() >= end {
            break false;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    drop(reg);
    ctl.removing.store(false, Ordering::SeqCst);
    ctl.released.store(false, Ordering::SeqCst);
    ctl.stop.store(false, Ordering::SeqCst);
    if gone {
        info!("{letter}: volume removed");
        let _ = tx.send(Msg::Drop { vol: letter as u8 });
    }
    gone
}

/// USN volumes (NTFS, ReFS). `resume`: a validated cursor to catch up from, or `None` for an
/// initial scan. Returns when shutting down or when the volume went away.
pub fn run(sh: Arc<Shared>, tx: Sender<Msg>, vol: Vol, mut resume: Option<JournalCursor>) {
    let l = vol.letter;
    let v = l as u8;
    let ctl = sh.ctl(l);
    sh.set_info(l, &vol.fs, vol.monitor());
    let cfg = MonitorConfig {
        version: if vol.kind == VolKind::Refs { RecordVersion::V3 } else { RecordVersion::V2 },
        ..MonitorConfig::default()
    };
    while !sh.shutdown.load(Ordering::Relaxed) {
        if ctl.removing.load(Ordering::SeqCst) && settle_removal(&sh, &tx, &ctl, l, None) {
            return;
        }
        let cursor = match resume.take() {
            Some(c) => c,
            None => match scan(&sh, &vol, &ctl) {
                Ok((fresh, c)) => {
                    let _ = tx.send(Msg::Replace { fresh: Box::new(fresh) });
                    c
                }
                Err(e) => {
                    if !present(l) {
                        info!("{l}: volume is gone");
                        let _ = tx.send(Msg::Drop { vol: v });
                        return;
                    }
                    ctl.stop.store(false, Ordering::Relaxed);
                    if sh.shutdown.load(Ordering::Relaxed) || ctl.removing.load(Ordering::SeqCst) {
                        continue;
                    }
                    if ctl.rescan.swap(false, Ordering::Relaxed) {
                        continue; // a ReFS walk interrupted by a rescan request
                    }
                    error!("{l}: scan failed: {e}; retrying in {}s", RETRY.as_secs());
                    sh.set_state(l, VolumeState::Error { msg: e });
                    sleep_stoppable(&sh, RETRY);
                    continue;
                }
            },
        };
        let handle = match open_volume_for_usn(l) {
            Ok(h) => h,
            Err(e) => {
                if !present(l) {
                    let _ = tx.send(Msg::Drop { vol: v });
                    return;
                }
                error!("{l}: {e}; retrying in {}s", RETRY.as_secs());
                sh.set_state(l, VolumeState::Error { msg: e.to_string() });
                resume = Some(cursor);
                sleep_stoppable(&sh, RETRY);
                continue;
            }
        };
        let reg = guard_removal(handle.raw(), ctl.clone(), None, l);
        info!("{l}: following USN journal {:x} from {} ({:?})", cursor.journal_id, cursor.next_usn, cfg.version);
        let mut last = cursor;
        let exit = run_monitor(&handle, cursor, &cfg, &ctl.stop, &mut |changes, cursor| {
            last = cursor;
            let _ = tx.send(Msg::Changes { vol: v, changes, cursor });
        });
        drop(handle);
        if ctl.removing.load(Ordering::SeqCst) {
            if settle_removal(&sh, &tx, &ctl, l, reg) {
                return;
            }
            resume = Some(last);
            continue;
        }
        drop(reg);
        match exit {
            MonitorExit::JournalReset => {
                warn!("{l}: USN journal reset; rescanning");
                sh.set_state(l, VolumeState::Rescanning);
            }
            MonitorExit::Stopped => {
                ctl.stop.store(false, Ordering::Relaxed);
                if ctl.rescan.swap(false, Ordering::Relaxed) {
                    info!("{l}: rescan requested");
                    sh.set_state(l, VolumeState::Rescanning);
                } else {
                    resume = Some(last); // spurious stop: just follow again
                }
            }
        }
    }
}
