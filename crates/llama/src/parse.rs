//! Blocking client for Infinity-Parser2 on llama-server's `POST /v1/chat/completions`, plus the
//! pure output handling: tolerant JSON parsing (fences, wrapper objects, truncation salvage),
//! plain text for FTS/embedding, markdown, tables, and the stored bbox locator.
//! See docs/specs/2026-10-14-p3-parser.md section A.
//!
//! Bboxes come back normalised to 0-1000 (RUNLOG 2026-10-14), not in pixels.

use std::io::Read;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Error, Result};

/// The layout prompt (verbatim from scripts/p2-parser-smoke.py).
pub const PARSE_PROMPT: &str = include_str!("../prompts/doc2json-v1.txt");
pub const PARSE_PROMPT_VERSION: &str = "doc2json-v1";
/// Boxes kept in a stored locator.
pub const MAX_LOCATOR_BOXES: usize = 300;
/// Characters of element text kept per stored box.
const BOX_TEXT_CHARS: usize = 200;
/// Categories that go after the body in plain text (page furniture).
const FURNITURE: [&str; 3] = ["header", "footer", "page_footnote"];

fn parse_err(msg: impl Into<String>) -> Error {
    Error::Parse(msg.into())
}

/// One layout element. `bbox` is `[x1, y1, x2, y2]` in 0-1000, clamped and ordered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub bbox: Option<[f32; 4]>,
    pub category: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parsed {
    pub elements: Vec<Element>,
    pub prompt_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub html: String,
    pub tsv: String,
}

/// Request body for one page image (pure).
pub fn request_body(jpeg: &[u8]) -> Value {
    json!({
        "messages": [{"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg))}},
            {"type": "text", "text": PARSE_PROMPT}
        ]}],
        "temperature": 0,
        "top_p": 1,
        "max_tokens": 8192,
        "chat_template_kwargs": {"enable_thinking": false}
    })
}

/// `choices[0].message.content` of a chat completion.
pub fn response_content(resp: &Value) -> Result<&str> {
    resp.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| parse_err("response has no choices[0].message.content"))
}

pub struct ParseClient {
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
}

impl ParseClient {
    pub fn new(base_url: &str, api_key: &str) -> ParseClient {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(300))
            .build();
        ParseClient { base_url: base_url.trim_end_matches('/').to_string(), api_key: api_key.to_string(), agent }
    }

    /// Run the layout prompt over one JPEG page image.
    pub fn parse_image_jpeg(&self, jpeg: &[u8]) -> Result<Parsed> {
        let resp = self
            .agent
            .post(&format!("{}/v1/chat/completions", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_string(&request_body(jpeg).to_string())
            .map_err(|e| Error::Http(e.to_string()))?;
        let mut text = String::new();
        resp.into_reader().read_to_string(&mut text)?;
        let v: Value = serde_json::from_str(&text).map_err(|e| parse_err(format!("bad json: {e}")))?;
        let elements = parse_output(response_content(&v)?)?;
        Ok(Parsed { elements, prompt_version: PARSE_PROMPT_VERSION.to_string() })
    }

    /// Several page images at once: one request each, sent in parallel so the server decodes them
    /// together across its slots (`-np`). Results are in input order; a failed page does not
    /// affect the others.
    pub fn parse_image_jpegs(&self, jpegs: &[&[u8]]) -> Vec<Result<Parsed>> {
        std::thread::scope(|s| {
            jpegs.iter()
                .map(|j| s.spawn(|| self.parse_image_jpeg(j)))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().expect("parse request thread panicked"))
                .collect()
        })
    }
}

/// Drop a leading ```` ```json ```` / ```` ``` ```` line and a trailing ```` ``` ````.
fn strip_fences(s: &str) -> &str {
    let mut s = s.trim();
    if let Some(rest) = s.strip_prefix("```") {
        s = rest.split_once('\n').map_or("", |(_, body)| body);
    }
    s.trim().strip_suffix("```").unwrap_or(s).trim()
}

fn bbox_from(v: &Value) -> Option<[f32; 4]> {
    let a = v.as_array().filter(|a| a.len() == 4)?;
    let mut n = [0f32; 4];
    for (i, x) in a.iter().enumerate() {
        let f = x.as_f64().filter(|f| f.is_finite())?;
        n[i] = f.clamp(0.0, 1000.0) as f32;
    }
    Some([n[0].min(n[2]), n[1].min(n[3]), n[0].max(n[2]), n[1].max(n[3])])
}

/// An element object has a `category` or at least `text`; anything else is not an element.
fn element_from(v: &Value) -> Option<Element> {
    let o = v.as_object()?;
    if !o.contains_key("category") && !o.contains_key("text") {
        return None;
    }
    let s = |k: &str, d: &str| o.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
    Some(Element { bbox: o.get("bbox").and_then(bbox_from), category: s("category", "text"), text: s("text", "") })
}

fn elements_from(v: &Value) -> Option<Vec<Element>> {
    match v {
        Value::Array(a) => Some(a.iter().filter_map(element_from).collect()),
        Value::Object(o) => {
            if let Some(e) = element_from(v) {
                return Some(vec![e]);
            }
            o.values().find_map(|x| x.as_array().map(|a| a.iter().filter_map(element_from).collect()))
        }
        _ => None,
    }
}

/// Every complete `{...}` object (any depth, string-aware) that has a `category`, in order.
fn salvage(s: &str) -> Vec<Element> {
    let (mut stack, mut out) = (Vec::new(), Vec::new());
    let (mut in_str, mut esc) = (false, false);
    for (i, c) in s.char_indices() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => stack.push(i),
            '}' => {
                if let Some(start) = stack.pop() {
                    if let Ok(v) = serde_json::from_str::<Value>(&s[start..=i]) {
                        if v.get("category").is_some() {
                            out.extend(element_from(&v));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Parse the model's reply into elements. Accepts an array, an object holding one array (any
/// key) or a single element object, optionally fenced; a reply cut off by `max_tokens` keeps
/// every complete element before the cut.
pub fn parse_output(content: &str) -> Result<Vec<Element>> {
    let body = strip_fences(content);
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        return elements_from(&v).ok_or_else(|| parse_err("JSON holds no element array"));
    }
    let salvaged = salvage(body);
    if salvaged.is_empty() {
        return Err(parse_err("output is not JSON and no complete element could be salvaged"));
    }
    Ok(salvaged)
}

/// Decode the handful of entities the parser emits; unknown ones stay verbatim.
fn decode_entities(s: &str) -> String {
    const TABLE: [(&str, &str); 6] =
        [("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"), ("&nbsp;", " ")];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        match TABLE.iter().find(|(e, _)| rest.starts_with(e)) {
            Some((e, r)) => {
                out.push_str(r);
                rest = &rest[e.len()..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Rows of cell texts from table HTML (tags stripped, entities decoded, whitespace collapsed).
pub fn table_rows(html: &str) -> Vec<Vec<String>> {
    fn close_cell(cell: &mut Option<String>, row: &mut Vec<String>) {
        if let Some(c) = cell.take() {
            row.push(decode_entities(&c.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
    }
    let (mut rows, mut row, mut cell): (Vec<Vec<String>>, Vec<String>, Option<String>) = (Vec::new(), Vec::new(), None);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        if let Some(c) = cell.as_mut() {
            c.push_str(&rest[..lt]);
        }
        let Some(gt) = rest[lt..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[lt + 1..lt + gt].trim().to_ascii_lowercase();
        rest = &rest[lt + gt + 1..];
        let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        let closing = tag.starts_with('/');
        match (name.as_str(), closing) {
            ("td" | "th", false) => {
                close_cell(&mut cell, &mut row);
                cell = Some(String::new());
            }
            ("td" | "th", true) => close_cell(&mut cell, &mut row),
            ("tr", _) => {
                close_cell(&mut cell, &mut row);
                if !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                }
            }
            ("br", _) => {
                if let Some(c) = cell.as_mut() {
                    c.push(' ');
                }
            }
            _ => {}
        }
    }
    if let Some(c) = cell.as_mut() {
        c.push_str(rest);
    }
    close_cell(&mut cell, &mut row);
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn tsv_of(rows: &[Vec<String>]) -> String {
    rows.iter().map(|r| r.join("\t")).collect::<Vec<_>>().join("\n")
}

/// GitHub pipe table (first row is the header). `None` when there are no rows.
fn pipe_table(rows: &[Vec<String>]) -> Option<String> {
    let cols = rows.iter().map(Vec::len).max().filter(|c| *c > 0)?;
    let line = |r: &Vec<String>| {
        let cells: Vec<String> = (0..cols).map(|i| r.get(i).map_or(String::new(), |c| c.replace('|', "\\|"))).collect();
        format!("| {} |", cells.join(" | "))
    };
    let mut out = vec![line(&rows[0]), format!("|{}", " --- |".repeat(cols))];
    out.extend(rows[1..].iter().map(line));
    Some(out.join("\n"))
}

fn has_spans(html: &str) -> bool {
    let h = html.to_ascii_lowercase();
    h.contains("rowspan") || h.contains("colspan")
}

impl Parsed {
    /// Reading-order text for FTS / embedding: tables as TSV rows, figures skipped,
    /// header / footer / page footnote last.
    pub fn plain_text(&self) -> String {
        let piece = |e: &Element| -> String {
            match e.category.as_str() {
                "figure" => String::new(),
                "table" => tsv_of(&table_rows(&e.text)),
                _ => e.text.trim().to_string(),
            }
        };
        let furniture = |e: &&Element| FURNITURE.contains(&e.category.as_str());
        let body = self.elements.iter().filter(|e| !furniture(e));
        let tail = self.elements.iter().filter(furniture);
        body.chain(tail).map(piece).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n")
    }

    /// Elements joined by blank lines; tables as pipe tables (raw HTML when they use spans).
    pub fn markdown(&self) -> String {
        let piece = |e: &Element| -> String {
            match e.category.as_str() {
                "figure" => String::new(),
                "table" if has_spans(&e.text) => e.text.trim().to_string(),
                "table" => pipe_table(&table_rows(&e.text)).unwrap_or_else(|| e.text.trim().to_string()),
                _ => e.text.trim().to_string(),
            }
        };
        self.elements.iter().map(piece).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n")
    }

    pub fn tables(&self) -> Vec<Table> {
        self.elements
            .iter()
            .filter(|e| e.category == "table")
            .map(|e| Table { html: e.text.clone(), tsv: tsv_of(&table_rows(&e.text)) })
            .collect()
    }

    /// The stored parser locator (see the spec): `{"page":N?,"parser":{"v","boxes":[{b,c,t}]}}`.
    /// Only elements with a bbox are kept (at most [`MAX_LOCATOR_BOXES`]).
    pub fn locator_json(&self, page: Option<u32>) -> String {
        let boxes: Vec<Value> = self
            .elements
            .iter()
            .filter_map(|e| {
                let b = e.bbox?;
                let t: String = e.text.chars().take(BOX_TEXT_CHARS).collect();
                Some(json!({"b": b, "c": e.category, "t": t}))
            })
            .take(MAX_LOCATOR_BOXES)
            .collect();
        let mut v = json!({"parser": {"v": self.prompt_version, "boxes": boxes}});
        if let Some(p) = page {
            v["page"] = json!(p);
        }
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    const ARR: &str = r##"[
      {"bbox":[100,50,900,120],"category":"title","text":"# Quarterly Invoice 4471"},
      {"bbox":[100,200,900,400],"category":"table","text":"<table><tr><th>Item</th><th>Qty</th></tr><tr><td>R&amp;D card</td><td>1</td></tr></table>"},
      {"bbox":[100,950,300,990],"category":"footer","text":"Page 1 of 1"},
      {"bbox":[100,130,900,180],"category":"text","text":"Billed by Winseek."},
      {"bbox":[10,10,20,20],"category":"figure","text":""}
    ]"##;

    #[test]
    fn prompt_is_pinned() {
        assert!(PARSE_PROMPT.starts_with("Extract layout information"));
        assert!(PARSE_PROMPT.ends_with("must be a single JSON object."));
        assert_eq!(PARSE_PROMPT_VERSION, "doc2json-v1");
    }

    #[test]
    fn fenced_and_unfenced() {
        let a = parse_output(ARR).unwrap();
        assert_eq!(a.len(), 5);
        assert_eq!(a[0].category, "title");
        assert_eq!(a[0].bbox, Some([100.0, 50.0, 900.0, 120.0]));
        assert_eq!(parse_output(&format!("```json\n{ARR}\n```")).unwrap(), a);
        assert_eq!(parse_output(&format!("```\n{ARR}\n```\n")).unwrap(), a);
        assert_eq!(parse_output(&format!("  {ARR}  ")).unwrap(), a);
    }

    #[test]
    fn array_object_and_single() {
        let el = r#"{"bbox":[1,2,3,4],"category":"text","text":"hi"}"#;
        for key in ["layout", "elements", "results"] {
            let r = parse_output(&format!(r#"{{"{key}":[{el},{el}]}}"#)).unwrap();
            assert_eq!(r.len(), 2, "{key}");
        }
        let one = parse_output(el).unwrap();
        assert_eq!(one, vec![Element { bbox: Some([1.0, 2.0, 3.0, 4.0]), category: "text".into(), text: "hi".into() }]);
        assert!(parse_output(r#"{"foo":1}"#).is_err());
        assert!(parse_output("42").is_err());
        assert!(parse_output("").is_err());
        assert!(parse_output("no json here").is_err());
    }

    #[test]
    fn truncated_output_is_salvaged() {
        let cut = r#"{"layout":[{"bbox":[0,0,10,10],"category":"title","text":"A {brace} \" quote"},{"bbox":[0,20,10,30],"category":"text","text":"B"},{"bbox":[0,40,10,50],"category":"te"#;
        let r = parse_output(cut).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].text, "A {brace} \" quote");
        assert_eq!(r[1].text, "B");
        // truncated array under a fence
        let r = parse_output("```json\n[{\"category\":\"text\",\"text\":\"x\"},{\"category\":\"te").unwrap();
        assert_eq!(r.len(), 1);
        // nothing complete
        assert!(parse_output("[{\"category\":\"te").is_err());
    }

    #[test]
    fn bbox_clamped_ordered_or_none() {
        let r = parse_output(
            r#"[{"bbox":[-5,900,1200,100],"category":"text","text":"a"},
                {"bbox":[1,2,3],"category":"text","text":"b"},
                {"bbox":["a",2,3,4],"category":"text","text":"c"},
                {"category":"text","text":"d"}]"#,
        )
        .unwrap();
        assert_eq!(r[0].bbox, Some([0.0, 100.0, 1000.0, 900.0]));
        assert_eq!((r[1].bbox, r[2].bbox, r[3].bbox), (None, None, None));
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn plain_text_order_and_tables() {
        let p = Parsed { elements: parse_output(ARR).unwrap(), prompt_version: PARSE_PROMPT_VERSION.into() };
        assert_eq!(
            p.plain_text(),
            "# Quarterly Invoice 4471\n\nItem\tQty\nR&D card\t1\n\nBilled by Winseek.\n\nPage 1 of 1"
        );
    }

    #[test]
    fn markdown_pipe_tables_and_span_fallback() {
        let p = Parsed { elements: parse_output(ARR).unwrap(), prompt_version: "v".into() };
        assert_eq!(
            p.markdown(),
            "# Quarterly Invoice 4471\n\n| Item | Qty |\n| --- | --- |\n| R&D card | 1 |\n\nPage 1 of 1\n\nBilled by Winseek."
        );
        let span = "<table><tr><td colspan=\"2\">Head</td></tr><tr><td>a</td><td>b</td></tr></table>";
        let p = Parsed {
            elements: vec![Element { bbox: None, category: "table".into(), text: span.into() }],
            prompt_version: "v".into(),
        };
        assert_eq!(p.markdown(), span);
        // pipes in cells are escaped, short rows padded
        let t = table_rows("<tr><td>a|b</td><td>c</td></tr><tr><td>d</td></tr>");
        assert_eq!(pipe_table(&t).unwrap(), "| a\\|b | c |\n| --- | --- |\n| d |  |");
    }

    #[test]
    fn tables_and_entities() {
        let html = "<table>\n<tr><td> a &lt;b&gt;\n x </td><td>&quot;q&quot; &#39;s&#39;&nbsp;z</td></tr><tr><td>1<br>2</td><td>&unknown; &amp;amp;</td></tr></table>";
        let p = Parsed {
            elements: vec![Element { bbox: None, category: "table".into(), text: html.into() }],
            prompt_version: "v".into(),
        };
        let t = p.tables();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].html, html);
        assert_eq!(t[0].tsv, "a <b> x\t\"q\" 's' z\n1 2\t&unknown; &amp;");
        assert!(table_rows("not a table").is_empty());
    }

    #[test]
    fn locator_shape_and_caps() {
        let p = Parsed { elements: parse_output(ARR).unwrap(), prompt_version: PARSE_PROMPT_VERSION.into() };
        let v: Value = serde_json::from_str(&p.locator_json(Some(3))).unwrap();
        assert_eq!(v["page"], 3);
        assert_eq!(v["parser"]["v"], "doc2json-v1");
        assert_eq!(v["parser"]["boxes"].as_array().unwrap().len(), 5);
        assert_eq!(v["parser"]["boxes"][0]["c"], "title");
        assert_eq!(v["parser"]["boxes"][0]["b"], json!([100.0, 50.0, 900.0, 120.0]));
        assert!(serde_json::from_str::<Value>(&p.locator_json(None)).unwrap().get("page").is_none());
        let many = Parsed {
            elements: (0..400)
                .map(|i| Element { bbox: Some([0.0; 4]), category: "text".into(), text: format!("{i}{}", "x".repeat(500)) })
                .collect(),
            prompt_version: "v".into(),
        };
        let v: Value = serde_json::from_str(&many.locator_json(None)).unwrap();
        assert_eq!(v["parser"]["boxes"].as_array().unwrap().len(), MAX_LOCATOR_BOXES);
        assert_eq!(v["parser"]["boxes"][0]["t"].as_str().unwrap().chars().count(), 200);
    }

    #[test]
    fn request_body_shape() {
        let b = request_body(&[0xff, 0xd8, 0xff]);
        assert_eq!(b["messages"][0]["content"][0]["image_url"]["url"], "data:image/jpeg;base64,/9j/");
        assert_eq!(b["messages"][0]["content"][1]["text"], PARSE_PROMPT);
        assert_eq!((b["temperature"].clone(), b["top_p"].clone(), b["max_tokens"].clone()), (json!(0), json!(1), json!(8192)));
        assert_eq!(b["chat_template_kwargs"], json!({"enable_thinking": false}));
    }

    #[test]
    fn client_roundtrip_against_mock() {
        let content = serde_json::to_string(ARR).unwrap();
        let resp = format!(r#"{{"choices":[{{"message":{{"content":{content}}}}}]}}"#);
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let (mut reqline, mut auth, mut len) = (String::new(), String::new(), 0usize);
            r.read_line(&mut reqline).unwrap();
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
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                resp.len()
            );
            (reqline, auth)
        });
        let p = ParseClient::new(&url, "SECRET").parse_image_jpeg(b"jpg").unwrap();
        assert_eq!(p.elements.len(), 5);
        assert_eq!(p.prompt_version, PARSE_PROMPT_VERSION);
        let (reqline, auth) = h.join().unwrap();
        assert_eq!(reqline.trim(), "POST /v1/chat/completions HTTP/1.1");
        assert_eq!(auth, "Bearer SECRET");
        assert!(response_content(&json!({"choices": []})).is_err());
    }

    /// One mocked chat-completions round trip (headers drained, canned reply, connection closed).
    fn serve_once(l: &TcpListener, resp: &str) {
        let (mut s, _) = l.accept().unwrap();
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
        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}", resp.len());
    }

    #[test]
    fn pages_parse_in_parallel_against_a_mock() {
        let content = serde_json::to_string(ARR).unwrap();
        let resp = format!(r#"{{"choices":[{{"message":{{"content":{content}}}}}]}}"#);
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", l.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                serve_once(&l, &resp);
            }
        });
        let client = ParseClient::new(&url, "K");
        let out = client.parse_image_jpegs(&[b"a", b"b", b"c"]);
        server.join().unwrap();
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|r| r.as_ref().unwrap().elements.len() == 5), "{out:?}");
    }

    /// Live: needs the real llama-server + Infinity-Parser2 files.
    /// `WINSEEK_LLAMA_EXE`, `WINSEEK_PARSER_MODEL`, `WINSEEK_PARSER_MMPROJ`, plus an invoice image
    /// (`WINSEEK_PARSER_IMAGE`, default `tests/fixtures/invoice.png`) showing "Total due" and a table.
    #[test]
    #[ignore = "needs llama-server + parser model files (WINSEEK_LLAMA_EXE, WINSEEK_PARSER_MODEL, WINSEEK_PARSER_MMPROJ)"]
    fn parse_live() {
        use crate::server::{LlamaServer, Role, SpawnConfig};
        use std::path::PathBuf;
        let env = |k: &str| PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("{k} not set")));
        let img = std::env::var("WINSEEK_PARSER_IMAGE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/invoice.png"));
        let png = std::fs::read(&img).unwrap_or_else(|e| panic!("{}: {e}", img.display()));
        let mut jpeg = Vec::new();
        image::load_from_memory(&png)
            .unwrap()
            .to_rgb8()
            .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        let cfg = SpawnConfig { role: Role::Parser, health_timeout: Duration::from_secs(180), ..Default::default() };
        let (model, mmproj) = (env("WINSEEK_PARSER_MODEL"), env("WINSEEK_PARSER_MMPROJ"));
        let srv = LlamaServer::spawn_with(&env("WINSEEK_LLAMA_EXE"), &model, Some(&mmproj), &[], &cfg).unwrap();
        let parsed = ParseClient::new(&srv.base_url(), srv.api_key()).parse_image_jpeg(&jpeg).unwrap();
        assert!(parsed.plain_text().contains("Total due"), "{}", parsed.plain_text());
        assert!(!parsed.tables().is_empty(), "no table in {:?}", parsed.elements);
    }
}
