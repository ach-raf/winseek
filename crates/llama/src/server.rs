//! `llama-server` child process: random port + API key, job-object lifetime, health polling.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::backend::Backend;
use crate::{Error, Result};

/// What a server is for. Embedding roles pin `-np 1` and every role pins an explicit `-c`
/// (llama-server otherwise defaults to 4 slots and the model's full trained context, which doubled
/// the VRAM; PLAN "VRAM budget"). The parser serves several pages at once — see [`parser_slots`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Text-only EmbeddingGemma-2 for search queries: no mmproj, CPU, ~0 VRAM. Capped at 4 threads:
    /// a query is ~30 ms either way, and uncapped it took every core whenever it was handed bulk work.
    EmbedQuery,
    /// EmbeddingGemma-2 with the mmproj for image/document indexing: GPU, ubatch >= 768.
    EmbedIndex,
    /// Chat / vision parsing (Infinity-Parser2).
    Parser,
}

/// Resource profile (PLAN section 7). Only `Low` changes the argument lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// Minimise VRAM: the index server runs on CPU, the parser's mmproj stays off the GPU.
    Low,
    /// Default: the query/index split, GPU servers loaded one at a time and unloaded when idle.
    #[default]
    Balanced,
    /// Like Balanced, but the GPU servers may stay resident while the app is open.
    Fast,
}

impl Profile {
    pub fn low_vram(self) -> bool {
        self == Profile::Low
    }

    /// The stored / UI name: `low`, `balanced` or `fast`.
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Low => "low",
            Profile::Balanced => "balanced",
            Profile::Fast => "fast",
        }
    }

    pub fn parse(s: &str) -> Option<Profile> {
        match s {
            "low" => Some(Profile::Low),
            "balanced" => Some(Profile::Balanced),
            "fast" => Some(Profile::Fast),
            _ => None,
        }
    }
}

/// Context kept per parser slot: a 1024 px page is a few thousand tokens in and out.
pub const PARSER_CTX_PER_SLOT: u32 = 8192;

/// Parallel pages one parser server decodes at once, per profile. Decode is memory-bandwidth
/// bound, so concurrent slots raise aggregate throughput roughly with the slot count until the
/// GPU saturates; Low keeps the single slot (VRAM first).
pub fn parser_slots(profile: Profile) -> u32 {
    match profile {
        Profile::Low => 1,
        Profile::Balanced => 2,
        Profile::Fast => 4,
    }
}

#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub role: Role,
    pub profile: Profile,
    pub health_timeout: Duration,
    /// Where to send the child's stdout+stderr (null if None).
    pub log_file: Option<PathBuf>,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        SpawnConfig { role: Role::Parser, profile: Profile::Balanced, health_timeout: Duration::from_secs(120), log_file: None }
    }
}

pub struct LlamaServer {
    child: Child,
    port: u16,
    api_key: String,
}

/// The role/profile specific flags (pure). EmbedIndex needs the ubatch to hold the whole image
/// (280 tokens): 512 silently drifts (cos 0.986), so it stays >= 768. The parser's `-c` is the
/// total across its slots (`-np`), so each keeps [`PARSER_CTX_PER_SLOT`].
pub fn role_args(role: Role, profile: Profile) -> Vec<String> {
    let low = profile.low_vram();
    let statics: &[&str] = match role {
        Role::EmbedQuery => &[
            "--embeddings", "--pooling", "mean", "-np", "1", "-c", "512", "-b", "512", "-ub", "512", "-ngl", "0", "--device",
            "none", "-t", "4", "-tb", "4",
        ],
        Role::EmbedIndex => &[
            "--embeddings", "--pooling", "mean", "--image-min-tokens", "280", "--image-max-tokens", "280", "-np", "1", "-c",
            "1024", "-b", "1024", "-ub", "1024", "-ngl", if low { "0" } else { "99" },
        ],
        // The numbers below are per-profile, so they are built as strings after the static flags.
        Role::Parser => {
            let slots = parser_slots(profile);
            let mut p: Vec<String> = [
                "--jinja".into(), "--reasoning-budget".into(), "0".into(), "-np".into(), slots.to_string(),
                "-c".into(), (PARSER_CTX_PER_SLOT * slots).to_string(), "-ctk".into(), "q8_0".into(), "-ctv".into(),
                "q8_0".into(), "-ngl".into(), "99".into(),
            ]
            .into();
            // Low: keep the projector off the GPU too (otherwise it still occupies ~650 MB).
            if low {
                p.push("--no-mmproj-offload".into());
            }
            return p;
        }
    };
    let mut a: Vec<String> = statics.iter().map(|s| s.to_string()).collect();
    // Low: keep the projector off the GPU too (otherwise it still occupies ~650 MB).
    if low && matches!(role, Role::EmbedIndex) {
        a.push("--no-mmproj-offload".into());
    }
    a
}

/// Build the argument list (pure, testable). `EmbedQuery` never gets an mmproj.
pub fn build_args(
    model: &Path,
    mmproj: Option<&Path>,
    port: u16,
    api_key: &str,
    role: Role,
    profile: Profile,
    extra: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = vec!["-m".into(), model.display().to_string()];
    if let Some(m) = mmproj.filter(|_| role != Role::EmbedQuery) {
        a.push("--mmproj".into());
        a.push(m.display().to_string());
    }
    a.extend(["--host".into(), "127.0.0.1".into(), "--port".into(), port.to_string()]);
    a.extend(["--api-key".into(), api_key.to_string()]);
    a.extend(role_args(role, profile));
    a.extend(extra.iter().cloned());
    a
}

/// Ask the OS for a free loopback port (small race before the child binds it).
pub fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// 32 random bytes from the OS CSPRNG, hex encoded.
pub fn random_key() -> std::io::Result<String> {
    let mut b = [0u8; 32];
    os_random(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

#[cfg(windows)]
fn os_random(buf: &mut [u8]) -> std::io::Result<()> {
    #[link(name = "advapi32")]
    extern "system" {
        // RtlGenRandom
        fn SystemFunction036(buf: *mut u8, len: u32) -> u8;
    }
    // SAFETY: valid buffer of given length.
    if unsafe { SystemFunction036(buf.as_mut_ptr(), buf.len() as u32) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn os_random(buf: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}

#[cfg(windows)]
mod job {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::sync::OnceLock;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimit {
        per_process_user_time: i64,
        per_job_user_time: i64,
        limit_flags: u32,
        min_ws: usize,
        max_ws: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct IoCounters([u64; 6]);
    #[repr(C)]
    #[derive(Default)]
    struct ExtLimit {
        basic: BasicLimit,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process: usize,
        peak_job: usize,
    }

    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(job: *mut c_void, class: i32, info: *mut c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    }

    /// One process-wide job; its handle is never closed, so when the app exits
    /// (or crashes) the OS closes it and kills every assigned child.
    fn global_job() -> Option<usize> {
        static JOB: OnceLock<Option<usize>> = OnceLock::new();
        *JOB.get_or_init(|| {
            // SAFETY: plain Win32 calls with valid arguments.
            unsafe {
                let h = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
                if h.is_null() {
                    return None;
                }
                let mut info = ExtLimit::default();
                info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    h,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                    &mut info as *mut _ as *mut c_void,
                    std::mem::size_of::<ExtLimit>() as u32,
                );
                if ok == 0 {
                    return None;
                }
                Some(h as usize)
            }
        })
    }

    /// Returns false if the job could not be set up / assigned.
    pub fn assign(child: &Child) -> bool {
        let Some(j) = global_job() else { return false };
        // SAFETY: both handles valid; child handle owned by `Child`.
        unsafe { AssignProcessToJobObject(j as *mut c_void, child.as_raw_handle()) != 0 }
    }
}

#[cfg(not(windows))]
mod job {
    pub fn assign(_: &std::process::Child) -> bool {
        false
    }
}

/// Poll `GET {base}/health` until 200, `alive()` turns false, or the timeout passes.
pub fn wait_healthy(
    base_url: &str,
    api_key: &str,
    timeout: Duration,
    alive: &mut dyn FnMut() -> bool,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(2)).build();
    loop {
        if !alive() {
            return Err(Error::Spawn("llama-server exited before becoming healthy".into()));
        }
        let r = agent
            .get(&format!("{base_url}/health"))
            .set("Authorization", &format!("Bearer {api_key}"))
            .call();
        if let Ok(resp) = r {
            if resp.status() == 200 {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::HealthTimeout);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

impl LlamaServer {
    /// Spawn a parser server (`Role::Parser`, default config).
    pub fn spawn(exe: &Path, model: &Path, mmproj: Option<&Path>, extra_args: &[String]) -> Result<LlamaServer> {
        Self::spawn_with(exe, model, mmproj, extra_args, &SpawnConfig::default())
    }

    pub fn spawn_with(
        exe: &Path,
        model: &Path,
        mmproj: Option<&Path>,
        extra_args: &[String],
        cfg: &SpawnConfig,
    ) -> Result<LlamaServer> {
        if cfg.role == Role::EmbedIndex && mmproj.is_none() {
            return Err(Error::Spawn("the embedding index server needs the mmproj".into()));
        }
        let port = free_port()?;
        let api_key = random_key()?;
        let args = build_args(model, mmproj, port, &api_key, cfg.role, cfg.profile, extra_args);
        let mut cmd = Command::new(exe);
        cmd.args(&args).stdin(Stdio::null());
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir); // DLLs sit next to the exe
        }
        match &cfg.log_file {
            Some(p) => {
                let f = std::fs::File::create(p)?;
                cmd.stdout(f.try_clone()?).stderr(f);
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = cmd.spawn().map_err(|e| Error::Spawn(format!("{}: {e}", exe.display())))?;
        // Drop-kills the child on every error path below.
        let mut srv = LlamaServer { child, port, api_key };
        if !job::assign(&srv.child) {
            // Non-fatal (e.g. already inside a restrictive job); Drop still kills.
        }
        let base = srv.base_url();
        let key = srv.api_key.clone();
        let child = &mut srv.child;
        wait_healthy(&base, &key, cfg.health_timeout, &mut || matches!(child.try_wait(), Ok(None)))?;
        Ok(srv)
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Preferred order starting from the detected backend: Cuda -> Vulkan -> Cpu.
pub fn fallback_order(start: Backend) -> Vec<Backend> {
    match start {
        Backend::Cuda12 => vec![Backend::Cuda12, Backend::Vulkan, Backend::Cpu],
        Backend::Vulkan => vec![Backend::Vulkan, Backend::Cpu],
        Backend::Cpu => vec![Backend::Cpu],
    }
}

/// Try `attempt` for each backend in order; return the first success with its backend.
/// `attempt` typically resolves/installs the exe for that backend and spawns it.
/// On total failure the error lists every backend's failure.
pub fn fallback_chain<T>(
    order: &[Backend],
    mut attempt: impl FnMut(Backend) -> Result<T>,
) -> Result<(Backend, T)> {
    let mut errs = Vec::new();
    for &b in order {
        match attempt(b) {
            Ok(v) => return Ok((b, v)),
            Err(e) => errs.push(format!("{}: {e}", b.dir_name())),
        }
    }
    Err(Error::Spawn(format!("all backends failed [{}]", errs.join("; "))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    const ALL: [Profile; 3] = [Profile::Low, Profile::Balanced, Profile::Fast];

    fn args(role: Role, profile: Profile, mmproj: bool) -> Vec<String> {
        build_args(Path::new("m.gguf"), mmproj.then(|| Path::new("p.gguf")), 5555, "KEY", role, profile, &[])
    }

    /// Expected full list: model, optional mmproj, fixed host/port/key, then `tail`.
    fn want(mmproj: bool, tail: &[&str]) -> Vec<String> {
        let mut v = vec!["-m", "m.gguf"];
        if mmproj {
            v.extend(["--mmproj", "p.gguf"]);
        }
        v.extend(["--host", "127.0.0.1", "--port", "5555", "--api-key", "KEY"]);
        v.extend(tail);
        v.into_iter().map(String::from).collect()
    }

    #[test]
    fn embed_query_is_text_only_cpu() {
        let tail = [
            "--embeddings", "--pooling", "mean", "-np", "1", "-c", "512", "-b", "512", "-ub", "512", "-ngl", "0", "--device", "none", "-t",
            "4", "-tb", "4",
        ];
        // An mmproj is ignored, in every profile.
        for p in ALL {
            assert_eq!(args(Role::EmbedQuery, p, true), want(false, &tail));
            assert_eq!(args(Role::EmbedQuery, p, false), want(false, &tail));
        }
    }

    #[test]
    fn embed_index_gpu_and_low_cpu() {
        let tail = |ngl: &'static str| {
            vec![
                "--embeddings", "--pooling", "mean", "--image-min-tokens", "280", "--image-max-tokens", "280", "-np", "1", "-c",
                "1024", "-b", "1024", "-ub", "1024", "-ngl", ngl,
            ]
        };
        for p in [Profile::Balanced, Profile::Fast] {
            assert_eq!(args(Role::EmbedIndex, p, true), want(true, &tail("99")));
        }
        let mut low = tail("0");
        low.push("--no-mmproj-offload");
        assert_eq!(args(Role::EmbedIndex, Profile::Low, true), want(true, &low));
    }

    #[test]
    fn parser_args_scale_slots_with_the_profile() {
        for (profile, slots) in [(Profile::Low, 1u32), (Profile::Balanced, 2), (Profile::Fast, 4)] {
            let (np, ctx) = (slots.to_string(), (PARSER_CTX_PER_SLOT * slots).to_string());
            let mut tail: Vec<&str> =
                vec!["--jinja", "--reasoning-budget", "0", "-np", &np, "-c", &ctx, "-ctk", "q8_0", "-ctv", "q8_0", "-ngl", "99"];
            if profile.low_vram() {
                tail.push("--no-mmproj-offload");
            }
            assert_eq!(args(Role::Parser, profile, true), want(true, &tail), "{profile:?}");
        }
    }

    #[test]
    fn extra_args_come_last() {
        let e = build_args(Path::new("m"), None, 1, "K", Role::EmbedIndex, Profile::Balanced, &["-ngl".into(), "0".into()]);
        assert_eq!(&e[e.len() - 2..], ["-ngl", "0"]);
    }

    #[test]
    fn embedding_roles_pin_one_slot_and_context_and_index_ubatch_holds_the_image() {
        let val = |a: &[String], flag: &str| -> Option<String> { a.iter().position(|x| x == flag).and_then(|i| a.get(i + 1)).cloned() };
        for role in [Role::EmbedQuery, Role::EmbedIndex] {
            for p in ALL {
                let a = args(role, p, true);
                assert_eq!(val(&a, "-np").as_deref(), Some("1"), "{role:?} {p:?}");
                assert!(val(&a, "-c").and_then(|c| c.parse::<u32>().ok()).is_some(), "{role:?} {p:?} has no -c");
                assert!(val(&a, "-ngl").is_some(), "{role:?} {p:?} has no -ngl");
            }
        }
        for p in ALL {
            let a = args(Role::EmbedIndex, p, true);
            let n = |f: &str| val(&a, f).unwrap().parse::<u32>().unwrap();
            let (ub, b, c) = (n("-ub"), n("-b"), n("-c"));
            assert!(ub >= 768 && b >= ub && c >= ub, "{p:?}: -c {c} -b {b} -ub {ub}");
        }
    }

    #[test]
    fn embed_index_without_mmproj_is_refused_before_spawning() {
        let cfg = SpawnConfig { role: Role::EmbedIndex, ..Default::default() };
        let e = LlamaServer::spawn_with(Path::new("does-not-exist.exe"), Path::new("m.gguf"), None, &[], &cfg).err().unwrap();
        assert!(e.to_string().contains("mmproj"), "{e}");
    }

    #[test]
    fn profile_names_round_trip() {
        for p in ALL {
            assert_eq!(Profile::parse(p.as_str()), Some(p));
        }
        assert_eq!(Profile::parse("turbo"), None);
        assert_eq!(Profile::parse("Balanced"), None);
        assert_eq!(Profile::default(), Profile::Balanced);
    }

    #[test]
    fn port_and_key() {
        let p = free_port().unwrap();
        assert!(p > 0);
        let (k1, k2) = (random_key().unwrap(), random_key().unwrap());
        assert_eq!(k1.len(), 64);
        assert_ne!(k1, k2);
    }

    #[test]
    fn fallback_orders() {
        assert_eq!(fallback_order(Backend::Cuda12), [Backend::Cuda12, Backend::Vulkan, Backend::Cpu]);
        assert_eq!(fallback_order(Backend::Cpu), [Backend::Cpu]);
    }

    #[test]
    fn chain_falls_through() {
        let mut tried = Vec::new();
        let r = fallback_chain(&fallback_order(Backend::Cuda12), |b| {
            tried.push(b);
            if b == Backend::Cpu {
                Ok(42)
            } else {
                Err(Error::HealthTimeout)
            }
        })
        .unwrap();
        assert_eq!(r, (Backend::Cpu, 42));
        assert_eq!(tried, [Backend::Cuda12, Backend::Vulkan, Backend::Cpu]);
        let e = fallback_chain::<()>(&[Backend::Vulkan], |_| Err(Error::HealthTimeout)).unwrap_err();
        assert!(e.to_string().contains("vulkan"));
    }

    #[test]
    fn health_polling_503_then_200_with_auth() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let t = std::thread::spawn(move || {
            let mut auth_ok = true;
            for i in 0..3 {
                let (mut s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if line.to_ascii_lowercase().starts_with("authorization:") {
                        auth_ok &= line.contains("Bearer sekret");
                    }
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let status = if i < 2 { "503 Service Unavailable" } else { "200 OK" };
                s.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
            }
            auth_ok
        });
        wait_healthy(&base, "sekret", Duration::from_secs(10), &mut || true).unwrap();
        assert!(t.join().unwrap());
    }

    #[test]
    fn health_stops_when_child_dies_or_times_out() {
        let e = wait_healthy("http://127.0.0.1:1", "k", Duration::from_secs(5), &mut || false).unwrap_err();
        assert!(matches!(e, Error::Spawn(_)));
        let e = wait_healthy("http://127.0.0.1:1", "k", Duration::from_millis(300), &mut || true).unwrap_err();
        assert!(matches!(e, Error::HealthTimeout));
    }

    #[cfg(windows)]
    #[test]
    fn child_that_exits_is_reported() {
        // cmd.exe given llama-style args with stdin=null exits at once.
        let exe = PathBuf::from(r"C:\Windows\System32\cmd.exe");
        let cfg = SpawnConfig { health_timeout: Duration::from_secs(10), ..Default::default() };
        let e = LlamaServer::spawn_with(&exe, Path::new("m.gguf"), None, &[], &cfg).err().unwrap();
        assert!(matches!(e, Error::Spawn(_)), "{e}");
    }

    #[cfg(windows)]
    #[test]
    fn job_assignment_and_kill() {
        // `ping -n 30` runs ~30s; just verify job assignment succeeds and kill works.
        let mut c = Command::new(r"C:\Windows\System32\ping.exe")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        // May be false only if the test runner itself sits in a non-breakaway job on old Windows.
        let assigned = job::assign(&c);
        c.kill().unwrap();
        c.wait().unwrap();
        assert!(assigned, "AssignProcessToJobObject failed");
    }
}
