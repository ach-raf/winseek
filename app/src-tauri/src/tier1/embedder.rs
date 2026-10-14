//! The embedding backend behind Tier 1: a trait, plus the real implementation that runs two
//! `llama-server`s (PLAN "VRAM budget"):
//!
//! - the *query* server: text-only, CPU, spawned on the first text embedding and resident while
//!   the app runs. Searching and text-only indexing never touch the GPU;
//! - the *index* server: GPU, with the mmproj, spawned only for image work, unloaded after 2
//!   minutes idle and immediately when the pause policy pauses.
//!
//! The split relies on both servers producing the same text vectors; [`text_parity`] checks that
//! once per session and routes all text to the index server if they drift.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use winseek_llama::install::resolve_exe;
use winseek_llama::manifest::{ModelEntry, ModelRole};
use winseek_llama::{
    detect_backend, fallback_chain, fallback_order, Backend, EmbedClient, IdleReaper, LlamaServer, Manifest, ModelProfile, Profile,
    Role, SpawnConfig,
};

use super::gpu_phase::{Gpu, GpuPhase};
use winseek_store::IndexMeta;

pub const MODEL_ID: &str = "embeddinggemma-2";
pub const QUANT: &str = "Q8_0";
pub const IMAGE_BUDGET: u32 = 280;
pub const DIMS: usize = 768;
pub const IDLE_UNLOAD: Duration = Duration::from_secs(120);
/// Start of the error `start_index` returns while the parser holds the GPU.
const GPU_BUSY: &str = "GPU is busy";

/// Is `msg` the "parser holds the GPU" error (not a strike against the file being embedded)?
pub fn is_gpu_busy(msg: &str) -> bool {
    msg.starts_with(GPU_BUSY)
}

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

/// What the status page shows about the embedding server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub running: bool,
    pub port: Option<u16>,
    /// Seconds until the idle unload frees the VRAM (only while running).
    pub unload_in_s: Option<u64>,
}

pub type Result<T> = std::result::Result<T, EmbedError>;

pub trait Embedder {
    /// One 768-d L2-normalised vector per JPEG, in order.
    fn embed_images(&mut self, jpegs: &[Vec<u8>]) -> Result<Vec<Vec<f32>>>;
    /// One 768-d L2-normalised query vector.
    fn embed_query(&mut self, q: &str) -> Result<Vec<f32>>;
    /// One 768-d vector per document text (e.g. OCR output), in order. Backends without text
    /// embedding keep the default error; callers treat that as "no text vector".
    fn embed_documents(&mut self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(EmbedError::Other("document embedding not supported".into()))
    }
    fn meta(&self) -> IndexMeta;
    /// Cheap check that embedding can be attempted (files on disk). Default: always.
    fn models_present(&self) -> bool {
        true
    }
    /// Called periodically; the real impl unloads the server when idle.
    fn tick_idle(&mut self) {}
    /// Drop any loaded resources (e.g. after a failure) so the next call starts clean.
    fn reset(&mut self) {}
    /// Indexing was paused (battery / fullscreen / user): unload the GPU model, not just the work.
    fn on_pause(&mut self) {}
    /// Server state for the status page. Default: nothing running.
    fn runtime_info(&mut self) -> RuntimeInfo {
        RuntimeInfo::default()
    }
    /// The document parser needs the GPU: let go of the index server (the next image batch reloads it).
    fn release_gpu(&mut self) {}
    /// The performance profile changed: drop the servers and use the new profile from now on.
    fn set_profile(&mut self, _p: Profile) {}
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

/// Where model files live: `<root>\models` (root = `%LOCALAPPDATA%\winseek`).
pub fn models_dir(root: &Path) -> PathBuf {
    root.join("models")
}

struct Running {
    // Field order: the client is dropped before the server process is killed.
    client: EmbedClient,
    server: LlamaServer,
}

impl Running {
    fn alive(&mut self) -> bool {
        self.server.is_running()
    }
}

/// Source of text vectors for [`text_parity`] (raw, untruncated). Implemented by the real client
/// and by fakes in tests.
pub trait TextVectors {
    fn text_vectors(&self, texts: &[&str]) -> winseek_llama::Result<Vec<Vec<f32>>>;
}

impl TextVectors for EmbedClient {
    fn text_vectors(&self, texts: &[&str]) -> winseek_llama::Result<Vec<Vec<f32>>> {
        self.embed_documents(texts)
    }
}

/// Fixed strings for the parity check: plain English, numbers/dates, non-ASCII.
pub const PARITY_STRINGS: [&str; 3] =
    ["a photo of a cat sleeping on a sofa", "Invoice 2026-03-31: total due 1,284.50 EUR", "Zażółć gęślą jaźń, café naïve"];
/// Below this the CPU query server and the GPU index server no longer share one vector space.
pub const PARITY_MIN: f32 = 0.999;

/// Worst (lowest) cosine between the two servers' vectors over [`PARITY_STRINGS`].
pub fn text_parity(query_srv: &dyn TextVectors, index_srv: &dyn TextVectors) -> winseek_llama::Result<f32> {
    let norm = |raw: Vec<Vec<f32>>| -> winseek_llama::Result<Vec<Vec<f32>>> {
        raw.iter().map(|v| winseek_llama::embed::truncate_normalize(v, DIMS)).collect()
    };
    let (q, i) = (norm(query_srv.text_vectors(&PARITY_STRINGS)?)?, norm(index_srv.text_vectors(&PARITY_STRINGS)?)?);
    if q.len() != PARITY_STRINGS.len() || i.len() != PARITY_STRINGS.len() {
        return Err(winseek_llama::Error::Embed("parity: wrong vector count".into()));
    }
    Ok(q.iter().zip(&i).map(|(a, b)| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()).fold(f32::INFINITY, f32::min))
}

/// Which server answers text (queries and documents).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRoute {
    /// The CPU query server (default).
    Query,
    /// The parity check failed: everything goes through the GPU index server.
    Index,
}

/// Session state of the parity check: runs once, after both servers have been up.
#[derive(Debug, Default)]
pub struct ParityGuard {
    done: bool,
    index_only: bool,
}

impl ParityGuard {
    pub fn route(&self) -> TextRoute {
        if self.index_only {
            TextRoute::Index
        } else {
            TextRoute::Query
        }
    }

    /// The check is due when it has not concluded yet and both servers are up.
    pub fn due(&self, both_up: bool) -> bool {
        !self.done && both_up
    }

    /// Record a check result. A low score switches to the index server for the rest of the session
    /// and returns the warning to log; an error (could not compare) leaves the check due.
    pub fn record(&mut self, r: winseek_llama::Result<f32>) -> Option<String> {
        match r {
            Ok(c) if c >= PARITY_MIN => {
                self.done = true;
                None
            }
            Ok(c) => {
                self.done = true;
                self.index_only = true;
                Some(format!("text parity between CPU query server and GPU index server is {c:.5} (< {PARITY_MIN}); routing text to the index server"))
            }
            Err(e) => Some(format!("text parity check failed to run: {e}")),
        }
    }
}

pub struct LlamaEmbedder {
    root: PathBuf,
    manifest: Manifest,
    profile: Profile,
    /// Text-only CPU server; never reaped.
    query: Option<Running>,
    /// GPU server with the mmproj; reaped when idle (not in the Fast profile) and on pause.
    index: Option<Running>,
    reaper: IdleReaper,
    backend: Option<Backend>,
    parity: ParityGuard,
    /// Which GPU model is loaded; shared with whatever spawns the parser so the two are phased.
    gpu: Arc<Mutex<GpuPhase>>,
}

impl LlamaEmbedder {
    /// `root` is `%LOCALAPPDATA%\winseek`: llama builds in `root\llama`, models in `root\models`.
    pub fn new(root: PathBuf, manifest: Manifest) -> Self {
        Self::with_profile(root, manifest, Profile::Balanced)
    }

    pub fn with_profile(root: PathBuf, manifest: Manifest, profile: Profile) -> Self {
        LlamaEmbedder {
            root,
            manifest,
            profile,
            query: None,
            index: None,
            reaper: IdleReaper::new(IDLE_UNLOAD),
            backend: None,
            parity: ParityGuard::default(),
            gpu: Arc::new(Mutex::new(GpuPhase::new(profile))),
        }
    }

    /// The GPU residency tracker, shared with the parser (it must `begin(Gpu::Parser)` first and gets
    /// told to release the index server via [`Embedder::release_gpu`] when that is busy).
    pub fn gpu_phase(&self) -> Arc<Mutex<GpuPhase>> {
        self.gpu.clone()
    }

    fn models_dir(&self) -> PathBuf {
        models_dir(&self.root)
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

    fn spawn_role(&mut self, role: Role) -> Result<Running> {
        if !self.files_present() || !self.any_exe() {
            return Err(EmbedError::ModelsMissing);
        }
        let (model, mmproj) = model_paths(&self.manifest, &self.models_dir()).ok_or(EmbedError::ModelsMissing)?;
        let mmproj = (role == Role::EmbedIndex).then_some(mmproj);
        let start = *self.backend.get_or_insert_with(|| detect_backend().backend);
        let (root, manifest) = (&self.root, &self.manifest);
        let cfg = SpawnConfig { role, profile: self.profile, ..Default::default() };
        let (_, server) = fallback_chain(&fallback_order(start), |b| {
            let exe = resolve_exe(root, manifest, b)
                .ok_or_else(|| winseek_llama::Error::NotFound(format!("llama-server for {}", b.dir_name())))?;
            LlamaServer::spawn_with(&exe, &model, mmproj.as_deref(), &[], &cfg)
        })
        .map_err(|e| EmbedError::Other(e.to_string()))?;
        let client = EmbedClient::new(&server.base_url(), server.api_key());
        if role == Role::EmbedIndex {
            // Without the mmproj the server would embed "images" as nothing useful: refuse.
            match client.vision_enabled() {
                Ok(true) => {}
                Ok(false) => return Err(EmbedError::Other("llama-server started without vision (mmproj not loaded)".into())),
                Err(e) => return Err(EmbedError::Other(format!("/props: {e}"))),
            }
        }
        Ok(Running { client, server })
    }

    fn start_query(&mut self) -> Result<()> {
        if let Some(r) = self.query.as_mut() {
            if r.alive() {
                return Ok(());
            }
            self.query = None; // crashed: respawn
        }
        self.query = Some(self.spawn_role(Role::EmbedQuery)?);
        Ok(())
    }

    fn start_index(&mut self) -> Result<()> {
        if let Some(r) = self.index.as_mut() {
            if r.alive() {
                return Ok(());
            }
            self.index = None; // crashed: respawn
            lock(&self.gpu).end(Gpu::Index);
        }
        if let Some(other) = lock(&self.gpu).begin(Gpu::Index) {
            return Err(EmbedError::Other(format!("{GPU_BUSY} with the {other:?} model; image work waits for it to unload")));
        }
        match self.spawn_role(Role::EmbedIndex) {
            Ok(r) => {
                self.index = Some(r);
                Ok(())
            }
            Err(e) => {
                lock(&self.gpu).end(Gpu::Index);
                Err(e)
            }
        }
    }

    fn drop_index(&mut self) {
        if self.index.take().is_some() {
            lock(&self.gpu).end(Gpu::Index);
        }
    }

    /// Run the parity check once both servers are up (a no-op until then, and after it concluded).
    fn check_parity(&mut self) {
        let both = self.query.is_some() && self.index.is_some();
        if !self.parity.due(both) {
            return;
        }
        let (Some(q), Some(i)) = (&self.query, &self.index) else { return };
        if let Some(w) = self.parity.record(text_parity(&q.client, &i.client)) {
            eprintln!("winseek: {w}");
        }
    }

    /// The client for image work: always the index server.
    fn index_client(&mut self) -> Result<&EmbedClient> {
        self.start_index()?;
        self.reaper.touch();
        self.check_parity();
        Ok(&self.index.as_ref().expect("started").client)
    }

    /// The client for text: the query server unless the parity check routed text to the index
    /// server. If the query server cannot start but the index server is already up, use that.
    fn text_client(&mut self) -> Result<&EmbedClient> {
        if self.parity.route() == TextRoute::Index {
            return self.index_client();
        }
        match self.start_query() {
            Ok(()) => {}
            Err(EmbedError::ModelsMissing) => return Err(EmbedError::ModelsMissing),
            Err(e) if self.index.as_mut().is_some_and(Running::alive) => {
                eprintln!("winseek: query server unavailable ({e}); using the index server");
                return self.index_client();
            }
            Err(e) => return Err(e),
        }
        self.check_parity();
        Ok(&self.query.as_ref().expect("started").client)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
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
        let raw = self.index_client()?.embed_images_jpeg(&refs).map_err(other)?;
        finish(raw)
    }

    fn embed_query(&mut self, q: &str) -> Result<Vec<f32>> {
        let raw = self.text_client()?.embed_queries(&[q]).map_err(other)?;
        finish(raw)?.into_iter().next().ok_or_else(|| EmbedError::Other("empty embedding response".into()))
    }

    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let raw = self.text_client()?.embed_documents(&refs).map_err(other)?;
        finish(raw)
    }

    fn meta(&self) -> IndexMeta {
        index_meta(&self.manifest.llama.build)
    }

    fn models_present(&self) -> bool {
        self.files_present() && self.any_exe()
    }

    fn tick_idle(&mut self) {
        // Fast keeps the index server resident while the app is open (pause still unloads it).
        if self.profile != Profile::Fast && self.index.is_some() && self.reaper.should_reap() {
            self.drop_index(); // drop kills llama-server, freeing VRAM
        }
    }

    fn reset(&mut self) {
        self.drop_index();
        self.query = None;
    }

    fn on_pause(&mut self) {
        self.drop_index();
    }

    fn release_gpu(&mut self) {
        self.drop_index();
    }

    fn set_profile(&mut self, p: Profile) {
        self.drop_index();
        self.query = None;
        self.profile = p;
        *lock(&self.gpu) = GpuPhase::new(p);
    }

    fn runtime_info(&mut self) -> RuntimeInfo {
        // The status page reports the GPU (index) server; the CPU query server costs no VRAM.
        let idle = self.reaper.idle_for();
        let fast = self.profile == Profile::Fast;
        match self.index.as_mut() {
            Some(r) => {
                if r.alive() {
                    RuntimeInfo {
                        running: true,
                        port: Some(r.server.port()),
                        unload_in_s: (!fast).then(|| IDLE_UNLOAD.saturating_sub(idle).as_secs()),
                    }
                } else {
                    RuntimeInfo::default()
                }
            }
            None => RuntimeInfo::default(),
        }
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

    struct Fake(Vec<Vec<f32>>);
    impl TextVectors for Fake {
        fn text_vectors(&self, texts: &[&str]) -> winseek_llama::Result<Vec<Vec<f32>>> {
            assert_eq!(texts, PARITY_STRINGS);
            Ok(self.0.clone())
        }
    }
    struct Broken;
    impl TextVectors for Broken {
        fn text_vectors(&self, _: &[&str]) -> winseek_llama::Result<Vec<Vec<f32>>> {
            Err(winseek_llama::Error::HealthTimeout)
        }
    }

    /// Unit-ish vector in DIMS dims: mostly axis `a`, `leak` of axis `b`.
    fn v(a: usize, b: usize, leak: f32) -> Vec<f32> {
        let mut x = vec![0.0; DIMS];
        x[a] = 1.0;
        x[b] += leak;
        x
    }

    fn three(leak: f32) -> Fake {
        Fake(vec![v(0, 9, leak), v(1, 9, leak), v(2, 9, leak)])
    }

    #[test]
    fn parity_is_the_worst_cosine_over_the_strings() {
        let same = text_parity(&three(0.0), &three(0.0)).unwrap();
        assert!((same - 1.0).abs() < 1e-6);
        // Only the second string drifts: the minimum reports it.
        let drift = Fake(vec![v(0, 9, 0.0), v(1, 9, 0.5), v(2, 9, 0.0)]);
        let c = text_parity(&three(0.0), &drift).unwrap();
        assert!(c < 0.95 && c > 0.8, "{c}");
        assert!(text_parity(&Broken, &three(0.0)).is_err());
        assert!(text_parity(&Fake(vec![v(0, 9, 0.0)]), &three(0.0)).is_err()); // wrong count
    }

    #[test]
    fn guard_keeps_query_route_when_vectors_agree() {
        let mut g = ParityGuard::default();
        assert!(!g.due(false) && g.due(true));
        assert_eq!(g.record(Ok(0.99993)), None);
        assert!(!g.due(true), "runs once per session");
        assert_eq!(g.route(), TextRoute::Query);
    }

    #[test]
    fn guard_routes_to_index_below_threshold_and_warns_once() {
        let mut g = ParityGuard::default();
        let w = g.record(Ok(0.998)).expect("warning");
        assert!(w.contains("0.99800") && w.contains("index server"), "{w}");
        assert_eq!(g.route(), TextRoute::Index);
        assert!(!g.due(true));
        // Exactly on the threshold passes.
        let mut g = ParityGuard::default();
        assert_eq!(g.record(Ok(PARITY_MIN)), None);
        assert_eq!(g.route(), TextRoute::Query);
    }

    #[test]
    fn guard_stays_due_after_a_failed_check() {
        let mut g = ParityGuard::default();
        assert!(g.record(Err(winseek_llama::Error::HealthTimeout)).is_some());
        assert!(g.due(true));
        assert_eq!(g.route(), TextRoute::Query);
    }

    #[test]
    fn end_to_end_guard_with_fakes() {
        let mut g = ParityGuard::default();
        g.record(text_parity(&three(0.0), &three(0.2)));
        assert_eq!(g.route(), TextRoute::Index); // 1/sqrt(1.04) = 0.98
    }

    #[test]
    fn profile_switch_rebuilds_the_shared_gpu_phase() {
        let mut e = LlamaEmbedder::new(PathBuf::from(r"C:\nonexistent-winseek-root"), Manifest::embedded());
        let gpu = e.gpu_phase();
        assert_eq!(lock(&gpu).begin(Gpu::Index), None);
        assert_eq!(lock(&gpu).begin(Gpu::Parser), Some(Gpu::Index), "balanced: one at a time");
        e.set_profile(Profile::Fast);
        assert_eq!(e.profile, Profile::Fast);
        // same Arc, new inner value: nothing is loaded any more and Fast lets both live together
        assert_eq!(lock(&gpu).begin(Gpu::Parser), None);
        assert_eq!(lock(&gpu).begin(Gpu::Index), None);
        e.set_profile(Profile::Balanced);
        e.release_gpu(); // no server: harmless
        assert_eq!(lock(&gpu).begin(Gpu::Index), None, "set_profile started from a clean phase");
        assert_eq!(lock(&gpu).begin(Gpu::Parser), Some(Gpu::Index));
    }

    #[test]
    fn gpu_busy_errors_are_recognised() {
        assert!(is_gpu_busy(&format!("{GPU_BUSY} with the Parser model; image work waits for it to unload")));
        assert!(!is_gpu_busy("boom"));
    }

    #[test]
    fn pause_and_reset_without_servers_are_harmless() {
        let mut e = LlamaEmbedder::new(PathBuf::from(r"C:\nonexistent-winseek-root"), Manifest::embedded());
        e.on_pause();
        e.reset();
        e.tick_idle();
        assert_eq!(e.runtime_info(), RuntimeInfo::default());
    }
}
