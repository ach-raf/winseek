//! Vector provenance in the app: stamp the index with the model / llama.cpp build that made it, keep
//! reference vectors of fixed probe inputs, and, when a llama.cpp bump changes only the build,
//! decide from those probes whether the stored vectors still match (keep the index) or moved
//! (re-embed). The pure parts live in `winseek_store::provenance` ([`winseek_store::verdict`]).

use std::sync::Mutex;

use winseek_llama::prep::prepare_jpeg;
use winseek_store::{verdict, Store, Verdict};

use super::embedder::{EmbedError, Embedder, PARITY_STRINGS};

/// What [`check`] found and did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    /// Stamp is current and the baseline exists: nothing to do.
    Current,
    /// The index had no reference vectors; they were stored now (made by the current build).
    Baselined,
    /// New llama.cpp build, probes still match (worst cosine): vectors kept, stamp updated.
    Kept(f32),
    /// The vectors cannot be trusted under the new build (probes moved, or nothing to compare
    /// with): the caller re-embeds the index.
    Rebuild(Option<f32>),
}

/// A deterministic probe picture (smooth colour field plus a hard edge) as the JPEG the index
/// server would get for a real image.
fn probe_jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_fn(96, 96, |x, y| {
        let edge = if x > 48 { 255u8 } else { 0 };
        image::Rgb([(x * 2) as u8, (y * 2) as u8, edge])
    });
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).expect("encode probe");
    prepare_jpeg(&png).expect("probe is a valid image")
}

/// Embed the probe set: the parity strings as documents, then the probe picture.
pub fn probe_vectors(embedder: &Mutex<Box<dyn Embedder + Send>>) -> Result<Vec<Vec<f32>>, EmbedError> {
    let docs: Vec<(&str, &str)> = PARITY_STRINGS.iter().map(|s| ("", *s)).collect();
    let mut e = embedder.lock().unwrap_or_else(|p| p.into_inner());
    let mut out = e.embed_documents(&docs)?;
    out.extend(e.embed_images(&[probe_jpeg()])?);
    Ok(out)
}

/// Compare the index's stamp with the running embedder's and act on it (see [`Outcome`]). `Err`
/// means the probes could not be embedded right now (retry later); nothing was changed.
pub fn check(store: &Mutex<Store>, embedder: &Mutex<Box<dyn Embedder + Send>>) -> Result<Outcome, EmbedError> {
    let current = embedder.lock().unwrap_or_else(|p| p.into_inner()).meta();
    let lock = || store.lock().unwrap_or_else(|p| p.into_inner());
    let (stamp, (_, stored)) = {
        let s = lock();
        (s.stamp().clone(), s.ref_vectors().map_err(|e| EmbedError::Other(e.to_string()))?)
    };
    if stamp == current {
        if !stored.is_empty() {
            return Ok(Outcome::Current);
        }
        let fresh = probe_vectors(embedder)?;
        lock().set_ref_vectors(&current.llama_build, &fresh).map_err(|e| EmbedError::Other(e.to_string()))?;
        return Ok(Outcome::Baselined);
    }
    if !stamp.same_but_build(&current) || stored.is_empty() {
        return Ok(Outcome::Rebuild(None));
    }
    let fresh = probe_vectors(embedder)?;
    match verdict(&stored, &fresh) {
        Verdict::Keep(c) => {
            lock().restamp_build(&current).map_err(|e| EmbedError::Other(e.to_string()))?;
            Ok(Outcome::Kept(c))
        }
        Verdict::Moved(c) => Ok(Outcome::Rebuild(Some(c))),
        Verdict::NoBaseline => Ok(Outcome::Rebuild(None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier1::embedder::{index_meta, DIMS};
    use winseek_store::{FileKey, Store};

    /// Vectors are a function of the input text/bytes and a `shift` (what a "new build" does to them).
    struct Fake {
        build: &'static str,
        shift: f32,
        fail: bool,
    }

    fn vec_for(seed: usize, shift: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; DIMS];
        v[seed % DIMS] = 1.0;
        v[(seed + 1) % DIMS] = shift;
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    impl Embedder for Fake {
        fn embed_images(&mut self, j: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            if self.fail {
                return Err(EmbedError::Other("GPU is busy with the Parser model".into()));
            }
            Ok(j.iter().map(|_| vec_for(100, self.shift)).collect())
        }
        fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(vec_for(0, 0.0))
        }
        fn embed_documents(&mut self, docs: &[(&str, &str)]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Ok(docs.iter().enumerate().map(|(i, _)| vec_for(i * 10, self.shift)).collect())
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            index_meta(self.build)
        }
    }

    fn emb(build: &'static str, shift: f32) -> Mutex<Box<dyn Embedder + Send>> {
        Mutex::new(Box::new(Fake { build, shift, fail: false }))
    }

    fn store_with_vectors(dir: &std::path::Path, build: &str) -> Mutex<Store> {
        let mut s = Store::open(dir.join("s.db"), &index_meta(build)).unwrap();
        let id = s.upsert_file(&FileKey { vol_guid: "v".into(), frn: 1 }, r"C:\p\a.txt", 1, 1, "doc").unwrap();
        let v = winseek_store::ChunkVec { chunk: 0, kind: winseek_store::VecKind::Text, locator: None, vec: vec_for(7, 0.0) };
        s.complete(id, "h", &[v], &[(0, "hello".into())]).unwrap();
        Mutex::new(s)
    }

    #[test]
    fn a_fresh_index_gets_a_baseline_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_vectors(dir.path(), "b1");
        let e = emb("b1", 0.0);
        assert_eq!(check(&store, &e).unwrap(), Outcome::Baselined);
        assert_eq!(check(&store, &e).unwrap(), Outcome::Current);
        let (build, refs) = store.lock().unwrap().ref_vectors().unwrap();
        assert_eq!((build.as_deref(), refs.len()), (Some("b1"), PARITY_STRINGS.len() + 1));
    }

    #[test]
    fn a_build_bump_that_does_not_move_vectors_keeps_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_vectors(dir.path(), "b1");
        check(&store, &emb("b1", 0.0)).unwrap();
        // The new build embeds the probes (almost) identically.
        let out = check(&store, &emb("b2", 0.0001)).unwrap();
        assert!(matches!(out, Outcome::Kept(c) if c > 0.9999), "{out:?}");
        let s = store.lock().unwrap();
        assert_eq!(s.stamp().llama_build, "b2");
        assert_eq!(s.stats().unwrap().done, 1, "the file and its vector are still there");
        assert_eq!(s.search_vectors(&vec_for(7, 0.0), 3, None).unwrap().len(), 1);
        drop(s);
        assert_eq!(check(&store, &emb("b2", 0.0001)).unwrap(), Outcome::Current, "restamped: no second check");
    }

    #[test]
    fn a_build_bump_that_moves_vectors_asks_for_a_rebuild_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_vectors(dir.path(), "b1");
        check(&store, &emb("b1", 0.0)).unwrap();
        let out = check(&store, &emb("b2", 0.5)).unwrap();
        assert!(matches!(out, Outcome::Rebuild(Some(c)) if c < 0.999), "{out:?}");
        assert_eq!(store.lock().unwrap().stamp().llama_build, "b1", "the caller resets; check itself keeps the stamp");
    }

    #[test]
    fn an_old_index_without_a_baseline_cannot_be_trusted_across_builds() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_vectors(dir.path(), "b1");
        assert_eq!(check(&store, &emb("b2", 0.0)).unwrap(), Outcome::Rebuild(None));
    }

    #[test]
    fn a_model_change_is_never_a_probe_question() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_vectors(dir.path(), "b1");
        check(&store, &emb("b1", 0.0)).unwrap();
        struct Other;
        impl Embedder for Other {
            fn embed_images(&mut self, _: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
                unreachable!("no probing when the model differs")
            }
            fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
                unreachable!()
            }
            fn meta(&self) -> winseek_store::IndexMeta {
                winseek_store::IndexMeta { model_id: "other-model".into(), ..index_meta("b1") }
            }
        }
        let e: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Other));
        assert_eq!(check(&store, &e).unwrap(), Outcome::Rebuild(None));
    }

    #[test]
    fn probes_that_cannot_be_embedded_now_are_retried_not_decided() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_vectors(dir.path(), "b1");
        check(&store, &emb("b1", 0.0)).unwrap();
        let busy: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Fake { build: "b2", shift: 0.0, fail: true }));
        assert!(check(&store, &busy).is_err());
        assert_eq!(store.lock().unwrap().stamp().llama_build, "b1", "nothing changed");
    }

    #[test]
    fn the_probe_picture_is_a_stable_jpeg() {
        let a = probe_jpeg();
        assert!(a.starts_with(&[0xff, 0xd8, 0xff]));
        assert_eq!(a, probe_jpeg());
    }
}
