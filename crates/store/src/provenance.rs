//! Vector provenance: which model / llama.cpp build / quant / image budget made the vectors
//! ([`IndexMeta`], stamped when the index is created), plus a few *reference vectors*: the
//! embeddings of fixed probe inputs, stored when the index was built.
//!
//! A llama.cpp bump changes `IndexMeta::llama_build` but usually not the vectors (same model file,
//! same maths). Wiping a library's worth of embeddings for a different version string is a poor
//! trade, so on a build-only mismatch the app embeds the probes again with the new build and
//! compares them with the stored ones ([`drift`]): the index is kept when they still match and
//! rebuilt only when they moved.

use rusqlite::params;

use crate::{IndexMeta, Result, Store};

/// Probe vectors must stay at least this similar (cosine) to the stored ones for the index to be
/// kept. Same value as the CPU/GPU server parity check: below it, two servers no longer agree.
pub const PROBE_MIN_COS: f32 = 0.999;

impl IndexMeta {
    /// Everything except the llama.cpp build matches: the vectors *should* be the same.
    pub fn same_but_build(&self, other: &IndexMeta) -> bool {
        self.model_id == other.model_id && self.quant == other.quant && self.image_budget == other.image_budget && self.dims == other.dims
    }
}

/// Worst (lowest) cosine between matching probe vectors; `None` when they cannot be compared
/// (different counts or sizes, nothing stored).
pub fn drift(stored: &[Vec<f32>], fresh: &[Vec<f32>]) -> Option<f32> {
    if stored.is_empty() || stored.len() != fresh.len() {
        return None;
    }
    let mut worst = f32::INFINITY;
    for (a, b) in stored.iter().zip(fresh) {
        if a.len() != b.len() || a.is_empty() {
            return None;
        }
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let (na, nb) = (a.iter().map(|x| x * x).sum::<f32>().sqrt(), b.iter().map(|x| x * x).sum::<f32>().sqrt());
        if na < 1e-9 || nb < 1e-9 {
            return None;
        }
        worst = worst.min(dot / (na * nb));
    }
    Some(worst)
}

/// The verdict of a probe comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// Probes match: keep the vectors, re-stamp the build.
    Keep(f32),
    /// Probes moved: the vectors are in another space, re-embed.
    Moved(f32),
    /// Nothing to compare with (index older than the probes): treat as moved.
    NoBaseline,
}

pub fn verdict(stored: &[Vec<f32>], fresh: &[Vec<f32>]) -> Verdict {
    match drift(stored, fresh) {
        Some(c) if c >= PROBE_MIN_COS => Verdict::Keep(c),
        Some(c) => Verdict::Moved(c),
        None => Verdict::NoBaseline,
    }
}

impl Store {
    /// The stamp this index was built with (what [`Store::open`] checks against).
    pub fn stamp(&self) -> &IndexMeta {
        &self.meta
    }

    /// Store the reference vectors (replacing any), with the build that made them.
    pub fn set_ref_vectors(&mut self, build: &str, vecs: &[Vec<f32>]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM ref_vectors", [])?;
        for (i, v) in vecs.iter().enumerate() {
            let blob: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
            tx.execute("INSERT INTO ref_vectors(idx, build, vec) VALUES (?1, ?2, ?3)", params![i as i64, build, blob])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The reference vectors in probe order (empty when none were stored) and the build that made them.
    pub fn ref_vectors(&self) -> Result<(Option<String>, Vec<Vec<f32>>)> {
        let mut st = self.conn.prepare_cached("SELECT build, vec FROM ref_vectors ORDER BY idx")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        let mut build = None;
        let mut out = Vec::new();
        for r in rows {
            let (b, blob) = r?;
            build.get_or_insert(b);
            out.push(blob.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect());
        }
        Ok((build, out))
    }

    /// Keep every vector and stamp the index with `meta` (a build-only change whose probes matched).
    /// Refuses anything but a build change: other fields mean the vectors really are different.
    pub fn restamp_build(&mut self, meta: &IndexMeta) -> Result<bool> {
        if !self.meta.same_but_build(meta) {
            return Ok(false);
        }
        self.conn.execute("UPDATE index_meta SET llama_build = ?1 WHERE id = 1", [&meta.llama_build])?;
        self.meta = meta.clone();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(build: &str) -> IndexMeta {
        IndexMeta { model_id: "m".into(), llama_build: build.into(), quant: "q".into(), image_budget: 280, dims: 4 }
    }

    #[test]
    fn drift_is_the_worst_cosine_and_needs_comparable_input() {
        let a = vec![vec![1., 0., 0., 0.], vec![0., 1., 0., 0.]];
        assert_eq!(drift(&a, &a), Some(1.0));
        let b = vec![vec![1., 0., 0., 0.], vec![0., 0.6, 0.8, 0.]];
        assert!((drift(&a, &b).unwrap() - 0.6).abs() < 1e-6);
        assert_eq!(drift(&[], &[]), None);
        assert_eq!(drift(&a, &a[..1]), None);
        assert_eq!(drift(&a, &[vec![1., 0.], vec![0., 1.]]), None);
        assert_eq!(drift(&[vec![0.; 4]], &[vec![0.; 4]]), None);
    }

    #[test]
    fn verdict_keeps_tiny_noise_and_flags_real_movement() {
        let a = vec![vec![1., 0., 0., 0.]];
        assert!(matches!(verdict(&a, &[vec![1., 0.0001, 0., 0.]]), Verdict::Keep(c) if c > 0.9999));
        assert!(matches!(verdict(&a, &[vec![0.95, 0.31, 0., 0.]]), Verdict::Moved(_)));
        assert_eq!(verdict(&[], &a), Verdict::NoBaseline);
    }

    #[test]
    fn restamp_only_changes_the_build_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        {
            let mut s = Store::open(&p, &meta("b1")).unwrap();
            assert_eq!(s.ref_vectors().unwrap(), (None, vec![]));
            s.set_ref_vectors("b1", &[vec![1., 0., 0., 0.], vec![0., 1., 0., 0.]]).unwrap();
            assert!(s.restamp_build(&meta("b2")).unwrap());
            assert_eq!(s.stamp().llama_build, "b2");
            let other_model = IndexMeta { model_id: "other".into(), ..meta("b3") };
            assert!(!s.restamp_build(&other_model).unwrap(), "a different model is never a restamp");
        }
        assert!(matches!(Store::open(&p, &meta("b1")), Err(crate::Error::MetaMismatch { .. })), "old stamp no longer opens");
        let s = Store::open(&p, &meta("b2")).unwrap();
        let (build, v) = s.ref_vectors().unwrap();
        assert_eq!((build.as_deref(), v.len()), (Some("b1"), 2), "the baseline keeps the build that made it");
        assert!(meta("b1").same_but_build(&meta("b9")));
    }
}
