//! Change bus: one pipe subscription to searchd's USN-derived events (all roots), coalesced
//! into batched `fs-delta` events for the UI.
//!
//! [`coalesce`] is pure (events -> [`FsDelta`]); [`Batcher`] is the window logic over explicit
//! `Instant`s; [`run`] is the thread body (connect / subscribe / backoff / emit).
//! Independent of the Tier 1 feed, which has its own subscription.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use winseek_ipc::pipe::connect;
use winseek_ipc::{write_message, ChangeKind, Request, Response, PIPE_NAME};

/// Tauri event name (the UI listens with `event.listen("fs-delta")`).
pub const EVENT: &str = "fs-delta";
/// Quiet gap that closes a window.
const WINDOW: Duration = Duration::from_millis(150);
/// Hard cap on a window under continuous churn.
const WINDOW_MAX: Duration = Duration::from_millis(400);
/// Max entries across removed/renamed/added/modified per event.
const MAX_ENTRIES: usize = 2000;
const MAX_DIRS: usize = 500;
/// Force a flush if this many raw events pile up in one window.
const MAX_RAW: usize = 100_000;
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A connection that lived this long counts as healthy (backoff reset).
const HEALTHY_AFTER: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEvt {
    pub kind: ChangeKind,
    pub path: String,
    pub old_path: Option<String>,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Renamed {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FsDelta {
    pub removed: Vec<String>,
    pub renamed: Vec<Renamed>,
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub dirs: Vec<String>,
    pub overflow: bool,
}

impl FsDelta {
    pub fn overflow() -> Self {
        FsDelta {
            overflow: true,
            ..Default::default()
        }
    }
    pub fn is_empty(&self) -> bool {
        !self.overflow
            && self.removed.is_empty()
            && self.renamed.is_empty()
            && self.added.is_empty()
            && self.modified.is_empty()
    }
}

fn key(p: &str) -> String {
    p.to_lowercase()
}

/// Parent folder of a path (`C:\a\b.txt` -> `C:\a`, `C:\a` -> `C:\`); `None` for bare names.
fn parent(p: &str) -> Option<&str> {
    let t = p.trim_end_matches(['\\', '/']);
    let i = t.rfind(['\\', '/'])?;
    let par = &t[..i];
    if par.is_empty() {
        return None;
    }
    if par.ends_with(':') {
        return Some(&t[..=i]);
    }
    Some(par)
}

/// A path that existed (or was created) during the window.
struct Item {
    /// Pre-window path when it already existed; `None` = created in this window.
    from: Option<String>,
    cur: String,
    modified: bool,
}

fn under(child_key: &str, dir_key: &str) -> bool {
    let d = dir_key.trim_end_matches(['\\', '/']);
    child_key.len() > d.len()
        && child_key.starts_with(d)
        && matches!(child_key.as_bytes()[d.len()], b'\\' | b'/')
}

fn fresh(items: &mut Vec<Option<Item>>, by_key: &mut HashMap<String, usize>, k: String, it: Item) {
    if let Some(prev) = by_key.insert(k, items.len()) {
        items[prev] = None;
    }
    items.push(Some(it));
}

/// Collapse an ordered batch of raw events into one delta.
pub fn coalesce(events: &[ChangeEvt]) -> FsDelta {
    let mut items: Vec<Option<Item>> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut removed: Vec<String> = Vec::new();

    for e in events {
        let k = key(&e.path);
        match e.kind {
            ChangeKind::Modified => {
                if e.is_dir {
                    continue;
                }
                match by_key.get(&k) {
                    Some(&i) => {
                        if let Some(it) = items[i].as_mut() {
                            it.modified = true;
                        }
                    }
                    None => fresh(
                        &mut items,
                        &mut by_key,
                        k,
                        Item {
                            from: Some(e.path.clone()),
                            cur: e.path.clone(),
                            modified: true,
                        },
                    ),
                }
            }
            ChangeKind::Created => fresh(
                &mut items,
                &mut by_key,
                k,
                Item {
                    from: None,
                    cur: e.path.clone(),
                    modified: false,
                },
            ),
            ChangeKind::Deleted => {
                if e.is_dir {
                    // Children touched in this window are covered by the folder removal.
                    let victims: Vec<String> =
                        by_key.keys().filter(|ck| under(ck, &k)).cloned().collect();
                    for vk in victims {
                        if let Some(i) = by_key.remove(&vk) {
                            if let Some(f) = items[i].take().and_then(|it| it.from) {
                                if !under(&key(&f), &k) {
                                    removed.push(f);
                                }
                            }
                        }
                    }
                }
                match by_key.remove(&k) {
                    Some(i) => {
                        if let Some(f) = items[i].take().and_then(|it| it.from) {
                            removed.push(f);
                        }
                    }
                    None => removed.push(e.path.clone()),
                }
            }
            ChangeKind::Renamed => {
                let Some(old) = e.old_path.as_deref() else {
                    fresh(
                        &mut items,
                        &mut by_key,
                        k,
                        Item {
                            from: None,
                            cur: e.path.clone(),
                            modified: false,
                        },
                    );
                    continue;
                };
                let existing = by_key.remove(&key(old)).and_then(|i| items[i].take());
                let mut it = existing.unwrap_or(Item {
                    from: Some(old.to_string()),
                    cur: old.to_string(),
                    modified: false,
                });
                it.cur = e.path.clone();
                fresh(&mut items, &mut by_key, k, it);
            }
        }
    }

    let mut d = FsDelta::default();
    let mut seen: HashSet<String> = HashSet::new();
    for p in removed {
        if seen.insert(key(&p)) {
            d.removed.push(p);
        }
    }
    for it in items.into_iter().flatten() {
        match &it.from {
            None => d.added.push(it.cur),
            Some(f) if f != &it.cur => {
                if it.modified {
                    d.modified.push(it.cur.clone());
                }
                d.renamed.push(Renamed {
                    from: f.clone(),
                    to: it.cur,
                });
            }
            Some(_) => {
                if it.modified {
                    d.modified.push(it.cur);
                }
            }
        }
    }

    let total = d.removed.len() + d.renamed.len() + d.added.len() + d.modified.len();
    if total > MAX_ENTRIES {
        return FsDelta::overflow();
    }
    let mut dseen: HashSet<String> = HashSet::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut add_parent = |p: &str| {
        if let Some(par) = parent(p) {
            if dseen.insert(key(par)) {
                dirs.push(par.to_string());
            }
        }
    };
    for p in d.removed.iter().chain(&d.added).chain(&d.modified) {
        add_parent(p);
    }
    for r in &d.renamed {
        add_parent(&r.from);
        add_parent(&r.to);
    }
    if dirs.len() > MAX_DIRS {
        return FsDelta::overflow();
    }
    d.dirs = dirs;
    d
}

/// Window logic over explicit clock values: a window opens on the first event, closes after
/// [`WINDOW`] of quiet, and never lasts longer than [`WINDOW_MAX`].
#[derive(Default)]
pub struct Batcher {
    events: Vec<ChangeEvt>,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Batcher {
    pub fn push(&mut self, e: ChangeEvt, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.events.push(e);
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    /// When the open window must be flushed (`None` if nothing is pending).
    pub fn deadline(&self) -> Option<Instant> {
        let (f, l) = (self.first?, self.last?);
        Some((l + WINDOW).min(f + WINDOW_MAX))
    }
    pub fn due(&self, now: Instant) -> bool {
        self.events.len() >= MAX_RAW || self.deadline().is_some_and(|d| now >= d)
    }
    /// Take the window and coalesce it (`None` if the result is empty).
    pub fn take(&mut self) -> Option<FsDelta> {
        let ev = std::mem::take(&mut self.events);
        self.first = None;
        self.last = None;
        let d = coalesce(&ev);
        (!d.is_empty()).then_some(d)
    }
}

fn next_backoff(cur: Duration) -> Duration {
    (cur * 2).min(BACKOFF_MAX)
}

fn sleep_unless_stopped(d: Duration, stop: &AtomicBool) {
    let end = Instant::now() + d;
    while !stop.load(Ordering::Relaxed) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Thread body. Connects, subscribes (all roots), batches and calls `emit`. After any drop /
/// error the next successful subscribe emits one overflow delta so every screen resyncs; the
/// very first connect emits nothing. Returns when `stop` is set.
pub fn run(mut emit: impl FnMut(FsDelta), stop: &AtomicBool) {
    let mut backoff = BACKOFF_START;
    let mut had_conn = false;
    let mut b = Batcher::default();
    while !stop.load(Ordering::Relaxed) {
        let mut conn = match connect(PIPE_NAME, Duration::from_millis(500)) {
            Ok(c) => c,
            Err(_) => {
                sleep_unless_stopped(backoff, stop);
                backoff = next_backoff(backoff);
                continue;
            }
        };
        let sub = Request::Subscribe {
            id: 1,
            roots: Vec::new(),
        };
        if write_message(&mut conn, &sub).is_err() {
            sleep_unless_stopped(backoff, stop);
            backoff = next_backoff(backoff);
            continue;
        }
        if had_conn {
            emit(FsDelta::overflow());
        }
        had_conn = true;
        let since = Instant::now();
        let mut got_msg = false;
        // Single-threaded read loop: reads are bounded, so `stop` and the window deadline are
        // honoured without a helper thread, and dropping `conn` closes the pipe.
        while !stop.load(Ordering::Relaxed) {
            let wait = b
                .deadline()
                .map_or(POLL, |d| {
                    d.saturating_duration_since(Instant::now())
                        .max(Duration::from_millis(5))
                })
                .min(POLL);
            match conn.read_message_timeout::<Response>(wait) {
                Ok(msg) => {
                    if !got_msg {
                        got_msg = true;
                        backoff = BACKOFF_START;
                    }
                    if let Response::Change {
                        kind,
                        path,
                        old_path,
                        is_dir,
                    } = msg
                    {
                        b.push(
                            ChangeEvt {
                                kind,
                                path,
                                old_path,
                                is_dir,
                            },
                            Instant::now(),
                        );
                    } else if matches!(msg, Response::Resync) {
                        // A rescan swapped a volume in / a drive left: no per-file events exist
                        // for it, so every screen must read its lists again (after any pending batch).
                        if let Some(d) = b.take() {
                            emit(d);
                        }
                        emit(FsDelta::overflow());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                Err(_) => break, // pipe closed / dropped by searchd: reconnect
            }
            if b.due(Instant::now()) {
                if let Some(d) = b.take() {
                    emit(d);
                }
            }
        }
        if let Some(d) = b.take() {
            emit(d);
        }
        if since.elapsed() >= HEALTHY_AFTER {
            backoff = BACKOFF_START;
        }
        if !stop.load(Ordering::Relaxed) {
            sleep_unless_stopped(backoff, stop);
        }
    }
}

/// Start the background thread (lives for the whole app run).
pub fn spawn(app: AppHandle) {
    let stop = Arc::new(AtomicBool::new(false));
    let _ = std::thread::Builder::new()
        .name("changebus".into())
        .spawn(move || {
            crate::tier1::lower_thread_priority();
            run(
                |d| {
                    let _ = app.emit(EVENT, d);
                },
                &stop,
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: ChangeKind, path: &str, old: Option<&str>, is_dir: bool) -> ChangeEvt {
        ChangeEvt {
            kind,
            path: path.into(),
            old_path: old.map(Into::into),
            is_dir,
        }
    }
    fn cr(p: &str) -> ChangeEvt {
        ev(ChangeKind::Created, p, None, false)
    }
    fn md(p: &str) -> ChangeEvt {
        ev(ChangeKind::Modified, p, None, false)
    }
    fn del(p: &str) -> ChangeEvt {
        ev(ChangeKind::Deleted, p, None, false)
    }
    fn rn(from: &str, to: &str) -> ChangeEvt {
        ev(ChangeKind::Renamed, to, Some(from), false)
    }

    #[test]
    fn create_then_delete_cancels() {
        let d = coalesce(&[cr("C:\\a\\x.txt"), md("C:\\a\\x.txt"), del("C:\\A\\X.TXT")]);
        assert!(d.is_empty());
        assert!(d.dirs.is_empty());
    }

    #[test]
    fn delete_after_modify_wins() {
        let d = coalesce(&[md("C:\\a\\x.txt"), md("C:\\a\\x.txt"), del("C:\\a\\x.txt")]);
        assert_eq!(d.removed, vec!["C:\\a\\x.txt"]);
        assert!(d.modified.is_empty());
        assert_eq!(d.dirs, vec!["C:\\a"]);
    }

    #[test]
    fn modifies_collapse_and_case_insensitive() {
        let d = coalesce(&[md("C:\\a\\x.txt"), md("c:\\A\\X.txt"), md("C:\\a\\x.txt")]);
        assert_eq!(d.modified, vec!["C:\\a\\x.txt"]);
    }

    #[test]
    fn delete_then_create_emits_both() {
        let d = coalesce(&[del("C:\\a\\x"), cr("C:\\a\\x")]);
        assert_eq!(d.removed, vec!["C:\\a\\x"]);
        assert_eq!(d.added, vec!["C:\\a\\x"]);
    }

    #[test]
    fn rename_chain() {
        let d = coalesce(&[rn("C:\\a", "C:\\b"), rn("C:\\b", "C:\\c")]);
        assert_eq!(
            d.renamed,
            vec![Renamed {
                from: "C:\\a".into(),
                to: "C:\\c".into()
            }]
        );
    }

    #[test]
    fn rename_then_delete_removes_original() {
        let d = coalesce(&[rn("C:\\d\\a", "C:\\d\\b"), del("C:\\d\\b")]);
        assert_eq!(d.removed, vec!["C:\\d\\a"]);
        assert!(d.renamed.is_empty());
    }

    #[test]
    fn create_then_rename_is_added_at_new_name() {
        let d = coalesce(&[cr("C:\\d\\a"), rn("C:\\d\\a", "C:\\d\\b")]);
        assert_eq!(d.added, vec!["C:\\d\\b"]);
        assert!(d.renamed.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn rename_back_is_noop() {
        let d = coalesce(&[rn("C:\\d\\a", "C:\\d\\b"), rn("C:\\d\\b", "C:\\d\\a")]);
        assert!(d.is_empty());
    }

    #[test]
    fn rename_without_old_path_is_added() {
        let d = coalesce(&[ev(ChangeKind::Renamed, "C:\\d\\n", None, false)]);
        assert_eq!(d.added, vec!["C:\\d\\n"]);
    }

    #[test]
    fn dir_delete_listed_without_children_and_cancels_inner_work() {
        let d = coalesce(&[
            cr("C:\\p\\new.txt"),
            md("C:\\p\\old.txt"),
            ev(ChangeKind::Deleted, "C:\\p", None, true),
            md("C:\\pq\\keep.txt"),
        ]);
        assert_eq!(d.removed, vec!["C:\\p"]);
        assert_eq!(d.modified, vec!["C:\\pq\\keep.txt"]);
        assert!(d.added.is_empty());
        assert_eq!(d.dirs, vec!["C:\\", "C:\\pq"]);
    }

    #[test]
    fn dir_modified_dropped_dir_rename_kept() {
        let d = coalesce(&[
            ev(ChangeKind::Modified, "C:\\p", None, true),
            ev(ChangeKind::Renamed, "C:\\q", Some("C:\\p"), true),
        ]);
        assert!(d.modified.is_empty());
        assert_eq!(d.renamed.len(), 1);
        assert_eq!(d.dirs, vec!["C:\\"]);
    }

    #[test]
    fn dirs_deduped_with_rename_parents() {
        let d = coalesce(&[cr("C:\\a\\1"), cr("c:\\A\\2"), rn("C:\\x\\f", "C:\\y\\f")]);
        assert_eq!(d.dirs, vec!["C:\\a", "C:\\x", "C:\\y"]);
    }

    #[test]
    fn entry_cap_overflows() {
        let evs: Vec<_> = (0..2001).map(|i| cr(&format!("C:\\a\\{i}"))).collect();
        let d = coalesce(&evs);
        assert!(d.overflow && d.added.is_empty() && d.dirs.is_empty());
        let ok: Vec<_> = (0..2000).map(|i| cr(&format!("C:\\a\\{i}"))).collect();
        assert!(!coalesce(&ok).overflow);
    }

    #[test]
    fn dirs_cap_overflows() {
        let evs: Vec<_> = (0..501).map(|i| cr(&format!("C:\\d{i}\\f"))).collect();
        assert!(coalesce(&evs).overflow);
    }

    #[test]
    fn json_shape() {
        let d = coalesce(&[
            del("C:\\a\\x"),
            rn("C:\\b\\o", "C:\\b\\n"),
            cr("C:\\c\\z"),
            md("C:\\c\\m"),
        ]);
        let j = serde_json::to_string(&d).unwrap();
        assert_eq!(
            j,
            r#"{"removed":["C:\\a\\x"],"renamed":[{"from":"C:\\b\\o","to":"C:\\b\\n"}],"added":["C:\\c\\z"],"modified":["C:\\c\\m"],"dirs":["C:\\a","C:\\c","C:\\b"],"overflow":false}"#
        );
    }

    #[test]
    fn batcher_window_quiet_and_cap() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut b = Batcher::default();
        assert!(b.deadline().is_none() && !b.due(ms(1000)));
        b.push(cr("C:\\a\\1"), ms(0));
        assert!(!b.due(ms(149)));
        assert!(b.due(ms(150)));
        // churn extends the quiet window but never past the cap
        b.push(cr("C:\\a\\2"), ms(100));
        assert_eq!(b.deadline(), Some(ms(250)));
        b.push(cr("C:\\a\\3"), ms(350));
        assert_eq!(b.deadline(), Some(ms(400)));
        let d = b.take().unwrap();
        assert_eq!(d.added.len(), 3);
        assert!(b.is_empty() && b.deadline().is_none());
    }

    #[test]
    fn batcher_empty_result_is_none() {
        let mut b = Batcher::default();
        let t = Instant::now();
        b.push(cr("C:\\a\\1"), t);
        b.push(del("C:\\a\\1"), t);
        assert!(b.take().is_none());
    }

    /// Live check against a running searchd (run not elevated): prints deltas for 30 s.
    #[test]
    #[ignore]
    fn live_prints_deltas() {
        let stop = AtomicBool::new(false);
        let end = Instant::now() + Duration::from_secs(30);
        std::thread::scope(|s| {
            s.spawn(|| {
                while Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(200));
                }
                stop.store(true, Ordering::Relaxed);
            });
            run(
                |d| println!("{}", serde_json::to_string(&d).unwrap()),
                &stop,
            );
        });
    }
}
