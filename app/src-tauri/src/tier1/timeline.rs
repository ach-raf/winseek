//! The Library timeline's photo list, kept here instead of in the page.
//!
//! A library of a million photos as JSON objects in the WebView cost ~2 GB. The page now only holds
//! what is on screen: this module keeps the sorted list (newest first) and the current *view* of it
//! (view filter, name filter, hidden files), answers with one summary per month (count and bytes)
//! and hands out rows by absolute index in small ranges. A view has a `rev`; a range or pick for an
//! older `rev` answers `None`, so a stale page can never mix rows of two different lists.
//!
//! User level only (Tier 1): it reads the same classified image list `library_images` does.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use super::imgclass::ImgClass;
use super::library::ClassedImage;

/// Most rows one range call returns (the page asks for 256).
pub const MAX_ROWS: u64 = 1024;
/// Month-based key / label need the local calendar; the offset is the current UTC offset in minutes.
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

#[derive(Debug, Clone)]
pub struct TlRow {
    pub path: String,
    pub size: u64,
    /// Modified time, unix ms.
    pub mtime: i64,
    pub class: ImgClass,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TlRowDto {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TlSection {
    /// "2025-03"
    pub key: String,
    /// "March 2025"
    pub label: String,
    pub year: i32,
    /// 0-11
    pub month: u32,
    pub count: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct TlCounts {
    pub all: u64,
    pub photos: u64,
    pub screenshots: u64,
    pub graphics: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TlSummary {
    pub rev: u64,
    pub total: u64,
    /// Per view, over everything not hidden (the tab badges).
    pub counts: TlCounts,
    pub sections: Vec<TlSection>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TlPick {
    pub count: u64,
    pub bytes: u64,
    /// Only when asked for (a recycle needs them; the size bar does not).
    pub paths: Vec<String>,
}

#[derive(Default)]
struct Inner {
    /// Bumped by every load; a view built from an older load is refused.
    load: u64,
    rev: u64,
    base: Vec<TlRow>,
    /// Indices into `base` of the current view, in order.
    order: Vec<u32>,
}

/// Tauri managed state.
#[derive(Default, Clone)]
pub struct TimelineState(Arc<Mutex<Inner>>);

fn lock(m: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn in_view(class: ImgClass, view: &str) -> bool {
    match view {
        "photos" => class == ImgClass::Photo,
        "screenshots" => class == ImgClass::Screenshot,
        "graphics" => class == ImgClass::Graphic,
        _ => class != ImgClass::Graphic, // "all": everything of yours
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Year and month (0-11) of a unix-ms time shifted by `offset_min` (civil-from-days, no calendar crate).
pub fn year_month(ms: i64, offset_min: i32) -> (i32, u32) {
    let local = ms + i64::from(offset_min) * 60_000;
    let days = local.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // 1-12
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, (m - 1) as u32)
}

/// Current UTC offset in minutes (`GetTimeZoneInformation`'s Bias is UTC = local + bias).
pub fn local_offset_min() -> i32 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tzi = TIME_ZONE_INFORMATION::default();
    // SAFETY: out-pointer to a local struct; only the Bias field is read.
    let _ = unsafe { GetTimeZoneInformation(&mut tzi) };
    -tzi.Bias
}

/// The view of `base` for (`view`, `q`, `hidden`): kept indices, tab counts and month sections.
/// `base` is sorted newest first, so the sections come out in order with one pass.
pub fn build_view(
    base: &[TlRow],
    view: &str,
    q: &str,
    hidden: &HashSet<&str>,
    offset_min: i32,
) -> (Vec<u32>, TlCounts, Vec<TlSection>) {
    let q = q.trim().to_lowercase();
    let mut order = Vec::new();
    let mut counts = TlCounts::default();
    let mut sections: Vec<TlSection> = Vec::new();
    let mut cur: Option<(i32, u32)> = None;
    for (i, r) in base.iter().enumerate() {
        if hidden.contains(r.path.as_str()) {
            continue;
        }
        match r.class {
            ImgClass::Graphic => counts.graphics += 1,
            ImgClass::Photo => {
                counts.all += 1;
                counts.photos += 1;
            }
            ImgClass::Screenshot => {
                counts.all += 1;
                counts.screenshots += 1;
            }
        }
        if !in_view(r.class, view) {
            continue;
        }
        if !q.is_empty() && !file_name(&r.path).to_lowercase().contains(&q) {
            continue;
        }
        let ym = year_month(r.mtime, offset_min);
        if cur != Some(ym) {
            cur = Some(ym);
            sections.push(TlSection {
                key: format!("{}-{:02}", ym.0, ym.1 + 1),
                label: format!("{} {}", MONTHS[ym.1 as usize], ym.0),
                year: ym.0,
                month: ym.1,
                count: 0,
                bytes: 0,
            });
        }
        let s = sections.last_mut().expect("a section was just pushed");
        s.count += 1;
        s.bytes += r.size;
        order.push(i as u32);
    }
    (order, counts, sections)
}

impl TimelineState {
    /// Replace the base list (newest first) and return the id of this load.
    pub fn load(&self, images: Vec<ClassedImage>) -> u64 {
        let mut base: Vec<TlRow> = images
            .into_iter()
            .map(|c| TlRow {
                path: c.row.path,
                size: c.row.size,
                mtime: c.row.mtime,
                class: c.class,
            })
            .collect();
        base.sort_by_key(|r| std::cmp::Reverse(r.mtime));
        let mut g = lock(&self.0);
        g.load += 1;
        g.base = base;
        g.order = Vec::new();
        g.load
    }

    /// Build the view; `None` when `load` is not the current load (the page re-reads the base).
    pub fn view(
        &self,
        load: u64,
        view: &str,
        q: &str,
        hidden: &[String],
        offset_min: i32,
    ) -> Option<TlSummary> {
        let mut g = lock(&self.0);
        if g.load != load {
            return None;
        }
        let hidden: HashSet<&str> = hidden.iter().map(String::as_str).collect();
        let (order, counts, sections) = build_view(&g.base, view, q, &hidden, offset_min);
        g.rev += 1;
        let total = order.len() as u64;
        g.order = order;
        Some(TlSummary {
            rev: g.rev,
            total,
            counts,
            sections,
        })
    }

    /// Rows [start, start + count) of the view; `None` for a stale `rev`.
    pub fn rows(&self, rev: u64, start: u64, count: u64) -> Option<Vec<TlRowDto>> {
        let g = lock(&self.0);
        if g.rev != rev {
            return None;
        }
        let end = start
            .saturating_add(count.min(MAX_ROWS))
            .min(g.order.len() as u64);
        Some(
            (start.min(end)..end)
                .map(|k| {
                    let r = &g.base[g.order[k as usize] as usize];
                    TlRowDto {
                        path: r.path.clone(),
                        size: r.size,
                        mtime: r.mtime,
                    }
                })
                .collect(),
        )
    }

    /// Count, total bytes (and paths when asked) of inclusive index ranges; `None` for a stale `rev`.
    pub fn pick(&self, rev: u64, ranges: &[[u64; 2]], with_paths: bool) -> Option<TlPick> {
        let g = lock(&self.0);
        if g.rev != rev {
            return None;
        }
        let n = g.order.len() as u64;
        let mut out = TlPick {
            count: 0,
            bytes: 0,
            paths: Vec::new(),
        };
        for &[a, b] in ranges {
            if n == 0 || a > b || a >= n {
                continue;
            }
            for k in a..=b.min(n - 1) {
                let r = &g.base[g.order[k as usize] as usize];
                out.count += 1;
                out.bytes += r.size;
                if with_paths {
                    out.paths.push(r.path.clone());
                }
            }
        }
        Some(out)
    }
}

// ---------------------------------------------------------------- Tauri commands

use super::Handle;

#[tauri::command]
pub async fn library_timeline_load(
    h: tauri::State<'_, Handle>,
    tl: tauri::State<'_, TimelineState>,
) -> Result<u64, String> {
    let t = h.get()?;
    let tl = tl.inner().clone();
    tauri::async_runtime::spawn_blocking(move || Ok(tl.load(t.classed_images()?)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_timeline_view(
    tl: tauri::State<'_, TimelineState>,
    load: u64,
    view: String,
    q: String,
    hidden: Vec<String>,
) -> Result<Option<TlSummary>, String> {
    let tl = tl.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        Ok(tl.view(load, &view, &q, &hidden, local_offset_min()))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn library_timeline_rows(
    tl: tauri::State<'_, TimelineState>,
    rev: u64,
    start: u64,
    count: u64,
) -> Option<Vec<TlRowDto>> {
    tl.rows(rev, start, count)
}

#[tauri::command]
pub async fn library_timeline_pick(
    tl: tauri::State<'_, TimelineState>,
    rev: u64,
    ranges: Vec<[u64; 2]>,
    with_paths: bool,
) -> Result<Option<TlPick>, String> {
    let tl = tl.inner().clone();
    tauri::async_runtime::spawn_blocking(move || Ok(tl.pick(rev, &ranges, with_paths)))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    fn row(path: &str, mtime: i64, class: ImgClass) -> TlRow {
        TlRow {
            path: path.into(),
            size: 10,
            mtime,
            class,
        }
    }

    fn state(rows: Vec<TlRow>) -> TimelineState {
        let s = TimelineState::default();
        let mut g = lock(&s.0);
        g.load = 1;
        g.base = rows;
        drop(g);
        s
    }

    #[test]
    fn year_month_matches_known_dates() {
        assert_eq!(year_month(0, 0), (1970, 0));
        // 2024-02-29 12:00 UTC, a leap day
        assert_eq!(year_month(1_709_208_000_000, 0), (2024, 1));
        // 2025-03-01 00:30 UTC is still February in UTC-1, March in UTC
        let t = 1_740_789_000_000;
        assert_eq!(year_month(t, 0), (2025, 2));
        assert_eq!(year_month(t, -60), (2025, 1));
        // 2025-12-31 23:30 UTC rolls into January 2026 at UTC+1
        assert_eq!(year_month(1_767_223_800_000, 60), (2026, 0));
        // before the epoch
        assert_eq!(year_month(-DAY, 0), (1969, 11));
    }

    #[test]
    fn sections_group_newest_first_with_counts_and_bytes() {
        let base = vec![
            row(r"C:\p\c.jpg", 1_740_789_000_000, ImgClass::Photo), // 2025-03
            row(r"C:\p\b.jpg", 1_740_789_000_000 - DAY, ImgClass::Screenshot), // 2025-02
            row(r"C:\p\a.jpg", 1_740_789_000_000 - 2 * DAY, ImgClass::Photo), // 2025-02
        ];
        let (order, counts, secs) = build_view(&base, "all", "", &HashSet::new(), 0);
        assert_eq!(order, vec![0, 1, 2]);
        assert_eq!(secs.len(), 2);
        assert_eq!(
            (secs[0].key.as_str(), secs[0].label.as_str(), secs[0].count),
            ("2025-03", "March 2025", 1)
        );
        assert_eq!(
            (secs[1].key.as_str(), secs[1].count, secs[1].bytes),
            ("2025-02", 2, 20)
        );
        assert_eq!((secs[1].year, secs[1].month), (2025, 1));
        assert_eq!(
            counts,
            TlCounts {
                all: 3,
                photos: 2,
                screenshots: 1,
                graphics: 0
            }
        );
    }

    #[test]
    fn views_name_filter_and_hidden() {
        let base = vec![
            row(r"C:\p\IMG_1.jpg", 3 * DAY, ImgClass::Photo),
            row(r"C:\p\Screenshot 2.png", 2 * DAY, ImgClass::Screenshot),
            row(r"C:\p\icon.png", DAY, ImgClass::Graphic),
        ];
        let none = HashSet::new();
        let v = |view: &str, q: &str, h: &HashSet<&str>| build_view(&base, view, q, h, 0);
        assert_eq!(
            v("all", "", &none).0,
            vec![0, 1],
            "graphics stay out of All"
        );
        assert_eq!(v("photos", "", &none).0, vec![0]);
        assert_eq!(v("screenshots", "", &none).0, vec![1]);
        assert_eq!(v("graphics", "", &none).0, vec![2]);
        assert_eq!(
            v("all", " SCREENSHOT ", &none).0,
            vec![1],
            "case-insensitive, trimmed"
        );
        assert_eq!(
            v("all", "p\\", &none).0,
            Vec::<u32>::new(),
            "the filter matches the file name, not the folder"
        );
        let hid: HashSet<&str> = [r"C:\p\IMG_1.jpg"].into();
        let (order, counts, _) = v("all", "", &hid);
        assert_eq!(order, vec![1]);
        assert_eq!(counts.all, 1, "hidden files leave the tab badges too");
    }

    #[test]
    fn rows_and_pick_follow_the_view_and_refuse_a_stale_rev() {
        let s = state(
            (0..10)
                .map(|i| row(&format!(r"C:\p\{i}.jpg"), (100 - i) * DAY, ImgClass::Photo))
                .collect(),
        );
        let sum = s
            .view(1, "all", "", &[r"C:\p\3.jpg".to_string()], 0)
            .unwrap();
        assert_eq!(sum.total, 9);
        let rows = s.rows(sum.rev, 2, 3).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            [r"C:\p\2.jpg", r"C:\p\4.jpg", r"C:\p\5.jpg"],
            "hidden rows are skipped, indices are of the view"
        );
        assert_eq!(
            s.rows(sum.rev, 7, 50).unwrap().len(),
            2,
            "clamped at the end"
        );
        assert!(s.rows(sum.rev, 99, 5).unwrap().is_empty());
        let pick = s.pick(sum.rev, &[[0, 1], [7, 99]], true).unwrap();
        assert_eq!((pick.count, pick.bytes), (4, 40));
        assert_eq!(pick.paths.len(), 4);
        assert!(s.pick(sum.rev, &[[0, 0]], false).unwrap().paths.is_empty());
        // a newer view makes the old rev stale
        let sum2 = s.view(1, "all", "", &[], 0).unwrap();
        assert!(sum2.rev > sum.rev);
        assert!(s.rows(sum.rev, 0, 5).is_none());
        assert!(s.pick(sum.rev, &[[0, 0]], false).is_none());
        // and a view built from an old load is refused
        assert!(s.view(0, "all", "", &[], 0).is_none());
    }

    #[test]
    fn row_count_is_capped() {
        let s = state(
            (0..(MAX_ROWS as i64 + 10))
                .map(|i| row(&format!(r"C:\p\{i}.jpg"), 1_000_000 - i, ImgClass::Photo))
                .collect(),
        );
        let sum = s.view(1, "all", "", &[], 0).unwrap();
        assert_eq!(
            s.rows(sum.rev, 0, 1_000_000).unwrap().len() as u64,
            MAX_ROWS
        );
    }
}
