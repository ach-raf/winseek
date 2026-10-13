//! Cases, glob matching, recall/MRR math and the before/after diff. All pure.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// recall@5 may drop by at most this much (absolute, 0..1) before `diff` fails.
pub const RECALL5_GATE: f64 = 0.02;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    pub q: String,
    /// File-name globs (`*`, `?`, case-insensitive); the case passes at k if any is in the top k.
    pub expect: Vec<String>,
    #[serde(default)]
    pub tag: String,
}

pub fn parse_cases(json: &str) -> Result<Vec<Case>, String> {
    let cases: Vec<Case> = serde_json::from_str(json).map_err(|e| format!("cases: {e}"))?;
    for c in &cases {
        if c.q.trim().is_empty() {
            return Err("cases: empty query".into());
        }
        if c.expect.is_empty() {
            return Err(format!("cases: `{}` has no expect", c.q));
        }
    }
    Ok(cases)
}

/// Case-insensitive glob over a whole name: `*` any run, `?` one char.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// 1-based rank of the first ranked name matching any glob.
pub fn rank_of(ranked: &[String], expect: &[String]) -> Option<usize> {
    ranked.iter().position(|n| expect.iter().any(|g| glob_match(g, n))).map(|i| i + 1)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub q: String,
    pub tag: String,
    pub expect: Vec<String>,
    pub rank: Option<usize>,
    pub top: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub n: usize,
    pub recall1: f64,
    pub recall5: f64,
    pub mrr: f64,
}

pub fn summarize(results: &[CaseResult]) -> Summary {
    let n = results.len();
    if n == 0 {
        return Summary { n: 0, recall1: 0.0, recall5: 0.0, mrr: 0.0 };
    }
    let at = |k: usize| results.iter().filter(|r| r.rank.is_some_and(|x| x <= k)).count() as f64 / n as f64;
    let mrr = results.iter().map(|r| r.rank.map_or(0.0, |x| 1.0 / x as f64)).sum::<f64>() / n as f64;
    Summary { n, recall1: at(1), recall5: at(5), mrr }
}

pub fn summarize_by_tag(results: &[CaseResult]) -> BTreeMap<String, Summary> {
    let mut groups: BTreeMap<String, Vec<CaseResult>> = BTreeMap::new();
    for r in results {
        groups.entry(r.tag.clone()).or_default().push(r.clone());
    }
    groups.into_iter().map(|(t, v)| (t, summarize(&v))).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub meta: serde_json::Value,
    pub overall: Summary,
    pub by_tag: BTreeMap<String, Summary>,
    pub cases: Vec<CaseResult>,
}

impl Report {
    pub fn new(meta: serde_json::Value, cases: Vec<CaseResult>) -> Report {
        Report { meta, overall: summarize(&cases), by_tag: summarize_by_tag(&cases), cases }
    }
}

pub fn format_summary_table(r: &Report) -> String {
    let mut out = format!("{:<12} {:>4} {:>9} {:>9} {:>7}\n", "tag", "n", "recall@1", "recall@5", "MRR");
    let row = |name: &str, s: &Summary| {
        format!("{:<12} {:>4} {:>9.3} {:>9.3} {:>7.3}\n", name, s.n, s.recall1, s.recall5, s.mrr)
    };
    for (t, s) in &r.by_tag {
        out.push_str(&row(if t.is_empty() { "(none)" } else { t }, s));
    }
    out.push_str(&row("OVERALL", &r.overall));
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    /// Cases that got worse (b relative to a), as printable lines.
    pub regressions: Vec<String>,
    pub improvements: Vec<String>,
    pub recall1_delta: f64,
    pub recall5_delta: f64,
    pub mrr_delta: f64,
    /// recall@5 dropped by more than [`RECALL5_GATE`].
    pub failed: bool,
}

fn rank_str(r: Option<usize>) -> String {
    r.map_or("miss".into(), |x| x.to_string())
}

/// Worse = lost a position or fell out of the ranking (`None` is the worst).
fn rank_key(r: Option<usize>) -> usize {
    r.unwrap_or(usize::MAX)
}

pub fn diff(a: &Report, b: &Report) -> Diff {
    let mut regressions = Vec::new();
    let mut improvements = Vec::new();
    for ca in &a.cases {
        let Some(cb) = b.cases.iter().find(|c| c.q == ca.q) else {
            regressions.push(format!("[{}] \"{}\": missing from b", ca.tag, ca.q));
            continue;
        };
        let line = format!("[{}] \"{}\": rank {} -> {}", ca.tag, ca.q, rank_str(ca.rank), rank_str(cb.rank));
        match rank_key(cb.rank).cmp(&rank_key(ca.rank)) {
            std::cmp::Ordering::Greater => regressions.push(line),
            std::cmp::Ordering::Less => improvements.push(line),
            std::cmp::Ordering::Equal => {}
        }
    }
    let recall5_delta = b.overall.recall5 - a.overall.recall5;
    Diff {
        regressions,
        improvements,
        recall1_delta: b.overall.recall1 - a.overall.recall1,
        recall5_delta,
        mrr_delta: b.overall.mrr - a.overall.mrr,
        // epsilon: exactly 2 points is still within the gate despite float noise
        failed: recall5_delta < -RECALL5_GATE - 1e-9,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn res(q: &str, tag: &str, rank: Option<usize>) -> CaseResult {
        CaseResult { q: q.into(), tag: tag.into(), expect: names(&["x"]), rank, top: vec![] }
    }

    #[test]
    fn glob_basics() {
        assert!(glob_match("img_0007.jpg", "img_0007.jpg"));
        assert!(glob_match("IMG_*.JPG", "img_0007.jpg"));
        assert!(glob_match("img_000?.jpg", "img_0007.jpg"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(glob_match("*.txt", "note_0001.txt"));
        assert!(!glob_match("img_0007.jpg", "img_0008.jpg"));
        assert!(!glob_match("img_000?.jpg", "img_00007.jpg"));
        assert!(!glob_match("a*b", "abc"));
        assert!(glob_match("a**b", "ab"));
    }

    #[test]
    fn rank_is_first_match_one_based() {
        let ranked = names(&["a.jpg", "b.jpg", "c.jpg"]);
        assert_eq!(rank_of(&ranked, &names(&["c.jpg"])), Some(3));
        assert_eq!(rank_of(&ranked, &names(&["z.jpg", "b.*"])), Some(2));
        assert_eq!(rank_of(&ranked, &names(&["z.jpg"])), None);
        assert_eq!(rank_of(&[], &names(&["a"])), None);
    }

    #[test]
    fn metric_math() {
        let r = [res("a", "t", Some(1)), res("b", "t", Some(2)), res("c", "u", Some(6)), res("d", "u", None)];
        let s = summarize(&r);
        assert_eq!(s.n, 4);
        assert!((s.recall1 - 0.25).abs() < 1e-12);
        assert!((s.recall5 - 0.5).abs() < 1e-12);
        assert!((s.mrr - (1.0 + 0.5 + 1.0 / 6.0) / 4.0).abs() < 1e-12);
        let by = summarize_by_tag(&r);
        assert_eq!(by["t"].recall1, 0.5);
        assert_eq!(by["u"].recall5, 0.0);
        assert_eq!(summarize(&[]).n, 0);
    }

    #[test]
    fn cases_parse_and_validate() {
        let c = parse_cases(r#"[{"q":"red","expect":["a*"],"tag":"color"},{"q":"b","expect":["x"]}]"#).unwrap();
        assert_eq!(c[0].tag, "color");
        assert_eq!(c[1].tag, "");
        assert!(parse_cases(r#"[{"q":"red","expect":[]}]"#).is_err());
        assert!(parse_cases(r#"[{"q":" ","expect":["a"]}]"#).is_err());
        assert!(parse_cases("{").is_err());
    }

    fn report(cases: Vec<CaseResult>) -> Report {
        Report::new(serde_json::json!({}), cases)
    }

    #[test]
    fn diff_flags_regressions_and_gate() {
        let a = report(vec![res("a", "t", Some(1)), res("b", "t", Some(2)), res("c", "t", None)]);
        let b = report(vec![res("a", "t", Some(3)), res("b", "t", Some(2)), res("c", "t", Some(1))]);
        let d = diff(&a, &b);
        assert_eq!(d.regressions.len(), 1);
        assert!(d.regressions[0].contains("\"a\"") && d.regressions[0].contains("1 -> 3"));
        assert_eq!(d.improvements.len(), 1);
        assert!(!d.failed); // recall@5 went up
        assert!(d.recall5_delta > 0.0);

        let worse = report(vec![res("a", "t", None), res("b", "t", Some(2)), res("c", "t", None)]);
        let d = diff(&a, &worse);
        assert!(d.failed); // 2/3 -> 1/3
        assert_eq!(diff(&a, &a).regressions.len(), 0);
    }

    #[test]
    fn gate_is_two_points() {
        let mk = |hits: usize| report((0..100).map(|i| res(&format!("q{i}"), "t", (i < hits).then_some(1))).collect());
        assert!(!diff(&mk(100), &mk(98)).failed, "exactly 2 points is allowed");
        assert!(diff(&mk(100), &mk(97)).failed);
    }

    #[test]
    fn report_roundtrips_through_json() {
        let r = report(vec![res("a", "t", Some(2))]);
        let back: Report = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        assert!(format_summary_table(&r).contains("OVERALL"));
    }
}
