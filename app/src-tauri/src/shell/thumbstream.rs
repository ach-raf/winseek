//! Picture streaming for the tile grids (search Pictures, Timeline, Collections), built like a game's texture
//! streamer: nothing is generated ahead of time, every picture is read live from the file, and only what is near
//! the viewport is read at all.
//!
//! - The page sends the tiles it wants on every scroll frame, in priority order (on screen first, centre out, then
//!   the rows just beyond). That list REPLACES the previous one: work for tiles that scrolled away is dropped before
//!   it starts (Mapbox GL's tile-cancel fix, PR #8633), so a fling across a million results costs only what stays.
//! - Two levels per tile, like mip levels. Level A is what the disk already holds and costs about a millisecond:
//!   Windows' own thumbnail cache (filled by Explorer) or the small preview a camera stores inside the JPEG.
//!   Level B is the real picture decoded at the tile's size (JPEGs decode straight at 1/2, 1/4 or 1/8 through
//!   WIC's source transform). During a fling the page asks for level A only — Unreal's streamer drops detail under
//!   pressure instead of leaving holes — and level B follows as soon as the scroll slows.
//! - Pixels go to the page as raw RGBA through WebView2 shared buffers, a few per window, created once and reused
//!   (posting new buffers forever stops working after ~32k posts, WebView2Feedback #3360). A batch is announced on
//!   an IPC channel; the page copies its pixels out and hands the buffer back. No per-picture request ever crosses
//!   the UI thread (WebView2 raises custom-protocol requests there, which is what froze the window before).

use super::icons::{shell_px, Shot};
use super::ComGuard;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tauri::ipc::Channel;
use tauri::Webview;
use windows::core::{Interface, GUID, HSTRING, PCWSTR};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
    GUID_WICPixelFormat32bppRGBA, IWICBitmapSource, IWICBitmapSourceTransform, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICBitmapTransformFlipHorizontal, WICBitmapTransformFlipVertical, WICBitmapTransformOptions,
    WICBitmapTransformRotate0, WICBitmapTransformRotate180, WICBitmapTransformRotate270,
    WICBitmapTransformRotate90, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

/// Shared buffers per window and their size. Four 8 MiB buffers hold ~50 screen-sized tiles each in flight while
/// the page copies the previous batch out.
const BUF_COUNT: usize = 4;
const BUF_BYTES: usize = 8 << 20;
/// Results are gathered this long after the first one lands, so a screenful arrives as a few batches, not 60.
const COALESCE: Duration = Duration::from_millis(6);
/// Level B never upscales and never goes past this many pixels on a side (a full-screen tile is ~1100 px wide).
const MAX_SIDE: u32 = 1600;

/// One tile the page wants, as a JSON array: `[key, path, width px, height px, need, visible]`. `need` bit 1 =
/// level A, bit 2 = level B (the page clears the bits for what it already holds); `visible` 1 = on screen, 0 =
/// just beyond it.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Want(String, String, u32, u32, u8, u8);

impl Want {
    fn key(&self) -> &str {
        &self.0
    }
    fn wants(&self, level: u8) -> bool {
        self.4 & level != 0
    }
    fn visible(&self) -> bool {
        self.5 != 0
    }
}

/// One finished picture in a batch: `k` key, `l` level (1 = A, 2 = B), `w` x `h` pixels at byte offset `o` of the
/// buffer (`n` bytes, RGBA). `f` = 1: this level has no picture (A: nothing on disk to show; B: unreadable).
#[derive(Serialize, Clone, Debug)]
pub struct Item {
    k: String,
    l: u8,
    w: u32,
    h: u32,
    o: usize,
    n: usize,
    f: u8,
}

/// A batch: pictures in shared buffer `b` of session generation `g`.
#[derive(Serialize, Clone, Debug)]
pub struct Batch {
    g: u64,
    b: usize,
    items: Vec<Item>,
}

struct Done {
    key: String,
    level: u8,
    /// None: no picture at this level.
    img: Option<(u32, u32, Vec<u8>)>,
}

/// A shared buffer's mapping in this process. Written only by the flusher, only while the buffer is out of the
/// page's hands.
#[derive(Clone, Copy)]
struct Buf {
    ptr: usize,
    len: usize,
}

struct Session {
    gen: u64,
    chan: Channel<Batch>,
    wants: Vec<Want>,
    /// Keys of `wants`, for the "still wanted?" checks.
    wanted: HashSet<String>,
    /// (key, level) being made, or delivered in a buffer the page has not handed back yet.
    pending: HashSet<(String, u8)>,
    out: Vec<Done>,
    first_out: Option<Instant>,
    bufs: Vec<Buf>,
    /// Per buffer: None = free; Some(keys) = with the page (the keys' pending entries end when it comes back).
    busy: Vec<Option<Vec<(String, u8)>>>,
}

impl Session {
    fn free_buf(&self) -> Option<usize> {
        self.busy.iter().position(Option::is_none)
    }
}

struct Hub {
    sessions: Mutex<HashMap<String, Session>>,
    /// Workers wait here for something to make.
    work: Condvar,
    /// The flusher waits here for results and free buffers.
    flush: Condvar,
}

static HUB: OnceLock<Arc<Hub>> = OnceLock::new();
static GEN: AtomicU64 = AtomicU64::new(1);

fn lock(h: &Hub) -> MutexGuard<'_, HashMap<String, Session>> {
    h.sessions.lock().unwrap_or_else(|e| e.into_inner())
}

fn hub() -> &'static Arc<Hub> {
    HUB.get_or_init(|| {
        let h = Arc::new(Hub {
            sessions: Mutex::new(HashMap::new()),
            work: Condvar::new(),
            flush: Condvar::new(),
        });
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .saturating_sub(1)
            .clamp(2, 8);
        for i in 0..n {
            let h = h.clone();
            let _ = std::thread::Builder::new()
                .name(format!("thumbstream-{i}"))
                .spawn(move || worker(&h));
        }
        let fh = h.clone();
        let _ = std::thread::Builder::new()
            .name("thumbstream-flush".into())
            .spawn(move || flusher(&fh));
        eprintln!("winseek: thumbstream up, {n} decode workers");
        h
    })
}

// ---- page commands ----------------------------------------------------------------------------------------------

/// Start (or restart, after a page reload) this window's stream: creates its shared buffers and posts them to the
/// page (`sharedbufferreceived`, additional data `{ "winseekThumbBuf": index, "gen": g }`). Returns the generation
/// the page tags its calls with.
#[tauri::command]
pub async fn thumbs_open(webview: Webview, on_batch: Channel<Batch>) -> Result<u64, String> {
    let label = webview.label().to_string();
    let gen = GEN.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = std::sync::mpsc::channel();
    let l2 = label.clone();
    webview
        .with_webview(move |pw| {
            // Runs on the UI thread, where WebView2 objects live.
            let _ = tx.send(post_buffers(&pw, &l2, gen));
        })
        .map_err(|e| e.to_string())?;
    let bufs =
        tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|_| "the window did not answer".to_string())??;
    let h = hub();
    lock(h).insert(
        label.clone(),
        Session {
            gen,
            chan: on_batch,
            wants: Vec::new(),
            wanted: HashSet::new(),
            pending: HashSet::new(),
            out: Vec::new(),
            first_out: None,
            busy: vec![None; bufs.len()],
            bufs,
        },
    );
    eprintln!("winseek: thumbstream session {label} gen {gen} open");
    Ok(gen)
}

/// The tiles this window wants now, in priority order; replaces the previous list.
#[tauri::command]
pub async fn thumbs_want(webview: Webview, gen: u64, items: Vec<Want>) -> Result<(), String> {
    let h = hub();
    let mut g = lock(h);
    let Some(s) = g.get_mut(webview.label()) else {
        return Ok(());
    };
    if s.gen != gen {
        return Ok(());
    }
    s.wanted = items.iter().map(|w| w.key().to_string()).collect();
    s.wants = items;
    // Finished pictures that are no longer wanted are not sent.
    let (keep, drop): (Vec<Done>, Vec<Done>) = std::mem::take(&mut s.out)
        .into_iter()
        .partition(|d| s.wanted.contains(&d.key));
    for d in drop {
        s.pending.remove(&(d.key, d.level));
    }
    s.out = keep;
    if s.out.is_empty() {
        s.first_out = None;
    }
    drop_guard(g);
    h.work.notify_all();
    Ok(())
}

/// The page copied buffer `b` out: it can be written again.
#[tauri::command]
pub async fn thumbs_ack(webview: Webview, gen: u64, b: usize) -> Result<(), String> {
    let h = hub();
    let mut g = lock(h);
    if let Some(s) = g.get_mut(webview.label()).filter(|s| s.gen == gen) {
        if let Some(keys) = s.busy.get_mut(b).and_then(Option::take) {
            for k in keys {
                s.pending.remove(&k);
            }
        }
    }
    drop_guard(g);
    h.flush.notify_all();
    h.work.notify_all();
    Ok(())
}

fn drop_guard<T>(g: T) {
    drop(g)
}

// ---- shared buffers (UI thread) ---------------------------------------------------------------------------------

thread_local! {
    /// The buffers' COM objects, kept alive on the UI thread that made them, per window. A reload keeps the previous
    /// generation one round longer (the flusher may still be writing into it) and closes the one before.
    static LIVE: RefCell<HashMap<String, Vec<Vec<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2SharedBuffer>>>> =
        RefCell::new(HashMap::new());
}

fn post_buffers(
    pw: &tauri::webview::PlatformWebview,
    label: &str,
    gen: u64,
) -> Result<Vec<Buf>, String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Environment12, ICoreWebView2_17, COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY,
    };
    let err = |what: &str, e: windows::core::Error| format!("{what}: {e}");
    // SAFETY: WebView2 COM calls on the UI thread that owns the webview; out-pointers point at live locals.
    unsafe {
        let env: ICoreWebView2Environment12 = pw
            .environment()
            .cast()
            .map_err(|e| err("WebView2 runtime too old for shared buffers", e))?;
        let core = pw
            .controller()
            .CoreWebView2()
            .map_err(|e| err("CoreWebView2", e))?;
        let core: ICoreWebView2_17 = core.cast().map_err(|e| err("ICoreWebView2_17", e))?;
        let mut objs = Vec::with_capacity(BUF_COUNT);
        let mut bufs = Vec::with_capacity(BUF_COUNT);
        for i in 0..BUF_COUNT {
            let sb = env
                .CreateSharedBuffer(BUF_BYTES as u64)
                .map_err(|e| err("CreateSharedBuffer", e))?;
            let mut p: *mut u8 = std::ptr::null_mut();
            sb.Buffer(&mut p)
                .map_err(|e| err("SharedBuffer.Buffer", e))?;
            let json = HSTRING::from(format!(r#"{{"winseekThumbBuf":{i},"gen":{gen}}}"#));
            core.PostSharedBufferToScript(
                &sb,
                COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY,
                PCWSTR(json.as_ptr()),
            )
            .map_err(|e| err("PostSharedBufferToScript", e))?;
            bufs.push(Buf {
                ptr: p as usize,
                len: BUF_BYTES,
            });
            objs.push(sb);
        }
        LIVE.with(|l| {
            let mut l = l.borrow_mut();
            let gens = l.entry(label.to_string()).or_default();
            gens.push(objs);
            while gens.len() > 2 {
                for sb in gens.remove(0) {
                    let _ = sb.Close();
                }
            }
        });
        Ok(bufs)
    }
}

// ---- scheduling -------------------------------------------------------------------------------------------------

struct Task {
    label: String,
    gen: u64,
    key: String,
    path: String,
    w: u32,
    h: u32,
    level: u8,
}

/// The next thing to make: level A on screen, then level B on screen, then A just beyond, then B just beyond —
/// each in the page's order (centre out). Marks it pending.
fn pick(g: &mut HashMap<String, Session>) -> Option<Task> {
    for (label, s) in g.iter_mut() {
        for (vis, level) in [(true, 1u8), (true, 2), (false, 1), (false, 2)] {
            let hit = s.wants.iter().find(|w| {
                w.visible() == vis
                    && w.wants(level)
                    && !s.pending.contains(&(w.key().to_string(), level))
                    // Level B already on its way makes level A pointless.
                    && !(level == 1 && s.pending.contains(&(w.key().to_string(), 2)) && w.wants(2))
            });
            if let Some(w) = hit {
                let t = Task {
                    label: label.clone(),
                    gen: s.gen,
                    key: w.0.clone(),
                    path: w.1.clone(),
                    w: w.2.clamp(1, MAX_SIDE),
                    h: w.3.clamp(1, MAX_SIDE),
                    level,
                };
                s.pending.insert((t.key.clone(), level));
                return Some(t);
            }
        }
    }
    None
}

fn worker(h: &Hub) {
    let _com = ComGuard::init();
    // SAFETY: plain COM object creation on this thread's apartment.
    let wic: Option<IWICImagingFactory> =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.ok();
    loop {
        let task = {
            let mut g = lock(h);
            loop {
                if let Some(t) = pick(&mut g) {
                    break t;
                }
                g = h.work.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        };
        let t0 = Instant::now();
        let img = match task.level {
            1 => level_a(wic.as_ref(), &task.path, task.w, task.h),
            _ => level_b(wic.as_ref(), &task.path, task.w, task.h),
        };
        stats_record(task.level, img.is_some(), t0.elapsed());
        let mut g = lock(h);
        if let Some(s) = g.get_mut(&task.label).filter(|s| s.gen == task.gen) {
            if s.wanted.contains(&task.key) {
                s.out.push(Done {
                    key: task.key,
                    level: task.level,
                    img,
                });
                s.first_out.get_or_insert_with(Instant::now);
                drop_guard(g);
                h.flush.notify_one();
            } else {
                s.pending.remove(&(task.key, task.level));
            }
        }
    }
}

fn flusher(h: &Hub) {
    loop {
        // Find a session with results and a free buffer whose first result has waited out the coalescing window.
        let (label, gen, b, buf, chan, items) = {
            let mut g = lock(h);
            loop {
                let now = Instant::now();
                let mut wait = None::<Duration>;
                let mut ready = None;
                for (label, s) in g.iter() {
                    let (Some(first), Some(b)) = (s.first_out, s.free_buf()) else {
                        continue;
                    };
                    let age = now.saturating_duration_since(first);
                    if age >= COALESCE {
                        ready = Some((label.clone(), b));
                        break;
                    }
                    let left = COALESCE - age;
                    wait = Some(wait.map_or(left, |w: Duration| w.min(left)));
                }
                if let Some((label, b)) = ready {
                    let s = g.get_mut(&label).expect("found above");
                    let buf = s.bufs[b];
                    let (take, keep) = fit_batch(std::mem::take(&mut s.out), buf.len);
                    s.out = keep;
                    s.first_out = if s.out.is_empty() { None } else { Some(now) };
                    s.busy[b] = Some(take.iter().map(|d| (d.key.clone(), d.level)).collect());
                    break (label, s.gen, b, buf, s.chan.clone(), take);
                }
                g = match wait {
                    Some(w) => {
                        h.flush
                            .wait_timeout(g, w)
                            .unwrap_or_else(|e| e.into_inner())
                            .0
                    }
                    None => h.flush.wait(g).unwrap_or_else(|e| e.into_inner()),
                };
            }
        };
        // Copy outside the lock: the buffer is marked busy, nobody else writes it.
        let mut off = 0usize;
        let mut meta = Vec::with_capacity(items.len());
        for d in items {
            match d.img {
                Some((w, hh, px)) => {
                    // SAFETY: `buf` maps BUF_BYTES of shared memory alive for this session generation (closed only
                    // two generations later); fit_batch kept off + len within it; this buffer is not with the page.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            px.as_ptr(),
                            (buf.ptr + off) as *mut u8,
                            px.len(),
                        );
                    }
                    meta.push(Item {
                        k: d.key,
                        l: d.level,
                        w,
                        h: hh,
                        o: off,
                        n: px.len(),
                        f: 0,
                    });
                    off += px.len();
                }
                None => meta.push(Item {
                    k: d.key,
                    l: d.level,
                    w: 0,
                    h: 0,
                    o: 0,
                    n: 0,
                    f: 1,
                }),
            }
        }
        if chan
            .send(Batch {
                g: gen,
                b,
                items: meta,
            })
            .is_err()
        {
            // The page is gone (window closed or reloaded): forget the session unless a new one replaced it.
            let mut g = lock(h);
            if g.get(&label).is_some_and(|s| s.gen == gen) {
                g.remove(&label);
                eprintln!("winseek: thumbstream session {label} gen {gen} closed");
            }
        }
    }
}

/// Split finished results into what fits one buffer (in order) and the rest. A picture larger than a whole buffer
/// cannot happen (MAX_SIDE^2 * 4 < BUF_BYTES) but would be sent as "no picture" rather than block the queue.
fn fit_batch(out: Vec<Done>, cap: usize) -> (Vec<Done>, Vec<Done>) {
    let mut used = 0usize;
    let mut take = Vec::new();
    let mut keep = Vec::new();
    for mut d in out {
        let n = d.img.as_ref().map_or(0, |i| i.2.len());
        if n > cap {
            d.img = None;
            take.push(d);
        } else if used + n <= cap && keep.is_empty() {
            used += n;
            take.push(d);
        } else {
            keep.push(d);
        }
    }
    (take, keep)
}

// ---- reading pictures -------------------------------------------------------------------------------------------

/// Level A: what the disk already has. Windows' thumbnail cache first (any file type Explorer has shown), then the
/// preview a JPEG carries in its EXIF block. Never decodes the picture itself.
fn level_a(
    wic: Option<&IWICImagingFactory>,
    path: &str,
    w: u32,
    h: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    if path.starts_with(r"\\") {
        return None; // a network share: no "instant" read there
    }
    let side = cover_side(w, h);
    // Explorer caches 256 px entries; a bigger ask never hits.
    if let Some(r) = shell_px(path, side.min(256), Shot::ThumbCached) {
        return Some(r);
    }
    exif_thumb(wic?, path)
}

/// The preview a JPEG carries in its EXIF block, upright. Never decodes the picture itself.
fn exif_thumb(wic: &IWICImagingFactory, path: &str) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: WIC calls on objects created on this thread.
    unsafe {
        let dec = wic
            .CreateDecoderFromFilename(
                &HSTRING::from(path),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .ok()?;
        // Only JPEG: other codecs answer GetThumbnail by decoding the whole picture, which is level B's job.
        if dec.GetContainerFormat().ok()? != GUID_ContainerFormatJpeg {
            return None;
        }
        let frame = dec.GetFrame(0).ok()?;
        let thumb = frame.GetThumbnail().ok()?;
        let orient = orientation(path);
        let src = orient_source(wic, &thumb, orient)?;
        copy_rgba(wic, &src)
    }
}

/// Level B: the picture decoded at the tile's size (cover: the short side fills the tile), oriented like Explorer
/// shows it. Files WIC cannot read (videos, PDFs, …) go through their shell thumbnail handler.
fn level_b(
    wic: Option<&IWICImagingFactory>,
    path: &str,
    w: u32,
    h: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    if let Some(wic) = wic {
        if let Some(r) = wic_decode(wic, path, w, h) {
            return Some(r);
        }
    }
    shell_px(path, cover_side(w, h), Shot::ThumbOnly)
}

fn wic_decode(wic: &IWICImagingFactory, path: &str, w: u32, h: u32) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: WIC calls on objects created on this thread; buffers outlive the calls that fill them.
    unsafe {
        let dec = wic
            .CreateDecoderFromFilename(
                &HSTRING::from(path),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let (mut iw, mut ih) = (0u32, 0u32);
        frame.GetSize(&mut iw, &mut ih).ok()?;
        if iw == 0 || ih == 0 {
            return None;
        }
        let orient = orientation(path);
        // The tile is in display orientation; a 90° picture is stored with its sides swapped.
        let (dw, dh) = if (5..=8).contains(&orient) {
            (ih, iw)
        } else {
            (iw, ih)
        };
        let (tw, th) = cover_size(dw, dh, w, h);
        let (tw, th) = if (5..=8).contains(&orient) {
            (th, tw)
        } else {
            (tw, th)
        };
        let mut src: IWICBitmapSource = frame.cast().ok()?;
        // DCT-domain scaling: the JPEG decoder hands out 1/2, 1/4 or 1/8 size without decoding full resolution.
        if tw < iw {
            if let Ok(tr) = frame.cast::<IWICBitmapSourceTransform>() {
                if let Some(small) = scaled_decode(wic, &tr, iw, ih, tw, th) {
                    src = small;
                }
            }
        }
        let (mut sw, mut sh) = (0u32, 0u32);
        src.GetSize(&mut sw, &mut sh).ok()?;
        if (sw, sh) != (tw, th) {
            let scaler = wic.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(&src, tw, th, WICBitmapInterpolationModeFant)
                .ok()?;
            src = scaler.cast().ok()?;
        }
        let src = orient_source(wic, &src, orient)?;
        copy_rgba(wic, &src)
    }
}

/// The smallest decoder-native size (a power-of-two fraction for JPEG) that still covers `tw` x `th`, as an
/// in-memory bitmap. None when the decoder offers nothing smaller than full size.
unsafe fn scaled_decode(
    wic: &IWICImagingFactory,
    tr: &IWICBitmapSourceTransform,
    iw: u32,
    ih: u32,
    tw: u32,
    th: u32,
) -> Option<IWICBitmapSource> {
    let mut best = None;
    for div in [8u32, 4, 2] {
        let (mut cw, mut ch) = (iw.div_ceil(div), ih.div_ceil(div));
        tr.GetClosestSize(&mut cw, &mut ch).ok()?;
        if cw >= tw && ch >= th && cw < iw {
            best = Some((cw, ch));
            break;
        }
    }
    let (cw, ch) = best?;
    let mut fmt: GUID = GUID_WICPixelFormat24bppBGR;
    tr.GetClosestPixelFormat(&mut fmt).ok()?;
    let bpp = if fmt == GUID_WICPixelFormat24bppBGR {
        3
    } else {
        4
    };
    if bpp == 4 && fmt != GUID_WICPixelFormat32bppRGBA {
        fmt = GUID_WICPixelFormat32bppRGBA;
        let mut probe = fmt;
        tr.GetClosestPixelFormat(&mut probe).ok()?;
        if probe != fmt {
            return None;
        }
    }
    let stride = (cw * bpp).div_ceil(4) * 4;
    let mut px = vec![0u8; (stride * ch) as usize];
    tr.CopyPixels(
        std::ptr::null::<WICRect>(),
        cw,
        ch,
        &fmt,
        WICBitmapTransformRotate0,
        stride,
        &mut px,
    )
    .ok()?;
    let bmp = wic.CreateBitmapFromMemory(cw, ch, &fmt, stride, &px).ok()?;
    bmp.cast().ok()
}

/// `src` turned upright for EXIF orientation `o` (1 = as stored).
unsafe fn orient_source(
    wic: &IWICImagingFactory,
    src: &IWICBitmapSource,
    o: u16,
) -> Option<IWICBitmapSource> {
    let t: WICBitmapTransformOptions = match o {
        2 => WICBitmapTransformFlipHorizontal,
        3 => WICBitmapTransformRotate180,
        4 => WICBitmapTransformFlipVertical,
        5 => WICBitmapTransformOptions(
            WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        6 => WICBitmapTransformRotate90,
        7 => WICBitmapTransformOptions(
            WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        8 => WICBitmapTransformRotate270,
        _ => return Some(src.clone()),
    };
    let r = wic.CreateBitmapFlipRotator().ok()?;
    r.Initialize(src, t).ok()?;
    r.cast().ok()
}

/// `src` as tightly packed straight-alpha RGBA.
unsafe fn copy_rgba(
    wic: &IWICImagingFactory,
    src: &IWICBitmapSource,
) -> Option<(u32, u32, Vec<u8>)> {
    let conv = wic.CreateFormatConverter().ok()?;
    conv.Initialize(
        src,
        &GUID_WICPixelFormat32bppRGBA,
        WICBitmapDitherTypeNone,
        None,
        0.0,
        WICBitmapPaletteTypeCustom,
    )
    .ok()?;
    let (mut w, mut h) = (0u32, 0u32);
    conv.GetSize(&mut w, &mut h).ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    let mut px = vec![0u8; (w * h * 4) as usize];
    conv.CopyPixels(std::ptr::null(), w * 4, &mut px).ok()?;
    Some((w, h, px))
}

/// EXIF orientation (1-8) from the file's header; 1 when there is none. Reads only the metadata segment.
fn orientation(path: &str) -> u16 {
    let Ok(f) = std::fs::File::open(path) else {
        return 1;
    };
    let Ok(e) = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)) else {
        return 1;
    };
    e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .filter(|v| (1..=8).contains(v))
        .map_or(1, |v| v as u16)
}

/// The size a `iw` x `ih` picture is scaled to so it covers a `w` x `h` tile (never up, never past MAX_SIDE).
fn cover_size(iw: u32, ih: u32, w: u32, h: u32) -> (u32, u32) {
    let s = (w as f64 / iw as f64).max(h as f64 / ih as f64).min(1.0);
    let s = s.min(MAX_SIDE as f64 / iw.max(ih) as f64);
    (
        ((iw as f64 * s).round() as u32).max(1),
        ((ih as f64 * s).round() as u32).max(1),
    )
}

/// The box side to ask the shell for so its fitted picture covers a `w` x `h` tile for any common aspect (the
/// shell fits the long side in the box; a 4:3 picture in a 4:3 tile needs the long side; a 3:4 one needs 4/3 of it).
fn cover_side(w: u32, h: u32) -> u32 {
    (w.max(h) * 4 / 3).clamp(16, MAX_SIDE)
}

// ---- timing log -------------------------------------------------------------------------------------------------

struct Stats {
    a_hit: u32,
    a_miss: u32,
    b_ok: u32,
    b_fail: u32,
    b_ms: Vec<u32>,
    since: Instant,
}

static STATS: Mutex<Option<Stats>> = Mutex::new(None);

/// Level A hit rate and level B decode times, logged once a minute while pictures stream.
fn stats_record(level: u8, ok: bool, took: Duration) {
    let Ok(mut g) = STATS.lock() else { return };
    let s = g.get_or_insert_with(|| Stats {
        a_hit: 0,
        a_miss: 0,
        b_ok: 0,
        b_fail: 0,
        b_ms: Vec::new(),
        since: Instant::now(),
    });
    match (level, ok) {
        (1, true) => s.a_hit += 1,
        (1, false) => s.a_miss += 1,
        (_, true) => {
            s.b_ok += 1;
            s.b_ms.push(took.as_millis().min(u32::MAX as u128) as u32);
        }
        _ => s.b_fail += 1,
    }
    if s.since.elapsed() < Duration::from_secs(60) {
        return;
    }
    let mut ms = std::mem::take(&mut s.b_ms);
    ms.sort_unstable();
    let at = |p: usize| {
        ms.get((ms.len().saturating_sub(1)) * p / 100)
            .copied()
            .unwrap_or(0)
    };
    eprintln!(
        "winseek: thumbstream A hit={} miss={} | B ok={} fail={} ms p50/90/99={}/{}/{}",
        s.a_hit,
        s.a_miss,
        s.b_ok,
        s.b_fail,
        at(50),
        at(90),
        at(99)
    );
    *s = Stats {
        a_hit: 0,
        a_miss: 0,
        b_ok: 0,
        b_fail: 0,
        b_ms: Vec::new(),
        since: Instant::now(),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(k: &str, need: u8, vis: u8) -> Want {
        Want(k.into(), format!(r"C:\p\{k}.jpg"), 200, 150, need, vis)
    }

    fn session(wants: Vec<Want>) -> HashMap<String, Session> {
        let (chan, _) = (Channel::new(|_| Ok(())), ());
        let mut m = HashMap::new();
        m.insert(
            "w".to_string(),
            Session {
                gen: 1,
                chan,
                wanted: wants.iter().map(|w| w.key().to_string()).collect(),
                wants,
                pending: HashSet::new(),
                out: Vec::new(),
                first_out: None,
                bufs: vec![Buf { ptr: 0, len: 100 }],
                busy: vec![None],
            },
        );
        m
    }

    #[test]
    fn on_screen_level_a_first_then_b_then_the_ring() {
        let mut g = session(vec![
            want("ring", 3, 0),
            want("vis1", 3, 1),
            want("vis2", 3, 1),
        ]);
        let order: Vec<(String, u8)> = std::iter::from_fn(|| pick(&mut g))
            .map(|t| (t.key, t.level))
            .collect();
        assert_eq!(
            order,
            vec![
                ("vis1".into(), 1),
                ("vis2".into(), 1),
                ("vis1".into(), 2),
                ("vis2".into(), 2),
                ("ring".into(), 1),
                ("ring".into(), 2),
            ]
        );
    }

    #[test]
    fn a_fling_asks_level_a_only_and_held_pictures_are_skipped() {
        // need = 1: level A only (fast scroll); need = 2: the page already holds A.
        let mut g = session(vec![want("a", 1, 1), want("b", 2, 1)]);
        let order: Vec<(String, u8)> = std::iter::from_fn(|| pick(&mut g))
            .map(|t| (t.key, t.level))
            .collect();
        assert_eq!(order, vec![("a".into(), 1), ("b".into(), 2)]);
    }

    #[test]
    fn level_a_is_skipped_once_level_b_is_under_way() {
        let mut g = session(vec![want("x", 3, 1)]);
        g.get_mut("w").unwrap().pending.insert(("x".into(), 2));
        assert!(pick(&mut g).is_none());
    }

    #[test]
    fn a_batch_takes_what_fits_in_order() {
        let d = |k: &str, n: usize| Done {
            key: k.into(),
            level: 2,
            img: Some((1, 1, vec![0; n])),
        };
        let (take, keep) = fit_batch(vec![d("a", 40), d("b", 40), d("c", 40), d("d", 10)], 100);
        let keys = |v: &[Done]| v.iter().map(|d| d.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys(&take), vec!["a", "b"]);
        // Order is kept: "d" would fit but waits behind "c".
        assert_eq!(keys(&keep), vec!["c", "d"]);
    }

    #[test]
    fn cover_size_fills_the_tile_without_upscaling() {
        assert_eq!(cover_size(4000, 3000, 200, 150), (200, 150));
        assert_eq!(cover_size(3000, 4000, 200, 150), (200, 267));
        assert_eq!(cover_size(100, 80, 200, 150), (100, 80));
        assert_eq!(cover_size(20000, 100, 200, 150), (1600, 8));
    }

    /// Read timings on real pictures: `WINSEEK_BENCH_DIR=<folder> cargo test -p winseek-app thumbstream_bench --
    /// --ignored --nocapture`. Prints counts and milliseconds only.
    #[test]
    #[ignore]
    fn thumbstream_bench() {
        let Ok(dir) = std::env::var("WINSEEK_BENCH_DIR") else {
            return;
        };
        let _com = ComGuard::init();
        // SAFETY: plain COM object creation on this thread.
        let wic: Option<IWICImagingFactory> =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.ok();
        let exts = [
            "jpg", "jpeg", "png", "webp", "gif", "bmp", "heic", "avif", "tif", "tiff",
        ];
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|x| exts.contains(&x.to_string_lossy().to_lowercase().as_str()))
            })
            .take(120)
            .collect();
        files.sort();
        let (mut a_hit, mut a_ms, mut b_ok, mut b_ms) = (0, Vec::new(), 0, Vec::new());
        let (mut sh_ms, mut ex_ms, mut sh_hit, mut ex_hit) = (Vec::new(), Vec::new(), 0, 0);
        for f in &files {
            let p = f.to_string_lossy();
            let t = Instant::now();
            if shell_px(&p, 256, Shot::ThumbCached).is_some() {
                sh_hit += 1;
            }
            sh_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            if exif_thumb(wic.as_ref().unwrap(), &p).is_some() {
                ex_hit += 1;
            }
            ex_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            if level_a(wic.as_ref(), &p, 300, 225).is_some() {
                a_hit += 1;
            }
            a_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            if level_b(wic.as_ref(), &p, 300, 225).is_some() {
                b_ok += 1;
            }
            b_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let pct = |v: &mut Vec<f64>, q: usize| {
            v.sort_by(|a, b| a.total_cmp(b));
            v.get((v.len().max(1) - 1) * q / 100)
                .copied()
                .unwrap_or(0.0)
        };
        let mut by_ext = HashMap::<String, usize>::new();
        for f in &files {
            *by_ext
                .entry(f.extension().unwrap().to_string_lossy().to_lowercase())
                .or_default() += 1;
        }
        eprintln!("files={} by_ext={by_ext:?}", files.len());
        eprintln!("  shell cache: hit={sh_hit} ms p50={:.1} p90={:.1} | exif: hit={ex_hit} ms p50={:.1} p90={:.1}", pct(&mut sh_ms, 50), pct(&mut sh_ms, 90), pct(&mut ex_ms, 50), pct(&mut ex_ms, 90));
        eprintln!(
            "level A: hit={a_hit} ms p50={:.1} p90={:.1} max={:.1}",
            pct(&mut a_ms, 50),
            pct(&mut a_ms, 90),
            pct(&mut a_ms, 100)
        );
        eprintln!(
            "level B: ok={b_ok} ms p50={:.1} p90={:.1} max={:.1}",
            pct(&mut b_ms, 50),
            pct(&mut b_ms, 90),
            pct(&mut b_ms, 100)
        );
    }

    #[test]
    fn want_parses_from_the_page_array() {
        let w: Want = serde_json::from_str(r#"["k","C:\\a.jpg",300,225,3,1]"#).unwrap();
        assert_eq!(w, Want("k".into(), r"C:\a.jpg".into(), 300, 225, 3, 1));
        assert!(w.wants(1) && w.wants(2) && w.visible());
    }
}
