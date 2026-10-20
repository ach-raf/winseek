//! The agent pipe: accepts `winseek-mcp` connections on `\\.\pipe\winseek-agent-<SID>` and
//! answers AgentRequest frames with read-only tool calls. The DACL admits only the current
//! user; the enabled flag is read per request so the Settings toggle acts without a restart.

use tauri::{AppHandle, Manager};
use winseek_ipc::pipe::PipeServer;
use winseek_ipc::{agent_pipe_name, agent_sddl, current_user_sid, read_message, write_message, AgentRequest, AgentResponse};

use serde_json::Value;

/// Start the accept loop on its own thread. Bind failure (another winseek instance owns the
/// pipe) is logged and given up on: that instance serves the pipe instead.
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || serve(app));
}

fn serve(app: AppHandle) {
    let sid = match current_user_sid() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("winseek: agent pipe not started (cannot get the user SID: {e})");
            return;
        }
    };
    let name = match agent_pipe_name() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("winseek: agent pipe not started ({e})");
            return;
        }
    };
    let mut server = match PipeServer::bind_with_sddl(&name, &agent_sddl(&sid)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("winseek: agent pipe not started ({e}); another winseek instance may own it");
            return;
        }
    };
    eprintln!("winseek: agent pipe listening ({name})");
    loop {
        match server.accept() {
            Ok(mut conn) => {
                let app = app.clone();
                std::thread::spawn(move || {
                    // One connection per shim; requests are sequential (PRD A3).
                    while let Ok(req) = read_message(&mut conn) {
                        let enabled = app.state::<std::sync::Arc<super::AgentState>>().config().enabled;
                        let resp = answer(req, super::tools::list_for(enabled), |client, tool, args| {
                            super::dispatch::call(&app, client, tool, args)
                        });
                        if write_message(&mut conn, &resp).is_err() {
                            break;
                        }
                    }
                });
            }
            Err(e) => {
                eprintln!("winseek: agent pipe accept failed: {e}");
                return;
            }
        }
    }
}

/// One request -> one response, given the current tools array and the tool runner. Split out
/// from the thread so tests drive it with a stub.
fn answer(
    req: AgentRequest,
    tools: Value,
    dispatch: impl FnOnce(&str, &str, Value) -> Result<Value, String>,
) -> AgentResponse {
    match req {
        AgentRequest::List { id } => AgentResponse::Tools { id, tools },
        AgentRequest::Call { id, client, tool, args } => match dispatch(&client, &tool, args) {
            Ok(value) => AgentResponse::Result { id, value },
            Err(message) => AgentResponse::Error { id, message },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools;
    use winseek_ipc::pipe::connect;
    use std::time::Duration;

    fn unique() -> String {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        format!(r"\\.\pipe\winseek-agent-server-test-{t}")
    }

    #[test]
    fn list_and_call_roundtrip_with_stub_dispatcher() {
        let name = unique();
        let sddl = agent_sddl(&current_user_sid().unwrap());
        let mut server = PipeServer::bind_with_sddl(&name, &sddl).unwrap();
        let tools_value = tools::list_for(true);
        let t = std::thread::spawn(move || {
            let mut conn = server.accept().unwrap();
            for _ in 0..2 {
                let req: AgentRequest = read_message(&mut conn).unwrap();
                let resp = answer(req, tools::list_for(true), |client, tool, _args| {
                    Ok(serde_json::json!({"client": client, "tool": tool}))
                });
                write_message(&mut conn, &resp).unwrap();
            }
            let _ = tools_value; // the real list_for output; the stub echoes tool names instead
        });
        let mut c = connect(&name, Duration::from_secs(5)).unwrap();
        write_message(&mut c, &AgentRequest::List { id: 1 }).unwrap();
        let resp: AgentResponse = read_message(&mut c).unwrap();
        let AgentResponse::Tools { id, tools } = resp else { panic!("tools expected") };
        assert_eq!(id, 1);
        assert_eq!(tools.as_array().unwrap().len(), 11);

        write_message(&mut c, &AgentRequest::Call { id: 2, client: "tester".into(), tool: "search_files".into(), args: serde_json::json!({}) }).unwrap();
        let resp: AgentResponse = read_message(&mut c).unwrap();
        let AgentResponse::Result { id, value } = resp else { panic!("result expected") };
        assert_eq!((id, value["tool"].as_str().unwrap()), (2, "search_files"));
        t.join().unwrap();
    }

    #[test]
    fn dispatch_error_becomes_an_error_response() {
        let resp = answer(
            AgentRequest::Call { id: 9, client: "tester".into(), tool: "recycle".into(), args: serde_json::json!({}) },
            serde_json::json!([]),
            |_, tool, _| Err(format!("unknown tool {tool}")),
        );
        let AgentResponse::Error { id, message } = resp else { panic!("error expected") };
        assert_eq!((id, message.as_str()), (9, "unknown tool recycle"));
    }
}
