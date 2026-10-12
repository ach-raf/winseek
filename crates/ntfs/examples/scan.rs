//! Elevated dev tool: `cargo run -p winseek-ntfs --example scan --release -- C`
//! Scans one NTFS volume, prints count + elapsed + first 20 entries. Output
//! goes to stdout AND `%TEMP%\winseek-scan-<letter>.log` (stdout is lost when
//! the process is launched elevated).

use std::fs::File;
use std::io::Write;
use std::time::Instant;

fn main() {
    let letter = std::env::args()
        .nth(1)
        .and_then(|a| a.chars().next())
        .unwrap_or('C')
        .to_ascii_uppercase();
    let path = std::env::temp_dir().join(format!("winseek-scan-{letter}.log"));
    let mut log = File::create(&path).ok();
    let mut say = |s: String| {
        println!("{s}");
        if let Some(f) = log.as_mut() {
            let _ = writeln!(f, "{s}");
            let _ = f.flush();
        }
    };
    say(format!("log file: {}", path.display()));
    say(format!("volumes: {:?}", winseek_ntfs::list_ntfs_volumes()));

    let t = Instant::now();
    let mut count = 0u64;
    let mut first: Vec<winseek_ntfs::MftEntry> = Vec::new();
    let mut last_pct = 0u64;
    let mut prog_lines: Vec<String> = Vec::new();
    let res = winseek_ntfs::scan_volume(
        letter,
        &mut |e| {
            count += 1;
            if first.len() < 20 {
                first.push(e);
            }
        },
        &mut |done, total| {
            let pct = done * 100 / total.max(1);
            if pct >= last_pct + 10 {
                last_pct = pct;
                prog_lines.push(format!("progress {pct}% ({done}/{total})"));
            }
        },
    );
    for l in prog_lines {
        say(l);
    }
    match res {
        Ok(stats) => {
            say(format!("entries: {count}  elapsed: {:.2?}", t.elapsed()));
            say(format!("stats: {stats:?}"));
            for e in &first {
                say(format!("{e:?}"));
            }
        }
        Err(e) => say(format!("scan failed: {e}")),
    }
}
