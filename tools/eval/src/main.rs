//! winseek-eval: Tier 1 eval harness (llama-server -> store -> vector search).
//!
//! winseek-eval gen <dir>                 write the deterministic synthetic corpus
//! winseek-eval cases                     print the cases JSON for that corpus (eval/cases.json)
//! winseek-eval run --corpus D --cases F --models D --llama D [--url U] [--api-key K] [--out F]
//! winseek-eval diff a.json b.json        per-case regressions; exit 1 if recall@5 drops > 2 points

mod corpus;
mod metrics;
mod run;

use std::path::PathBuf;
use std::process::ExitCode;

use metrics::{diff, format_summary_table, Report, RECALL5_GATE};

const USAGE: &str = "usage:
  winseek-eval gen <dir>
  winseek-eval cases
  winseek-eval run --corpus <dir> --cases <cases.json> --models <dir> --llama <dir> [--url <http://127.0.0.1:PORT>] [--api-key <key>] [--out <results.json>]
  winseek-eval diff <a.json> <b.json>";

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
            let n = corpus::generate(std::path::Path::new(dir)).map_err(|e| e.to_string())?;
            println!("wrote {n} files + corpus.json to {dir}");
            Ok(ExitCode::SUCCESS)
        }
        Some("cases") => {
            let json = serde_json::to_string_pretty(&corpus::cases()).map_err(|e| e.to_string())?;
            println!("{json}");
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
