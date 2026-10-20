//! The document parser behind Tier 1 (Infinity-Parser2): a trait, plus the real implementation that
//! runs one `llama-server` (`Role::Parser`) on demand.
//!
//! The server is spawned on the first page, shares the GPU with the embedding index server through
//! [`GpuPhase`] (outside the Fast profile the two are never loaded together), is unloaded after 2
//! minutes idle (not in Fast) and immediately when indexing pauses.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use winseek_llama::install::resolve_exe;
use winseek_llama::manifest::{ModelEntry, ModelRole};
use winseek_llama::{
    detect_backend, fallback_chain, fallback_order, Backend, IdleReaper, LlamaServer, Manifest, ModelProfile, ParseClient, Parsed,
    Profile, Role, SpawnConfig,
};

use super::embedder::{models_dir, Embedder, RuntimeInfo};
use super::gpu_phase::{Gpu, GpuPhase};

pub const MODEL_ID: &str = "infinity-parser2-flash";
pub const IDLE_UNLOAD: Duration = Duration::from_secs(120);
/// A cold start loads about 1.3 GB of weights: give the server this long to become healthy.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// llama.cpp build or the parser's model files are not on disk.
    ModelsMissing,
    /// The embedding index server holds the GPU: the caller releases it and retries.
    GpuBusy,
    /// The server answered but the output could not be used ("output is not JSON" and the
    /// like): a property of this page's answer, not of the server — worth a same-server retry.
    Output(String),
    /// Anything else (spawn failed, HTTP error): the server itself is in a bad state.
    Other(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::ModelsMissing => write!(f, "document parser not installed"),
            ParseError::GpuBusy => write!(f, "GPU is busy with the index model"),
            ParseError::Output(m) | ParseError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Model-output problems (`Error::Parse`) get their own variant so the parse pass can retry
/// them in place; transport/spawn failures stay `Other`.
fn classify(e: winseek_llama::Error) -> ParseError {
    match e {
        winseek_llama::Error::Parse(m) => ParseError::Output(m),
        other => ParseError::Other(other.to_string()),
    }
}

pub trait Parser {
    /// The model files and a llama build are on disk (cheap check, no spawning).
    fn present(&self) -> bool;
    /// Parse one page image (JPEG, long side 1024).
    fn parse(&mut self, jpeg: &[u8]) -> Result<Parsed, ParseError>;
    /// Parse several pages at once. Default: one after another. The real server decodes
    /// concurrent requests across its slots (`-np`), so pages go out in parallel.
    fn parse_batch(&mut self, jpegs: &[&[u8]]) -> Vec<Result<Parsed, ParseError>> {
        jpegs.iter().map(|j| self.parse(j)).collect()
    }
    /// Called periodically; the real impl unloads the server when idle.
    fn tick_idle(&mut self) {}
    /// Drop the server (pause, profile change, GPU wanted by the index model).
    fn unload(&mut self) {}
    /// Server state for the status page. Default: nothing running.
    fn runtime_info(&mut self) -> RuntimeInfo {
        RuntimeInfo::default()
    }
    /// The performance profile changed: unload and use the new one.
    fn set_profile(&mut self, _p: Profile) {}
}

/// Parse with GPU phasing: a `GpuBusy` answer makes the index server let go (`release_gpu`) and the
/// page is tried once more. The two locks are never held together.
pub fn parse_phased(
    parser: &Mutex<Box<dyn Parser + Send>>,
    embedder: &Mutex<Box<dyn Embedder + Send>>,
    jpeg: &[u8],
) -> Result<Parsed, ParseError> {
    let lock = |m: &Mutex<Box<dyn Parser + Send>>| -> Result<Parsed, ParseError> {
        m.lock().unwrap_or_else(|e| e.into_inner()).parse(jpeg)
    };
    match lock(parser) {
        Err(ParseError::GpuBusy) => {
            embedder.lock().unwrap_or_else(|e| e.into_inner()).release_gpu();
            lock(parser)
        }
        r => r,
    }
}

/// Model files of the default parser entry: `(model, mmproj)` paths under `models_dir`.
pub fn model_paths(manifest: &Manifest, models_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let entry: &ModelEntry = manifest.model(MODEL_ID, ModelProfile::Default)?;
    let file = |role: ModelRole| entry.files.iter().find(|f| f.role == role).map(|f| models_dir.join(f.file_name()));
    Some((file(ModelRole::Model)?, file(ModelRole::Mmproj)?))
}

struct Running {
    // Field order: the client is dropped before the server process is killed.
    client: ParseClient,
    server: LlamaServer,
}

pub struct LlamaParser {
    root: PathBuf,
    manifest: Manifest,
    profile: Profile,
    /// Shared with the embedder (same `Arc`), so the two phase each other.
    gpu: Arc<Mutex<GpuPhase>>,
    running: Option<Running>,
    reaper: IdleReaper,
    backend: Option<Backend>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl LlamaParser {
    /// `root` is `%LOCALAPPDATA%\winseek`; `gpu` is the embedder's [`GpuPhase`].
    pub fn new(root: PathBuf, manifest: Manifest, profile: Profile, gpu: Arc<Mutex<GpuPhase>>) -> Self {
        LlamaParser { root, manifest, profile, gpu, running: None, reaper: IdleReaper::new(IDLE_UNLOAD), backend: None }
    }

    fn files_present(&self) -> bool {
        match model_paths(&self.manifest, &models_dir(&self.root)) {
            Some((m, p)) => m.is_file() && p.is_file(),
            None => false,
        }
    }

    fn any_exe(&self) -> bool {
        [Backend::Cuda12, Backend::Vulkan, Backend::Cpu]
            .into_iter()
            .any(|b| resolve_exe(&self.root, &self.manifest, b).is_some())
    }

    fn spawn(&mut self) -> Result<Running, ParseError> {
        let (model, mmproj) = model_paths(&self.manifest, &models_dir(&self.root)).ok_or(ParseError::ModelsMissing)?;
        let start = *self.backend.get_or_insert_with(|| detect_backend().backend);
        let (root, manifest) = (&self.root, &self.manifest);
        let cfg = SpawnConfig { role: Role::Parser, profile: self.profile, health_timeout: HEALTH_TIMEOUT, ..Default::default() };
        let (_, server) = fallback_chain(&fallback_order(start), |b| {
            let exe = resolve_exe(root, manifest, b)
                .ok_or_else(|| winseek_llama::Error::NotFound(format!("llama-server for {}", b.dir_name())))?;
            LlamaServer::spawn_with(&exe, &model, Some(&mmproj), &[], &cfg)
        })
        .map_err(|e| ParseError::Other(e.to_string()))?;
        eprintln!("winseek: parser server on port {} ({} slot(s), {})", server.port(), winseek_llama::server::parser_slots(self.profile), self.profile.as_str());
        Ok(Running { client: ParseClient::new(&server.base_url(), server.api_key()), server })
    }

    /// Make sure a live server exists (loading it, behind the GPU phase gate, if needed).
    fn ensure(&mut self) -> Result<(), ParseError> {
        if let Some(r) = self.running.as_mut() {
            if r.server.is_running() {
                return Ok(());
            }
            self.unload(); // crashed: respawn
        }
        if lock(&self.gpu).begin(Gpu::Parser).is_some() {
            return Err(ParseError::GpuBusy);
        }
        match self.spawn() {
            Ok(r) => {
                self.running = Some(r);
                Ok(())
            }
            Err(e) => {
                lock(&self.gpu).end(Gpu::Parser);
                Err(e)
            }
        }
    }
}

impl Parser for LlamaParser {
    fn present(&self) -> bool {
        self.files_present() && self.any_exe()
    }

    fn parse(&mut self, jpeg: &[u8]) -> Result<Parsed, ParseError> {
        if !self.present() {
            return Err(ParseError::ModelsMissing);
        }
        self.ensure()?;
        self.reaper.touch();
        let r = self.running.as_ref().expect("started").client.parse_image_jpeg(jpeg).map_err(classify);
        self.reaper.touch(); // a 2.5 s page must not count as idle time
        r
    }

    fn parse_batch(&mut self, jpegs: &[&[u8]]) -> Vec<Result<Parsed, ParseError>> {
        if jpegs.is_empty() {
            return vec![];
        }
        if let Err(e) = self.ensure() {
            let msg = e.to_string();
            return (0..jpegs.len()).map(|_| Err(ParseError::Other(msg.clone()))).collect();
        }
        self.reaper.touch();
        let out = self.running.as_ref().expect("started").client.parse_image_jpegs(jpegs);
        self.reaper.touch();
        out.into_iter().map(|r| r.map_err(classify)).collect()
    }

    fn tick_idle(&mut self) {
        // Fast keeps the server resident while the app is open (pause still unloads it).
        if self.profile != Profile::Fast && self.running.is_some() && self.reaper.should_reap() {
            self.unload();
        }
    }

    fn unload(&mut self) {
        if self.running.take().is_some() {
            lock(&self.gpu).end(Gpu::Parser);
        }
    }

    fn runtime_info(&mut self) -> RuntimeInfo {
        let idle = self.reaper.idle_for();
        let fast = self.profile == Profile::Fast;
        match self.running.as_mut() {
            Some(r) => {
                if r.server.is_running() {
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

    fn set_profile(&mut self, p: Profile) {
        self.unload();
        self.profile = p;
        *lock(&self.gpu) = GpuPhase::new(p);
    }
}

/// Parser stand-in for tests that do not touch parsing: never present.
#[cfg(test)]
pub struct NoParser;

#[cfg(test)]
impl Parser for NoParser {
    fn present(&self) -> bool {
        false
    }
    fn parse(&mut self, _: &[u8]) -> Result<Parsed, ParseError> {
        Err(ParseError::ModelsMissing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winseek_llama::Element;

    fn llama(root: &str) -> LlamaParser {
        let gpu = Arc::new(Mutex::new(GpuPhase::new(Profile::Balanced)));
        LlamaParser::new(PathBuf::from(root), Manifest::embedded(), Profile::Balanced, gpu)
    }

    #[test]
    fn model_paths_use_manifest_file_names() {
        let (m, p) = model_paths(&Manifest::embedded(), Path::new(r"C:\m")).unwrap();
        assert_eq!(m, Path::new(r"C:\m\Infinity-Parser2-Flash.Q4_K_M.gguf"));
        assert_eq!(p, Path::new(r"C:\m\Infinity-Parser2-Flash.mmproj-q8_0.gguf"));
    }

    #[test]
    fn missing_files_report_models_missing_without_spawning() {
        let mut p = llama(r"C:\nonexistent-winseek-root");
        assert!(!p.present());
        assert_eq!(p.parse(&[1, 2, 3]).unwrap_err(), ParseError::ModelsMissing);
        assert_eq!(p.runtime_info(), RuntimeInfo::default());
    }

    #[test]
    fn present_files_without_llama_exe_still_missing() {
        let d = tempfile::tempdir().unwrap();
        let man = Manifest::embedded();
        let models = models_dir(d.path());
        std::fs::create_dir_all(&models).unwrap();
        let (m, p) = model_paths(&man, &models).unwrap();
        std::fs::write(m, b"x").unwrap();
        std::fs::write(p, b"x").unwrap();
        let parser = LlamaParser::new(d.path().to_path_buf(), man, Profile::Balanced, Arc::new(Mutex::new(GpuPhase::new(Profile::Balanced))));
        assert!(!parser.present());
    }

    #[test]
    fn unload_tick_and_profile_change_without_a_server_are_harmless() {
        let mut p = llama(r"C:\nonexistent-winseek-root");
        p.unload();
        p.tick_idle();
        p.set_profile(Profile::Fast);
        assert_eq!(p.profile, Profile::Fast);
        // the shared phase was rebuilt for the new profile: Fast lets both models live together
        let mut g = lock(&p.gpu);
        assert_eq!(g.begin(Gpu::Index), None);
        assert_eq!(g.begin(Gpu::Parser), None);
    }

    #[test]
    fn busy_gpu_is_reported_before_anything_spawns() {
        let mut p = llama(r"C:\nonexistent-winseek-root");
        // pretend the index server is up; `ensure` must not spawn (and must not mark the parser loaded)
        assert_eq!(lock(&p.gpu).begin(Gpu::Index), None);
        assert_eq!(p.ensure().unwrap_err(), ParseError::GpuBusy);
        assert!(!lock(&p.gpu).is_loaded(Gpu::Parser));
    }

    /// Parser that answers `GpuBusy` until the index server released the GPU.
    struct Gated {
        released: Arc<std::sync::atomic::AtomicBool>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Parser for Gated {
        fn present(&self) -> bool {
            true
        }
        fn parse(&mut self, _: &[u8]) -> Result<Parsed, ParseError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.released.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(Parsed {
                    elements: vec![Element { bbox: None, category: "text".into(), text: "ok".into() }],
                    prompt_version: "v".into(),
                })
            } else {
                Err(ParseError::GpuBusy)
            }
        }
    }

    struct Index(Arc<std::sync::atomic::AtomicBool>);
    impl Embedder for Index {
        fn embed_images(&mut self, _: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Ok(vec![])
        }
        fn embed_query(&mut self, _: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(vec![])
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            super::super::embedder::index_meta("test")
        }
        fn release_gpu(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn parse_phased_releases_the_index_server_and_retries_once() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let released = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let parser: Mutex<Box<dyn Parser + Send>> = Mutex::new(Box::new(Gated { released: released.clone(), calls: calls.clone() }));
        let embedder: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Index(released.clone())));
        let r = parse_phased(&parser, &embedder, b"jpg").unwrap();
        assert_eq!(r.elements[0].text, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one busy answer, one retry");
        assert!(released.load(Ordering::SeqCst));
        // no retry when it worked the first time
        calls.store(0, Ordering::SeqCst);
        parse_phased(&parser, &embedder, b"jpg").unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // a second busy answer is returned, not retried again
        let stuck: Mutex<Box<dyn Parser + Send>> =
            Mutex::new(Box::new(Gated { released: Arc::new(AtomicBool::new(false)), calls: calls.clone() }));
        calls.store(0, Ordering::SeqCst);
        assert_eq!(parse_phased(&stuck, &embedder, b"jpg").unwrap_err(), ParseError::GpuBusy);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
