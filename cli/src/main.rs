//! winseek-cli: search the name index and rescan volumes from a terminal, over the searchd named
//! pipe (the same pipe the app uses; only interactive users may open it).
//!
//! ```text
//! winseek-cli search <query...> [-n N] [--sort name|size|date|type|path[_desc|_asc]] [--long] [--json]
//! winseek-cli status [--json]
//! winseek-cli index [C|D:|--all] [--wait]     rescan volume(s) from the file table
//! ```
//!
//! Output goes to stdout (one path per line by default, so it pipes); notes and errors go to
//! stderr. Exit code: 0 ok, 1 failed (service not running, bad volume...), 2 bad usage.

use std::io::{self, Write};
use std::time::Duration;

use winseek_ipc::pipe::connect;
use winseek_ipc::{read_message, write_message, Hit, Request, Response, Sort, VolumeState, VolumeStatus, PIPE_NAME};

const USAGE: &str = "\
winseek-cli - search the winseek name index from a terminal

USAGE
  winseek-cli search <query...> [-n N] [--sort ORDER] [--long] [--json]
  winseek-cli status [--json]
  winseek-cli index [VOLUME | --all] [--wait]

search   Name search with the same syntax as the app (ext:pdf, size:>10mb, dm:today, folder:...).
         -n N        at most N results (default 50)
         --sort      name (default), size, date, type, path; add _desc / _asc to flip (size_asc, date_asc, name_desc...)
         --long      add size and modified date
         --json      JSON array of {name, path, is_dir, size, mtime}
status   Volumes, file counts and state of the name index.
index    Rescan a volume (C or C:) or all of them from the file table. --wait polls until it is live again.

Needs the winseek name service (searchd) running. Exit code 0 ok, 1 failed, 2 bad usage.";

#[derive(Debug, PartialEq)]
enum Cmd {
    Help,
    Version,
    Search { query: String, limit: u32, sort: Sort, long: bool, json: bool },
    Status { json: bool },
    Index { volumes: Option<char>, wait: bool },
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
        other => return Err(format!("unknown sort '{other}' (name, size, date, type, path; _asc / _desc)")),
    })
}

fn parse(args: &[String]) -> Result<Cmd, String> {
    let Some((cmd, rest)) = args.split_first() else { return Ok(Cmd::Help) };
    match cmd.as_str() {
        "-h" | "--help" | "help" => Ok(Cmd::Help),
        "-V" | "--version" | "version" => Ok(Cmd::Version),
        "search" | "s" => {
            let (mut words, mut limit, mut sort, mut long, mut json) = (Vec::new(), 50u32, Sort::NameAsc, false, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-n" | "--limit" => {
                        limit = it.next().and_then(|v| v.parse().ok()).filter(|n| *n > 0).ok_or("-n needs a positive number")?;
                    }
                    "--sort" => sort = sort_of(it.next().ok_or("--sort needs a value")?)?,
                    "--long" | "-l" => long = true,
                    "--json" => json = true,
                    "--" => words.extend(it.by_ref().cloned()),
                    f if f.starts_with('-') && f.len() > 1 && !f.starts_with("-\"") => return Err(format!("unknown option {f}")),
                    w => words.push(w.to_string()),
                }
            }
            let query = words.join(" ");
            if query.trim().is_empty() {
                return Err("search needs a query".into());
            }
            Ok(Cmd::Search { query, limit, sort, long, json })
        }
        "status" => match rest {
            [] => Ok(Cmd::Status { json: false }),
            [f] if f == "--json" => Ok(Cmd::Status { json: true }),
            _ => Err("usage: winseek-cli status [--json]".into()),
        },
        "index" | "rescan" => {
            let (mut vol, mut all, mut wait) = (None, false, false);
            for a in rest {
                match a.as_str() {
                    "--all" => all = true,
                    "--wait" => wait = true,
                    v if v.trim_end_matches(':').chars().count() == 1 && v.trim_end_matches(':').chars().all(|c| c.is_ascii_alphabetic()) => {
                        vol = Some(v.chars().next().unwrap().to_ascii_uppercase());
                    }
                    other => return Err(format!("not a volume: {other}")),
                }
            }
            match (vol, all) {
                (Some(_), true) => Err("give a volume or --all, not both".into()),
                (None, false) => Err("index needs a volume (C or C:) or --all".into()),
                (v, _) => Ok(Cmd::Index { volumes: v, wait }),
            }
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

/// One request/response round trip.
type Send<'a> = dyn FnMut(Request) -> io::Result<Response> + 'a;

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
        VolumeState::Scanning { done, total } if *total > 0 => format!("scanning {}%", done.min(total) * 100 / total),
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
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

fn size_text(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

fn print_hits(out: &mut dyn Write, hits: &[Hit], long: bool, json: bool) -> io::Result<()> {
    if json {
        let v: Vec<_> = hits
            .iter()
            .map(|h| serde_json::json!({"name": h.name, "path": h.path, "is_dir": h.is_dir, "size": h.size, "mtime": h.mtime}))
            .collect();
        return writeln!(out, "{}", serde_json::to_string(&v).unwrap_or_else(|_| "[]".into()));
    }
    for h in hits {
        if long {
            let size = if h.is_dir { "<dir>".to_string() } else { h.size.map(size_text).unwrap_or_default() };
            writeln!(out, "{:>10}  {:16}  {}", size, h.mtime.map(when).unwrap_or_default(), h.path)?;
        } else {
            writeln!(out, "{}", h.path)?;
        }
    }
    Ok(())
}

fn run(cmd: Cmd, send: &mut Send, out: &mut dyn Write, err: &mut dyn Write) -> Result<(), String> {
    match cmd {
        Cmd::Help => writeln!(out, "{USAGE}").map_err(fail),
        Cmd::Version => writeln!(out, "winseek-cli {}", env!("CARGO_PKG_VERSION")).map_err(fail),
        Cmd::Search { query, limit, sort, long, json } => {
            match send(Request::Search { id: 1, query, offset: 0, limit, sort }).map_err(fail)? {
                Response::SearchResult { total, hits, elapsed_us, .. } => {
                    print_hits(out, &hits, long, json).map_err(fail)?;
                    if !json {
                        let _ = writeln!(err, "{} of {} results in {:.1} ms", hits.len(), total, elapsed_us as f64 / 1000.0);
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
                writeln!(out, "{}", serde_json::to_string(&v).unwrap_or_else(|_| "[]".into())).map_err(fail)
            } else {
                for v in &vols {
                    writeln!(out, "{}:  {:>12} files  {:>10} folders  {:6}  {}", v.letter, v.files, v.folders, v.fs, state_text(&v.state)).map_err(fail)?;
                }
                Ok(())
            }
        }
        Cmd::Index { volumes, wait } => {
            let letters: Vec<char> = match volumes {
                Some(v) => vec![v],
                None => expect_status(send(Request::Status { id: 1 }).map_err(fail)?)?.into_iter().map(|v| v.letter).collect(),
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
                wait_live(send, &letters, err)?;
            }
            Ok(())
        }
    }
}

/// Poll until none of `letters` is scanning or rescanning (or errored).
fn wait_live(send: &mut Send, letters: &[char], err: &mut dyn Write) -> Result<(), String> {
    let mut last = String::new();
    loop {
        let vols = expect_status(send(Request::Status { id: 2 }).map_err(fail)?)?;
        let mine: Vec<&VolumeStatus> = vols.iter().filter(|v| letters.contains(&v.letter)).collect();
        if let Some(bad) = mine.iter().find(|v| matches!(v.state, VolumeState::Error { .. })) {
            return Err(format!("{}: {}", bad.letter, state_text(&bad.state)));
        }
        let busy: Vec<String> = mine
            .iter()
            .filter(|v| matches!(v.state, VolumeState::Scanning { .. } | VolumeState::Rescanning))
            .map(|v| format!("{}: {}", v.letter, state_text(&v.state)))
            .collect();
        if busy.is_empty() {
            return Ok(());
        }
        let line = busy.join("  ");
        if line != last {
            let _ = writeln!(err, "{line}");
            last = line;
        }
        std::thread::sleep(Duration::from_millis(1000));
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
    if matches!(cmd, Cmd::Help | Cmd::Version) {
        let _ = run(cmd, &mut |_| Err(io::Error::other("no pipe needed")), &mut out, &mut err);
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
        read_message(&mut conn)
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

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn hit(name: &str, path: &str, size: u64) -> Hit {
        Hit { name: name.into(), path: path.into(), is_dir: false, size: Some(size), mtime: Some(1_700_000_000_000) }
    }

    #[test]
    fn parses_search() {
        assert_eq!(
            parse(&a("search invoice ext:pdf -n 5 --sort size --json")).unwrap(),
            Cmd::Search { query: "invoice ext:pdf".into(), limit: 5, sort: Sort::SizeDesc, long: false, json: true }
        );
        assert_eq!(parse(&a("s foo --sort date_asc -l")).unwrap(), Cmd::Search { query: "foo".into(), limit: 50, sort: Sort::MtimeAsc, long: true, json: false });
        // `--` lets a query start with a dash
        assert!(matches!(parse(&a("search -- -draft")).unwrap(), Cmd::Search { query, .. } if query == "-draft"));
        assert!(parse(&a("search")).is_err());
        assert!(parse(&a("search x -n 0")).is_err());
        assert!(parse(&a("search x --sort nope")).is_err());
        assert!(parse(&a("search x --wat")).is_err());
    }

    #[test]
    fn parses_index_status_and_help() {
        assert_eq!(parse(&a("index c:")).unwrap(), Cmd::Index { volumes: Some('C'), wait: false });
        assert_eq!(parse(&a("index D --wait")).unwrap(), Cmd::Index { volumes: Some('D'), wait: true });
        assert_eq!(parse(&a("index --all")).unwrap(), Cmd::Index { volumes: None, wait: false });
        assert!(parse(&a("index")).is_err());
        assert!(parse(&a("index C --all")).is_err());
        assert!(parse(&a("index C:\\Users")).is_err());
        assert_eq!(parse(&a("status --json")).unwrap(), Cmd::Status { json: true });
        assert_eq!(parse(&[]).unwrap(), Cmd::Help);
        assert!(parse(&a("frobnicate")).is_err());
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
        let hits = vec![hit("a.pdf", r"C:\d\a.pdf", 2048), hit("b.pdf", r"C:\d\b.pdf", 1)];
        let resp = Response::SearchResult { id: 1, total: 7, hits: hits.clone(), elapsed_us: 1500 };
        let mut send = |req: Request| -> io::Result<Response> {
            assert!(matches!(req, Request::Search { ref query, limit: 2, .. } if query == "pdf"));
            Ok(resp.clone())
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(Cmd::Search { query: "pdf".into(), limit: 2, sort: Sort::NameAsc, long: false, json: false }, &mut send, &mut out, &mut err).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "C:\\d\\a.pdf\nC:\\d\\b.pdf\n");
        assert_eq!(String::from_utf8(err).unwrap(), "2 of 7 results in 1.5 ms\n");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(Cmd::Search { query: "pdf".into(), limit: 2, sort: Sort::NameAsc, long: false, json: true }, &mut send, &mut out, &mut err).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v[1]["path"], "C:\\d\\b.pdf");
        assert!(err.is_empty());
    }

    #[test]
    fn server_errors_become_failures() {
        let mut send = |_| Ok(Response::Error { id: 1, message: "bad query".into() });
        let r = run(Cmd::Search { query: "(".into(), limit: 1, sort: Sort::NameAsc, long: false, json: false }, &mut send, &mut Vec::new(), &mut Vec::new());
        assert_eq!(r.unwrap_err(), "bad query");
        let mut send = |_| Err(io::Error::other("pipe closed"));
        assert!(run(Cmd::Status { json: false }, &mut send, &mut Vec::new(), &mut Vec::new()).unwrap_err().contains("pipe closed"));
    }

    fn vol(l: char, state: VolumeState) -> VolumeStatus {
        VolumeStatus { letter: l, files: 10, folders: 2, state, fs: "NTFS".into(), monitor: winseek_ipc::Monitor::Usn }
    }

    #[test]
    fn index_all_rescans_every_volume_and_waits() {
        let mut calls: Vec<String> = Vec::new();
        let mut polls = 0;
        let mut send = |req: Request| -> io::Result<Response> {
            Ok(match req {
                Request::Status { .. } if calls.is_empty() => Response::StatusResult { id: 1, volumes: vec![vol('C', VolumeState::Live), vol('D', VolumeState::Live)], uptime_s: 1 },
                Request::Status { .. } => {
                    polls += 1;
                    let st = if polls < 2 { VolumeState::Rescanning } else { VolumeState::Live };
                    Response::StatusResult { id: 2, volumes: vec![vol('C', st.clone()), vol('D', st)], uptime_s: 1 }
                }
                Request::Rescan { volume, .. } => {
                    calls.push(volume.to_string());
                    Response::StatusResult { id: 1, volumes: vec![], uptime_s: 1 }
                }
                other => panic!("unexpected {other:?}"),
            })
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(Cmd::Index { volumes: None, wait: true }, &mut send, &mut out, &mut err).unwrap();
        assert_eq!(calls, ["C", "D"]);
        let e = String::from_utf8(err).unwrap();
        assert!(e.contains("C: rescan started") && e.contains("D: rescan started") && e.contains("rescanning"), "{e}");
    }

    #[test]
    fn wait_reports_a_volume_error() {
        let mut send = |_| Ok(Response::StatusResult { id: 2, volumes: vec![vol('C', VolumeState::Error { msg: "access denied".into() })], uptime_s: 1 });
        assert!(wait_live(&mut send, &['C'], &mut Vec::new()).unwrap_err().contains("access denied"));
    }

    #[test]
    fn talks_to_a_real_pipe() {
        let name = format!(r"\\.\pipe\winseek-cli-test-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let mut server = PipeServer::bind(&name).unwrap();
        let t = std::thread::spawn(move || {
            let mut c = server.accept().unwrap();
            let req: Request = read_message(&mut c).unwrap();
            assert!(matches!(req, Request::Search { .. }));
            write_message(&mut c, &Response::SearchResult { id: 1, total: 1, hits: vec![hit("x.txt", r"C:\x.txt", 3)], elapsed_us: 10 }).unwrap();
        });
        let mut conn = connect(&name, Duration::from_secs(2)).unwrap();
        let mut send = |req: Request| -> io::Result<Response> {
            write_message(&mut conn, &req)?;
            read_message(&mut conn)
        };
        let mut out = Vec::new();
        run(Cmd::Search { query: "x".into(), limit: 5, sort: Sort::NameAsc, long: true, json: false }, &mut send, &mut out, &mut Vec::new()).unwrap();
        assert!(String::from_utf8(out).unwrap().contains(r"C:\x.txt"));
        t.join().unwrap();
    }
}
