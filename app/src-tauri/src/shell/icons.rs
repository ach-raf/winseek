//! `wsicon:` protocol: shell icons and thumbnails as PNG, rendered on worker threads (never the main thread).

use super::{shell_item, wide, ComGuard};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
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
    is_dir: bool,
    size: u32,
    thumb: bool,
    responder: UriSchemeResponder,
}

static POOL: OnceLock<Sender<Job>> = OnceLock::new();
static CACHE: Mutex<Option<HashMap<String, Arc<Vec<u8>>>>> = Mutex::new(None);

fn pool() -> &'static Sender<Job> {
    POOL.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..WORKERS {
            let rx = rx.clone();
            let _ = std::thread::Builder::new().name(format!("wsicon-{i}")).spawn(move || {
                let _com = ComGuard::init();
                loop {
                    let job = match rx.lock() {
                        Ok(g) => g.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    let png = render(&job.path, job.is_dir, job.size, job.thumb);
                    job.responder.respond(response(png));
                }
            });
        }
        tx
    })
}

fn response(png: Option<Arc<Vec<u8>>>) -> Response<Cow<'static, [u8]>> {
    let b = Response::builder().header("Access-Control-Allow-Origin", "*");
    match png {
        Some(p) => b
            .status(200)
            .header("Content-Type", "image/png")
            .header("Cache-Control", "max-age=3600")
            .body(Cow::Owned(p.as_ref().clone())),
        None => b.status(404).body(Cow::Borrowed(&[][..])),
    }
    .expect("static response")
}

fn percent_decode(s: &str) -> String {
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

fn query_param(q: &str, key: &str) -> Option<String> {
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

pub fn handle(req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let q = req.uri().query().unwrap_or("");
    let Some(path) = query_param(q, "p").filter(|p| !p.is_empty()) else {
        return responder.respond(response(None));
    };
    let is_dir = query_param(q, "d").as_deref() == Some("1");
    let thumb = query_param(q, "t").as_deref() == Some("1");
    let size = query_param(q, "s").and_then(|s| s.parse::<u32>().ok()).unwrap_or(32).clamp(16, 256);
    let job = Job { path, is_dir, size, thumb, responder };
    if let Err(e) = pool().send(job) {
        e.0.responder.respond(response(None));
    }
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
    let png = if per_path { path_icon_png(path, size, thumb) } else { generic_icon_png(&ext, is_dir, size) }?;
    let png = Arc::new(png);
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

/// Per-path icon or thumbnail through IShellItemImageFactory.
pub(super) fn path_icon_png(path: &str, size: u32, thumb: bool) -> Option<Vec<u8>> {
    let item = shell_item(path).ok()?;
    let f: IShellItemImageFactory = item.cast().ok()?;
    let sz = SIZE { cx: size as i32, cy: size as i32 };
    // SAFETY: GetImage returns an owned HBITMAP that BitmapGuard deletes.
    let hbm = unsafe {
        if thumb {
            f.GetImage(sz, SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY).or_else(|_| f.GetImage(sz, SIIGBF_RESIZETOFIT))
        } else {
            f.GetImage(sz, SIIGBF_RESIZETOFIT | SIIGBF_ICONONLY)
        }
    }
    .ok()?;
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
    encode_png(w, h, &px)
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
    fn query_decoding() {
        let q = "p=C%3A%5Cdir%2Fa%20b%C3%A9.txt&d=0&s=40";
        assert_eq!(query_param(q, "p").unwrap(), "C:\\dir/a b\u{e9}.txt");
        assert_eq!(query_param(q, "s").unwrap(), "40");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%2"), "a%2");
    }
}
