//! Ask a running searchd for disk usage over the real pipe and print what comes back with timings:
//! `cargo run -p winseek-ipc --example usage_probe -- [path ...]` (no path = the drive list).
#[cfg(windows)]
fn main() -> std::io::Result<()> {
    use std::time::{Duration, Instant};
    use winseek_ipc::{pipe, read_message, write_message, Request, Response, PIPE_NAME};

    let mut c = pipe::connect(PIPE_NAME, Duration::from_secs(2))?;
    let mut paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        paths.push(String::new());
    }
    for (i, p) in paths.iter().enumerate() {
        let t = Instant::now();
        write_message(&mut c, &Request::Usage { id: i as u64 + 1, path: p.clone(), limit: 0 })?;
        let r = read_message::<_, Response>(&mut c)?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match r {
            Response::UsageResult { usage, .. } => {
                println!("{p:?}: {ms:.0} ms  size={} files={} folders={} entries={} more={}", usage.size, usage.files, usage.folders, usage.entries.len(), usage.more);
                for e in usage.entries.iter().take(12) {
                    println!("   {:<28} dir={} kind={} size={} files={} path={}", e.name, e.is_dir, e.kind, e.size, e.files, e.path);
                }
            }
            other => println!("{p:?}: {ms:.0} ms  {other:?}"),
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
