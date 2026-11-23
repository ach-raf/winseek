//! Best match preview on the live index, read-only: the running searchd is only asked (in its
//! name order, what "Best match" shows today), never replaced or changed.
//!
//! `cargo run -p searchd --example rank_preview -- <query> [rows=12] [max=200000]`
//!
//! The query's matches are read from the running service, rebuilt as an in-memory index and
//! ordered with the real ranking (`winseek_index::rank` through the index), so both lists sit
//! side by side on real data. What is printed per row (depth, build folder, age) is read from
//! the path and the time the service sent, for display.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use winseek_index::{
    age_days, is_noise_dir, now_filetime, Index, IndexBuilder, MatchClass, Query, RECENCY_DAYS,
};
use winseek_ipc::pipe::connect;
use winseek_ipc::{read_message, write_message, Hit, Request, Response, Sort, PIPE_NAME};

/// 1601 -> 1970 in 100 ns units (the index stores times as FILETIME; hits carry unix ms).
const FT_EPOCH: i64 = 116_444_736_000_000_000;
/// Added to a score inside its sort key (`rank::SCORE_FLOOR`).
const SCORE_FLOOR: i64 = 256;

fn main() {
    let mut args = std::env::args().skip(1);
    let query = args.next().unwrap_or_else(|| "awb".into());
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(12);
    let max: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let mut c = connect(PIPE_NAME, Duration::from_secs(2)).unwrap_or_else(|e| {
        eprintln!("cannot connect to searchd ({PIPE_NAME}): {e}");
        std::process::exit(1);
    });
    let (total, hits, read_ms) = read_all(&mut c, &query, max);
    println!(
        "query {query:?}: {total} matches on the live index ({} read in {read_ms} ms)\n",
        hits.len()
    );
    if (hits.len() as u64) < total {
        println!(
            "  (only the first {} were read; the new order below ranks those)\n",
            hits.len()
        );
    }
    if hits.is_empty() {
        return;
    }

    println!("=== NOW (A to Z): what Best match shows today ===");
    for (i, h) in hits.iter().take(rows).enumerate() {
        println!("{:2}. {}", i + 1, show(&h.path, h.is_dir));
    }

    let ix = rebuild(&hits);
    let q = Query::parse(&query);
    let now = now_filetime();
    // Timings of the three steps the service runs (it orders the leading 50,000 rows for a
    // request near the top of a huge result, the whole list only when asked further down).
    let slots = ix.match_slots(&q);
    let t = Instant::now();
    let keys = ix.rank_keys(&q, &slots, now);
    let score_ms = t.elapsed().as_millis();
    let t = Instant::now();
    let top = Index::top_pairs(keys.clone(), 50_000);
    let top_ms = t.elapsed().as_millis();
    let t = Instant::now();
    let best = Index::sort_pairs(keys);
    let full_ms = t.elapsed().as_millis();
    let rank_ms = score_ms + top_ms + full_ms;
    println!(
        "timing ({} matches): score {} ms, leading {} rows ordered in {} ms, whole list ordered in {} ms",
        best.len(), score_ms, top.len(), top_ms, full_ms
    );
    let scores: HashMap<u32, i64> = ix
        .rank_keys(&q, &best, now)
        .into_iter()
        .map(|(key, slot)| (slot, ((key as u64) >> 52) as i64 - SCORE_FLOOR))
        .collect();

    println!("\n=== NEW (Best match): whole name first, then starts-with, a word inside, letters inside a word last ===");
    let mut last = None;
    let mut classes = [0usize; 4];
    for (i, &slot) in best.iter().enumerate() {
        let class = class_of(scores[&slot]);
        classes[class.index()] += 1;
        if i >= rows {
            continue;
        }
        if Some(class) != last {
            println!("-- {} --", explain(class));
            last = Some(class);
        }
        let path = ix.full_path(slot);
        let parts: Vec<&str> = path.split('\\').filter(|p| !p.is_empty()).collect();
        let depth = parts.len().saturating_sub(2);
        let noisy = parts[..parts.len().saturating_sub(1)]
            .iter()
            .any(|p| is_noise_dir(p.as_bytes()));
        let age = ix.mtime(slot).filter(|&m| m > 0).map(|m| age_days(now, m));
        let notes = [
            (ix.is_dir(slot)).then(|| "folder".to_string()),
            (depth > 0).then(|| format!("{depth} folders deep")),
            noisy.then(|| "under a build / cache folder".to_string()),
            age.map(|d| format!("changed {}", ago(d))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        println!(
            "{:2}. [{:<6} {:>4}] {}",
            i + 1,
            format!("{:?}", class),
            scores[&slot],
            show(&path, ix.is_dir(slot))
        );
        println!("        {notes}");
    }
    println!(
        "\nwhole name: {}   starts with it: {}   a word inside: {}   letters inside a word: {}   (ordered {} in {rank_ms} ms)",
        classes[3], classes[2], classes[1], classes[0], best.len()
    );
}

fn show(path: &str, dir: bool) -> String {
    format!("{}{}", path, if dir { "\\" } else { "" })
}

/// Every match of `query` from the running service, in its name order, at most `max`.
fn read_all(c: &mut winseek_ipc::pipe::PipeConn, query: &str, max: usize) -> (u64, Vec<Hit>, u128) {
    let t = Instant::now();
    let mut out = Vec::new();
    let mut offset = 0u32;
    let mut total = 0;
    while out.len() < max {
        write_message(
            c,
            &Request::Search {
                id: 7,
                query: query.to_string(),
                offset,
                limit: 10_000,
                sort: Sort::NameAsc,
                exclude: Vec::new(),
            },
        )
        .expect("send");
        match read_message::<_, Response>(c).expect("receive") {
            Response::SearchResult { total: t, hits, .. } => {
                total = t;
                let got = hits.len();
                out.extend(hits);
                if got == 0 || out.len() as u64 >= t {
                    break;
                }
                offset += got as u32;
            }
            other => {
                eprintln!("unexpected answer: {other:?}");
                std::process::exit(2);
            }
        }
    }
    out.truncate(max);
    (total, out, t.elapsed().as_millis())
}

/// The matches as an index: every folder above every path is a record too (synthetic ids), so
/// depth and build folders can be read; a folder that is itself a match keeps its own time.
fn rebuild(hits: &[Hit]) -> Index {
    let mut b = IndexBuilder::new(true);
    // `c:\users\me\` -> (record id, time); a folder's path as its children write it
    let mut folders: HashMap<String, (u64, i64)> = HashMap::new();
    for h in hits.iter().filter(|h| h.is_dir) {
        folders.insert(
            format!("{}\\", h.path.trim_end_matches('\\')).to_lowercase(),
            (0, FT_EPOCH + h.mtime.unwrap_or(0) * 10_000),
        );
    }
    let mut next = 1000u64;
    for h in hits {
        let vol = h.path.as_bytes()[0].to_ascii_uppercase();
        if b.volume_mut(vol).is_none() {
            b.add_volume(vol, 5);
        }
        // `C:\a\b\file` -> `C:`, `a`, `b`, `file`: the root is frn 5, the rest get ids.
        let parts: Vec<&str> = h.path.split('\\').filter(|p| !p.is_empty()).collect();
        let mut prefix = format!("{}\\", parts[0].to_lowercase());
        let mut parent = 5u64;
        for part in &parts[1..parts.len() - 1] {
            prefix.push_str(&part.to_lowercase());
            prefix.push('\\');
            let id = match folders.get(&prefix) {
                Some((id, _)) => *id,
                None => {
                    next += 1;
                    let mtime = folders.get(&prefix).map_or(0, |(_, t)| *t);
                    b.push(vol, next, parent, part, true, 0, mtime);
                    folders.insert(prefix.clone(), (next, mtime));
                    next
                }
            };
            parent = id;
        }
        next += 1;
        b.push(
            vol,
            next,
            parent,
            parts[parts.len() - 1],
            h.is_dir,
            h.size.unwrap_or(0),
            FT_EPOCH + h.mtime.unwrap_or(0) * 10_000,
        );
    }
    b.finish()
}

/// The class a score belongs to: the class bases are 300 apart and the other weights move a
/// score by less than 200 (see `rank`).
fn class_of(score: i64) -> MatchClass {
    if score >= 890 {
        MatchClass::Exact
    } else if score >= 590 {
        MatchClass::Prefix
    } else if score >= 290 {
        MatchClass::Word
    } else {
        MatchClass::Part
    }
}

fn explain(c: MatchClass) -> &'static str {
    match c {
        MatchClass::Exact => {
            "WHOLE NAME: the name is exactly what was typed (a file: without its extension)"
        }
        MatchClass::Prefix => "STARTS WITH what was typed",
        MatchClass::Word => "has a WORD that starts with it",
        MatchClass::Part => "only has the letters INSIDE a word (weakest)",
    }
}

fn ago(days: u32) -> String {
    let bands = [
        "today",
        "this week",
        "this month",
        "this quarter",
        "this year",
    ];
    for (k, limit) in RECENCY_DAYS.iter().enumerate() {
        if days < *limit {
            return bands[k].to_string();
        }
    }
    if days < 730 {
        format!("{} months ago", days / 30)
    } else {
        format!("{} years ago", days / 365)
    }
}
