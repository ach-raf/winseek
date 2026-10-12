//! One worker thread per volume: validate/obtain a journal cursor (rescanning when needed),
//! then follow the USN journal and forward batches to the writer.

use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winseek_index::{Index, IndexBuilder, VolumeMeta};
use winseek_ipc::VolumeState;
use winseek_ntfs::{open_volume_for_usn, scan_volume, VolumeInfo};
use winseek_usn::{query_journal, run_monitor, JournalCursor, MonitorConfig, MonitorExit};

use crate::log::{error, info, warn};
use crate::state::{Msg, Shared};

const RETRY: Duration = Duration::from_secs(60);

/// Is the cursor stored in the snapshot still usable for this volume?
pub fn stored_cursor(info: &VolumeInfo, meta: &VolumeMeta) -> Option<JournalCursor> {
    let l = info.letter;
    if meta.journal_id == 0 {
        return None;
    }
    if meta.serial != 0 && info.serial != 0 && meta.serial != info.serial {
        warn!("{l}: volume serial changed ({:x} -> {:x}); full scan", meta.serial, info.serial);
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
/// replay is idempotent), then sweep the $MFT into a fresh single-volume index.
fn scan(sh: &Shared, info: &VolumeInfo) -> Result<(Index, JournalCursor), String> {
    let l = info.letter;
    let vol = l as u8;
    let h = open_volume_for_usn(l).map_err(|e| e.to_string())?;
    let j = query_journal(&h).map_err(|e| format!("{e} (is the USN journal enabled?)"))?;
    let cursor = JournalCursor { journal_id: j.journal_id, next_usn: j.next_usn };
    drop(h);

    let t = Instant::now();
    let mut b = IndexBuilder::new(true);
    b.add_volume(vol, 5);
    let mut count = 0u64;
    let mut last_log = Instant::now();
    let mut last_pct = 0u64;
    let stats = scan_volume(
        l,
        &mut |e| {
            count += 1;
            b.push(vol, e.frn, e.parent_frn, &e.name, e.is_dir, e.size, e.mtime);
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
    let mut ix = b.finish();
    if let Some(m) = ix.volume_mut(vol) {
        m.serial = info.serial;
        m.journal_id = cursor.journal_id;
        m.next_usn = cursor.next_usn;
    }
    info!("{l}: index sorted in {:.1?}; built (scan + sort) in {:.1?}", t_sort.elapsed(), t.elapsed());
    Ok((ix, cursor))
}

fn sleep_stoppable(sh: &Shared, d: Duration) {
    let end = Instant::now() + d;
    while !sh.shutdown.load(Ordering::Relaxed) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `resume`: a validated cursor to catch up from, or `None` for an initial scan.
pub fn run(sh: Arc<Shared>, tx: Sender<Msg>, info: VolumeInfo, mut resume: Option<JournalCursor>) {
    let l = info.letter;
    let vol = l as u8;
    let ctl = sh.ctl[&l].clone();
    let cfg = MonitorConfig::default();
    while !sh.shutdown.load(Ordering::Relaxed) {
        let cursor = match resume.take() {
            Some(c) => c,
            None => match scan(&sh, &info) {
                Ok((fresh, c)) => {
                    let _ = tx.send(Msg::Replace { fresh: Box::new(fresh) });
                    c
                }
                Err(e) => {
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
                error!("{l}: {e}; retrying in {}s", RETRY.as_secs());
                sh.set_state(l, VolumeState::Error { msg: e.to_string() });
                resume = Some(cursor);
                sleep_stoppable(&sh, RETRY);
                continue;
            }
        };
        info!("{l}: following USN journal {:x} from {}", cursor.journal_id, cursor.next_usn);
        let exit = run_monitor(&handle, cursor, &cfg, &ctl.stop, &mut |changes, cursor| {
            let _ = tx.send(Msg::Changes { vol, changes, cursor });
        });
        drop(handle);
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
                    resume = Some(cursor); // spurious stop: just follow again
                }
            }
        }
    }
}
