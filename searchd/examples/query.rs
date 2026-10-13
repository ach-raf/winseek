//! Quick client check: `cargo run -p searchd --example query -- <query> [limit]` (or `-- --rescan C`)
//! Connects to the running searchd, prints status, total, the first 10 paths and the elapsed time.

use std::time::{Duration, Instant};

use winseek_ipc::pipe::connect;
use winseek_ipc::{read_message, write_message, Request, Response, Sort, PIPE_NAME};

fn main() {
    let mut args = std::env::args().skip(1);
    let query = args.next().unwrap_or_else(|| "readme".into());
    let mut c = match connect(PIPE_NAME, Duration::from_secs(2)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot connect to searchd ({PIPE_NAME}): {e}");
            std::process::exit(1);
        }
    };
    let mut call = |req: Request| -> Response {
        write_message(&mut c, &req).expect("send");
        read_message(&mut c).expect("receive")
    };
    if query == "--rescan" {
        let volume = args.next().and_then(|s| s.chars().next()).unwrap_or('C').to_ascii_uppercase();
        println!("{:?}", call(Request::Rescan { id: 1, volume }));
        return;
    }
    // Script modes (used by scripts/p1-gate.ps1): `--status`, `--count <q>`, `--list <q>`.
    match query.as_str() {
        "--status" => {
            if let Response::StatusResult { volumes, .. } = call(Request::Status { id: 1 }) {
                for v in volumes {
                    println!("STATUS {} {} {} {:?}", v.letter, v.files, v.folders, v.state);
                }
            }
            return;
        }
        "--count" | "--list" => {
            let q = args.next().unwrap_or_default();
            let mut offset = 0u32;
            loop {
                let lim = if query == "--count" { 1 } else { 10_000 };
                match call(Request::Search { id: 2, query: q.clone(), offset, limit: lim, sort: Sort::NameAsc }) {
                    Response::SearchResult { total: t, hits, .. } => {
                        if query == "--count" {
                            println!("COUNT {t}");
                            return;
                        }
                        for h in &hits {
                            println!("{}{}", h.path, if h.is_dir { "\\" } else { "" });
                        }
                        offset += hits.len() as u32;
                        if hits.is_empty() || u64::from(offset) >= t {
                            break;
                        }
                    }
                    other => {
                        eprintln!("unexpected: {other:?}");
                        std::process::exit(2);
                    }
                }
            }
            return;
        }
        _ => {}
    }
    // Only the plain `<query> [limit]` form takes a limit; script modes above consume their own args.
    let limit: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
    if let Response::StatusResult { volumes, uptime_s, .. } = call(Request::Status { id: 1 }) {
        println!("uptime {uptime_s}s");
        for v in volumes {
            println!("  {}: {} files, {} folders, {:?}", v.letter, v.files, v.folders, v.state);
        }
    }
    let t = Instant::now();
    match call(Request::Search { id: 2, query: query.clone(), offset: 0, limit, sort: Sort::NameAsc }) {
        Response::SearchResult { total, hits, elapsed_us, .. } => {
            println!("query {query:?}: {total} matches (server {elapsed_us} us, round trip {:.2?})", t.elapsed());
            for h in hits.iter().take(10) {
                println!("  {}{}", h.path, if h.is_dir { "\\" } else { "" });
            }
        }
        other => println!("unexpected: {other:?}"),
    }
    if let Response::RecentResult { hits, .. } = call(Request::Recent { id: 3, limit: 5 }) {
        println!("recent changes: {}", hits.len());
        for h in hits {
            println!("  {}", h.path);
        }
    }
}
