//! Pipe link: implements [`Backend`] over the app's agent pipe. Lazily connects, starts the
//! app when it is not running, and reconnects once when the app restarts mid-session.

use crate::rpc::Backend;
use serde_json::Value;
use std::io;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use winseek_ipc::pipe::{connect, PipeConn};
use winseek_ipc::{read_message, write_message, AgentRequest, AgentResponse};
#[cfg(windows)]
use winseek_ipc::agent_pipe_name;

/// Quick first probe: the common case is the app already running.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
/// The app may take a while to boot (plugins, tray); retry until this window closes.
const START_WINDOW: Duration = Duration::from_secs(15);
const RETRY_STEP: Duration = Duration::from_millis(250);

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

pub struct Link {
    name: String,
    conn: Option<PipeConn>,
    next_id: AtomicU64,
}

impl Link {
    pub fn new() -> io::Result<Self> {
        Ok(Link { name: agent_pipe_name()?, conn: None, next_id: AtomicU64::new(1) })
    }

    #[cfg(test)]
    fn with_name(name: String) -> Self {
        Link { name, conn: None, next_id: AtomicU64::new(1) }
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn ensure_conn(&mut self) -> Result<(), String> {
        if self.conn.is_some() {
            return Ok(());
        }
        if let Ok(c) = connect(&self.name, CONNECT_TIMEOUT) {
            self.conn = Some(c);
            return Ok(());
        }
        eprintln!("winseek-mcp: winseek is not running; starting it in the tray");
        start_app()?;
        let deadline = Instant::now() + START_WINDOW;
        loop {
            match connect(&self.name, RETRY_STEP) {
                Ok(c) => {
                    self.conn = Some(c);
                    return Ok(());
                }
                Err(e) if Instant::now() >= deadline => {
                    return Err(format!("winseek is not running and could not be started ({e})"))
                }
                Err(_) => {}
            }
        }
    }

    fn try_send(&mut self, req: &AgentRequest) -> Result<AgentResponse, String> {
        let c = self.conn.as_mut().ok_or("no pipe connection")?;
        write_message(c, req).map_err(|e| e.to_string())?;
        read_message(c).map_err(|e| e.to_string())
    }
}

impl Default for Link {
    fn default() -> Self {
        Link::new().expect("agent pipe name")
    }
}

impl Backend for Link {
    fn list(&mut self) -> Result<Value, String> {
        let id = self.id();
        self.ensure_conn()?;
        match self.try_send(&AgentRequest::List { id }) {
            Err(first) => self.retry_after_reconnect(&first, &AgentRequest::List { id }),
            Ok(AgentResponse::Tools { tools, .. }) => Ok(tools),
            Ok(AgentResponse::Error { message, .. }) => Err(message),
            Ok(_) => Err("unexpected response to tools/list".into()),
        }
    }

    fn call(&mut self, client: &str, tool: &str, args: Value) -> Result<Value, String> {
        let req = AgentRequest::Call { id: self.id(), client: client.to_owned(), tool: tool.to_owned(), args };
        self.ensure_conn()?;
        match self.try_send(&req) {
            Err(first) => self.retry_after_reconnect(&first, &req),
            Ok(AgentResponse::Result { value, .. }) => Ok(value),
            Ok(AgentResponse::Error { message, .. }) => Err(message),
            Ok(_) => Err("unexpected response to tools/call".into()),
        }
    }
}

impl Link {
    fn retry_after_reconnect(&mut self, first: &str, req: &AgentRequest) -> Result<Value, String> {
        eprintln!("winseek-mcp: pipe request failed ({first}); reconnecting once");
        self.conn = None;
        self.ensure_conn()?;
        match self.try_send(req)? {
            AgentResponse::Tools { tools, .. } => Ok(tools),
            AgentResponse::Result { value, .. } => Ok(value),
            AgentResponse::Error { message, .. } => Err(message),
        }
    }
}

/// Start the app in the tray. MCP clients often run servers in a kill-on-close job object, so
/// the app must break away from it to outlive the client; when the job forbids breakaway we
/// retry without it and say so (the app then closes with the client).
fn start_app() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| format!("where am I: {e}"))?;
    let app = match std::env::var_os("WINSEEK_APP") {
        Some(p) => std::path::PathBuf::from(p),
        None => exe.with_file_name("winseek-app.exe"),
    };
    if !app.exists() {
        return Err("winseek-app.exe not found next to winseek-mcp.exe".into());
    }
    let mut cmd = Command::new(&app);
    cmd.arg(crate::AUTOSTART_ARG).creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    match cmd.spawn() {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            eprintln!("winseek-mcp: could not detach winseek from the client's job object ({e}); winseek will close when the client closes");
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            cmd.spawn().map(|_| ()).map_err(|e| format!("could not start winseek-app.exe: {e}"))
        }
        Err(e) => Err(format!("could not start winseek-app.exe: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winseek_ipc::agent_sddl;
    use winseek_ipc::pipe::PipeServer;
    use winseek_ipc::{current_user_sid, AgentResponse};

    fn unique() -> String {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        format!(r"\\.\pipe\winseek-mcp-test-{t}")
    }

    /// Fake app: answers `n` requests, then either keeps the connection or drops it.
    fn fake_server(name: String, n: usize, drop_after_each: bool) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let sddl = agent_sddl(&current_user_sid().unwrap());
            let mut server = PipeServer::bind_with_sddl(&name, &sddl).unwrap();
            for _ in 0..n {
                let mut c = server.accept().unwrap();
                let req: AgentRequest = read_message(&mut c).unwrap();
                let resp = match req {
                    AgentRequest::List { id } => AgentResponse::Tools { id, tools: serde_json::json!([{"name": "index_status"}]) },
                    AgentRequest::Call { id, tool, .. } => AgentResponse::Result { id, value: serde_json::json!({"echo": tool}) },
                };
                write_message(&mut c, &resp).unwrap();
                if drop_after_each {
                    drop(c); // simulate the app closing right after answering
                }
            }
        })
    }

    #[test]
    fn list_and_call_roundtrip() {
        let name = unique();
        let t = fake_server(name.clone(), 2, false);
        let mut link = Link::with_name(name);
        let tools = link.list().unwrap();
        assert_eq!(tools[0]["name"], "index_status");
        let v = link.call("tester", "index_status", serde_json::json!({})).unwrap();
        assert_eq!(v["echo"], "index_status");
        t.join().unwrap();
    }

    #[test]
    fn reconnects_when_the_app_restarts() {
        // Server 1 answers one request and its connection dies; server 2 takes over.
        let name = unique();
        let t1 = fake_server(name.clone(), 1, true);
        let mut link = Link::with_name(name.clone());
        assert_eq!(link.call("tester", "first", serde_json::json!({})).unwrap()["echo"], "first");
        t1.join().unwrap();
        let t2 = fake_server(name, 1, true);
        assert_eq!(link.call("tester", "second", serde_json::json!({})).unwrap()["echo"], "second");
        t2.join().unwrap();
    }

    #[test]
    fn start_app_errors_without_the_app_exe() {
        // Point WINSEEK_APP at a path that does not exist (dev checkout without a built app).
        let saved = std::env::var_os("WINSEEK_APP");
        std::env::set_var("WINSEEK_APP", "Z:\\definitely-not-here\\winseek-app.exe");
        let err = start_app().expect_err("should fail: no app exe");
        match saved {
            Some(v) => std::env::set_var("WINSEEK_APP", v),
            None => std::env::remove_var("WINSEEK_APP"),
        }
        assert!(err.contains("winseek-app.exe"), "{err}");
    }

    #[test]
    #[ignore] // needs the real app (starts it in the tray); run manually: cargo test -p winseek-mcp -- --ignored
    fn selftest_against_real_app() {
        let mut link = Link::new().unwrap();
        let tools = link.list().unwrap();
        println!("tools: {tools}");
        let status = link.call("selftest", "index_status", serde_json::json!({})).unwrap();
        println!("index_status: {status}");
    }
}
