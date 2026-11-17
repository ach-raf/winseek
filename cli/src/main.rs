//! winseek-cli: winseek from a terminal. Two surfaces:
//!
//! * name index — straight to the searchd named pipe (the same pipe the app uses; only
//!   interactive users may open it): `search`, `status`, `index`.
//! * the app's agent tools — over the agent pipe, the same pipe and dispatch `winseek-mcp`
//!   uses: `meaning`, `read`, `recent`, `apps` (the winseek app must be running and agent
//!   access on).
//! * `path` adds/removes the install directory to/from the user or machine PATH so shells
//!   find `winseek-cli` and `winseek-mcp` (the installer does this for the machine PATH).
//!
//! ```text
//! winseek-cli search <query...> [-n N] [--sort name|size|date|type|path[_desc|_asc]] [--long] [--json]
//! winseek-cli status [--json]
//! winseek-cli index [C|D:|--all] [--wait]     rescan volume(s) from the file table
//! winseek-cli meaning <query...> [-n N] [--long] [--json]
//! winseek-cli read <path> [--page N] [--max N] [--json]
//! winseek-cli recent [-n N] [--json]
//! winseek-cli apps <query...> [-n N] [--kind app|setting|command] [--json]
//! winseek-cli path [status|add|remove] [--machine] [--json]
//! ```
//!
//! Output goes to stdout (one line per result by default, so it pipes); notes and errors go
//! to stderr. Exit code: 0 ok, 1 failed (service/app not running, bad volume...), 2 bad usage.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use winseek_ipc::pipe::connect;
use winseek_ipc::{
    add_to_path, agent_pipe_name, path_contains, path_raw, remove_from_path, write_message,
    AgentRequest, AgentResponse, Hit, Request, Response, Sort, VolumeState, VolumeStatus,
    PIPE_NAME,
};

const USAGE: &str = "\
winseek-cli - winseek from a terminal

USAGE
  winseek-cli search <query...> [-n N] [--sort ORDER] [--long] [--json]
  winseek-cli status [--json]
  winseek-cli index [VOLUME | --all] [--wait] [--timeout SECS]
  winseek-cli meaning <query...> [-n N] [--long] [--json]
  winseek-cli read <path> [--page N] [--max N] [--json]
  winseek-cli recent [-n N] [--json]
  winseek-cli apps <query...> [-n N] [--kind app|setting|command] [--json]
  winseek-cli path [status | add | remove] [--machine] [--json]

search   Name search with the same syntax as the app (ext:pdf, size:>10mb, dm:today, folder:...).
         -n N        at most N results (default 50)
         --sort      name (default), size, date, type, path; add _desc / _asc to flip (size_asc, date_asc, name_desc...)
         --long      add size and modified date
         --json      JSON array of {name, path, is_dir, size, mtime}
status   Volumes, file counts and state of the name index.
index    Rescan a volume (C or C:) or all of them from the file table. --wait polls until it is live again;
         the poller also gives up when a scan shows no progress for 10 minutes (30 without per-file
         counts) or hits --timeout SECS, so a wedged scan cannot hang the command forever.
meaning  Search documents, screenshots and photos by what they CONTAIN (semantic index).
         Same -n / --json; --long adds score and snippet. Needs the winseek app running.
read     Extract the text of a file (PDF, Office, image OCR already parsed, code, ...).
         --page N    just that page (PDFs)
         --max N     at most N characters (default 20000)
         --json      the full {text, chars, truncated, source} object
recent   Recently used files. -n / --json like search.
apps     Search the launcher catalog (apps, Windows settings, commands). --long adds kind and title.
path     Show whether the install directory is on PATH; add/remove it (user PATH by default,
         --machine for all users — the installer already does that).

search/status/index need the winseek name service (searchd); meaning/read/recent/apps need the
winseek app running with agent access on (Settings › Agents).
Exit code 0 ok, 1 failed, 2 bad usage.";

#[derive(Debug, PartialEq)]
enum PathAction {
    Status,
    Add,
    Remove,
}

#[derive(Debug, PartialEq)]
enum Cmd {
    Help,
    Version,
    Search {
        query: String,
        limit: u32,
        sort: Sort,
        long: bool,
        json: bool,
    },
    Status {
        json: bool,
    },
    Index {
        volumes: Option<char>,
        wait: bool,
        /// Total wait cap for --wait, in seconds (from --timeout; None = unbounded).
        timeout: Option<u64>,
    },
    Meaning {
        query: String,
        limit: u32,
        long: bool,
        json: bool,
    },
    Read {
        path: String,
        page: Option<u32>,
        max_chars: Option<u32>,
        json: bool,
    },
    Recent {
        limit: u32,
        json: bool,
    },
    Apps {
        query: String,
        limit: u32,
        kind: Option<String>,
        json: bool,
    },
    Path {
        action: PathAction,
        machine: bool,
        json: bool,
    },
}

fn sort_of(s: &str) -> Result<Sort, String> {
    Ok(match s {
        "name" | "name_asc" => Sort::NameAsc,
        "name_desc" => Sort::NameDesc,
        "path" | "path_asc" => Sort::PathAsc,
        "size" | "size_desc" => Sort::SizeDesc,
        "size_asc" => Sort::SizeAsc,
        "date" | "date_desc" | "mtime" => Sort::MtimeDesc,
        "date_asc" => Sort::MtimeAsc,
        "type" | "type_asc" => Sort::TypeAsc,
        "type_desc" => Sort::TypeDesc,
        other => {
            return Err(format!(
                "unknown sort '{other}' (name, size, date, type, path; _asc / _desc)"
            ))
        }
    })
}

/// `-n N` / `--limit N` shared by four commands; rejects 0 and junk, caps at the tool limit.
fn limit_of(it: &mut std::slice::Iter<String>) -> Result<u32, String> {
    it.next()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .map(|n| n.min(200))
        .ok_or_else(|| "needs a positive number (at most 200)".to_string())
}

fn kind_of(s: &str) -> Result<String, String> {
    match s {
        "app" | "setting" | "command" => Ok(s.to_string()),
        other => Err(format!("unknown kind '{other}' (app, setting, command)")),
    }
}

fn parse(args: &[String]) -> Result<Cmd, String> {
    let Some((cmd, rest)) = args.split_first() else {
        return Ok(Cmd::Help);
    };
    match cmd.as_str() {
        "-h" | "--help" | "help" => Ok(Cmd::Help),
        "-V" | "--version" | "version" => Ok(Cmd::Version),
        "search" | "s" => {
            let (mut words, mut limit, mut sort, mut long, mut json) =
                (Vec::new(), 50u32, Sort::NameAsc, false, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-n" | "--limit" => limit = limit_of(&mut it)?,
                    "--sort" => sort = sort_of(it.next().ok_or("--sort needs a value")?)?,
                    "--long" | "-l" => long = true,
                    "--json" => json = true,
                    "--" => words.extend(it.by_ref().cloned()),
                    f if f.starts_with('-') && f.len() > 1 && !f.starts_with("-\"") => {
                        return Err(format!("unknown option {f}"))
                    }
                    w => words.push(w.to_string()),
                }
            }
            let query = words.join(" ");
            if query.trim().is_empty() {
                return Err("search needs a query".into());
            }
            Ok(Cmd::Search {
                query,
                limit,
                sort,
                long,
                json,
            })
        }
        "status" => match rest {
            [] => Ok(Cmd::Status { json: false }),
            [f] if f == "--json" => Ok(Cmd::Status { json: true }),
            _ => Err("usage: winseek-cli status [--json]".into()),
        },
        "index" | "rescan" => {
            let (mut vol, mut all, mut wait, mut timeout) = (None, false, false, None);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--all" => all = true,
                    "--wait" => wait = true,
                    "--timeout" => {
                        let v = it.next().ok_or("usage: index --timeout SECS")?;
                        timeout = Some(
                            v.parse::<u64>()
                                .map_err(|_| format!("--timeout: not a number: {v}"))?,
                        );
                    }
                    v if v.trim_end_matches(':').chars().count() == 1
                        && v.trim_end_matches(':')
                            .chars()
                            .all(|c| c.is_ascii_alphabetic()) =>
                    {
                        vol = Some(v.chars().next().unwrap().to_ascii_uppercase());
                    }
                    other => return Err(format!("not a volume or option: {other}")),
                }
            }
            match (vol, all) {
                (Some(_), true) => Err("give a volume or --all, not both".into()),
                (None, false) => Err("index needs a volume (C or C:) or --all".into()),
                (v, _) => Ok(Cmd::Index {
                    volumes: v,
                    wait,
                    timeout,
                }),
            }
        }
        "meaning" | "m" => {
            let (mut words, mut limit, mut long, mut json) = (Vec::new(), 20u32, false, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-n" | "--limit" => limit = limit_of(&mut it)?,
                    "--long" | "-l" => long = true,
                    "--json" => json = true,
                    "--" => words.extend(it.by_ref().cloned()),
                    f if f.starts_with('-') && f.len() > 1 && !f.starts_with("-\"") => {
                        return Err(format!("unknown option {f}"))
                    }
                    w => words.push(w.to_string()),
                }
            }
            let query = words.join(" ");
            if query.trim().is_empty() {
                return Err("meaning needs a query".into());
            }
            Ok(Cmd::Meaning {
                query,
                limit,
                long,
                json,
            })
        }
        "read" | "r" => {
            let (mut words, mut page, mut max, mut json) = (Vec::new(), None, None, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--page" => {
                        page = Some(
                            it.next()
                                .and_then(|v| v.parse().ok())
                                .filter(|n: &u32| *n > 0)
                                .ok_or("--page needs a positive number")?,
                        )
                    }
                    "--max" | "--max-chars" => {
                        max = Some(
                            it.next()
                                .and_then(|v| v.parse().ok())
                                .filter(|n: &u32| *n > 0 && *n <= 200_000)
                                .ok_or("--max needs a number from 1 to 200000")?,
                        )
                    }
                    "--json" => json = true,
                    "--" => words.extend(it.by_ref().cloned()),
                    f if f.starts_with('-') && f.len() > 1 && !f.starts_with("-\"") => {
                        return Err(format!("unknown option {f}"))
                    }
                    w => words.push(w.to_string()),
                }
            }
            if words.is_empty() {
                return Err("read needs a path".into());
            }
            Ok(Cmd::Read {
                path: words.join(" "),
                page,
                max_chars: max,
                json,
            })
        }
        "recent" => {
            let (mut limit, mut json) = (30u32, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-n" | "--limit" => limit = limit_of(&mut it)?,
                    "--json" => json = true,
                    other => return Err(format!("unknown option {other}")),
                }
            }
            Ok(Cmd::Recent { limit, json })
        }
        "apps" => {
            let (mut words, mut limit, mut kind, mut json) = (Vec::new(), 10u32, None, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-n" | "--limit" => limit = limit_of(&mut it)?,
                    "--kind" => {
                        kind = Some(kind_of(it.next().ok_or("--kind needs a value")?)?);
                    }
                    "--json" => json = true,
                    "--" => words.extend(it.by_ref().cloned()),
                    f if f.starts_with('-') && f.len() > 1 && !f.starts_with("-\"") => {
                        return Err(format!("unknown option {f}"))
                    }
                    w => words.push(w.to_string()),
                }
            }
            let query = words.join(" ");
            if query.trim().is_empty() {
                return Err("apps needs a query".into());
            }
            Ok(Cmd::Apps {
                query,
                limit,
                kind,
                json,
            })
        }
        "path" => {
            let (mut action, mut machine, mut json) = (PathAction::Status, false, false);
            for a in rest {
                match a.as_str() {
                    "status" => action = PathAction::Status,
                    "add" => action = PathAction::Add,
                    "remove" => action = PathAction::Remove,
                    "--machine" => machine = true,
                    "--json" => json = true,
                    other => {
                        return Err(format!(
                            "unknown path action '{other}' (status, add, remove)"
                        ))
                    }
                }
            }
            Ok(Cmd::Path {
                action,
                machine,
                json,
            })
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

/// One request/response round trip to searchd.
type Send<'a> = dyn FnMut(Request) -> io::Result<Response> + 'a;
/// One request/response round trip to the app's agent pipe.
type AgentSend<'a> = dyn FnMut(AgentRequest) -> io::Result<AgentResponse> + 'a;

/// Hard cap on one request/response round trip: a wedged backend must not hang the CLI
/// forever (a healthy searchd answers in milliseconds, agent reads in well under a second).
const ROUND_TRIP: Duration = Duration::from_secs(30);

fn fail(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn expect_status(r: Response) -> Result<Vec<VolumeStatus>, String> {
    match r {
        Response::StatusResult { volumes, .. } => Ok(volumes),
        Response::Error { message, .. } => Err(message),
        _ => Err("unexpected answer from searchd".into()),
    }
}

fn state_text(s: &VolumeState) -> String {
    match s {
        VolumeState::Scanning { done, total } if *total > 0 => {
            format!("scanning {}%", done.min(total) * 100 / total)
        }
        VolumeState::Scanning { .. } => "scanning".into(),
        VolumeState::Live => "live".into(),
        VolumeState::Rescanning => "rescanning".into(),
        VolumeState::Error { msg } => format!("error: {msg}"),
    }
}

/// Unix ms -> `YYYY-MM-DD HH:MM` (UTC).
fn when(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

fn size_text(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn print_hits(out: &mut dyn Write, hits: &[Hit], long: bool, json: bool) -> io::Result<()> {
    if json {
        let v: Vec<_> = hits
            .iter()
            .map(|h| serde_json::json!({"name": h.name, "path": h.path, "is_dir": h.is_dir, "size": h.size, "mtime": h.mtime}))
            .collect();
        return writeln!(
            out,
            "{}",
            serde_json::to_string(&v).unwrap_or_else(|_| "[]".into())
        );
    }
    for h in hits {
        if long {
            let size = if h.is_dir {
                "<dir>".to_string()
            } else {
                h.size.map(size_text).unwrap_or_default()
            };
            writeln!(
                out,
                "{:>10}  {:16}  {}",
                size,
                h.mtime.map(when).unwrap_or_default(),
                h.path
            )?;
        } else {
            writeln!(out, "{}", h.path)?;
        }
    }
    Ok(())
}

// -------------------------------------------------------------- agent tool rows

/// String field with a default, for tool-result rows.
fn vs<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// One tool call to the app. Tool errors arrive as `Error` messages; surface them verbatim
/// (the app's wording already tells the user what to flip in Settings).
fn agent_call(send: &mut AgentSend, tool: &str, args: Value) -> Result<Value, String> {
    let req = AgentRequest::Call {
        id: 1,
        client: "winseek-cli".into(),
        tool: tool.to_string(),
        args,
    };
    match send(req).map_err(fail)? {
        AgentResponse::Result { value, .. } => Ok(value),
        AgentResponse::Error { message, .. } => Err(message),
        _ => Err("unexpected answer from the winseek app".into()),
    }
}

fn rows_of(v: &Value) -> Result<&Vec<Value>, String> {
    v.get("hits")
        .and_then(Value::as_array)
        .ok_or_else(|| "unexpected answer from the winseek app".to_string())
}

fn print_json(out: &mut dyn Write, v: &Value) -> Result<(), String> {
    writeln!(out, "{}", serde_json::to_string(v).map_err(fail)?).map_err(fail)
}

/// What an `apps` row is worth launching: its target's primary value (path, exe, uri, AUMID
/// or command name), falling back to the title.
fn app_target(h: &Value) -> String {
    let t = h.get("target").cloned().unwrap_or(Value::Null);
    let s = match t.get("kind").and_then(Value::as_str) {
        Some("path") => vs(&t, "path"),
        Some("exec") => vs(&t, "exe"),
        Some("uri") => vs(&t, "uri"),
        Some("aumid") if !vs(&t, "path").is_empty() => vs(&t, "path"),
        Some("aumid") => vs(&t, "aumid"),
        Some("command") => vs(&t, "name"),
        _ => "",
    };
    if s.is_empty() {
        vs(h, "title").to_string()
    } else {
        s.to_string()
    }
}

/// The `path status` text (pure so it is testable): the directory and where it is on PATH.
fn path_status_text(dir: &str, user_raw: &str, machine_raw: &str) -> String {
    let yes = |b: bool| if b { "yes" } else { "no" };
    format!(
        "{dir}\nuser PATH:   {}\nmachine PATH: {}",
        yes(path_contains(user_raw, dir)),
        yes(path_contains(machine_raw, dir))
    )
}

/// The install directory: where winseek-cli.exe (and winseek-mcp.exe) live.
fn install_dir() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("where am I: {e}"))?;
    exe.parent()
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| "no install directory".to_string())
}

fn run(cmd: Cmd, send: &mut Send, out: &mut dyn Write, err: &mut dyn Write) -> Result<(), String> {
    match cmd {
        Cmd::Help => writeln!(out, "{USAGE}").map_err(fail),
        Cmd::Version => writeln!(out, "winseek-cli {}", env!("CARGO_PKG_VERSION")).map_err(fail),
        Cmd::Search {
            query,
            limit,
            sort,
            long,
            json,
        } => {
            match send(Request::Search {
                id: 1,
                query,
                offset: 0,
                limit,
                sort,
            })
            .map_err(fail)?
            {
                Response::SearchResult {
                    total,
                    hits,
                    elapsed_us,
                    ..
                } => {
                    print_hits(out, &hits, long, json).map_err(fail)?;
                    if !json {
                        let _ = writeln!(
                            err,
                            "{} of {} results in {:.1} ms",
                            hits.len(),
                            total,
                            elapsed_us as f64 / 1000.0
                        );
                    }
                    Ok(())
                }
                Response::Error { message, .. } => Err(message),
                _ => Err("unexpected answer from searchd".into()),
            }
        }
        Cmd::Status { json } => {
            let vols = expect_status(send(Request::Status { id: 1 }).map_err(fail)?)?;
            if json {
                let v: Vec<_> = vols
                    .iter()
                    .map(|v| serde_json::json!({"volume": v.letter.to_string(), "files": v.files, "folders": v.folders, "fs": v.fs, "state": state_text(&v.state)}))
                    .collect();
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string(&v).unwrap_or_else(|_| "[]".into())
                )
                .map_err(fail)
            } else {
                for v in &vols {
                    writeln!(
                        out,
                        "{}:  {:>12} files  {:>10} folders  {:6}  {}",
                        v.letter,
                        v.files,
                        v.folders,
                        v.fs,
                        state_text(&v.state)
                    )
                    .map_err(fail)?;
                }
                Ok(())
            }
        }
        Cmd::Index {
            volumes,
            wait,
            timeout,
        } => {
            let letters: Vec<char> = match volumes {
                Some(v) => vec![v],
                None => expect_status(send(Request::Status { id: 1 }).map_err(fail)?)?
                    .into_iter()
                    .map(|v| v.letter)
                    .collect(),
            };
            if letters.is_empty() {
                return Err("no volumes are indexed".into());
            }
            for &l in &letters {
                match send(Request::Rescan { id: 1, volume: l }).map_err(fail)? {
                    Response::StatusResult { .. } => {
                        let _ = writeln!(err, "{l}: rescan started");
                    }
                    Response::Error { message, .. } => return Err(message),
                    _ => return Err("unexpected answer from searchd".into()),
                }
            }
            if wait {
                wait_live(send, &letters, err, timeout.map(Duration::from_secs))?;
            }
            Ok(())
        }
        Cmd::Path { .. }
        | Cmd::Meaning { .. }
        | Cmd::Read { .. }
        | Cmd::Recent { .. }
        | Cmd::Apps { .. } => Err("internal: not a searchd command".into()),
    }
}

/// The agent-tool commands (`meaning`/`read`/`recent`/`apps`), over the app's agent pipe.
fn run_agent(
    cmd: Cmd,
    send: &mut AgentSend,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), String> {
    match cmd {
        Cmd::Meaning {
            query,
            limit,
            long,
            json,
        } => {
            let v = agent_call(
                send,
                "search_by_meaning",
                json!({"query": query, "limit": limit}),
            )?;
            if json {
                return print_json(out, &v);
            }
            let hits = rows_of(&v)?;
            for h in hits {
                let path = vs(h, "path");
                if long {
                    let score = h.get("score").and_then(Value::as_f64).unwrap_or(0.0);
                    writeln!(out, "{score:.3}  {path}").map_err(fail)?;
                    let snippet = vs(h, "snippet");
                    if !snippet.is_empty() {
                        writeln!(out, "    {snippet}").map_err(fail)?;
                    }
                } else {
                    writeln!(out, "{path}").map_err(fail)?;
                }
            }
            let _ = writeln!(
                err,
                "{} results (semantic index: {})",
                hits.len(),
                vs(&v, "state")
            );
            Ok(())
        }
        Cmd::Read {
            path,
            page,
            max_chars,
            json,
        } => {
            let mut args = json!({"path": path});
            if let Some(p) = page {
                args["page"] = json!(p);
            }
            if let Some(m) = max_chars {
                args["max_chars"] = json!(m);
            }
            let v = agent_call(send, "read_text", args)?;
            if json {
                return print_json(out, &v);
            }
            let text = v
                .get("text")
                .and_then(Value::as_str)
                .ok_or("unexpected answer from the winseek app")?;
            write!(out, "{text}").map_err(fail)?;
            if !text.ends_with('\n') {
                writeln!(out).map_err(fail)?;
            }
            let chars = v.get("chars").and_then(Value::as_u64).unwrap_or(0);
            let cut = if v.get("truncated").and_then(Value::as_bool).unwrap_or(false) {
                ", truncated"
            } else {
                ""
            };
            let _ = writeln!(err, "{chars} chars (source: {}{cut})", vs(&v, "source"));
            Ok(())
        }
        Cmd::Recent { limit, json } => {
            let v = agent_call(send, "recent_files", json!({"limit": limit}))?;
            if json {
                return print_json(out, &v);
            }
            let hits = rows_of(&v)?;
            for h in hits {
                writeln!(out, "{}", vs(h, "path")).map_err(fail)?;
            }
            let _ = writeln!(err, "{} recent files", hits.len());
            Ok(())
        }
        Cmd::Apps {
            query,
            limit,
            kind,
            json,
        } => {
            let mut args = json!({"query": query, "limit": limit});
            if let Some(k) = kind {
                args["kind"] = json!(k);
            }
            let v = agent_call(send, "find_apps", args)?;
            if json {
                return print_json(out, &v);
            }
            for h in rows_of(&v)? {
                let title = vs(h, "title");
                let target = app_target(h);
                writeln!(out, "{title:>40}  {target}").map_err(fail)?;
            }
            let _ = writeln!(err, "{} apps and commands", rows_of(&v)?.len());
            Ok(())
        }
        Cmd::Path { .. } => Err("internal: path is not an agent command".into()),
        Cmd::Help | Cmd::Version | Cmd::Search { .. } | Cmd::Status { .. } | Cmd::Index { .. } => {
            Err("internal: not an agent command".into())
        }
    }
}

/// `path` — registry only, no pipes, works even with everything down.
fn run_path(cmd: Cmd, out: &mut dyn Write, err: &mut dyn Write) -> Result<(), String> {
    let Cmd::Path {
        action,
        machine,
        json,
    } = cmd
    else {
        return Err("internal: not a path command".into());
    };
    let dir = install_dir()?;
    match action {
        PathAction::Status => {
            let user = path_raw(false).map_err(fail)?;
            let machine = path_raw(true).map_err(fail)?;
            if json {
                return print_json(
                    out,
                    &json!({
                        "dir": dir,
                        "user_path": path_contains(&user, &dir),
                        "machine_path": path_contains(&machine, &dir),
                    }),
                );
            }
            writeln!(out, "{}", path_status_text(&dir, &user, &machine)).map_err(fail)
        }
        PathAction::Add | PathAction::Remove => {
            let changed = if action == PathAction::Add {
                add_to_path(&dir, machine).map_err(fail)?
            } else {
                remove_from_path(&dir, machine).map_err(fail)?
            };
            let scope = if machine { "machine" } else { "user" };
            let note = if changed {
                format!("{dir}: {scope} PATH updated (new terminals will see it)")
            } else {
                let state = if action == PathAction::Add {
                    "on"
                } else {
                    "absent from"
                };
                format!("{dir}: already {state} the {scope} PATH, nothing to do")
            };
            let _ = writeln!(err, "{note}");
            Ok(())
        }
    }
}

/// A scanning volume stuck on one status line for this long is wedged, not working:
/// `done/total` counts change as a healthy scan progresses.
const STALL_COUNTED: Duration = Duration::from_secs(600);
/// Same for states with no per-file counts to watch (e.g. "rescanning").
const STALL_BLIND: Duration = Duration::from_secs(1800);
const POLL: Duration = Duration::from_millis(1000);

/// Tunables for [`wait_live_with`], injectable so the wedged-scan paths are testable.
struct WaitPolicy {
    /// Stall window when per-file counts are visible.
    stall_counted: Duration,
    /// Stall window without counts (the status line is static by design).
    stall_blind: Duration,
    /// How often to ask the service for status.
    poll: Duration,
    /// Total wait cap (from --timeout); `None` = unbounded.
    cap: Option<Duration>,
}

/// Poll until none of `letters` is scanning or rescanning (or errored). A status line that
/// stops changing for the stall window — or the `--timeout` cap, when set — ends the wait
/// with an error, so `--wait` cannot hang forever on a wedged scan.
fn wait_live(
    send: &mut Send,
    letters: &[char],
    err: &mut dyn Write,
    cap: Option<Duration>,
) -> Result<(), String> {
    wait_live_with(
        send,
        letters,
        err,
        WaitPolicy {
            stall_counted: STALL_COUNTED,
            stall_blind: STALL_BLIND,
            poll: POLL,
            cap,
        },
        Instant::now,
    )
}

/// [`wait_live`] with injectable policy and clock.
fn wait_live_with(
    send: &mut Send,
    letters: &[char],
    err: &mut dyn Write,
    p: WaitPolicy,
    now: fn() -> Instant,
) -> Result<(), String> {
    let mut last = String::new();
    let mut last_change = now();
    let started = last_change;
    loop {
        let vols = expect_status(send(Request::Status { id: 2 }).map_err(fail)?)?;
        let mine: Vec<&VolumeStatus> = vols
            .iter()
            .filter(|v| letters.contains(&v.letter))
            .collect();
        if let Some(bad) = mine
            .iter()
            .find(|v| matches!(v.state, VolumeState::Error { .. }))
        {
            return Err(format!("{}: {}", bad.letter, state_text(&bad.state)));
        }
        let busy: Vec<String> = mine
            .iter()
            .filter(|v| {
                matches!(
                    v.state,
                    VolumeState::Scanning { .. } | VolumeState::Rescanning
                )
            })
            .map(|v| format!("{}: {}", v.letter, state_text(&v.state)))
            .collect();
        if busy.is_empty() {
            return Ok(());
        }
        let line = busy.join("  ");
        if line != last {
            let _ = writeln!(err, "{line}");
            last = line.clone();
            last_change = now();
        }
        // A scan whose counts are visible changes the line as it progresses; without counts
        // the line is static by design, so give it a wider window.
        let counted = mine
            .iter()
            .any(|v| matches!(&v.state, VolumeState::Scanning { total, .. } if *total > 0));
        let stall = if counted {
            p.stall_counted
        } else {
            p.stall_blind
        };
        if now().duration_since(last_change) > stall {
            return Err(format!(
                "no progress for {} min ({line}) — the scan looks wedged; \
                 check the winseek app or re-run without --wait",
                stall.as_secs() / 60
            ));
        }
        if let Some(cap) = p.cap {
            if now().duration_since(started) > cap {
                return Err(format!(
                    "still not live after {} s ({line}); re-run with a larger --timeout",
                    cap.as_secs()
                ));
            }
        }
        std::thread::sleep(p.poll);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("winseek-cli: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let (stdout, stderr) = (io::stdout(), io::stderr());
    let (mut out, mut err) = (stdout.lock(), stderr.lock());

    // path needs no pipes at all.
    if matches!(cmd, Cmd::Path { .. }) {
        if let Err(e) = run_path(cmd, &mut out, &mut err) {
            let _ = writeln!(err, "winseek-cli: {e}");
            std::process::exit(1);
        }
        return;
    }
    // Help/version need no pipes either.
    if matches!(cmd, Cmd::Help | Cmd::Version) {
        let _ = run(
            cmd,
            &mut |_| Err(io::Error::other("no pipe needed")),
            &mut out,
            &mut err,
        );
        return;
    }
    // Agent tools talk to the app, not the service.
    if matches!(
        cmd,
        Cmd::Meaning { .. } | Cmd::Read { .. } | Cmd::Recent { .. } | Cmd::Apps { .. }
    ) {
        let name = match agent_pipe_name() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("winseek-cli: cannot derive the agent pipe name ({e})");
                std::process::exit(1);
            }
        };
        let mut conn = match connect(&name, Duration::from_secs(2)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "winseek-cli: cannot reach the winseek app ({e}). Agent tools need the \
                     winseek app running (not just the search service) and agent access on \
                     (winseek Settings › Agents)."
                );
                std::process::exit(1);
            }
        };
        let mut send = |req: AgentRequest| -> io::Result<AgentResponse> {
            write_message(&mut conn, &req)?;
            conn.read_message_timeout(ROUND_TRIP)
        };
        if let Err(e) = run_agent(cmd, &mut send, &mut out, &mut err) {
            let _ = writeln!(err, "winseek-cli: {e}");
            std::process::exit(1);
        }
        return;
    }
    let mut conn = match connect(PIPE_NAME, Duration::from_secs(2)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("winseek-cli: cannot reach the winseek name service ({e}). Is it running? Start it from the winseek app, or: sc start winseek-searchd");
            std::process::exit(1);
        }
    };
    let mut send = |req: Request| -> io::Result<Response> {
        write_message(&mut conn, &req)?;
        conn.read_message_timeout(ROUND_TRIP)
    };
    if let Err(e) = run(cmd, &mut send, &mut out, &mut err) {
        let _ = writeln!(err, "winseek-cli: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winseek_ipc::pipe::PipeServer;
    use winseek_ipc::read_message;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn hit(name: &str, path: &str, size: u64) -> Hit {
        Hit {
            name: name.into(),
            path: path.into(),
            is_dir: false,
            size: Some(size),
            mtime: Some(1_700_000_000_000),
        }
    }

    #[test]
    fn parses_search() {
        assert_eq!(
            parse(&a("search invoice ext:pdf -n 5 --sort size --json")).unwrap(),
            Cmd::Search {
                query: "invoice ext:pdf".into(),
                limit: 5,
                sort: Sort::SizeDesc,
                long: false,
                json: true
            }
        );
        assert_eq!(
            parse(&a("s foo --sort date_asc -l")).unwrap(),
            Cmd::Search {
                query: "foo".into(),
                limit: 50,
                sort: Sort::MtimeAsc,
                long: true,
                json: false
            }
        );
        // `--` lets a query start with a dash
        assert!(
            matches!(parse(&a("search -- -draft")).unwrap(), Cmd::Search { query, .. } if query == "-draft")
        );
        assert!(parse(&a("search")).is_err());
        assert!(parse(&a("search x -n 0")).is_err());
        assert!(parse(&a("search x --sort nope")).is_err());
        assert!(parse(&a("search x --wat")).is_err());
    }

    #[test]
    fn parses_index_status_and_help() {
        assert_eq!(
            parse(&a("index c:")).unwrap(),
            Cmd::Index {
                volumes: Some('C'),
                wait: false,
                timeout: None
            }
        );
        assert_eq!(
            parse(&a("index D --wait")).unwrap(),
            Cmd::Index {
                volumes: Some('D'),
                wait: true,
                timeout: None
            }
        );
        assert_eq!(
            parse(&a("index --all")).unwrap(),
            Cmd::Index {
                volumes: None,
                wait: false,
                timeout: None
            }
        );
        assert_eq!(
            parse(&a("index C --wait --timeout 600")).unwrap(),
            Cmd::Index {
                volumes: Some('C'),
                wait: true,
                timeout: Some(600)
            }
        );
        assert!(parse(&a("index C --timeout")).is_err());
        assert!(parse(&a("index C --timeout soon")).is_err());
        assert!(parse(&a("index")).is_err());
        assert!(parse(&a("index C --all")).is_err());
        assert!(parse(&a("index C:\\Users")).is_err());
        assert_eq!(
            parse(&a("status --json")).unwrap(),
            Cmd::Status { json: true }
        );
        assert_eq!(parse(&[]).unwrap(), Cmd::Help);
        assert!(parse(&a("frobnicate")).is_err());
    }

    #[test]
    fn parses_meaning_read_recent_apps() {
        assert_eq!(
            parse(&a("meaning quarterly report -n 5 -l --json")).unwrap(),
            Cmd::Meaning {
                query: "quarterly report".into(),
                limit: 5,
                long: true,
                json: true
            }
        );
        assert!(parse(&a("meaning")).is_err());
        assert!(parse(&a("meaning x -n 0")).is_err());
        assert!(parse(&a("meaning x --sort size")).is_err());
        assert_eq!(
            parse(&a("read C:\\a b\\doc.pdf --page 3 --max 5000 --json")).unwrap(),
            Cmd::Read {
                path: r"C:\a b\doc.pdf".into(),
                page: Some(3),
                max_chars: Some(5000),
                json: true
            }
        );
        assert!(parse(&a("read")).is_err());
        assert!(parse(&a("read x --page 0")).is_err());
        assert!(parse(&a("read x --max 999999")).is_err());
        assert_eq!(
            parse(&a("recent -n 5 --json")).unwrap(),
            Cmd::Recent {
                limit: 5,
                json: true
            }
        );
        assert!(parse(&a("recent --long")).is_err());
        assert_eq!(
            parse(&a("apps calendar --kind setting -n 3")).unwrap(),
            Cmd::Apps {
                query: "calendar".into(),
                limit: 3,
                kind: Some("setting".into()),
                json: false
            }
        );
        assert!(parse(&a("apps x --kind theme")).is_err());
        assert!(parse(&a("apps")).is_err());
    }

    #[test]
    fn parses_path() {
        assert_eq!(
            parse(&a("path")).unwrap(),
            Cmd::Path {
                action: PathAction::Status,
                machine: false,
                json: false
            }
        );
        assert_eq!(
            parse(&a("path add --machine")).unwrap(),
            Cmd::Path {
                action: PathAction::Add,
                machine: true,
                json: false
            }
        );
        assert_eq!(
            parse(&a("path remove --json")).unwrap(),
            Cmd::Path {
                action: PathAction::Remove,
                machine: false,
                json: true
            }
        );
        assert!(parse(&a("path frobnicate")).is_err());
    }

    #[test]
    fn formats() {
        assert_eq!(when(1_700_000_000_000), "2023-11-14 22:13");
        assert_eq!(when(0), "1970-01-01 00:00");
        assert_eq!(size_text(1023), "1023 B");
        assert_eq!(size_text(1536), "1.5 KB");
    }

    #[test]
    fn search_prints_paths_and_json() {
        let hits = vec![
            hit("a.pdf", r"C:\d\a.pdf", 2048),
            hit("b.pdf", r"C:\d\b.pdf", 1),
        ];
        let resp = Response::SearchResult {
            id: 1,
            total: 7,
            hits: hits.clone(),
            elapsed_us: 1500,
        };
        let mut send = |req: Request| -> io::Result<Response> {
            assert!(matches!(req, Request::Search { ref query, limit: 2, .. } if query == "pdf"));
            Ok(resp.clone())
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            Cmd::Search {
                query: "pdf".into(),
                limit: 2,
                sort: Sort::NameAsc,
                long: false,
                json: false,
            },
            &mut send,
            &mut out,
            &mut err,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "C:\\d\\a.pdf\nC:\\d\\b.pdf\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "2 of 7 results in 1.5 ms\n"
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            Cmd::Search {
                query: "pdf".into(),
                limit: 2,
                sort: Sort::NameAsc,
                long: false,
                json: true,
            },
            &mut send,
            &mut out,
            &mut err,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v[1]["path"], "C:\\d\\b.pdf");
        assert!(err.is_empty());
    }

    #[test]
    fn server_errors_become_failures() {
        let mut send = |_| {
            Ok(Response::Error {
                id: 1,
                message: "bad query".into(),
            })
        };
        let r = run(
            Cmd::Search {
                query: "(".into(),
                limit: 1,
                sort: Sort::NameAsc,
                long: false,
                json: false,
            },
            &mut send,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        assert_eq!(r.unwrap_err(), "bad query");
        let mut send = |_| Err(io::Error::other("pipe closed"));
        assert!(run(
            Cmd::Status { json: false },
            &mut send,
            &mut Vec::new(),
            &mut Vec::new()
        )
        .unwrap_err()
        .contains("pipe closed"));
    }

    fn vol(l: char, state: VolumeState) -> VolumeStatus {
        VolumeStatus {
            letter: l,
            files: 10,
            folders: 2,
            state,
            fs: "NTFS".into(),
            monitor: winseek_ipc::Monitor::Usn,
        }
    }

    #[test]
    fn index_all_rescans_every_volume_and_waits() {
        let mut calls: Vec<String> = Vec::new();
        let mut polls = 0;
        let mut send = |req: Request| -> io::Result<Response> {
            Ok(match req {
                Request::Status { .. } if calls.is_empty() => Response::StatusResult {
                    id: 1,
                    volumes: vec![vol('C', VolumeState::Live), vol('D', VolumeState::Live)],
                    uptime_s: 1,
                },
                Request::Status { .. } => {
                    polls += 1;
                    let st = if polls < 2 {
                        VolumeState::Rescanning
                    } else {
                        VolumeState::Live
                    };
                    Response::StatusResult {
                        id: 2,
                        volumes: vec![vol('C', st.clone()), vol('D', st)],
                        uptime_s: 1,
                    }
                }
                Request::Rescan { volume, .. } => {
                    calls.push(volume.to_string());
                    Response::StatusResult {
                        id: 1,
                        volumes: vec![],
                        uptime_s: 1,
                    }
                }
                other => panic!("unexpected {other:?}"),
            })
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(
            Cmd::Index {
                volumes: None,
                wait: true,
                timeout: None,
            },
            &mut send,
            &mut out,
            &mut err,
        )
        .unwrap();
        assert_eq!(calls, ["C", "D"]);
        let e = String::from_utf8(err).unwrap();
        assert!(
            e.contains("C: rescan started")
                && e.contains("D: rescan started")
                && e.contains("rescanning"),
            "{e}"
        );
    }

    #[test]
    fn wait_reports_a_volume_error() {
        let mut send = |_| {
            Ok(Response::StatusResult {
                id: 2,
                volumes: vec![vol(
                    'C',
                    VolumeState::Error {
                        msg: "access denied".into(),
                    },
                )],
                uptime_s: 1,
            })
        };
        assert!(wait_live(&mut send, &['C'], &mut Vec::new(), None)
            .unwrap_err()
            .contains("access denied"));
    }

    #[test]
    fn wait_gives_up_when_a_scan_stalls() {
        let mut send = |_| {
            Ok(Response::StatusResult {
                id: 2,
                volumes: vec![vol('C', VolumeState::Scanning { done: 3, total: 0 })],
                uptime_s: 1,
            })
        };
        let tiny = Duration::from_millis(30);
        let e = wait_live_with(
            &mut send,
            &['C'],
            &mut Vec::new(),
            WaitPolicy {
                stall_counted: tiny,
                stall_blind: tiny,
                poll: Duration::from_millis(5),
                cap: None,
            },
            Instant::now,
        )
        .unwrap_err();
        assert!(e.contains("no progress"), "{e}");
    }

    #[test]
    fn wait_respects_the_timeout_cap() {
        let mut send = |_| {
            Ok(Response::StatusResult {
                id: 2,
                volumes: vec![vol('C', VolumeState::Rescanning)],
                uptime_s: 1,
            })
        };
        let huge = Duration::from_secs(3600);
        let e = wait_live_with(
            &mut send,
            &['C'],
            &mut Vec::new(),
            WaitPolicy {
                stall_counted: huge,
                stall_blind: huge,
                poll: Duration::from_millis(5),
                cap: Some(Duration::from_millis(40)),
            },
            Instant::now,
        )
        .unwrap_err();
        assert!(e.contains("still not live"), "{e}");
    }

    #[test]
    fn talks_to_a_real_pipe() {
        let name = format!(
            r"\\.\pipe\winseek-cli-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut server = PipeServer::bind(&name).unwrap();
        let t = std::thread::spawn(move || {
            let mut c = server.accept().unwrap();
            let req: Request = read_message(&mut c).unwrap();
            assert!(matches!(req, Request::Search { .. }));
            write_message(
                &mut c,
                &Response::SearchResult {
                    id: 1,
                    total: 1,
                    hits: vec![hit("x.txt", r"C:\x.txt", 3)],
                    elapsed_us: 10,
                },
            )
            .unwrap();
        });
        let mut conn = connect(&name, Duration::from_secs(2)).unwrap();
        let mut send = |req: Request| -> io::Result<Response> {
            write_message(&mut conn, &req)?;
            read_message(&mut conn)
        };
        let mut out = Vec::new();
        run(
            Cmd::Search {
                query: "x".into(),
                limit: 5,
                sort: Sort::NameAsc,
                long: true,
                json: false,
            },
            &mut send,
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().contains(r"C:\x.txt"));
        t.join().unwrap();
    }

    // ---------------------------------------------------------- agent commands

    /// Fake app answering `meaning`, `read_text`, `recent_files` and `find_apps`.
    fn fake_agent(
        tool: &'static str,
        value: Value,
    ) -> impl FnMut(AgentRequest) -> io::Result<AgentResponse> {
        move |req: AgentRequest| {
            Ok(match req {
                AgentRequest::Call { tool: t, .. } if t == tool => AgentResponse::Result {
                    id: 1,
                    value: value.clone(),
                },
                AgentRequest::Call { tool: t, .. } => AgentResponse::Error {
                    id: 1,
                    message: format!("unknown tool {t}"),
                },
                other => panic!("unexpected {other:?}"),
            })
        }
    }

    #[test]
    fn meaning_prints_paths_long_and_json() {
        let hits = json!([
            {"path": r"C:\d\a.pdf", "score": 0.91, "snippet": "quarterly numbers", "page": 2,
             "source": "pdf", "size": 100, "modified": 1_700_000_000_000i64},
            {"path": r"C:\d\b.png", "score": 0.75, "snippet": "", "page": null,
             "source": "ocr", "size": 200, "modified": 1_700_000_000_001i64},
        ]);
        let result = json!({"state": "live", "hits": hits, "total": 2, "truncated": false});

        let mut send = fake_agent("search_by_meaning", result.clone());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run_agent(
            Cmd::Meaning {
                query: "quarterly".into(),
                limit: 20,
                long: false,
                json: false,
            },
            &mut send,
            &mut out,
            &mut err,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "C:\\d\\a.pdf\nC:\\d\\b.png\n"
        );
        assert!(String::from_utf8(err).unwrap().contains("2 results"));

        let mut send = fake_agent("search_by_meaning", result.clone());
        let mut out = Vec::new();
        run_agent(
            Cmd::Meaning {
                query: "q".into(),
                limit: 20,
                long: true,
                json: false,
            },
            &mut send,
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.starts_with("0.910  C:\\d\\a.pdf\n    quarterly numbers\n0.750"),
            "{s}"
        );

        let mut send = fake_agent("search_by_meaning", result);
        let mut out = Vec::new();
        run_agent(
            Cmd::Meaning {
                query: "q".into(),
                limit: 20,
                long: false,
                json: true,
            },
            &mut send,
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["state"], "live");
        assert_eq!(v["hits"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn read_prints_text_and_json() {
        let result =
            json!({"text": "hello\r\nworld", "chars": 12, "truncated": false, "source": "pdf"});
        let mut send = fake_agent("read_text", result);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run_agent(
            Cmd::Read {
                path: r"C:\d\a.pdf".into(),
                page: Some(2),
                max_chars: Some(5000),
                json: false,
            },
            &mut send,
            &mut out,
            &mut err,
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.ends_with('\n') && !s.ends_with("\n\n"), "{s:?}");
        assert!(s.contains("world"));
        assert!(String::from_utf8(err).unwrap().contains("12 chars"));

        // already ends with a newline: no extra blank line
        let result = json!({"text": "ends\n", "chars": 5, "truncated": true, "source": "txt"});
        let mut send = fake_agent("read_text", result);
        let mut out = Vec::new();
        run_agent(
            Cmd::Read {
                path: "x".into(),
                page: None,
                max_chars: None,
                json: false,
            },
            &mut send,
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "ends\n");

        let result = json!({"text": "hi", "chars": 2, "truncated": false, "source": "txt"});
        let mut send = fake_agent("read_text", result);
        let mut out = Vec::new();
        run_agent(
            Cmd::Read {
                path: "x".into(),
                page: None,
                max_chars: None,
                json: true,
            },
            &mut send,
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["source"], "txt");
    }

    #[test]
    fn recent_and_apps_print_rows() {
        let result = json!({"total": 1, "hits": [{"name": "a.txt", "path": r"C:\a.txt",
            "is_dir": false, "size": 10, "modified": 1_700_000_000_000i64}], "truncated": false});
        let mut send = fake_agent("recent_files", result);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run_agent(
            Cmd::Recent {
                limit: 30,
                json: false,
            },
            &mut send,
            &mut out,
            &mut err,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "C:\\a.txt\n");
        assert!(String::from_utf8(err).unwrap().contains("1 recent files"));

        let result = json!({"total": 2, "hits": [
            {"id": "x", "kind": "app", "title": "Notepad", "subtitle": "", "score": 1.0,
             "target": {"kind": "path", "path": r"C:\Windows\notepad.exe", "args": null}},
            {"id": "y", "kind": "setting", "title": "Dark mode", "subtitle": "", "score": 0.5,
             "target": {"kind": "uri", "uri": "ms-settings:themes"}},
            {"id": "z", "kind": "command", "title": "Sleep", "subtitle": "", "score": 0.4,
             "target": {"kind": "command", "name": "sleep"}},
        ], "truncated": false});
        let mut send = fake_agent("find_apps", result);
        let mut out = Vec::new();
        run_agent(
            Cmd::Apps {
                query: "n".into(),
                limit: 10,
                kind: None,
                json: false,
            },
            &mut send,
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(r"C:\Windows\notepad.exe"), "{s}");
        assert!(s.contains("ms-settings:themes"), "{s}");
        assert!(s.contains("sleep"), "{s}");
    }

    #[test]
    fn agent_errors_surface_verbatim() {
        let mut send = |_req: AgentRequest| -> io::Result<AgentResponse> {
            Ok(AgentResponse::Error {
                id: 1,
                message: "Agent access is off. Turn it on in winseek Settings › Agents.".into(),
            })
        };
        let e = run_agent(
            Cmd::Meaning {
                query: "q".into(),
                limit: 1,
                long: false,
                json: false,
            },
            &mut send,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            e,
            "Agent access is off. Turn it on in winseek Settings › Agents."
        );
    }

    #[test]
    fn talks_to_a_real_agent_pipe() {
        let name = format!(
            r"\\.\pipe\winseek-cli-agent-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut server = PipeServer::bind(&name).unwrap();
        let t = std::thread::spawn(move || {
            let mut c = server.accept().unwrap();
            let req: AgentRequest = read_message(&mut c).unwrap();
            assert!(
                matches!(req, AgentRequest::Call { ref tool, ref client, .. }
                    if tool == "recent_files" && client == "winseek-cli")
            );
            write_message(
                &mut c,
                &AgentResponse::Result {
                    id: 1,
                    value: json!({"total": 1, "hits": [{"name": "a", "path": r"C:\a.txt",
                        "is_dir": false, "size": 1, "modified": 1}], "truncated": false}),
                },
            )
            .unwrap();
        });
        let mut conn = connect(&name, Duration::from_secs(2)).unwrap();
        let mut send = |req: AgentRequest| -> io::Result<AgentResponse> {
            write_message(&mut conn, &req)?;
            conn.read_message_timeout(ROUND_TRIP)
        };
        let mut out = Vec::new();
        run_agent(
            Cmd::Recent {
                limit: 30,
                json: false,
            },
            &mut send,
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().contains(r"C:\a.txt"));
        t.join().unwrap();
    }

    #[test]
    fn path_status_text_shows_where_the_dir_is() {
        assert_eq!(
            path_status_text(
                r"C:\Program Files\winseek",
                r"C:\Windows;C:\Program Files\winseek",
                r"C:\Windows",
            ),
            "C:\\Program Files\\winseek\nuser PATH:   yes\nmachine PATH: no"
        );
        assert_eq!(
            path_status_text(r"C:\w", "", ""),
            "C:\\w\nuser PATH:   no\nmachine PATH: no"
        );
    }

    #[test]
    fn app_target_picks_the_primary_value() {
        assert_eq!(
            app_target(
                &json!({"title": "N", "target": {"kind": "path", "path": "p", "args": null}})
            ),
            "p"
        );
        assert_eq!(
            app_target(&json!({"title": "D", "target": {"kind": "uri", "uri": "ms-settings:x"}})),
            "ms-settings:x"
        );
        assert_eq!(
            app_target(&json!({"title": "C", "target": {"kind": "command", "name": "sleep"}})),
            "sleep"
        );
        // aumid prefers the real path behind it, falls back to the aumid
        assert_eq!(
            app_target(
                &json!({"title": "A", "target": {"kind": "aumid", "aumid": "X!App", "path": r"C:\p.exe"}})
            ),
            r"C:\p.exe"
        );
        assert_eq!(
            app_target(
                &json!({"title": "A", "target": {"kind": "aumid", "aumid": "X!App", "path": null}})
            ),
            "X!App"
        );
        // no target at all: the title
        assert_eq!(app_target(&json!({"title": "T", "target": null})), "T");
    }
}
