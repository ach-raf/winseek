//! Calibrated relevance for search by meaning.
//!
//! Raw cosines are not comparable between queries or between modalities: with EmbeddingGemma-2
//! every image sits at 0.46..0.76 against any query, and some vectors (app icons, tiny OCR
//! fragments, files that hold only a hash) are close to *every* query ("hubs"). A fixed cosine
//! threshold therefore lets junk through, and the old top-k showed whatever was least unrelated.
//!
//! This follows DigUp's "meaning" scale (`Searcher.swift`: `meaning` / `modalityMeans`), at row
//! level, and keeps our hub correction:
//! 1. Hub correction: `s = cos(q, v) - cos(hub, v)` for EVERY vector row, where `hub` is the mean
//!    embedding of [`HUB_PROMPTS`] (generic queries). A vector that matches everything loses its
//!    head start (measured: it keeps vocal stems / WhatsApp videos out of "invoice", "email").
//! 2. Per-modality centring: subtract the mean of `s` over the rows of the row's modality (visual
//!    = image + page, text, audio); a modality with fewer than [`MIN_MODALITY_ROWS`] rows borrows
//!    the mean over all rows. This removes the "images sit high, audio sits low" offset.
//! 3. ONE deviation across all modalities: `z = (centred - mean(centred)) / max(sd, SD_FLOOR)`
//!    over every row. A file content's z is that of its best row.
//! 4. Gate: `z >= Z_MIN`. Nothing passes -> nothing is shown.
//!
//! Why not a deviation per modality (the previous scale, Z_MIN 4.0 + a cosine floor): a library
//! of all music has a wide spread, so the best song only reached z~2.7 and audio never passed.
//! Measured on the real index (2026-11-14: 14,743 visual / 4,263 text / 1,221 audio files, 12,901
//! audio rows): the old per-modality gate passed 0 audio files for "music about addiction", "song
//! about drugs", "love song", "rap music" and "piano". The new scale passes them with the right
//! songs on top (Lana Del Rey - Yayo, Eminem - I'm Having A Relapse, Sick Puppies - Riptide for
//! "song about drugs"; Constitution of Morocco.wav at the top for "someone talking about the
//! constitution"), while "invoice" gets 0 audio at z >= 3.
//!
//! Earlier measurement of the hub + per-modality idea (1,453 images + 2,630 texts, 20 hand-labelled
//! queries): precision of the shown meaning hits 0.19 -> 0.69, relevant hits shown 31 -> 59;
//! "email" went from 3/8 (LocalSend / Instagram screenshots) to 7/8 (Gmail icons, email templates).

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

/// Minimum standing above the library for this query, in standard deviations of the centred row
/// scores (DigUp's `zGate`).
pub const Z_MIN: f32 = 2.0;
/// Below this many vector rows in total the z-score is not trusted; only `TINY_ADJ_MIN` applies.
pub const MIN_POPULATION: usize = 30;
/// A modality with fewer rows than this is centred on the mean of all rows instead of its own.
pub const MIN_MODALITY_ROWS: usize = 20;
/// Floor of the deviation, so a library of near-identical scores does not blow z up.
pub const SD_FLOOR: f64 = 0.02;
/// Hub-corrected cosine floor used only when there is no z (a tiny library, fewer than
/// [`MIN_POPULATION`] rows): with no population to stand out from, a plain floor is all there is.
const TINY_ADJ_MIN: f32 = 0.07;

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
    /// Standing of `adj` on the meaning scale (see the module doc); `None` = fewer than
    /// [`MIN_POPULATION`] rows in the library.
    pub z: Option<f32>,
}

impl Candidate {
    pub fn passes(&self) -> bool {
        self.passes_at(Z_MIN)
    }
    /// Same gate with a caller-chosen strictness (collections dial it with their slider).
    pub fn passes_at(&self, z_min: f32) -> bool {
        match self.z {
            Some(z) => z >= z_min,
            None => self.adj >= TINY_ADJ_MIN,
        }
    }
    /// Sort key: z when known, else a z-like scale of `adj`.
    pub fn rank(&self) -> f32 {
        self.z.unwrap_or(self.adj / TINY_ADJ_MIN * Z_MIN)
    }
}

impl Modality {
    fn idx(self) -> usize {
        match self {
            Modality::Visual => 0,
            Modality::Text => 1,
            Modality::Audio => 2,
        }
    }
}

/// Running sums of one query's row scores `s`, per modality. Accumulated in the single pass over
/// the vectors; f64 because the variance is a difference of large sums.
#[derive(Debug, Clone, Default)]
pub struct RowStats {
    n: [usize; 3],
    sum: [f64; 3],
    sumsq: [f64; 3],
}

impl RowStats {
    pub fn add(&mut self, m: Modality, s: f32) {
        let (i, s) = (m.idx(), s as f64);
        self.n[i] += 1;
        self.sum[i] += s;
        self.sumsq[i] += s * s;
    }

    pub fn rows(&self) -> usize {
        self.n.iter().sum()
    }

    /// The scale for this query, or `None` when there are fewer than [`MIN_POPULATION`] rows.
    ///
    /// With `u_m` the centring mean of modality m (its own mean, or the all-row mean `A` when it
    /// has fewer than [`MIN_MODALITY_ROWS`] rows), centred rows are `s - u_m`, so over the
    /// N rows: `mu_c = sum_m (sum_m - n_m u_m) / N` and
    /// `var_c = sum_m (sumsq_m - 2 u_m sum_m + n_m u_m^2) / N - mu_c^2`
    /// (for a modality centred on its own mean the first term is 0, otherwise it is the offset
    /// `mean_m - A`; the second is its variance plus that offset squared).
    pub fn scale(&self) -> Option<Scale> {
        let total = self.rows();
        if total < MIN_POPULATION {
            return None;
        }
        let nf = total as f64;
        let all_mean = self.sum.iter().sum::<f64>() / nf;
        let mut centre = [all_mean; 3];
        let (mut sum_c, mut sumsq_c) = (0.0, 0.0);
        for i in 0..3 {
            if self.n[i] == 0 {
                continue;
            }
            let own = self.sum[i] / self.n[i] as f64;
            let u = if self.n[i] >= MIN_MODALITY_ROWS { own } else { all_mean };
            centre[i] = u;
            sum_c += self.sum[i] - self.n[i] as f64 * u;
            sumsq_c += self.sumsq[i] - 2.0 * u * self.sum[i] + self.n[i] as f64 * u * u;
        }
        let mu_c = sum_c / nf;
        let var = (sumsq_c / nf - mu_c * mu_c).max(0.0);
        Some(Scale { centre, mu_c, sd: var.sqrt().max(SD_FLOOR) })
    }
}

/// Maps a hub-corrected row score to z for one query.
#[derive(Debug, Clone, Copy)]
pub struct Scale {
    centre: [f64; 3],
    mu_c: f64,
    sd: f64,
}

impl Scale {
    pub fn z(&self, m: Modality, s: f32) -> f32 {
        ((s as f64 - self.centre[m.idx()] - self.mu_c) / self.sd) as f32
    }
}

/// Words of the query that name a kind of file (DigUp `kindHints`): a soft boost for that
/// modality, never a filter.
pub fn kind_hints(q: &str) -> Vec<Modality> {
    const AUDIO: [&str; 10] = ["audio", "song", "songs", "music", "track", "podcast", "voice", "sound", "recording", "recordings"];
    const VISUAL: [&str; 8] = ["photo", "photos", "picture", "pictures", "image", "images", "screenshot", "screenshots"];
    const TEXT: [&str; 8] = ["document", "documents", "note", "notes", "pdf", "pdfs", "paper", "papers"];
    let lower = q.to_lowercase();
    let mut out = Vec::new();
    for w in lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        for (m, words) in [(Modality::Audio, &AUDIO[..]), (Modality::Visual, &VISUAL[..]), (Modality::Text, &TEXT[..])] {
            if words.contains(&w) && !out.contains(&m) {
                out.push(m);
            }
        }
    }
    out
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
        assert_eq!((Z_MIN, MIN_POPULATION, MIN_MODALITY_ROWS, SD_FLOOR), (2.0, 30, 20, 0.02));
        assert_eq!(HUB_PROMPTS.len(), 48);
    }

    #[test]
    fn gate_drops_weak_and_ordinary_hits() {
        let out = gate(
            vec![
                cand(1, Modality::Visual, 0.10, Some(3.0)), // passes
                cand(2, Modality::Visual, 0.30, Some(1.5)), // not unusual enough, whatever its adj
                cand(3, Modality::Text, 0.12, Some(1.9)),   // just below the gate
                cand(4, Modality::Text, 0.01, Some(2.5)),   // passes: no cosine floor any more
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
                cand(1, Modality::Visual, 0.09, Some(2.2)),
                cand(1, Modality::Text, 0.11, Some(4.0)),
                cand(2, Modality::Visual, 0.10, Some(3.0)),
                cand(3, Modality::Visual, 0.10, Some(2.1)),
            ],
            2,
        );
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].file_id.0, out[0].modality), (1, Modality::Text));
        assert_eq!(out[1].file_id.0, 2);
    }

    #[test]
    fn gate_at_dials_strictness() {
        let cands = vec![cand(1, Modality::Visual, 0.10, Some(2.2)), cand(2, Modality::Visual, 0.10, Some(1.8))];
        assert_eq!(gate_at(cands.clone(), 10, 2.0).len(), 1, "default strictness: only the 2.2 passes");
        assert_eq!(gate_at(cands.clone(), 10, 1.5).len(), 2, "looser: both pass");
        assert!(gate_at(cands.clone(), 10, 3.0).is_empty(), "stricter: neither passes");
        assert_eq!(gate(cands.clone(), 10).len(), gate_at(cands, 10, Z_MIN).len(), "gate == gate_at(Z_MIN)");
    }

    #[test]
    fn tiny_library_uses_adj_only() {
        assert!(cand(1, Modality::Visual, 0.08, None).passes());
        assert!(!cand(1, Modality::Visual, 0.06, None).passes());
        assert!((cand(1, Modality::Visual, 0.07, None).rank() - Z_MIN).abs() < 1e-5);
        let mut st = RowStats::default();
        for i in 0..(MIN_POPULATION - 1) {
            st.add(Modality::Visual, i as f32 * 0.01);
        }
        assert!(st.scale().is_none(), "fewer than MIN_POPULATION rows: no z");
        st.add(Modality::Visual, 0.5);
        assert!(st.scale().is_some());
    }

    #[test]
    fn hub_is_the_mean() {
        let h = hub_vector(&[vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert_eq!(h, vec![0.5, 0.5]);
        assert!(hub_vector(&[]).is_empty());
    }

    /// Deterministic pseudo-random scores in [lo, hi).
    fn scores(seed: &mut u64, n: usize, lo: f32, hi: f32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                lo + ((*seed >> 40) as f32 / (1u64 << 24) as f32) * (hi - lo)
            })
            .collect()
    }

    /// DigUp's definition computed the slow way: materialise every centred row.
    fn brute_z(rows: &[(Modality, f32)], m: Modality, s: f32) -> f32 {
        let all: f64 = rows.iter().map(|r| r.1 as f64).sum::<f64>() / rows.len() as f64;
        let mean_of = |k: Modality| {
            let xs: Vec<f64> = rows.iter().filter(|r| r.0 == k).map(|r| r.1 as f64).collect();
            if xs.len() >= MIN_MODALITY_ROWS { xs.iter().sum::<f64>() / xs.len() as f64 } else { all }
        };
        let centred: Vec<f64> = rows.iter().map(|r| r.1 as f64 - mean_of(r.0)).collect();
        let mu = centred.iter().sum::<f64>() / centred.len() as f64;
        let var = centred.iter().map(|c| (c - mu) * (c - mu)).sum::<f64>() / centred.len() as f64;
        ((s as f64 - mean_of(m) - mu) / var.sqrt().max(SD_FLOOR)) as f32
    }

    #[test]
    fn analytic_z_equals_brute_force() {
        let mut seed = 7u64;
        // big visual, mid text (wide), tiny audio (< MIN_MODALITY_ROWS: borrows the all-row mean)
        let mut rows = Vec::new();
        rows.extend(scores(&mut seed, 200, 0.10, 0.20).into_iter().map(|s| (Modality::Visual, s)));
        rows.extend(scores(&mut seed, 60, -0.20, 0.30).into_iter().map(|s| (Modality::Text, s)));
        rows.extend(scores(&mut seed, 7, 0.20, 0.60).into_iter().map(|s| (Modality::Audio, s)));
        for case in [rows.clone(), rows.iter().filter(|r| r.0 != Modality::Audio).cloned().collect::<Vec<_>>()] {
            let mut st = RowStats::default();
            case.iter().for_each(|r| st.add(r.0, r.1));
            let sc = st.scale().unwrap();
            for m in [Modality::Visual, Modality::Text, Modality::Audio] {
                for s in [-0.3f32, 0.0, 0.15, 0.5] {
                    let (a, b) = (sc.z(m, s), brute_z(&case, m, s));
                    assert!((a - b).abs() < 1e-3, "{m:?} {s}: analytic {a} vs brute {b}");
                }
            }
        }
    }

    #[test]
    fn wide_spread_audio_still_passes_next_to_a_narrow_visual_library() {
        // The regression: music has a wide spread, pictures a narrow one. A per-modality
        // deviation put the best song at z~2.7 (< the old 4.0); one deviation across modalities
        // does not punish audio for being varied.
        let mut seed = 99u64;
        let mut st = RowStats::default();
        for s in scores(&mut seed, 2000, 0.00, 0.04) {
            st.add(Modality::Visual, s);
        }
        let audio = scores(&mut seed, 400, -0.15, 0.15);
        for s in &audio {
            st.add(Modality::Audio, *s);
        }
        let sc = st.scale().unwrap();
        let best = audio.iter().cloned().fold(f32::MIN, f32::max);
        let z_best = sc.z(Modality::Audio, best);
        assert!(z_best >= Z_MIN, "best song z = {z_best}");
        let typical = sc.z(Modality::Audio, 0.0);
        assert!(typical < Z_MIN, "an average song does not pass: {typical}");
        let best_visual = sc.z(Modality::Visual, 0.04);
        assert!(best_visual < Z_MIN, "a narrow visual library has no outlier: {best_visual}");
    }

    #[test]
    fn kind_words_hint_their_modality() {
        assert_eq!(kind_hints("music about addiction"), vec![Modality::Audio]);
        assert_eq!(kind_hints("Photos of my SONG-writing notes"), vec![Modality::Visual, Modality::Audio, Modality::Text]);
        assert_eq!(kind_hints("a podcast, voice recordings"), vec![Modality::Audio]);
        assert_eq!(kind_hints("screenshot of a paper"), vec![Modality::Visual, Modality::Text]);
        assert!(kind_hints("invoice").is_empty());
        assert!(kind_hints("musical").is_empty(), "whole words only");
    }
}
