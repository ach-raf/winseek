//! The optional reranker behind semantic search (Qwen3-Reranker-0.6B): a trait, plus the real
//! implementation that runs one CPU-only `llama-server` (`Role::Rerank`) on demand.
//!
//! Nothing here may ever make a search worse: every failure (files missing, spawn failed, timeout,
//! bad reply) is an `Err` and the caller keeps the ranking it already has. The server is spawned
//! lazily; the query that finds none running does not wait for it (it starts loading on a
//! background thread and that query gets `Warming`), later queries are reranked. It is unloaded
//! after [`IDLE_UNLOAD`] without use. It needs no GPU, so it takes no part in `GpuPhase`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use winseek_llama::install::resolve_exe;
use winseek_llama::manifest::ModelRole;
use winseek_llama::{
    detect_backend, fallback_chain, fallback_order, Backend, IdleReaper, LlamaServer, Manifest,
    ModelProfile, Profile, RerankClient, Role, SpawnConfig,
};

use super::embedder::models_dir;

pub const MODEL_ID: &str = "qwen3-reranker-0.6b";
pub const IDLE_UNLOAD: Duration = Duration::from_secs(120);
/// Hard budget of one rerank call (the HTTP request); a slower answer is dropped.
pub const BUDGET: Duration = Duration::from_millis(1500);
/// A 0.6B Q6_K model loads in a second or two; a cold disk may take longer.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(60);
/// After a failed start, wait this long before trying to spawn again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq)]
pub enum RerankError {
    /// Model file or llama build not on disk.
    Missing,
    /// No server is running yet: it is being started in the background, this query is not refined.
    Warming,
    /// Spawn failed, timeout, HTTP or reply problem.
    Other(String),
}

impl fmt::Display for RerankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RerankError::Missing => write!(f, "reranker not installed"),
            RerankError::Warming => write!(f, "reranker is starting"),
            RerankError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for RerankError {}

pub trait Reranker {
    /// The model file and a llama build are on disk (cheap check, no spawning).
    fn present(&self) -> bool;
    /// Relevance in 0..1 per document, in input order. Must come back within [`BUDGET`].
    fn rerank(&mut self, query: &str, docs: &[String]) -> Result<Vec<f32>, RerankError>;
    /// Called periodically; the real impl unloads the server when idle.
    fn tick_idle(&mut self);
    /// Drop the server (switched off, shutting down).
    fn unload(&mut self);
    /// Start loading in the background so the next search is refined. Default: nothing.
    fn warm(&mut self) {}
    /// "off" (not installed) | "warming" | "ready" (loaded, or loads on first use) | "error".
    fn state(&self) -> &'static str {
        "off"
    }
}

/// Model file of the reranker entry under `models_dir`.
pub fn model_path(manifest: &Manifest, models_dir: &Path) -> Option<PathBuf> {
    let entry = manifest.model(MODEL_ID, ModelProfile::Default)?;
    entry
        .files
        .iter()
        .find(|f| f.role == ModelRole::Model)
        .map(|f| models_dir.join(f.file_name()))
}

struct Running {
    // Field order: the client is dropped before the server process is killed.
    client: RerankClient,
    server: LlamaServer,
}

/// State shared with the background start-up thread.
#[derive(Default)]
struct Shared {
    slot: Mutex<Option<Running>>,
    warming: AtomicBool,
    /// Bumped by `unload`: a start that finishes after it throws its server away.
    epoch: AtomicU64,
    failed: Mutex<Option<(Instant, String)>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct LlamaReranker {
    root: PathBuf,
    manifest: Manifest,
    shared: Arc<Shared>,
    reaper: IdleReaper,
}

impl LlamaReranker {
    /// `root` is `%LOCALAPPDATA%\winseek`.
    pub fn new(root: PathBuf, manifest: Manifest) -> Self {
        LlamaReranker {
            root,
            manifest,
            shared: Arc::new(Shared::default()),
            reaper: IdleReaper::new(IDLE_UNLOAD),
        }
    }

    fn file_present(&self) -> bool {
        model_path(&self.manifest, &models_dir(&self.root)).is_some_and(|p| p.is_file())
    }

    fn any_exe(&self) -> bool {
        [Backend::Cuda12, Backend::Vulkan, Backend::Cpu]
            .into_iter()
            .any(|b| resolve_exe(&self.root, &self.manifest, b).is_some())
    }

    /// Spawn on a background thread unless one is running / a recent attempt failed.
    fn start_warmup(&self) {
        if let Some((at, _)) = lock(&self.shared.failed).as_ref() {
            if at.elapsed() < RETRY_AFTER {
                return;
            }
        }
        if self.shared.warming.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = self.shared.clone();
        let epoch = shared.epoch.load(Ordering::SeqCst);
        let (root, manifest) = (self.root.clone(), self.manifest.clone());
        let started = std::thread::Builder::new()
            .name("rerank-warm".into())
            .spawn({
                let shared = shared.clone();
                move || {
                    let r = spawn_server(&root, &manifest);
                    match r {
                        Ok(run) => {
                            *lock(&shared.failed) = None;
                            if shared.epoch.load(Ordering::SeqCst) == epoch {
                                *lock(&shared.slot) = Some(run);
                            } // else unloaded meanwhile: dropping `run` kills the server
                        }
                        Err(e) => {
                            eprintln!("winseek: reranker not started: {e}");
                            *lock(&shared.failed) = Some((Instant::now(), e));
                        }
                    }
                    shared.warming.store(false, Ordering::SeqCst);
                }
            });
        if started.is_err() {
            shared.warming.store(false, Ordering::SeqCst);
        }
    }
}

fn spawn_server(root: &Path, manifest: &Manifest) -> Result<Running, String> {
    let model = model_path(manifest, &models_dir(root)).ok_or("reranker not in manifest")?;
    let start = detect_backend().backend;
    // `Role::Rerank` is CPU-only whatever build runs it; the chain only finds an exe that starts.
    let cfg = SpawnConfig {
        role: Role::Rerank,
        profile: Profile::Balanced,
        health_timeout: HEALTH_TIMEOUT,
        ..Default::default()
    };
    let (_, server) = fallback_chain(&fallback_order(start), |b| {
        let exe = resolve_exe(root, manifest, b).ok_or_else(|| {
            winseek_llama::Error::NotFound(format!("llama-server for {}", b.dir_name()))
        })?;
        LlamaServer::spawn_with(&exe, &model, None, &[], &cfg)
    })
    .map_err(|e| e.to_string())?;
    eprintln!("winseek: reranker server on port {}", server.port());
    Ok(Running {
        client: RerankClient::new(&server.base_url(), server.api_key()),
        server,
    })
}

impl Reranker for LlamaReranker {
    fn present(&self) -> bool {
        self.file_present() && self.any_exe()
    }

    fn rerank(&mut self, query: &str, docs: &[String]) -> Result<Vec<f32>, RerankError> {
        if !self.present() {
            return Err(RerankError::Missing);
        }
        self.reaper.touch();
        {
            let mut slot = lock(&self.shared.slot);
            if let Some(r) = slot.as_mut() {
                if r.server.is_running() {
                    return r
                        .client
                        .rerank(query, docs, BUDGET)
                        .map_err(|e| RerankError::Other(e.to_string()));
                }
                *slot = None; // crashed: start again below
            }
        }
        if let Some((at, msg)) = lock(&self.shared.failed).as_ref() {
            if at.elapsed() < RETRY_AFTER {
                return Err(RerankError::Other(msg.clone()));
            }
        }
        self.start_warmup();
        Err(RerankError::Warming)
    }

    fn tick_idle(&mut self) {
        if lock(&self.shared.slot).is_some() && self.reaper.should_reap() {
            self.unload();
        }
    }

    fn unload(&mut self) {
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        lock(&self.shared.slot).take();
    }

    fn warm(&mut self) {
        if self.present() && lock(&self.shared.slot).is_none() {
            self.reaper.touch();
            self.start_warmup();
        }
    }

    fn state(&self) -> &'static str {
        if !self.file_present() {
            "off"
        } else if self.shared.warming.load(Ordering::SeqCst) {
            "warming"
        } else if lock(&self.shared.slot).is_none() && lock(&self.shared.failed).is_some() {
            "error"
        } else {
            "ready"
        }
    }
}

/// Reranker stand-in: never present (the default until the real one is installed in `Tier1`).
pub struct NoReranker;

impl Reranker for NoReranker {
    fn present(&self) -> bool {
        false
    }
    fn rerank(&mut self, _: &str, _: &[String]) -> Result<Vec<f32>, RerankError> {
        Err(RerankError::Missing)
    }
    fn tick_idle(&mut self) {}
    fn unload(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_files_mean_missing_and_off() {
        let d = tempfile::tempdir().unwrap();
        let mut r = LlamaReranker::new(d.path().to_path_buf(), Manifest::embedded());
        assert!(!r.present());
        assert_eq!(r.state(), "off");
        assert_eq!(r.rerank("q", &["a".into()]), Err(RerankError::Missing));
        r.tick_idle();
        r.unload();
    }

    #[test]
    fn model_path_is_the_pinned_file_name() {
        let man = Manifest::embedded();
        let p = model_path(&man, Path::new("m")).unwrap();
        assert_eq!(p, Path::new("m").join("Qwen3-Reranker-0.6B-Q6_K.gguf"));
        assert!(!NoReranker.present());
    }

    /// Live: `WINSEEK_RERANK_GGUF=<path to Qwen3-Reranker-0.6B-Q6_K.gguf>` plus an installed
    /// `llama-server.exe` under `%LOCALAPPDATA%\winseek\llama`.
    #[test]
    #[ignore = "needs WINSEEK_RERANK_GGUF and an installed llama-server"]
    fn live_server_ranks_the_relevant_document_first() {
        let Some(gguf) = std::env::var_os("WINSEEK_RERANK_GGUF").map(PathBuf::from) else {
            eprintln!("WINSEEK_RERANK_GGUF not set");
            return;
        };
        let Some(root) = super::super::data_root() else {
            return;
        };
        let man = Manifest::embedded();
        let Some(exe) = [Backend::Cuda12, Backend::Vulkan, Backend::Cpu]
            .into_iter()
            .find_map(|b| resolve_exe(&root, &man, b))
        else {
            eprintln!("no llama-server installed");
            return;
        };
        let cfg = SpawnConfig {
            role: Role::Rerank,
            health_timeout: Duration::from_secs(120),
            ..Default::default()
        };
        let server = LlamaServer::spawn_with(&exe, &gguf, None, &[], &cfg).expect("spawn");
        let c = RerankClient::new(&server.base_url(), server.api_key());
        let docs: Vec<String> = [
            "Quarterly sales figures for the northern region, with a breakdown by product.",
            "The capital of France is Paris, a city on the river Seine.",
            "Recipe: whisk eggs and sugar, fold in flour, bake for forty minutes.",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let s = c
            .rerank(
                "What is the capital of France?",
                &docs,
                Duration::from_secs(20),
            )
            .expect("rerank");
        assert_eq!(s.len(), 3);
        assert!(
            s[1] > s[0] && s[1] > s[2] && s[1] != s[0],
            "scores must differ and favour the Paris document: {s:?}"
        );
    }
}
