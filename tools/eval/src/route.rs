//! `winseek-eval route`: measure the app's zero-shot thresholds on a labelled image set.
//!
//! The set is `<dir>/doc/<category>/*.jpg|png` (document-like: should go to the parser) and
//! `<dir>/other/<category>/*` (photos, memes, screenshots: should not). Every image is embedded
//! exactly like the app does it (`prepare_jpeg` -> EmbedIndex server -> truncate + normalise), the
//! prototype prompts and the default smart-collection queries are embedded as queries, and the
//! thresholds are swept. Prompts and queries are copies of the app's; tests keep them in sync.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;
use winseek_llama::prep::prepare_jpeg;

use crate::run::{connect, finish, Embed, RunOptions};

/// Copies of `app/src-tauri/src/tier1/route.rs` (checked by `prompts_match_the_app`).
pub const DOC_PROMPTS: [&str; 6] = [
    "a scanned document page",
    "a receipt",
    "a whiteboard with writing",
    "a printed form",
    "a presentation slide",
    "a page of a book",
];
pub const OTHER_PROMPTS: [&str; 6] = [
    "a photo of people",
    "a landscape photo",
    "a photo of an animal",
    "a photo of food",
    "a meme",
    "a product photo",
];

/// Default smart collections (`app/src-tauri/src/tier1/library.rs`): name, query, and the set
/// categories that count as members.
pub const COLLECTIONS: [(&str, &str, &[&str]); 4] = [
    ("Receipts", "a photo of a paper receipt or invoice", &["receipt", "invoice"]),
    ("Whiteboards", "a whiteboard with handwritten notes and diagrams", &["whiteboard"]),
    ("Memes", "a meme: an image with a funny caption", &["meme"]),
    ("Documents to sign", "a contract or form with a signature line to sign", &["contract", "form"]),
];

const BATCH: usize = 8;

#[derive(Debug, Clone, Serialize)]
pub struct Scored {
    pub path: String,
    pub doc: bool,
    pub category: String,
    /// Best cosine against the document / non-document prototypes.
    pub best_doc: f32,
    pub best_other: f32,
    /// Cosine against each `COLLECTIONS` query, in order.
    pub collections: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Confusion {
    pub doc_min: f32,
    pub margin: f32,
    pub tp: usize,
    pub fp: usize,
    pub fn_: usize,
    pub tn: usize,
}

impl Confusion {
    pub fn precision(&self) -> f32 {
        if self.tp + self.fp == 0 { 1.0 } else { self.tp as f32 / (self.tp + self.fp) as f32 }
    }
    pub fn recall(&self) -> f32 {
        if self.tp + self.fn_ == 0 { 1.0 } else { self.tp as f32 / (self.tp + self.fn_) as f32 }
    }
    pub fn fp_rate(&self) -> f32 {
        if self.fp + self.tn == 0 { 0.0 } else { self.fp as f32 / (self.fp + self.tn) as f32 }
    }
}

/// Same rule as `Prototypes::is_doc` in the app.
pub fn is_doc(s: &Scored, doc_min: f32, margin: f32) -> bool {
    s.best_doc >= doc_min && s.best_doc - s.best_other >= margin
}

pub fn confusion(set: &[Scored], doc_min: f32, margin: f32) -> Confusion {
    let mut c = Confusion { doc_min, margin, tp: 0, fp: 0, fn_: 0, tn: 0 };
    for s in set {
        match (s.doc, is_doc(s, doc_min, margin)) {
            (true, true) => c.tp += 1,
            (false, true) => c.fp += 1,
            (true, false) => c.fn_ += 1,
            (false, false) => c.tn += 1,
        }
    }
    c
}

fn grid(lo: f32, hi: f32, step: f32) -> Vec<f32> {
    let n = ((hi - lo) / step).round() as usize;
    (0..=n).map(|i| ((lo + i as f32 * step) * 1000.0).round() / 1000.0).collect()
}

/// Every (doc_min, margin) pair on the grid. EmbeddingGemma-2 image/query cosines live in about
/// 0.45..0.80, so that is where the minimum is swept.
pub fn sweep(set: &[Scored]) -> Vec<Confusion> {
    let mut out = Vec::new();
    for m in grid(-0.02, 0.10, 0.01) {
        for d in grid(0.40, 0.80, 0.01) {
            out.push(confusion(set, d, m));
        }
    }
    out
}

/// Highest recall with a false-positive rate at most `max_fp_rate`; ties go to the stricter pair
/// (larger margin, then larger minimum), which is the cheaper mistake.
pub fn pick(sweep: &[Confusion], max_fp_rate: f32) -> Option<Confusion> {
    sweep.iter().filter(|c| c.fp_rate() <= max_fp_rate).copied().max_by(|a, b| {
        a.tp.cmp(&b.tp).then(a.margin.total_cmp(&b.margin)).then(a.doc_min.total_cmp(&b.doc_min))
    })
}

/// Per collection: the score threshold with the best F1 over the set, plus that F1's P/R. On a set
/// rich in documents this overfits (it trades photos for other documents); see `midpoint`.
pub fn collection_best(set: &[Scored], i: usize, members: &[&str]) -> (f32, f32, f32, f32) {
    let mut best = (0.0f32, 0.0f32, 0.0f32, 0.0f32); // (threshold, f1, precision, recall)
    for t in grid(0.30, 0.90, 0.005) {
        let (mut tp, mut fp, mut fn_) = (0usize, 0usize, 0usize);
        for s in set {
            match (members.contains(&s.category.as_str()), s.collections[i] >= t) {
                (true, true) => tp += 1,
                (false, true) => fp += 1,
                (true, false) => fn_ += 1,
                _ => {}
            }
        }
        let p = if tp + fp == 0 { 1.0 } else { tp as f32 / (tp + fp) as f32 };
        let r = if tp + fn_ == 0 { 1.0 } else { tp as f32 / (tp + fn_) as f32 };
        let f1 = if p + r == 0.0 { 0.0 } else { 2.0 * p * r / (p + r) };
        if f1 > best.1 {
            best = (t, f1, p, r);
        }
    }
    best
}

/// The threshold the app ships: halfway between the best-scoring non-member *non-document* image
/// (a photo in a collection is the visible mistake) and the members' 10th percentile, to 0.01.
/// `None` without members or photos.
pub fn midpoint(set: &[Scored], i: usize, members: &[&str]) -> Option<f32> {
    let photo_max = set.iter().filter(|x| !x.doc && !members.contains(&x.category.as_str())).map(|x| x.collections[i]).reduce(f32::max)?;
    let mut pos: Vec<f32> = set.iter().filter(|x| members.contains(&x.category.as_str())).map(|x| x.collections[i]).collect();
    if pos.is_empty() {
        return None;
    }
    pos.sort_by(f32::total_cmp);
    let p10 = pos[((pos.len() - 1) as f32 * 0.1).round() as usize];
    Some(((photo_max + p10) / 2.0 * 100.0).round() / 100.0)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn best(protos: &[Vec<f32>], v: &[f32]) -> f32 {
    protos.iter().map(|p| dot(p, v)).fold(f32::NEG_INFINITY, f32::max)
}

/// `(path, is doc, category)` for every image under `<dir>/{doc,other}/<category>/`.
pub fn list_set(dir: &Path) -> Result<Vec<(PathBuf, bool, String)>, String> {
    let mut out = Vec::new();
    for (kind, doc) in [("doc", true), ("other", false)] {
        let root = dir.join(kind);
        let mut cats: Vec<_> = std::fs::read_dir(&root).map_err(|e| format!("{}: {e}", root.display()))?.flatten().filter(|e| e.path().is_dir()).collect();
        cats.sort_by_key(|e| e.file_name());
        for cat in cats {
            let mut files: Vec<PathBuf> = std::fs::read_dir(cat.path())
                .map_err(|e| e.to_string())?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()).is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png")))
                .collect();
            files.sort();
            let name = cat.file_name().to_string_lossy().into_owned();
            out.extend(files.into_iter().map(|p| (p, doc, name.clone())));
        }
    }
    Ok(out)
}

/// Embed the set and score it against the prompts and collection queries.
pub fn score(set: &[(PathBuf, bool, String)], emb: &mut dyn Embed) -> Result<Vec<Scored>, String> {
    let mut q = |texts: &[&str]| -> Result<Vec<Vec<f32>>, String> { finish(emb.queries(texts)?) };
    let doc = q(&DOC_PROMPTS)?;
    let other = q(&OTHER_PROMPTS)?;
    let coll = q(&COLLECTIONS.map(|c| c.1))?;
    let mut out = Vec::with_capacity(set.len());
    for (k, batch) in set.chunks(BATCH).enumerate() {
        let jpegs = batch
            .iter()
            .map(|(p, _, _)| std::fs::read(p).map_err(|e| e.to_string()).and_then(|b| prepare_jpeg(&b)).map_err(|e| format!("{}: {e}", p.display())))
            .collect::<Result<Vec<_>, _>>()?;
        let refs: Vec<&[u8]> = jpegs.iter().map(Vec::as_slice).collect();
        let vecs = finish(emb.images(&refs)?)?;
        for ((p, is_doc, cat), v) in batch.iter().zip(vecs) {
            out.push(Scored {
                path: p.display().to_string(),
                doc: *is_doc,
                category: cat.clone(),
                best_doc: best(&doc, &v),
                best_other: best(&other, &v),
                collections: coll.iter().map(|c| dot(c, &v)).collect(),
            });
        }
        eprint!("\r{} / {}", ((k + 1) * BATCH).min(set.len()), set.len());
    }
    eprintln!();
    Ok(out)
}

fn stats(xs: &mut [f32]) -> String {
    xs.sort_by(f32::total_cmp);
    let q = |f: f32| xs[((xs.len() - 1) as f32 * f).round() as usize];
    format!("min {:.3}  p10 {:.3}  med {:.3}  p90 {:.3}  max {:.3}", q(0.0), q(0.1), q(0.5), q(0.9), q(1.0))
}

/// Human-readable report of a scored set.
pub fn report(set: &[Scored], set_dir: &Path, max_fp_rate: f32, current: (f32, f32)) -> String {
    let root = set_dir.to_string_lossy().into_owned();
    let short = |p: &str| p.strip_prefix(root.as_str()).unwrap_or(p).trim_start_matches(['/', '\\']).to_string();
    let mut s = String::new();
    let mut cats: Vec<(bool, &str)> = set.iter().map(|x| (x.doc, x.category.as_str())).collect();
    cats.dedup();
    s += "per category: best_doc | best_doc - best_other\n";
    for (doc, cat) in &cats {
        let of: Vec<&Scored> = set.iter().filter(|x| x.category == *cat && x.doc == *doc).collect();
        let mut d: Vec<f32> = of.iter().map(|x| x.best_doc).collect();
        let mut m: Vec<f32> = of.iter().map(|x| x.best_doc - x.best_other).collect();
        s += &format!("  {:5} {:12} n={:3}  doc {}\n  {:24}margin {}\n", if *doc { "doc" } else { "other" }, cat, of.len(), stats(&mut d), "", stats(&mut m));
    }
    let sw = sweep(set);
    let line = |c: &Confusion| {
        format!(
            "  min {:.2} margin {:+.2}: P {:.3} R {:.3} FP {} ({:.1}%) FN {}\n",
            c.doc_min, c.margin, c.precision(), c.recall(), c.fp, c.fp_rate() * 100.0, c.fn_
        )
    };
    s += "current:\n";
    s += &line(&confusion(set, current.0, current.1));
    s += "best recall per margin with FP rate <= limit:\n";
    for m in grid(-0.02, 0.10, 0.01) {
        let row: Vec<Confusion> = sw.iter().filter(|c| c.margin == m).copied().collect();
        if let Some(c) = pick(&row, max_fp_rate) {
            s += &line(&c);
        }
    }
    if let Some(c) = pick(&sw, max_fp_rate) {
        s += &format!("pick (FP rate <= {:.1}%):\n", max_fp_rate * 100.0);
        s += &line(&c);
        let flagged: Vec<&Scored> = set.iter().filter(|x| !x.doc && is_doc(x, c.doc_min, c.margin)).collect();
        for x in flagged {
            s += &format!("    FP {} (doc {:.3}, other {:.3})\n", short(&x.path), x.best_doc, x.best_other);
        }
    }
    s += "collections (midpoint rule; F1-best threshold over the set for comparison):\n";
    for (i, (name, _, members)) in COLLECTIONS.iter().enumerate() {
        let (t, f1, p, r) = collection_best(set, i, members);
        let member = |x: &Scored| members.contains(&x.category.as_str());
        let of = |f: &dyn Fn(&Scored) -> bool| set.iter().filter(|x| f(x)).map(|x| x.collections[i]).collect::<Vec<f32>>();
        let mut pos = of(&|x| member(x));
        let mut docs = of(&|x| x.doc && !member(x));
        let mut photos = of(&|x| !x.doc && !member(x));
        if let Some(m) = midpoint(set, i, members) {
            let n = |v: &[f32]| v.iter().filter(|&&x| x >= m).count();
            s += &format!("  {name:18} midpoint {m:.2}: members {}/{}  other docs {}  non-docs {}\n", n(&pos), pos.len(), n(&docs), n(&photos));
        }
        s += &format!("  {:18} F1-best {t:.3}: F1 {f1:.3} P {p:.3} R {r:.3}\n", "");
        for (k, v) in [("members", &mut pos), ("other docs", &mut docs), ("non-docs", &mut photos)] {
            if !v.is_empty() {
                s += &format!("    {k:10} {}\n", stats(v));
            }
        }
    }
    s
}

/// Live run: connect / spawn, score the set, write the JSON (meta, current, pick, every image).
pub fn run(opts: &RunOptions, set_dir: &Path, max_fp_rate: f32, current: (f32, f32), out: Option<&Path>) -> Result<String, String> {
    let set = list_set(set_dir)?;
    if set.is_empty() {
        return Err(format!("no images under {}", set_dir.display()));
    }
    let mut meta = json!({ "set": set_dir.display().to_string(), "images": set.len(), "max_fp_rate": max_fp_rate });
    let mut conn = connect(opts, &mut meta)?;
    eprintln!("embedding {} images...", set.len());
    let scored = score(&set, &mut conn.client)?;
    let text = report(&scored, set_dir, max_fp_rate, current);
    if let Some(out) = out {
        let doc = json!({
            "meta": meta,
            "current": confusion(&scored, current.0, current.1),
            "pick": pick(&sweep(&scored), max_fp_rate),
            "images": scored,
        });
        let s = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        std::fs::write(out, s + "\n").map_err(|e| format!("{}: {e}", out.display()))?;
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc(doc: bool, cat: &str, d: f32, o: f32) -> Scored {
        Scored { path: String::new(), doc, category: cat.into(), best_doc: d, best_other: o, collections: vec![d; 4] }
    }

    #[test]
    fn prompts_match_the_app() {
        let route = include_str!("../../../app/src-tauri/src/tier1/route.rs");
        for p in DOC_PROMPTS.iter().chain(&OTHER_PROMPTS) {
            assert!(route.contains(&format!("\"{p}\",")), "{p} not in the app's route.rs");
        }
        let lib = include_str!("../../../app/src-tauri/src/tier1/library.rs");
        for (name, query, _) in COLLECTIONS {
            assert!(lib.contains(&format!("c(\"{name}\", \"{query}\"")), "{name} not in the app's default collections");
        }
    }

    #[test]
    fn confusion_counts_and_rates() {
        let set = [sc(true, "a", 0.3, 0.1), sc(true, "a", 0.1, 0.0), sc(false, "b", 0.3, 0.29), sc(false, "b", 0.0, 0.3)];
        let c = confusion(&set, 0.12, 0.03);
        assert_eq!((c.tp, c.fn_, c.fp, c.tn), (1, 1, 0, 2));
        assert_eq!((c.precision(), c.recall(), c.fp_rate()), (1.0, 0.5, 0.0));
        let c = confusion(&set, 0.05, 0.0);
        assert_eq!((c.tp, c.fp), (2, 1));
    }

    #[test]
    fn pick_maximises_recall_under_the_fp_limit_and_prefers_strict() {
        let set = [sc(true, "a", 0.70, 0.60), sc(true, "a", 0.65, 0.605), sc(false, "b", 0.66, 0.63)];
        let p = pick(&sweep(&set), 0.0).unwrap();
        // the photo has margin 0.03: catching doc #2 (margin 0.045) needs margin 0.04, min <= 0.65
        assert_eq!((p.tp, p.fp), (2, 0));
        assert_eq!(p.margin, 0.04);
        assert_eq!(p.doc_min, 0.65);
    }

    #[test]
    fn collection_threshold_separates_members() {
        let set = [sc(true, "receipt", 0.7, 0.0), sc(true, "invoice", 0.65, 0.0), sc(false, "photo", 0.5, 0.0), sc(true, "book", 0.66, 0.0)];
        let (t, f1, p, r) = collection_best(&set, 0, &["receipt", "invoice"]);
        assert!(t > 0.5 && t <= 0.65, "{t}");
        assert_eq!(r, 1.0);
        assert!(p < 1.0 && f1 < 1.0, "the book is in the way");
        // midpoint: photo max 0.5, members p10 0.65 -> 0.575 -> 0.58 (rounded to 0.01)
        assert_eq!(midpoint(&set, 0, &["receipt", "invoice"]), Some(0.58));
        assert_eq!(midpoint(&set, 0, &["nothing"]), None);
    }

    #[test]
    fn set_layout_is_doc_and_other_by_category() {
        let d = tempfile::tempdir().unwrap();
        for (k, c, f) in [("doc", "receipt", "a.jpg"), ("doc", "receipt", "b.txt"), ("other", "photo", "c.PNG")] {
            std::fs::create_dir_all(d.path().join(k).join(c)).unwrap();
            std::fs::write(d.path().join(k).join(c).join(f), b"x").unwrap();
        }
        let s = list_set(d.path()).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].1, s[0].2.as_str()), (true, "receipt"));
        assert_eq!((s[1].1, s[1].2.as_str()), (false, "photo"));
    }
}
