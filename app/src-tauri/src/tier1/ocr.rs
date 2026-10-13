//! Screenshot OCR for the Tier 1 worker, via `Windows.Media.Ocr` (WinRT).
//!
//! Runs on the worker thread only (it blocks on the async operations), never on the UI
//! thread and never in searchd. No engine / no recognizer language installed is reported as
//! [`OcrError::Unavailable`]; the caller just skips OCR (Tesseract fallback: not implemented).

use std::io::Cursor;
use std::sync::OnceLock;

use serde::Serialize;

/// Cap on OCR text kept per image (FTS body).
pub const MAX_TEXT_CHARS: usize = 20_000;
/// Cap on stored line rectangles.
pub const MAX_LINES: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OcrLine {
    #[serde(rename = "t")]
    pub text: String,
    /// `[x, y, w, h]` in pixels of the original image.
    #[serde(rename = "r")]
    pub rect: [i32; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OcrText {
    pub text: String,
    pub lines: Vec<OcrLine>,
    /// Original image size the rectangles refer to.
    pub width: u32,
    pub height: u32,
}

impl OcrText {
    /// True when no usable words were recognised.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Locator JSON for the store (`{"ocr":{"w":..,"h":..,"lines":[{"t":..,"r":[x,y,w,h]}]}}`),
    /// kept for the preview's future bounding-box highlight.
    pub fn locator_json(&self) -> String {
        #[derive(Serialize)]
        struct Ocr<'a> {
            w: u32,
            h: u32,
            lines: &'a [OcrLine],
        }
        #[derive(Serialize)]
        struct Wrap<'a> {
            ocr: Ocr<'a>,
        }
        let n = self.lines.len().min(MAX_LINES);
        serde_json::to_string(&Wrap { ocr: Ocr { w: self.width, h: self.height, lines: &self.lines[..n] } })
            .unwrap_or_default()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum OcrError {
    /// No OCR engine / recognizer language on this machine.
    Unavailable,
    Failed(String),
}

pub trait Ocr {
    fn recognize(&mut self, image_bytes: &[u8]) -> Result<OcrText, OcrError>;
}

// ---------------------------------------------------------------- detection (pure)

/// Display resolutions a full-screen screenshot commonly has (landscape; portrait is derived).
const COMMON_RESOLUTIONS: &[(u32, u32)] = &[
    (1280, 720),
    (1280, 800),
    (1366, 768),
    (1440, 900),
    (1536, 864),
    (1600, 900),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2048, 1152),
    (2256, 1504),
    (2560, 1080),
    (2560, 1440),
    (2560, 1600),
    (2880, 1800),
    (3000, 2000),
    (3440, 1440),
    (3840, 1600),
    (3840, 2160),
    (5120, 1440),
    (5120, 2880),
];

/// Folder names that hold screenshots (lower-case, apostrophes normalised to `'`).
const SCREENSHOT_DIRS: &[&str] = &[
    "screenshots",
    "screenshot",
    "screen captures",
    "screen shots",
    "captures d'écran",
    "captures d'ecran",
    "capturas de pantalla",
    "bildschirmfotos",
    "schermafbeeldingen",
    "schermopnamen",
    "istantanee",
    "screenshoty",
];

/// File-name prefixes of screenshot tools (lower-case, apostrophes normalised).
const SCREENSHOT_PREFIXES: &[&str] = &[
    "screenshot",
    "screen shot",
    "screen capture",
    "capture",
    "snip",
    "capture d'écran",
    "capture d'ecran",
    "captura de pantalla",
    "bildschirmfoto",
    "schermafbeelding",
];

fn norm(s: &str) -> String {
    s.to_lowercase().replace(['\u{2019}', '\u{2018}', '\u{02bc}', '`'], "'")
}

fn is_png(path: &str) -> bool {
    path.rsplit('.').next().is_some_and(|e| e.eq_ignore_ascii_case("png")) && path.contains('.')
}

fn is_common_resolution(w: u32, h: u32) -> bool {
    COMMON_RESOLUTIONS.iter().any(|&(a, b)| (w, h) == (a, b) || (w, h) == (b, a))
}

/// Does this image look like a screenshot? True when any of:
/// - the path is under `screenshots_dir` (the known Screenshots folder) or has a path
///   component with a well-known (localized) screenshot folder name;
/// - the file name starts with a screenshot-tool pattern (`Screenshot*`, `Capture*`,
///   `Screen Shot*`, `Snip*`, `Capture d’écran*`, ...);
/// - it is a PNG whose pixel size equals a common display resolution.
pub fn is_screenshot(path: &str, dims: Option<(u32, u32)>, screenshots_dir: Option<&str>) -> bool {
    let p = path.replace('/', "\\");
    if let Some(dir) = screenshots_dir {
        let mut d = dir.replace('/', "\\").trim_end_matches('\\').to_lowercase();
        if !d.is_empty() {
            d.push('\\');
            if p.to_lowercase().starts_with(&d) {
                return true;
            }
        }
    }
    let mut parts: Vec<&str> = p.split('\\').collect();
    let name = parts.pop().unwrap_or("");
    if parts.iter().any(|c| SCREENSHOT_DIRS.contains(&norm(c).as_str())) {
        return true;
    }
    let n = norm(name);
    if SCREENSHOT_PREFIXES.iter().any(|pre| n.starts_with(pre)) {
        return true;
    }
    is_png(name) && dims.is_some_and(|(w, h)| is_common_resolution(w, h))
}

/// Largest size with the same aspect whose long side is at most `max` (never upscales,
/// never returns a zero side).
pub fn fit_dimensions(w: u32, h: u32, max: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max || long == 0 || max == 0 {
        return (w, h);
    }
    let scale = |v: u32| ((u64::from(v) * u64::from(max) + u64::from(long) / 2) / u64::from(long)).max(1) as u32;
    (scale(w), scale(h))
}

/// Pixel size without decoding the whole image.
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

/// The known Screenshots folder, resolved once.
pub fn screenshots_dir() -> Option<&'static str> {
    static DIR: OnceLock<Option<String>> = OnceLock::new();
    DIR.get_or_init(super::scopes::screenshots_dir).as_deref()
}

/// Builds the stored text from raw line strings: trims, drops empties, caps the length.
fn join_lines(lines: &[OcrLine]) -> String {
    let mut out = String::new();
    for l in lines {
        let t = l.text.trim();
        if t.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(t);
        if out.chars().count() >= MAX_TEXT_CHARS {
            return out.chars().take(MAX_TEXT_CHARS).collect();
        }
    }
    out
}

// ---------------------------------------------------------------- Windows.Media.Ocr

pub use win::WinOcr;

mod win {
    use super::*;
    use windows::Globalization::Language;
    use windows::Graphics::Imaging::{
        BitmapAlphaMode, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform, ColorManagementMode,
        ExifOrientationMode, SoftwareBitmap,
    };
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    /// Lazily created engine; `unavailable` is sticky for the process lifetime once tried.
    #[derive(Default)]
    pub struct WinOcr {
        engine: Option<OcrEngine>,
        unavailable: bool,
        com_ready: bool,
    }

    impl WinOcr {
        pub fn new() -> Self {
            Self::default()
        }

        fn engine(&mut self) -> Result<&OcrEngine, OcrError> {
            if !self.com_ready {
                // MTA so `.join()` can block; an already-initialised thread (RPC_E_CHANGED_MODE) is fine too.
                // SAFETY: plain COM init on the calling worker thread, never uninitialised (thread lives for the process).
                let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                self.com_ready = true;
            }
            if self.unavailable {
                return Err(OcrError::Unavailable);
            }
            if self.engine.is_none() {
                match create_engine() {
                    Some(e) => self.engine = Some(e),
                    None => {
                        self.unavailable = true;
                        return Err(OcrError::Unavailable);
                    }
                }
            }
            Ok(self.engine.as_ref().expect("engine set"))
        }
    }

    fn create_engine() -> Option<OcrEngine> {
        if let Ok(e) = OcrEngine::TryCreateFromUserProfileLanguages() {
            return Some(e);
        }
        let langs = OcrEngine::AvailableRecognizerLanguages().ok()?;
        let first: Language = langs.into_iter().next()?;
        OcrEngine::TryCreateFromLanguage(&first).ok()
    }

    fn err(what: &str, e: windows::core::Error) -> OcrError {
        OcrError::Failed(format!("{what}: {e}"))
    }

    fn decode(bytes: &[u8], max_dim: u32) -> Result<(SoftwareBitmap, u32, u32), OcrError> {
        let stream = InMemoryRandomAccessStream::new().map_err(|e| err("stream", e))?;
        let writer = DataWriter::CreateDataWriter(&stream).map_err(|e| err("writer", e))?;
        writer.WriteBytes(bytes).map_err(|e| err("write", e))?;
        writer.StoreAsync().map_err(|e| err("store", e))?.join().map_err(|e| err("store", e))?;
        writer.DetachStream().map_err(|e| err("detach", e))?;
        stream.Seek(0).map_err(|e| err("seek", e))?;
        let decoder = BitmapDecoder::CreateAsync(&stream)
            .map_err(|e| err("decoder", e))?
            .join()
            .map_err(|e| err("decode", e))?;
        let (w, h) = (decoder.PixelWidth().map_err(|e| err("size", e))?, decoder.PixelHeight().map_err(|e| err("size", e))?);
        let (sw, sh) = fit_dimensions(w, h, max_dim);
        let transform = BitmapTransform::new().map_err(|e| err("transform", e))?;
        if (sw, sh) != (w, h) {
            transform.SetScaledWidth(sw).map_err(|e| err("transform", e))?;
            transform.SetScaledHeight(sh).map_err(|e| err("transform", e))?;
            transform.SetInterpolationMode(BitmapInterpolationMode::Fant).map_err(|e| err("transform", e))?;
        }
        let bmp = decoder
            .GetSoftwareBitmapTransformedAsync(
                BitmapPixelFormat::Bgra8,
                BitmapAlphaMode::Premultiplied,
                &transform,
                ExifOrientationMode::IgnoreExifOrientation,
                ColorManagementMode::DoNotColorManage,
            )
            .map_err(|e| err("bitmap", e))?
            .join()
            .map_err(|e| err("bitmap", e))?;
        Ok((bmp, w, h))
    }

    /// Re-encode through the `image` crate (WebP etc. that WIC may lack) as PNG.
    fn to_png(bytes: &[u8]) -> Option<Vec<u8>> {
        let img = image::load_from_memory(bytes).ok()?;
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
        Some(out)
    }

    impl Ocr for WinOcr {
        fn recognize(&mut self, bytes: &[u8]) -> Result<OcrText, OcrError> {
            let engine = self.engine()?.clone();
            let max = OcrEngine::MaxImageDimension().map_err(|e| err("max dim", e))?;
            let (bmp, w, h) = match decode(bytes, max) {
                Ok(v) => v,
                Err(first) => match to_png(bytes) {
                    Some(png) => decode(&png, max).map_err(|_| first)?,
                    None => return Err(first),
                },
            };
            let result = engine
                .RecognizeAsync(&bmp)
                .map_err(|e| err("recognize", e))?
                .join()
                .map_err(|e| err("recognize", e))?;
            // Rectangles are in scaled-bitmap pixels; map back to the original size.
            let bw = bmp.PixelWidth().map_err(|e| err("size", e))?.max(1) as f32;
            let bh = bmp.PixelHeight().map_err(|e| err("size", e))?.max(1) as f32;
            let (fx, fy) = (w as f32 / bw, h as f32 / bh);
            let mut lines = Vec::new();
            for line in result.Lines().map_err(|e| err("lines", e))? {
                let text = line.Text().map_err(|e| err("line", e))?.to_string();
                // Union of the word boxes = the line rect.
                let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for word in line.Words().map_err(|e| err("words", e))? {
                    let r = word.BoundingRect().map_err(|e| err("rect", e))?;
                    x0 = x0.min(r.X);
                    y0 = y0.min(r.Y);
                    x1 = x1.max(r.X + r.Width);
                    y1 = y1.max(r.Y + r.Height);
                }
                if x0 > x1 {
                    continue;
                }
                lines.push(OcrLine {
                    text,
                    rect: [(x0 * fx) as i32, (y0 * fy) as i32, ((x1 - x0) * fx).ceil() as i32, ((y1 - y0) * fy).ceil() as i32],
                });
            }
            Ok(OcrText { text: join_lines(&lines), lines, width: w, height: h })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOTS: &str = r"C:\Users\me\Pictures\Screenshots";

    #[test]
    fn detects_by_known_folder_and_localized_dirs() {
        assert!(is_screenshot(r"C:\Users\me\Pictures\Screenshots\a.jpg", None, Some(SHOTS)));
        assert!(is_screenshot(r"c:\users\me\pictures\screenshots\sub\a.jpg", None, Some(&format!("{SHOTS}\\"))));
        assert!(!is_screenshot(r"C:\Users\me\Pictures\ScreenshotsOld\a.jpg", None, Some(SHOTS)));
        assert!(is_screenshot(r"C:\Users\me\Images\Captures d’écran\x.jpg", None, None));
        assert!(is_screenshot(r"C:\Users\me\Images\Captures d'écran\x.jpg", None, None));
        assert!(is_screenshot(r"D:\Capturas de pantalla\x.jpg", None, None));
        assert!(!is_screenshot(r"C:\Users\me\Pictures\holiday\x.jpg", None, Some(SHOTS)));
    }

    #[test]
    fn detects_by_name_pattern() {
        for n in [
            "Screenshot 2026-01-02 101010.png",
            "screenshot_1.jpg",
            "Capture.PNG",
            "Capture d’écran 2026-01-02 à 10.10.10.png",
            "Capture d'écran du 2026.png",
            "Screen Shot 2026-01-02 at 10.10.10.png",
            "Snip.png",
            "Snipping Tool 1.png",
        ] {
            assert!(is_screenshot(&format!(r"C:\x\{n}"), None, None), "{n}");
        }
        assert!(!is_screenshot(r"C:\x\IMG_2041.jpg", Some((4032, 3024)), None));
        assert!(!is_screenshot(r"C:\x\holiday capture.jpg", None, None), "prefix only");
    }

    #[test]
    fn detects_png_with_display_resolution() {
        assert!(is_screenshot(r"C:\x\a.png", Some((1920, 1080)), None));
        assert!(is_screenshot(r"C:\x\a.PNG", Some((1080, 1920)), None), "portrait");
        assert!(is_screenshot(r"C:\x\a.png", Some((2560, 1440)), None));
        assert!(!is_screenshot(r"C:\x\a.png", Some((1921, 1080)), None));
        assert!(!is_screenshot(r"C:\x\a.jpg", Some((1920, 1080)), None), "png only");
        assert!(!is_screenshot(r"C:\x\a.png", None, None));
        assert!(!is_screenshot(r"C:\x\png", Some((1920, 1080)), None), "no extension");
    }

    #[test]
    fn fit_dimensions_math() {
        assert_eq!(fit_dimensions(1920, 1080, 10_000), (1920, 1080), "never upscales");
        assert_eq!(fit_dimensions(10_000, 5_000, 10_000), (10_000, 5_000));
        assert_eq!(fit_dimensions(20_000, 10_000, 10_000), (10_000, 5_000));
        assert_eq!(fit_dimensions(10_000, 30_000, 10_000), (3_333, 10_000));
        assert_eq!(fit_dimensions(100_000, 3, 10_000), (10_000, 1), "never zero");
        assert_eq!(fit_dimensions(0, 0, 100), (0, 0));
        assert_eq!(fit_dimensions(500, 400, 0), (500, 400));
    }

    #[test]
    fn locator_json_shape_and_text_join() {
        let lines = vec![
            OcrLine { text: " Hello ".into(), rect: [1, 2, 30, 10] },
            OcrLine { text: "  ".into(), rect: [0, 0, 1, 1] },
            OcrLine { text: "World".into(), rect: [1, 14, 40, 10] },
        ];
        let o = OcrText { text: join_lines(&lines), lines, width: 100, height: 50 };
        assert_eq!(o.text, "Hello\nWorld");
        let v: serde_json::Value = serde_json::from_str(&o.locator_json()).unwrap();
        assert_eq!(v["ocr"]["w"], 100);
        assert_eq!(v["ocr"]["lines"][0]["t"], " Hello ");
        assert_eq!(v["ocr"]["lines"][2]["r"], serde_json::json!([1, 14, 40, 10]));
        assert!(!o.is_empty());
        assert!(OcrText::default().is_empty());
    }

    // ---- real engine -------------------------------------------------------------

    /// 5x7 glyphs (rows, MSB-left in the low 5 bits) for the letters the test draws.
    fn glyph(c: char) -> [u8; 7] {
        match c {
            'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
            'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
            'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
            'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
            'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
            'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
            'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
            'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
            'N' => [0b10001, 0b11001, 0b10101, 0b10101, 0b10011, 0b10001, 0b10001],
            'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
            'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
            _ => [0; 7],
        }
    }

    /// White 1920x1080 PNG with black block text `lines` at 12x scale (letters ~84 px tall).
    fn text_png(lines: &[&str]) -> Vec<u8> {
        const S: u32 = 12;
        let mut img = image::RgbImage::from_pixel(1920, 1080, image::Rgb([255, 255, 255]));
        for (li, line) in lines.iter().enumerate() {
            let y0 = 100 + li as u32 * 9 * S * 2;
            for (ci, ch) in line.chars().enumerate() {
                let x0 = 80 + ci as u32 * 7 * S;
                for (ry, row) in glyph(ch).iter().enumerate() {
                    for cx in 0..5u32 {
                        if row & (1 << (4 - cx)) != 0 {
                            for dy in 0..S {
                                for dx in 0..S {
                                    img.put_pixel(x0 + cx * S + dx, y0 + ry as u32 * S + dy, image::Rgb([0, 0, 0]));
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn generated_png_is_detected_as_screenshot() {
        let png = text_png(&["HELLO"]);
        assert_eq!(image_dimensions(&png), Some((1920, 1080)));
        assert!(is_screenshot(r"C:\x\gen.png", image_dimensions(&png), None));
    }

    /// Real Windows.Media.Ocr round trip; skipped (passes) when no recognizer language is installed.
    #[test]
    fn winrt_ocr_reads_large_black_text() {
        let mut ocr = WinOcr::new();
        let png = text_png(&["HELLO WORLD", "VIDEO CODEC"]);
        match ocr.recognize(&png) {
            Err(OcrError::Unavailable) => eprintln!("SKIP: no OCR language installed"),
            Err(e) => panic!("ocr failed: {e:?}"),
            Ok(r) => {
                eprintln!("OCR text: {:?} lines: {:?}", r.text, r.lines);
                assert!(!r.is_empty(), "no text recognised");
                let up = r.text.to_uppercase();
                assert!(up.contains("HELLO") || up.contains("WORLD") || up.contains("VIDEO") || up.contains("CODEC"), "got {up:?}");
                assert_eq!((r.width, r.height), (1920, 1080));
                assert!(r.lines.iter().all(|l| l.rect[2] > 0 && l.rect[3] > 0));
            }
        }
    }
}
