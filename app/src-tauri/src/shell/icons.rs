//! `wsicon:` protocol: shell icons and thumbnails as PNG, rendered on worker threads (never the main thread).

use super::{shell_item, wide, ComGuard};
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::http::{Request, Response};
use tauri::UriSchemeResponder;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
    SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES, SIIGBF, SIIGBF_ICONONLY, SIIGBF_INCACHEONLY,
    SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

/// In-memory cache budget, in encoded bytes (a 48 px tier is ~2 KB, a 256 px thumbnail ~15-30 KB).
const CACHE_BUDGET: usize = 64 * 1024 * 1024;
const WORKERS: usize = 4;
/// Workers that serve only the tiny lane, on top of the general ones: a small tier never waits
/// behind 512 px extractions.
const TINY_WORKERS: usize = 2;
/// Requests up to this size go to the tiny lane; thumbnails up to it are served from one tier.
const TINY_MAX: u32 = 48;
/// The always-available low-res thumbnail tier. 48 rather than 32: the list rows ask for `s=40`
/// thumbnails, and a 32 px tier would upscale them.
const TINY_PX: u32 = 48;
/// A miss rendered at least this sharp also writes the tiny tier from the same decode.
const TINY_FROM_PX: u32 = 96;
const TINY_JPEG_Q: u8 = 70;
const THUMB_JPEG_Q: u8 = 82;
/// Ask Windows' own thumbnail cache (`SIIGBF_INCACHEONLY`) before a full extraction, for sizes up to
/// this. The cache-only call never extracts, so a miss costs one quick failed call.
const INCACHE_MAX: u32 = 256;
const USE_INCACHEONLY: bool = true;
/// Extensions whose icon is inside the file: rendered per path, never cached by extension.
const PER_PATH_EXT: &[&str] = &["exe", "ico", "lnk", "url", "cur", "ani", "msc", "appref-ms"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    /// Sizes up to `TINY_MAX` (icons, the low-res thumbnail tier): served first, by every worker.
    Tiny = 0,
    /// What the screen waits for.
    Hi = 1,
    /// Idle prefetch, only served when the other lanes are empty.
    Lo = 2,
}

const LANE_NAMES: [&str; 3] = ["tiny", "hi", "lo"];

struct Job {
    path: String,
    /// Store/Start-menu app id — icon fetched through `shell:AppsFolder\<aumid>` (launcher).
    aumid: Option<String>,
    is_dir: bool,
    size: u32,
    thumb: bool,
    /// Idle prefetch (`w=1`): render into the caches, answer 204 without a body.
    warm: bool,
    /// The URL carries a version (`v=`, the file's mtime): the answer never changes.
    immutable: bool,
    queued: Instant,
    responder: UriSchemeResponder,
}

impl Job {
    fn lane(&self) -> Lane {
        lane_for(self.warm, self.size)
    }
}

fn lane_for(warm: bool, size: u32) -> Lane {
    if warm {
        Lane::Lo
    } else if size <= TINY_MAX {
        Lane::Tiny
    } else {
        Lane::Hi
    }
}

/// Waiting jobs. The newest is served first: the tile requested last is the one under the viewport, older ones
/// belong to rows already scrolled past (the page also holds most requests back, see thumbQueue.ts).
struct Queue {
    jobs: Mutex<Jobs>,
    /// General workers wait here.
    cv: Condvar,
    /// Tiny-lane workers wait here (one condvar would let a tiny-only worker swallow a wake-up meant for a hi job).
    cv_tiny: Condvar,
}

#[derive(Default)]
struct Jobs {
    tiny: VecDeque<Job>,
    hi: VecDeque<Job>,
    lo: VecDeque<Job>,
}

/// Waiting jobs kept; beyond this the oldest are answered with 503 (the page does not cache that).
const MAX_WAITING: usize = 512;
/// Prefetch jobs kept: the page sends its newest window, so older ones are stale.
const MAX_WARM: usize = 192;
/// Cancellation stand-in: the responder carries no abort signal, so a job that waited longer than this in a lane
/// more than half full is answered 503 unrendered (its tile has most likely scrolled away; the page re-requests
/// visible tiles).
const STALE_AFTER: Duration = Duration::from_secs(3);

static POOL: OnceLock<Arc<Queue>> = OnceLock::new();

/// Encoded images, insertion order kept alongside: past the byte budget the oldest entries are
/// evicted one by one (the old wipe-everything made every visible icon re-render at once — a
/// latency spike mid-scroll).
#[derive(Default)]
struct IconCache {
    map: HashMap<String, Arc<Vec<u8>>>,
    order: VecDeque<String>,
    bytes: usize,
}

/// Insert into the cache, evicting oldest entries until it fits `budget` bytes. The only place that
/// mutates `IconCache`, so map, order and byte count can never drift apart.
fn cache_insert(c: &mut IconCache, key: String, png: Arc<Vec<u8>>, budget: usize) {
    if let Some(old) = c.map.get_mut(&key) {
        // Two workers rendered the same key: replace in place, keep its age.
        c.bytes = c.bytes - old.len() + png.len();
        *old = png;
        return;
    }
    while c.bytes + png.len() > budget && !c.map.is_empty() {
        match c.order.pop_front() {
            Some(old) => {
                if let Some(v) = c.map.remove(&old) {
                    c.bytes -= v.len();
                }
            }
            None => {
                // Order lost (cannot happen: every insert goes through here) — start over.
                c.map.clear();
                c.bytes = 0;
                break;
            }
        }
    }
    c.bytes += png.len();
    c.map.insert(key.clone(), png);
    c.order.push_back(key);
}

static CACHE: Mutex<Option<IconCache>> = Mutex::new(None);

fn cache_get(key: &str) -> Option<Arc<Vec<u8>>> {
    CACHE.lock().ok()?.as_ref()?.map.get(key).cloned()
}

fn cache_put(key: String, v: Arc<Vec<u8>>) {
    if let Ok(mut g) = CACHE.lock() {
        cache_insert(
            g.get_or_insert_with(IconCache::default),
            key,
            v,
            CACHE_BUDGET,
        );
    }
}

/// Decode workers: cores - 1 (the UI keeps one), between 4 and 8.
fn workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .saturating_sub(1)
        .clamp(WORKERS, 8)
}

/// Take the jobs that waited longer than `after` off the old end of a lane, but only while the lane is more than
/// half of `cap` full (a short queue is worth finishing).
fn drain_stale<T>(
    lane: &mut VecDeque<T>,
    cap: usize,
    now: Instant,
    after: Duration,
    queued: impl Fn(&T) -> Instant,
) -> Vec<T> {
    let mut out = Vec::new();
    while lane.len() > cap / 2 {
        match lane.front() {
            Some(j) if now.saturating_duration_since(queued(j)) > after => {
                out.extend(lane.pop_front());
            }
            _ => break,
        }
    }
    out
}

fn next_job(q: &Queue, tiny_only: bool) -> Option<Job> {
    let mut dead = Vec::new();
    let job = {
        let mut g = q.jobs.lock().ok()?;
        loop {
            let now = Instant::now();
            dead.extend(drain_stale(
                &mut g.tiny,
                MAX_WAITING,
                now,
                STALE_AFTER,
                |j: &Job| j.queued,
            ));
            if !tiny_only {
                dead.extend(drain_stale(
                    &mut g.hi,
                    MAX_WAITING,
                    now,
                    STALE_AFTER,
                    |j: &Job| j.queued,
                ));
                dead.extend(drain_stale(
                    &mut g.lo,
                    MAX_WARM,
                    now,
                    STALE_AFTER,
                    |j: &Job| j.queued,
                ));
            }
            let j = if tiny_only {
                g.tiny.pop_back()
            } else {
                g.tiny
                    .pop_back()
                    .or_else(|| g.hi.pop_back())
                    .or_else(|| g.lo.pop_back())
            };
            if let Some(j) = j {
                break j;
            }
            g = if tiny_only { &q.cv_tiny } else { &q.cv }.wait(g).ok()?;
        }
    };
    if !dead.is_empty() {
        STATS_STALE.fetch_add(dead.len() as u64, Ordering::Relaxed);
        for d in dead {
            d.responder.respond(busy_response());
        }
    }
    Some(job)
}

fn run_job(job: Job) {
    let lane = job.lane();
    let waited = job.queued.elapsed();
    let t = Instant::now();
    let png = match &job.aumid {
        Some(aumid) => render_aumid(aumid, job.size),
        None => render(&job.path, job.is_dir, job.size, job.thumb),
    };
    let rendered = t.elapsed();
    if job.warm {
        job.responder.respond(warm_response());
    } else {
        job.responder.respond(response(png, job.immutable));
    }
    stats_record(lane, waited, rendered);
}

fn pool() -> &'static Arc<Queue> {
    POOL.get_or_init(|| {
        let q = Arc::new(Queue {
            jobs: Mutex::new(Jobs::default()),
            cv: Condvar::new(),
            cv_tiny: Condvar::new(),
        });
        let general = workers();
        for i in 0..general + TINY_WORKERS {
            let q = q.clone();
            let tiny_only = i >= general;
            let _ = std::thread::Builder::new()
                .name(format!("wsicon-{}{i}", if tiny_only { "t" } else { "" }))
                .spawn(move || {
                    let _com = ComGuard::init();
                    while let Some(job) = next_job(&q, tiny_only) {
                        run_job(job);
                    }
                });
        }
        q
    })
}

fn submit(job: Job) {
    let q = pool();
    let lane = job.lane();
    let dropped = match q.jobs.lock() {
        Ok(mut g) => {
            let (l, cap) = match lane {
                Lane::Tiny => (&mut g.tiny, MAX_WAITING),
                Lane::Hi => (&mut g.hi, MAX_WAITING),
                Lane::Lo => (&mut g.lo, MAX_WARM),
            };
            l.push_back(job);
            if l.len() > cap {
                l.pop_front()
            } else {
                None
            }
        }
        Err(_) => Some(job),
    };
    q.cv.notify_one();
    if lane == Lane::Tiny {
        q.cv_tiny.notify_one();
    }
    if let Some(old) = dropped {
        old.responder.respond(busy_response());
    }
}

// ---- timing log -------------------------------------------------------------------------------------------------
// Queue wait and render time per lane, summarised to stderr every minute (or every 2000 jobs) while busy.

#[derive(Default)]
struct LaneStats {
    /// Microseconds.
    wait: Vec<u32>,
    render: Vec<u32>,
}

struct Stats {
    lanes: [LaneStats; 3],
    since: Instant,
}

static STATS: Mutex<Option<Stats>> = Mutex::new(None);
static STATS_FAST_HITS: AtomicU64 = AtomicU64::new(0);
static STATS_STALE: AtomicU64 = AtomicU64::new(0);
const STATS_EVERY: Duration = Duration::from_secs(60);
const STATS_MAX_SAMPLES: usize = 2000;

/// p50, p90, p99 and max of `v` (sorted in place); zeros when empty.
fn percentiles(v: &mut [u32]) -> [u32; 4] {
    if v.is_empty() {
        return [0; 4];
    }
    v.sort_unstable();
    let at = |p: usize| v[(v.len() - 1) * p / 100];
    [at(50), at(90), at(99), v[v.len() - 1]]
}

fn stats_record(lane: Lane, waited: Duration, rendered: Duration) {
    let us = |d: Duration| d.as_micros().min(u32::MAX as u128) as u32;
    let taken = {
        let Ok(mut g) = STATS.lock() else {
            return;
        };
        let s = g.get_or_insert_with(|| Stats {
            lanes: Default::default(),
            since: Instant::now(),
        });
        let l = &mut s.lanes[lane as usize];
        l.wait.push(us(waited));
        l.render.push(us(rendered));
        let n: usize = s.lanes.iter().map(|l| l.wait.len()).sum();
        if n < STATS_MAX_SAMPLES && s.since.elapsed() < STATS_EVERY {
            return;
        }
        s.since = Instant::now();
        std::mem::take(&mut s.lanes)
    };
    let fast = STATS_FAST_HITS.swap(0, Ordering::Relaxed);
    let stale = STATS_STALE.swap(0, Ordering::Relaxed);
    let mut line = format!("winseek: wsicon fast-hits={fast} stale-503={stale}");
    for (i, mut l) in taken.into_iter().enumerate() {
        if l.wait.is_empty() {
            continue;
        }
        let ms = |v: [u32; 4]| v.map(|x| format!("{:.1}", x as f64 / 1000.0)).join("/");
        line.push_str(&format!(
            " | {} n={} wait-ms p50/90/99/max={} render-ms={}",
            LANE_NAMES[i],
            l.wait.len(),
            ms(percentiles(&mut l.wait)),
            ms(percentiles(&mut l.render)),
        ));
    }
    eprintln!("{line}");
}

fn busy_response() -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "no-store")
        .status(503)
        .body(Cow::Borrowed(&[][..]))
        .expect("static response")
}

fn warm_response() -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "no-store")
        .status(204)
        .body(Cow::Borrowed(&[][..]))
        .expect("static response")
}

fn cache_control(immutable: bool) -> &'static str {
    if immutable {
        "public, max-age=31536000, immutable"
    } else {
        "max-age=3600"
    }
}

fn response(png: Option<Arc<Vec<u8>>>, immutable: bool) -> Response<Cow<'static, [u8]>> {
    let b = Response::builder().header("Access-Control-Allow-Origin", "*");
    match png {
        Some(p) => b
            .status(200)
            .header(
                "Content-Type",
                if p.starts_with(&[0xFF, 0xD8]) {
                    "image/jpeg"
                } else {
                    "image/png"
                },
            )
            .header("Cache-Control", cache_control(immutable))
            .body(Cow::Owned(p.as_ref().clone())),
        None => b.status(404).body(Cow::Borrowed(&[][..])),
    }
    .expect("static response")
}

pub(crate) fn percent_decode(s: &str) -> String {
    // Invalid escapes (%zz, a trailing %2) stay literal, like the hand-rolled loop it replaces.
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

pub(crate) fn query_param(q: &str, key: &str) -> Option<String> {
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

/// A parsed `wsicon:` request.
#[derive(Debug, PartialEq)]
struct Req {
    path: String,
    aumid: Option<String>,
    is_dir: bool,
    size: u32,
    thumb: bool,
    warm: bool,
    immutable: bool,
}

fn parse(q: &str) -> Option<Req> {
    let size = query_param(q, "s")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(32)
        .clamp(16, 1024);
    let immutable = query_param(q, "v").is_some_and(|v| !v.is_empty());
    if let Some(aumid) = query_param(q, "a").filter(|a| !a.is_empty()) {
        return Some(Req {
            path: String::new(),
            aumid: Some(aumid),
            is_dir: false,
            size,
            thumb: false,
            warm: false,
            immutable,
        });
    }
    let path = query_param(q, "p").filter(|p| !p.is_empty())?;
    let is_dir = query_param(q, "d").as_deref() == Some("1");
    let thumb = query_param(q, "t").as_deref() == Some("1");
    // Prefetch only makes sense for thumbnails (icons are cached by extension and cheap).
    let warm = thumb && query_param(q, "w").as_deref() == Some("1");
    // Every small thumbnail is served from the one low-res tier, which sharper renders keep filled.
    let size = if thumb && size <= TINY_MAX {
        TINY_PX
    } else {
        size
    };
    Some(Req {
        path,
        aumid: None,
        is_dir,
        size,
        thumb,
        warm,
        immutable,
    })
}

/// Memory-cache key: generic by extension, or per path for exe/lnk/thumbnails and app ids.
fn mem_key(path: &str, aumid: Option<&str>, is_dir: bool, size: u32, thumb: bool) -> String {
    if let Some(a) = aumid {
        return format!("a:{a}:{size}");
    }
    let ext = ext_of(path);
    if thumb || (!is_dir && PER_PATH_EXT.contains(&ext.as_str())) {
        format!("p:{}:{size}:{}", path.to_lowercase(), thumb as u8)
    } else if is_dir {
        format!("dir:{size}")
    } else {
        format!("x:{ext}:{size}")
    }
}

/// Cached answer without COM or extraction: the memory cache, then (thumbnails) the disk cache. Runs on the
/// protocol thread, so cache hits never queue behind slow extractions.
fn fast_lookup(r: &Req) -> Option<Arc<Vec<u8>>> {
    let key = mem_key(&r.path, r.aumid.as_deref(), r.is_dir, r.size, r.thumb);
    if let Some(hit) = cache_get(&key) {
        return Some(hit);
    }
    // Disk: one stat of the picture and one small read. Skipped for network paths, where a stat can stall.
    if !r.thumb || r.size > 512 || r.path.starts_with(r"\\") {
        return None;
    }
    let b = Arc::new(disk_get(thumb_dir()?, &r.path, r.size)?);
    if r.size <= 256 {
        cache_put(key, b.clone());
    }
    Some(b)
}

pub fn handle(req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let Some(r) = parse(req.uri().query().unwrap_or("")) else {
        return responder.respond(response(None, false));
    };
    if let Some(hit) = fast_lookup(&r) {
        STATS_FAST_HITS.fetch_add(1, Ordering::Relaxed);
        return responder.respond(if r.warm {
            warm_response()
        } else {
            response(Some(hit), r.immutable)
        });
    }
    submit(Job {
        path: r.path,
        aumid: r.aumid,
        is_dir: r.is_dir,
        size: r.size,
        thumb: r.thumb,
        warm: r.warm,
        immutable: r.immutable,
        queued: Instant::now(),
        responder,
    });
}

fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Cached PNG for a path (generic by extension, or per path for exe/lnk/thumbnails).
pub(super) fn render(path: &str, is_dir: bool, size: u32, thumb: bool) -> Option<Arc<Vec<u8>>> {
    let ext = ext_of(path);
    let per_path = thumb || (!is_dir && PER_PATH_EXT.contains(&ext.as_str()));
    let key = mem_key(path, None, is_dir, size, thumb);
    if let Some(hit) = cache_get(&key) {
        return Some(hit);
    }
    let png = if thumb && size <= 512 {
        thumb_bytes(path, size)
    } else if per_path {
        path_icon_png(path, size, thumb)
    } else {
        generic_icon_png(&ext, is_dir, size)
    }?;
    let png = Arc::new(png);
    if size > 256 {
        return Some(png); // preview-sized renders are one-offs; keep the cache small
    }
    cache_put(key, png.clone());
    Some(png)
}

/// Cached PNG for a shell app id (Store or Start-menu entry), via `shell:AppsFolder\<aumid>`.
pub(super) fn render_aumid(aumid: &str, size: u32) -> Option<Arc<Vec<u8>>> {
    let key = mem_key("", Some(aumid), false, size, false);
    if let Some(hit) = cache_get(&key) {
        return Some(hit);
    }
    let png = Arc::new(path_icon_png(
        &format!("shell:AppsFolder\\{aumid}"),
        size,
        false,
    )?);
    if size > 256 {
        return Some(png);
    }
    cache_put(key, png.clone());
    Some(png)
}

/// Extension-based icon via SHGFI_USEFILEATTRIBUTES (no disk access).
pub(super) fn generic_icon_png(ext: &str, is_dir: bool, size: u32) -> Option<Vec<u8>> {
    let name = match (is_dir, ext.is_empty()) {
        (true, _) => "folder".to_string(),
        (false, true) => "file".to_string(),
        (false, false) => format!("file.{ext}"),
    };
    let attrs = if is_dir {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    let sz = if size <= 16 {
        SHGFI_SMALLICON
    } else {
        SHGFI_LARGEICON
    };
    let w = wide(&name);
    let mut sfi = SHFILEINFOW::default();
    // SAFETY: `w` is NUL-terminated; `sfi` is a valid out struct of the stated size.
    let r = unsafe {
        SHGetFileInfoW(
            PCWSTR(w.as_ptr()),
            attrs,
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_USEFILEATTRIBUTES | SHGFI_ICON | sz,
        )
    };
    if r == 0 || sfi.hIcon.is_invalid() {
        return None;
    }
    let _icon = IconGuard(sfi.hIcon);
    let (w, h, rgba) = icon_rgba(sfi.hIcon)?;
    encode_png(w, h, &rgba)
}

/// Per-path icon or thumbnail through IShellItemImageFactory, as PNG.
pub(super) fn path_icon_png(path: &str, size: u32, thumb: bool) -> Option<Vec<u8>> {
    let (w, h, px) = path_icon_px(path, size, thumb)?;
    encode_png(w, h, &px)
}

/// Per-path icon or thumbnail through IShellItemImageFactory, as straight-alpha RGBA.
fn path_icon_px(path: &str, size: u32, thumb: bool) -> Option<(u32, u32, Vec<u8>)> {
    shell_px(path, size, if thumb { Shot::Thumb } else { Shot::Icon })
}

#[derive(Clone, Copy, PartialEq)]
enum Shot {
    Icon,
    /// Thumbnail, extracted if Windows' cache does not have it (falls back to the icon).
    Thumb,
    /// Thumbnail only if Windows' thumbnail cache already holds it: never extracts, never waits.
    ThumbCached,
}

fn shot_flags(shot: Shot) -> SIIGBF {
    match shot {
        Shot::Icon => SIIGBF_RESIZETOFIT | SIIGBF_ICONONLY,
        Shot::Thumb => SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY,
        Shot::ThumbCached => SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY | SIIGBF_INCACHEONLY,
    }
}

fn shell_px(path: &str, size: u32, shot: Shot) -> Option<(u32, u32, Vec<u8>)> {
    let item = shell_item(path).ok()?;
    let f: IShellItemImageFactory = item.cast().ok()?;
    let sz = SIZE {
        cx: size as i32,
        cy: size as i32,
    };
    // SAFETY: GetImage returns an owned HBITMAP that BitmapGuard deletes.
    let get = || unsafe {
        let r = f.GetImage(sz, shot_flags(shot));
        if shot == Shot::Thumb {
            r.or_else(|_| f.GetImage(sz, SIIGBF_RESIZETOFIT))
        } else {
            r
        }
    };
    // The shell extracts icons asynchronously and answers E_PENDING until the image is ready;
    // poll until a deadline rather than counting attempts, so slow GetImage calls cannot
    // stretch the total wait past the cap. A cache-only call does not wait at all.
    const E_PENDING: windows::core::HRESULT = windows::core::HRESULT(0x8000000Au32 as i32);
    const ICON_WAIT: std::time::Duration = std::time::Duration::from_millis(500);
    let deadline = std::time::Instant::now()
        + if shot == Shot::ThumbCached {
            Duration::ZERO
        } else {
            ICON_WAIT
        };
    let mut res = get();
    while matches!(&res, Err(e) if e.code() == E_PENDING) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        res = get();
    }
    let hbm = res.ok()?;
    let _g = BitmapGuard(hbm);
    let (w, h, mut px) = bitmap_bgra(hbm)?;
    let mut any = false;
    for p in px.as_chunks_mut::<4>().0.iter_mut() {
        any |= p[3] != 0;
    }
    if !any {
        px.as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|p| p[3] = 255);
    } else {
        // Un-premultiply.
        for p in px.as_chunks_mut::<4>().0.iter_mut() {
            let a = p[3] as u32;
            if a != 0 && a != 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
    }
    bgra_to_rgba(&mut px);
    Some((w, h, px))
}

struct BitmapGuard(HBITMAP);
impl Drop for BitmapGuard {
    fn drop(&mut self) {
        // SAFETY: owned GDI bitmap, deleted exactly once.
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

struct IconGuard(HICON);
impl Drop for IconGuard {
    fn drop(&mut self) {
        // SAFETY: owned icon handle, destroyed exactly once.
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

/// Top-down 32bpp BGRA pixels of a bitmap.
fn bitmap_bgra(hbm: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let mut bm = BITMAP::default();
    // SAFETY: `bm` is a BITMAP of the size passed.
    let n = unsafe {
        GetObjectW(
            hbm.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut c_void),
        )
    };
    if n == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth as u32, bm.bmHeight.unsigned_abs());
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    // SAFETY: `buf` holds w*h*4 bytes as required for a top-down 32bpp DIB; the DC is released.
    let lines = unsafe {
        let dc = GetDC(None);
        let l = GetDIBits(
            dc,
            hbm,
            0,
            h,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);
        l
    };
    (lines != 0).then_some((w, h, buf))
}

fn bgra_to_rgba(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0.iter_mut() {
        p.swap(0, 2);
    }
}

/// HICON -> RGBA. Icons without an alpha channel take their alpha from the AND mask.
fn icon_rgba(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    let mut ii = ICONINFO::default();
    // SAFETY: valid icon handle and out struct; both returned bitmaps are freed below.
    unsafe { GetIconInfo(icon, &mut ii) }.ok()?;
    let color = (!ii.hbmColor.is_invalid()).then(|| BitmapGuard(ii.hbmColor));
    let mask = (!ii.hbmMask.is_invalid()).then(|| BitmapGuard(ii.hbmMask));
    let (w, h, mut px) = bitmap_bgra(color.as_ref()?.0)?;
    if px.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        match mask
            .as_ref()
            .and_then(|m| bitmap_bgra(m.0))
            .filter(|m| m.0 == w && m.1 == h)
        {
            Some((_, _, m)) => {
                for (p, mp) in px
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(m.as_chunks::<4>().0.iter())
                {
                    p[3] = if mp[0] == 0 { 255 } else { 0 };
                }
            }
            None => px
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .for_each(|p| p[3] = 255),
        }
    }
    bgra_to_rgba(&mut px);
    Some((w, h, px))
}

// ---- persistent thumbnail cache ---------------------------------------------------------------------------------
// `%LOCALAPPDATA%\winseek\thumbs\<xx>\<sha256>.img`, keyed by path + mtime + size + requested px, so an edited file
// gets a new entry. Opaque pictures are stored as small JPEGs, pictures with transparency as PNG. The shell's own
// thumbnail cache stays the first source for a miss; this layer saves re-extraction and the per-request encode and
// survives restarts.

const DISK_CAP_BYTES: u64 = 600 * 1024 * 1024;

fn thumb_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("winseek")
            .join("thumbs");
        std::fs::create_dir_all(&d).ok()?;
        spawn_trim(d.clone());
        Some(d)
    })
    .as_ref()
}

/// Disk writes since the last trim, and when that trim started (seconds since `trim_epoch`).
static TRIM_WRITES: AtomicU32 = AtomicU32::new(0);
static TRIM_LAST: AtomicU64 = AtomicU64::new(0);
static TRIMMING: AtomicBool = AtomicBool::new(false);
const TRIM_EVERY_WRITES: u32 = 500;
const TRIM_EVERY_SECS: u64 = 600;

fn trim_epoch() -> &'static Instant {
    static E: OnceLock<Instant> = OnceLock::new();
    E.get_or_init(Instant::now)
}

/// A trim is due after `TRIM_EVERY_WRITES` writes, or after `TRIM_EVERY_SECS` once anything was written.
fn trim_due(writes: u32, secs_since: u64) -> bool {
    writes >= TRIM_EVERY_WRITES || (writes > 0 && secs_since >= TRIM_EVERY_SECS)
}

/// Trim on a thread of its own, at most one at a time.
fn spawn_trim(dir: PathBuf) {
    if TRIMMING.swap(true, Ordering::AcqRel) {
        return;
    }
    TRIM_WRITES.store(0, Ordering::Relaxed);
    TRIM_LAST.store(trim_epoch().elapsed().as_secs(), Ordering::Relaxed);
    let spawned = std::thread::Builder::new()
        .name("wsicon-trim".into())
        .spawn(move || {
            trim_dir(&dir, DISK_CAP_BYTES);
            TRIMMING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        TRIMMING.store(false, Ordering::Release);
    }
}

/// Count one cache write; start a trim when one is due. Only the real cache directory is trimmed.
fn note_disk_write(dir: &Path) {
    let writes = TRIM_WRITES.fetch_add(1, Ordering::Relaxed) + 1;
    let since = trim_epoch()
        .elapsed()
        .as_secs()
        .saturating_sub(TRIM_LAST.load(Ordering::Relaxed));
    if trim_due(writes, since) && thumb_dir().is_some_and(|d| d == dir) {
        spawn_trim(dir.to_path_buf());
    }
}

fn thumb_key(path: &str, mtime_ns: u128, len: u64, size: u32) -> String {
    use sha2::{Digest, Sha256};
    let h =
        Sha256::digest(format!("v1|{}|{mtime_ns}|{len}|{size}", path.to_lowercase()).as_bytes());
    format!("{h:x}")
}

/// The picture's (mtime ns, length): the version half of the cache key.
fn file_stamp(path: &str) -> Option<(u128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let ns = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((ns, meta.len()))
}

fn cache_file(dir: &Path, path: &str, (ns, len): (u128, u64), px: u32) -> PathBuf {
    let key = thumb_key(path, ns, len, px);
    dir.join(&key[..2]).join(format!("{key}.img"))
}

fn read_nonempty(f: &Path) -> Option<Vec<u8>> {
    std::fs::read(f).ok().filter(|b| !b.is_empty())
}

/// Disk-cache hit only (no COM, no decode).
fn disk_get(dir: &Path, path: &str, px: u32) -> Option<Vec<u8>> {
    read_nonempty(&cache_file(dir, path, file_stamp(path)?, px))
}

fn thumb_bytes(path: &str, size: u32) -> Option<Vec<u8>> {
    thumb_bytes_in(thumb_dir().map(|d| d.as_path()), path, size)
}

/// Disk cache, then Windows' thumbnail cache, then a full extraction. A miss rendered at `TINY_FROM_PX` or more
/// also writes the tiny tier (disk + memory) from the same pixels, so the low-res tier is there for every file
/// that ever had a sharp one.
fn thumb_bytes_in(dir: Option<&Path>, path: &str, size: u32) -> Option<Vec<u8>> {
    let stamp = file_stamp(path);
    let file = dir.zip(stamp).map(|(d, s)| cache_file(d, path, s, size));
    if let Some(b) = file.as_deref().and_then(read_nonempty) {
        return Some(b);
    }
    let (w, h, px) = extract_px(path, size)?;
    let bytes = encode_thumb_q(w, h, &px, quality_for(size))?;
    if let Some(f) = &file {
        // Best effort: the thumbnail is already in hand, the cache only speeds up next time.
        if crate::fsutil::write_atomic(f, &bytes).is_ok() {
            note_disk_write(dir.expect("file implies dir"));
        }
    }
    if size >= TINY_FROM_PX {
        write_tiny_tier(dir.zip(stamp), path, w, h, &px);
    }
    Some(bytes)
}

fn quality_for(size: u32) -> u8 {
    if size <= TINY_MAX {
        TINY_JPEG_Q
    } else {
        THUMB_JPEG_Q
    }
}

/// Pixels for a thumbnail: Windows' cache first (cheap, sizes up to `INCACHE_MAX`), then the full extraction,
/// then the `image` crate.
fn extract_px(path: &str, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    if USE_INCACHEONLY && size <= INCACHE_MAX {
        if let Some(r) = shell_px(path, size, Shot::ThumbCached) {
            return Some(r);
        }
    }
    path_icon_px(path, size, true).or_else(|| decode_small(path, size))
}

/// Downscale already-decoded pixels to the tiny tier and store it, unless it is cached already.
fn write_tiny_tier(disk: Option<(&Path, (u128, u64))>, path: &str, w: u32, h: u32, rgba: &[u8]) {
    let key = mem_key(path, None, false, TINY_PX, true);
    let file = disk.map(|(d, s)| cache_file(d, path, s, TINY_PX));
    let in_mem = cache_get(&key).is_some();
    let on_disk = file.as_deref().is_none_or(Path::exists);
    if in_mem && on_disk {
        return;
    }
    let Some(bytes) = tiny_from_px(w, h, rgba) else {
        return;
    };
    if let (Some(f), false) = (&file, on_disk) {
        if crate::fsutil::write_atomic(f, &bytes).is_ok() {
            note_disk_write(disk.expect("file implies dir").0);
        }
    }
    cache_put(key, Arc::new(bytes));
}

/// The tiny tier (fit in `TINY_PX`, JPEG q70 or PNG) from straight-alpha RGBA.
fn tiny_from_px(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if w <= TINY_PX && h <= TINY_PX {
        return encode_thumb_q(w, h, rgba, TINY_JPEG_Q);
    }
    let img = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(w, h, rgba)?;
    // `imageops::thumbnail` scales to exactly the given box: compute the aspect-kept box first.
    let (tw, th) = fit(w, h, TINY_PX);
    let t = image::imageops::thumbnail(&img, tw, th);
    encode_thumb_q(tw, th, t.as_raw(), TINY_JPEG_Q)
}

/// `w`x`h` scaled to fit a `max` square, aspect kept, at least 1 px.
fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w, h);
    }
    let (w64, h64, m) = (w as u64, h as u64, max as u64);
    if w >= h {
        (max, ((h64 * m + w64 / 2) / w64).max(1) as u32)
    } else {
        (((w64 * m + h64 / 2) / h64).max(1) as u32, max)
    }
}

/// Fallback when the shell has no picture: decode with the `image` crate and downscale.
fn decode_small(path: &str, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let t = img.thumbnail(size, size).to_rgba8();
    Some((t.width(), t.height(), t.into_raw()))
}

/// JPEG for opaque pictures, PNG when any pixel is transparent.
#[cfg(test)]
fn encode_thumb(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    encode_thumb_q(w, h, rgba, THUMB_JPEG_Q)
}

fn encode_thumb_q(w: u32, h: u32, rgba: &[u8], quality: u8) -> Option<Vec<u8>> {
    if rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255) {
        return encode_png(w, h, rgba);
    }
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for p in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&p[..3]);
    }
    let mut out = Vec::with_capacity(rgb.len() / 8 + 256);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

/// Keep the cache under `cap` bytes: delete the oldest files first (down to 80%).
fn trim_dir(dir: &Path, cap: u64) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for sub in rd.flatten() {
        let Ok(inner) = std::fs::read_dir(sub.path()) else {
            continue;
        };
        for f in inner.flatten() {
            if let Ok(m) = f.metadata() {
                files.push((
                    m.modified().unwrap_or(std::time::UNIX_EPOCH),
                    m.len(),
                    f.path(),
                ));
            }
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= cap {
        return;
    }
    files.sort_by_key(|f| f.0);
    for (_, len, p) in files {
        if total <= cap / 10 * 8 {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(rgba.len() / 4 + 256);
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().ok()?;
    wr.write_image_data(rgba).ok()?;
    wr.finish().ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode and return (w, h, any non-zero alpha).
    fn check_png(bytes: &[u8]) -> (u32, u32, bool) {
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let mut r = png::Decoder::new(bytes).read_info().expect("valid png");
        let mut buf = vec![0; r.output_buffer_size()];
        let info = r.next_frame(&mut buf).expect("frame");
        assert_eq!(info.color_type, png::ColorType::Rgba);
        let any = buf[..info.buffer_size()]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3] != 0);
        (info.width, info.height, any)
    }

    #[test]
    fn generic_txt_icon() {
        let _c = ComGuard::init();
        let png = generic_icon_png("txt", false, 32).expect("icon");
        let (w, h, alpha) = check_png(&png);
        assert!(w >= 16 && h >= 16 && alpha);
        let dir = generic_icon_png("", true, 16).expect("folder icon");
        assert!(check_png(&dir).2);
    }

    #[test]
    fn per_path_exe_icon_and_cache() {
        let _c = ComGuard::init();
        let exe = r"C:\Windows\explorer.exe";
        let png = path_icon_png(exe, 32, false).expect("icon");
        let (w, h, alpha) = check_png(&png);
        assert!(w > 0 && h > 0 && alpha);
        let a = render(exe, false, 32, false).expect("render");
        let b = render(exe, false, 32, false).expect("cached");
        // Byte equality, not ptr_eq: parallel tests share the global cache.
        assert_eq!(a, b);
    }

    #[test]
    fn missing_path_is_none() {
        let _c = ComGuard::init();
        assert!(path_icon_png(r"C:\definitely\not\here.exe", 32, false).is_none());
    }

    #[test]
    fn thumb_key_changes_with_mtime_size_and_px() {
        let a = thumb_key(r"C:\P\a.jpg", 1, 10, 256);
        assert_eq!(a, thumb_key(r"c:\p\A.JPG", 1, 10, 256));
        assert_ne!(a, thumb_key(r"C:\P\a.jpg", 2, 10, 256));
        assert_ne!(a, thumb_key(r"C:\P\a.jpg", 1, 11, 256));
        assert_ne!(a, thumb_key(r"C:\P\a.jpg", 1, 10, 480));
    }

    #[test]
    fn opaque_thumbs_are_jpeg_transparent_are_png() {
        let opaque = [200u8, 30, 30, 255].repeat(16 * 16);
        let j = encode_thumb(16, 16, &opaque).expect("jpeg");
        assert_eq!(&j[..2], &[0xFF, 0xD8]);
        let mut clear = opaque.clone();
        clear[3] = 0;
        let p = encode_thumb(16, 16, &clear).expect("png");
        assert_eq!(&p[..4], b"\x89PNG");
    }

    #[test]
    fn trim_keeps_the_cache_under_its_cap() {
        let dir = std::env::temp_dir().join(format!("wsthumb-trim-{}", std::process::id()));
        let sub = dir.join("ab");
        std::fs::create_dir_all(&sub).unwrap();
        for i in 0..10 {
            std::fs::write(sub.join(format!("{i}.img")), vec![0u8; 100]).unwrap();
        }
        trim_dir(&dir, 500);
        let left: u64 = std::fs::read_dir(&sub)
            .unwrap()
            .flatten()
            .map(|f| f.metadata().unwrap().len())
            .sum();
        assert!(left <= 500, "left {left}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_decoding() {
        let q = "p=C%3A%5Cdir%2Fa%20b%C3%A9.txt&d=0&s=40";
        assert_eq!(query_param(q, "p").unwrap(), "C:\\dir/a b\u{e9}.txt");
        assert_eq!(query_param(q, "s").unwrap(), "40");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    /// The launcher's icon URLs carry an AUMID (percent-encoded, `!` inside).
    #[test]
    fn aumid_query_param() {
        let aumid = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        let url = format!("a={}&s=48", aumid.replace('!', "%21"));
        assert_eq!(query_param(&url, "a").unwrap(), aumid);
        assert_eq!(query_param(&url, "s").unwrap(), "48");
        assert_eq!(query_param("s=32", "a"), None);
    }

    /// Live check (manual): renders Calculator's shell icon to a non-empty PNG.
    #[test]
    #[ignore]
    fn calculator_aumid_icon_renders() {
        let _c = ComGuard::init();
        let png = render_aumid("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", 32).expect("icon");
        let (w, h, alpha) = check_png(&png);
        assert!(w >= 16 && h >= 16 && alpha);
        let cached =
            render_aumid("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", 32).expect("cached");
        assert_eq!(png, cached);
    }

    #[test]
    fn cache_evicts_oldest_by_bytes_and_replaces_in_place() {
        let mut c = IconCache::default();
        let v = |n: usize| Arc::new(vec![0u8; n]);
        cache_insert(&mut c, "a".into(), v(40), 100);
        cache_insert(&mut c, "b".into(), v(40), 100);
        assert_eq!(c.bytes, 80);
        // Replacing a key adjusts the count and does not duplicate it in the order.
        cache_insert(&mut c, "a".into(), v(30), 100);
        assert_eq!((c.bytes, c.order.len()), (70, 2));
        // 70 + 50 > 100: the oldest ("a") goes.
        cache_insert(&mut c, "c".into(), v(50), 100);
        assert!(!c.map.contains_key("a"));
        assert_eq!(c.bytes, 90);
        assert_eq!(c.bytes, c.map.values().map(|v| v.len()).sum::<usize>());
        // An entry larger than the budget empties the cache but is still kept.
        cache_insert(&mut c, "big".into(), v(500), 100);
        assert_eq!((c.map.len(), c.bytes), (1, 500));
    }

    #[test]
    fn stale_jobs_drop_only_from_a_lane_over_half_full() {
        let t0 = Instant::now();
        let after = Duration::from_secs(3);
        let now = t0 + Duration::from_secs(10);
        // Oldest first, as `submit` pushes them: 4 stale, 2 fresh.
        let mk = || -> VecDeque<Instant> {
            let mut l: VecDeque<Instant> = (0..4).map(|_| t0).collect();
            l.extend([now, now]);
            l
        };
        let mut l = mk();
        assert!(
            drain_stale(&mut l, 12, now, after, |t| *t).is_empty(),
            "half full: keep all"
        );
        let mut l = mk();
        let dead = drain_stale(&mut l, 8, now, after, |t| *t);
        assert_eq!((dead.len(), l.len()), (2, 4), "stops at half the cap");
        let mut l = mk();
        let dead = drain_stale(&mut l, 2, now, after, |t| *t);
        assert_eq!((dead.len(), l.len()), (4, 2), "fresh jobs stay");
    }

    #[test]
    fn percentiles_of_samples() {
        assert_eq!(percentiles(&mut []), [0; 4]);
        let mut v: Vec<u32> = (1..=100).rev().collect();
        assert_eq!(percentiles(&mut v), [50, 90, 99, 100]);
        assert_eq!(percentiles(&mut [7]), [7, 7, 7, 7]);
    }

    #[test]
    fn trim_runs_every_500_writes_or_10_minutes() {
        assert!(!trim_due(0, 10_000), "nothing written");
        assert!(!trim_due(499, 599));
        assert!(trim_due(500, 0));
        assert!(trim_due(1, 600));
    }

    #[test]
    fn small_thumbs_share_the_tiny_tier_and_lane() {
        let r = parse("p=C%3A%5Ca.jpg&d=0&s=40&t=1").unwrap();
        assert_eq!((r.size, r.thumb, r.immutable), (TINY_PX, true, false));
        assert_eq!(lane_for(r.warm, r.size), Lane::Tiny);
        // Icons keep their size; large thumbnails keep theirs and go to the hi lane.
        assert_eq!(parse("p=C%3A%5Ca.txt&s=40").unwrap().size, 40);
        let big = parse("p=C%3A%5Ca.jpg&s=512&t=1&v=123").unwrap();
        assert_eq!((big.size, big.immutable), (512, true));
        assert_eq!(lane_for(big.warm, big.size), Lane::Hi);
        let warm = parse("p=C%3A%5Ca.jpg&s=40&t=1&w=1").unwrap();
        assert_eq!(lane_for(warm.warm, warm.size), Lane::Lo);
        assert!(!parse("p=C%3A%5Ca.jpg&s=256&t=1&v=").unwrap().immutable);
        assert_eq!(parse("s=32"), None);
    }

    #[test]
    fn versioned_urls_are_immutable() {
        assert_eq!(cache_control(true), "public, max-age=31536000, immutable");
        assert_eq!(cache_control(false), "max-age=3600");
        let r = response(Some(Arc::new(vec![0xFF, 0xD8, 1])), true);
        assert_eq!(r.headers()["Cache-Control"], cache_control(true));
        assert_eq!(r.headers()["Content-Type"], "image/jpeg");
    }

    #[test]
    fn mem_keys_match_the_old_scheme() {
        assert_eq!(
            mem_key(r"C:\A.jpg", None, false, 256, true),
            r"p:c:\a.jpg:256:1"
        );
        assert_eq!(mem_key(r"C:\a.TXT", None, false, 32, false), "x:txt:32");
        assert_eq!(mem_key(r"C:\d", None, true, 16, false), "dir:16");
        assert_eq!(
            mem_key(r"C:\x.exe", None, false, 32, false),
            r"p:c:\x.exe:32:0"
        );
        assert_eq!(mem_key("", Some("App!Id"), false, 48, false), "a:App!Id:48");
    }

    #[test]
    fn fit_keeps_aspect() {
        assert_eq!(fit(256, 128, 48), (48, 24));
        assert_eq!(fit(100, 400, 48), (12, 48));
        assert_eq!(fit(1000, 1, 48), (48, 1));
        assert_eq!(fit(30, 20, 48), (30, 20));
    }

    #[test]
    fn tiny_tier_is_small_jpeg() {
        let (w, h) = (256u32, 192u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[
                    (x % 256) as u8,
                    (y % 256) as u8,
                    ((x ^ y) % 256) as u8,
                    255,
                ]);
            }
        }
        let t = tiny_from_px(w, h, &px).expect("tiny");
        assert_eq!(&t[..2], &[0xFF, 0xD8]);
        assert!(t.len() < 4096, "tiny tier {} bytes", t.len());
        let img = image::load_from_memory(&t).unwrap();
        assert_eq!((img.width(), img.height()), (48, 36));
    }

    /// A synthetic opaque JPEG on disk (gradient + a little texture, so the encoder has work to do).
    fn synthetic_jpeg(path: &Path, w: u32, h: u32, seed: u32) {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let n = (x.wrapping_mul(31) ^ y.wrapping_mul(17) ^ seed.wrapping_mul(2654435761)) & 31;
            image::Rgb([
                ((x * 255 / w) as u8).wrapping_add(n as u8),
                (y * 255 / h) as u8,
                (seed * 37 % 256) as u8,
            ])
        });
        img.save(path).expect("write jpeg");
    }

    /// Full round trip into a private cache dir: the sharp render also leaves the tiny tier on disk and in
    /// memory, and a second request is a disk hit. Prints what `SIIGBF_INCACHEONLY` did.
    #[test]
    fn sharp_render_writes_tiny_tier_and_hits_disk_next() {
        let _c = ComGuard::init();
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("thumbs");
        std::fs::create_dir_all(&cache).unwrap();
        let pic = tmp.path().join("pic.jpg");
        synthetic_jpeg(&pic, 800, 600, 1);
        let p = pic.to_str().unwrap();

        let cold_cached = shell_px(p, 256, Shot::ThumbCached).is_some();
        let a = thumb_bytes_in(Some(&cache), p, 256).expect("render");
        assert_eq!(&a[..2], &[0xFF, 0xD8]);
        let warm_cached = shell_px(p, 256, Shot::ThumbCached);
        eprintln!(
            "INCACHEONLY: before extraction {cold_cached}, after {:?}",
            warm_cached.as_ref().map(|r| (r.0, r.1))
        );

        let tiny = disk_get(&cache, p, TINY_PX).expect("tiny tier on disk");
        assert_eq!(&tiny[..2], &[0xFF, 0xD8]);
        assert!(tiny.len() < 4096, "tiny {} bytes", tiny.len());
        assert!(
            cache_get(&mem_key(p, None, false, TINY_PX, true)).is_some(),
            "tiny tier in memory"
        );
        let img = image::load_from_memory(&tiny).unwrap();
        assert!(img.width() <= TINY_PX && img.height() <= TINY_PX && img.width() >= 40);

        assert_eq!(disk_get(&cache, p, 256).as_deref(), Some(&a[..]));
        assert_eq!(
            thumb_bytes_in(Some(&cache), p, 256).unwrap(),
            a,
            "second call is the disk hit"
        );
        // An edit changes the key: the old entry is not served.
        synthetic_jpeg(&pic, 640, 480, 2);
        assert!(disk_get(&cache, p, 256).is_none_or(|b| b != a));
    }

    /// Per-tier timings. Set `WSICON_BENCH_DIR` to a folder of pictures (the first 200 are used); without it a
    /// temp folder of synthetic JPEGs is generated (`WSICON_BENCH_N`, default 40). Writes only to a temp cache dir.
    /// `cargo test -p winseek-app shell::icons::tests::bench_tiers -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore]
    fn bench_tiers() {
        let _c = ComGuard::init();
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("thumbs");
        std::fs::create_dir_all(&cache).unwrap();
        let pics: Vec<PathBuf> = match std::env::var_os("WSICON_BENCH_DIR") {
            Some(d) => std::fs::read_dir(d)
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    matches!(
                        ext_of(&p.to_string_lossy()).as_str(),
                        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif"
                    )
                })
                .take(200)
                .collect(),
            None => {
                let n: u32 = std::env::var("WSICON_BENCH_N")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(40);
                let src = tmp.path().join("src");
                std::fs::create_dir_all(&src).unwrap();
                (0..n)
                    .map(|i| {
                        let p = src.join(format!("img{i:03}.jpg"));
                        synthetic_jpeg(&p, 1600, 1200, i + 10);
                        p
                    })
                    .collect()
            }
        };
        let ms = |t: Instant| t.elapsed().as_micros() as u32;
        let mut rows: Vec<(&str, Vec<u32>)> = [
            "incache-only(cold) 256",
            "full miss 256 (+tiny)",
            "incache-only(warm) 256",
            "disk hit 256",
            "disk hit tiny",
            "full miss 512 (+tiny)",
        ]
        .into_iter()
        .map(|n| (n, Vec::new()))
        .collect();
        let mut incache_hits = 0;
        for pic in &pics {
            let p = pic.to_str().unwrap();
            let t = Instant::now();
            let _ = shell_px(p, 256, Shot::ThumbCached);
            rows[0].1.push(ms(t));
            let t = Instant::now();
            let _ = thumb_bytes_in(Some(&cache), p, 256);
            rows[1].1.push(ms(t));
            let t = Instant::now();
            incache_hits += shell_px(p, 256, Shot::ThumbCached).is_some() as u32;
            rows[2].1.push(ms(t));
            let t = Instant::now();
            let _ = disk_get(&cache, p, 256);
            rows[3].1.push(ms(t));
            let t = Instant::now();
            let _ = disk_get(&cache, p, TINY_PX);
            rows[4].1.push(ms(t));
            let t = Instant::now();
            let _ = thumb_bytes_in(Some(&cache), p, 512);
            rows[5].1.push(ms(t));
        }
        eprintln!(
            "{} pictures; INCACHEONLY hits after extraction: {incache_hits}",
            pics.len()
        );
        for (name, mut v) in rows {
            let [p50, p90, p99, max] = percentiles(&mut v).map(|x| x as f64 / 1000.0);
            eprintln!("{name:>24}: p50 {p50:.2} ms  p90 {p90:.2}  p99 {p99:.2}  max {max:.2}");
        }
    }
}
