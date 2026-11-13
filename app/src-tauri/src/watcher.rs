//! Source watcher: swaps demo data for the real index when searchd becomes reachable, and keeps
//! the tray tooltip in sync with the index status.

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::commands::AppState;
use crate::pipe_source::PipeSource;
use crate::service;
use crate::source::VolumeStatus;
use crate::tier1::policy::PauseReason;

pub const TRAY_ID: &str = "winseek-tray";

/// The tray's pause/resume menu item, kept in managed state so any pause change can refresh it.
pub struct PauseItem(pub tauri::menu::MenuItem<tauri::Wry>);

/// Tray label for the effective Tier 1 pause state.
pub fn pause_label(r: Option<PauseReason>) -> &'static str {
    match r {
        None => "Pause indexing",
        Some(PauseReason::User) => "Resume indexing",
        Some(PauseReason::Battery) => "Indexing paused (on battery)",
        Some(PauseReason::Fullscreen) => "Indexing paused (full screen)",
    }
}

/// Re-sync the tray item text with the real Tier 1 state. Call after every pause change
/// (tray, panel, automatic policy); cheap and idempotent.
pub fn refresh_pause_item(app: &AppHandle) {
    let (Some(item), Ok(t)) = (app.try_state::<PauseItem>(), app.state::<crate::tier1::Handle>().get()) else {
        return;
    };
    let _ = item.0.set_text(pause_label(t.paused_reason()));
}

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

/// "about 12 min", "about 2 h 10 min", "under a minute".
pub fn eta_text(secs: u64) -> String {
    match secs {
        0..=59 => "under a minute".into(),
        60..=3599 => format!("about {} min", secs.div_ceil(60)),
        _ => {
            let m = (secs / 60) % 60;
            if m == 0 { format!("about {} h", secs / 3600) } else { format!("about {} h {m} min", secs / 3600) }
        }
    }
}

/// Meaning-index progress for the tray: `None` when there is nothing to report (idle and caught up,
/// models still missing, or the worker unavailable).
pub fn tier1_line(state: &str, done: u64, pending: u64, rate: Option<f64>) -> Option<String> {
    let total = done + pending;
    match state {
        "paused" if pending > 0 => Some(format!("meaning index paused · {} left", thousands(pending))),
        "indexing" if total > 0 && pending > 0 => {
            let pct = done * 100 / total;
            let eta = crate::tier1::progress::eta_secs(pending, rate).map(|s| format!(" · {}", eta_text(s))).unwrap_or_default();
            Some(format!("meaning index {pct}%{eta}"))
        }
        _ => None,
    }
}

/// Full tray tooltip: the name-index line, plus the meaning-index line when there is one.
pub fn tray_tip(vols: &[VolumeStatus], t1: Option<String>) -> String {
    let base = tooltip_text(vols);
    match t1 {
        Some(l) => format!("{base} · {l}"),
        None => base,
    }
}

/// Disabled menu line mirroring the meaning-index progress.
pub struct ProgressItem(pub tauri::menu::MenuItem<tauri::Wry>);

/// How often the worker's speed is remembered for first-pass estimates.
const REMEMBER_EVERY: Duration = Duration::from_secs(60);

/// Read the worker once: `(tooltip fragment, menu text)`. Also remembers its speed now and then.
fn tier1_progress(app: &AppHandle, last_remember: &mut std::time::Instant) -> (Option<String>, Option<String>) {
    let Ok(t) = app.state::<crate::tier1::Handle>().get() else { return (None, None) };
    let st = t.status();
    let Some(s) = st.stats else { return (None, None) };
    let rate = t.rate();
    if rate.is_some() && last_remember.elapsed() >= REMEMBER_EVERY {
        *last_remember = std::time::Instant::now();
        t.remember_rate();
    }
    let line = tier1_line(&st.state, s.done, s.pending, rate);
    let menu = line.as_ref().map(|l| {
        let mut c = l.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    });
    (line, menu)
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last_tip = String::new();
        let mut last_poll = std::time::Instant::now();
        let mut last_remember = std::time::Instant::now();
        let mut real = false;
        loop {
            refresh_pause_item(&app); // picks up automatic (battery / full-screen) pauses
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
                let (t1_line, t1_menu) = tier1_progress(&app, &mut last_remember);
                let tip = tray_tip(&app.state::<AppState>().source().status(), t1_line);
                if let Some(item) = app.try_state::<ProgressItem>() {
                    let _ = item.0.set_text(t1_menu.as_deref().unwrap_or("Meaning index: up to date"));
                }
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
    fn pause_labels() {
        assert_eq!(pause_label(None), "Pause indexing");
        assert_eq!(pause_label(Some(PauseReason::User)), "Resume indexing");
        assert!(pause_label(Some(PauseReason::Battery)).contains("battery"));
        assert!(pause_label(Some(PauseReason::Fullscreen)).contains("full screen"));
    }

    #[test]
    fn eta_wording() {
        assert_eq!(eta_text(5), "under a minute");
        assert_eq!(eta_text(60), "about 1 min");
        assert_eq!(eta_text(61), "about 2 min");
        assert_eq!(eta_text(3600), "about 1 h");
        assert_eq!(eta_text(3600 + 600), "about 1 h 10 min");
    }

    #[test]
    fn tier1_tray_lines() {
        assert_eq!(tier1_line("indexing", 450, 550, Some(5.0)).unwrap(), "meaning index 45% · about 2 min");
        assert_eq!(tier1_line("indexing", 450, 550, None).unwrap(), "meaning index 45%");
        assert_eq!(tier1_line("paused", 1, 1234, None).unwrap(), "meaning index paused · 1,234 left");
        assert_eq!(tier1_line("idle", 10, 0, None), None);
        assert_eq!(tier1_line("models_missing", 0, 100, None), None);
        let tip = tray_tip(&[v("C", 5, 5, "live", None, None)], Some("meaning index 45%".into()));
        assert_eq!(tip, "winseek · 10 files · meaning index 45%");
        assert!(tip.len() < 128, "tray tooltips are limited to 127 characters");
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
