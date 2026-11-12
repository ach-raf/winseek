//! Synthetic benchmark: `cargo run -p winseek-index --release --example bench [records]`
use std::time::Instant;
use winseek_index::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "report", "annual", "design", "doc", "notes", "photo", "vacation", "IMG", "main", "index", "config", "readme", "kernel", "driver",
    "system", "backup", "invoice", "budget", "project", "draft", "final", "archive", "data", "test", "build", "release", "debug",
    "Windows", "Program", "Users", "Documents", "Pictures", "src", "lib", "node_modules", "cache", "temp", "log", "update", "setup",
    "été", "Müller", "résumé", "日本語", "مرحبا", "tmp", "old", "new", "copy", "screenshot", "music", "video", "game", "save",
];
const EXTS: &[&str] = &[
    "txt", "jpg", "png", "pdf", "docx", "rs", "dll", "exe", "log", "md", "json", "xml", "cpp", "h", "py", "js", "zip", "mp3", "mp4", "ini",
];

fn name(r: &mut Rng, ext: bool) -> String {
    let mut s = String::from(WORDS[r.below(WORDS.len())]);
    match r.below(4) {
        0 => {}
        1 => {
            s.push('_');
            s.push_str(WORDS[r.below(WORDS.len())]);
        }
        2 => s.push_str(&format!("_{}", r.below(100000))),
        _ => s.push_str(&format!(" {} ({})", WORDS[r.below(WORDS.len())], r.below(50))),
    }
    if ext {
        s.push('.');
        s.push_str(EXTS[r.below(EXTS.len())]);
    }
    s
}

fn pct(v: &mut [u128], p: f64) -> u128 {
    v.sort_unstable();
    v[(((v.len() - 1) as f64) * p) as usize]
}

fn main() {
    let total: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2_000_000);
    let mut r = Rng(0x9E3779B97F4A7C15);
    let nfolders = total / 8;
    // folders: (frn, parent frn, depth)
    let mut rows: Vec<(u64, u64, String, bool)> = Vec::with_capacity(total);
    let mut fold: Vec<(u64, u8)> = Vec::with_capacity(nfolders);
    let mut frn = 100u64;
    for k in 0..nfolders {
        frn += 1;
        let (parent, depth) = if k < 20 || r.below(50) == 0 {
            (5, 1)
        } else {
            // prefer deeper folders so depths spread over 3..12
            let mut p = fold[r.below(fold.len())];
            for _ in 0..2 {
                let q = fold[r.below(fold.len())];
                if q.1 > p.1 && q.1 < 12 {
                    p = q;
                }
            }
            if p.1 >= 12 {
                (5, 1)
            } else {
                (p.0, p.1 + 1)
            }
        };
        fold.push((frn, depth));
        rows.push((frn, parent, name(&mut r, false), true));
    }
    let deep: Vec<(u64, u8)> = fold.iter().copied().filter(|f| f.1 >= 3).collect();
    for _ in 0..total - nfolders {
        frn += 1;
        let p = deep[r.below(deep.len())].0;
        rows.push((frn, p, name(&mut r, true), false));
    }
    // MFT-ish order: shuffle so children often precede their parent
    for i in (1..rows.len()).rev() {
        let j = r.below(i + 1);
        rows.swap(i, j);
    }
    let mean_depth = fold.iter().map(|f| f.1 as f64).sum::<f64>() / fold.len() as f64;
    println!("synth: {} records ({} folders), mean folder depth {:.1}", rows.len(), nfolders, mean_depth);

    let t = Instant::now();
    let mut b = IndexBuilder::with_capacity(true, rows.len());
    b.add_volume(b'C', 5);
    for (f, p, n, d) in &rows {
        b.push(b'C', *f, *p, n, *d, 1234, 5678);
    }
    let mut ix = b.finish_unsorted();
    let t_build = t.elapsed();
    let t = Instant::now();
    ix.rebuild_order();
    let t_sort = t.elapsed();
    println!("build (push+resolve): {:?}   name+path sort: {:?}", t_build, t_sort);
    let mem = ix.mem_bytes();
    println!("memory: {:.1} MB  = {:.1} bytes/record (incl. size+mtime 16 B, no mutation links)", mem as f64 / 1e6, mem as f64 / ix.slots() as f64);

    let t = Instant::now();
    ix.insert(b'C', 9_999_999, 5, "warm.txt", false, 0, 0);
    ix.refresh();
    println!("first insert (builds links+hash table) + refresh: {:?}", t.elapsed());
    let mem2 = ix.mem_bytes();
    println!("memory with mutation structures: {:.1} bytes/record", mem2 as f64 / ix.slots() as f64);
    let t = Instant::now();
    for k in 0..1000u64 {
        ix.insert(b'C', 10_000_000 + k, 5, &format!("live_{k}.txt"), false, 0, 0);
    }
    let t_ins = t.elapsed();
    let t = Instant::now();
    ix.refresh();
    println!("1000 inserts: {:?}; refresh (merge 1000 into {}): {:?}", t_ins, ix.order_len(), t.elapsed());

    let path = std::env::temp_dir().join("winseek_bench.wsix");
    let t = Instant::now();
    let _ = ix.to_bytes();
    println!("serialize to memory: {:?}", t.elapsed());
    let t = Instant::now();
    ix.save(&path).unwrap();
    let t_save = t.elapsed();
    let sz = std::fs::metadata(&path).unwrap().len();
    let t = Instant::now();
    let ld = Index::load(&path).unwrap();
    println!("snapshot: {:.1} MB ({:.1} B/rec); save {:?}; load {:?}; loaded {} recs", sz as f64 / 1e6, sz as f64 / ix.slots() as f64, t_save, t.elapsed(), ld.live_count());
    std::fs::remove_file(&path).ok();

    let queries = [
        "report",
        "re",
        "a",
        "annual report",
        "\"design doc\"",
        "ext:jpg",
        "ext:jpg;png vacation",
        "ext:rs main",
        "file:readme",
        "folder:src",
        "!tmp report",
        "été",
        "日本",
        "*.log",
        "img_????.jpg",
        "a*e",
        "windows\\system",
        "users\\documents\\ report",
        "node_modules\\ index",
        "program*\\*.dll",
    ];
    let opts = SearchOptions { offset: 0, limit: 100, order: Order::NameAsc };
    println!("\n{:<28} {:>9} {:>9} {:>9}", "query", "matches", "p50 us", "p99 us");
    let mut all = Vec::new();
    for q in queries {
        let qq = Query::parse(q);
        ix.search(&qq, &opts); // warm
        let mut lat = Vec::new();
        let mut total = 0;
        for _ in 0..40 {
            let t = Instant::now();
            let res = ix.search(&qq, &opts);
            lat.push(t.elapsed().as_micros());
            total = res.total;
        }
        all.extend(lat.iter().copied());
        println!("{:<28} {:>9} {:>9} {:>9}", q, total, pct(&mut lat, 0.5), pct(&mut lat, 0.99));
    }
    println!("{:<28} {:>9} {:>9} {:>9}", "ALL", "", pct(&mut all, 0.5), pct(&mut all, 0.99));

    // Paging a huge result (what a scrollbar jump costs): the match set is built once per
    // query, then any 400-row window is a lookup.
    let q = Query::parse("e");
    let t = Instant::now();
    let set = ix.name_matches(&q);
    let build = t.elapsed();
    let t = Instant::now();
    let mut rows = 0;
    for k in 0..1000 {
        rows += ix.name_page(&set, set.total() / 1000 * k, 400, false).len();
    }
    let page = t.elapsed() / 1000;
    let t = Instant::now();
    let count = ix.count(&q);
    let count_t = t.elapsed();
    let t = Instant::now();
    let slots = ix.match_slots(&q);
    let top = ix.top_slots(&slots, MetaKey::Mtime, 50_000).len();
    let top_t = t.elapsed();
    let t = Instant::now();
    let sorted = ix.sort_slots(slots, MetaKey::Mtime).len();
    let sort_t = t.elapsed();
    println!("\npaging \"e\": {} matches ({count} counted in {count_t:.1?})", set.total());
    println!("  name order : match set {build:.1?} ({} KB), then {page:.1?} per 400-row window anywhere ({rows} rows read)", set.mem_bytes() / 1024);
    println!("  date order : first {top} rows {top_t:.1?} (scan + select); full sort of {sorted} for deep jumps {sort_t:.1?}, then slices");
}
