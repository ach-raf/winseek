//! MCP protocol subset over a [`Backend`], hand-written on serde_json (PRD RA1: no SDK —
//! the full rmcp stack pulls tokio for a ~200-line job). One line of the client's stdin in,
//! zero or one line out (notifications get no answer). This module is pure: no I/O.

use serde_json::{json, Value};

/// Protocol versions winseek speaks; a client asking for another one gets the newest.
pub const SUPPORTED: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

const INSTRUCTIONS: &str = "winseek indexes every file name on this PC (all NTFS drives, updated live) and the contents of the user's chosen folders. Prefer search_files over dir/Get-ChildItem/find for locating files by name anywhere on the machine — it answers in milliseconds. Use search_by_meaning to find documents, screenshots and photos by what they contain. Use read_text to get the text of PDFs and Office files. Call index_status first if a call says access is off.";

/// What the shim needs from the app. Implemented by the pipe link ([`crate::link::Link`]);
/// tests inject fakes.
pub trait Backend {
    /// MCP `tools` array (from the app, so it reflects the access level).
    fn list(&mut self) -> Result<Value, String>;
    fn call(&mut self, client: &str, tool: &str, args: Value) -> Result<Value, String>;
}

pub struct Session {
    /// `initialize.params.clientInfo.name`, for the app's activity log.
    pub client: String,
    /// Negotiated protocol version.
    pub protocol: String,
    /// `notifications/initialized` seen.
    pub initialized: bool,
}

impl Session {
    pub fn new() -> Self {
        Session { client: "unknown".into(), protocol: SUPPORTED[0].into(), initialized: false }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// One input line -> zero or one output line.
pub fn handle_line(s: &mut Session, line: &str, b: &mut dyn Backend) -> Option<String> {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error_line(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = v.get("method").and_then(Value::as_str).map(String::from) else {
        return Some(error_line(id, -32600, "not a request: no \"method\""));
    };
    // Notifications are fire-and-forget; the only one we track is the client's handshake end.
    if method.starts_with("notifications/") {
        if method == "notifications/initialized" {
            s.initialized = true;
        }
        return None;
    }
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    match method.as_str() {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            s.protocol = match asked {
                Some(v) if SUPPORTED.contains(&v) => v.to_owned(),
                _ => SUPPORTED[0].to_owned(),
            };
            s.client = params
                .get("clientInfo")
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            Some(
                ok_line(
                    id,
                    json!({
                        "protocolVersion": s.protocol,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "winseek", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": INSTRUCTIONS,
                    }),
                ),
            )
        }
        "ping" => Some(ok_line(id, json!({}))),
        "tools/list" => Some(match b.list() {
            Ok(tools) => ok_line(id, json!({"tools": tools})),
            // App unreachable: offer the one tool that still means something ("am I running?").
            Err(_) => ok_line(id, json!({"tools": [fallback_status_tool()]})),
        }),
        "tools/call" => {
            let Some(tool) = params.get("name").and_then(Value::as_str) else {
                return Some(error_line(id, -32602, "tools/call needs a string \"name\""));
            };
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let answer = match b.call(&s.client, tool, args) {
                Ok(v) => {
                    // Tool errors are results (isError), not JSON-RPC errors (MCP spec).
                    let structured = if v.is_object() { v.clone() } else { json!({"result": v}) };
                    ok_line(id, json!({"content": [{"type": "text", "text": v.to_string()}], "structuredContent": structured}))
                }
                Err(msg) => ok_line(id, json!({"content": [{"type": "text", "text": msg}], "isError": true})),
            };
            Some(answer)
        }
        other => Some(error_line(id, -32601, &format!("method not found: {other}"))),
    }
}

fn ok_line(id: Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn error_line(id: Value, code: i32, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn fallback_status_tool() -> Value {
    json!({
        "name": "index_status",
        "title": "winseek index status",
        "description": "winseek is not reachable right now; call this to let the user know.",
        "inputSchema": {"type": "object", "properties": {}},
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::from_str;

    struct Fake(Vec<(String, Result<Value, String>)>);

    impl Backend for Fake {
        fn list(&mut self) -> Result<Value, String> {
            self.0.iter().find(|(n, _)| n == "list").map(|(_, r)| r.clone()).unwrap_or_else(|| Ok(json!([{"name": "x"}])))
        }
        fn call(&mut self, client: &str, tool: &str, _args: Value) -> Result<Value, String> {
            self.0
                .iter()
                .find(|(n, _)| n == tool)
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| Err(format!("{client} called {tool}: unknown tool")))
        }
    }

    fn line(s: &mut Session, v: Value, b: &mut dyn Backend) -> Option<Value> {
        handle_line(s, &v.to_string(), b).map(|l| from_str(&l).unwrap())
    }

    fn init(s: &mut Session, b: &mut dyn Backend) -> Value {
        line(s, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"tester"}}}), b).unwrap()
    }

    #[test]
    fn initialize_negotiates_version_and_client() {
        let mut b = Fake(vec![]);
        let mut s = Session::new();
        let r = init(&mut s, &mut b);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18"); // echoed: in the supported list
        assert_eq!(s.client, "tester");
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}), &mut b).unwrap();
        assert_eq!(r["result"]["protocolVersion"], SUPPORTED[0]); // unknown: newest
        assert_eq!(r["result"]["serverInfo"]["name"], "winseek");
        assert!(r["result"]["instructions"].as_str().unwrap().contains("search_files"));
    }

    #[test]
    fn notification_gets_no_answer() {
        let mut b = Fake(vec![]);
        let mut s = Session::new();
        init(&mut s, &mut b);
        assert!(handle_line(&mut s, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &mut b).is_none());
        assert!(s.initialized);
        assert!(handle_line(&mut s, r#"{"jsonrpc":"2.0","method":"notifications/whatever"}"#, &mut b).is_none());
    }

    #[test]
    fn unknown_method_is_32601() {
        let mut b = Fake(vec![]);
        let mut s = Session::new();
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":"a","method":"resources/list"}), &mut b).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert_eq!(r["id"], "a");
    }

    #[test]
    fn garbage_line_is_32700() {
        let mut b = Fake(vec![]);
        let mut s = Session::new();
        let out = handle_line(&mut s, "{nope", &mut b).unwrap();
        let r: Value = from_str(&out).unwrap();
        assert_eq!(r["error"]["code"], -32700);
        assert!(r["id"].is_null());
    }

    #[test]
    fn missing_method_is_32600() {
        let mut b = Fake(vec![("list".into(), Ok(json!([])))]);
        let mut s = Session::new();
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":5,"params":{}}), &mut b).unwrap();
        assert_eq!(r["error"]["code"], -32600);
    }

    #[test]
    fn ping_and_tools_list() {
        let mut b = Fake(vec![("list".into(), Ok(json!([{"name": "search_files"}])))]);
        let mut s = Session::new();
        init(&mut s, &mut b);
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":3,"method":"ping"}), &mut b).unwrap();
        assert_eq!(r["result"], json!({}));
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}), &mut b).unwrap();
        assert_eq!(r["result"]["tools"][0]["name"], "search_files");
    }

    #[test]
    fn list_failure_falls_back_to_index_status() {
        let mut b = Fake(vec![("list".into(), Err("app down".into()))]);
        let mut s = Session::new();
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":6,"method":"tools/list"}), &mut b).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "index_status");
    }

    #[test]
    fn tool_call_ok_is_content_plus_structured() {
        let mut b = Fake(vec![("search_files".into(), Ok(json!({"total": 1, "hits": []})))]);
        let mut s = Session::new();
        init(&mut s, &mut b);
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"search_files","arguments":{"query":"x"}}}), &mut b).unwrap();
        let res = &r["result"];
        assert_eq!(res["content"][0]["type"], "text");
        assert_eq!(from_str::<Value>(res["content"][0]["text"].as_str().unwrap()).unwrap()["total"], 1);
        assert_eq!(res["structuredContent"]["total"], 1);
        assert!(res.get("isError").is_none());
    }

    #[test]
    fn tool_error_is_a_result_not_an_rpc_error() {
        let mut b = Fake(vec![("recycle".into(), Err("unknown tool recycle".into()))]);
        let mut s = Session::new();
        init(&mut s, &mut b);
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"recycle"}}), &mut b).unwrap();
        let res = &r["result"];
        assert_eq!(res["isError"], true);
        assert_eq!(res["content"][0]["text"], "unknown tool recycle");
        assert!(r.get("error").is_none());
    }

    #[test]
    fn missing_name_is_32602_and_defaults_to_empty_args() {
        let mut b = Fake(vec![("search_files".into(), Ok(json!({"ok": true})))]);
        let mut s = Session::new();
        init(&mut s, &mut b);
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{}}), &mut b).unwrap();
        assert_eq!(r["error"]["code"], -32602);
        // no "arguments" key at all -> empty object reaches the backend (hits the Fake)
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"search_files"}}), &mut b).unwrap();
        assert_eq!(r["result"]["structuredContent"]["ok"], true);
    }

    #[test]
    fn string_and_number_ids_are_echoed() {
        let mut b = Fake(vec![("list".into(), Ok(json!([])))]);
        let mut s = Session::new();
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":"str-id","method":"ping"}), &mut b).unwrap();
        assert_eq!(r["id"], "str-id");
        let r = line(&mut s, json!({"jsonrpc":"2.0","id":42,"method":"ping"}), &mut b).unwrap();
        assert_eq!(r["id"], 42);
    }
}
