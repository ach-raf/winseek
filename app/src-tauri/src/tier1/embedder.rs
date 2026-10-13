//! The embedding backend behind Tier 1: a trait, plus the real implementation that lazily
//! spawns `llama-server` (EmbeddingGemma-2 + mmproj) and unloads it after 2 minutes idle.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use winseek_llama::install::resolve_exe;
use winseek_llama::manifest::{ModelEntry, ModelRole};
use winseek_llama::{
    detect_backend, fallback_chain, fallback_order, Backend, EmbedClient, IdleReaper, LlamaServer, Manifest, ModelProfile,
};
use winseek_store::IndexMeta;

pub const MODEL_ID: &str = "embeddinggemma-2";
pub const QUANT: &str = "Q8_0";
pub const IMAGE_BUDGET: u32 = 280;
pub const DIMS: usize = 768;
pub const IDLE_UNLOAD: Duration = Duration::from_secs(120);

#[derive(Debug)]
pub enum EmbedError {
    /// llama.cpp build or model files are not on disk (nothing to retry until they are).
    ModelsMissing,
    Other(String),
}

impl fmt::Display for EmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmbedError::ModelsMissing => write!(f, "models missing"),
            EmbedError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EmbedError {}

pub type Result<T> = std::result::Result<T, EmbedError>;

pub trait Embedder {
    /// One 768-d L2-normalised vector per JPEG, in order.
    fn embed_images(&mut self, jpegs: &[Vec<u8>]) -> Result<Vec<Vec<f32>>>;
    /// One 768-d L2-normalised query vector.
    fn embed_query(&mut self, q: &str) -> Result<Vec<f32>>;
    fn meta(&self) -> IndexMeta;
    /// Cheap check that embedding can be attempted (files on disk). Default: always.
    fn models_present(&self) -> bool {
        true
    }
    /// Called periodically; the real impl unloads the server when idle.
    fn tick_idle(&mut self) {}
    /// Drop any loaded resources (e.g. after a failure) so the next call starts clean.
    fn reset(&mut self) {}
}

/// Matches the stored meta for a given llama build.
pub fn index_meta(llama_build: &str) -> IndexMeta {
    IndexMeta {
        model_id: MODEL_ID.into(),
        llama_build: llama_build.into(),
        quant: QUANT.into(),
        image_budget: IMAGE_BUDGET,
        dims: DIMS as u32,
    }
}

/// Model files of the default EmbeddingGemma-2 entry: `(model, mmproj)` paths under `models_dir`.
pub fn model_paths(manifest: &Manifest, models_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let entry: &ModelEntry = manifest.model(MODEL_ID, ModelProfile::Default)?;
    let file = |role: ModelRole| entry.files.iter().find(|f| f.role == role).map(|f| models_dir.join(f.file_name()));
    Some((file(ModelRole::Model)?, file(ModelRole::Mmproj)?))
}

struct Running {
    // Field order: the client is dropped before the server process is killed.
    client: EmbedClient,
    server: LlamaServer,
}

pub struct LlamaEmbedder {
    root: PathBuf,
    manifest: Manifest,
    running: Option<Running>,
    reaper: IdleReaper,
    backend: Option<Backend>,
}

impl LlamaEmbedder {
    /// `root` is `%LOCALAPPDATA%\winseek`: llama builds in `root\llama`, models in `root\models`.
    pub fn new(root: PathBuf, manifest: Manifest) -> Self {
        LlamaEmbedder { root, manifest, running: None, reaper: IdleReaper::new(IDLE_UNLOAD), backend: None }
    }

    fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    fn files_present(&self) -> bool {
        match model_paths(&self.manifest, &self.models_dir()) {
            Some((m, p)) => m.is_file() && p.is_file(),
            None => false,
        }
    }

    fn any_exe(&self) -> bool {
        [Backend::Cuda12, Backend::Vulkan, Backend::Cpu]
            .into_iter()
            .any(|b| resolve_exe(&self.root, &self.manifest, b).is_some())
    }

    fn start(&mut self) -> Result<()> {
        if let Some(r) = self.running.as_mut() {
            if r.server.is_running() {
                return Ok(());
            }
            self.running = None; // crashed: respawn
        }
        if !self.files_present() || !self.any_exe() {
            return Err(EmbedError::ModelsMissing);
        }
        let (model, mmproj) = model_paths(&self.manifest, &self.models_dir()).ok_or(EmbedError::ModelsMissing)?;
        let start = *self.backend.get_or_insert_with(|| detect_backend().backend);
        let (root, manifest) = (&self.root, &self.manifest);
        let (_, server) = fallback_chain(&fallback_order(start), |b| {
            let exe = resolve_exe(root, manifest, b)
                .ok_or_else(|| winseek_llama::Error::NotFound(format!("llama-server for {}", b.dir_name())))?;
            LlamaServer::spawn_embedding(&exe, &model, Some(&mmproj), &[])
        })
        .map_err(|e| EmbedError::Other(e.to_string()))?;
        let client = EmbedClient::new(&server.base_url(), server.api_key());
        // Without the mmproj the server would embed "images" as nothing useful: refuse.
        match client.vision_enabled() {
            Ok(true) => {}
            Ok(false) => return Err(EmbedError::Other("llama-server started without vision (mmproj not loaded)".into())),
            Err(e) => return Err(EmbedError::Other(format!("/props: {e}"))),
        }
        self.running = Some(Running { client, server });
        Ok(())
    }

    fn client(&mut self) -> Result<&EmbedClient> {
        self.start()?;
        self.reaper.touch();
        Ok(&self.running.as_ref().expect("started").client)
    }
}

fn other(e: winseek_llama::Error) -> EmbedError {
    EmbedError::Other(e.to_string())
}

fn finish(raw: Vec<Vec<f32>>) -> Result<Vec<Vec<f32>>> {
    raw.iter().map(|v| winseek_llama::embed::truncate_normalize(v, DIMS).map_err(other)).collect()
}

impl Embedder for LlamaEmbedder {
    fn embed_images(&mut self, jpegs: &[Vec<u8>]) -> Result<Vec<Vec<f32>>> {
        let refs: Vec<&[u8]> = jpegs.iter().map(Vec::as_slice).collect();
        let raw = self.client()?.embed_images_jpeg(&refs).map_err(other)?;
        finish(raw)
    }

    fn embed_query(&mut self, q: &str) -> Result<Vec<f32>> {
        let raw = self.client()?.embed_queries(&[q]).map_err(other)?;
        finish(raw)?.into_iter().next().ok_or_else(|| EmbedError::Other("empty embedding response".into()))
    }

    fn meta(&self) -> IndexMeta {
        index_meta(&self.manifest.llama.build)
    }

    fn models_present(&self) -> bool {
        self.files_present() && self.any_exe()
    }

    fn tick_idle(&mut self) {
        if self.running.is_some() && self.reaper.should_reap() {
            self.running = None; // drop kills llama-server, freeing VRAM
        }
    }

    fn reset(&mut self) {
        self.running = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_matches_spec() {
        let e = LlamaEmbedder::new(PathBuf::from(r"C:\nonexistent-winseek-root"), Manifest::embedded());
        let m = e.meta();
        assert_eq!(m.model_id, "embeddinggemma-2");
        assert_eq!(m.quant, "Q8_0");
        assert_eq!((m.image_budget, m.dims), (280, 768));
        assert_eq!(m.llama_build, Manifest::embedded().llama.build);
    }

    #[test]
    fn missing_files_report_models_missing_without_spawning() {
        let mut e = LlamaEmbedder::new(PathBuf::from(r"C:\nonexistent-winseek-root"), Manifest::embedded());
        assert!(!e.models_present());
        assert!(matches!(e.embed_images(&[vec![1, 2, 3]]), Err(EmbedError::ModelsMissing)));
        assert!(matches!(e.embed_query("cat"), Err(EmbedError::ModelsMissing)));
    }

    #[test]
    fn model_paths_use_manifest_file_names() {
        let (m, p) = model_paths(&Manifest::embedded(), Path::new(r"C:\m")).unwrap();
        assert_eq!(m, Path::new(r"C:\m\embeddinggemma-2-Q8_0.gguf"));
        assert_eq!(p, Path::new(r"C:\m\mmproj-embeddinggemma-2-Q8_0.gguf"));
    }

    #[test]
    fn present_files_without_llama_exe_still_missing() {
        let d = tempfile::tempdir().unwrap();
        let man = Manifest::embedded();
        let models = d.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        let (m, p) = model_paths(&man, &models).unwrap();
        std::fs::write(m, b"x").unwrap();
        std::fs::write(p, b"x").unwrap();
        let e = LlamaEmbedder::new(d.path().to_path_buf(), man);
        assert!(!e.models_present()); // no llama build installed
    }
}
