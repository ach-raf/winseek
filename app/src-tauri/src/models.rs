//! First-run download of everything Tier 1 needs: the llama.cpp build for the detected GPU
//! backend plus the default EmbeddingGemma-2 model and mmproj (PLAN "llama.cpp distribution").
//!
//! `models_plan` shows sizes up front; `models_download` runs in a background thread and emits
//! `models-progress` / `models-done` events; `models_cancel` aborts but keeps `.part` files, so
//! the next `models_download` resumes.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use winseek_llama::install::{install_llama_with, resolve_exe};
use winseek_llama::manifest::ModelFile;
use winseek_llama::{
    detect_backend, fetch_with, Backend, Detected, Error as LlamaError, FetchOptions, Manifest, ModelProfile, ModelRole, Progress,
};

use crate::tier1::{self, embedder};

/// Progress events are throttled to about 10 per second.
pub const EMIT_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlanItem {
    pub name: String,
    /// `None` when the manifest does not carry a size (llama.cpp zips): learned once the download starts.
    pub bytes: Option<u64>,
    pub present: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    /// "cuda12" | "vulkan" | "cpu"
    pub backend: Backend,
    pub gpu: Option<String>,
    pub driver: Option<String>,
    pub items: Vec<PlanItem>,
    /// Sum of the known sizes of the missing items.
    pub total_bytes_missing: u64,
    /// True when a missing item has no known size (so the total is a lower bound).
    pub size_unknown: bool,
    pub downloading: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProgressEvent {
    pub item: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub overall_downloaded: u64,
    pub overall_total: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoneEvent {
    pub ok: bool,
    pub error: Option<String>,
}

/// One downloadable thing and how to fetch it.
enum Source {
    /// A zip of the llama.cpp build; `file` is the zip's file name (what install reports progress under).
    Llama { file: String },
    Model(ModelFile),
}

struct Job {
    item: PlanItem,
    source: Source,
}

fn backend_label(b: Backend) -> &'static str {
    match b {
        Backend::Cuda12 => "CUDA 12",
        Backend::Vulkan => "Vulkan",
        Backend::Cpu => "CPU",
    }
}

fn zip_name(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

/// Everything the default setup needs, in download order, with what is already on disk.
fn jobs(root: &Path, manifest: &Manifest, backend: Backend) -> Vec<Job> {
    let mut out = Vec::new();
    if let Some(asset) = manifest.asset(backend) {
        let present = resolve_exe(root, manifest, backend).is_some();
        let mut push = |name: String, url: &str| {
            out.push(Job { item: PlanItem { name, bytes: None, present }, source: Source::Llama { file: zip_name(url) } });
        };
        push(format!("llama.cpp {} ({})", manifest.llama.build, backend_label(backend)), &asset.url);
        for e in &asset.extra {
            push("CUDA runtime libraries".into(), &e.url);
        }
    }
    if let Some(entry) = manifest.model(embedder::MODEL_ID, ModelProfile::Default) {
        let dir = embedder::models_dir(root);
        for f in &entry.files {
            let name = match f.role {
                ModelRole::Model => "EmbeddingGemma-2 model",
                ModelRole::Mmproj => "EmbeddingGemma-2 vision projector",
            };
            out.push(Job {
                item: PlanItem { name: name.into(), bytes: Some(f.size), present: dir.join(f.file_name()).is_file() },
                source: Source::Model(f.clone()),
            });
        }
    }
    out
}

pub fn build_plan(root: &Path, manifest: &Manifest, det: &Detected, downloading: bool) -> Plan {
    let items: Vec<PlanItem> = jobs(root, manifest, det.backend).into_iter().map(|j| j.item).collect();
    let missing = items.iter().filter(|i| !i.present);
    Plan {
        backend: det.backend,
        gpu: det.gpu_name.clone(),
        driver: det.driver_version.clone(),
        total_bytes_missing: missing.clone().filter_map(|i| i.bytes).sum(),
        size_unknown: missing.clone().any(|i| i.bytes.is_none()),
        items,
        downloading,
    }
}

/// Per-item byte counts -> overall progress. Only items that are actually being downloaded count.
struct Tracker {
    items: Vec<(String, u64, u64)>, // (name, total, downloaded)
}

impl Tracker {
    fn new(items: &[PlanItem]) -> Tracker {
        Tracker { items: items.iter().filter(|i| !i.present).map(|i| (i.name.clone(), i.bytes.unwrap_or(0), 0)).collect() }
    }

    fn update(&mut self, name: &str, p: Progress) -> ProgressEvent {
        if let Some(it) = self.items.iter_mut().find(|i| i.0 == name) {
            if let Some(t) = p.total {
                it.1 = t;
            }
            it.2 = p.downloaded;
        }
        self.event(name, p.total)
    }

    /// Mark an item fully downloaded.
    fn finish(&mut self, name: &str) -> ProgressEvent {
        let mut total = None;
        if let Some(it) = self.items.iter_mut().find(|i| i.0 == name) {
            it.2 = it.1;
            total = Some(it.1);
        }
        self.event(name, total)
    }

    fn event(&self, name: &str, total: Option<u64>) -> ProgressEvent {
        let downloaded = self.items.iter().find(|i| i.0 == name).map_or(0, |i| i.2);
        ProgressEvent {
            item: name.into(),
            downloaded,
            total,
            overall_downloaded: self.items.iter().map(|i| i.2).sum(),
            overall_total: self.items.iter().map(|i| i.1.max(i.2)).sum(),
        }
    }
}

struct Throttle {
    every: Duration,
    last: Option<Instant>,
}

impl Throttle {
    fn allow(&mut self, now: Instant) -> bool {
        match self.last {
            Some(l) if now.duration_since(l) < self.every => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Install the llama build and fetch the missing model files under `root`. Blocking.
/// `opts.cancel` aborts with `Err("cancelled")`; partial files stay for the next call.
pub fn run_download(
    root: &Path,
    manifest: &Manifest,
    backend: Backend,
    opts: &FetchOptions,
    emit_every: Duration,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<(), String> {
    let jobs = jobs(root, manifest, backend);
    let plan_items: Vec<PlanItem> = jobs.iter().map(|j| j.item.clone()).collect();
    let tracker = Mutex::new(Tracker::new(&plan_items));
    let throttle = Mutex::new(Throttle { every: emit_every, last: None });
    let emit = Mutex::new(emit);
    let err = |e: LlamaError| e.to_string();
    // `force` bypasses the throttle (item boundaries must always be seen).
    let send = |ev: ProgressEvent, force: bool| {
        if force || locked(&throttle).allow(Instant::now()) {
            (locked(&emit))(ev);
        }
    };

    let llama: Vec<&Job> = jobs.iter().filter(|j| matches!(j.source, Source::Llama { .. }) && !j.item.present).collect();
    if !llama.is_empty() {
        let name_of = |file: &str| {
            llama
                .iter()
                .find(|j| matches!(&j.source, Source::Llama { file: f } if f == file))
                .map(|j| j.item.name.clone())
        };
        install_llama_with(root, manifest, backend, opts, &mut |file, p| {
            if let Some(name) = name_of(file) {
                let ev = locked(&tracker).update(&name, p);
                send(ev, false);
            }
        })
        .map_err(err)?;
        for j in &llama {
            let ev = locked(&tracker).finish(&j.item.name);
            send(ev, true);
        }
    }
    let dir = embedder::models_dir(root);
    for j in &jobs {
        let Source::Model(f) = &j.source else { continue };
        if j.item.present {
            continue;
        }
        let name = j.item.name.clone();
        fetch_with(&f.urls(), &dir.join(f.file_name()), &f.sha256, opts, &mut |p| {
            let ev = locked(&tracker).update(&name, p);
            send(ev, false);
        })
        .map_err(err)?;
        let ev = locked(&tracker).finish(&name);
        send(ev, true);
    }
    Ok(())
}

// ---------------------------------------------------------------- Tauri state + commands

/// Tauri managed state: one download at a time, with a cancel flag.
#[derive(Default)]
pub struct Models {
    running: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    detected: Arc<Mutex<Option<Detected>>>,
}

fn detect_cached(slot: &Mutex<Option<Detected>>) -> Detected {
    locked(slot).get_or_insert_with(detect_backend).clone()
}

/// Clears the running flag even if the download thread panics.
struct RunGuard(Arc<AtomicBool>);
impl Drop for RunGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn models_plan(m: State<'_, Models>) -> Result<Plan, String> {
    let (slot, running) = (m.detected.clone(), m.running.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let root = tier1::data_root().ok_or("LOCALAPPDATA is not set")?;
        Ok(build_plan(&root, &Manifest::embedded(), &detect_cached(&slot), running.load(Ordering::SeqCst)))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn models_download(app: AppHandle, m: State<'_, Models>, h: State<'_, tier1::Handle>) -> Result<(), String> {
    let root = tier1::data_root().ok_or("LOCALAPPDATA is not set")?;
    if m.running.swap(true, Ordering::SeqCst) {
        return Err("A download is already running".into());
    }
    m.cancel.store(false, Ordering::SeqCst);
    let guard = RunGuard(m.running.clone());
    let (cancel, slot) = (m.cancel.clone(), m.detected.clone());
    let worker = h.get().ok();
    let spawned = std::thread::Builder::new().name("models-download".into()).spawn(move || {
        let backend = detect_cached(&slot).backend;
        let opts = FetchOptions { cancel: Some(cancel), ..FetchOptions::default() };
        let r = run_download(&root, &Manifest::embedded(), backend, &opts, EMIT_EVERY, &mut |ev| {
            let _ = app.emit("models-progress", ev);
        });
        if r.is_ok() {
            if let Some(t) = &worker {
                t.kick(); // leave models_missing now instead of at the next 30 s poll
            }
        }
        drop(guard);
        let _ = app.emit("models-done", DoneEvent { ok: r.is_ok(), error: r.err() });
    });
    spawned.map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn models_cancel(m: State<'_, Models>) {
    m.cancel.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use winseek_llama::install::{find_server_exe, install_dir};
    use winseek_llama::manifest::{Asset, LlamaManifest, ModelEntry};
    use winseek_llama::sha256_hex;

    fn det(b: Backend) -> Detected {
        Detected { backend: b, gpu_name: Some("GPU".into()), driver_version: None }
    }

    #[test]
    fn plan_lists_zips_and_models_with_sizes() {
        let d = tempfile::tempdir().unwrap();
        let man = Manifest::embedded();
        let p = build_plan(d.path(), &man, &det(Backend::Cuda12), false);
        let names: Vec<_> = p.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names[0], format!("llama.cpp {} (CUDA 12)", man.llama.build));
        assert_eq!(names[1], "CUDA runtime libraries");
        assert_eq!(&names[2..], ["EmbeddingGemma-2 model", "EmbeddingGemma-2 vision projector"]);
        assert!(p.items.iter().all(|i| !i.present));
        assert_eq!(p.total_bytes_missing, 309_855_456 + 554_821_024);
        assert!(p.size_unknown && !p.downloading);
        assert_eq!(p.backend, Backend::Cuda12);
        let cpu = build_plan(d.path(), &man, &det(Backend::Cpu), true);
        assert_eq!(cpu.items.len(), 3);
        assert!(cpu.downloading);
        assert_eq!(serde_json::to_value(&cpu).unwrap()["backend"], "cpu");
    }

    #[test]
    fn plan_marks_present_files_and_installed_llama() {
        let d = tempfile::tempdir().unwrap();
        let man = Manifest::embedded();
        let models = embedder::models_dir(d.path());
        std::fs::create_dir_all(&models).unwrap();
        let (m, _) = embedder::model_paths(&man, &models).unwrap();
        std::fs::write(m, b"x").unwrap();
        let dir = install_dir(d.path(), &man.llama.build, Backend::Vulkan);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("llama-server.exe"), b"x").unwrap();
        assert!(find_server_exe(&dir).is_some());
        let p = build_plan(d.path(), &man, &det(Backend::Vulkan), false);
        let present: Vec<_> = p.items.iter().map(|i| i.present).collect();
        assert_eq!(present, [true, true, false]);
        assert_eq!(p.total_bytes_missing, 554_821_024);
        assert!(!p.size_unknown);
    }

    #[test]
    fn tracker_overall_counts_only_missing_items() {
        let items = vec![
            PlanItem { name: "zip".into(), bytes: None, present: false },
            PlanItem { name: "have".into(), bytes: Some(500), present: true },
            PlanItem { name: "model".into(), bytes: Some(1000), present: false },
        ];
        let mut t = Tracker::new(&items);
        let e = t.update("zip", Progress { downloaded: 50, total: Some(200) });
        assert_eq!((e.downloaded, e.overall_downloaded, e.overall_total), (50, 50, 1200)); // learned zip size
        let e = t.finish("zip");
        assert_eq!((e.downloaded, e.overall_downloaded), (200, 200));
        let e = t.update("model", Progress { downloaded: 300, total: Some(1000) });
        assert_eq!((e.overall_downloaded, e.overall_total, e.total), (500, 1200, Some(1000)));
        // unknown names are harmless
        assert_eq!(t.update("nope", Progress { downloaded: 1, total: None }).overall_downloaded, 500);
    }

    #[test]
    fn throttle_limits_rate() {
        let mut th = Throttle { every: Duration::from_millis(100), last: None };
        let t0 = Instant::now();
        assert!(th.allow(t0));
        assert!(!th.allow(t0 + Duration::from_millis(50)));
        assert!(th.allow(t0 + Duration::from_millis(100)));
        assert!(!th.allow(t0 + Duration::from_millis(150)));
    }

    /// Loopback server (no internet): serves `files` by path for up to `conns` connections.
    fn serve(files: Vec<(&'static str, Vec<u8>)>, conns: usize) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for _ in 0..conns {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                r.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                match files.iter().find(|(p, _)| *p == path) {
                    Some((_, b)) => {
                        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len());
                        let _ = s.write_all(b);
                    }
                    None => {
                        let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            }
        });
        base
    }

    fn test_manifest(base: &str, a: &[u8], b: &[u8]) -> Manifest {
        let file = |role, name: &str, body: &[u8]| ModelFile {
            role,
            url: format!("{base}/{name}"),
            sha256: sha256_hex(body),
            size: body.len() as u64,
            mirrors: vec![],
        };
        Manifest {
            llama: LlamaManifest { build: "b1".into(), assets: Vec::<Asset>::new() },
            models: vec![ModelEntry {
                id: embedder::MODEL_ID.into(),
                profile: ModelProfile::Default,
                files: vec![file(ModelRole::Model, "m.gguf", a), file(ModelRole::Mmproj, "p.gguf", b)],
            }],
        }
    }

    fn quick(cancel: Option<Arc<AtomicBool>>) -> FetchOptions {
        FetchOptions { backoff_base: Duration::from_millis(1), max_attempts: 1, cancel, ..FetchOptions::default() }
    }

    #[test]
    fn downloads_models_from_loopback_with_progress() {
        let (a, b) = (vec![7u8; 600_000], vec![9u8; 300_000]);
        let base = serve(vec![("/m.gguf", a.clone()), ("/p.gguf", b.clone())], 2);
        let man = test_manifest(&base, &a, &b);
        let d = tempfile::tempdir().unwrap();
        let mut evs = Vec::new();
        run_download(d.path(), &man, Backend::Cpu, &quick(None), Duration::ZERO, &mut |e| evs.push(e)).unwrap();
        let (m, p) = embedder::model_paths(&man, &embedder::models_dir(d.path())).unwrap();
        assert_eq!(std::fs::read(m).unwrap(), a);
        assert_eq!(std::fs::read(p).unwrap(), b);
        let last = evs.last().unwrap();
        assert_eq!((last.overall_downloaded, last.overall_total), (900_000, 900_000));
        assert!(evs.windows(2).all(|w| w[0].overall_downloaded <= w[1].overall_downloaded));
        assert!(evs.iter().any(|e| e.item == "EmbeddingGemma-2 model") && evs.iter().any(|e| e.item.ends_with("projector")));
        // already present -> nothing to do (no server left to answer)
        let mut n = 0;
        run_download(d.path(), &man, Backend::Cpu, &quick(None), Duration::ZERO, &mut |_| n += 1).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn cancel_keeps_part_and_resumes() {
        let (a, b) = (vec![7u8; 900_000], vec![9u8; 10]);
        let base = serve(vec![("/m.gguf", a.clone()), ("/p.gguf", b.clone())], 3);
        let man = test_manifest(&base, &a, &b);
        let d = tempfile::tempdir().unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let f2 = flag.clone();
        let r = run_download(d.path(), &man, Backend::Cpu, &quick(Some(flag.clone())), Duration::ZERO, &mut |e| {
            if e.downloaded > 0 {
                f2.store(true, Ordering::SeqCst);
            }
        });
        assert_eq!(r, Err("cancelled".to_string()));
        let (m, p) = embedder::model_paths(&man, &embedder::models_dir(d.path())).unwrap();
        assert!(!m.exists() && !p.exists());
        let mut part = m.clone().into_os_string();
        part.push(".part");
        assert!(Path::new(&part).is_file());
        // resume: this server ignores Range and restarts, which fetch handles
        flag.store(false, Ordering::SeqCst);
        run_download(d.path(), &man, Backend::Cpu, &quick(Some(flag)), Duration::ZERO, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(m).unwrap(), a);
        assert_eq!(std::fs::read(p).unwrap(), b);
    }

    #[test]
    fn http_failure_is_reported() {
        let base = serve(vec![], 1);
        let man = test_manifest(&base, b"a", b"b");
        let d = tempfile::tempdir().unwrap();
        let r = run_download(d.path(), &man, Backend::Cpu, &quick(None), Duration::ZERO, &mut |_| {});
        assert!(r.unwrap_err().contains("404"));
    }
}
