//! Audio + video content search.
//!
//! - **Audio**: the soundtrack is decoded once (ffmpeg -> 16 kHz mono s16le in a temp file) and cut
//!   into overlapping ~30 s windows ([`plan_windows`]); each window goes to the multimodal embedder
//!   as a WAV clip (`Embedder::embed_audio`, llama.cpp `input_audio`). There is no ASR model: the
//!   vectors live in the same space as text queries.
//! - **Video**: one frame per shot (ffmpeg `select` on the scene score, plus a frame at the start and
//!   at least one per [`MAX_SHOT_GAP_S`]) embedded like an image, plus the soundtrack windows.
//! - Every vector carries a locator `{"t":<seconds>,...}`; a hit exposes it as `at` and the preview
//!   seeks the `<video>` / `<audio>` element there.
//!
//! ffmpeg is a pinned sidecar (`manifest.json` -> `ffmpeg`, installed under
//! `%LOCALAPPDATA%\winseek\ffmpeg\<build>\ffmpeg.exe`, or `WINSEEK_FFMPEG`). Without it audio / video
//! files stay queued, like PDFs without pdfium. It runs as the user, below normal priority, with no
//! window, and is killed after [`FFMPEG_TIMEOUT`].
//!
//! The pure parts (window plan, WAV header, locators, showinfo parsing, fingerprint) are unit-tested;
//! [`MediaTool`] is the seam the pass tests replace with a fake.

use std::io::{Read, Seek, SeekFrom};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use winseek_llama::install::installed_ffmpeg;
use winseek_llama::prep::prepare_jpeg;
use winseek_llama::Manifest;
use winseek_store::{ChunkVec, PendingFile, Store, VecKind};

use super::crawler::KIND_VIDEO;
use super::embedder::{is_gpu_busy, is_no_audio, EmbedError, Embedder};
use super::guard;

/// `WINSEEK_FFMPEG` overrides the install lookup (dev).
pub const ENV_FFMPEG: &str = "WINSEEK_FFMPEG";

pub const SAMPLE_RATE: u32 = 16_000;
/// 16-bit mono.
const BYTES_PER_SEC: u64 = SAMPLE_RATE as u64 * 2;
/// Audio window length and hop (50 % overlap, so a phrase is whole in at least one window).
pub const WINDOW_S: f64 = 30.0;
pub const HOP_S: f64 = 15.0;
/// A trailing remainder shorter than this is folded into the previous window instead of getting its own.
const MIN_TAIL_S: f64 = 5.0;
/// Windows kept per file (2 h at the 15 s hop); the rest of a longer recording is not indexed.
pub const MAX_WINDOWS: usize = 480;
/// Decoded audio longer than this is cut (ffmpeg `-t`): `MAX_WINDOWS` hops plus one window.
const MAX_AUDIO_S: f64 = HOP_S * MAX_WINDOWS as f64 + WINDOW_S;
/// Frames kept per video.
pub const MAX_FRAMES: usize = 200;
/// Scene-change score (ffmpeg `scene`, 0..1) that starts a new shot.
pub const SHOT_THRESHOLD: f64 = 0.30;
/// Cuts closer together than this are one shot (flashes, fades).
pub const MIN_SHOT_GAP_S: f64 = 2.0;
/// A long shot still gets a frame this often.
pub const MAX_SHOT_GAP_S: f64 = 60.0;
/// Video longer than this is cut (ffmpeg `-t`).
const MAX_VIDEO_S: u32 = 4 * 3600;
/// Long side of a sampled frame (the embedder canonicalises to 768 anyway).
const FRAME_LONG_SIDE: u32 = 768;
/// Windows / frames per embedder request.
const AUDIO_BATCH: usize = 4;
const FRAME_BATCH: usize = 8;
/// A decode that runs longer than this is killed and the file fails.
pub const FFMPEG_TIMEOUT: Duration = Duration::from_secs(45 * 60);
/// Files larger than this are skipped (`too_large`).
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Bytes sampled from the start, middle and end of a file by [`fingerprint`].
const FP_SAMPLE: u64 = 1 << 20;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

// ---------------------------------------------------------------- pure helpers

/// `(start, end)` seconds of the overlapping windows over `duration_s` of audio: [`WINDOW_S`] long,
/// every [`HOP_S`], the last one ending exactly at the end, at most [`MAX_WINDOWS`].
pub fn plan_windows(duration_s: f64) -> Vec<(f64, f64)> {
    if duration_s.is_nan() || duration_s <= 0.5 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0.0;
    loop {
        let end = start + WINDOW_S;
        // the remainder after this window would be a sliver: stretch it to the end and stop
        if end + MIN_TAIL_S >= duration_s {
            out.push((start, duration_s));
            break;
        }
        out.push((start, end));
        if out.len() >= MAX_WINDOWS {
            break;
        }
        start += HOP_S;
    }
    out
}

/// A canonical 44-byte-header WAV (PCM, mono, 16-bit) around `pcm`.
pub fn wav_bytes(pcm: &[u8], rate: u32) -> Vec<u8> {
    let data = pcm.len() as u32;
    let mut w = Vec::with_capacity(44 + pcm.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    w.extend_from_slice(pcm);
    w
}

/// Locator of a soundtrack window: where it starts / ends in seconds.
pub fn window_locator(start: f64, end: f64) -> String {
    serde_json::json!({ "t": round1(start), "end": round1(end), "media": "audio" }).to_string()
}

/// Locator of a video frame.
pub fn frame_locator(t: f64) -> String {
    serde_json::json!({ "t": round1(t), "media": "frame" }).to_string()
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// The time anchor (seconds) of a chunk locator, if it has one.
pub fn locator_time(locator: Option<&str>) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(locator?).ok()?;
    v.get("t")?.as_f64().filter(|t| t.is_finite() && *t >= 0.0)
}

/// `pts_time` of every frame ffmpeg's `showinfo` filter reported on stderr, in order.
pub fn parse_showinfo_times(stderr: &str) -> Vec<f64> {
    stderr
        .lines()
        .filter(|l| l.contains("Parsed_showinfo"))
        .filter_map(|l| {
            let rest = &l[l.find("pts_time:")? + "pts_time:".len()..];
            rest.split_whitespace().next()?.parse::<f64>().ok()
        })
        .collect()
}

/// The `select` expression: the first frame, every scene cut at least [`MIN_SHOT_GAP_S`] after the
/// previous pick, and a frame whenever [`MAX_SHOT_GAP_S`] passed without one.
pub fn select_expr() -> String {
    format!(
        "isnan(prev_selected_t)+gte(t-prev_selected_t\\,{MAX_SHOT_GAP_S})+gt(scene\\,{SHOT_THRESHOLD})*gte(t-prev_selected_t\\,{MIN_SHOT_GAP_S})"
    )
}

/// Cheap content identity for files too big to hash whole: size plus three 1 MiB samples (start,
/// middle, end). Two files that agree on these are the same recording for dedup purposes.
pub fn fingerprint(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut h = Sha256::new();
    h.update(b"winseek-media-v1");
    h.update(len.to_le_bytes());
    let mut buf = vec![0u8; FP_SAMPLE.min(len) as usize];
    for off in [0, len.saturating_sub(FP_SAMPLE) / 2, len.saturating_sub(FP_SAMPLE)] {
        f.seek(SeekFrom::Start(off))?;
        let n = f.read(&mut buf)?;
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ---------------------------------------------------------------- decoding

/// A decoded soundtrack: 16 kHz mono s16le in memory or in a temp file that is deleted on drop.
pub struct Pcm {
    src: PcmSrc,
}

enum PcmSrc {
    Mem(Vec<u8>),
    File { path: PathBuf, len: u64 },
}

impl Pcm {
    pub fn from_bytes(bytes: Vec<u8>) -> Pcm {
        Pcm { src: PcmSrc::Mem(bytes) }
    }

    pub fn seconds(&self) -> f64 {
        let len = match &self.src {
            PcmSrc::Mem(b) => b.len() as u64,
            PcmSrc::File { len, .. } => *len,
        };
        len as f64 / BYTES_PER_SEC as f64
    }

    /// The WAV clip for `[start, end)` seconds.
    pub fn window_wav(&self, start: f64, end: f64) -> std::io::Result<Vec<u8>> {
        let total = (self.seconds() * BYTES_PER_SEC as f64) as u64;
        // sample-align (2 bytes) the byte range
        let a = ((start.max(0.0) * BYTES_PER_SEC as f64) as u64 & !1).min(total);
        let b = ((end.max(0.0) * BYTES_PER_SEC as f64) as u64 & !1).min(total).max(a);
        let pcm = match &self.src {
            PcmSrc::Mem(m) => m[a as usize..b as usize].to_vec(),
            PcmSrc::File { path, .. } => {
                let mut f = std::fs::File::open(path)?;
                f.seek(SeekFrom::Start(a))?;
                let mut v = vec![0u8; (b - a) as usize];
                f.read_exact(&mut v)?;
                v
            }
        };
        Ok(wav_bytes(&pcm, SAMPLE_RATE))
    }
}

impl Drop for Pcm {
    fn drop(&mut self) {
        if let PcmSrc::File { path, .. } = &self.src {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// One sampled video frame.
pub struct Frame {
    pub t: f64,
    pub jpeg: Vec<u8>,
}

/// What the pass needs from ffmpeg. The real one is [`Ffmpeg`]; tests inject a fake.
pub trait MediaTool {
    /// Is the sidecar installed (cheap; called every pass)?
    fn available(&self) -> bool;
    /// The first audio stream as PCM; empty (zero seconds) when the file has none.
    fn decode_audio(&self, path: &Path) -> Result<Pcm, String>;
    /// At most `max` frames, one per shot.
    fn shot_frames(&self, path: &Path, max: usize) -> Result<Vec<Frame>, String>;
}

/// Where ffmpeg is: `WINSEEK_FFMPEG` if it points at a file, else the pinned install.
pub fn locate_ffmpeg_in(env: Option<&std::ffi::OsStr>, root: Option<&Path>, manifest: &Manifest) -> Option<PathBuf> {
    if let Some(p) = env.filter(|e| !e.is_empty()).map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    installed_ffmpeg(root?, manifest.ffmpeg.as_ref()?)
}

/// `%NAME%` references expanded from the process environment (registry PATH values are unexpanded).
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        match tail.find('%') {
            Some(j) if j > 0 => match std::env::var(&tail[..j]) {
                Ok(v) => {
                    out.push_str(&v);
                    rest = &tail[j + 1..];
                }
                Err(_) => {
                    out.push('%');
                    rest = tail;
                }
            },
            _ => {
                out.push('%');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The data of `reg query <key> /v Path`: the value after the type column, `None` when absent.
fn parse_reg_value(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|l| {
        let l = l.trim();
        let (_, after) = l.split_once("REG_EXPAND_SZ").or_else(|| l.split_once("REG_SZ"))?;
        let v = after.trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// PATH as the user's shell would see it: a GUI app started from the tray / autostart inherits the
/// environment of the session's first process, which may predate a later PATH change, so the
/// registry copies (user, then system) are read as well.
fn registry_path_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in [r"HKCU\Environment", r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment"] {
        let res = Command::new("reg")
            .args(["query", key, "/v", "Path"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        if let Some(v) = res.ok().and_then(|o| parse_reg_value(&String::from_utf8_lossy(&o.stdout))) {
            out.extend(expand_env(&v).split(';').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from));
        }
    }
    out
}

/// Places a user's ffmpeg usually is, outside PATH or before PATH was updated: winget (Links and
/// the package folder), scoop, chocolatey, and the folders people unzip to.
fn common_ffmpeg_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut v = Vec::new();
    if let Some(l) = env("LOCALAPPDATA") {
        v.push(l.join("Microsoft").join("WinGet").join("Links"));
        // Packages\Gyan.FFmpeg_...\ffmpeg-<ver>-full_build\bin
        let pk = l.join("Microsoft").join("WinGet").join("Packages");
        for p in std::fs::read_dir(pk).into_iter().flatten().flatten() {
            if p.file_name().to_string_lossy().to_ascii_lowercase().contains("ffmpeg") {
                for b in std::fs::read_dir(p.path()).into_iter().flatten().flatten() {
                    v.push(b.path().join("bin"));
                }
            }
        }
    }
    if let Some(h) = env("USERPROFILE") {
        v.push(h.join("scoop").join("shims"));
        v.push(h.join("scoop").join("apps").join("ffmpeg").join("current").join("bin"));
    }
    if let Some(c) = env("ChocolateyInstall") {
        v.push(c.join("bin"));
    }
    if let Some(d) = env("ProgramData") {
        v.push(d.join("chocolatey").join("bin"));
    }
    for p in [r"C:\ffmpeg\bin", r"C:\ffmpeg", r"C:\tools\ffmpeg\bin", r"C:\tools\ffmpeg", r"C:\Program Files\ffmpeg\bin"] {
        v.push(PathBuf::from(p));
    }
    v
}

/// Candidate `ffmpeg.exe` files in lookup order after the override and our own install: the process
/// PATH, the registry PATH, then the usual install folders. Duplicates dropped.
fn path_candidates(process_path: Option<std::ffi::OsString>, registry: &[PathBuf], common: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = process_path.map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(registry.iter().cloned());
    dirs.extend(common.iter().cloned());
    let mut seen = std::collections::HashSet::new();
    dirs.into_iter()
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join("ffmpeg.exe"))
        .filter(|p| seen.insert(p.to_string_lossy().to_ascii_lowercase()))
        .collect()
}

/// Run a tool for its stdout, giving up after `limit` (a wedged shim must not wedge the worker).
fn stdout_of(exe: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let (exe, args) = (exe.to_path_buf(), args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let o = Command::new(exe).args(args).stdin(Stdio::null()).stderr(Stdio::null()).creation_flags(CREATE_NO_WINDOW).output();
        let _ = tx.send(o.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()));
    });
    rx.recv_timeout(limit).ok().flatten()
}

/// Does `-version` / `-decoders` / `-filters` output describe a build that can do our jobs: it is
/// ffmpeg, decodes the common audio and video codecs, and has the shot-detection filters?
fn build_is_usable(version: &str, decoders: &str, filters: &str) -> bool {
    let word = |text: &str, name: &str| text.lines().any(|l| l.split_whitespace().nth(1) == Some(name));
    version.trim_start().starts_with("ffmpeg version")
        && ["aac", "mp3", "flac", "h264"].iter().all(|d| word(decoders, d))
        && ["select", "scale", "showinfo"].iter().all(|f| word(filters, f))
}

/// Does `exe` run, and can it decode what we feed it? Cached per file (path, size, mtime).
fn ffmpeg_works(exe: &Path) -> bool {
    type Entry = (PathBuf, u64, Option<std::time::SystemTime>, bool);
    static CACHE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
    let Ok(meta) = std::fs::metadata(exe) else { return false };
    let (len, mtime) = (meta.len(), meta.modified().ok());
    {
        let c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = c.iter().find(|r| r.0 == exe && r.1 == len && r.2 == mtime) {
            return r.3;
        }
    }
    let t = Duration::from_secs(20);
    let ok = stdout_of(exe, &["-hide_banner", "-version"], t).is_some_and(|v| {
        let d = stdout_of(exe, &["-hide_banner", "-decoders"], t).unwrap_or_default();
        let f = stdout_of(exe, &["-hide_banner", "-filters"], t).unwrap_or_default();
        build_is_usable(&v, &d, &f)
    });
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).push((exe.to_path_buf(), len, mtime, ok));
    ok
}

static FFMPEG_FOUND: Mutex<Option<(Instant, Option<PathBuf>)>> = Mutex::new(None);

/// Forget the remembered lookup (after our own install finished).
pub fn forget_ffmpeg_lookup() {
    *FFMPEG_FOUND.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A working ffmpeg, looked up in this order: `WINSEEK_FFMPEG`, our pinned install, PATH (process
/// and registry copies), the common install folders. The first candidate that runs and has the
/// decoders / filters wins. The answer is remembered briefly (30 s found, 5 s not found): status
/// polls and every worker pass ask.
pub fn locate_ffmpeg() -> Option<PathBuf> {
    {
        let c = FFMPEG_FOUND.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, found)) = c.as_ref() {
            let ttl = if found.is_some() { 30 } else { 5 };
            if at.elapsed() < Duration::from_secs(ttl) && found.as_ref().is_none_or(|p| p.is_file()) {
                return found.clone();
            }
        }
    }
    let found = locate_ffmpeg_uncached();
    *FFMPEG_FOUND.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), found.clone()));
    found
}

fn locate_ffmpeg_uncached() -> Option<PathBuf> {
    // the override and our own install first; a broken one falls through to the next place
    if let Some(p) = locate_ffmpeg_in(std::env::var_os(ENV_FFMPEG).as_deref(), super::data_root().as_deref(), &Manifest::embedded()) {
        if ffmpeg_works(&p) {
            return Some(p);
        }
    }
    path_candidates(std::env::var_os("PATH"), &registry_path_dirs(), &common_ffmpeg_dirs())
        .into_iter()
        .filter(|p| p.is_file())
        .find(|p| ffmpeg_works(p))
}

/// The real sidecar. The exe is looked up on every call (it may be installed while the app runs).
#[derive(Default)]
pub struct Ffmpeg;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str, ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!("winseek-media-{}-{}-{tag}{ext}", std::process::id(), TEMP_SEQ.fetch_add(1, Ordering::Relaxed)))
}

/// A temp directory removed on drop.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run ffmpeg with `args`, capturing stderr, killing it after `timeout`. `Ok(stderr)` on exit 0.
fn run(exe: &Path, args: &[std::ffi::OsString], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS)
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    // stderr is drained on its own thread: a full pipe would block ffmpeg forever
    let mut err = child.stderr.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = err.read_to_end(&mut s);
        String::from_utf8_lossy(&s).into_owned()
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("ffmpeg: timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("ffmpeg: {e}")),
        }
    };
    let stderr = reader.join().unwrap_or_default();
    if status.success() {
        Ok(stderr)
    } else {
        let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
        Err(format!("ffmpeg: {}", tail.into_iter().rev().collect::<Vec<_>>().join(" | ")))
    }
}

fn os(s: &str) -> std::ffi::OsString {
    s.into()
}

impl MediaTool for Ffmpeg {
    fn available(&self) -> bool {
        locate_ffmpeg().is_some()
    }

    fn decode_audio(&self, path: &Path) -> Result<Pcm, String> {
        let exe = locate_ffmpeg().ok_or("ffmpeg is not installed")?;
        let out = temp_path("a", ".pcm");
        let args = [
            os("-v"),
            os("error"),
            os("-nostdin"),
            os("-i"),
            path.as_os_str().to_owned(),
            os("-map"),
            os("0:a:0?"), // optional: a video without sound is not an error
            os("-vn"),
            os("-t"),
            os(&MAX_AUDIO_S.to_string()),
            os("-ac"),
            os("1"),
            os("-ar"),
            os(&SAMPLE_RATE.to_string()),
            os("-f"),
            os("s16le"),
            os("-y"),
            out.as_os_str().to_owned(),
        ];
        if let Err(e) = run(&exe, &args, FFMPEG_TIMEOUT) {
            let _ = std::fs::remove_file(&out);
            return Err(e);
        }
        let len = std::fs::metadata(&out).map_err(|e| format!("decoded audio: {e}"))?.len();
        Ok(Pcm { src: PcmSrc::File { path: out, len: len & !1 } })
    }

    fn shot_frames(&self, path: &Path, max: usize) -> Result<Vec<Frame>, String> {
        let exe = locate_ffmpeg().ok_or("ffmpeg is not installed")?;
        let dir = TempDir(temp_path("f", ""));
        std::fs::create_dir_all(&dir.0).map_err(|e| format!("temp dir: {e}"))?;
        let long = FRAME_LONG_SIDE;
        let vf = format!("select='{}',scale='if(gt(iw,ih),{long},-2)':'if(gt(iw,ih),-2,{long})',showinfo", select_expr());
        let pattern = dir.0.join("f%05d.jpg");
        let args = [
            os("-v"),
            os("info"), // showinfo logs at info level
            os("-nostdin"),
            os("-i"),
            path.as_os_str().to_owned(),
            os("-an"),
            os("-sn"),
            os("-t"),
            os(&MAX_VIDEO_S.to_string()),
            os("-vf"),
            os(&vf),
            os("-fps_mode"),
            os("vfr"),
            os("-frames:v"),
            os(&max.to_string()),
            os("-q:v"),
            os("3"),
            os("-y"),
            pattern.as_os_str().to_owned(),
        ];
        let stderr = run(&exe, &args, FFMPEG_TIMEOUT)?;
        let times = parse_showinfo_times(&stderr);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir.0)
            .map_err(|e| format!("frames: {e}"))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
            .collect();
        files.sort();
        let mut out = Vec::with_capacity(files.len());
        for (f, t) in files.iter().zip(times) {
            out.push(Frame { t, jpeg: std::fs::read(f).map_err(|e| format!("frame: {e}"))? });
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------- the per-file step

/// What one media file did to the pass.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Stored (done / skipped / failed): count it.
    Handled,
    /// Indexing was paused mid-file; the file stays queued.
    Paused,
    ModelsMissing,
    /// The GPU is held by the parser, or an embed error to retry (strikes already counted).
    Retry(String),
}

fn db(e: winseek_store::Error) -> String {
    e.to_string()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Embed `items` in batches, one request at a time with the lock released in between. A batch that
/// fails is retried one by one so a single bad clip or frame is dropped instead of failing the file.
/// `Err` only for models-missing / paused / a failure of every item.
fn embed_batches<T>(
    embedder: &Mutex<Box<dyn Embedder + Send>>,
    paused: &AtomicBool,
    items: &[T],
    batch: usize,
    call: impl Fn(&mut dyn Embedder, &[T]) -> Result<Vec<Vec<f32>>, EmbedError>,
) -> Result<Vec<Option<Vec<f32>>>, Stop>
where
    T: Clone,
{
    let mut out: Vec<Option<Vec<f32>>> = Vec::with_capacity(items.len());
    let mut last_err = String::new();
    for group in items.chunks(batch) {
        if paused.load(Ordering::Relaxed) {
            return Err(Stop::Paused);
        }
        let r = call(&mut **lock(embedder), group);
        match r {
            Ok(v) if v.len() == group.len() => {
                out.extend(v.into_iter().map(Some));
                continue;
            }
            Err(EmbedError::ModelsMissing) => return Err(Stop::ModelsMissing),
            Err(EmbedError::Other(m)) if is_gpu_busy(&m) || is_no_audio(&m) => return Err(Stop::Other(m)),
            _ => {}
        }
        for one in group {
            match call(&mut **lock(embedder), std::slice::from_ref(one)) {
                Ok(mut v) if v.len() == 1 => out.push(Some(v.remove(0))),
                Err(EmbedError::ModelsMissing) => return Err(Stop::ModelsMissing),
                Err(EmbedError::Other(m)) => {
                    last_err = m;
                    out.push(None);
                }
                Ok(_) => {
                    last_err = "wrong vector count".into();
                    out.push(None);
                }
            }
        }
    }
    if !out.is_empty() && out.iter().all(Option::is_none) {
        return Err(Stop::Other(last_err));
    }
    Ok(out)
}

enum Stop {
    Paused,
    ModelsMissing,
    Other(String),
}

/// Index one audio / video file. Reads nothing into memory but 1 MiB samples and one window at a
/// time; stores the vectors (or skips / fails the file) and says what the pass should do next.
pub fn run_file(
    tool: &dyn MediaTool,
    store: &Mutex<Store>,
    embedder: &Mutex<Box<dyn Embedder + Send>>,
    paused: &AtomicBool,
    f: &PendingFile,
    strikes: &mut std::collections::HashMap<i64, u32>,
    max_strikes: u32,
) -> Result<Step, String> {
    let path = Path::new(&f.path);
    if f.size > MAX_FILE_BYTES {
        lock(store).skip(f.id, "too_large").map_err(db)?;
        return Ok(Step::Handled);
    }
    if f.size == 0 {
        lock(store).skip(f.id, "empty_file").map_err(db)?;
        return Ok(Step::Handled);
    }
    if let Err(e) = guard::check_readable(path) {
        match guard::skip_reason(&e) {
            Some(r) => lock(store).skip(f.id, r).map_err(db)?,
            None => lock(store).fail(f.id, &format!("read: {e}")).map_err(db)?,
        }
        return Ok(Step::Handled);
    }
    let hash = match fingerprint(path) {
        Ok(h) => h,
        Err(e) => {
            lock(store).fail(f.id, &format!("read: {e}")).map_err(db)?;
            return Ok(Step::Handled);
        }
    };
    if lock(store).has_content(&hash).map_err(db)? {
        lock(store).complete(f.id, &hash, &[], &[]).map_err(db)?;
        return Ok(Step::Handled);
    }

    let mut chunks: Vec<ChunkVec> = Vec::new();
    let mut next = 0u32;

    if f.kind == KIND_VIDEO {
        let frames = match tool.shot_frames(path, MAX_FRAMES) {
            Ok(fr) => fr,
            Err(e) => return fail_or_retry(store, f, &e, strikes, max_strikes, false),
        };
        // canonical 768 px JPEGs; a frame that will not decode is dropped
        let ready: Vec<(f64, Vec<u8>)> = frames.into_iter().filter_map(|fr| Some((fr.t, prepare_jpeg(&fr.jpeg).ok()?))).collect();
        let jpegs: Vec<Vec<u8>> = ready.iter().map(|r| r.1.clone()).collect();
        match embed_batches(embedder, paused, &jpegs, FRAME_BATCH, |e, g| e.embed_images(g)) {
            Ok(vecs) => {
                for ((t, _), v) in ready.iter().zip(vecs) {
                    if let Some(vec) = v {
                        chunks.push(ChunkVec { chunk: next, kind: VecKind::Image, locator: Some(frame_locator(*t)), vec });
                        next += 1;
                    }
                }
            }
            Err(s) => return stop_step(store, f, s, strikes, max_strikes),
        }
    }

    // the soundtrack
    let pcm = match tool.decode_audio(path) {
        Ok(p) => p,
        Err(e) => {
            // a video whose frames were stored is still searchable; an audio file with no decode is not
            if f.kind == KIND_VIDEO && !chunks.is_empty() {
                Pcm::from_bytes(Vec::new())
            } else {
                return fail_or_retry(store, f, &e, strikes, max_strikes, false);
            }
        }
    };
    let plan = plan_windows(pcm.seconds());
    let mut wavs: Vec<(f64, f64, Vec<u8>)> = Vec::with_capacity(plan.len().min(AUDIO_BATCH));
    // windows are cut (and embedded) a batch at a time so memory stays at a few clips
    for group in plan.chunks(AUDIO_BATCH) {
        wavs.clear();
        for &(a, b) in group {
            match pcm.window_wav(a, b) {
                Ok(w) => wavs.push((a, b, w)),
                Err(e) => return fail_or_retry(store, f, &format!("audio: {e}"), strikes, max_strikes, false),
            }
        }
        let clips: Vec<Vec<u8>> = wavs.iter().map(|w| w.2.clone()).collect();
        match embed_batches(embedder, paused, &clips, AUDIO_BATCH, |e, g| e.embed_audio(g)) {
            Ok(vecs) => {
                for ((a, b, _), v) in wavs.iter().zip(vecs) {
                    if let Some(vec) = v {
                        chunks.push(ChunkVec { chunk: next, kind: VecKind::Audio, locator: Some(window_locator(*a, *b)), vec });
                        next += 1;
                    }
                }
            }
            // The model has no audio encoder: a video keeps its frames, an audio file has nothing to store.
            Err(Stop::Other(m)) if is_no_audio(&m) => {
                if chunks.is_empty() {
                    lock(store).skip(f.id, "no_audio_model").map_err(db)?;
                    return Ok(Step::Handled);
                }
                break;
            }
            Err(s) => return stop_step(store, f, s, strikes, max_strikes),
        }
    }

    strikes.remove(&f.id.0);
    if chunks.is_empty() {
        lock(store).skip(f.id, "no_media_content").map_err(db)?;
    } else {
        lock(store).complete(f.id, &hash, &chunks, &[]).map_err(db)?;
    }
    Ok(Step::Handled)
}

fn stop_step(
    store: &Mutex<Store>,
    f: &PendingFile,
    s: Stop,
    strikes: &mut std::collections::HashMap<i64, u32>,
    max_strikes: u32,
) -> Result<Step, String> {
    match s {
        Stop::Paused => Ok(Step::Paused),
        Stop::ModelsMissing => Ok(Step::ModelsMissing),
        // The parser holds the GPU: not the file's fault, no strike.
        Stop::Other(m) if is_gpu_busy(&m) => Ok(Step::Retry(m)),
        Stop::Other(m) => fail_or_retry(store, f, &format!("embed: {m}"), strikes, max_strikes, true),
    }
}

/// A failure: count a strike and retry later, or fail the file at `max_strikes`. Decode errors fail
/// at once (`transient` false): the same file will not decode better next time.
fn fail_or_retry(
    store: &Mutex<Store>,
    f: &PendingFile,
    msg: &str,
    strikes: &mut std::collections::HashMap<i64, u32>,
    max_strikes: u32,
    transient: bool,
) -> Result<Step, String> {
    if transient {
        let n = strikes.entry(f.id.0).or_insert(0);
        *n += 1;
        if *n < max_strikes {
            return Ok(Step::Retry(msg.to_string()));
        }
        strikes.remove(&f.id.0);
    }
    guard::fail_file(store, f.id, &f.path, msg)?;
    Ok(Step::Handled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier1::embedder::{index_meta, Result as ER, DIMS};
    use std::collections::HashMap;
    use winseek_store::{FileKey, IndexMeta};

    #[test]
    fn windows_overlap_cover_and_end_at_the_end() {
        assert!(plan_windows(0.0).is_empty() && plan_windows(0.4).is_empty());
        assert_eq!(plan_windows(10.0), [(0.0, 10.0)]);
        assert_eq!(plan_windows(30.0), [(0.0, 30.0)]);
        let w = plan_windows(100.0);
        assert_eq!(w[0], (0.0, 30.0));
        assert_eq!(w[1], (15.0, 45.0), "50 % overlap");
        assert_eq!(w.last().unwrap().1, 100.0, "the last window ends at the end");
        assert!(w.windows(2).all(|p| p[1].0 - p[0].0 == HOP_S || p[1].1 == 100.0));
        // no sliver: 62 s must not end in a window shorter than MIN_TAIL_S
        let w = plan_windows(62.0);
        assert!(w.iter().all(|(a, b)| b - a >= MIN_TAIL_S), "{w:?}");
        assert_eq!(w.last().unwrap().1, 62.0);
        // every second of the recording is inside some window
        for t in 0..100 {
            assert!(plan_windows(100.0).iter().any(|(a, b)| (t as f64) >= *a && (t as f64) < *b), "second {t}");
        }
    }

    #[test]
    fn windows_are_capped() {
        let w = plan_windows(10.0 * 3600.0);
        assert_eq!(w.len(), MAX_WINDOWS);
        assert_eq!(w[1].0, HOP_S);
    }

    #[test]
    fn wav_header_is_canonical() {
        let w = wav_bytes(&[1, 0, 2, 0], 16_000);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 4);
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(w[20..22].try_into().unwrap()), 1, "PCM");
        assert_eq!(u16::from_le_bytes(w[22..24].try_into().unwrap()), 1, "mono");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(w[28..32].try_into().unwrap()), 32_000);
        assert_eq!(&w[36..40], b"data");
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 4);
        assert_eq!(&w[44..], &[1, 0, 2, 0]);
    }

    #[test]
    fn pcm_windows_slice_by_seconds_in_memory_and_on_disk() {
        // 3 s: sample i has value i (mod 256) in the low byte
        let bytes: Vec<u8> = (0..3 * BYTES_PER_SEC as usize).map(|i| (i % 251) as u8).collect();
        let mem = Pcm::from_bytes(bytes.clone());
        assert!((mem.seconds() - 3.0).abs() < 1e-9);
        let w = mem.window_wav(1.0, 2.0).unwrap();
        assert_eq!(w.len(), 44 + BYTES_PER_SEC as usize);
        assert_eq!(&w[44..], &bytes[BYTES_PER_SEC as usize..2 * BYTES_PER_SEC as usize]);
        assert_eq!(mem.window_wav(2.5, 99.0).unwrap().len(), 44 + (BYTES_PER_SEC / 2) as usize, "clamped to the end");

        let p = temp_path("test", ".pcm");
        std::fs::write(&p, &bytes).unwrap();
        let disk = Pcm { src: PcmSrc::File { path: p.clone(), len: bytes.len() as u64 } };
        assert_eq!(disk.window_wav(1.0, 2.0).unwrap(), w);
        drop(disk);
        assert!(!p.exists(), "the temp file goes with the Pcm");
    }

    #[test]
    fn locators_round_trip_and_page_locators_have_no_time() {
        assert_eq!(locator_time(Some(&window_locator(12.34, 42.0))), Some(12.3));
        assert_eq!(locator_time(Some(&frame_locator(0.0))), Some(0.0));
        assert_eq!(locator_time(Some(r#"{"page":3}"#)), None);
        assert_eq!(locator_time(Some(r#"{"t":-1}"#)), None);
        assert_eq!(locator_time(Some("junk")), None);
        assert_eq!(locator_time(None), None);
    }

    #[test]
    fn showinfo_times_are_read_in_order() {
        let log = "Input #0, mov...\n\
[Parsed_showinfo_2 @ 000001] n:   0 pts:      0 pts_time:0       pos:   48 fmt:yuv420p\n\
[Parsed_showinfo_2 @ 000001] n:   1 pts:  90000 pts_time:12.5    pos:  900 fmt:yuv420p\n\
[libx264 @ 000002] pts_time:99 not showinfo\n\
frame=    2 fps=0.0\n";
        assert_eq!(parse_showinfo_times(log), [0.0, 12.5]);
        assert!(parse_showinfo_times("").is_empty());
    }

    #[test]
    fn select_expression_has_the_three_rules() {
        let e = select_expr();
        assert!(e.contains("isnan(prev_selected_t)"), "first frame");
        assert!(e.contains("gt(scene\\,0.3)*gte(t-prev_selected_t\\,2)"), "{e}");
        assert!(e.contains("gte(t-prev_selected_t\\,60)"), "{e}");
    }

    #[test]
    fn fingerprint_follows_content_and_size() {
        let d = tempfile::tempdir().unwrap();
        let w = |n: &str, b: &[u8]| {
            let p = d.path().join(n);
            std::fs::write(&p, b).unwrap();
            p
        };
        let big = vec![7u8; 5 << 20];
        let (a, b) = (w("a", &big), w("b", &big));
        assert_eq!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap(), "same bytes, same identity");
        let mut tail = big.clone();
        *tail.last_mut().unwrap() = 8;
        assert_ne!(fingerprint(&a).unwrap(), fingerprint(&w("c", &tail)).unwrap(), "end sample");
        assert_ne!(fingerprint(&a).unwrap(), fingerprint(&w("d", &big[..big.len() - 1])).unwrap(), "size");
        assert!(fingerprint(&w("e", b"")).is_ok(), "an empty file is fine");
        assert!(fingerprint(&d.path().join("missing")).is_err());
    }

    #[test]
    fn ffmpeg_lookup_prefers_env_then_install() {
        let d = tempfile::tempdir().unwrap();
        let m = Manifest::embedded();
        assert_eq!(locate_ffmpeg_in(None, Some(d.path()), &m), None);
        let f = m.ffmpeg.as_ref().unwrap();
        let exe = winseek_llama::install::ffmpeg_path(d.path(), f);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"x").unwrap();
        assert_eq!(locate_ffmpeg_in(None, Some(d.path()), &m), Some(exe.clone()));
        let own = d.path().join("my-ffmpeg.exe");
        std::fs::write(&own, b"x").unwrap();
        assert_eq!(locate_ffmpeg_in(Some(own.as_os_str()), Some(d.path()), &m), Some(own));
        assert_eq!(locate_ffmpeg_in(Some(d.path().join("nope").as_os_str()), Some(d.path()), &m), Some(exe), "a bad override falls back");
    }

    #[test]
    fn registry_path_values_are_parsed_and_expanded() {
        let out = "\r\nHKEY_CURRENT_USER\\Environment\r\n    Path    REG_EXPAND_SZ    %WS_TEST_HOME%\\bin;C:\\ffmpeg\\bin\r\n\r\n";
        assert_eq!(parse_reg_value(out).as_deref(), Some("%WS_TEST_HOME%\\bin;C:\\ffmpeg\\bin"));
        assert_eq!(parse_reg_value("    Path    REG_SZ    C:\\a").as_deref(), Some("C:\\a"));
        assert_eq!(parse_reg_value("ERROR: The system was unable to find the specified registry key or value."), None);
        std::env::set_var("WS_TEST_HOME", "D:\\h");
        assert_eq!(expand_env("%WS_TEST_HOME%\\bin;%WS_NOPE_X%\\x;100%"), "D:\\h\\bin;%WS_NOPE_X%\\x;100%");
    }

    #[test]
    fn path_candidates_run_process_path_then_registry_then_common_without_duplicates() {
        let process = std::env::join_paths([r"C:\a", r"C:\b"]).unwrap();
        let got = path_candidates(Some(process), &[PathBuf::from(r"C:\B"), PathBuf::from(r"C:\reg")], &[PathBuf::from(r"C:\ffmpeg\bin")]);
        let want: Vec<PathBuf> = [r"C:\a\ffmpeg.exe", r"C:\b\ffmpeg.exe", r"C:\reg\ffmpeg.exe", r"C:\ffmpeg\bin\ffmpeg.exe"].iter().map(PathBuf::from).collect();
        assert_eq!(got, want, "C:\\B is the same folder as C:\\b");
    }

    #[test]
    fn a_build_must_decode_the_common_codecs_and_have_the_shot_filters() {
        let dec = " A....D aac                  AAC\n A....D mp3   MP3\n A....D flac  FLAC\n V....D h264  H.264\n";
        let fil = " T. select  Select frames\n .. scale   Scale\n .. showinfo Show\n";
        assert!(build_is_usable("ffmpeg version 7.1.1-essentials", dec, fil));
        assert!(!build_is_usable("ffprobe version 7", dec, fil), "not ffmpeg");
        assert!(!build_is_usable("ffmpeg version 7", " A....D aac AAC\n", fil), "no mp3 / h264");
        assert!(!build_is_usable("ffmpeg version 7", dec, " .. scale Scale\n"), "no showinfo");
    }

    // ------------------------------------------------------------ run_file with fakes

    struct FakeTool {
        frames: Vec<f64>,
        audio_secs: f64,
        fail_audio: bool,
    }

    impl MediaTool for FakeTool {
        fn available(&self) -> bool {
            true
        }
        fn decode_audio(&self, _: &Path) -> Result<Pcm, String> {
            if self.fail_audio {
                return Err("ffmpeg: boom".into());
            }
            Ok(Pcm::from_bytes(vec![0u8; (self.audio_secs * BYTES_PER_SEC as f64) as usize]))
        }
        fn shot_frames(&self, _: &Path, max: usize) -> Result<Vec<Frame>, String> {
            let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8, y as u8, 9]));
            let mut jpg = Vec::new();
            image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg).unwrap();
            Ok(self.frames.iter().take(max).map(|&t| Frame { t, jpeg: jpg.clone() }).collect())
        }
    }

    /// The i-th frame embeds to axis `FRAME0 + i`, the i-th audio window to axis `AUDIO0 + i`, so a
    /// query along one axis finds exactly one chunk and its locator.
    struct FakeEmbedder {
        audio: bool,
        frames: usize,
        windows: usize,
    }

    const FRAME0: usize = 10;
    const AUDIO0: usize = 100;

    impl FakeEmbedder {
        fn new(audio: bool) -> Self {
            FakeEmbedder { audio, frames: 0, windows: 0 }
        }
    }

    fn axis(i: usize) -> Vec<f32> {
        let mut v = vec![0f32; DIMS];
        v[i] = 1.0;
        v
    }

    impl Embedder for FakeEmbedder {
        fn embed_images(&mut self, j: &[Vec<u8>]) -> ER<Vec<Vec<f32>>> {
            Ok(j
                .iter()
                .map(|_| {
                    self.frames += 1;
                    axis(FRAME0 + self.frames - 1)
                })
                .collect())
        }
        fn embed_audio(&mut self, w: &[Vec<u8>]) -> ER<Vec<Vec<f32>>> {
            if !self.audio {
                return Err(EmbedError::Other(super::super::embedder::NO_AUDIO.into()));
            }
            assert!(w.iter().all(|c| &c[..4] == b"RIFF"), "clips are WAV");
            Ok(w
                .iter()
                .map(|_| {
                    self.windows += 1;
                    axis(AUDIO0 + self.windows - 1)
                })
                .collect())
        }
        fn embed_query(&mut self, _: &str) -> ER<Vec<f32>> {
            Ok(axis(1))
        }
        fn meta(&self) -> IndexMeta {
            index_meta("test")
        }
    }

    fn rig(kind: &str, content: &[u8]) -> (Mutex<Store>, PendingFile, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(if kind == "video" { "clip.mp4" } else { "memo.mp3" });
        std::fs::write(&p, content).unwrap();
        let store = Mutex::new(Store::open_in_memory(&index_meta("test")).unwrap());
        let key = FileKey { vol_guid: "{v}".into(), frn: 1 };
        store.lock().unwrap().upsert_file(&key, &p.to_string_lossy(), content.len() as u64, 1, kind).unwrap();
        let f = store.lock().unwrap().next_pending(5).unwrap().remove(0);
        (store, f, dir)
    }

    fn boxed(e: FakeEmbedder) -> Mutex<Box<dyn Embedder + Send>> {
        Mutex::new(Box::new(e))
    }

    #[test]
    fn video_gets_frame_and_soundtrack_vectors_with_timestamps() {
        let (store, f, _d) = rig("video", b"video-bytes");
        let tool = FakeTool { frames: vec![0.0, 41.5, 90.0], audio_secs: 70.0, fail_audio: false };
        let emb = boxed(FakeEmbedder::new(true));
        let step = run_file(&tool, &store, &emb, &AtomicBool::new(false), &f, &mut HashMap::new(), 3).unwrap();
        assert_eq!(step, Step::Handled);
        let s = store.lock().unwrap();
        assert_eq!(s.stats().unwrap().done, 1);
        // a query along frame i finds that file with the locator of frame i: the hit's timestamp anchor
        for (i, want) in [0.0, 41.5, 90.0].into_iter().enumerate() {
            let h = s.search_vectors(&axis(FRAME0 + i), 5, Some(VecKind::Image)).unwrap();
            assert_eq!(locator_time(h[0].locator.as_deref()), Some(want), "frame {i}");
        }
        let plan = plan_windows(70.0);
        for (i, (start, end)) in plan.iter().enumerate() {
            let h = s.search_vectors(&axis(AUDIO0 + i), 5, Some(VecKind::Audio)).unwrap();
            assert_eq!(locator_time(h[0].locator.as_deref()), Some(*start), "window {i}");
            assert!(h[0].locator.as_deref().unwrap().contains(&format!("\"end\":{end:?}")), "{:?}", h[0].locator);
        }
        assert!(s.search_vectors(&axis(AUDIO0 + plan.len()), 5, Some(VecKind::Audio)).unwrap()[0].score.abs() < 1e-6, "no extra window");
    }

    #[test]
    fn audio_file_is_windows_only_and_identical_content_is_linked() {
        let (store, f, _d) = rig("audio", b"same-bytes");
        let tool = FakeTool { frames: vec![5.0], audio_secs: 40.0, fail_audio: false };
        let emb = boxed(FakeEmbedder::new(true));
        run_file(&tool, &store, &emb, &AtomicBool::new(false), &f, &mut HashMap::new(), 3).unwrap();
        let s = store.lock().unwrap();
        assert!(s.search_vectors(&axis(FRAME0), 10, Some(VecKind::Image)).unwrap().is_empty(), "no frames for audio");
        assert!(!s.search_vectors(&axis(AUDIO0), 10, Some(VecKind::Audio)).unwrap().is_empty());
        assert!(s.has_content(&fingerprint(Path::new(&f.path)).unwrap()).unwrap());
    }

    #[test]
    fn a_model_without_audio_keeps_video_frames_and_skips_audio_files() {
        let (store, f, _d) = rig("video", b"v");
        let tool = FakeTool { frames: vec![0.0], audio_secs: 40.0, fail_audio: false };
        let emb = boxed(FakeEmbedder::new(false));
        assert_eq!(run_file(&tool, &store, &emb, &AtomicBool::new(false), &f, &mut HashMap::new(), 3).unwrap(), Step::Handled);
        {
            let s = store.lock().unwrap();
            assert_eq!(s.stats().unwrap().done, 1, "frames stored");
            assert!(s.search_vectors(&axis(AUDIO0), 5, Some(VecKind::Audio)).unwrap().is_empty());
        }
        let (store, f, _d) = rig("audio", b"a");
        assert_eq!(run_file(&tool, &store, &emb, &AtomicBool::new(false), &f, &mut HashMap::new(), 3).unwrap(), Step::Handled);
        let st = store.lock().unwrap().stats().unwrap();
        assert_eq!((st.skipped, st.done), (1, 0), "nothing to index: skipped, not failed");
    }

    #[test]
    fn decode_failures_fail_the_file_but_a_silent_video_still_gets_frames() {
        let (store, f, _d) = rig("audio", b"x");
        let bad = FakeTool { frames: vec![], audio_secs: 0.0, fail_audio: true };
        let emb = boxed(FakeEmbedder::new(true));
        assert_eq!(run_file(&bad, &store, &emb, &AtomicBool::new(false), &f, &mut HashMap::new(), 3).unwrap(), Step::Handled);
        assert_eq!(store.lock().unwrap().stats().unwrap().failed, 1);

        let (store, f, _d) = rig("video", b"y");
        let silent = FakeTool { frames: vec![1.0, 2.0], audio_secs: 0.0, fail_audio: false };
        run_file(&silent, &store, &emb, &AtomicBool::new(false), &f, &mut HashMap::new(), 3).unwrap();
        assert_eq!(store.lock().unwrap().stats().unwrap().done, 1);
    }

    #[test]
    fn pause_leaves_the_file_queued_and_embed_errors_strike_out() {
        let (store, f, _d) = rig("audio", b"z");
        let tool = FakeTool { frames: vec![], audio_secs: 40.0, fail_audio: false };
        let emb = boxed(FakeEmbedder::new(true));
        assert_eq!(run_file(&tool, &store, &emb, &AtomicBool::new(true), &f, &mut HashMap::new(), 3).unwrap(), Step::Paused);
        assert_eq!(store.lock().unwrap().stats().unwrap().pending, 1);

        struct Down;
        impl Embedder for Down {
            fn embed_images(&mut self, _: &[Vec<u8>]) -> ER<Vec<Vec<f32>>> {
                Err(EmbedError::Other("down".into()))
            }
            fn embed_audio(&mut self, _: &[Vec<u8>]) -> ER<Vec<Vec<f32>>> {
                Err(EmbedError::Other("down".into()))
            }
            fn embed_query(&mut self, _: &str) -> ER<Vec<f32>> {
                Err(EmbedError::Other("down".into()))
            }
            fn meta(&self) -> IndexMeta {
                index_meta("test")
            }
        }
        let down: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Down));
        let mut strikes = HashMap::new();
        for _ in 0..2 {
            assert!(matches!(run_file(&tool, &store, &down, &AtomicBool::new(false), &f, &mut strikes, 3).unwrap(), Step::Retry(_)));
        }
        assert_eq!(run_file(&tool, &store, &down, &AtomicBool::new(false), &f, &mut strikes, 3).unwrap(), Step::Handled);
        assert_eq!(store.lock().unwrap().stats().unwrap().failed, 1, "third strike fails it");
    }

    #[test]
    fn busy_gpu_is_not_a_strike_and_missing_models_are_reported() {
        let (store, f, _d) = rig("audio", b"q");
        let tool = FakeTool { frames: vec![], audio_secs: 40.0, fail_audio: false };
        struct Busy;
        impl Embedder for Busy {
            fn embed_images(&mut self, _: &[Vec<u8>]) -> ER<Vec<Vec<f32>>> {
                Err(EmbedError::ModelsMissing)
            }
            fn embed_audio(&mut self, _: &[Vec<u8>]) -> ER<Vec<Vec<f32>>> {
                Err(EmbedError::Other("GPU is busy with the Parser model; image work waits for it to unload".into()))
            }
            fn embed_query(&mut self, _: &str) -> ER<Vec<f32>> {
                Err(EmbedError::ModelsMissing)
            }
            fn meta(&self) -> IndexMeta {
                index_meta("test")
            }
        }
        let busy: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Busy));
        let mut strikes = HashMap::new();
        for _ in 0..5 {
            assert!(matches!(run_file(&tool, &store, &busy, &AtomicBool::new(false), &f, &mut strikes, 3).unwrap(), Step::Retry(_)));
        }
        assert!(strikes.is_empty() && store.lock().unwrap().stats().unwrap().pending == 1);

        struct Missing;
        impl Embedder for Missing {
            fn embed_images(&mut self, _: &[Vec<u8>]) -> ER<Vec<Vec<f32>>> {
                Err(EmbedError::ModelsMissing)
            }
            fn embed_audio(&mut self, _: &[Vec<u8>]) -> ER<Vec<Vec<f32>>> {
                Err(EmbedError::ModelsMissing)
            }
            fn embed_query(&mut self, _: &str) -> ER<Vec<f32>> {
                Err(EmbedError::ModelsMissing)
            }
            fn meta(&self) -> IndexMeta {
                index_meta("test")
            }
        }
        let gone: Mutex<Box<dyn Embedder + Send>> = Mutex::new(Box::new(Missing));
        assert_eq!(run_file(&tool, &store, &gone, &AtomicBool::new(false), &f, &mut strikes, 3).unwrap(), Step::ModelsMissing);
    }

    /// Needs a real ffmpeg: `WINSEEK_FFMPEG=path cargo test -p winseek-app ffmpeg_live -- --ignored --nocapture`.
    /// Synthesises a 70 s clip (a 440 Hz tone, three colour shots) and checks the real decode path.
    #[test]
    #[ignore = "needs ffmpeg (set WINSEEK_FFMPEG)"]
    fn ffmpeg_live_decodes_audio_and_samples_shots() {
        let exe = locate_ffmpeg().expect("WINSEEK_FFMPEG");
        let d = tempfile::tempdir().unwrap();
        let clip = d.path().join("t.mp4");
        let a = |s: &str| os(s);
        let args = [
            a("-v"), a("error"), a("-f"), a("lavfi"), a("-i"), a("color=c=red:s=320x240:d=20,format=yuv420p"),
            a("-f"), a("lavfi"), a("-i"), a("color=c=blue:s=320x240:d=20,format=yuv420p"),
            a("-f"), a("lavfi"), a("-i"), a("color=c=green:s=320x240:d=30,format=yuv420p"),
            a("-f"), a("lavfi"), a("-i"), a("sine=frequency=440:duration=70"),
            a("-filter_complex"), a("[0:v][1:v][2:v]concat=n=3:v=1:a=0[v]"),
            a("-map"), a("[v]"), a("-map"), a("3:a"), a("-shortest"), a("-y"), clip.as_os_str().to_owned(),
        ];
        run(&exe, &args, Duration::from_secs(120)).expect("synth");
        let tool = Ffmpeg;
        let pcm = tool.decode_audio(&clip).unwrap();
        assert!((pcm.seconds() - 70.0).abs() < 1.0, "{}", pcm.seconds());
        let frames = tool.shot_frames(&clip, MAX_FRAMES).unwrap();
        let ts: Vec<f64> = frames.iter().map(|f| f.t).collect();
        eprintln!("shot times: {ts:?}");
        assert!(frames.len() >= 3 && ts[0] == 0.0 && ts.iter().any(|t| (*t - 20.0).abs() < 1.0), "{ts:?}");
    }
}
