//! `llama-server` child process: random port + API key, job-object lifetime, health polling.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::backend::Backend;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Chat / vision parsing (Infinity-Parser2).
    Chat,
    /// Adds `--embeddings` (EmbeddingGemma-2).
    Embedding,
}

#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub role: Role,
    pub health_timeout: Duration,
    /// Where to send the child's stdout+stderr (null if None).
    pub log_file: Option<PathBuf>,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        SpawnConfig { role: Role::Chat, health_timeout: Duration::from_secs(120), log_file: None }
    }
}

pub struct LlamaServer {
    child: Child,
    port: u16,
    api_key: String,
}

/// Build the argument list (pure, testable).
pub fn build_args(
    model: &Path,
    mmproj: Option<&Path>,
    port: u16,
    api_key: &str,
    role: Role,
    extra: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = vec!["-m".into(), model.display().to_string()];
    if let Some(m) = mmproj {
        a.push("--mmproj".into());
        a.push(m.display().to_string());
    }
    a.extend(["--host".into(), "127.0.0.1".into(), "--port".into(), port.to_string()]);
    a.extend(["--api-key".into(), api_key.to_string()]);
    if role == Role::Embedding {
        a.push("--embeddings".into());
        a.extend(
            ["--pooling", "mean", "--image-min-tokens", "280", "--image-max-tokens", "280", "-b", "8192", "-ub", "8192"]
                .map(String::from),
        );
    }
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
    /// Spawn a chat/vision server (`Role::Chat`, default config).
    pub fn spawn(exe: &Path, model: &Path, mmproj: Option<&Path>, extra_args: &[String]) -> Result<LlamaServer> {
        Self::spawn_with(exe, model, mmproj, extra_args, &SpawnConfig::default())
    }

    /// Spawn with `--embeddings`.
    pub fn spawn_embedding(exe: &Path, model: &Path, mmproj: Option<&Path>, extra_args: &[String]) -> Result<LlamaServer> {
        let cfg = SpawnConfig { role: Role::Embedding, ..Default::default() };
        Self::spawn_with(exe, model, mmproj, extra_args, &cfg)
    }

    pub fn spawn_with(
        exe: &Path,
        model: &Path,
        mmproj: Option<&Path>,
        extra_args: &[String],
        cfg: &SpawnConfig,
    ) -> Result<LlamaServer> {
        let port = free_port()?;
        let api_key = random_key()?;
        let args = build_args(model, mmproj, port, &api_key, cfg.role, extra_args);
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

    #[test]
    fn args_chat_and_embedding() {
        let a = build_args(Path::new("m.gguf"), Some(Path::new("p.gguf")), 5555, "KEY", Role::Chat, &["-ngl".into(), "99".into()]);
        assert_eq!(
            a,
            ["-m", "m.gguf", "--mmproj", "p.gguf", "--host", "127.0.0.1", "--port", "5555", "--api-key", "KEY", "-ngl", "99"]
        );
        let e = build_args(Path::new("m.gguf"), None, 1, "K", Role::Embedding, &[]);
        assert!(e.contains(&"--embeddings".to_string()) && !e.contains(&"--mmproj".to_string()));
        assert_eq!(
            e,
            [
                "-m", "m.gguf", "--host", "127.0.0.1", "--port", "1", "--api-key", "K", "--embeddings", "--pooling", "mean",
                "--image-min-tokens", "280", "--image-max-tokens", "280", "-b", "8192", "-ub", "8192"
            ]
        );
        let e2 = build_args(Path::new("m.gguf"), None, 1, "K", Role::Embedding, &["-ngl".into(), "0".into()]);
        assert_eq!(&e2[e2.len() - 2..], ["-ngl", "0"]);
        assert!(!a.contains(&"--pooling".to_string()));
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
