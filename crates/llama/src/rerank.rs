//! Blocking client for llama-server's `POST /v1/rerank` (Qwen3-Reranker, `--reranking`).
//!
//! The reranker's chat template lives in the GGUF (`tokenizer.chat_template.rerank`) and is applied
//! server-side, so the caller sends the plain query and plain documents. Never call
//! `/v1/embeddings` on this server.

use std::io::Read;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::{Error, Result};

fn rerank_err(msg: impl Into<String>) -> Error {
    Error::Rerank(msg.into())
}

/// Request body: `{"query":..,"documents":[..],"top_n":N}` (all documents scored).
pub fn rerank_body(query: &str, docs: &[String]) -> Value {
    json!({ "query": query, "documents": docs, "top_n": docs.len() })
}

/// Parse `{"results":[{"index":i,"relevance_score":f}]}` into input order.
pub fn parse_scores(resp: &Value, expected: usize) -> Result<Vec<f32>> {
    let results = resp
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| rerank_err("response has no `results` array"))?;
    if results.len() != expected {
        return Err(rerank_err(format!(
            "expected {expected} scores, got {}",
            results.len()
        )));
    }
    let mut out: Vec<Option<f32>> = vec![None; expected];
    for item in results {
        let i = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|i| i as usize)
            .filter(|i| *i < expected)
            .ok_or_else(|| rerank_err("result has a missing or out-of-range `index`"))?;
        let s = item
            .get("relevance_score")
            .and_then(Value::as_f64)
            .filter(|s| s.is_finite())
            .ok_or_else(|| rerank_err("result has no numeric `relevance_score`"))?;
        if out[i].replace(s as f32).is_some() {
            return Err(rerank_err("duplicate result index"));
        }
    }
    out.into_iter()
        .map(|s| s.ok_or_else(|| rerank_err("missing result index")))
        .collect()
}

pub struct RerankClient {
    base_url: String,
    api_key: String,
}

impl RerankClient {
    pub fn new(base_url: &str, api_key: &str) -> RerankClient {
        RerankClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// Relevance scores (0..1, input order). `timeout` bounds the whole call. A 400 (llama-server's
    /// "request exceeds the available context size", typical for dense non-English text) is retried
    /// once with every document cut to half its length, inside the same budget; any other failure,
    /// or a second 400, is an error.
    pub fn rerank(&self, query: &str, docs: &[String], timeout: Duration) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let started = Instant::now();
        match self.request(query, docs, timeout) {
            Err(Error::Http(m)) if m == STATUS_400 => {
                let left = timeout.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Err(Error::Http(m));
                }
                let halved: Vec<String> = docs.iter().map(|d| halve(d)).collect();
                self.request(query, &halved, left)
            }
            r => r,
        }
    }

    fn request(&self, query: &str, docs: &[String], timeout: Duration) -> Result<Vec<f32>> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(2).min(timeout))
            .timeout(timeout)
            .build();
        let resp = agent
            .post(&format!("{}/v1/rerank", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_string(&rerank_body(query, docs).to_string())
            .map_err(|e| match e {
                ureq::Error::Status(code, _) => Error::Http(format!("status code {code}")),
                e => Error::Http(e.to_string()),
            })?;
        let mut text = String::new();
        resp.into_reader().read_to_string(&mut text)?;
        let v: Value =
            serde_json::from_str(&text).map_err(|e| rerank_err(format!("bad json: {e}")))?;
        parse_scores(&v, docs.len())
    }
}

const STATUS_400: &str = "status code 400";

/// The first half of `s` (by characters, rounded up so a non-empty text stays non-empty).
fn halve(s: &str) -> String {
    s.chars().take(s.chars().count().div_ceil(2)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn body_shape() {
        let b = rerank_body("q", &["a".into(), "b".into()]);
        assert_eq!(b, json!({"query":"q","documents":["a","b"],"top_n":2}));
    }

    #[test]
    fn parse_reorders_by_index_and_validates() {
        let v = json!({"results":[{"index":1,"relevance_score":0.9},{"index":0,"relevance_score":0.1}]});
        assert_eq!(parse_scores(&v, 2).unwrap(), vec![0.1, 0.9]);
        assert!(parse_scores(&v, 3).is_err());
        assert!(parse_scores(&json!({}), 1).is_err());
        let dup = json!({"results":[{"index":0,"relevance_score":0.1},{"index":0,"relevance_score":0.2}]});
        assert!(parse_scores(&dup, 2).is_err());
        let oob = json!({"results":[{"index":5,"relevance_score":0.1}]});
        assert!(parse_scores(&oob, 1).is_err());
        let nan = json!({"results":[{"index":0}]});
        assert!(parse_scores(&nan, 1).is_err());
    }

    type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

    /// One-shot server: records (request line, auth, body), then replies after `delay`.
    fn mock(status: &'static str, resp: String, delay: Duration) -> (String, Seen) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut reqline = String::new();
            r.read_line(&mut reqline).unwrap();
            let (mut auth, mut len) = (String::new(), 0usize);
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                let low = line.to_ascii_lowercase();
                if let Some(v) = low.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if low.starts_with("authorization:") {
                    auth = line["authorization:".len()..].trim().to_string();
                }
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            seen2.lock().unwrap().push((
                reqline.trim().to_string(),
                auth,
                String::from_utf8(body).unwrap(),
            ));
            std::thread::sleep(delay);
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                resp.len()
            );
        });
        (url, seen)
    }

    #[test]
    fn roundtrip_against_mock() {
        let resp =
            r#"{"results":[{"index":1,"relevance_score":0.8},{"index":0,"relevance_score":0.2}]}"#;
        let (url, seen) = mock("200 OK", resp.into(), Duration::ZERO);
        let c = RerankClient::new(&format!("{url}/"), "SECRET");
        let docs = vec!["a".to_string(), "b".to_string()];
        let s = c.rerank("q", &docs, Duration::from_secs(5)).unwrap();
        assert_eq!(s, vec![0.2, 0.8]);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "POST /v1/rerank HTTP/1.1");
        assert_eq!(seen[0].1, "Bearer SECRET");
        let b: Value = serde_json::from_str(&seen[0].2).unwrap();
        assert_eq!(b, rerank_body("q", &docs));
    }

    /// Serves each `(status, body)` in turn on its own connection; records every request body.
    fn mock_seq(replies: Vec<(&'static str, String)>) -> (String, Seen) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for (status, resp) in replies {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut reqline = String::new();
                r.read_line(&mut reqline).unwrap();
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                seen2.lock().unwrap().push((
                    reqline.trim().to_string(),
                    String::new(),
                    String::from_utf8(body).unwrap(),
                ));
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                    resp.len()
                );
            }
        });
        (url, seen)
    }

    const OVERSIZE: &str = r#"{"error":{"code":400,"message":"request (1175 tokens) exceeds the available context size"}}"#;

    #[test]
    fn halve_cuts_by_characters() {
        assert_eq!(halve("abcd"), "ab");
        assert_eq!(halve("abc"), "ab");
        assert_eq!(halve("x"), "x");
        assert_eq!(halve("日本語テキスト"), "日本語テ");
        assert_eq!(halve(""), "");
    }

    #[test]
    fn a_400_is_retried_once_with_documents_halved() {
        let ok =
            r#"{"results":[{"index":0,"relevance_score":0.3},{"index":1,"relevance_score":0.7}]}"#;
        let (url, seen) = mock_seq(vec![
            ("400 Bad Request", OVERSIZE.into()),
            ("200 OK", ok.into()),
        ]);
        let c = RerankClient::new(&url, "k");
        let docs = vec!["abcdefgh".to_string(), "ijkl".to_string()];
        let s = c.rerank("q", &docs, Duration::from_secs(5)).unwrap();
        assert_eq!(s, vec![0.3, 0.7]);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let first: Value = serde_json::from_str(&seen[0].2).unwrap();
        let second: Value = serde_json::from_str(&seen[1].2).unwrap();
        assert_eq!(first, rerank_body("q", &docs));
        assert_eq!(second["documents"], json!(["abcd", "ij"]));
        assert_eq!(second["query"], "q");
    }

    #[test]
    fn a_second_400_is_an_error_and_other_statuses_are_not_retried() {
        let (url, seen) = mock_seq(vec![
            ("400 Bad Request", OVERSIZE.into()),
            ("400 Bad Request", OVERSIZE.into()),
        ]);
        let c = RerankClient::new(&url, "k");
        let e = c
            .rerank("q", &["abcd".into()], Duration::from_secs(5))
            .unwrap_err();
        assert!(e.to_string().contains("400"), "{e}");
        assert_eq!(seen.lock().unwrap().len(), 2, "exactly one retry");

        let (url, seen) = mock_seq(vec![("500 Internal Server Error", "{}".into())]);
        let c = RerankClient::new(&url, "k");
        assert!(c
            .rerank("q", &["abcd".into()], Duration::from_secs(5))
            .is_err());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn empty_docs_make_no_request() {
        let c = RerankClient::new("http://127.0.0.1:1", "k");
        assert!(c
            .rerank("q", &[], Duration::from_secs(1))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn server_error_is_an_error() {
        let (url, _) = mock("500 Internal Server Error", "{}".into(), Duration::ZERO);
        let c = RerankClient::new(&url, "k");
        assert!(c
            .rerank("q", &["a".into()], Duration::from_secs(5))
            .is_err());
    }

    #[test]
    fn slow_server_times_out() {
        let resp = r#"{"results":[{"index":0,"relevance_score":0.5}]}"#;
        let (url, _) = mock("200 OK", resp.into(), Duration::from_millis(1500));
        let c = RerankClient::new(&url, "k");
        let t = std::time::Instant::now();
        assert!(c
            .rerank("q", &["a".into()], Duration::from_millis(200))
            .is_err());
        assert!(t.elapsed() < Duration::from_millis(1200));
    }
}
