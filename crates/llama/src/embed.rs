//! Blocking client for llama-server's OpenAI-style `POST /v1/embeddings` (EmbeddingGemma-2).
//!
//! Images go in as `{"content":[{"type":"image_url",...}]}` inputs. Never use a bare
//! `image_url` part (400) nor `/embedding` + `image_data` (silently ignores the image,
//! llama.cpp #30082).

use std::io::Read;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

use crate::{Error, Result};

pub const DOC_PREFIX: &str = "title: none | text: ";
pub const QUERY_PREFIX: &str = "task: search result | query: ";
/// Matryoshka sizes EmbeddingGemma-2 supports.
pub const MATRYOSHKA_DIMS: [usize; 4] = [768, 512, 256, 128];

fn embed_err(msg: impl Into<String>) -> Error {
    Error::Embed(msg.into())
}

/// Shortest text [`EmbedClient::embed_documents`] keeps halving towards.
const MIN_SHRINK_CHARS: usize = 64;

/// An HTTP error with the server's own message (llama-server answers `{"error":{"message":..}}`).
fn http_err(e: ureq::Error) -> Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_string))
                .unwrap_or(body);
            let msg: String = msg.trim().chars().take(300).collect();
            if msg.is_empty() {
                Error::Http(format!("status code {code}"))
            } else {
                Error::Http(format!("status code {code}: {msg}"))
            }
        }
        e => Error::Http(e.to_string()),
    }
}

/// The server refused this input (too long for its batch / context), as opposed to being down.
fn rejected(e: &Error) -> bool {
    matches!(e, Error::Http(m) if m.starts_with("status code 500") || m.starts_with("status code 400"))
}

pub fn document_input(text: &str) -> String {
    format!("{DOC_PREFIX}{text}")
}

pub fn query_input(text: &str) -> String {
    format!("{QUERY_PREFIX}{text}")
}

/// One image input object (JPEG bytes -> data URL).
pub fn image_input(jpeg: &[u8]) -> Value {
    json!({
        "content": [{
            "type": "image_url",
            "image_url": { "url": format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg)) }
        }]
    })
}

/// One audio input object (16 kHz mono WAV bytes -> `input_audio`). Like images it rides in a
/// `content` array; the audio tokens come from the mmproj's audio encoder (no ASR model).
pub fn audio_input(wav: &[u8]) -> Value {
    json!({
        "content": [{
            "type": "input_audio",
            "input_audio": { "data": STANDARD.encode(wav), "format": "wav" }
        }]
    })
}

/// Request body for WAV clips.
pub fn audio_body(wavs: &[&[u8]]) -> Value {
    json!({ "input": wavs.iter().map(|w| audio_input(w)).collect::<Vec<_>>() })
}

/// Request body for text inputs (already prefixed).
pub fn text_body(inputs: &[String]) -> Value {
    json!({ "input": inputs })
}

/// Request body for JPEG images.
pub fn images_body(jpegs: &[&[u8]]) -> Value {
    json!({ "input": jpegs.iter().map(|j| image_input(j)).collect::<Vec<_>>() })
}

/// Parse `{"data":[{"index":i,"embedding":[..]}]}` into input order.
pub fn parse_embeddings(resp: &Value, expected: usize) -> Result<Vec<Vec<f32>>> {
    let data = resp
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| embed_err("response has no `data` array"))?;
    if data.len() != expected {
        return Err(embed_err(format!("expected {expected} embeddings, got {}", data.len())));
    }
    let mut rows: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
    for (pos, item) in data.iter().enumerate() {
        let index = item.get("index").and_then(Value::as_u64).map(|i| i as usize).unwrap_or(pos);
        let emb = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| embed_err("item has no `embedding` array"))?
            .iter()
            .map(|x| x.as_f64().map(|f| f as f32).ok_or_else(|| embed_err("non-numeric embedding value")))
            .collect::<Result<Vec<f32>>>()?;
        rows.push((index, emb));
    }
    rows.sort_by_key(|r| r.0);
    for (i, r) in rows.iter().enumerate() {
        if r.0 != i {
            return Err(embed_err(format!("embedding indices are not 0..{expected}")));
        }
    }
    Ok(rows.into_iter().map(|r| r.1).collect())
}

/// Matryoshka-truncate to `dims` (768/512/256/128), then L2-normalise.
/// An all-zero vector stays zero.
pub fn truncate_normalize(v: &[f32], dims: usize) -> Result<Vec<f32>> {
    if !MATRYOSHKA_DIMS.contains(&dims) {
        return Err(embed_err(format!("unsupported dims {dims} (use 768/512/256/128)")));
    }
    if v.len() < dims {
        return Err(embed_err(format!("vector has {} dims, need at least {dims}", v.len())));
    }
    let mut out = v[..dims].to_vec();
    let norm = out.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
    if norm > 0.0 {
        for x in &mut out {
            *x = (f64::from(*x) / norm) as f32;
        }
    }
    Ok(out)
}

/// True iff a `/props` document reports vision support (`modalities.vision == true`).
pub fn props_vision(props: &Value) -> bool {
    props.pointer("/modalities/vision").and_then(Value::as_bool).unwrap_or(false)
}

/// True iff a `/props` document reports audio support (`modalities.audio == true`).
pub fn props_audio(props: &Value) -> bool {
    props.pointer("/modalities/audio").and_then(Value::as_bool).unwrap_or(false)
}

pub struct EmbedClient {
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
}

impl EmbedClient {
    pub fn new(base_url: &str, api_key: &str) -> EmbedClient {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(300))
            .build();
        EmbedClient { base_url: base_url.trim_end_matches('/').to_string(), api_key: api_key.to_string(), agent }
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    fn post_embeddings(&self, body: &Value, expected: usize) -> Result<Vec<Vec<f32>>> {
        if expected == 0 {
            return Ok(Vec::new());
        }
        let resp = self
            .agent
            .post(&format!("{}/v1/embeddings", self.base_url))
            .set("Authorization", &self.auth())
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(http_err)?;
        let mut text = String::new();
        resp.into_reader().read_to_string(&mut text)?;
        let v: Value = serde_json::from_str(&text).map_err(|e| embed_err(format!("bad json: {e}")))?;
        parse_embeddings(&v, expected)
    }

    /// Raw (untruncated, unnormalised) document embeddings.
    ///
    /// llama-server rejects (HTTP 500) any input longer than its batch, and one such chunk would
    /// fail the whole request (and so the file). On a rejected batch every text is sent on its own,
    /// and a text that is still too long is cut in half until it fits.
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let inputs: Vec<String> = texts.iter().map(|t| document_input(t)).collect();
        match self.post_embeddings(&text_body(&inputs), inputs.len()) {
            Err(e) if texts.len() > 1 && rejected(&e) => texts.iter().map(|t| self.embed_one_document(t)).collect(),
            Err(e) if rejected(&e) => self.embed_one_document(texts[0]).map(|v| vec![v]),
            r => r,
        }
    }

    fn embed_one_document(&self, text: &str) -> Result<Vec<f32>> {
        let mut t = text;
        loop {
            let input = [document_input(t)];
            match self.post_embeddings(&text_body(&input), 1) {
                Err(e) if rejected(&e) && t.chars().count() > MIN_SHRINK_CHARS => {
                    let half = t.char_indices().nth(t.chars().count() / 2).map_or(t.len(), |(i, _)| i);
                    t = &t[..half];
                }
                r => return r.map(|mut v| v.remove(0)),
            }
        }
    }

    /// Raw query embeddings.
    pub fn embed_queries(&self, queries: &[&str]) -> Result<Vec<Vec<f32>>> {
        let inputs: Vec<String> = queries.iter().map(|q| query_input(q)).collect();
        self.post_embeddings(&text_body(&inputs), inputs.len())
    }

    /// Raw image embeddings (no prefix). Callers should canonicalise images to
    /// 768 px long side first and check [`vision_enabled`](Self::vision_enabled).
    pub fn embed_images_jpeg(&self, jpegs: &[&[u8]]) -> Result<Vec<Vec<f32>>> {
        self.post_embeddings(&images_body(jpegs), jpegs.len())
    }

    /// Raw audio embeddings: one 16 kHz mono 16-bit WAV clip per input (about 30 s each). Callers
    /// should check [`audio_enabled`](Self::audio_enabled) first.
    pub fn embed_audio_wav(&self, wavs: &[&[u8]]) -> Result<Vec<Vec<f32>>> {
        self.post_embeddings(&audio_body(wavs), wavs.len())
    }

    /// `GET /props`: is the mmproj loaded (vision available)? Missing field => false.
    pub fn vision_enabled(&self) -> Result<bool> {
        Ok(self.props()?.0)
    }

    /// `GET /props`: does the loaded mmproj take audio? Missing field => false.
    pub fn audio_enabled(&self) -> Result<bool> {
        Ok(self.props()?.1)
    }

    /// `(vision, audio)` from one `/props` call.
    fn props(&self) -> Result<(bool, bool)> {
        let resp = self
            .agent
            .get(&format!("{}/props", self.base_url))
            .set("Authorization", &self.auth())
            .call()
            .map_err(http_err)?;
        let mut text = String::new();
        resp.into_reader().read_to_string(&mut text)?;
        let v: Value = serde_json::from_str(&text).map_err(|e| embed_err(format!("bad json: {e}")))?;
        Ok((props_vision(&v), props_audio(&v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn text_body_shape_and_prefixes() {
        let docs = vec![document_input("hello"), document_input("")];
        assert_eq!(docs[0], "title: none | text: hello");
        assert_eq!(query_input("cats"), "task: search result | query: cats");
        assert_eq!(
            text_body(&docs),
            json!({"input": ["title: none | text: hello", "title: none | text: "]})
        );
    }

    #[test]
    fn image_body_exact_shape() {
        let body = images_body(&[&[0xff, 0xd8, 0xff], b"ab"]);
        assert_eq!(
            body,
            json!({"input": [
                {"content":[{"type":"image_url","image_url":{"url":"data:image/jpeg;base64,/9j/"}}]},
                {"content":[{"type":"image_url","image_url":{"url":"data:image/jpeg;base64,YWI="}}]}
            ]})
        );
        let s = serde_json::to_string(&body).unwrap();
        assert!(!s.contains("image_data"));
    }

    #[test]
    fn audio_body_exact_shape() {
        let body = audio_body(&[b"RIFF", b"ab"]);
        assert_eq!(
            body,
            json!({"input": [
                {"content":[{"type":"input_audio","input_audio":{"data":"UklGRg==","format":"wav"}}]},
                {"content":[{"type":"input_audio","input_audio":{"data":"YWI=","format":"wav"}}]}
            ]})
        );
        assert!(props_audio(&json!({"modalities":{"vision":true,"audio":true}})));
        assert!(!props_audio(&json!({"modalities":{"vision":true}})));
        assert!(!props_audio(&json!({"modalities":{"audio":"yes"}})));
    }

    #[test]
    fn truncate_and_normalise() {
        let v: Vec<f32> = (0..768).map(|i| (i % 7) as f32 + 1.0).collect();
        for d in MATRYOSHKA_DIMS {
            let t = truncate_normalize(&v, d).unwrap();
            assert_eq!(t.len(), d);
            let n: f32 = t.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((n - 1.0).abs() < 1e-5, "{d}: {n}");
        }
        // truncation happens before normalisation: only the first `dims` values count
        let t = truncate_normalize(&[3.0, 4.0, 100.0, 100.0].repeat(64), 128).unwrap();
        let want = 3.0 / (32.0f32 * (9.0 + 16.0 + 20000.0)).sqrt();
        assert!((t[0] - want).abs() < 1e-6);
        let small = truncate_normalize(&vec![2.0; 768], 128).unwrap();
        assert!((small[0] - 1.0 / (128f32).sqrt()).abs() < 1e-6);
        assert!(truncate_normalize(&v, 100).is_err());
        assert!(truncate_normalize(&v, 1024).is_err());
        assert!(truncate_normalize(&v[..200], 256).is_err());
        assert_eq!(truncate_normalize(&vec![0.0; 128], 128).unwrap(), vec![0.0; 128]);
    }

    #[test]
    fn parse_sorts_by_index_and_validates() {
        let r = json!({"data":[
            {"index":1,"embedding":[3.0,4.0]},
            {"index":0,"embedding":[1.0,2.0]}
        ]});
        assert_eq!(parse_embeddings(&r, 2).unwrap(), vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
        assert!(parse_embeddings(&r, 3).is_err());
        assert!(parse_embeddings(&json!({"data":[{"index":5,"embedding":[1.0]}]}), 1).is_err());
        assert!(parse_embeddings(&json!({"oops":1}), 1).is_err());
        assert!(parse_embeddings(&json!({"data":[{"index":0,"embedding":["x"]}]}), 1).is_err());
    }

    #[test]
    fn props_vision_parsing() {
        assert!(props_vision(&json!({"modalities":{"vision":true,"audio":false}})));
        assert!(!props_vision(&json!({"modalities":{"vision":false}})));
        assert!(!props_vision(&json!({"modalities":{}})));
        assert!(!props_vision(&json!({"other":1})));
        assert!(!props_vision(&json!({"modalities":{"vision":"yes"}})));
    }

    type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

    /// Answers `conns` requests; records (request line, auth header, body).
    fn mock(conns: usize, responses: Vec<String>) -> (String, Seen) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for resp in responses.iter().take(conns) {
                let (mut s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut reqline = String::new();
                r.read_line(&mut reqline).unwrap();
                let (mut auth, mut len) = (None, 0usize);
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
                        auth = Some(line["authorization:".len()..].trim().to_string());
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                seen2.lock().unwrap().push((
                    reqline.trim().to_string(),
                    auth.clone().unwrap_or_default(),
                    String::from_utf8(body).unwrap(),
                ));
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                    resp.len()
                );
            }
        });
        (url, seen)
    }

    #[test]
    fn client_roundtrip_against_mock() {
        let emb = r#"{"data":[{"index":1,"embedding":[0.0,1.0]},{"index":0,"embedding":[1.0,0.0]}]}"#.to_string();
        let props = r#"{"modalities":{"vision":true}}"#.to_string();
        let noprops = r#"{"default_generation_settings":{}}"#.to_string();
        let (url, seen) = mock(5, vec![emb.clone(), emb.clone(), emb, props, noprops]);
        let c = EmbedClient::new(&format!("{url}/"), "SECRET");

        let d = c.embed_documents(&["a", "b"]).unwrap();
        assert_eq!(d, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        let q = c.embed_queries(&["a", "b"]).unwrap();
        assert_eq!(q[1], vec![0.0, 1.0]);
        let im = c.embed_images_jpeg(&[b"xx", b"yy"]).unwrap();
        assert_eq!(im.len(), 2);
        assert!(c.vision_enabled().unwrap());
        assert!(!c.vision_enabled().unwrap());

        let seen = seen.lock().unwrap();
        for s in seen.iter() {
            assert_eq!(s.1, "Bearer SECRET");
        }
        assert_eq!(seen[0].0, "POST /v1/embeddings HTTP/1.1");
        let b0: Value = serde_json::from_str(&seen[0].2).unwrap();
        assert_eq!(b0, json!({"input": ["title: none | text: a", "title: none | text: b"]}));
        let b1: Value = serde_json::from_str(&seen[1].2).unwrap();
        assert_eq!(b1["input"][0], "task: search result | query: a");
        let b2: Value = serde_json::from_str(&seen[2].2).unwrap();
        assert_eq!(b2, images_body(&[b"xx", b"yy"]));
        assert_eq!(seen[3].0, "GET /props HTTP/1.1");
    }

    #[test]
    fn empty_input_makes_no_request() {
        let c = EmbedClient::new("http://127.0.0.1:1", "k");
        assert!(c.embed_documents(&[]).unwrap().is_empty());
        assert!(c.embed_images_jpeg(&[]).unwrap().is_empty());
        assert!(c.embed_audio_wav(&[]).unwrap().is_empty());
    }

    /// A llama-server stand-in that refuses (500, with its real message) any input longer than
    /// `max` characters and embeds the rest as `[len]`.
    fn picky(max: usize) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || loop {
            let Ok((mut s, _)) = l.accept() else { return };
            let mut r = BufReader::new(s.try_clone().unwrap());
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
            let v: Value = serde_json::from_slice(&body).unwrap();
            let inputs: Vec<usize> = v["input"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().chars().count()).collect();
            let (status, resp) = if inputs.iter().any(|&n| n > max) {
                ("500 Internal Server Error", r#"{"error":{"code":500,"message":"input is too large to process. increase the physical batch size","type":"server_error"}}"#.to_string())
            } else {
                let data: Vec<Value> = inputs.iter().enumerate().map(|(i, &n)| json!({"index": i, "embedding": [n as f32]})).collect();
                ("200 OK", json!({ "data": data }).to_string())
            };
            let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}", resp.len());
        });
        url
    }

    #[test]
    fn too_long_chunks_are_shrunk_not_fatal() {
        let c = EmbedClient::new(&picky(DOC_PREFIX.len() + 300), "K");
        let long = "word ".repeat(400); // 2000 chars
        let v = c.embed_documents(&["short", &long, "also short"]).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], vec![(DOC_PREFIX.len() + 5) as f32]);
        assert!(v[1][0] as usize <= DOC_PREFIX.len() + 300 && v[1][0] as usize > DOC_PREFIX.len() + 100, "{:?}", v[1]);
        // the server's message reaches the error text
        let e = EmbedClient::new(&picky(1), "K").embed_queries(&["x"]).unwrap_err().to_string();
        assert!(e.contains("500") && e.contains("too large"), "{e}");
    }
}
