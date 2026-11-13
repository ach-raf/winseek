//! PDF support for Tier 1: text per page and page rendering through pdfium (`pdfium-render`, bound
//! dynamically to a downloaded `pdfium.dll`; PLAN "Scanners, store, retrieval").
//!
//! - [`runtime`] binds the DLL once per process (pdfium keeps global state, so a second bind is an
//!   error). The `thread_safe` feature of pdfium-render serialises the individual calls; the worker
//!   thread does all indexing work and the preview command only renders single pages.
//! - [`read_pdf`] returns the text of every page (at most [`MAX_PAGES`]). A page with fewer than
//!   [`SCANNED_MIN_CHARS`] characters is "scanned": it is rendered to a [`RENDER_LONG_SIDE`] px JPEG so
//!   the caller can OCR it and embed it as an image.
//! - Encrypted (password protected) documents report [`PdfError::Encrypted`].
//!
//! The DLL is pinned in the llama manifest (`pdfium`) and installed under
//! `%LOCALAPPDATA%\winseek\pdfium\<build>\pdfium.dll`; `WINSEEK_PDFIUM` overrides the path (dev).

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use pdfium_render::prelude::*;
use winseek_llama::install::pdfium_path;
use winseek_llama::Manifest;

use super::hitbox::HitBox;

/// Pages [`read_pdf`] returns at most (the agent's whole-document read).
pub const MAX_PAGES: usize = 300;
/// Pages of a document indexed in the first pass: searchable at once.
pub const HEAD_PAGES: u32 = 30;
/// Most pages indexed per document, tail included; later pages are ignored.
pub const TAIL_PAGE_CAP: u32 = 5000;
/// Pages per tail slice (one pass of the background tail work).
pub const TAIL_SLICE: u32 = 30;
/// Larger files are skipped (`too_large`).
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// A page with fewer non-blank characters than this is treated as a scan.
pub const SCANNED_MIN_CHARS: usize = 20;
/// Long side of a rendered scanned page (OCR + image embedding input).
pub const RENDER_LONG_SIDE: u32 = 1024;
const JPEG_QUALITY: u8 = 88;
/// Dev override: path to `pdfium.dll`.
pub const ENV_PDFIUM: &str = "WINSEEK_PDFIUM";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfError {
    /// pdfium.dll is not installed / could not be loaded (retry once it is).
    Unavailable(String),
    /// Password protected.
    Encrypted,
    /// Corrupt or unreadable document.
    Failed(String),
}

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PdfError::Unavailable(m) | PdfError::Failed(m) => write!(f, "{m}"),
            PdfError::Encrypted => write!(f, "encrypted"),
        }
    }
}

// ---------------------------------------------------------------- the runtime

/// Where pdfium.dll is: the `WINSEEK_PDFIUM` override if it points at a file, else the pinned
/// install under `root`.
pub fn locate_dll_in(env: Option<&std::ffi::OsStr>, root: Option<&Path>, manifest: &Manifest) -> Option<PathBuf> {
    if let Some(p) = env.filter(|e| !e.is_empty()).map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    let p = pdfium_path(root?, &manifest.pdfium);
    p.is_file().then_some(p)
}

/// [`locate_dll_in`] for the running app.
pub fn locate_dll() -> Option<PathBuf> {
    locate_dll_in(std::env::var_os(ENV_PDFIUM).as_deref(), super::data_root().as_deref(), &Manifest::embedded())
}

/// Tests bind the dev copy of pdfium through this instead of the process environment, so the
/// "is pdfium installed" checks of other tests stay deterministic.
#[cfg(test)]
pub(crate) static TEST_DLL: OnceLock<PathBuf> = OnceLock::new();

static RUNTIME: OnceLock<Pdfium> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());

/// The process-wide pdfium instance, bound on first use. Fails (without caching the failure) while
/// the DLL is missing, so it starts working as soon as the download finishes.
pub fn runtime() -> Result<&'static Pdfium, PdfError> {
    if let Some(p) = RUNTIME.get() {
        return Ok(p);
    }
    let _guard = INIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = RUNTIME.get() {
        return Ok(p);
    }
    let found = locate_dll();
    #[cfg(test)]
    let found = found.or_else(|| TEST_DLL.get().cloned());
    let dll = found.ok_or_else(|| PdfError::Unavailable("pdfium.dll is not installed".into()))?;
    let bindings = Pdfium::bind_to_library(&dll).map_err(|e| PdfError::Unavailable(format!("load {}: {e}", dll.display())))?;
    Ok(RUNTIME.get_or_init(|| Pdfium::new(bindings)))
}

// ---------------------------------------------------------------- reading

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageOut {
    /// 1-based page number.
    pub page: u32,
    pub text: String,
    /// Fewer than [`SCANNED_MIN_CHARS`] characters of text.
    pub scanned: bool,
    /// Rendered page (JPEG, long side [`RENDER_LONG_SIDE`]); only for scanned pages.
    pub jpeg: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfRead {
    /// Pages in the document (may exceed [`MAX_PAGES`]).
    pub total_pages: u32,
    pub pages: Vec<PageOut>,
}

fn map_load(e: PdfiumError) -> PdfError {
    match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError | PdfiumInternalError::SecurityError) => {
            PdfError::Encrypted
        }
        other => PdfError::Failed(format!("pdf: {other:?}")),
    }
}

/// True when the (trimmed) text is too short to be a real text layer.
pub fn is_scanned_text(text: &str) -> bool {
    text.chars().filter(|c| !c.is_whitespace()).count() < SCANNED_MIN_CHARS
}

/// Pixel size of a `w` x `h` pt page scaled so its long side is `long` px.
pub fn render_size(w: f32, h: f32, long: u32) -> (i32, i32) {
    let m = w.max(h).max(1.0);
    let s = long as f32 / m;
    (((w * s).round() as i32).max(1), ((h * s).round() as i32).max(1))
}

/// Render `page` to an RGB image whose long side is `long` px.
fn render_rgb(page: &PdfPage, long: u32) -> Result<image::RgbImage, PdfError> {
    let (w, h) = render_size(page.width().value, page.height().value, long);
    let bitmap = page
        .render_with_config(&PdfRenderConfig::new().set_fixed_size(w, h).render_annotations(true))
        .map_err(|e| PdfError::Failed(format!("render: {e:?}")))?;
    let img = bitmap.as_image().map_err(|e| PdfError::Failed(format!("bitmap: {e:?}")))?;
    Ok(img.to_rgb8())
}

fn render_jpeg(page: &PdfPage, long: u32) -> Result<Vec<u8>, PdfError> {
    let rgb = render_rgb(page, long)?;
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), JPEG_QUALITY)
        .encode_image(&rgb)
        .map_err(|e| PdfError::Failed(format!("jpeg: {e}")))?;
    Ok(out)
}

/// Text of every page; scanned pages are rendered when `render_scanned`.
pub fn read_pdf(pdfium: &Pdfium, bytes: &[u8], render_scanned: bool) -> Result<PdfRead, PdfError> {
    read_pdf_range(pdfium, bytes, render_scanned, 1, MAX_PAGES as u32)
}

/// [`read_pdf`] for the 1-based pages `from..=to` (clamped to the document). `total_pages` is the
/// whole document's count, so the caller knows what is left.
pub fn read_pdf_range(pdfium: &Pdfium, bytes: &[u8], render_scanned: bool, from: u32, to: u32) -> Result<PdfRead, PdfError> {
    let doc = pdfium.load_pdf_from_byte_slice(bytes, None).map_err(map_load)?;
    let pages = doc.pages();
    let total = pages.len() as usize;
    let mut out = Vec::new();
    for i in (from.max(1) as usize - 1)..total.min(to as usize) {
        let Ok(page) = pages.get(i as i32) else { continue };
        let text = page.text().map(|t| t.all()).unwrap_or_default().replace("\r\n", "\n").replace('\r', "\n");
        let scanned = is_scanned_text(&text);
        let jpeg = if scanned && render_scanned { render_jpeg(&page, RENDER_LONG_SIDE).ok() } else { None };
        out.push(PageOut { page: i as u32 + 1, text, scanned, jpeg });
    }
    Ok(PdfRead { total_pages: total as u32, pages: out })
}

/// Render the given 1-based `pages` of a document to JPEGs (used when image vectors of a text-only
/// file are computed later). Pages that fail to render are left out.
pub fn render_pages(pdfium: &Pdfium, bytes: &[u8], pages: &[u32]) -> Result<Vec<(u32, Vec<u8>)>, PdfError> {
    let doc = pdfium.load_pdf_from_byte_slice(bytes, None).map_err(map_load)?;
    let all = doc.pages();
    let mut out = Vec::new();
    for &n in pages {
        if n == 0 || n as usize > all.len() as usize {
            continue;
        }
        if let Ok(page) = all.get((n - 1) as i32) {
            if let Ok(j) = render_jpeg(&page, RENDER_LONG_SIDE) {
                out.push((n, j));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- preview

/// Page count of a document on disk (`None` for unreadable / encrypted files).
pub fn page_count(pdfium: &Pdfium, path: &Path) -> Option<u32> {
    let doc = pdfium.load_pdf_from_file(path, None).ok()?;
    Some(doc.pages().len() as u32)
}

/// One page (1-based) as a PNG `width` px wide (height follows the page's aspect ratio).
pub fn render_page_png(pdfium: &Pdfium, path: &Path, page: u32, width: u32) -> Result<Vec<u8>, PdfError> {
    let doc = pdfium.load_pdf_from_file(path, None).map_err(map_load)?;
    let pages = doc.pages();
    let n = pages.len() as u32;
    if page == 0 || page > n {
        return Err(PdfError::Failed(format!("page {page} of {n}")));
    }
    let p = pages.get((page - 1) as i32).map_err(|e| PdfError::Failed(format!("page: {e:?}")))?;
    // `render_size` scales the long side, so derive it from the wanted width.
    let (pw, ph) = (p.width().value.max(1.0), p.height().value.max(1.0));
    let long = (width as f32 * pw.max(ph) / pw).round().max(1.0) as u32;
    let rgb = render_rgb(&p, long)?;
    let mut out = Vec::new();
    rgb.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| PdfError::Failed(format!("png: {e}")))?;
    Ok(out)
}

// ---------------------------------------------------------------- boxes

/// Most line boxes kept for one page (a huge match would rather lose its tail than stall the pane).
const MAX_BOXES: usize = 400;
/// Most occurrences of one term searched per page.
const MAX_HITS_PER_TERM: usize = 200;

/// Where `terms` occur on one page's text layer, as fractions 0-1 of the page (top-left origin),
/// one box per matching line (pdfium splits a match into per-line rects itself). Text-layer PDFs
/// store no boxes in their chunk locators (unlike OCR text and parser output), so the preview
/// asks for these live.
pub fn page_word_boxes(pdfium: &Pdfium, path: &Path, page: u32, terms: &[String]) -> Result<Vec<HitBox>, PdfError> {
    let doc = pdfium.load_pdf_from_file(path, None).map_err(map_load)?;
    let pages = doc.pages();
    let n = pages.len() as u32;
    if page == 0 || page > n {
        return Err(PdfError::Failed(format!("page {page} of {n}")));
    }
    let p = pages.get((page - 1) as i32).map_err(|e| PdfError::Failed(format!("page: {e:?}")))?;
    let text = p.text().map_err(|e| PdfError::Failed(format!("text: {e:?}")))?;
    let (w, h) = (p.width().value.max(1.0), p.height().value.max(1.0));

    // Every occurrence of every term as line rectangles in page points (bottom-left origin).
    let mut rects: Vec<PdfRect> = Vec::new();
    'terms: for term in terms {
        let Ok(search) = text.search(term, &PdfSearchOptions::new()) else { continue };
        for _ in 0..MAX_HITS_PER_TERM {
            let Some(segs) = search.find_next() else { break };
            for i in 0..segs.len() {
                match segs.get(i).map(|s| s.bounds()) {
                    Ok(r) if rects.len() < MAX_BOXES => rects.push(r),
                    Ok(_) => break 'terms,
                    Err(_) => {}
                }
            }
        }
    }
    // Two terms can hit the same line: group by vertical overlap and union each group.
    let mut lines: Vec<(f32, f32, f32, f32)> = Vec::new(); // left, top, right, bottom (page points)
    for r in rects {
        let (l, t, r, b) = (r.left().value, r.top().value, r.right().value, r.bottom().value);
        if r <= l || t <= b {
            continue;
        }
        let cy = (t + b) / 2.0;
        let ch = t - b;
        match lines.iter_mut().find(|(_, lt, _, lb)| cy <= *lt + ch * 0.5 && cy >= *lb - ch * 0.5) {
            Some((ll, lt, lr, lb)) => {
                *ll = ll.min(l);
                *lt = lt.max(t);
                *lr = lr.max(r);
                *lb = lb.min(b);
            }
            None => lines.push((l, t, r, b)),
        }
    }
    let frac = |v: f32, total: f32| (v / total).clamp(0.0, 1.0);
    Ok(lines
        .into_iter()
        .map(|(l, t, r, b)| HitBox { x: frac(l, w), y: frac(h - t, h), w: frac(r - l, w), h: frac(t - b, h) })
        .collect())
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// The pinned pdfium (found via `WINSEEK_PDFIUM`).
    /// `None` when it cannot be loaded; PDF tests then skip.
    pub fn pdfium() -> Option<&'static Pdfium> {
        match runtime() {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("skipping pdfium test: {e}");
                None
            }
        }
    }

    /// A PDF with one A4 page per entry: `Some(text)` draws it, `None` leaves the page blank (a "scan").
    pub fn make_pdf(pdfium: &Pdfium, pages: &[Option<&str>]) -> Vec<u8> {
        let mut doc = pdfium.create_new_pdf().unwrap();
        let font = doc.fonts_mut().helvetica();
        for text in pages {
            let mut page = doc.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).unwrap();
            if let Some(t) = text {
                // one object per line: pdfium text objects do not wrap
                for (i, line) in t.lines().enumerate() {
                    page.objects_mut()
                        .create_text_object(PdfPoints::new(50.0), PdfPoints::new(780.0 - 20.0 * i as f32), line, font, PdfPoints::new(14.0))
                        .unwrap();
                }
            }
        }
        doc.save_to_bytes().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{make_pdf, pdfium};
    use super::*;

    #[test]
    fn scanned_text_threshold() {
        assert!(is_scanned_text(""));
        assert!(is_scanned_text("  \n page 3 \n"));
        assert!(!is_scanned_text("This page has a real text layer."));
        assert!(is_scanned_text(&"x".repeat(SCANNED_MIN_CHARS - 1)));
        assert!(!is_scanned_text(&"x".repeat(SCANNED_MIN_CHARS)));
        // whitespace does not count towards the threshold
        assert!(is_scanned_text(&format!("{} a", " ".repeat(50))));
    }

    #[test]
    fn render_size_keeps_aspect_and_long_side() {
        assert_eq!(render_size(595.0, 842.0, 1024), (724, 1024));
        assert_eq!(render_size(842.0, 595.0, 1024), (1024, 724));
        assert_eq!(render_size(0.0, 0.0, 1024), (1, 1));
    }

    #[test]
    fn dll_lookup_prefers_override_then_install() {
        let man = Manifest::embedded();
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(locate_dll_in(None, Some(dir.path()), &man), None);
        assert_eq!(locate_dll_in(None, None, &man), None);
        let installed = pdfium_path(dir.path(), &man.pdfium);
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, b"x").unwrap();
        assert_eq!(locate_dll_in(None, Some(dir.path()), &man), Some(installed.clone()));
        let dev = dir.path().join("dev.dll");
        assert_eq!(locate_dll_in(Some(dev.as_os_str()), Some(dir.path()), &man), Some(installed.clone()), "override ignored when it is not a file");
        std::fs::write(&dev, b"x").unwrap();
        assert_eq!(locate_dll_in(Some(dev.as_os_str()), Some(dir.path()), &man), Some(dev));
    }

    #[test]
    fn reads_text_pages_and_flags_scans() {
        let Some(p) = pdfium() else { return };
        let bytes = make_pdf(p, &[Some("Quarterly invoice number 4711\nTotal due 250 EUR"), None, Some("Second page about gardening tools")]);
        let r = read_pdf(p, &bytes, true).unwrap();
        assert_eq!((r.total_pages, r.pages.len()), (3, 3));
        assert_eq!(r.pages[0].page, 1);
        assert!(r.pages[0].text.contains("invoice number 4711") && r.pages[0].text.contains("Total due"));
        assert!(!r.pages[0].scanned && r.pages[0].jpeg.is_none());
        assert!(r.pages[1].scanned, "blank page = scanned");
        let jpeg = r.pages[1].jpeg.as_ref().expect("scanned page rendered");
        let img = image::load_from_memory(jpeg).unwrap();
        assert_eq!(img.width().max(img.height()), RENDER_LONG_SIDE);
        assert!(r.pages[2].text.contains("gardening"));
        // without rendering no JPEG is produced
        assert!(read_pdf(p, &bytes, false).unwrap().pages[1].jpeg.is_none());
        // later rendering of chosen pages
        let pages = render_pages(p, &bytes, &[2, 9, 0]).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].0, 2);
    }

    #[test]
    fn page_cap_is_enforced() {
        let Some(p) = pdfium() else { return };
        let pages: Vec<Option<&str>> = (0..MAX_PAGES + 5).map(|_| Some("same words on every page")).collect();
        let bytes = make_pdf(p, &pages);
        let r = read_pdf(p, &bytes, false).unwrap();
        assert_eq!(r.total_pages as usize, MAX_PAGES + 5);
        assert_eq!(r.pages.len(), MAX_PAGES);
        assert_eq!(r.pages.last().unwrap().page as usize, MAX_PAGES);
    }

    #[test]
    fn page_ranges_slice_the_document() {
        let Some(p) = pdfium() else { return };
        let pages: Vec<Option<&str>> = (1..=7).map(|_| Some("same words on every page")).collect();
        let bytes = make_pdf(p, &pages);
        let r = read_pdf_range(p, &bytes, false, 3, 5).unwrap();
        assert_eq!(r.total_pages, 7);
        assert_eq!(r.pages.iter().map(|p| p.page).collect::<Vec<_>>(), [3, 4, 5]);
        let r = read_pdf_range(p, &bytes, false, 6, 99).unwrap();
        assert_eq!(r.pages.iter().map(|p| p.page).collect::<Vec<_>>(), [6, 7], "clamped to the document");
        assert!(read_pdf_range(p, &bytes, false, 8, 20).unwrap().pages.is_empty());
        assert_eq!(read_pdf_range(p, &bytes, false, 0, 2).unwrap().pages.len(), 2, "0 reads like 1");
    }

    #[test]
    fn password_errors_mean_encrypted() {
        let pw = PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError);
        assert_eq!(map_load(pw), PdfError::Encrypted);
        let sec = PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::SecurityError);
        assert_eq!(map_load(sec), PdfError::Encrypted);
        let fmt = PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError);
        assert!(matches!(map_load(fmt), PdfError::Failed(_)));
    }

    #[test]
    fn garbage_is_a_failure_not_a_panic() {
        let Some(p) = pdfium() else { return };
        assert!(matches!(read_pdf(p, b"definitely not a pdf", true), Err(PdfError::Failed(_))));
    }

    #[test]
    fn preview_page_png_and_count() {
        let Some(p) = pdfium() else { return };
        let dir = crate::tier1::test_tmp();
        let path = dir.path().join("doc.pdf");
        std::fs::write(&path, make_pdf(p, &[Some("page one"), Some("page two")])).unwrap();
        assert_eq!(page_count(p, &path), Some(2));
        let png = render_page_png(p, &path, 2, 400).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!(img.width(), 400);
        assert!(img.height() > img.width(), "A4 portrait");
        assert!(render_page_png(p, &path, 3, 400).is_err());
        assert!(render_page_png(p, &path, 0, 400).is_err());
        assert_eq!(page_count(p, &dir.path().join("none.pdf")), None);
    }

    #[test]
    fn word_boxes_land_on_the_page() {
        let Some(p) = pdfium() else { return };
        let dir = crate::tier1::test_tmp();
        let path = dir.path().join("words.pdf");
        std::fs::write(&path, make_pdf(p, &[Some("The quick brown fox jumps over the lazy dog.")])).unwrap();
        let in01 = |b: HitBox| b.x > 0.0 && b.x + b.w < 1.0 && b.y > 0.0 && b.y + b.h < 1.0 && b.w > 0.0 && b.h > 0.0;
        let fox = page_word_boxes(p, &path, 1, &["fox".into()]).unwrap();
        assert_eq!(fox.len(), 1, "{fox:?}");
        assert!(in01(fox[0]), "{fox:?}");
        // no match: no boxes
        assert!(page_word_boxes(p, &path, 1, &["zebra".into()]).unwrap().is_empty());
        // two terms of the same line merge into one box that spans both
        let both = page_word_boxes(p, &path, 1, &["quick".into(), "dog".into()]).unwrap();
        assert_eq!(both.len(), 1, "{both:?}");
        assert!(both[0].w > fox[0].w, "merged {both:?} vs single {fox:?}");
        // page out of range is an error, not a panic
        assert!(page_word_boxes(p, &path, 9, &["fox".into()]).is_err());
    }
}
