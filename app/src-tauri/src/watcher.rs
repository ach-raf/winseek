//! Source watcher: swaps demo data for the real index when searchd becomes reachable, and keeps
//! the tray tooltip in sync with the index status.

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::commands::AppState;
use crate::pipe_source::PipeSource;
use crate::service;
use crate::source::VolumeStatus;

pub const TRAY_ID: &str = "winseek-tray";

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Tray tooltip text for a status snapshot.
pub fn tooltip_text(vols: &[VolumeStatus]) -> String {
    if let Some(v) = vols.iter().find(|v| v.state == "scanning") {
        return match (v.done, v.total) {
            (Some(d), Some(t)) if t > 0 => format!("winseek · indexing {}: {}%", v.letter, (d.min(t) * 100 / t)),
            _ => format!("winseek · indexing {}:", v.letter),
        };
    }
    if vols.iter().all(|v| v.state == "demo") {
        return "winseek · demo data".to_string();
    }
    let total: u64 = vols.iter().map(|v| v.files + v.folders).sum();
    format!("winseek · {} files", thousands(total))
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last_tip = String::new();
        let mut last_poll = std::time::Instant::now();
        let mut real = false;
        loop {
            if !real {
                if let Ok(p) = PipeSource::connect(Duration::from_millis(300)) {
                    app.state::<AppState>().set_source(Arc::new(p));
                    real = true;
                    let _ = app.emit("source-changed", ());
                    last_poll = std::time::Instant::now() - Duration::from_secs(10);
                }
            }
            if last_poll.elapsed() >= Duration::from_secs(2) {
                last_poll = std::time::Instant::now();
                let tip = tooltip_text(&app.state::<AppState>().source().status());
                if tip != last_tip {
                    if let Some(tray) = app.tray_by_id(TRAY_ID) {
                        let _ = tray.set_tooltip(Some(&tip));
                    }
                    last_tip = tip;
                }
            }
            let nap = if !real && !service::fast_polling() { 2000 } else { 250 };
            // While demo and not fast-polling, sleep in short slices so a kick is honoured quickly.
            let mut slept = 0;
            while slept < nap {
                std::thread::sleep(Duration::from_millis(250));
                slept += 250;
                if !real && service::fast_polling() {
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(letter: &str, files: u64, folders: u64, state: &str, done: Option<u64>, total: Option<u64>) -> VolumeStatus {
        VolumeStatus { letter: letter.into(), files, folders, state: state.into(), done, total }
    }

    #[test]
    fn tooltip_variants() {
        assert_eq!(
            tooltip_text(&[v("C", 0, 0, "scanning", Some(42), Some(100)), v("D", 1, 1, "live", None, None)]),
            "winseek · indexing C: 42%"
        );
        assert_eq!(
            tooltip_text(&[v("C", 5_000_000, 951_011, "live", None, None)]),
            "winseek · 5,951,011 files"
        );
        assert_eq!(tooltip_text(&[v("C", 1, 1, "demo", None, None)]), "winseek · demo data");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
    }
}
