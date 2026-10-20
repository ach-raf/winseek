//! Agent pipe protocol: the `winseek-mcp` shim talks to the running app over this pipe.
//!
//! Same framing and transport as the searchd pipe ([`super::pipe`]); the DACL is tighter —
//! only the current user's SID may open it, so nothing else on the machine (not even SYSTEM)
//! can reach the app through it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    /// Tool definitions (only `index_status` while agent access is off).
    List { id: u64 },
    Call {
        id: u64,
        client: String,
        tool: String,
        args: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentResponse {
    /// MCP `tools` array, ready to forward to the client.
    Tools {
        id: u64,
        tools: serde_json::Value,
    },
    Result {
        id: u64,
        value: serde_json::Value,
    },
    Error {
        id: u64,
        message: String,
    },
}

/// Protected DACL granting only `sid` (a bare string SID — SDDL trustee fields take it
/// without brackets). `P` stops administrators and SYSTEM from opening it.
pub fn agent_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

#[cfg(windows)]
pub use imp::{agent_pipe_name, current_user_sid};

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io;
    use std::ptr::null_mut;

    type Handle = *mut c_void;

    const TOKEN_QUERY: u32 = 8;
    const TOKEN_USER: i32 = 1;

    #[repr(C)]
    struct SidAndAttributes {
        sid: *mut c_void,
        attributes: u32,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn GetTokenInformation(
            token: Handle,
            class: i32,
            info: *mut c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
        fn ConvertSidToStringSidW(sid: *mut c_void, string: *mut *mut u16) -> i32;
        fn LocalFree(p: *mut c_void) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn CloseHandle(h: Handle) -> i32;
    }

    /// The current user's SID as a string (`S-1-5-21-…`), used to scope the agent pipe name
    /// (one pipe per Windows account, like Chromium's DevTools pipes).
    pub fn current_user_sid() -> io::Result<String> {
        // SAFETY: token is opened, queried and closed here; ConvertSidToStringSidW's buffer
        // is owned by us and freed with LocalFree before returning.
        unsafe {
            let mut token = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let sid = (|| {
                let mut len = 0u32;
                // First call with a null buffer reports the needed size.
                let _ = GetTokenInformation(token, TOKEN_USER, null_mut(), 0, &mut len);
                let mut buf = vec![0u8; len as usize];
                if GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr().cast(), len, &mut len)
                    == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let user: SidAndAttributes = buf.as_ptr().cast::<SidAndAttributes>().read();
                let mut str = null_mut::<u16>();
                if ConvertSidToStringSidW(user.sid, &mut str) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut n = 0usize;
                while *str.add(n) != 0 {
                    n += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(str, n));
                LocalFree(str.cast());
                Ok(s)
            })();
            CloseHandle(token);
            sid
        }
    }

    /// `\\.\pipe\winseek-agent-<SID>`: the shim and the app derive the same name without a
    /// config file, and two Windows accounts never collide.
    pub fn agent_pipe_name() -> io::Result<String> {
        Ok(format!(r"\\.\pipe\winseek-agent-{}", current_user_sid()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::{connect, PipeServer};
    use crate::{read_message, write_message};
    use std::time::Duration;

    #[test]
    fn request_roundtrip() {
        let call = AgentRequest::Call {
            id: 7,
            client: "claude-code".into(),
            tool: "search_files".into(),
            args: serde_json::json!({"query": "invoice"}),
        };
        let json = serde_json::to_string(&call).unwrap();
        assert!(json.contains(r#""type":"call""#));
        assert_eq!(serde_json::from_str::<AgentRequest>(&json).unwrap(), call);

        let tools = AgentResponse::Tools {
            id: 3,
            tools: serde_json::json!([]),
        };
        let json = serde_json::to_string(&tools).unwrap();
        assert!(json.contains(r#""type":"tools""#));
        assert_eq!(serde_json::from_str::<AgentResponse>(&json).unwrap(), tools);
    }

    #[cfg(windows)]
    #[test]
    fn sid_is_current_user() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "got {sid}");
        assert!(!sid.contains(' '));
    }

    #[cfg(windows)]
    #[test]
    fn pipe_name_has_no_spaces() {
        let name = agent_pipe_name().unwrap();
        assert!(name.starts_with(r"\\.\pipe\winseek-agent-"));
        assert!(!name.contains(' '));
    }

    #[cfg(windows)]
    #[test]
    fn request_roundtrip_over_agent_pipe() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(r"\\.\pipe\winseek-agent-test-{t}");
        // The agent SDDL must still admit the process's own user (the test is that user).
        let sddl = agent_sddl(&current_user_sid().unwrap());
        assert!(sddl.contains("GA"));
        let mut server = PipeServer::bind_with_sddl(&name, &sddl).expect("bind");
        let t = std::thread::spawn(move || {
            let mut c = server.accept().unwrap();
            let req: AgentRequest = read_message(&mut c).unwrap();
            let resp = match req {
                AgentRequest::List { id } => AgentResponse::Tools {
                    id,
                    tools: serde_json::json!([]),
                },
                other => AgentResponse::Error {
                    id: match other {
                        AgentRequest::List { id, .. } => id,
                        _ => 0,
                    },
                    message: "unsupported".into(),
                },
            };
            write_message(&mut c, &resp).unwrap();
        });
        let mut c = connect(&name, Duration::from_secs(5)).unwrap();
        let req = AgentRequest::List { id: 11 };
        write_message(&mut c, &req).unwrap();
        let resp: AgentResponse = read_message(&mut c).unwrap();
        assert_eq!(
            resp,
            AgentResponse::Tools {
                id: 11,
                tools: serde_json::json!([])
            }
        );
        t.join().unwrap();
    }
}
