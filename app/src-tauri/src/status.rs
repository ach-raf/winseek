//! The status page: one command that gathers Tier 0 (searchd), Tier 1 (meaning index) and the
//! models/llama runtime into a single DTO, plus the page's two actions (rescan, rebuild).
//!
//! The Tier 0 probe talks to searchd over the pipe and may hang, so it runs on its own thread
//! with a short timeout; on failure that section reads "unavailable" and the rest still shows.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};
use winseek_llama::install::resolve_exe;
use winseek_llama::manifest::ModelRole;
use winseek_llama::{Detected, Manifest, ModelProfile};
use winseek_store::Store;

use crate::commands::AppState;
use crate::models::Models;
use crate::service;
use crate::source::{NameSource, Tier0Detail, VolumeDetail};
use crate::tier1::embedder::{self, RuntimeInfo};
use crate::tier1::progress::{coverage, eta_secs};
use crate::tier1::{self, StatsDto, Tier1};

const TIER0_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Serialize)]
pub struct Tier0Dto {
    /// "searchd" | "demo" | "unavailable"
    pub source: &'static str,
    /// Windows service state: "not_installed" | "stopped" | "starting" | "running" | "stopping" | "unknown".
    pub service: String,
    pub uptime_s: Option<u64>,
    pub volumes: Vec<VolumeDetail>,
    /// Rescan only works against a live searchd.
    pub can_rescan: bool,
    /// Why rescan is off (tooltip), when it is.
    pub rescan_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tier1PageDto {
    /// "models_missing" | "idle" | "indexing" | "paused" | "unavailable"
    pub state: String,
    pub paused_reason: Option<&'static str>,
    pub error: Option<String>,
    pub stats: Option<StatsDto>,
    /// embedded / (embedded + pending + failed), 0..1.
    pub coverage: Option<f64>,
    /// Files waiting to be embedded.
    pub queue: u64,
    /// Files per second over the last minute (null when idle / unknown).
    pub rate: Option<f64>,
    pub eta_s: Option<u64>,
    pub can_rebuild: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModelFileDto {
    pub name: String,
    /// "model" | "mmproj"
    pub role: &'static str,
    pub expected_bytes: u64,
    /// On-disk size, null when the file is not there.
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServerDto {
    pub running: bool,
    pub port: Option<u16>,
    /// Seconds until the idle unload frees the GPU memory (only while running).
    pub unload_in_s: Option<u64>,
    /// The idle timeout itself.
    pub idle_unload_s: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelsPageDto {
    pub llama_build: String,
    /// "cuda12" | "vulkan" | "cpu"
    pub backend: winseek_llama::Backend,
    pub gpu: Option<String>,
    pub llama_present: bool,
    pub model_id: &'static str,
    pub profile: &'static str,
    pub quant: &'static str,
    pub files: Vec<ModelFileDto>,
    pub server: ServerDto,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusPageDto {
    pub tier0: Tier0Dto,
    pub tier1: Tier1PageDto,
    pub models: ModelsPageDto,
}

// ---------------------------------------------------------------- Tier 0

/// Run `f` on a helper thread and give up after `timeout`. Only one probe runs at a time
/// (`busy`): while an earlier one is still stuck, later polls report unavailable at once instead of piling up.
fn probe(busy: &'static AtomicBool, timeout: Duration, f: impl FnOnce() -> Tier0Detail + Send + 'static) -> Tier0Detail {
    let unavailable = || Tier0Detail { kind: "unavailable", uptime_s: None, volumes: vec![] };
    if busy.swap(true, Ordering::SeqCst) {
        return unavailable();
    }
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("status-tier0".into()).spawn(move || {
        let d = f();
        busy.store(false, Ordering::SeqCst);
        let _ = tx.send(d);
    });
    if spawned.is_err() {
        busy.store(false, Ordering::SeqCst);
        return unavailable();
    }
    rx.recv_timeout(timeout).unwrap_or_else(|_| unavailable())
}

fn tier0_dto(detail: Tier0Detail, service: String) -> Tier0Dto {
    let can_rescan = detail.kind == "searchd";
    let rescan_hint = match detail.kind {
        "searchd" => None,
        "demo" => Some("Needs the name service, which is not running.".to_string()),
        _ => Some("The name service is not answering.".to_string()),
    };
    Tier0Dto { source: detail.kind, service, uptime_s: detail.uptime_s, volumes: detail.volumes, can_rescan, rescan_hint }
}

fn tier0_section(source: Arc<dyn NameSource>) -> Tier0Dto {
    static BUSY: AtomicBool = AtomicBool::new(false);
    tier0_dto(probe(&BUSY, TIER0_TIMEOUT, move || source.detail()), service::service_state())
}

// ---------------------------------------------------------------- Tier 1

fn tier1_section(t: Option<&Tier1>, unavailable: Option<String>) -> Tier1PageDto {
    let Some(t) = t else {
        return Tier1PageDto {
            state: "unavailable".into(),
            paused_reason: None,
            error: unavailable,
            stats: None,
            coverage: None,
            queue: 0,
            rate: None,
            eta_s: None,
            can_rebuild: false,
        };
    };
    let st = t.status();
    let (cov, queue) = st.stats.as_ref().map_or((None, 0), |s| (coverage(s.done, s.pending, s.failed), s.pending));
    // Throughput only means something while the worker is actually embedding.
    let rate = if st.state == "indexing" { t.rate() } else { None };
    Tier1PageDto {
        eta_s: eta_secs(queue, rate),
        state: st.state,
        paused_reason: st.paused_reason,
        error: st.error,
        stats: st.stats,
        coverage: cov,
        queue,
        rate,
        can_rebuild: true,
    }
}

// ---------------------------------------------------------------- Models

/// The default embedder's model files: expected size from the manifest, actual size from disk.
fn model_files(manifest: &Manifest, dir: &Path) -> Vec<ModelFileDto> {
    let Some(entry) = manifest.model(embedder::MODEL_ID, ModelProfile::Default) else { return vec![] };
    entry
        .files
        .iter()
        .map(|f| ModelFileDto {
            name: f.file_name().to_string(),
            role: match f.role {
                ModelRole::Model => "model",
                ModelRole::Mmproj => "mmproj",
            },
            expected_bytes: f.size,
            bytes: std::fs::metadata(dir.join(f.file_name())).ok().filter(|m| m.is_file()).map(|m| m.len()),
        })
        .collect()
}

fn models_section(root: Option<&Path>, manifest: &Manifest, det: &Detected, rt: RuntimeInfo) -> ModelsPageDto {
    ModelsPageDto {
        llama_build: manifest.llama.build.clone(),
        backend: det.backend,
        gpu: det.gpu_name.clone(),
        llama_present: root.is_some_and(|r| resolve_exe(r, manifest, det.backend).is_some()),
        model_id: embedder::MODEL_ID,
        profile: "default",
        quant: embedder::QUANT,
        files: root.map(|r| model_files(manifest, &embedder::models_dir(r))).unwrap_or_default(),
        server: ServerDto {
            running: rt.running,
            port: rt.port,
            unload_in_s: rt.unload_in_s,
            idle_unload_s: embedder::IDLE_UNLOAD.as_secs(),
        },
    }
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub async fn status_page(app: AppHandle) -> Result<StatusPageDto, String> {
    let a = app.clone();
    let t0 = tauri::async_runtime::spawn_blocking(move || tier0_section(a.state::<AppState>().source()));
    let rest = tauri::async_runtime::spawn_blocking(move || {
        let handle = app.state::<tier1::Handle>();
        let (worker, why) = match handle.get() {
            Ok(t) => (Some(t), None),
            Err(e) => (None, Some(e)),
        };
        let rt = worker.as_ref().map(|t| t.runtime_info()).unwrap_or_default();
        let det = app.state::<Models>().detected();
        let root = tier1::data_root();
        (tier1_section(worker.as_deref(), why), models_section(root.as_deref(), &Manifest::embedded(), &det, rt))
    });
    let tier0 = t0.await.map_err(|e| e.to_string())?;
    let (tier1, models) = rest.await.map_err(|e| e.to_string())?;
    Ok(StatusPageDto { tier0, tier1, models })
}

/// Ask searchd to rebuild one volume's name index. Allowed for a normal user: the pipe grants
/// the interactive user read/write and searchd does not check elevation.
#[tauri::command]
pub async fn rescan_volume(app: AppHandle, letter: String) -> Result<(), String> {
    let mut chars = letter.chars();
    let (Some(l), None) = (chars.next(), chars.next()) else { return Err("Expected a single drive letter".into()) };
    if !l.is_ascii_alphabetic() {
        return Err("Expected a drive letter".into());
    }
    let source = app.state::<AppState>().source();
    tauri::async_runtime::spawn_blocking(move || source.rescan(l.to_ascii_uppercase())).await.map_err(|e| e.to_string())?
}

/// Drop every embedding and index the chosen folders again. The name index (Tier 0) is untouched.
#[tauri::command]
pub async fn tier1_rebuild(h: tauri::State<'_, tier1::Handle>) -> Result<(), String> {
    let t = h.get()?;
    tauri::async_runtime::spawn_blocking(move || {
        let root = tier1::data_root().ok_or("LOCALAPPDATA is not set")?;
        let meta = embedder::index_meta(&Manifest::embedded().llama.build);
        t.rebuild_with(|| Store::reset(root.join("tier1.sqlite"), &meta).map_err(|e| format!("reset store: {e}")))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn detail(kind: &'static str) -> Tier0Detail {
        Tier0Detail { kind, uptime_s: Some(5), volumes: vec![] }
    }

    #[test]
    fn rescan_only_with_live_searchd() {
        let live = tier0_dto(detail("searchd"), "running".into());
        assert!(live.can_rescan && live.rescan_hint.is_none());
        for k in ["demo", "unavailable"] {
            let d = tier0_dto(detail(k), "stopped".into());
            assert!(!d.can_rescan, "{k}");
            assert!(d.rescan_hint.is_some(), "{k}");
        }
    }

    #[test]
    fn probe_returns_the_answer_and_frees_the_slot() {
        static BUSY: AtomicBool = AtomicBool::new(false);
        let d = probe(&BUSY, Duration::from_secs(5), || detail("searchd"));
        assert_eq!(d.kind, "searchd");
        assert!(!BUSY.load(Ordering::SeqCst));
    }

    #[test]
    fn probe_times_out_and_does_not_pile_up() {
        static BUSY: AtomicBool = AtomicBool::new(false);
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let (release, wait) = mpsc::channel::<()>();
        let d = probe(&BUSY, Duration::from_millis(50), move || {
            CALLS.fetch_add(1, Ordering::SeqCst);
            let _ = wait.recv();
            detail("searchd")
        });
        assert_eq!(d.kind, "unavailable");
        // The stuck probe still owns the slot: the next poll answers immediately without a new thread.
        let d2 = probe(&BUSY, Duration::from_secs(5), || {
            CALLS.fetch_add(1, Ordering::SeqCst);
            detail("searchd")
        });
        assert_eq!(d2.kind, "unavailable");
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        drop(release); // lets the stuck thread finish
        for _ in 0..100 {
            if !BUSY.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!BUSY.load(Ordering::SeqCst));
    }

    #[test]
    fn model_files_report_disk_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let man = Manifest::embedded();
        let none = model_files(&man, dir.path());
        assert_eq!(none.len(), 2);
        assert!(none.iter().all(|f| f.bytes.is_none() && f.expected_bytes > 0));
        assert_eq!(none.iter().map(|f| f.role).collect::<Vec<_>>(), ["model", "mmproj"]);
        std::fs::write(dir.path().join(&none[0].name), b"12345").unwrap();
        let some = model_files(&man, dir.path());
        assert_eq!(some[0].bytes, Some(5));
        assert_eq!(some[1].bytes, None);
    }

    #[test]
    fn models_section_without_a_data_root() {
        let det = Detected { backend: winseek_llama::Backend::Cpu, gpu_name: None, driver_version: None };
        let m = models_section(None, &Manifest::embedded(), &det, RuntimeInfo { running: true, port: Some(8123), unload_in_s: Some(90) });
        assert!(!m.llama_present && m.files.is_empty());
        assert_eq!((m.server.running, m.server.port, m.server.unload_in_s, m.server.idle_unload_s), (true, Some(8123), Some(90), 120));
        assert_eq!(m.model_id, "embeddinggemma-2");
    }

    #[test]
    fn dto_json_shape() {
        let t1 = tier1_section(None, Some("open store: locked".into()));
        let page = StatusPageDto {
            tier0: tier0_dto(
                Tier0Detail {
                    kind: "searchd",
                    uptime_s: Some(61),
                    volumes: vec![VolumeDetail {
                        letter: "C".into(),
                        files: 10,
                        folders: 2,
                        state: "error".into(),
                        done: None,
                        total: None,
                        error: Some("journal".into()),
                    }],
                },
                "running".into(),
            ),
            tier1: t1,
            models: models_section(
                None,
                &Manifest::embedded(),
                &Detected { backend: winseek_llama::Backend::Vulkan, gpu_name: Some("GPU".into()), driver_version: None },
                RuntimeInfo::default(),
            ),
        };
        let v = serde_json::to_value(&page).unwrap();
        assert_eq!(v["tier0"]["source"], "searchd");
        assert_eq!(v["tier0"]["volumes"][0]["error"], "journal");
        assert_eq!(v["tier0"]["can_rescan"], true);
        assert_eq!(v["tier1"]["state"], "unavailable");
        assert_eq!(v["tier1"]["error"], "open store: locked");
        assert_eq!(v["tier1"]["can_rebuild"], false);
        assert!(v["tier1"]["coverage"].is_null() && v["tier1"]["eta_s"].is_null());
        assert_eq!(v["models"]["backend"], "vulkan");
        assert_eq!(v["models"]["server"]["running"], false);
        assert_eq!(v["models"]["server"]["idle_unload_s"], 120);
    }
}
