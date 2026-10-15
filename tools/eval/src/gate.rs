//! `winseek-eval gate`: precision of the meaning results on a real index, before / after the
//! calibrated relevance gate (`winseek_store::relevance`).
//!
//! Input: a copy of a `tier1.sqlite` (never the live one) and a labels file
//! `{"query": ["relevant path", ...], ...}` (paths as stored; an empty list = nothing in the
//! library is relevant, so the right answer is to show nothing). Queries are embedded by a
//! running llama-server (`--url`, the app's text-only query preset is enough).
//!
//! "before" = the old popup: raw cosine top `SHOW`, no threshold. "after" = `relevance::gate`.
//! A shown file that is not labelled counts as irrelevant (labels come from looking at the
//! union of both lists, so nothing shown goes unjudged in practice).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use winseek_store::relevance::{self, HUB_PROMPTS};
use winseek_store::Store;

use crate::run::{finish, Embed};

pub const SHOW: usize = 8;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tally {
    pub shown: usize,
    pub relevant: usize,
}

impl Tally {
    pub fn precision(&self) -> f32 {
        if self.shown == 0 { 1.0 } else { self.relevant as f32 / self.shown as f32 }
    }
}

pub struct QueryRow {
    pub query: String,
    pub labelled: usize,
    pub before: Tally,
    pub after: Tally,
}

fn tally(shown: &[String], rel: &HashSet<String>) -> Tally {
    Tally { shown: shown.len(), relevant: shown.iter().filter(|p| rel.contains(&p.to_lowercase())).count() }
}

/// Score every labelled query on `store`; `emb` embeds queries (raw; truncated + normalised here).
pub fn evaluate(store: &Store, labels: &BTreeMap<String, Vec<String>>, emb: &mut dyn Embed) -> Result<Vec<QueryRow>, String> {
    let bank = finish(emb.queries(&HUB_PROMPTS)?)?;
    let hub = relevance::hub_vector(&bank);
    let queries: Vec<&str> = labels.keys().map(String::as_str).collect();
    let qv = finish(emb.queries(&queries)?)?;
    let mut rows = Vec::new();
    for ((q, rel), v) in labels.iter().zip(&qv) {
        let rel: HashSet<String> = rel.iter().map(|p| p.to_lowercase()).collect();
        let before: Vec<String> = store.search_vectors(v, SHOW, None).map_err(|e| e.to_string())?.into_iter().map(|h| h.path).collect();
        let cands = store.search_calibrated(v, &hub, 40).map_err(|e| e.to_string())?;
        let after: Vec<String> = relevance::gate(cands, SHOW).into_iter().map(|c| c.path).collect();
        rows.push(QueryRow { query: q.clone(), labelled: rel.len(), before: tally(&before, &rel), after: tally(&after, &rel) });
    }
    Ok(rows)
}

pub fn report(rows: &[QueryRow]) -> String {
    let mut s = String::new();
    let sum = |f: &dyn Fn(&QueryRow) -> &Tally| {
        rows.iter().fold(Tally::default(), |a, r| Tally { shown: a.shown + f(r).shown, relevant: a.relevant + f(r).relevant })
    };
    let (b, a) = (sum(&|r| &r.before), sum(&|r| &r.after));
    let pool: usize = rows.iter().map(|r| r.labelled.min(SHOW)).sum();
    s += &format!("gate z>={} adj>={}  ({} queries, top {SHOW})\n", relevance::Z_MIN, relevance::ADJ_MIN, rows.len());
    s += &format!("{:<22} {:>10} {:>10}\n", "query", "before", "after");
    for r in rows {
        s += &format!("{:<22} {:>10} {:>10}\n", r.query, format!("{}/{}", r.before.relevant, r.before.shown), format!("{}/{}", r.after.relevant, r.after.shown));
    }
    s += &format!(
        "precision  before {:.3}  after {:.3}\nrelevant shown  before {}  after {}  (of {} labelled, capped at {SHOW} per query)\n",
        b.precision(), a.precision(), b.relevant, a.relevant, pool
    );
    s
}

pub fn load_labels(path: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winseek_store::{ChunkVec, FileKey, VecKind};

    /// Queries: "cat" -> axis 1; hub prompts -> axis 3.
    struct Fake;
    impl Embed for Fake {
        fn images(&mut self, _: &[&[u8]]) -> Result<Vec<Vec<f32>>, String> {
            unreachable!()
        }
        fn documents(&mut self, _: &[&str]) -> Result<Vec<Vec<f32>>, String> {
            unreachable!()
        }
        fn queries(&mut self, q: &[&str]) -> Result<Vec<Vec<f32>>, String> {
            Ok(q.iter().map(|q| axis(if *q == "cat" { 1 } else { 3 })).collect())
        }
    }

    fn axis(i: usize) -> Vec<f32> {
        let mut v = vec![0.0; crate::run::DIMS];
        v[i] = 1.0;
        v
    }

    #[test]
    fn gate_beats_raw_top_k_on_unrelated_files() {
        let mut s = Store::open_in_memory(&crate::run::index_meta("t")).unwrap();
        for (n, path, ax) in [(1, "cat.jpg", 1), (2, "dog.jpg", 2), (3, "icon.png", 0)] {
            let id = s.upsert_file(&FileKey { vol_guid: "v".into(), frn: n }, path, 1, 1, "image").unwrap();
            let v = [ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec: axis(ax) }];
            s.complete(id, &format!("h{n}"), &v, &[]).unwrap();
        }
        let labels = BTreeMap::from([("cat".to_string(), vec!["CAT.jpg".to_string()])]);
        let rows = evaluate(&s, &labels, &mut Fake).unwrap();
        assert_eq!(rows[0].before, Tally { shown: 3, relevant: 1 });
        assert_eq!(rows[0].after, Tally { shown: 1, relevant: 1 });
        let r = report(&rows);
        assert!(r.contains("precision  before 0.333  after 1.000"), "{r}");
    }
}
