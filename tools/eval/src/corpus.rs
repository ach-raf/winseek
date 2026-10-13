//! Deterministic synthetic corpus + the eval cases derived from it.
//!
//! File names are opaque (`img_0007.jpg`); only `corpus.json` (and `eval/cases.json`) know
//! what is inside. The spec table below is the single source of truth: `gen` renders it,
//! `cases` emits the matching cases, and a test keeps `eval/cases.json` in sync.

use std::io::Cursor;
use std::path::Path;

use image::{Rgb, RgbImage};
use serde::{Deserialize, Serialize};

use crate::metrics::Case;

const W: u32 = 800;
const H: u32 = 600;
const JPEG_Q: u8 = 92;

type Color = [u8; 3];
const RED: Color = [220, 30, 30];
const GREEN: Color = [30, 160, 50];
const BLUE: Color = [30, 60, 220];
const YELLOW: Color = [240, 220, 30];
const ORANGE: Color = [245, 140, 20];
const PURPLE: Color = [130, 40, 170];
const BLACK: Color = [15, 15, 15];
const WHITE: Color = [252, 252, 252];

#[derive(Debug, Clone, Copy)]
enum Shape {
    Circle,
    Square,
    Triangle,
}

#[derive(Debug, Clone, Copy)]
enum Pattern {
    Solid(Color),
    /// Coloured shape centred on white.
    Shaped(Shape, Color),
    /// Alternating bands; `true` = vertical bands.
    Stripes(Color, Color, bool),
    Checker(Color, Color),
    /// Left-to-right gradient.
    Gradient(Color, Color),
    /// Mostly-white portrait page with black bar "text lines"; the seed varies the line lengths.
    Page(u32),
    /// Near-duplicate of the image at this spec index (tiny brightness shift + a few pixels).
    NearDup(usize),
}

enum Content {
    Image(Pattern),
    Text(&'static str),
}

struct Spec {
    label: &'static str,
    content: Content,
}

const fn img(label: &'static str, p: Pattern) -> Spec {
    Spec { label, content: Content::Image(p) }
}

const fn txt(label: &'static str, body: &'static str) -> Spec {
    Spec { label, content: Content::Text(body) }
}

use Pattern::*;
use Shape::*;

/// The corpus, in file-index order. Do not reorder: indices are baked into file names.
const SPECS: &[Spec] = &[
    img("solid-red", Solid(RED)),
    img("solid-green", Solid(GREEN)),
    img("solid-blue", Solid(BLUE)),
    img("solid-yellow", Solid(YELLOW)),
    img("solid-orange", Solid(ORANGE)),
    img("solid-purple", Solid(PURPLE)),
    img("solid-black", Solid(BLACK)),
    img("circle-red", Shaped(Circle, RED)),
    img("circle-blue", Shaped(Circle, BLUE)),
    img("circle-green", Shaped(Circle, GREEN)),
    img("circle-yellow", Shaped(Circle, YELLOW)),
    img("square-red", Shaped(Square, RED)),
    img("square-blue", Shaped(Square, BLUE)),
    img("square-green", Shaped(Square, GREEN)),
    img("square-orange", Shaped(Square, ORANGE)),
    img("triangle-yellow", Shaped(Triangle, YELLOW)),
    img("triangle-purple", Shaped(Triangle, PURPLE)),
    img("triangle-green", Shaped(Triangle, GREEN)),
    img("triangle-red", Shaped(Triangle, RED)),
    img("stripes-red-white-h", Stripes(RED, WHITE, false)),
    img("stripes-blue-yellow-v", Stripes(BLUE, YELLOW, true)),
    img("stripes-black-white-v", Stripes(BLACK, WHITE, true)),
    img("stripes-green-white-h", Stripes(GREEN, WHITE, false)),
    img("checker-black-white", Checker(BLACK, WHITE)),
    img("checker-red-yellow", Checker(RED, YELLOW)),
    img("gradient-red-yellow", Gradient(RED, YELLOW)),
    img("gradient-blue-white", Gradient(BLUE, WHITE)),
    img("gradient-black-white", Gradient(BLACK, WHITE)),
    img("gradient-green-blue", Gradient(GREEN, BLUE)),
    img("page-1", Page(1)),
    img("page-2", Page(2)),
    img("page-3", Page(3)),
    img("page-4", Page(4)),
    img("dup-square-blue", NearDup(12)),
    img("dup-triangle-yellow", NearDup(15)),
    txt(
        "note-pancakes",
        "Pancake recipe: mix flour, eggs, milk and a pinch of salt. Let the batter rest, then fry thin pancakes in butter. Serve with sugar and lemon.",
    ),
    txt(
        "note-meeting",
        "Meeting notes: quarterly budget review with the finance team. Action items: reduce cloud spending, hire two engineers, approve the marketing plan.",
    ),
    txt(
        "note-hiking",
        "Packing list for the hiking trip: boots, rain jacket, water bottle, map, compass, first aid kit and sunscreen.",
    ),
    txt(
        "note-invoice",
        "Invoice 2043: consulting services, 12 hours at 90 euros per hour, total 1080 euros, payment due within 30 days.",
    ),
    txt(
        "note-gateau-fr",
        "Recette de gateau au chocolat : melanger le chocolat fondu, le beurre, le sucre et les oeufs, puis cuire quarante minutes au four.",
    ),
    txt("note-compras-es", "Lista de la compra: manzanas, leche, pan, queso y tomates para la cena del viernes."),
];

/// `(query, tag, labels of acceptable files)`.
const CASES: &[(&str, &str, &[&str])] = &[
    ("a solid red image", "color", &["solid-red"]),
    ("plain green background", "color", &["solid-green"]),
    ("a completely blue picture", "color", &["solid-blue"]),
    ("yellow color field", "color", &["solid-yellow"]),
    ("an orange image", "color", &["solid-orange"]),
    ("purple", "color", &["solid-purple"]),
    ("an all black image", "color", &["solid-black"]),
    ("a red circle on a white background", "shape", &["circle-red"]),
    ("a blue circle", "shape", &["circle-blue"]),
    ("a green circle", "shape", &["circle-green"]),
    ("a yellow circle", "shape", &["circle-yellow"]),
    ("a red square", "shape", &["square-red"]),
    ("a blue square", "shape", &["square-blue", "dup-square-blue"]),
    ("a green square", "shape", &["square-green"]),
    ("an orange square", "shape", &["square-orange"]),
    ("a yellow triangle", "shape", &["triangle-yellow", "dup-triangle-yellow"]),
    ("a purple triangle", "shape", &["triangle-purple"]),
    ("a green triangle", "shape", &["triangle-green"]),
    ("a red triangle", "shape", &["triangle-red"]),
    ("red and white horizontal stripes", "pattern", &["stripes-red-white-h"]),
    ("blue and yellow vertical stripes", "pattern", &["stripes-blue-yellow-v"]),
    ("black and white vertical stripes", "pattern", &["stripes-black-white-v"]),
    ("green and white striped pattern", "pattern", &["stripes-green-white-h"]),
    ("a black and white checkerboard", "pattern", &["checker-black-white"]),
    ("a red and yellow checkered pattern", "pattern", &["checker-red-yellow"]),
    ("a gradient from red to yellow", "gradient", &["gradient-red-yellow"]),
    ("a gradient fading from blue to white", "gradient", &["gradient-blue-white"]),
    ("a smooth gradient from black to white", "gradient", &["gradient-black-white"]),
    ("a gradient from green to blue", "gradient", &["gradient-green-blue"]),
    ("a white page with lines of text", "doc", &["page-1", "page-2", "page-3", "page-4"]),
    ("a scanned document", "doc", &["page-1", "page-2", "page-3", "page-4"]),
    ("pancake recipe with flour and eggs", "text", &["note-pancakes"]),
    ("quarterly budget meeting notes", "text", &["note-meeting"]),
    ("what to pack for a hike", "text", &["note-hiking"]),
    ("consulting invoice with hourly rate", "text", &["note-invoice"]),
    ("un cercle rouge sur fond blanc", "xling", &["circle-red"]),
    ("rayures bleues et jaunes verticales", "xling", &["stripes-blue-yellow-v"]),
    ("recette de crepes", "xling", &["note-pancakes"]),
    ("un triangulo amarillo", "xling", &["triangle-yellow", "dup-triangle-yellow"]),
    ("grocery shopping list", "xling", &["note-compras-es"]),
    ("chocolate cake recipe", "xling", &["note-gateau-fr"]),
];

/// File name for the spec at `idx`.
pub fn file_name(idx: usize) -> String {
    match SPECS[idx].content {
        Content::Image(_) => format!("img_{idx:04}.jpg"),
        Content::Text(_) => format!("note_{idx:04}.txt"),
    }
}

fn name_of_label(label: &str) -> Option<String> {
    SPECS.iter().position(|s| s.label == label).map(file_name)
}

/// The eval cases for this corpus (what `eval/cases.json` must equal).
pub fn cases() -> Vec<Case> {
    CASES
        .iter()
        .map(|(q, tag, labels)| Case {
            q: (*q).into(),
            tag: (*tag).into(),
            expect: labels.iter().map(|l| name_of_label(l).unwrap_or_else(|| panic!("unknown label {l}"))).collect(),
        })
        .collect()
}

// ------------------------------------------------------------------ rendering

fn mix(a: Color, b: Color, t: f32) -> Color {
    let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

/// Tiny deterministic generator (no rand dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

fn in_shape(shape: Shape, x: u32, y: u32) -> bool {
    let (cx, cy) = (W as i32 / 2, H as i32 / 2);
    let (dx, dy) = (x as i32 - cx, y as i32 - cy);
    match shape {
        Shape::Circle => dx * dx + dy * dy <= 200 * 200,
        Shape::Square => dx.abs() <= 170 && dy.abs() <= 170,
        Shape::Triangle => {
            let top = cy - 200;
            let y = y as i32;
            y >= top && y <= cy + 180 && dx.abs() <= (y - top) / 2
        }
    }
}

fn render_rgb(idx: usize) -> RgbImage {
    let Content::Image(p) = SPECS[idx].content else { panic!("spec {idx} is not an image") };
    match p {
        Solid(c) => RgbImage::from_pixel(W, H, Rgb(c)),
        Shaped(s, c) => RgbImage::from_fn(W, H, |x, y| Rgb(if in_shape(s, x, y) { c } else { WHITE })),
        Stripes(a, b, vertical) => RgbImage::from_fn(W, H, |x, y| {
            let band = if vertical { x / 50 } else { y / 50 };
            Rgb(if band % 2 == 0 { a } else { b })
        }),
        Checker(a, b) => RgbImage::from_fn(W, H, |x, y| Rgb(if (x / 80 + y / 80) % 2 == 0 { a } else { b })),
        Gradient(a, b) => RgbImage::from_fn(W, H, |x, _| Rgb(mix(a, b, x as f32 / (W - 1) as f32))),
        Page(seed) => {
            let (pw, ph) = (600u32, 800u32);
            let mut img = RgbImage::from_pixel(pw, ph, Rgb(WHITE));
            let mut rng = Lcg(u64::from(seed) * 7919 + 1);
            let bar = |img: &mut RgbImage, y: u32, len: u32, thick: u32| {
                for yy in y..(y + thick).min(ph) {
                    for xx in 60..(60 + len).min(pw - 60) {
                        img.put_pixel(xx, yy, Rgb(BLACK));
                    }
                }
            };
            bar(&mut img, 50, 220 + rng.next() % 100, 16); // title
            let mut y = 110;
            while y + 10 < ph - 50 {
                let len = if rng.next().is_multiple_of(6) { 120 + rng.next() % 200 } else { 440 };
                bar(&mut img, y, len, 7);
                y += 26;
            }
            img
        }
        NearDup(of) => {
            let mut img = render_rgb(of);
            for px in img.pixels_mut() {
                for c in &mut px.0 {
                    *c = c.saturating_add(2);
                }
            }
            let mut rng = Lcg(of as u64 + 99);
            for _ in 0..40 {
                let (x, y) = (rng.next() % img.width(), rng.next() % img.height());
                img.put_pixel(x, y, Rgb([128, 128, 128]));
            }
            img
        }
    }
}

/// Encoded bytes of file `idx` (JPEG q92 for images, UTF-8 for notes). Deterministic.
pub fn render(idx: usize) -> Vec<u8> {
    match SPECS[idx].content {
        Content::Text(body) => format!("{body}\n").into_bytes(),
        Content::Image(_) => {
            let img = render_rgb(idx);
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), JPEG_Q)
                .encode_image(&img)
                .expect("jpeg encode");
            out
        }
    }
}

// ------------------------------------------------------------------ corpus.json

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpusFile {
    pub name: String,
    /// `image` or `text`.
    pub kind: String,
    /// What the file actually shows (the sidecar answer key).
    pub desc: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpusIndex {
    pub version: u32,
    pub files: Vec<CorpusFile>,
}

pub fn index() -> CorpusIndex {
    CorpusIndex {
        version: 1,
        files: SPECS
            .iter()
            .enumerate()
            .map(|(i, s)| CorpusFile {
                name: file_name(i),
                kind: if matches!(s.content, Content::Image(_)) { "image" } else { "text" }.into(),
                desc: s.label.into(),
            })
            .collect(),
    }
}

/// Write the corpus into `dir`. Returns the number of files written.
pub fn generate(dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dir)?;
    for i in 0..SPECS.len() {
        std::fs::write(dir.join(file_name(i)), render(i))?;
    }
    let json = serde_json::to_string_pretty(&index()).expect("serialise");
    std::fs::write(dir.join("corpus.json"), json + "\n")?;
    Ok(SPECS.len())
}

pub fn load_index(dir: &Path) -> Result<CorpusIndex, String> {
    let p = dir.join("corpus.json");
    let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e} (run `winseek-eval gen` first)", p.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{glob_match, parse_cases};

    #[test]
    fn rendering_is_deterministic() {
        for i in 0..SPECS.len() {
            assert_eq!(render(i), render(i), "spec {i}");
        }
    }

    #[test]
    fn generate_twice_gives_identical_bytes() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        generate(a.path()).unwrap();
        generate(b.path()).unwrap();
        for f in index().files {
            assert_eq!(std::fs::read(a.path().join(&f.name)).unwrap(), std::fs::read(b.path().join(&f.name)).unwrap());
        }
        assert_eq!(std::fs::read(a.path().join("corpus.json")).unwrap(), std::fs::read(b.path().join("corpus.json")).unwrap());
        assert_eq!(load_index(a.path()).unwrap(), index());
    }

    #[test]
    fn names_are_opaque_and_unique() {
        let idx = index();
        let mut names: Vec<_> = idx.files.iter().map(|f| f.name.clone()).collect();
        for n in &names {
            assert!(n.starts_with("img_") || n.starts_with("note_"), "{n}");
            assert!(!["red", "blue", "circle", "stripes", "page"].iter().any(|w| n.contains(w)), "{n}");
        }
        names.sort();
        names.dedup();
        assert_eq!(names.len(), SPECS.len());
        let labels: std::collections::HashSet<_> = SPECS.iter().map(|s| s.label).collect();
        assert_eq!(labels.len(), SPECS.len(), "labels unique");
    }

    #[test]
    fn images_decode_and_differ() {
        let a = image::load_from_memory(&render(0)).unwrap();
        assert_eq!((a.width(), a.height()), (W, H));
        let page = image::load_from_memory(&render(29)).unwrap();
        assert_eq!((page.width(), page.height()), (600, 800));
        assert_ne!(render(0), render(1));
        // near-dup differs from its source but is the same size
        let (src, dup) = (12, 33);
        assert_ne!(render(src), render(dup));
        assert_eq!(SPECS[dup].label, "dup-square-blue");
        assert_eq!(SPECS[src].label, "square-blue");
    }

    #[test]
    fn cases_resolve_to_existing_files() {
        let names: Vec<String> = index().files.into_iter().map(|f| f.name).collect();
        let cs = cases();
        assert!(cs.len() >= 40);
        for c in &cs {
            for e in &c.expect {
                assert!(names.iter().any(|n| glob_match(e, n)), "{} expects missing {e}", c.q);
            }
        }
        let tags: std::collections::HashSet<_> = cs.iter().map(|c| c.tag.as_str()).collect();
        for t in ["color", "shape", "pattern", "gradient", "doc", "text", "xling"] {
            assert!(tags.contains(t), "{t}");
        }
        assert!(cs.iter().filter(|c| c.tag == "xling").count() >= 2);
    }

    #[test]
    fn committed_cases_json_is_in_sync() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/cases.json");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            parse_cases(&text).unwrap(),
            cases(),
            "eval/cases.json is stale; regenerate with `winseek-eval cases > eval/cases.json`"
        );
    }
}
