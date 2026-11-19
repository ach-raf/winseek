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
    Spec {
        label,
        content: Content::Image(p),
    }
}

const fn txt(label: &'static str, body: &'static str) -> Spec {
    Spec {
        label,
        content: Content::Text(body),
    }
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
    // Exact-token notes (appended: earlier indices are baked into file names). Two near-twins each,
    // so meaning alone cannot tell the identifier apart.
    txt("note-inv-00817", "Invoice INV-2024-00817: office chairs, 8 units, total 2480 euros, payment due within 30 days."),
    txt("note-inv-00931", "Invoice INV-2024-00931: cloud hosting subscription, total 1175 euros, payment due within 14 days."),
    txt("note-err-5", "Troubleshooting log: the installer stopped with error 0x80070005 (access denied) while writing to the Program Files folder."),
    txt("note-err-57", "Troubleshooting log: the installer stopped with error 0x80070057 (invalid parameter) while reading the configuration file."),
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
    (
        "a red circle on a white background",
        "shape",
        &["circle-red"],
    ),
    ("a blue circle", "shape", &["circle-blue"]),
    ("a green circle", "shape", &["circle-green"]),
    ("a yellow circle", "shape", &["circle-yellow"]),
    ("a red square", "shape", &["square-red"]),
    (
        "a blue square",
        "shape",
        &["square-blue", "dup-square-blue"],
    ),
    ("a green square", "shape", &["square-green"]),
    ("an orange square", "shape", &["square-orange"]),
    (
        "a yellow triangle",
        "shape",
        &["triangle-yellow", "dup-triangle-yellow"],
    ),
    ("a purple triangle", "shape", &["triangle-purple"]),
    ("a green triangle", "shape", &["triangle-green"]),
    ("a red triangle", "shape", &["triangle-red"]),
    (
        "red and white horizontal stripes",
        "pattern",
        &["stripes-red-white-h"],
    ),
    (
        "blue and yellow vertical stripes",
        "pattern",
        &["stripes-blue-yellow-v"],
    ),
    (
        "black and white vertical stripes",
        "pattern",
        &["stripes-black-white-v"],
    ),
    (
        "green and white striped pattern",
        "pattern",
        &["stripes-green-white-h"],
    ),
    (
        "a black and white checkerboard",
        "pattern",
        &["checker-black-white"],
    ),
    (
        "a red and yellow checkered pattern",
        "pattern",
        &["checker-red-yellow"],
    ),
    (
        "a gradient from red to yellow",
        "gradient",
        &["gradient-red-yellow"],
    ),
    (
        "a gradient fading from blue to white",
        "gradient",
        &["gradient-blue-white"],
    ),
    (
        "a smooth gradient from black to white",
        "gradient",
        &["gradient-black-white"],
    ),
    (
        "a gradient from green to blue",
        "gradient",
        &["gradient-green-blue"],
    ),
    (
        "a white page with lines of text",
        "doc",
        &["page-1", "page-2", "page-3", "page-4"],
    ),
    (
        "a scanned document",
        "doc",
        &["page-1", "page-2", "page-3", "page-4"],
    ),
    (
        "pancake recipe with flour and eggs",
        "text",
        &["note-pancakes"],
    ),
    ("quarterly budget meeting notes", "text", &["note-meeting"]),
    ("what to pack for a hike", "text", &["note-hiking"]),
    (
        "consulting invoice with hourly rate",
        "text",
        &["note-invoice"],
    ),
    ("un cercle rouge sur fond blanc", "xling", &["circle-red"]),
    (
        "rayures bleues et jaunes verticales",
        "xling",
        &["stripes-blue-yellow-v"],
    ),
    ("recette de crepes", "xling", &["note-pancakes"]),
    (
        "un triangulo amarillo",
        "xling",
        &["triangle-yellow", "dup-triangle-yellow"],
    ),
    ("grocery shopping list", "xling", &["note-compras-es"]),
    ("chocolate cake recipe", "xling", &["note-gateau-fr"]),
    ("INV-2024-00817", "exact", &["note-inv-00817"]),
    ("INV-2024-00931", "exact", &["note-inv-00931"]),
    ("0x80070005", "exact", &["note-err-5"]),
    ("installer error 0x80070057", "exact", &["note-err-57"]),
];

/// File name for the spec at `idx`.
pub fn file_name(idx: usize) -> String {
    match SPECS[idx].content {
        Content::Image(_) => format!("img_{idx:04}.jpg"),
        Content::Text(_) => format!("note_{idx:04}.txt"),
    }
}

// ------------------------------------------------------------------ title experiment
//
// The main corpus keeps file names opaque on purpose, so it can never measure what a
// meaningful file NAME contributes. These notes are the opposite: named like real files
// ("pancake-recipe.txt"), written with `gen --titles`, and embedded with
// `title: {name} | text: {body}` by `run --titles`. Three probes:
// - body+name agree (rows 1..9): does the title help or hurt plain meaning search?
// - name-only (generic body, all the signal in the name): the sharpest title probe.
// - junk names: the opaque note_XXXX.txt files, embedded titled too — worst case.

/// `(file stem, body)`. Stems double as the `title_cases` expectation labels.
pub const TITLE_NOTES: &[(&str, &str)] = &[
    ("pancake-recipe", "Pancake recipe: mix flour, eggs, milk and a pinch of salt. Let the batter rest, then fry thin pancakes in butter. Serve with sugar and lemon."),
    ("quarterly-budget-review-notes", "Meeting notes: quarterly budget review with the finance team. Action items: reduce cloud spending, hire two engineers, approve the marketing plan."),
    ("hiking-packing-list", "Packing list for the hiking trip: boots, rain jacket, water bottle, map, compass, first aid kit and sunscreen."),
    ("consulting-invoice-2043", "Invoice 2043: consulting services, 12 hours at 90 euros per hour, total 1080 euros, payment due within 30 days."),
    ("recette-gateau-chocolat", "Recette de gateau au chocolat : melanger le chocolat fondu, le beurre, le sucre et les oeufs, puis cuire quarante minutes au four."),
    ("lista-de-la-compra", "Lista de la compra: manzanas, leche, pan, queso y tomates para la cena del viernes."),
    ("office-chairs-invoice", "Invoice INV-2024-00817: office chairs, 8 units, total 2480 euros, payment due within 30 days."),
    ("cloud-hosting-invoice", "Invoice INV-2024-00931: cloud hosting subscription, total 1175 euros, payment due within 14 days."),
    ("installer-access-denied-log", "Troubleshooting log: the installer stopped with error 0x80070005 (access denied) while writing to the Program Files folder."),
    ("invalid-parameter-log", "Troubleshooting log: the installer stopped with error 0x80070057 (invalid parameter) while reading the configuration file."),
    // Name-only probes: the body is filler; only the file name knows the subject.
    ("hawaii-trip-packing-list", "Attached is the document you asked for. Please review it and confirm by Friday afternoon."),
    ("wedding-speech-draft", "Draft. Opening line, two short anecdotes, the toast, and a closing thank you to the families."),
    ("dental-cleaning-appointment-card", "Reminder card. Please arrive ten minutes early and bring your insurance card along."),
];

pub fn title_file_name(i: usize) -> String {
    format!("{}.txt", TITLE_NOTES[i].0)
}

/// `(query, acceptable title-note stems)`.
const TITLE_CASES: &[(&str, &[&str])] = &[
    ("pancake recipe with flour and eggs", &["pancake-recipe"]),
    (
        "quarterly budget review meeting notes",
        &["quarterly-budget-review-notes"],
    ),
    ("what to pack for a hike", &["hiking-packing-list"]),
    (
        "consulting invoice with hourly rate",
        &["consulting-invoice-2043"],
    ),
    ("chocolate cake recipe", &["recette-gateau-chocolat"]),
    ("grocery shopping list", &["lista-de-la-compra"]),
    ("invoice for the office chairs", &["office-chairs-invoice"]),
    (
        "cloud hosting subscription bill",
        &["cloud-hosting-invoice"],
    ),
    (
        "installer stopped access denied",
        &["installer-access-denied-log"],
    ),
    (
        "what should I pack for the hawaii trip",
        &["hawaii-trip-packing-list"],
    ),
    (
        "draft of the wedding speech and toast",
        &["wedding-speech-draft"],
    ),
    (
        "dentist appointment reminder",
        &["dental-cleaning-appointment-card"],
    ),
];

/// The eval cases for the title notes (what `eval/title-cases.json` must equal).
pub fn title_cases() -> Vec<Case> {
    TITLE_CASES
        .iter()
        .map(|(q, stems)| Case {
            q: (*q).into(),
            tag: "title".into(),
            expect: stems
                .iter()
                .map(|s| {
                    TITLE_NOTES
                        .iter()
                        .position(|(stem, _)| stem == s)
                        .map(title_file_name)
                        .unwrap_or_else(|| panic!("unknown title stem {s}"))
                })
                .collect(),
        })
        .collect()
}

/// The main corpus plus the titled notes, listed in `corpus.json` so a single store embeds
/// both (one run then measures the opaque `note_XXXX.txt` files under junk titles too).
pub fn generate_with_titles(dir: &Path) -> std::io::Result<usize> {
    let mut n = generate(dir)?;
    let mut idx = load_index(dir).map_err(std::io::Error::other)?;
    for (i, (_, body)) in TITLE_NOTES.iter().enumerate() {
        let name = title_file_name(i);
        std::fs::write(dir.join(&name), format!("{body}\n"))?;
        idx.files.push(CorpusFile {
            name,
            kind: "text".into(),
            desc: format!("title-{}", TITLE_NOTES[i].0),
        });
        n += 1;
    }
    let json = serde_json::to_string_pretty(&idx).expect("serialise");
    std::fs::write(dir.join("corpus.json"), json + "\n")?;
    Ok(n)
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
            expect: labels
                .iter()
                .map(|l| name_of_label(l).unwrap_or_else(|| panic!("unknown label {l}")))
                .collect(),
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
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
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
    let Content::Image(p) = SPECS[idx].content else {
        panic!("spec {idx} is not an image")
    };
    match p {
        Solid(c) => RgbImage::from_pixel(W, H, Rgb(c)),
        Shaped(s, c) => {
            RgbImage::from_fn(W, H, |x, y| Rgb(if in_shape(s, x, y) { c } else { WHITE }))
        }
        Stripes(a, b, vertical) => RgbImage::from_fn(W, H, |x, y| {
            let band = if vertical { x / 50 } else { y / 50 };
            Rgb(if band % 2 == 0 { a } else { b })
        }),
        Checker(a, b) => RgbImage::from_fn(W, H, |x, y| {
            Rgb(if (x / 80 + y / 80) % 2 == 0 { a } else { b })
        }),
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
                let len = if rng.next().is_multiple_of(6) {
                    120 + rng.next() % 200
                } else {
                    440
                };
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
                kind: if matches!(s.content, Content::Image(_)) {
                    "image"
                } else {
                    "text"
                }
                .into(),
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
    let s = std::fs::read_to_string(&p)
        .map_err(|e| format!("{}: {e} (run `winseek-eval gen` first)", p.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))
}
// ------------------------------------------------------------------ code corpus

/// A small, fully deterministic repository for the code cases: an image-thumbnailing service
/// across Rust / Python / TypeScript / Go, its build files and its docs. Every file fits one
/// stretch except the SQL tokenizer, which crosses one, so line-range expectations exercise
/// multi-stretch files too.
pub static CODE_FILES: &[(&str, &str)] = &[
    (
        "src/net/retry.py",
        "\"\"\"Retrying calls that fail: exponential backoff with jitter, and a breaker.\"\"\"\nimport random\nimport time\n\n\ndef retry_with_backoff(call, tries=5, base=0.2, cap=8.0):\n    \"\"\"Run `call` until it works, waiting longer each time it fails.\n\n    The wait grows exponentially from `base` seconds up to `cap`, with full\n    jitter so a hundred callers never line up and stampede together.\n    \"\"\"\n    for attempt in range(tries):\n        try:\n            return call()\n        except TransientError:\n            if attempt == tries - 1:\n                raise\n            wait = min(cap, base * 2 ** attempt)\n            time.sleep(random.uniform(0.0, wait))\n\n\nclass CircuitBreaker:\n    \"\"\"Stop calling a dependency that keeps failing, and let it heal.\"\"\"\n\n    def __init__(self, threshold=5, cooldown=30.0):\n        self.failures = 0\n        self.threshold = threshold\n        self.cooldown = cooldown\n        self.opened_at = None\n\n    def allow(self):\n        if self.opened_at is None:\n            return True\n        if time.monotonic() - self.opened_at >= self.cooldown:\n            self.opened_at = None  # half-open: one probe may go through\n            return True\n        return False\n\n    def record(self, ok):\n        if ok:\n            self.failures = 0\n            self.opened_at = None\n            return\n        self.failures += 1\n        if self.failures >= self.threshold:\n            self.opened_at = time.monotonic()\n\n",
    ),
    (
        "src/net/gateway.py",
        "\"\"\"The payment gateway: charging cards, refunds, declines.\"\"\"\nfrom dataclasses import dataclass\n\n\n@dataclass\nclass PaymentDeclined(Exception):\n    \"\"\"The bank refused the charge; `reason` carries its code.\"\"\"\n\n    reason: str\n\n\ndef charge_card(card, amount_cents, currency=\"usd\"):\n    \"\"\"Charge a customer's card and return the provider's charge id.\n\n    A decline is not an error to retry blindly: the bank said no, and\n    retrying the same card makes the decline score worse.\n    \"\"\"\n    reply = provider.authorize(card, amount_cents, currency)\n    if reply.status == \"declined\":\n        raise PaymentDeclined(reply.reason)\n    provider.capture(reply.charge_id)\n    return reply.charge_id\n\n\ndef refund(charge_id, amount_cents=None):\n    \"\"\"Give a customer their money back, fully or in part.\"\"\"\n    return provider.reverse(charge_id, amount_cents or FULL)\n\n",
    ),
    (
        "src/auth.rs",
        "//! Sessions: log in, keep the tokens fresh, log out.\n\nuse std::time::{Duration, Instant};\n\n/// A signed session: who it belongs to and when it stops being valid.\npub struct Session {\n    pub user: String,\n    pub access_token: String,\n    pub refresh_token: String,\n    pub expires_at: Instant,\n}\n\n/// Log a user in and start a signed session.\npub fn login(user: &str, password: &str) -> Result<Session, AuthError> {\n    let proof = verifier.check(user, password)?;\n    let access_token = signer.sign(&proof, Duration::from_secs(600));\n    let refresh_token = signer.sign(&proof, Duration::from_secs(30 * 24 * 3600));\n    Ok(Session {\n        user: user.to_owned(),\n        access_token,\n        refresh_token,\n        expires_at: Instant::now() + Duration::from_secs(600),\n    })\n}\n\n/// Issue a fresh access token when the session expires; the refresh token\n/// is single use, and using it again revokes the whole session.\npub fn refresh_token(session: &Session) -> Result<Session, AuthError> {\n    let proof = verifier.verify_refresh(&session.refresh_token)?;\n    let access_token = signer.sign(&proof, Duration::from_secs(600));\n    Ok(Session {\n        refresh_token: proof.rotated(),\n        access_token,\n        ..session.clone()\n    })\n}\n\n/// Log out: both tokens stop working at once.\npub fn logout(session: &Session) {\n    verifier.revoke(&session.refresh_token);\n}\n\n",
    ),
    (
        "src/thumbs.rs",
        "//! Thumbnails: shrink a picture to fit its box without distorting it.\n\nuse image::{DynamicImage, ImageError};\n\n/// The biggest box a thumbnail may fill.\npub const BOX: u32 = 320;\n\n/// Decode `bytes` and render a thumbnail that fits [`BOX`] on the long side,\n/// keeping the aspect ratio so nothing is squashed or stretched.\npub fn thumbnail(bytes: &[u8]) -> Result<DynamicImage, ImageError> {\n    let img = image::load_from_memory(bytes)?;\n    Ok(fit_inside(&img, BOX, BOX))\n}\n\n/// Scale `img` down until both sides fit the box; an image smaller than the\n/// box is returned untouched (never upscaled).\nfn fit_inside(img: &DynamicImage, max_w: u32, max_h: u32) -> DynamicImage {\n    let (w, h) = (img.width(), img.height());\n    let scale = (max_w as f32 / w as f32).min(max_h as f32 / h as f32).min(1.0);\n    if scale >= 1.0 {\n        return img.clone();\n    }\n    img.resize_exact(\n        ((w as f32 * scale).round() as u32).max(1),\n        ((h as f32 * scale).round() as u32).max(1),\n        image::imageops::FilterType::Lanczos3,\n    )\n}\n\n",
    ),
    (
        "src/parse/sql.rs",
        "//! Tokenizing SQL: a SELECT statement becomes keywords, names, strings and numbers.\n\n#[derive(Debug, Clone, PartialEq)]\npub enum Tok {\n    Keyword(String),\n    Name(String),\n    Str(String),\n    Num(String),\n    Punct(char),\n}\n\n/// Where a token starts and ends in the statement it came from.\n#[derive(Debug, Clone, PartialEq)]\npub struct Spanned {\n    pub tok: Tok,\n    pub start: usize,\n    pub end: usize,\n}\n\nconst KEYWORDS: &[&str] = &[\n    \"select\", \"from\", \"where\", \"group\", \"by\", \"order\", \"limit\", \"having\", \"as\", \"and\", \"or\",\n    \"not\", \"in\", \"join\", \"left\", \"inner\", \"on\", \"distinct\",\n];\n\n/// Break a select statement into keywords and values, keeping each token's\n/// character span so a hit can underline exactly what matched.\npub fn tokenize(sql: &str) -> Vec<Spanned> {\n    let chars: Vec<char> = sql.chars().collect();\n    let mut out = Vec::new();\n    let mut i = 0usize;\n    while i < chars.len() {\n        let c = chars[i];\n        if c.is_whitespace() {\n            i += 1;\n            continue;\n        }\n        let start = i;\n        if c == '\\'' {\n            // A string literal runs to the closing quote; '' inside is an escaped quote.\n            i += 1;\n            while i < chars.len() {\n                if chars[i] == '\\'' {\n                    if chars.get(i + 1) == Some(&'\\'') {\n                        i += 2; // escaped ''\n                        continue;\n                    }\n                    break;\n                }\n                i += 1;\n            }\n            i = (i + 1).min(chars.len());\n            let end = i.saturating_sub(1).max(start + 1);\n            let text: String = chars[start + 1..end].iter().collect();\n            out.push(Spanned { tok: Tok::Str(text), start, end: i });\n            continue;\n        }\n        if c.is_ascii_digit() {\n            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {\n                i += 1;\n            }\n            let text: String = chars[start..i].iter().collect();\n            out.push(Spanned { tok: Tok::Num(text), start, end: i });\n            continue;\n        }\n        if c.is_alphabetic() || c == '_' {\n            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {\n                i += 1;\n            }\n            let text: String = chars[start..i].iter().collect();\n            let tok = if KEYWORDS.contains(&text.to_lowercase().as_str()) {\n                Tok::Keyword(text.to_lowercase())\n            } else {\n                Tok::Name(text)\n            };\n            out.push(Spanned { tok, start, end: i });\n            continue;\n        }\n        i += 1;\n        out.push(Spanned { tok: Tok::Punct(c), start, end: i });\n    }\n    out\n}\n\n",
    ),
    (
        "core/cache.go",
        "// Package cache: a tiny LRU the API reads through before touching the database.\npackage cache\n\nimport \"container/list\"\n\n// LRU keeps at most `cap` entries; the least recently used one is evicted.\ntype LRU struct {\n    cap  int\n    ll   *list.List\n    item map[string]*list.Element\n}\n\ntype entry struct {\n    key string\n    val any\n}\n\n// NewLRU makes a cache that holds `cap` entries at most.\nfunc NewLRU(cap int) *LRU {\n    return &LRU{cap: cap, ll: list.New(), item: make(map[string]*list.Element, cap)}\n}\n\nfunc (c *LRU) Get(key string) (any, bool) {\n    el, ok := c.item[key]\n    if !ok {\n        return nil, false\n    }\n    c.ll.MoveToFront(el)\n    return el.Value.(*entry).val, true\n}\n\nfunc (c *LRU) Set(key string, val any) {\n    if el, ok := c.item[key]; ok {\n        c.ll.MoveToFront(el)\n        el.Value.(*entry).val = val\n        return\n    }\n    c.item[key] = c.ll.PushFront(&entry{key, val})\n    if c.ll.Len() > c.cap {\n        c.evict()\n    }\n}\n\n// evict drops the least recently used entry; the list's back is exactly that.\nfunc (c *LRU) evict() {\n    el := c.ll.Back()\n    if el == nil {\n        return\n    }\n    c.ll.Remove(el)\n    delete(c.item, el.Value.(*entry).key)\n}\n\n",
    ),
    (
        "web/app.tsx",
        "import { useEffect, useRef, useState } from \"react\";\n\n/** Wait for the user to stop typing before searching: the query only fires\n * once `ms` milliseconds pass without a keystroke. */\nexport function useDebouncedQuery(query: string, ms = 250) {\n  const [ready, setReady] = useState(query);\n  useEffect(() => {\n    const t = setTimeout(() => setReady(query), ms);\n    return () => clearTimeout(t);\n  }, [query, ms]);\n  return ready;\n}\n\n/** The header search box: debounced, cancellable, keyboard first. */\nexport function SearchBar({ onSearch }: { onSearch: (q: string) => void }) {\n  const [text, setText] = useState(\"\");\n  const q = useDebouncedQuery(text);\n  useEffect(() => {\n    if (q.trim()) onSearch(q.trim());\n  }, [q, onSearch]);\n  return (\n    <div className=\"search-bar\">\n      <input\n        value={text}\n        onChange={(e) => setText(e.target.value)}\n        placeholder=\"Search\"\n        autoFocus\n      />\n    </div>\n  );\n}\n\n",
    ),
    (
        "web/styles.css",
        "/* The header search box and its states. */\n\n.search-bar {\n  display: flex;\n  align-items: center;\n  border: 1px solid var(--line);\n  border-radius: 10px;\n  padding: 4px 10px;\n  background: var(--bg);\n}\n\n.search-bar input {\n  border: none;\n  outline: none;\n  flex: 1;\n  font: inherit;\n}\n\n.search-bar:focus-within {\n  border-color: var(--accent);\n}\n\n",
    ),
    (
        "Makefile",
        "# Pixelforge: build, test, lint. `make` alone does a debug build.\n.PHONY: build test lint clean\n\nbuild:\n\\tcargo build --workspace\n\ntest:\n\\tcargo test --workspace -- --test-threads=1\n\\tnpm --prefix web run test\n\nlint:\n\\tcargo clippy --workspace -- -D warnings\n\\tnpm --prefix web run lint\n\nclean:\n\\tcargo clean\n\n",
    ),
    (
        "Dockerfile",
        "# Multi-stage: build the web bundle with node, ship it under nginx.\nFROM node:20-slim AS web\nWORKDIR /app\nCOPY web/package.json web/package-lock.json ./\nRUN npm ci\nCOPY web/ .\nRUN npm run build\n\nFROM nginx:1.27-alpine\nCOPY --from=web /app/dist /usr/share/nginx/html\nEXPOSE 80\n\n",
    ),
    (
        "deploy/Jenkinsfile",
        "// Continuous delivery: build, test, push the image, roll it out.\npipeline {\n  agent any\n  stages {\n    stage('build') { steps { sh 'make build' } }\n    stage('test') { steps { sh 'make test' } }\n    stage('lint') { steps { sh 'make lint' } }\n    stage('image') { steps { sh 'docker build -t pixelforge:$GIT_COMMIT .' } }\n    stage('deploy') { steps { sh './scripts/rollout.sh pixelforge:$GIT_COMMIT' } }\n  }\n  post { failure { slackSend channel: '#deploys', message: \"pixelforge failed: ${BUILD_URL}\" } }\n}\n\n",
    ),
    (
        "docs/architecture.md",
        "# Pixelforge architecture\n\n## Surviving a flaky dependency\n\nEvery call that crosses the network goes through the retry wrapper: a failed\ncall is retried with exponential backoff and full jitter (see\n`retry_with_backoff`), because retries without jitter arrive in waves and\nflatten the very service they are trying to reach. When failures keep piling\nup the circuit breaker opens and the call fails fast for a cool-down instead\nof piling onto a dependency that is already drowning.\n\n## Why reads go through the cache\n\nThumbnails and session lookups are read far more often than they change, so\nevery read passes through the LRU cache first and only a miss reaches the\ndatabase. The cache is bounded on purpose: an unbounded cache is a memory\nleak with good manners, and the eviction of the least recently used entry\nkeeps the hot set in memory at a fixed price.\n\n## Layout\n\n`src/` is the Rust service, `web/` the React bundle, `core/` the Go cache\nlibrary, `deploy/` the delivery pipeline.\n\n",
    ),
];

/// The code eval cases, tagged like DigUp's evals: `code` asks for behaviour in words,
/// `code-name` looks up an identifier as written, `code-doc` asks the repo's prose.
/// Expectations are `path@first-last` — the path (glob) of the answering stretch and the lines
/// it must overlap; a bare path accepts any stretch in the file.
pub fn code_cases() -> Vec<Case> {
    [
        (
            "retry a failed call with exponential backoff and jitter",
            "code",
            "src/net/retry.py@6-18",
        ),
        (
            "stop calling a service that keeps failing until it heals",
            "code",
            "src/net/retry.py@22-35",
        ),
        (
            "charge a customer's card and handle the bank declining it",
            "code",
            "src/net/gateway.py@12-22",
        ),
        (
            "give a customer their money back",
            "code",
            "src/net/gateway.py@25-26",
        ),
        (
            "log a user in and start a signed session",
            "code",
            "src/auth.rs@14-25",
        ),
        (
            "issue a fresh access token when the session expires",
            "code",
            "src/auth.rs@28-38",
        ),
        (
            "shrink a picture to fit its box without distorting it",
            "code",
            "src/thumbs.rs@10-15",
        ),
        (
            "drop the least recently used entry when the cache is full",
            "code",
            "core/cache.go@45-51",
        ),
        (
            "wait for the user to stop typing before searching",
            "code",
            "web/app.tsx@5-13",
        ),
        (
            "break a select statement into keywords and quoted values",
            "code",
            "src/parse/sql.rs@27-36",
        ),
        (
            "style the search box with a rounded border",
            "code",
            "web/styles.css@3-10",
        ),
        (
            "multi-stage container image that builds the web bundle",
            "code",
            "Dockerfile@2-8",
        ),
        (
            "continuous delivery pipeline that builds, tests and deploys",
            "code",
            "deploy/Jenkinsfile@2-9",
        ),
        (
            "how does the service survive a dependency that keeps timing out",
            "code-doc",
            "docs/architecture.md@3-10",
        ),
        (
            "why does every read go through the cache first",
            "code-doc",
            "docs/architecture.md@12-18",
        ),
        ("retry_with_backoff", "code-name", "src/net/retry.py@6-18"),
        ("CircuitBreaker", "code-name", "src/net/retry.py@22-24"),
        ("PaymentDeclined", "code-name", "src/net/gateway.py"),
        ("useDebouncedQuery", "code-name", "web/app.tsx@5-12"),
        ("NewLRU", "code-name", "core/cache.go@19-21"),
        ("fit_inside", "code-name", "src/thumbs.rs@17-28"),
    ]
    .iter()
    .map(|(q, tag, expect)| Case {
        q: (*q).into(),
        tag: (*tag).into(),
        expect: vec![(*expect).into()],
    })
    .collect()
}

/// Write the code corpus into `dir` (relative paths preserved) + corpus.json. Returns the
/// number of files written.
pub fn generate_code(dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dir)?;
    for (name, text) in CODE_FILES {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(p, text)?;
    }
    let idx = CorpusIndex {
        version: 1,
        files: CODE_FILES
            .iter()
            .map(|(name, _)| CorpusFile {
                name: (*name).into(),
                kind: "code".into(),
                desc: String::new(),
            })
            .collect(),
    };
    let json = serde_json::to_string_pretty(&idx).expect("serialise");
    std::fs::write(dir.join("corpus.json"), json + "\n")?;
    Ok(CODE_FILES.len())
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
            assert_eq!(
                std::fs::read(a.path().join(&f.name)).unwrap(),
                std::fs::read(b.path().join(&f.name)).unwrap()
            );
        }
        assert_eq!(
            std::fs::read(a.path().join("corpus.json")).unwrap(),
            std::fs::read(b.path().join("corpus.json")).unwrap()
        );
        assert_eq!(load_index(a.path()).unwrap(), index());
    }

    #[test]
    fn names_are_opaque_and_unique() {
        let idx = index();
        let mut names: Vec<_> = idx.files.iter().map(|f| f.name.clone()).collect();
        for n in &names {
            assert!(n.starts_with("img_") || n.starts_with("note_"), "{n}");
            assert!(
                !["red", "blue", "circle", "stripes", "page"]
                    .iter()
                    .any(|w| n.contains(w)),
                "{n}"
            );
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
                assert!(
                    names.iter().any(|n| glob_match(e, n)),
                    "{} expects missing {e}",
                    c.q
                );
            }
        }
        let tags: std::collections::HashSet<_> = cs.iter().map(|c| c.tag.as_str()).collect();
        for t in [
            "color", "shape", "pattern", "gradient", "doc", "text", "xling",
        ] {
            assert!(tags.contains(t), "{t}");
        }
        assert!(cs.iter().filter(|c| c.tag == "xling").count() >= 2);
    }

    #[test]
    fn committed_cases_json_is_in_sync() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/cases.json");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            parse_cases(&text).unwrap(),
            cases(),
            "eval/cases.json is stale; regenerate with `winseek-eval cases > eval/cases.json`"
        );
    }

    #[test]
    fn committed_title_cases_json_is_in_sync() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/title-cases.json");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            parse_cases(&text).unwrap(),
            title_cases(),
            "eval/title-cases.json is stale; regenerate with `winseek-eval cases --titles > eval/title-cases.json`"
        );
    }

    #[test]
    fn titled_corpus_writes_unique_files() {
        let dir = tempfile::tempdir().unwrap();
        let n = generate_with_titles(dir.path()).unwrap();
        assert_eq!(n, SPECS.len() + TITLE_NOTES.len());
        let idx = load_index(dir.path()).unwrap();
        let mut names: Vec<_> = idx.files.iter().map(|f| f.name.clone()).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "file names collide");
        for c in title_cases() {
            for e in &c.expect {
                assert!(
                    names.iter().any(|n| glob_match(e, n)),
                    "{} expects missing {e}",
                    c.q
                );
            }
        }
    }
}
