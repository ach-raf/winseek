//! Quick client check: `cargo run -p searchd --example query -- <query> [limit]`
//! Connects to the running searchd, prints status, total, the first 10 paths and the elapsed time.

use std::time::{Duration, Instant};

use winseek_ipc::pipe::connect;
use winseek_ipc::{read_message, write_message, Request, Response, Sort, PIPE_NAME};

fn main() {
    let mut args = std::env::args().skip(1);
    let query = args.next().unwrap_or_else(|| "readme".into());
    let limit: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
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
