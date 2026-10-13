//! Status-page math: indexing coverage, rolling throughput and ETA. Pure, no I/O.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Throughput is measured over this trailing window.
pub const WINDOW: Duration = Duration::from_secs(60);
/// No completed work for this long means the rate is unknown ("—"), not "slow".
pub const STALE: Duration = Duration::from_secs(15);

/// Share of the files that should end up embedded which already are: `done / (done + pending + failed)`.
/// Skipped files (not images, too big...) are not part of the goal. `None` when there is nothing to do yet.
pub fn coverage(done: u64, pending: u64, failed: u64) -> Option<f64> {
    let total = done + pending + failed;
    (total > 0).then(|| done as f64 / total as f64)
}

/// Seconds until `pending` files are done at `rate` files/s. `None` when idle or the rate is unknown.
pub fn eta_secs(pending: u64, rate: Option<f64>) -> Option<u64> {
    let r = rate?;
    (pending > 0 && r > 0.0 && r.is_finite()).then(|| (pending as f64 / r).ceil() as u64)
}

type Event = (Instant, Instant, usize);

/// Completed-file counts of recent worker passes, for a rolling files/s figure.
#[derive(Debug, Default)]
pub struct Throughput {
    /// `(pass start, pass end, files completed)`, oldest first.
    events: VecDeque<Event>,
}

impl Throughput {
    pub fn record(&mut self, start: Instant, end: Instant, n: usize) {
        if n == 0 {
            return;
        }
        self.events.push_back((start, end, n));
        while self.events.front().is_some_and(|e| end.saturating_duration_since(e.1) > WINDOW) {
            self.events.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Files per second over the last [`WINDOW`], or `None` when nothing finished lately.
    /// A run is measured from its first pass's start, so idle time before it does not dilute the rate.
    pub fn rate(&self, now: Instant) -> Option<f64> {
        let last = self.events.back()?;
        if now.saturating_duration_since(last.1) > STALE {
            return None;
        }
        let from = now.checked_sub(WINDOW);
        let mut start: Option<Instant> = None;
        let mut prev_end: Option<Instant> = None;
        let mut total = 0usize;
        for e in self.events.iter().filter(|e| from.is_none_or(|f| e.1 >= f)) {
            // A long gap between passes means the work stopped and restarted: only count the latest run.
            if prev_end.is_none_or(|p| e.0.saturating_duration_since(p) > STALE) {
                start = Some(e.0);
                total = 0;
            }
            total += e.2;
            prev_end = Some(e.1);
        }
        let span = now.saturating_duration_since(start?).as_secs_f64().max(1.0);
        Some(total as f64 / span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_math() {
        assert_eq!(coverage(0, 0, 0), None);
        assert_eq!(coverage(50, 50, 0), Some(0.5));
        assert_eq!(coverage(90, 5, 5), Some(0.9));
        assert_eq!(coverage(0, 10, 0), Some(0.0));
        assert_eq!(coverage(10, 0, 0), Some(1.0));
    }

    #[test]
    fn eta_math() {
        assert_eq!(eta_secs(120, Some(10.0)), Some(12));
        assert_eq!(eta_secs(121, Some(10.0)), Some(13)); // rounds up
        assert_eq!(eta_secs(0, Some(10.0)), None);
        assert_eq!(eta_secs(5, None), None);
        assert_eq!(eta_secs(5, Some(0.0)), None);
        assert_eq!(eta_secs(5, Some(f64::NAN)), None);
    }

    #[test]
    fn rate_over_recent_passes() {
        let t0 = Instant::now() + Duration::from_secs(1000);
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut t = Throughput::default();
        assert_eq!(t.rate(s(0)), None);
        t.record(s(0), s(5), 50);
        t.record(s(5), s(10), 50);
        // 100 files in the 10 s since the run started
        assert!((t.rate(s(10)).unwrap() - 10.0).abs() < 1e-9);
        // 5 s later nothing new: the same 100 files over 15 s
        assert!((t.rate(s(15)).unwrap() - 100.0 / 15.0).abs() < 1e-9);
        // idle too long: unknown
        assert_eq!(t.rate(s(10 + 16)), None);
    }

    #[test]
    fn idle_gap_restarts_the_run_and_empty_passes_are_ignored() {
        let t0 = Instant::now() + Duration::from_secs(1000);
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut t = Throughput::default();
        t.record(s(0), s(10), 1000); // long ago, fast
        t.record(s(100), s(110), 100); // new run after an idle gap
        let r = t.rate(s(110)).unwrap();
        assert!((r - 10.0).abs() < 1e-9, "{r}"); // the old burst does not count
        t.clear();
        assert_eq!(t.rate(s(110)), None);
        t.record(s(1), s(1), 0);
        assert_eq!(t.rate(s(1)), None);
    }
}
