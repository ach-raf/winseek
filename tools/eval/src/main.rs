//! winseek-eval: Tier 1 eval harness (llama-server -> store -> vector search).
//!
//! winseek-eval gen <dir> [--titles]      write the deterministic synthetic corpus (+ titled notes)
//! winseek-eval cases [--titles]          print the cases JSON for that corpus (eval/cases.json,
//!                                        or eval/title-cases.json with --titles)
//! winseek-eval run --corpus D --cases F --models D --llama D [--url U] [--api-key K] [--hybrid] [--titles] [--out F]
//! winseek-eval diff a.json b.json        per-case regressions; exit 1 if recall@5 drops > 2 points
//! winseek-eval route --set D --models D --llama D [--max-fp F] [--current MIN,MARGIN] [--out F]
//!                                        doc-image routing + smart-collection thresholds on a labelled set
//! winseek-eval gate --db F --labels F --url U [--api-key K]
//!                                        meaning-result precision before/after the relevance gate on a real index copy

mod corpus;
mod gate;
mod metrics;
mod route;
mod run;

use std::path::PathBuf;
use std::process::ExitCode;

use metrics::{diff, format_summary_table, Report, RECALL5_GATE};

const USAGE: &str = "usage:
  winseek-eval gen <dir> [--titles]
  winseek-eval cases [--titles]
  winseek-eval run --corpus <dir> --cases <cases.json> --models <dir> --llama <dir> [--url <http://127.0.0.1:PORT>] [--api-key <key>] [--hybrid] [--titles] [--out <results.json>]
  winseek-eval diff <a.json> <b.json>
  winseek-eval route --set <dir with doc/<cat>/ and other/<cat>/> --models <dir> --llama <dir> [--url <u>] [--max-fp <rate, default 0.01>] [--current <min,margin>] [--out <scores.json>]
  winseek-eval gate --db <copy of tier1.sqlite> --labels <labels.json> --url <http://127.0.0.1:PORT> [--api-key <key>]

`--titles` embeds every text file as `title: {file name} | text: {body}` instead of the
untitled form; `gen --titles` adds the meaningfully named title notes, and their cases
live in eval/title-cases.json.";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn need(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("missing {name}\n{USAGE}"))
}

fn load_report(path: &str) -> Result<Report, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_str(&s).map_err(|e| format!("{path}: {e}"))
}

fn real_main(args: Vec<String>) -> Result<ExitCode, String> {
    match args.first().map(String::as_str) {
        Some("gen") => {
            let dir = args.get(1).ok_or(USAGE)?;
            let titles = args.iter().any(|a| a == "--titles");
            let n = (if titles { corpus::generate_with_titles(std::path::Path::new(dir)) } else { corpus::generate(std::path::Path::new(dir)) })
                .map_err(|e| e.to_string())?;
            println!("wrote {n} files + corpus.json to {dir}{}", if titles { " (titled notes included)" } else { "" });
            Ok(ExitCode::SUCCESS)
        }
        Some("cases") => {
            let json = if args.iter().any(|a| a == "--titles") { corpus::title_cases() } else { corpus::cases() };
            let out = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
            println!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        Some("run") => {
            let opts = run::RunOptions {
                corpus: PathBuf::from(need(&args, "--corpus")?),
                cases: PathBuf::from(need(&args, "--cases")?),
                models: PathBuf::from(flag(&args, "--models").unwrap_or_default()),
                llama: PathBuf::from(flag(&args, "--llama").unwrap_or_default()),
                url: flag(&args, "--url"),
                api_key: flag(&args, "--api-key").unwrap_or_default(),
                hybrid: args.iter().any(|a| a == "--hybrid"),
                titles: args.iter().any(|a| a == "--titles"),
            };
            if opts.url.is_none() && (flag(&args, "--models").is_none() || flag(&args, "--llama").is_none()) {
                return Err(format!("--models and --llama are required unless --url is given\n{USAGE}"));
            }
            let report = run::run(&opts)?;
            print!("{}", format_summary_table(&report));
            for c in report.cases.iter().filter(|c| c.rank.is_none_or(|r| r > 5)) {
                println!("  MISS@5 [{}] \"{}\" -> top: {}", c.tag, c.q, c.top.join(", "));
            }
            if let Some(out) = flag(&args, "--out") {
                let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
                std::fs::write(&out, json + "\n").map_err(|e| format!("{out}: {e}"))?;
                println!("results written to {out}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("route") => {
            let opts = run::RunOptions {
                corpus: PathBuf::new(),
                cases: PathBuf::new(),
                models: PathBuf::from(flag(&args, "--models").unwrap_or_default()),
                llama: PathBuf::from(flag(&args, "--llama").unwrap_or_default()),
                url: flag(&args, "--url"),
                api_key: flag(&args, "--api-key").unwrap_or_default(),
                hybrid: false,
                titles: false,
            };
            let set = PathBuf::from(need(&args, "--set")?);
            let max_fp: f32 = flag(&args, "--max-fp").map_or(Ok(0.01), |s| s.parse()).map_err(|e| format!("--max-fp: {e}"))?;
            let current = flag(&args, "--current").unwrap_or_else(|| "0.60,0.04".into());
            let (a, b) = current.split_once(',').ok_or("--current takes min,margin")?;
            let current = (a.trim().parse().map_err(|e| format!("--current: {e}"))?, b.trim().parse().map_err(|e| format!("--current: {e}"))?);
            let out = flag(&args, "--out").map(PathBuf::from);
            print!("{}", route::run(&opts, &set, max_fp, current, out.as_deref())?);
            Ok(ExitCode::SUCCESS)
        }
        Some("gate") => {
            let db = need(&args, "--db")?;
            let labels = gate::load_labels(std::path::Path::new(&need(&args, "--labels")?))?;
            let url = need(&args, "--url")?;
            let store = winseek_store::Store::open(&db, &run::index_meta(&flag(&args, "--llama-build").unwrap_or_else(|| "b11588".into())))
                .map_err(|e| format!("{db}: {e}"))?;
            let mut client = winseek_llama::EmbedClient::new(&url, &flag(&args, "--api-key").unwrap_or_default());
            print!("{}", gate::report(&gate::evaluate(&store, &labels, &mut client)?));
            Ok(ExitCode::SUCCESS)
        }
        Some("diff") => {
            let (a, b) = (args.get(1).ok_or(USAGE)?, args.get(2).ok_or(USAGE)?);
            let (ra, rb) = (load_report(a)?, load_report(b)?);
            let d = diff(&ra, &rb);
            println!("recall@1 {:+.3}  recall@5 {:+.3}  MRR {:+.3}", d.recall1_delta, d.recall5_delta, d.mrr_delta);
            for l in &d.regressions {
                println!("  REGRESSION {l}");
            }
            for l in &d.improvements {
                println!("  improved   {l}");
            }
            if d.failed {
                println!("FAIL: recall@5 dropped by more than {:.0} points", RECALL5_GATE * 100.0);
                Ok(ExitCode::from(1))
            } else {
                println!("OK");
                Ok(ExitCode::SUCCESS)
            }
        }
        _ => Err(USAGE.into()),
    }
}

fn main() -> ExitCode {
    match real_main(std::env::args().skip(1).collect()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
