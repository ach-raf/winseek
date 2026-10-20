//! `winseek-mcp`: MCP stdio shim. Speaks MCP (JSON-RPC 2.0, one JSON message per line) to the
//! agent on stdin/stdout and forwards every request to the winseek app over a private named
//! pipe. Never writes anything but protocol JSON to stdout; diagnostics go to stderr.

use std::io::{BufRead, Write};

mod link;
mod rpc;

use rpc::{Backend, Session};

/// Same string the app's `autostart` module uses (`app/src-tauri/src/autostart.rs`).
pub const AUTOSTART_ARG: &str = "--autostart";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("winseek-mcp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--selftest") {
        std::process::exit(selftest());
    }
    serve();
}

/// stdin line in -> answer line out, until EOF (the client decides the shim's lifetime).
fn serve() {
    let mut session = Session::new();
    let mut backend = match link::Link::new() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("winseek-mcp: cannot derive the agent pipe name ({e}); serving tool-less");
            return;
        }
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("winseek-mcp: stdin error: {e}");
                break;
            }
        };
        if let Some(answer) = rpc::handle_line(&mut session, &line, &mut backend) {
            if writeln!(out, "{answer}").and_then(|_| out.flush()).is_err() {
                break; // the client went away; nothing left to answer
            }
        }
    }
}

/// `--selftest` (PRD RA8): connect (starting the app if needed), show the tool list and one
/// real call. Exit 0/1 for humans and CI.
fn selftest() -> i32 {
    let mut backend = match link::Link::new() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("winseek-mcp: {e}");
            return 1;
        }
    };
    let mut session = Session::new();
    let backend_ref: &mut dyn Backend = &mut backend;
    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {"protocolVersion": rpc::SUPPORTED[0], "clientInfo": {"name": "selftest"}},
    });
    if rpc::handle_line(&mut session, &init.to_string(), backend_ref).is_none() {
        eprintln!("winseek-mcp: initialize got no answer");
        return 1;
    }
    match backend.list() {
        Ok(tools) => {
            let names: Vec<String> = tools.as_array().map(|a| a.iter().filter_map(|t| t["name"].as_str().map(String::from)).collect()).unwrap_or_default();
            println!("tools ({}): {}", names.len(), names.join(", "));
        }
        Err(e) => {
            eprintln!("winseek-mcp: tools/list failed: {e}");
            return 1;
        }
    }
    match backend.call("selftest", "index_status", serde_json::json!({})) {
        Ok(v) => {
            println!("index_status: {}", serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()));
            0
        }
        Err(e) => {
            eprintln!("winseek-mcp: index_status failed: {e}");
            1
        }
    }
}
