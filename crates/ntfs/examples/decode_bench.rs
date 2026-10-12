//! Offline decode benchmark (no disk, no elevation):
//! `cargo run -p winseek-ntfs --example decode_bench --release --features testutil -- [records] [threads...]`
//! Builds a mixed synthetic template (files, dirs, hard links, DOS aliases,
//! deleted/zeroed slots, $ATTRIBUTE_LIST bases + extension records), then
//! decodes `records` records (default 2,000,000) at each thread count.

use winseek_ntfs::record::testutil::Rb;

fn template(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * 1024);
    for i in 0..n {
        let parent = 5u64 | (5 << 48);
        let rec = match i % 16 {
            0 => Rb::new(true, true).std_info(130_000_000_000_000_000, 0x10).name(parent, &format!("dir{i}"), 1).seal(),
            1 => vec![0u8; 1024],
            2 => Rb::new(false, false).name(parent, "deleted.tmp", 1).seal(),
            3 => Rb::new(true, false)
                .std_info(130_000_000_000_000_000, 0x20)
                .name(parent, "LONGFI~1.TXT", 2)
                .name(parent, &format!("long file name number {i}.txt"), 1)
                .data_resident(120)
                .seal(),
            4 => Rb::new(true, false)
                .std_info(130_000_000_000_000_000, 0x20)
                .name(parent, &format!("link{i}.bin"), 1)
                .name(9 | (1 << 48), &format!("other{i}.bin"), 1)
                .data_nonres(0, 1 << 20, 200)
                .seal(),
            // base with attribute list, its extension is the next slot
            5 => Rb::new(true, false).name(parent, &format!("frag{i}.dat"), 1).attr_list().seal(),
            6 => Rb::new(true, false).base((i - 1) as u64 | (7 << 48)).data_nonres(0, 5 << 20, 64).seal(),
            7 => Rb::new(true, false).std_info(1, 0x20).name(parent, &format!("日本語ファイル{i}.docx"), 1).data_nonres(0, 90_000, 24).seal(),
            _ => Rb::new(true, false)
                .std_info(130_000_000_000_000_000, 0x20)
                .name(parent, &format!("file_{i}.txt"), 1)
                .data_resident((i % 700) as u32)
                .seal(),
        };
        out.extend_from_slice(&rec);
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let total: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(2_000_000);
    let threads: Vec<usize> = if args.len() > 1 {
        args[1..].iter().filter_map(|a| a.parse().ok()).collect()
    } else {
        vec![1, 2, 4, 8]
    };
    let per = 2048;
    let tpl = template(per);
    let batches = total / per;
    println!("template {per} records, {batches} batches = {} records", batches * per);
    {
        let mut buf = tpl.clone();
        let t0 = std::time::Instant::now();
        for _ in 0..batches {
            buf.copy_from_slice(&tpl);
            std::hint::black_box(&buf);
        }
        println!("memcpy-only overhead included in the numbers below: {:.3}s", t0.elapsed().as_secs_f64());
    }
    for &t in &threads {
        let mut best = f64::MAX;
        let mut last = (0, 0);
        for _ in 0..3 {
            let (recs, entries, el) = winseek_ntfs::bench_decode(&tpl, 1024, batches, t);
            best = best.min(el.as_secs_f64());
            last = (recs, entries);
        }
        println!(
            "threads={t:<2} records={} entries={} best={:.3}s  {:.2} M rec/s",
            last.0,
            last.1,
            best,
            last.0 as f64 / best / 1e6
        );
    }
}
