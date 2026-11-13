//! Calibrated relevance for search by meaning.
//!
//! Raw cosines are not comparable between queries or between modalities: with EmbeddingGemma-2
//! every image sits at 0.46..0.76 against any query, and some vectors (app icons, tiny OCR
//! fragments, files that hold only a hash) are close to *every* query ("hubs"). A fixed cosine
//! threshold therefore lets junk through, and the old top-k showed whatever was least unrelated.
//!
//! Two corrections, then a gate:
//! 1. Hub correction: `adj = cos(q, v) - cos(hub, v)`, where `hub` is the mean embedding of
//!    [`HUB_PROMPTS`] (generic queries). A vector that matches everything loses its head start.
//! 2. Per-query, per-modality z-score of `adj` over the whole library (one value per file
//!    content): a hit must stand out from its own kind (images vs extracted text) for this query.
//! 3. Gate: `z >= Z_MIN` and `adj >= ADJ_MIN`. Nothing passes -> nothing is shown.
//!
//! Measured on this machine's real index (1,453 images + 2,630 texts, 20 hand-labelled queries,
//! see the gate sweep): precision of the shown meaning hits 0.19 -> 0.69, relevant hits shown
//! 31 -> 59; "email" went from 3/8 (LocalSend / Instagram screenshots) to 7/8 (Gmail icons,
//! email templates).

use std::collections::HashMap;

use crate::{FileId, VecKind};

/// Generic queries whose mean embedding is the "hub" direction. Deliberately broad and mixed
/// (kinds of images, documents, everyday subjects) so no single topic is penalised.
pub const HUB_PROMPTS: [&str; 48] = [
    "photo", "document", "screenshot", "a picture", "text", "notes", "a page", "an icon",
    "a logo", "a chart", "a table", "people", "a person", "landscape", "a building", "food",
    "an animal", "a car", "a phone", "a computer", "a website", "an app", "settings", "a menu",
    "a letter", "a report", "code", "a list", "a map", "music", "a video game", "a movie",
    "travel", "work", "family", "money", "a bill", "a book", "a drawing", "a diagram", "a form",
    "a message", "a chat", "news", "sports", "a room", "nature", "the sky",
];

/// Minimum standing above the library for this query, in standard deviations.
pub const Z_MIN: f32 = 4.0;
/// Minimum hub-corrected cosine.
pub const ADJ_MIN: f32 = 0.07;
/// Below this many files of a modality the z-score is not trusted; only `ADJ_MIN` applies.
pub const MIN_POPULATION: usize = 30;

/// Mean of the (normalised) hub prompt embeddings.
pub fn hub_vector(bank: &[Vec<f32>]) -> Vec<f32> {
    let Some(first) = bank.first() else { return Vec::new() };
    let mut m = vec![0.0f32; first.len()];
    for v in bank {
        for (a, b) in m.iter_mut().zip(v) {
            *a += b;
        }
    }
    let n = bank.len() as f32;
    m.iter_mut().for_each(|a| *a /= n);
    m
}

/// Images and rendered PDF pages compete with each other; extracted text with extracted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modality {
    Visual,
    Text,
    /// Soundtrack windows: their own population, so speech / music does not skew the z-scores of
    /// pictures or text.
    Audio,
}

impl From<VecKind> for Modality {
    fn from(k: VecKind) -> Self {
        match k {
            VecKind::Image | VecKind::Page => Modality::Visual,
            VecKind::Text => Modality::Text,
            VecKind::Audio => Modality::Audio,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub file_id: FileId,
    pub path: String,
    /// Content hash (copies of one file share it).
    pub hash: String,
    pub chunk: u32,
    pub locator: Option<String>,
    pub modality: Modality,
    /// Raw cosine.
    pub score: f32,
    /// Hub-corrected cosine.
    pub adj: f32,
    /// Standing of `adj` among this modality's files for this query; `None` = population too small.
    pub z: Option<f32>,
}

impl Candidate {
    pub fn passes(&self) -> bool {
        self.passes_at(Z_MIN)
    }
    /// Same gate with a caller-chosen strictness (collections dial it with their slider).
    pub fn passes_at(&self, z_min: f32) -> bool {
        self.adj >= ADJ_MIN && self.z.is_none_or(|z| z >= z_min)
    }
    /// Sort key: z when known, else a z-like scale of `adj`.
    pub fn rank(&self) -> f32 {
        self.z.unwrap_or(self.adj / ADJ_MIN * Z_MIN)
    }
}

/// Mean and population standard deviation.
pub fn mean_sd(xs: &[f32]) -> (f32, f32) {
    if xs.is_empty() {
        return (0.0, 0.0);
    }
    let n = xs.len() as f32;
    let mu = xs.iter().sum::<f32>() / n;
    let var = xs.iter().map(|x| (x - mu) * (x - mu)).sum::<f32>() / n;
    (mu, var.sqrt())
}

/// z of `x` in a population, or `None` when the population is too small / degenerate.
pub fn z_of(x: f32, n: usize, mu: f32, sd: f32) -> Option<f32> {
    (n >= MIN_POPULATION && sd > 1e-6).then(|| (x - mu) / sd)
}

/// Keep what passes the gate, one entry per file (its best modality), best first, at most `k`.
pub fn gate(cands: Vec<Candidate>, k: usize) -> Vec<Candidate> {
    gate_at(cands, k, Z_MIN)
}

/// [`gate`] with a caller-chosen strictness: collections map their slider onto `z_min`.
pub fn gate_at(cands: Vec<Candidate>, k: usize, z_min: f32) -> Vec<Candidate> {
    let mut best: HashMap<FileId, Candidate> = HashMap::new();
    for c in cands.into_iter().filter(|c| c.passes_at(z_min)) {
        match best.get(&c.file_id) {
            Some(b) if b.rank() >= c.rank() => {}
            _ => {
                best.insert(c.file_id, c);
            }
        }
    }
    let mut out: Vec<Candidate> = best.into_values().collect();
    out.sort_by(|a, b| b.rank().total_cmp(&a.rank()).then_with(|| a.file_id.0.cmp(&b.file_id.0)));
    out.truncate(k);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: i64, m: Modality, adj: f32, z: Option<f32>) -> Candidate {
        Candidate { file_id: FileId(id), path: format!("p{id}"), hash: format!("h{id}"), chunk: 0, locator: None, modality: m, score: 0.7, adj, z }
    }

    #[test]
    fn thresholds_are_the_measured_ones() {
        // Changing these needs a new measurement (scratch gate sweep).
        assert_eq!((Z_MIN, ADJ_MIN, MIN_POPULATION), (4.0, 0.07, 30));
        assert_eq!(HUB_PROMPTS.len(), 48);
    }

    #[test]
    fn gate_drops_weak_and_ordinary_hits() {
        let out = gate(
            vec![
                cand(1, Modality::Visual, 0.10, Some(5.0)), // passes
                cand(2, Modality::Visual, 0.05, Some(6.0)), // adj too low
                cand(3, Modality::Text, 0.12, Some(3.0)),   // not unusual for this query
                cand(4, Modality::Text, 0.08, Some(4.5)),   // passes
            ],
            10,
        );
        let ids: Vec<i64> = out.iter().map(|c| c.file_id.0).collect();
        assert_eq!(ids, vec![1, 4]);
    }

    #[test]
    fn gate_keeps_one_entry_per_file_and_truncates() {
        let out = gate(
            vec![
                cand(1, Modality::Visual, 0.09, Some(4.2)),
                cand(1, Modality::Text, 0.11, Some(6.0)),
                cand(2, Modality::Visual, 0.10, Some(5.0)),
                cand(3, Modality::Visual, 0.10, Some(4.1)),
            ],
            2,
        );
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].file_id.0, out[0].modality), (1, Modality::Text));
        assert_eq!(out[1].file_id.0, 2);
    }

    #[test]
    fn gate_at_dials_strictness() {
        let cands = vec![cand(1, Modality::Visual, 0.10, Some(4.2)), cand(2, Modality::Visual, 0.10, Some(3.8))];
        assert_eq!(gate_at(cands.clone(), 10, 4.0).len(), 1, "default strictness: only the 4.2 passes");
        assert_eq!(gate_at(cands.clone(), 10, 3.5).len(), 2, "looser: both pass");
        assert!(gate_at(cands, 10, 5.0).is_empty(), "stricter: neither passes");
        assert_eq!(gate(cands_dummy_for_default(), 10).len(), gate_at(cands_dummy_for_default(), 10, Z_MIN).len(), "gate == gate_at(Z_MIN)");
    }

    fn cands_dummy_for_default() -> Vec<Candidate> {
        vec![cand(1, Modality::Visual, 0.10, Some(4.2)), cand(2, Modality::Visual, 0.10, Some(3.8))]
    }

    #[test]
    fn small_library_uses_adj_only() {
        assert_eq!(z_of(0.1, 5, 0.0, 0.01), None);
        assert!(cand(1, Modality::Visual, 0.08, None).passes());
        assert!(!cand(1, Modality::Visual, 0.06, None).passes());
        assert_eq!(z_of(0.1, 100, 0.0, 0.0), None);
        assert!((z_of(0.1, 100, 0.0, 0.02).unwrap() - 5.0).abs() < 1e-4);
    }

    #[test]
    fn hub_is_the_mean() {
        let h = hub_vector(&[vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert_eq!(h, vec![0.5, 0.5]);
        assert!(hub_vector(&[]).is_empty());
        assert_eq!(mean_sd(&[1.0, 3.0]), (2.0, 1.0));
    }
}
