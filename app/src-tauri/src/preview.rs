//! Space preview (Quick Look): `preview_info` command + `wsfile:` protocol.
//!
//! `preview_info` classifies a file by extension and returns what the pane needs: size / dates, and for
//! images the header dimensions + EXIF, for text the first 256 KB decoded. Nothing is decoded in full.
//! `wsfile:` streams image / video / audio bytes with HTTP Range support (so `<video>` can seek). It only
//! serves extensions that classify as media, which is narrower than Tauri's asset protocol (a glob scope).

use crate::shell::icons::query_param;
use serde::Serialize;
use std::borrow::Cow;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;
use tauri::http::{Request, Response};
use tauri::UriSchemeResponder;

/// Text preview read cap.
pub const TEXT_CAP: usize = 256 * 1024;
/// Images above this are shown through the shell thumbnail instead of the raw bytes.
pub const IMAGE_DIRECT_MAX: u64 = 20 * 1024 * 1024;
/// `wsfile:` answers a request without Range in one piece up to this size, else with the first chunk.
const FULL_MAX: u64 = 24 * 1024 * 1024;
/// One ranged response never carries more than this.
const CHUNK_MAX: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Video,
    Audio,
    Text,
    Folder,
    Other,
}

const WEB_IMAGES: &[&str] = &["jpg", "jpeg", "jfif", "png", "gif", "webp", "bmp", "svg", "ico", "avif"];
/// Images WebView2 cannot decode; shown through the Windows shell thumbnail.
const SHELL_IMAGES: &[&str] =
    &["heic", "heif", "tif", "tiff", "jxl", "psd", "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf"];
const VIDEO: &[&str] = &["mp4", "m4v", "webm", "mkv", "mov", "ogv", "avi", "wmv"];
const AUDIO: &[&str] = &["mp3", "wav", "flac", "ogg", "oga", "m4a", "aac", "opus", "wma"];
const TEXT: &[&str] = &[
    "txt", "md", "markdown", "log", "json", "jsonc", "toml", "yaml", "yml", "xml", "csv", "tsv", "ini", "cfg", "conf",
    "env", "properties", "rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "css", "scss", "html", "htm", "vue",
    "svelte", "c", "h", "cpp", "hpp", "cc", "cs", "java", "kt", "go", "rb", "php", "swift", "lua", "sh", "bash",
    "bat", "cmd", "ps1", "sql", "gitignore", "lock", "tex", "srt", "vtt", "diff", "patch",
];

pub fn ext_of(path: &str) -> String {
    Path::new(path).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn classify(ext: &str) -> Kind {
    let e = ext;
    if WEB_IMAGES.contains(&e) || SHELL_IMAGES.contains(&e) {
        Kind::Image
    } else if VIDEO.contains(&e) {
        Kind::Video
    } else if AUDIO.contains(&e) {
        Kind::Audio
    } else if TEXT.contains(&e) {
        Kind::Text
    } else {
        Kind::Other
    }
}

fn mime_of(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wma" => "audio/x-ms-wma",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct ExifInfo {
    /// "2024-05-03 14:22:10" (camera local time).
    pub date: Option<String>,
    pub camera: Option<String>,
    /// Does the file carry GPS coordinates? (The coordinates themselves are not shown.)
    pub gps: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct ImageInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// true: show the shell thumbnail instead of the file bytes (huge file or a format WebView2 cannot decode).
    pub thumb: bool,
    /// null: no EXIF block at all.
    pub exif: Option<ExifInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextInfo {
    pub content: String,
    /// The file is longer than what was read.
    pub truncated: bool,
    pub encoding: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewInfo {
    pub kind: Kind,
    pub name: String,
    pub path: String,
    pub ext: String,
    pub size: Option<u64>,
    /// Unix ms.
    pub mtime: Option<i64>,
    pub ctime: Option<i64>,
    pub image: Option<ImageInfo>,
    pub text: Option<TextInfo>,
}

fn unix_ms(t: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    let d = t.ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(d.as_millis()).ok()
}

// ---- text -----------------------------------------------------------------

/// Decode a text prefix: UTF-8 / UTF-16 BOM, else UTF-8, else Latin-1. `None` = binary (NUL bytes without a
/// UTF-16 BOM). A multi-byte character cut by the read cap is dropped, not turned into U+FFFD.
pub fn decode_text(bytes: &[u8], truncated: bool) -> Option<(String, &'static str)> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Some((utf8_prefix(rest, truncated).0, "UTF-8 BOM"));
    }
    for (bom, le, name) in [([0xFF, 0xFE], true, "UTF-16 LE"), ([0xFE, 0xFF], false, "UTF-16 BE")] {
        if let Some(rest) = bytes.strip_prefix(&bom) {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) })
                .collect();
            return Some((String::from_utf16_lossy(&units), name));
        }
    }
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    let (s, valid) = utf8_prefix(bytes, truncated);
    if valid {
        Some((s, "UTF-8"))
    } else {
        Some((bytes.iter().map(|&b| b as char).collect(), "Latin-1"))
    }
}

/// (text, was valid UTF-8). With `truncated`, an incomplete character at the very end is not an error.
fn utf8_prefix(bytes: &[u8], truncated: bool) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_owned(), true),
        Err(e) if truncated && e.error_len().is_none() => {
            (String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned(), true)
        }
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), false),
    }
}

fn read_text(path: &Path, size: u64) -> Option<TextInfo> {
    let mut buf = Vec::with_capacity(TEXT_CAP.min(size as usize) + 4);
    File::open(path).ok()?.take(TEXT_CAP as u64).read_to_end(&mut buf).ok()?;
    let truncated = size > TEXT_CAP as u64;
    let (content, encoding) = decode_text(&buf, truncated)?;
    Some(TextInfo { content, truncated, encoding })
}

// ---- images ---------------------------------------------------------------

fn ascii(e: &exif::Exif, tag: exif::Tag) -> Option<String> {
    match &e.get_field(tag, exif::In::PRIMARY)?.value {
        exif::Value::Ascii(v) => {
            let s = String::from_utf8_lossy(v.first()?).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_owned();
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

pub fn exif_info(e: &exif::Exif) -> ExifInfo {
    let date = [exif::Tag::DateTimeOriginal, exif::Tag::DateTimeDigitized, exif::Tag::DateTime]
        .into_iter()
        .find_map(|t| ascii(e, t))
        .map(|d| {
            // "2024:05:03 14:22:10" -> "2024-05-03 14:22:10"
            let (day, time) = d.split_once(' ').unwrap_or((&d, ""));
            format!("{} {}", day.replace(':', "-"), time).trim().to_owned()
        });
    let make = ascii(e, exif::Tag::Make);
    let model = ascii(e, exif::Tag::Model);
    let camera = match (make, model) {
        (Some(a), Some(b)) if b.to_lowercase().starts_with(&a.to_lowercase()) => Some(b),
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, b) => a.or(b),
    };
    let gps = e.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY).is_some()
        && e.get_field(exif::Tag::GPSLongitude, exif::In::PRIMARY).is_some();
    ExifInfo { date, camera, gps }
}

fn image_info(path: &Path, ext: &str, size: u64) -> ImageInfo {
    let dims = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    let exif = if matches!(ext, "jpg" | "jpeg" | "jfif" | "tif" | "tiff" | "heic" | "heif" | "png" | "webp" | "avif") {
        File::open(path)
            .ok()
            .and_then(|f| exif::Reader::new().read_from_container(&mut BufReader::new(f)).ok())
            .map(|e| exif_info(&e))
    } else {
        None
    };
    ImageInfo {
        width: dims.map(|d| d.0),
        height: dims.map(|d| d.1),
        thumb: size > IMAGE_DIRECT_MAX || SHELL_IMAGES.contains(&ext),
        exif,
    }
}

// ---- command --------------------------------------------------------------

pub fn read_info(path: &str) -> Result<PreviewInfo, String> {
    let p = Path::new(path);
    let md = std::fs::metadata(p).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "File not found".to_string(),
        _ => format!("Cannot read file: {e}"),
    })?;
    let ext = ext_of(path);
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_owned());
    let mut kind = if md.is_dir() { Kind::Folder } else { classify(&ext) };
    let size = (!md.is_dir()).then_some(md.len());
    let (mut image, mut text) = (None, None);
    match kind {
        Kind::Image => image = Some(image_info(p, &ext, md.len())),
        Kind::Text => {
            text = read_text(p, md.len());
            if text.is_none() {
                kind = Kind::Other; // binary content under a text extension
            }
        }
        _ => {}
    }
    Ok(PreviewInfo {
        kind,
        name,
        path: path.to_owned(),
        ext,
        size,
        mtime: unix_ms(md.modified()),
        ctime: unix_ms(md.created()),
        image,
        text,
    })
}

/// Runs on a worker thread (never the main thread, so a slow disk or a huge file cannot freeze the popup).
#[tauri::command]
pub async fn preview_info(path: String) -> Result<PreviewInfo, String> {
    tauri::async_runtime::spawn_blocking(move || read_info(&path)).await.map_err(|e| e.to_string())?
}

// ---- wsfile: protocol ---------------------------------------------------------

/// `Range: bytes=a-b | a- | -n` against a file of `len` bytes -> inclusive (start, end), clipped to `CHUNK_MAX`.
/// `None`: not satisfiable (416) or not a single byte range.
pub fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        if n == 0 {
            return None;
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() { len - 1 } else { b.parse::<u64>().ok()?.min(len - 1) };
        (start, end)
    };
    if start > end || start >= len {
        return None;
    }
    Some((start, end.min(start + CHUNK_MAX - 1)))
}

fn status_only(code: u16) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(code)
        .header("Access-Control-Allow-Origin", "*")
        .body(Cow::Borrowed(&[][..]))
        .expect("static response")
}

fn read_range(path: &Path, start: u64, len: u64) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(len as usize);
    f.take(len).read_to_end(&mut buf)?;
    Ok(buf)
}

fn respond_file(path: &str, range: Option<&str>) -> Response<Cow<'static, [u8]>> {
    let ext = ext_of(path);
    if !matches!(classify(&ext), Kind::Image | Kind::Video | Kind::Audio) {
        return status_only(403);
    }
    let p = Path::new(path);
    let Ok(md) = std::fs::metadata(p) else { return status_only(404) };
    if !md.is_file() {
        return status_only(404);
    }
    let total = md.len();
    let b = Response::builder()
        .header("Access-Control-Allow-Origin", "*")
        .header("Accept-Ranges", "bytes")
        .header("Content-Type", mime_of(&ext))
        .header("Cache-Control", "no-store");
    let (start, end, partial) = match range {
        Some(h) => match parse_range(h, total) {
            Some((s, e)) => (s, e, true),
            None => {
                return Response::builder()
                    .status(416)
                    .header("Content-Range", format!("bytes */{total}"))
                    .header("Access-Control-Allow-Origin", "*")
                    .body(Cow::Borrowed(&[][..]))
                    .expect("static response")
            }
        },
        None if total > FULL_MAX => (0, CHUNK_MAX.min(total) - 1, true),
        None => (0, total.saturating_sub(1), false),
    };
    let len = if total == 0 { 0 } else { end - start + 1 };
    let Ok(body) = read_range(p, start, len) else { return status_only(404) };
    let b = b.header("Content-Length", body.len().to_string());
    let r = if partial {
        b.status(206).header("Content-Range", format!("bytes {start}-{end}/{total}"))
    } else {
        b.status(200)
    };
    r.body(Cow::Owned(body)).expect("response")
}

pub fn serve(req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let path = query_param(req.uri().query().unwrap_or(""), "p").filter(|p| !p.is_empty());
    let range = req.headers().get("range").and_then(|v| v.to_str().ok()).map(str::to_owned);
    let Some(path) = path else { return responder.respond(status_only(404)) };
    // A thread per request: the webview aborts superseded loads, and disk reads must not block the runtime.
    let _ = std::thread::Builder::new().name("wsfile".into()).spawn(move || {
        responder.respond(respond_file(&path, range.as_deref()));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn classifies_by_extension() {
        assert_eq!(classify("jpg"), Kind::Image);
        assert_eq!(classify("heic"), Kind::Image);
        assert_eq!(classify("mkv"), Kind::Video);
        assert_eq!(classify("flac"), Kind::Audio);
        assert_eq!(classify("rs"), Kind::Text);
        assert_eq!(classify("pdf"), Kind::Other);
        assert_eq!(classify(""), Kind::Other);
        assert_eq!(ext_of(r"C:\a\B.JPG"), "jpg");
    }

    #[test]
    fn range_forms() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=900-5000", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=5-2", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 1000), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
        // an open-ended range over a huge file is clipped to one chunk
        assert_eq!(parse_range("bytes=0-", 1 << 40), Some((0, CHUNK_MAX - 1)));
    }

    #[test]
    fn decodes_boms_and_fallbacks() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFhi", false), Some(("hi".into(), "UTF-8 BOM")));
        assert_eq!(decode_text(b"\xFF\xFEh\0i\0", false), Some(("hi".into(), "UTF-16 LE")));
        assert_eq!(decode_text(b"\xFE\xFF\0h\0i", false), Some(("hi".into(), "UTF-16 BE")));
        assert_eq!(decode_text("h\u{e9}llo".as_bytes(), false), Some(("h\u{e9}llo".into(), "UTF-8")));
        // invalid UTF-8 -> Latin-1
        assert_eq!(decode_text(b"caf\xE9", false), Some(("caf\u{e9}".into(), "Latin-1")));
        // binary
        assert_eq!(decode_text(b"MZ\0\0\x03", false), None);
        // a character cut by the read cap is dropped when truncated, an error otherwise
        let cut = &"a\u{e9}".as_bytes()[..2];
        assert_eq!(decode_text(cut, true), Some(("a".into(), "UTF-8")));
        assert_eq!(decode_text(cut, false).map(|d| d.1), Some("Latin-1"));
    }

    fn jpeg_with_exif() -> Vec<u8> {
        use exif::{Field, In, Rational, Tag, Value};
        let fields = [
            Field { tag: Tag::Make, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Canon".to_vec()]) },
            Field { tag: Tag::Model, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Canon EOS R5".to_vec()]) },
            Field { tag: Tag::DateTimeOriginal, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"2024:05:03 14:22:10".to_vec()]) },
            Field { tag: Tag::GPSLatitude, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 48, denom: 1 }; 3]) },
            Field { tag: Tag::GPSLongitude, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 2, denom: 1 }; 3]) },
        ];
        let mut w = exif::experimental::Writer::new();
        for f in &fields {
            w.push_field(f);
        }
        let mut tiff = std::io::Cursor::new(Vec::new());
        w.write(&mut tiff, false).expect("write exif");
        let tiff = tiff.into_inner();

        let mut jpg = Vec::new();
        let img = image::RgbImage::from_pixel(7, 5, image::Rgb([200, 30, 30]));
        img.write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg).expect("encode");
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpg[2..]);
        out
    }

    #[test]
    fn image_dimensions_and_exif_without_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("IMG_0001.JPG");
        std::fs::write(&p, jpeg_with_exif()).unwrap();
        let info = read_info(p.to_str().unwrap()).unwrap();
        assert_eq!(info.kind, Kind::Image);
        let img = info.image.unwrap();
        assert_eq!((img.width, img.height), (Some(7), Some(5)));
        assert!(!img.thumb);
        let ex = img.exif.expect("exif block");
        assert_eq!(ex.date.as_deref(), Some("2024-05-03 14:22:10"));
        assert_eq!(ex.camera.as_deref(), Some("Canon EOS R5"));
        assert!(ex.gps);
    }

    #[test]
    fn png_has_dimensions_but_no_exif() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image::RgbImage::new(11, 3).save(&p).unwrap();
        let img = read_info(p.to_str().unwrap()).unwrap().image.unwrap();
        assert_eq!((img.width, img.height), (Some(11), Some(3)));
        assert!(img.exif.is_none());
    }

    #[test]
    fn text_is_capped_and_binary_becomes_other() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.log");
        let line = "0123456789abcdef\n";
        std::fs::write(&big, line.repeat(TEXT_CAP / line.len() + 500)).unwrap();
        let t = read_info(big.to_str().unwrap()).unwrap().text.unwrap();
        assert!(t.truncated);
        assert_eq!(t.content.len(), TEXT_CAP);

        let bin = dir.path().join("fake.txt");
        std::fs::File::create(&bin).unwrap().write_all(b"MZ\x90\0\x03\0\0\0").unwrap();
        let i = read_info(bin.to_str().unwrap()).unwrap();
        assert_eq!(i.kind, Kind::Other);
        assert!(i.text.is_none());
    }

    #[test]
    fn missing_file_and_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_info(dir.path().join("nope.txt").to_str().unwrap()).unwrap_err(), "File not found");
        let d = read_info(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(d.kind, Kind::Folder);
        assert_eq!(d.size, None);
    }

    #[test]
    fn wsfile_serves_media_only_with_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path().join("clip.mp4");
        std::fs::write(&v, (0..=255u8).collect::<Vec<_>>()).unwrap();
        let r = respond_file(v.to_str().unwrap(), Some("bytes=10-19"));
        assert_eq!(r.status(), 206);
        assert_eq!(r.headers()["content-range"], "bytes 10-19/256");
        assert_eq!(r.body().as_ref(), &(10..=19u8).collect::<Vec<_>>()[..]);
        let full = respond_file(v.to_str().unwrap(), None);
        assert_eq!((full.status().as_u16(), full.body().len()), (200, 256));
        assert_eq!(respond_file(v.to_str().unwrap(), Some("bytes=999-")).status(), 416);

        let secret = dir.path().join("passwords.kdbx");
        std::fs::write(&secret, b"x").unwrap();
        assert_eq!(respond_file(secret.to_str().unwrap(), None).status(), 403);
        assert_eq!(respond_file(dir.path().join("none.mp4").to_str().unwrap(), None).status(), 404);
    }
}
