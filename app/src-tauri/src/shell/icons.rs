//! `wsicon:` protocol: shell icons and thumbnails as PNG, rendered on worker threads (never the main thread).

use super::{shell_item, wide, ComGuard};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use tauri::http::{Request, Response};
use tauri::UriSchemeResponder;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SMALLICON,
    SHGFI_USEFILEATTRIBUTES, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

const CACHE_CAP: usize = 2000;
const WORKERS: usize = 4;
/// Extensions whose icon is inside the file: rendered per path, never cached by extension.
const PER_PATH_EXT: &[&str] = &["exe", "ico", "lnk", "url", "cur", "ani", "msc", "appref-ms"];

struct Job {
    path: String,
    /// Store/Start-menu app id — icon fetched through `shell:AppsFolder\<aumid>` (launcher).
    aumid: Option<String>,
    is_dir: bool,
    size: u32,
    thumb: bool,
    responder: UriSchemeResponder,
}

/// Waiting jobs. The newest is served first: the tile requested last is the one under the viewport, older ones
/// belong to rows already scrolled past (the page also holds most requests back, see thumbQueue.ts).
struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    cv: Condvar,
}

/// Waiting jobs kept; beyond this the oldest are answered with 503 (the page does not cache that).
const MAX_WAITING: usize = 512;

static POOL: OnceLock<Arc<Queue>> = OnceLock::new();
static CACHE: Mutex<Option<HashMap<String, Arc<Vec<u8>>>>> = Mutex::new(None);

/// Decode workers: cores - 1 (the UI keeps one), between 3 and 8.
fn workers() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_sub(1).clamp(WORKERS, 8)
}

fn next_job(q: &Queue) -> Option<Job> {
    let mut g = q.jobs.lock().ok()?;
    loop {
        if let Some(j) = g.pop_back() {
            return Some(j);
        }
        g = q.cv.wait(g).ok()?;
    }
}

fn pool() -> &'static Arc<Queue> {
    POOL.get_or_init(|| {
        let q = Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), cv: Condvar::new() });
        for i in 0..workers() {
            let q = q.clone();
            let _ = std::thread::Builder::new().name(format!("wsicon-{i}")).spawn(move || {
                let _com = ComGuard::init();
                while let Some(job) = next_job(&q) {
                    let png = match &job.aumid {
                        Some(aumid) => render_aumid(aumid, job.size),
                        None => render(&job.path, job.is_dir, job.size, job.thumb),
                    };
                    job.responder.respond(response(png));
                }
            });
        }
        q
    })
}

fn submit(job: Job) {
    let q = pool();
    let dropped = match q.jobs.lock() {
        Ok(mut g) => {
            g.push_back(job);
            if g.len() > MAX_WAITING {
                g.pop_front()
            } else {
                None
            }
        }
        Err(_) => Some(job),
    };
    q.cv.notify_one();
    if let Some(old) = dropped {
        old.responder.respond(busy_response());
    }
}

fn busy_response() -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "no-store")
        .status(503)
        .body(Cow::Borrowed(&[][..]))
        .expect("static response")
}

fn response(png: Option<Arc<Vec<u8>>>) -> Response<Cow<'static, [u8]>> {
    let b = Response::builder().header("Access-Control-Allow-Origin", "*");
    match png {
        Some(p) => b
            .status(200)
            .header("Content-Type", if p.starts_with(&[0xFF, 0xD8]) { "image/jpeg" } else { "image/png" })
            .header("Cache-Control", "max-age=3600")
            .body(Cow::Owned(p.as_ref().clone())),
        None => b.status(404).body(Cow::Borrowed(&[][..])),
    }
    .expect("static response")
}

pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn query_param(q: &str, key: &str) -> Option<String> {
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

pub fn handle(req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let q = req.uri().query().unwrap_or("");
    let size = query_param(q, "s").and_then(|s| s.parse::<u32>().ok()).unwrap_or(32).clamp(16, 1024);
    let job = if let Some(aumid) = query_param(q, "a").filter(|a| !a.is_empty()) {
        Job { path: String::new(), aumid: Some(aumid), is_dir: false, size, thumb: false, responder }
    } else if let Some(path) = query_param(q, "p").filter(|p| !p.is_empty()) {
        let is_dir = query_param(q, "d").as_deref() == Some("1");
        let thumb = query_param(q, "t").as_deref() == Some("1");
        Job { path, aumid: None, is_dir, size, thumb, responder }
    } else {
        return responder.respond(response(None));
    };
    submit(job);
}

fn ext_of(path: &str) -> String {
    Path::new(path).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Cached PNG for a path (generic by extension, or per path for exe/lnk/thumbnails).
pub(super) fn render(path: &str, is_dir: bool, size: u32, thumb: bool) -> Option<Arc<Vec<u8>>> {
    let ext = ext_of(path);
    let per_path = thumb || (!is_dir && PER_PATH_EXT.contains(&ext.as_str()));
    let key = if per_path {
        format!("p:{}:{size}:{}", path.to_lowercase(), thumb as u8)
    } else if is_dir {
        format!("dir:{size}")
    } else {
        format!("x:{ext}:{size}")
    };
    if let Some(hit) = CACHE.lock().ok()?.as_ref().and_then(|c| c.get(&key)) {
        return Some(hit.clone());
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
    let mut g = CACHE.lock().ok()?;
    let c = g.get_or_insert_with(HashMap::new);
    if c.len() >= CACHE_CAP {
        c.clear();
    }
    c.insert(key, png.clone());
    Some(png)
}

/// Cached PNG for a shell app id (Store or Start-menu entry), via `shell:AppsFolder\<aumid>`.
pub(super) fn render_aumid(aumid: &str, size: u32) -> Option<Arc<Vec<u8>>> {
    let key = format!("a:{aumid}:{size}");
    if let Some(hit) = CACHE.lock().ok()?.as_ref().and_then(|c| c.get(&key)) {
        return Some(hit.clone());
    }
    let png = Arc::new(path_icon_png(&format!("shell:AppsFolder\\{aumid}"), size, false)?);
    if size > 256 {
        return Some(png);
    }
    let mut g = CACHE.lock().ok()?;
    let c = g.get_or_insert_with(HashMap::new);
    if c.len() >= CACHE_CAP {
        c.clear();
    }
    c.insert(key, png.clone());
    Some(png)
}

/// Extension-based icon via SHGFI_USEFILEATTRIBUTES (no disk access).
pub(super) fn generic_icon_png(ext: &str, is_dir: bool, size: u32) -> Option<Vec<u8>> {
    let name = match (is_dir, ext.is_empty()) {
        (true, _) => "folder".to_string(),
        (false, true) => "file".to_string(),
        (false, false) => format!("file.{ext}"),
    };
    let attrs = if is_dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
    let sz = if size <= 16 { SHGFI_SMALLICON } else { SHGFI_LARGEICON };
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
    let item = shell_item(path).ok()?;
    let f: IShellItemImageFactory = item.cast().ok()?;
    let sz = SIZE { cx: size as i32, cy: size as i32 };
    // SAFETY: GetImage returns an owned HBITMAP that BitmapGuard deletes.
    let get = || unsafe {
        if thumb {
            f.GetImage(sz, SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY).or_else(|_| f.GetImage(sz, SIIGBF_RESIZETOFIT))
        } else {
            f.GetImage(sz, SIIGBF_RESIZETOFIT | SIIGBF_ICONONLY)
        }
    };
    // The shell extracts icons asynchronously and answers E_PENDING until the image is ready.
    const E_PENDING: windows::core::HRESULT = windows::core::HRESULT(0x8000000Au32 as i32);
    let mut res = get();
    for _ in 0..25 {
        match &res {
            Err(e) if e.code() == E_PENDING => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                res = get();
            }
            _ => break,
        }
    }
    let hbm = res.ok()?;
    let _g = BitmapGuard(hbm);
    let (w, h, mut px) = bitmap_bgra(hbm)?;
    let mut any = false;
    for p in px.as_chunks_mut::<4>().0.iter_mut() {
        any |= p[3] != 0;
    }
    if !any {
        px.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
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
        GetObjectW(hbm.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void))
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
        let l = GetDIBits(dc, hbm, 0, h, Some(buf.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
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
        match mask.as_ref().and_then(|m| bitmap_bgra(m.0)).filter(|m| m.0 == w && m.1 == h) {
            Some((_, _, m)) => {
                for (p, mp) in px.as_chunks_mut::<4>().0.iter_mut().zip(m.as_chunks::<4>().0.iter()) {
                    p[3] = if mp[0] == 0 { 255 } else { 0 };
                }
            }
            None => px.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255),
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
        let d = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("winseek").join("thumbs");
        std::fs::create_dir_all(&d).ok()?;
        let t = d.clone();
        let _ = std::thread::Builder::new().name("wsicon-trim".into()).spawn(move || trim_dir(&t, DISK_CAP_BYTES));
        Some(d)
    })
    .as_ref()
}

fn thumb_key(path: &str, mtime_ns: u128, len: u64, size: u32) -> String {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(format!("v1|{}|{mtime_ns}|{len}|{size}", path.to_lowercase()).as_bytes());
    format!("{h:x}")
}

fn thumb_bytes(path: &str, size: u32) -> Option<Vec<u8>> {
    let file = (|| {
        let meta = std::fs::metadata(path).ok()?;
        let ns = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        let key = thumb_key(path, ns, meta.len(), size);
        Some(thumb_dir()?.join(&key[..2]).join(format!("{key}.img")))
    })();
    if let Some(f) = &file {
        if let Ok(b) = std::fs::read(f) {
            if !b.is_empty() {
                return Some(b);
            }
        }
    }
    let (w, h, px) = path_icon_px(path, size, true).or_else(|| decode_small(path, size))?;
    let bytes = encode_thumb(w, h, &px)?;
    if let Some(f) = file {
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
            let tmp = f.with_extension(format!("tmp{}", std::process::id()));
            if std::fs::write(&tmp, &bytes).is_ok() && std::fs::rename(&tmp, &f).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
    Some(bytes)
}

/// Fallback when the shell has no picture: decode with the `image` crate and downscale.
fn decode_small(path: &str, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let t = img.thumbnail(size, size).to_rgba8();
    Some((t.width(), t.height(), t.into_raw()))
}

/// JPEG for opaque pictures, PNG when any pixel is transparent.
fn encode_thumb(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255) {
        return encode_png(w, h, rgba);
    }
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for p in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&p[..3]);
    }
    let mut out = Vec::with_capacity(rgb.len() / 8 + 256);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
        .encode(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

/// Keep the cache under `cap` bytes: delete the oldest files first (down to 80%).
fn trim_dir(dir: &Path, cap: u64) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for sub in rd.flatten() {
        let Ok(inner) = std::fs::read_dir(sub.path()) else { continue };
        for f in inner.flatten() {
            if let Ok(m) = f.metadata() {
                files.push((m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), f.path()));
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
        let any = buf[..info.buffer_size()].as_chunks::<4>().0.iter().any(|p| p[3] != 0);
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
        let left: u64 = std::fs::read_dir(&sub).unwrap().flatten().map(|f| f.metadata().unwrap().len()).sum();
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
        let cached = render_aumid("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", 32).expect("cached");
        assert_eq!(png, cached);
    }
}
