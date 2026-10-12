//! Elevated read-strategy benchmark for the $MFT sweep.
//!
//! ```text
//! cargo run -p winseek-ntfs --example scan_bench --release -- C [mode...]
//! ```
//!
//! A mode is `-`/`:`-separated tokens: `nobuf` | `cached`, `qdN`, and a batch
//! size `NNk` / `NNm` (default 2m). Example: `nobuf-qd8-1m cached-qd1-2m`.
//! Default modes: nobuf qd1/4/8/16 @1m, nobuf qd8 @4m, then cached qd1 @2m
//! (last; it reads through the file cache, which an earlier cached run or
//! other activity may have warmed). Output goes to stdout AND `scan_bench.txt`
//! in the current directory (stdout is lost when launched elevated).

use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use winseek_ntfs::{scan_volume_with, ScanOptions, ScanStats};

fn parse_mode(s: &str) -> Option<ScanOptions> {
    let mut o = ScanOptions { no_buffering: false, queue_depth: 1, batch_bytes: 2 << 20 };
    let mut have_buf = false;
    for tok in s.to_ascii_lowercase().split(['-', ':', ',']).filter(|t| !t.is_empty()) {
        match tok {
            "nobuf" => (o.no_buffering, have_buf) = (true, true),
            "cached" => (o.no_buffering, have_buf) = (false, true),
            _ => {
                if let Some(n) = tok.strip_prefix("qd") {
                    o.queue_depth = n.parse().ok().filter(|&n| n >= 1)?;
                } else if let Some(n) = tok.strip_suffix('m') {
                    o.batch_bytes = n.parse::<usize>().ok().filter(|&n| n >= 1)? << 20;
                } else {
                    let n = tok.strip_suffix('k')?;
                    o.batch_bytes = n.parse::<usize>().ok().filter(|&n| n >= 1)? << 10;
                }
            }
        }
    }
    have_buf.then_some(o)
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let letter = args.next().and_then(|a| a.chars().next()).unwrap_or('C').to_ascii_uppercase();
    let mut modes: Vec<ScanOptions> = Vec::new();
    for a in args {
        match parse_mode(&a) {
            Some(o) => modes.push(o),
            None => eprintln!("ignoring unparsable mode {a:?}"),
        }
    }
    if modes.is_empty() {
        let nb = |qd, mib: usize| ScanOptions { no_buffering: true, queue_depth: qd, batch_bytes: mib << 20 };
        modes = vec![
            nb(1, 1),
            nb(4, 1),
            nb(8, 1),
            nb(16, 1),
            nb(8, 4),
            ScanOptions { no_buffering: false, queue_depth: 1, batch_bytes: 2 << 20 },
        ];
    }

    let path = std::env::current_dir().unwrap_or_default().join("scan_bench.txt");
    let mut file = File::create(&path).ok();
    let mut say = |s: String| {
        println!("{s}");
        if let Some(f) = file.as_mut() {
            let _ = writeln!(f, "{s}");
            let _ = f.flush();
        }
    };
    say(format!("scan_bench {letter}: -> {}", path.display()));
    say(format!("{} modes: {}", modes.len(), modes.iter().map(|m| m.label()).collect::<Vec<_>>().join(", ")));

    struct Row {
        label: String,
        stats: Option<(ScanStats, u64)>,
        err: Option<String>,
    }
    let mut rows: Vec<Row> = Vec::new();
    for m in &modes {
        say(format!("--- {} ...", m.label()));
        let mut entries = 0u64;
        let t = Instant::now();
        let res = scan_volume_with(letter, *m, &mut |_e| entries += 1, &mut |_d, _t| {});
        let wall = t.elapsed();
        match res {
            Ok(st) => {
                say(format!(
                    "    elapsed {:.2}s (wall {:.2}s)  read_time {:.2}s  buf_wait {:.2}s  decode {:.2}s  sink {:.2}s  main_wait {:.2}s",
                    secs(st.elapsed), secs(wall), secs(st.read_time), secs(st.reader_buf_wait),
                    secs(st.decode_time), secs(st.sink_time), secs(st.main_wait)
                ));
                say(format!(
                    "    {:.1} MB read, extents {}, reads {}, avg read {} KiB, records {}, entries {}",
                    st.bytes_read as f64 / 1e6, st.extents, st.reads, st.avg_read_bytes() / 1024,
                    st.mft_records, entries
                ));
                rows.push(Row { label: m.label(), stats: Some((st, entries)), err: None });
            }
            Err(e) => {
                say(format!("    FAILED: {e}"));
                rows.push(Row { label: m.label(), stats: None, err: Some(e.to_string()) });
            }
        }
    }

    say(String::new());
    say(format!(
        "{:<18} {:>8} {:>8} {:>9} {:>8} {:>7} {:>7} {:>9} {:>10}",
        "mode", "elapsed", "MB/s", "read_time", "extents", "reads", "avgKiB", "decode", "entries"
    ));
    for r in &rows {
        match &r.stats {
            Some((st, entries)) => {
                let e = secs(st.elapsed);
                say(format!(
                    "{:<18} {:>7.2}s {:>8.0} {:>8.2}s {:>8} {:>7} {:>7} {:>8.2}s {:>10}",
                    r.label, e, st.bytes_read as f64 / 1e6 / e.max(1e-9), secs(st.read_time), st.extents,
                    st.reads, st.avg_read_bytes() / 1024, secs(st.decode_time), entries
                ));
            }
            None => say(format!("{:<18} FAILED: {}", r.label, r.err.as_deref().unwrap_or("?"))),
        }
    }
    let counts: Vec<u64> = rows.iter().filter_map(|r| r.stats.as_ref().map(|s| s.1)).collect();
    if counts.windows(2).any(|w| w[0] != w[1]) {
        say("WARNING: entry counts differ between modes (read path bug?)".into());
    }
    say("note: the cached run reads through the file cache and may be warm; compare nobuf rows first.".into());
}
