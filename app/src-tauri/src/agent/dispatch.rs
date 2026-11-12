//! Tool dispatch: args in, JSON out, strictly read-only (PRD RA3). Every tool calls the same
//! functions the Tauri commands call — nothing here opens, launches, renames, recycles or
//! copies, records launcher history or changes any setting. The whole file is the guarantee.

use serde_json::{json, Value};
use std::sync::Mutex;
use tauri::Manager;

use crate::launch::item::{Kind, Target};
use crate::launch::Launcher;
use crate::preview;
use crate::source::{Hit, NameSource};
use crate::tier1;

use super::config::{AgentConfig, ContentScope};
use super::dates;
use super::log;
use super::scan::{self, Bound};
use super::tools;

/// The agent's own semantic-search lock, separate from the popup's `LATEST` supersede: the
/// user typing cannot drop an agent query and vice versa (PRD RA4).
static AGENT_SEM: Mutex<()> = Mutex::new(());

/// Everything a tool call reads, gathered once from the app (fakes in tests).
pub struct Deps<'a> {
    pub source: &'a dyn NameSource,
    /// The Tier 1 worker when it runs (semantic search, parser, library, scopes).
    pub tier1: Option<&'a tier1::Tier1>,
    pub launcher: &'a Launcher,
    pub cfg: AgentConfig,
    /// Injected clock / timezone so range parsing is deterministic in tests.
    pub now_ms: i64,
    pub tz_offset_min: i32,
}

impl Deps<'_> {
    fn indexed_only(&self) -> bool {
        self.cfg.content != ContentScope::Any
    }

    fn scopes(&self) -> (Vec<String>, Vec<String>) {
        match self.tier1 {
            Some(t) => {
                let s = t.snapshot();
                (s.scopes.include.clone(), s.scopes.exclude.clone())
            }
            None => (vec![], vec![]),
        }
    }

    /// The content-scope gate for `read_text` / `parse_document` / EXIF (PRD A4).
    fn content_ok(&self, path: &str) -> Result<(), String> {
        let (inc, exc) = self.scopes();
        if tools::content_allowed(path, self.indexed_only(), &inc, &exc) {
            Ok(())
        } else {
            Err(format!("\"{path}\" is outside the folders winseek may read for agents (Settings › Agents)"))
        }
    }

    fn tier1(&self) -> Result<&tier1::Tier1, String> {
        self.tier1.ok_or_else(|| "The semantic index is not running".to_string())
    }
}

/// One tool call. Pure-ish core (no Tauri): the server and tests both call this.
pub fn run(deps: &Deps, tool: &str, args: &Value) -> Result<Value, String> {
    if !deps.cfg.enabled && tool != "index_status" {
        return Err("Agent access is off. Turn it on in winseek Settings › Agents.".into());
    }
    match tool {
        "index_status" => index_status(deps),
        "search_files" => search_files(deps, args),
        "search_by_meaning" => search_by_meaning(deps, args),
        "recent_files" => recent_files(deps, args),
        "file_info" => file_info(deps, args),
        "read_text" => read_text(deps, args),
        "parse_document" => parse_document(deps, args),
        "find_apps" => find_apps(deps, args),
        "library_big_files" => library_big_files(deps, args),
        "library_duplicates" => library_duplicates(deps, args),
        "library_collections" => library_collections(deps, args),
        other => Err(format!("unknown tool {other}")),
    }
}

/// Entry the pipe server uses: run with the per-call timeout (PRD RA5) and log the call.
pub fn call(app: &tauri::AppHandle, client: &str, tool: &str, args: Value) -> Result<Value, String> {
    let started = std::time::Instant::now();
    let secs: u64 = if tool == "parse_document" { 180 } else { 60 };
    let (tx, rx) = std::sync::mpsc::channel();
    let (app2, tool2, args2) = (app.clone(), tool.to_owned(), args.clone());
    std::thread::spawn(move || {
        let _ = tx.send(with_app(&app2, &tool2, &args2));
    });
    let res = match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
        Ok(r) => r,
        Err(_) => Err(format!("timed out after {secs}s")),
    };
    let st = app.state::<std::sync::Arc<super::AgentState>>();
    st.log.append(&log::Entry {
        ts_ms: now_ms(),
        client: client.to_owned(),
        tool: tool.to_owned(),
        args: log::truncate_args(&args),
        ok: res.is_ok(),
        count: res.as_ref().ok().and_then(|v| v.get("hits")).and_then(Value::as_array).map(Vec::len),
        error: res.as_ref().err().cloned(),
        ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    });
    res
}

fn with_app(app: &tauri::AppHandle, tool: &str, args: &Value) -> Result<Value, String> {
    let agent = app.state::<std::sync::Arc<super::AgentState>>();
    let cfg = agent.config();
    let src = app.state::<crate::commands::AppState>().source();
    let t1 = app.state::<tier1::Handle>().get().ok();
    let deps = Deps {
        source: &*src,
        tier1: t1.as_deref(),
        launcher: &app.state::<Launcher>(),
        cfg,
        now_ms: now_ms(),
        tz_offset_min: tz_offset_min(),
    };
    run(&deps, tool, args)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Local offset from UTC in minutes (`GetTimeZoneInformation`'s Bias is UTC = local + bias).
fn tz_offset_min() -> i32 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tzi = TIME_ZONE_INFORMATION::default();
    // SAFETY: out-pointer to a local struct; only the Bias field is read.
    let _ = unsafe { GetTimeZoneInformation(&mut tzi) };
    -tzi.Bias
}

// ---------------------------------------------------------------- arg helpers

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| format!("\"{key}\" is required (a string)"))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn arg_int(args: &Value, key: &str, default: usize, lo: usize, hi: usize) -> Result<usize, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| format!("\"{key}\" must be a whole number"))?;
            usize::try_from(n)
                .ok()
                .filter(|n| (lo..=hi).contains(n))
                .ok_or_else(|| format!("{key} must be {lo}\u{2013}{hi}"))
        }
    }
}

fn opt_int(args: &Value, key: &str) -> Result<Option<u32>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_u64().filter(|n| *n <= u64::from(u32::MAX)).ok_or_else(|| format!("\"{key}\" must be a page number"))?;
            Ok(Some(n as u32))
        }
    }
}

fn enum_arg<'a>(args: &'a Value, key: &str, allowed: &[&str]) -> Result<Option<&'a str>, String> {
    match opt_str(args, key) {
        None => Ok(None),
        Some(s) if allowed.contains(&s) => Ok(Some(s)),
        Some(s) => Err(format!("{key} must be one of {} (got \"{s}\")", allowed.join(", "))),
    }
}

// ---------------------------------------------------------------- search_files

struct SearchArgs {
    query: String,
    filter: String,
    sort: String,
    bound: Option<Bound>,
    size: Option<(Option<u64>, Option<u64>)>,
    limit: usize,
    offset: usize,
}

impl SearchArgs {
    fn parse(args: &Value, now_ms: i64, tz_offset_min: i32) -> Result<Self, String> {
        let filter = enum_arg(args, "type", &["all", "folders", "images", "docs", "video", "audio", "apps"])?
            .unwrap_or("all")
            .to_owned();
        let sort = enum_arg(args, "sort", &["relevance", "name", "date", "size"])?
            .map(|s| if s == "relevance" { "rel".to_owned() } else { s.to_owned() })
            .unwrap_or_else(|| "rel".into());
        // Dates: a bound pair each; "after" takes the range start, "before" the range start too
        // (modified_before 2025-01-31 means "before that day began", like the popup's date chip).
        let after = match opt_str(args, "modified_after") {
            None => None,
            Some(s) => Some(dates::date_range(s, now_ms, tz_offset_min)?.0),
        };
        let before = match opt_str(args, "modified_before") {
            None => None,
            Some(s) => Some(dates::date_range(s, now_ms, tz_offset_min)?.0),
        };
        let bound = match (after, before) {
            (None, None) => None,
            (Some(f), None) => Some(Bound::Date { from_ms: f, to_ms: i64::MAX }),
            (None, Some(t)) => Some(Bound::Date { from_ms: i64::MIN, to_ms: t }),
            (Some(f), Some(t)) if t <= f => return Err("modified_before must be after modified_after".into()),
            (Some(f), Some(t)) => Some(Bound::Date { from_ms: f, to_ms: t }),
        };
        let min = match args.get("min_size") {
            None | Some(Value::Null) => None,
            Some(v) => Some(dates::parse_size(v)?),
        };
        let max = match args.get("max_size") {
            None | Some(Value::Null) => None,
            Some(v) => Some(dates::parse_size(v)?),
        };
        let size = match (min, max) {
            (None, None) => None,
            (min, max) => Some((min, max)),
        };
        Ok(SearchArgs {
            query: arg_str(args, "query")?,
            filter,
            sort,
            bound,
            size,
            limit: arg_int(args, "limit", 50, 1, 200)?,
            offset: arg_int(args, "offset", 0, 0, 1_000_000_000)?,
        })
    }
}

fn size_in(size: Option<u64>, min: Option<u64>, max: Option<u64>) -> bool {
    match size {
        None => false,
        Some(s) => min.is_none_or(|m| s >= m) && max.is_none_or(|m| s <= m),
    }
}

fn hit_row(h: &Hit) -> Value {
    json!({
        "name": h.name, "path": h.path, "is_dir": h.is_dir,
        "size": h.size, "modified": h.mtime,
    })
}

fn search_files(deps: &Deps, args: &Value) -> Result<Value, String> {
    let a = SearchArgs::parse(args, deps.now_ms, deps.tz_offset_min)?;
    let src = deps.source;
    // Date wins when both filters are present (the popup's rule); the other rides in `extra`.
    let bound = a.bound.clone().or_else(|| a.size.map(|(min, max)| Bound::Size { min, max }));
    let (total, hits, truncated) = match (&bound, &a.size) {
        (Some(bound), size) => {
            // Date pulls newest-first pages, size biggest-first (same as the popup's scan).
            let sort = if matches!(bound, Bound::Date { .. }) { "date" } else { "size" };
            // Folders report the bytes they hold; a size filter is about files (the popup's rule).
            let file_size = |h: &Hit| if h.is_dir { None } else { h.size };
            let extra = |h: &Hit| size.is_none_or(|(min, max)| size_in(file_size(h), min, max));
            scan::scan(
                |off, lim| {
                    let r = src.search(&a.query, &a.filter, off, lim, sort);
                    Ok((r.total, r.hits))
                },
                bound,
                &extra,
                |h| h.mtime.unwrap_or(0),
                file_size,
                a.limit,
                a.offset,
            )?
        }
        (None, _) => {
            let r = src.search(&a.query, &a.filter, a.offset, a.limit, &a.sort);
            (r.total, r.hits, false)
        }
    };
    let rows: Vec<Value> = hits.iter().map(hit_row).collect();
    Ok(cap_size(json!({"total": total, "hits": rows, "truncated": truncated})))
}

/// RA5: keep list-shaped results under ~1 MB by cutting the `hits` list, flagging truncated.
fn cap_size(mut v: Value) -> Value {
    const CAP: usize = 1_000_000;
    if serde_json::to_vec(&v).map(|b| b.len()).unwrap_or(CAP + 1) <= CAP {
        return v;
    }
    // Take `hits` out of `v` so the size probe can read the rest without a borrow conflict.
    let mut hits = v.get_mut("hits").map(Value::take).unwrap_or(Value::Null);
    // Shorten the list until the whole payload fits again (shortest borrow per step).
    if serde_json::to_vec(&(&v, &hits)).map(|b| b.len()).unwrap_or(CAP + 1) > CAP {
        loop {
            let len = hits.as_array().map(Vec::len).unwrap_or(0);
            let over = serde_json::to_vec(&(&v, &hits)).map(|b| b.len()).unwrap_or(CAP + 1) > CAP;
            if !over || len <= 1 {
                break;
            }
            hits.as_array_mut().unwrap().pop();
        }
        v["truncated"] = json!(true);
    }
    v["hits"] = hits;
    v
}

// ---------------------------------------------------------------- the rest

fn search_by_meaning(deps: &Deps, args: &Value) -> Result<Value, String> {
    let q = arg_str(args, "query")?;
    let k = arg_int(args, "limit", 20, 1, 50)?;
    let t = deps.tier1()?;
    let _turn = AGENT_SEM.lock().unwrap_or_else(|e| e.into_inner());
    let r = t.semantic_search(&q, k)?;
    let hits: Vec<Value> = r
        .hits
        .iter()
        .map(|h| {
            json!({
                "path": h.path, "score": h.score, "snippet": h.snippet, "page": h.page,
                "source": h.source, "size": h.size, "modified": h.mtime,
            })
        })
        .collect();
    Ok(cap_size(json!({"state": r.state, "hits": hits, "total": hits.len(), "truncated": false})))
}

fn recent_files(deps: &Deps, args: &Value) -> Result<Value, String> {
    let limit = arg_int(args, "limit", 30, 1, 200)?;
    let hits = deps.source.recent(limit);
    let rows: Vec<Value> = hits.iter().map(hit_row).collect();
    let n = rows.len();
    Ok(json!({"total": n, "hits": rows, "truncated": false}))
}

fn file_info(deps: &Deps, args: &Value) -> Result<Value, String> {
    let path = arg_str(args, "path")?;
    let info = preview::read_info(&path)?;
    let mut v = serde_json::to_value(&info).map_err(|e| e.to_string())?;
    // EXIF needs the file's bytes, so it obeys the content scope; the metadata itself stays.
    if deps.content_ok(&path).is_err() {
        if let Some(img) = v.get_mut("image").filter(|i| !i.is_null()) {
            img["exif"] = Value::Null;
        }
    }
    Ok(v)
}

fn read_text(deps: &Deps, args: &Value) -> Result<Value, String> {
    let path = arg_str(args, "path")?;
    let page = opt_int(args, "page")?;
    let max_chars = arg_int(args, "max_chars", 20_000, 1, 200_000)?;
    deps.content_ok(&path)?;
    // read_info classifies and already reuses the preview's text / document decoders.
    let info = preview::read_info(&path)?;
    let (text, source_cut) = match info.kind {
        preview::Kind::Pdf => {
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let rt = tier1::pdf::runtime().map_err(|e| format!("the PDF engine is not available ({e:?})"))?;
            let doc = tier1::pdf::read_pdf(rt, &bytes, false).map_err(|e| format!("pdf: {e:?}"))?;
            let text = match page {
                Some(p) => doc
                    .pages
                    .iter()
                    .find(|pg| pg.page == p)
                    .map(|pg| pg.text.clone())
                    .ok_or_else(|| format!("no page {p} (the document has {})", doc.total_pages))?,
                None => doc.pages.iter().map(|pg| pg.text.as_str()).collect::<Vec<_>>().join("\n\n"),
            };
            (text, ("pdf", false))
        }
        preview::Kind::Image => {
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let hash = tier1::process::sha256_hex(&bytes);
            let t = deps.tier1()?;
            let texts = t
                .store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .texts_of(&hash)
                .map_err(|e| e.to_string())?;
            if texts.is_empty() {
                return Err("no text: run parse_document".into());
            }
            (texts.into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n"), ("ocr", false))
        }
        preview::Kind::Text => {
            let ti = info.text.ok_or("no text: the file has none")?;
            (ti.content, ("file", ti.truncated))
        }
        preview::Kind::Document => {
            let ti = info.text.ok_or("no text: the document has none")?;
            (ti.content, ("office", ti.truncated))
        }
        preview::Kind::Folder => return Err("cannot read text from a folder".into()),
        preview::Kind::Other => return Err("no text: the file is binary or has no readable text".into()),
        // Video/Audio carry no text today.
        _ => return Err("no text: this file kind has none".into()),
    };
    let (source, file_cut) = source_cut;
    let chars = text.chars().count();
    let (text, truncated) = if chars > max_chars {
        (text.chars().take(max_chars).collect::<String>(), true)
    } else {
        (text, file_cut)
    };
    Ok(json!({"text": text, "chars": text.chars().count(), "truncated": truncated, "source": source}))
}

fn parse_document(deps: &Deps, args: &Value) -> Result<Value, String> {
    let path = arg_str(args, "path")?;
    let page = opt_int(args, "page")?;
    deps.content_ok(&path)?;
    let t = deps.tier1()?;
    if !t.parser_present() {
        return Err("parser not installed".into());
    }
    let doc = t.parse_document(&path, page)?;
    serde_json::to_value(doc).map_err(|e| e.to_string())
}

fn find_apps(deps: &Deps, args: &Value) -> Result<Value, String> {
    let q = arg_str(args, "query")?;
    let limit = arg_int(args, "limit", 10, 1, 50)?;
    let kind = enum_arg(args, "kind", &["app", "setting", "command"])?;
    // Over-fetch so a `kind` filter still fills the page (the catalog is small; search is cheap).
    let hits = deps.launcher.search(&q, 200);
    let rows: Vec<Value> = hits
        .into_iter()
        .filter(|h| kind.is_none_or(|k| k == kind_str(&h.item.kind)))
        .take(limit)
        .map(|h| {
            json!({
                "id": h.item.id, "kind": kind_str(&h.item.kind), "title": h.item.title,
                "subtitle": h.item.subtitle, "target": target_value(&h.item.target), "score": h.score,
            })
        })
        .collect();
    let n = rows.len();
    Ok(json!({"total": n, "hits": rows, "truncated": false}))
}

fn kind_str(k: &Kind) -> &'static str {
    match k {
        Kind::App => "app",
        Kind::Setting => "setting",
        Kind::Command => "command",
    }
}

/// What the item opens, flattened for an agent that launches with its own tools.
fn target_value(t: &Target) -> Value {
    match t {
        Target::Aumid { aumid, path } => json!({"kind": "aumid", "aumid": aumid, "path": path}),
        Target::Path { path, args } => json!({"kind": "path", "path": path, "args": args}),
        Target::Uri { uri } => json!({"kind": "uri", "uri": uri}),
        Target::Exec { exe, args } => json!({"kind": "exec", "exe": exe, "args": args}),
        Target::Cmd { name } => json!({"kind": "command", "name": name}),
    }
}

fn index_status(deps: &Deps) -> Result<Value, String> {
    if !deps.cfg.enabled {
        // PRD A4: always allowed, and when off this is all the agent needs to tell the user.
        return Ok(json!({"access": "off"}));
    }
    // `detail()` keeps the filesystem name and per-volume errors, like the Status page.
    let t0 = serde_json::to_value(deps.source.detail()).map_err(|e| e.to_string())?;
    let tier1_part = match deps.tier1 {
        Some(t) => serde_json::to_value(t.status()).map_err(|e| e.to_string())?,
        None => Value::Null,
    };
    Ok(json!({
        "access": "on",
        "tier0": t0,
        "tier1": tier1_part,
        "parser_present": deps.tier1.map(|t| t.parser_present()),
        "content_scope": if deps.indexed_only() { "indexed" } else { "any" },
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

fn library_big_files(deps: &Deps, args: &Value) -> Result<Value, String> {
    let limit = arg_int(args, "limit", 50, 1, 500)?;
    let profile = std::env::var("USERPROFILE").map_err(|_| "USERPROFILE is not set".to_string())?;
    let q = tier1::library::big_files_query(&profile);
    let hits = deps.source.search(&q, "all", 0, limit, "size").hits;
    let rows: Vec<Value> = hits.iter().map(hit_row).collect();
    let n = rows.len();
    Ok(json!({"total": n, "hits": rows, "truncated": false}))
}

fn library_duplicates(deps: &Deps, args: &Value) -> Result<Value, String> {
    let limit = arg_int(args, "limit", 50, 1, 500)?;
    let groups = deps.tier1()?.library_dup_groups()?;
    let rows: Vec<Value> = groups
        .into_iter()
        .take(limit)
        .map(|g| {
            let size = g.members.first().map(|m| m.size).unwrap_or(0);
            let paths: Vec<&str> = g.members.iter().map(|m| m.path.as_str()).collect();
            json!({"size": size, "paths": paths, "reclaimable": g.reclaimable})
        })
        .collect();
    let n = rows.len();
    Ok(json!({"total": n, "hits": rows, "truncated": false}))
}

fn library_collections(deps: &Deps, args: &Value) -> Result<Value, String> {
    let t = deps.tier1()?;
    match opt_str(args, "name") {
        None => {
            let cols = t.get_collections()?;
            let rows: Vec<Value> = cols
                .iter()
                .map(|c| json!({"name": c.name, "query": c.query, "threshold": c.threshold, "rule": c.rule}))
                .collect();
            let n = rows.len();
            Ok(json!({"total": n, "hits": rows, "truncated": false}))
        }
        Some(name) => {
            let cols = t.get_collections()?;
            let c = cols
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("no collection named \"{name}\""))?;
            let query = if c.query.is_empty() { &c.name } else { &c.query };
            let opts = tier1::library::CollectionOpts { exclude: &c.exclude, graphics: c.graphics };
            let r = t.collection_hits(query, c.threshold, tier1::library::COLLECTION_CAP, opts)?;
            let hits: Vec<Value> = r
                .hits
                .iter()
                .map(|h| json!({"path": h.path, "score": h.score, "page": h.page, "size": h.size, "modified": h.mtime}))
                .collect();
            let n = hits.len();
            Ok(json!({"name": c.name, "hits": hits, "total": n, "truncated": false}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Launcher;
    use crate::source::MockSource;

    fn deps<'a>(src: &'a MockSource, cfg: AgentConfig, launcher: &'a Launcher) -> Deps<'a> {
        Deps {
            source: src,
            tier1: None,
            launcher,
            cfg,
            now_ms: 1_792_473_600_000, // 2026-10-20T12:00:00Z
            tz_offset_min: 60,
        }
    }

    fn enabled() -> AgentConfig {
        AgentConfig { enabled: true, content: ContentScope::Any }
    }

    #[test]
    fn off_refuses_everything_but_index_status() {
        let src = MockSource::generate(10);
        let launcher = Launcher::new();
        let d = deps(&src, AgentConfig::default(), &launcher);
        let msg = run(&d, "search_files", &json!({"query": "x"})).err().unwrap();
        assert!(msg.contains("Agent access is off"), "{msg}");
        assert_eq!(run(&d, "index_status", &json!({})).unwrap(), json!({"access": "off"}));
    }

    #[test]
    fn action_names_are_unknown() {
        let src = MockSource::generate(10);
        let launcher = Launcher::new();
        let d = deps(&src, enabled(), &launcher);
        for t in ["open", "launch", "recycle", "rename", "run", "copy", "restore", "shutdown", "show_in_winseek"] {
            let msg = run(&d, t, &json!({})).err().unwrap();
            assert_eq!(msg, format!("unknown tool {t}"));
        }
    }

    #[test]
    fn search_files_against_mock() {
        let src = MockSource::generate(2_000);
        let launcher = Launcher::new();
        let d = deps(&src, enabled(), &launcher);
        let v = run(&d, "search_files", &json!({"query": "invoice", "type": "docs", "limit": 5})).unwrap();
        assert!(v["total"].as_u64().unwrap() > 0);
        let hits = v["hits"].as_array().unwrap();
        assert!(hits.len() <= 5);
        assert!(hits[0]["path"].as_str().unwrap().contains("invoice"));
        assert!(hits[0]["modified"].is_i64());
    }

    #[test]
    fn search_files_date_filter() {
        let src = MockSource::generate(2_000);
        let launcher = Launcher::new();
        let d = deps(&src, enabled(), &launcher);
        let v = run(&d, "search_files", &json!({"query": "", "modified_after": "2026-10-01", "modified_before": "2026-10-21", "limit": 10})).unwrap();
        let hits = v["hits"].as_array().unwrap();
        for h in hits {
            let m = h["modified"].as_i64().unwrap();
            assert!(m >= 1_791_926_400_000, "hit {m} before the range"); // 2026-10-01
        }
    }

    #[test]
    fn arg_validation_messages() {
        let src = MockSource::generate(10);
        let launcher = Launcher::new();
        let d = deps(&src, enabled(), &launcher);
        let e = run(&d, "search_files", &json!({})).err().unwrap();
        assert!(e.contains("\"query\""), "{e}");
        let e = run(&d, "search_files", &json!({"query": "x", "limit": 5000})).err().unwrap();
        assert!(e.contains("limit must be 1\u{2013}200"), "{e}");
        let e = run(&d, "search_files", &json!({"query": "x", "type": "movies"})).err().unwrap();
        assert!(e.contains("type must be one of"), "{e}");
        let e = run(&d, "search_files", &json!({"query": "x", "min_size": "12 XX"})).err().unwrap();
        assert!(e.contains("bad size"), "{e}");
        let e = run(&d, "search_files", &json!({"query": "x", "modified_after": "notadate"})).err().unwrap();
        assert!(e.contains("bad date"), "{e}");
        let e = run(&d, "read_text", &json!({})).err().unwrap();
        assert!(e.contains("\"path\""), "{e}");
    }

    #[test]
    fn read_text_plain_file() {
        let tmp = crate::tier1::test_tmp();
        let p = tmp.path().join("agent-read.txt");
        std::fs::write(&p, "hello agent world\n".repeat(10)).unwrap();
        let src = MockSource::generate(10);
        let launcher = Launcher::new();
        let d = deps(&src, enabled(), &launcher);
        let v = run(&d, "read_text", &json!({"path": p.to_string_lossy(), "max_chars": 20})).unwrap();
        assert_eq!(v["chars"], 20);
        assert_eq!(v["truncated"], true);
        assert_eq!(v["source"], "file");
        // Outside the scope -> refused (indexed-only scope with no Tier 1 scopes = nothing allowed).
        let launcher2 = Launcher::new();
        let d2 = deps(&src, AgentConfig { enabled: true, content: ContentScope::Indexed }, &launcher2);
        let e = run(&d2, "read_text", &json!({"path": p.to_string_lossy()})).err().unwrap();
        assert!(e.contains("outside the folders"), "{e}");
    }

    #[test]
    fn read_text_docx() {
        let tmp = crate::tier1::test_tmp();
        let p = tmp.path().join("agent-read.docx");
        write_docx(&p, "agent docx body");
        let src = MockSource::generate(10);
        let launcher = Launcher::new();
        let d = deps(&src, enabled(), &launcher);
        let v = run(&d, "read_text", &json!({"path": p.to_string_lossy()})).unwrap();
        assert_eq!(v["source"], "office");
        assert!(v["text"].as_str().unwrap().contains("agent docx body"));
    }

    fn write_docx(path: &std::path::Path, body: &str) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.start_file("word/document.xml", SimpleFileOptions::default()).unwrap();
        write!(z, "<?xml version=\"1.0\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>{body}</w:t></w:r></w:p></w:body></w:document>").unwrap();
        z.finish().unwrap();
    }

    #[test]
    fn results_are_size_capped() {
        let big = json!({"total": 10_000, "hits": vec![json!({"path": "x".repeat(400)}); 10_000], "truncated": false});
        let v = cap_size(big);
        assert_eq!(v["truncated"], true);
        assert!(serde_json::to_vec(&v).unwrap().len() <= 1_100_000);
    }
}
