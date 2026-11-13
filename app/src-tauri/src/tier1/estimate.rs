//! First-pass ETA for one folder, worked out on this machine before the folder is added.
//!
//! Two measurements, nothing assumed:
//! * the folder is walked (counting what the crawler would queue, by reader kind), and a handful
//!   of its files are read and timed right now (office/text/code extraction, image decoding);
//! * the indexing speed the worker last achieved on this PC (files/s, remembered across runs).
//!
//! With an observed speed the ETA is `files / speed`. Without one (models never ran here) only
//! the reading time is known, so the answer is marked as a lower bound instead of guessing the
//! embedding cost.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use winseek_store::{in_scope_with, Scopes};

use super::crawler::{kind_of, skip_attrs, KIND_CODE, KIND_DOC, KIND_IMAGE, KIND_PDF};
use super::extract::extract_file;
use super::gitignore::Chain;
use super::identity::ATTR_DIRECTORY;
use super::{lock, Handle, Tier1};

/// Walking stops here; the counts are then partial and say so.
const WALK_BUDGET: Duration = Duration::from_secs(8);
/// Reading the sample files stops here.
const MEASURE_BUDGET: Duration = Duration::from_secs(3);
/// Files timed per kind.
const SAMPLES_PER_KIND: usize = 12;
/// Larger files are not timed (the worker has its own limits; one huge file would skew the mean).
const SAMPLE_MAX_BYTES: u64 = 8 << 20;
/// Store setting: the indexing speed the worker last achieved, files per second.
const SETTING_RATE: &str = "observed_rate";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub images: u64,
    pub pdfs: u64,
    pub docs: u64,
    pub code: u64,
    pub bytes: u64,
}

impl Counts {
    pub fn files(&self) -> u64 {
        self.images + self.pdfs + self.docs + self.code
    }

    fn add(&mut self, kind: &str, size: u64) {
        match kind {
            KIND_IMAGE => self.images += 1,
            KIND_PDF => self.pdfs += 1,
            KIND_DOC => self.docs += 1,
            KIND_CODE => self.code += 1,
            _ => return,
        }
        self.bytes += size;
    }
}

/// Seconds per file, measured by timing sample files; `None` for a kind that could not be timed.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PerFile {
    pub image: Option<f64>,
    pub pdf: Option<f64>,
    pub doc: Option<f64>,
    pub code: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    pub secs: Option<f64>,
    /// The real time is longer: parts of the work could not be measured.
    pub lower_bound: bool,
    /// "observed" (the worker's own speed), "sampled" (timed reading only) or "none".
    pub basis: &'static str,
}

/// Turn counts + measurements into a time. Pure.
pub fn project(c: &Counts, per: &PerFile, observed: Option<f64>) -> Projection {
    if c.files() == 0 {
        return Projection { secs: Some(0.0), lower_bound: false, basis: "none" };
    }
    if let Some(r) = observed.filter(|r| r.is_finite() && *r > 0.0) {
        return Projection { secs: Some(c.files() as f64 / r), lower_bound: false, basis: "observed" };
    }
    let mut total = 0.0;
    let mut known = false;
    for (n, p) in [(c.images, per.image), (c.pdfs, per.pdf), (c.docs, per.doc), (c.code, per.code)] {
        if n == 0 {
            continue;
        }
        if let Some(p) = p {
            total += n as f64 * p;
            known = true;
        }
    }
    // Embedding is not part of a reading-only measurement, so this is always a floor.
    Projection { secs: known.then_some(total), lower_bound: true, basis: if known { "sampled" } else { "none" } }
}

#[derive(Debug, Default)]
struct Samples {
    image: Vec<PathBuf>,
    pdf: Vec<PathBuf>,
    doc: Vec<PathBuf>,
    code: Vec<PathBuf>,
}

impl Samples {
    fn offer(&mut self, kind: &str, path: &Path, size: u64) {
        if size > SAMPLE_MAX_BYTES {
            return;
        }
        let v = match kind {
            KIND_IMAGE => &mut self.image,
            KIND_PDF => &mut self.pdf,
            KIND_DOC => &mut self.doc,
            KIND_CODE => &mut self.code,
            _ => return,
        };
        if v.len() < SAMPLES_PER_KIND {
            v.push(path.to_path_buf());
        }
    }
}

/// Count what the crawler would queue under `root`, and keep a few files of each kind to time.
fn walk(root: &Path, scopes: &Scopes, ignore: &winseek_store::Ignore, index_code: bool, deadline: Instant) -> (Counts, Samples, bool) {
    use std::os::windows::fs::MetadataExt;
    let mut counts = Counts::default();
    let mut samples = Samples::default();
    let mut stack: Vec<(PathBuf, Chain)> = vec![(root.to_path_buf(), Chain::default())];
    let mut seen = 0u32;
    while let Some((dir, parent)) = stack.pop() {
        let chain = if index_code { parent.descend(&dir) } else { parent };
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            seen += 1;
            if seen.is_multiple_of(256) && Instant::now() >= deadline {
                return (counts, samples, true);
            }
            let Ok(md) = entry.metadata() else { continue };
            let attrs = md.file_attributes();
            if skip_attrs(attrs) {
                continue;
            }
            let path = entry.path();
            let ps = path.to_string_lossy();
            if attrs & ATTR_DIRECTORY != 0 {
                if in_scope_with(scopes, ignore, &ps) {
                    stack.push((path, chain.clone()));
                }
                continue;
            }
            let Some(kind) = kind_of(&ps, index_code) else { continue };
            if !in_scope_with(scopes, ignore, &ps) || (kind == KIND_CODE && chain.ignored(&ps, false)) {
                continue;
            }
            counts.add(kind, md.len());
            samples.offer(kind, &path, md.len());
        }
    }
    (counts, samples, false)
}

/// Mean seconds per file over `files`, timing `one` on each until the deadline; `None` when
/// nothing could be timed.
fn time_each(files: &[PathBuf], deadline: Instant, one: impl Fn(&Path) -> bool) -> Option<f64> {
    let (mut n, mut spent) = (0u32, Duration::ZERO);
    for f in files {
        if Instant::now() >= deadline {
            break;
        }
        let t = Instant::now();
        if one(f) {
            spent += t.elapsed();
            n += 1;
        }
    }
    (n > 0).then(|| spent.as_secs_f64() / n as f64)
}

/// Read + extract text from a file, the way the worker does.
fn extract_ok(p: &Path) -> bool {
    std::fs::read(p).ok().is_some_and(|bytes| extract_file(&p.to_string_lossy(), &bytes).is_ok())
}

fn decode_ok(p: &Path) -> bool {
    image::ImageReader::open(p).ok().and_then(|r| r.with_guessed_format().ok()).is_some_and(|r| r.decode().is_ok())
}

fn measure(s: &Samples) -> PerFile {
    let deadline = Instant::now() + MEASURE_BUDGET;
    PerFile {
        doc: time_each(&s.doc, deadline, extract_ok),
        code: time_each(&s.code, deadline, extract_ok),
        image: time_each(&s.image, deadline, decode_ok),
        pdf: None, // needs pdfium, which may not be downloaded yet: reported as unmeasured
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FolderEstimate {
    pub path: String,
    pub files: u64,
    pub images: u64,
    pub pdfs: u64,
    pub docs: u64,
    pub code: u64,
    pub bytes: u64,
    /// Estimated seconds for the first pass; `None` when it cannot be measured yet.
    pub secs: Option<u64>,
    /// The real time is longer than `secs`.
    pub lower_bound: bool,
    /// The walk hit its time budget: the counts (and so the time) are partial.
    pub capped: bool,
    /// "observed" | "sampled" | "none"
    pub basis: String,
    /// Indexing speed the time is based on (files/s), when observed.
    pub rate: Option<f64>,
}

impl Tier1 {
    /// Remember the worker's current speed so a later estimate can use it. Call now and then;
    /// does nothing while the worker is not producing a rate.
    pub fn remember_rate(&self) {
        if let Some(r) = self.rate().filter(|r| r.is_finite() && *r > 0.0) {
            let _ = lock(&self.store).set_setting(SETTING_RATE, Some(&format!("{r:.3}")));
        }
    }

    /// The last remembered speed, files per second.
    pub fn remembered_rate(&self) -> Option<f64> {
        let s = lock(&self.store).get_setting(SETTING_RATE).ok().flatten()?;
        s.parse::<f64>().ok().filter(|r| r.is_finite() && *r > 0.0)
    }

    /// Walk `path`, time a sample of its files, and project the first pass. Blocking (up to ~11 s).
    pub fn estimate_folder(&self, path: &str) -> Result<FolderEstimate, String> {
        let root = super::scopes::normalize_root(path);
        let dir = PathBuf::from(if root.len() == 2 && root.ends_with(':') { format!("{root}\\") } else { root.clone() });
        if !dir.is_dir() {
            return Err(format!("{root}: not a folder"));
        }
        let snap = self.snapshot();
        let scopes = Scopes { include: vec![root.clone()], exclude: snap.scopes.exclude.clone(), configured: true };
        let (counts, samples, capped) = walk(&dir, &scopes, &snap.ignore, snap.index_code, Instant::now() + WALK_BUDGET);
        let observed = self.rate().or_else(|| self.remembered_rate());
        let proj = project(&counts, &measure(&samples), observed);
        Ok(FolderEstimate {
            path: root,
            files: counts.files(),
            images: counts.images,
            pdfs: counts.pdfs,
            docs: counts.docs,
            code: counts.code,
            bytes: counts.bytes,
            secs: proj.secs.map(|s| s.ceil() as u64),
            lower_bound: proj.lower_bound || capped,
            capped,
            basis: proj.basis.into(),
            rate: if proj.basis == "observed" { observed } else { None },
        })
    }
}

/// Measure a folder's first-pass time before adding it to the indexed folders.
#[tauri::command]
pub async fn tier1_estimate(h: tauri::State<'_, Handle>, path: String) -> Result<FolderEstimate, String> {
    let t = h.get()?;
    crate::commands::blocking(move || t.estimate_folder(&path)).await?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(images: u64, pdfs: u64, docs: u64, code: u64) -> Counts {
        Counts { images, pdfs, docs, code, bytes: 0 }
    }

    #[test]
    fn observed_speed_wins() {
        let p = project(&c(100, 0, 50, 0), &PerFile::default(), Some(5.0));
        assert_eq!((p.secs, p.lower_bound, p.basis), (Some(30.0), false, "observed"));
        // a nonsense speed is ignored
        let p = project(&c(10, 0, 0, 0), &PerFile::default(), Some(f64::NAN));
        assert_eq!(p.basis, "none");
    }

    #[test]
    fn sampled_reading_is_a_floor_and_unmeasured_kinds_add_nothing() {
        let per = PerFile { doc: Some(0.02), image: Some(0.05), ..PerFile::default() };
        let p = project(&c(10, 400, 100, 0), &per, None);
        assert!((p.secs.unwrap() - (0.5 + 2.0)).abs() < 1e-9);
        assert!(p.lower_bound);
        assert_eq!(p.basis, "sampled");
        // only PDFs, nothing measurable
        let p = project(&c(0, 5, 0, 0), &per, None);
        assert_eq!((p.secs, p.basis), (None, "none"));
    }

    #[test]
    fn empty_folder_is_instant() {
        let p = project(&c(0, 0, 0, 0), &PerFile::default(), None);
        assert_eq!((p.secs, p.lower_bound), (Some(0.0), false));
    }

    #[test]
    fn walks_counts_and_times_a_real_folder() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        for i in 0..5 {
            std::fs::write(d.path().join(format!("n{i}.txt")), "hello world ".repeat(200)).unwrap();
        }
        std::fs::write(sub.join("p.pdf"), b"%PDF-1.4").unwrap();
        std::fs::write(sub.join("skip.xyz"), b"x").unwrap();
        let root = d.path().to_string_lossy().into_owned();
        let scopes = Scopes { include: vec![root], exclude: vec![], configured: true };
        let ignore = winseek_store::Ignore::default();
        let (counts, samples, capped) = walk(d.path(), &scopes, &ignore, false, Instant::now() + Duration::from_secs(5));
        assert!(!capped);
        assert_eq!((counts.docs, counts.pdfs, counts.images, counts.code), (5, 1, 0, 0));
        assert_eq!(counts.bytes, 5 * 2400 + 8);
        let per = measure(&samples);
        assert!(per.doc.is_some_and(|s| s >= 0.0));
        assert_eq!(per.pdf, None);
    }

    #[test]
    fn an_expired_budget_marks_the_walk_partial() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..600 {
            std::fs::write(d.path().join(format!("n{i}.txt")), "x").unwrap();
        }
        let scopes = Scopes { include: vec![d.path().to_string_lossy().into_owned()], exclude: vec![], configured: true };
        let (counts, _, capped) = walk(d.path(), &scopes, &winseek_store::Ignore::default(), false, Instant::now());
        assert!(capped);
        assert!(counts.files() < 600);
    }
}
