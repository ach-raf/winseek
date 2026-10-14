//! Ask a running searchd for its status and one search over the real pipe (install checks):
//! `cargo run -p winseek-ipc --example probe -- [query]`
#[cfg(windows)]
fn main() -> std::io::Result<()> {
    use std::time::Duration;
    use winseek_ipc::{pipe, read_message, write_message, Request, Response, Sort, PIPE_NAME};

    let query = std::env::args().nth(1).unwrap_or_else(|| "winseek".into());
    let mut c = pipe::connect(PIPE_NAME, Duration::from_secs(2))?;
    write_message(&mut c, &Request::Status { id: 1 })?;
    if let Response::StatusResult { volumes, uptime_s, .. } = read_message::<_, Response>(&mut c)? {
        println!("searchd up {uptime_s} s");
        for v in volumes {
            println!("  {}: {} files, {} folders, {:?} ({} / {:?})", v.letter, v.files, v.folders, v.state, v.fs, v.monitor);
        }
    }
    write_message(&mut c, &Request::Search { id: 2, query: query.clone(), offset: 0, limit: 5, sort: Sort::NameAsc })?;
    match read_message::<_, Response>(&mut c)? {
        Response::SearchResult { total, hits, elapsed_us, .. } => {
            println!("search {query:?}: {total} hits in {elapsed_us} us");
            for h in hits {
                println!("  {h:?}");
            }
        }
        other => println!("unexpected reply: {other:?}"),
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
