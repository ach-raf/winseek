//! Launcher selection history (Flow's UserSelectedRecord idea): every picked result bumps a
//! (query prefix, item) counter and an item counter, so "I always open mpv with `mp`" is
//! learned — and every prefix of the query learns, so `m`, `mp` and `mpv` all benefit.

use std::collections::HashMap;
use std::path::Path;

use super::matcher::fold;

/// Per-counter cap so a single item cannot grow its boost forever.
const MAX_COUNT: u32 = 1000;
/// Flow weights history 5:1 (pair : item). Our match scores peak around 150-200, so one
/// pair pick adds 50 and one item pick adds 2; MAX_BOOST keeps a habit from burying exact
/// matches (an exact title scores 130+ on its own, a 200 boost would otherwise be infinite).
const PAIR_WEIGHT: u32 = 50;
/// Frecency of the item itself (any query): sqrt(picks) * FREC_SCALE, decayed by how long ago
/// the last pick was — an app you use daily outranks one you tried once last year.
const FREC_SCALE: f64 = 6.0;
const MAX_BOOST: u32 = 200;
/// Prefixes longer than 16 chars are the same query, nobody retypes that.
const PREFIX_CHARS: usize = 16;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct History {
    /// key = `<folded prefix 16 chars>\u{1}<item id>`.
    pairs: HashMap<String, u32>,
    items: HashMap<String, u32>,
    /// Unix seconds of the latest pick per item (frecency decay). Absent in old files.
    #[serde(default)]
    last: HashMap<String, i64>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Recency multiplier: today 1.0, week 0.7, month 0.45, quarter 0.25, older 0.12.
fn decay(age_secs: i64) -> f64 {
    const DAY: i64 = 86_400;
    match age_secs {
        a if a < DAY => 1.0,
        a if a < 7 * DAY => 0.7,
        a if a < 30 * DAY => 0.45,
        a if a < 90 * DAY => 0.25,
        _ => 0.12,
    }
}

impl History {
    pub fn record(&mut self, query: &str, id: &str) {
        self.record_at(query, id, now_secs());
    }

    fn record_at(&mut self, query: &str, id: &str, now: i64) {
        self.last.insert(id.to_string(), now);
        let q: String = fold(query).chars().take(PREFIX_CHARS).collect();
        for len in 1..=q.chars().count() {
            let prefix: String = q.chars().take(len).collect();
            bump(&mut self.pairs, format!("{prefix}\u{1}{id}"));
        }
        bump(&mut self.items, id.to_string());
    }

    pub fn boost(&self, query: &str, id: &str) -> u32 {
        self.boost_at(query, id, now_secs())
    }

    fn boost_at(&self, query: &str, id: &str, now: i64) -> u32 {
        let item_count = self.items.get(id).copied().unwrap_or(0);
        // Old files have counts but no timestamps: treat those as a month-old habit.
        let age = self.last.get(id).map_or(30 * 86_400, |t| (now - t).max(0));
        let frec = ((item_count as f64).sqrt() * FREC_SCALE * decay(age)).round() as u32;
        if self.pairs.is_empty() {
            return frec.min(MAX_BOOST);
        }
        let q: String = fold(query).chars().take(PREFIX_CHARS).collect();
        let mut key = String::with_capacity(q.len() + id.len() + 1);
        key.push_str(&q);
        key.push('\u{1}');
        key.push_str(id);
        let pair = self.pairs.get(&key).copied().unwrap_or(0);
        (pair * PAIR_WEIGHT + frec).min(MAX_BOOST)
    }

    pub fn clear(&mut self) {
        self.pairs.clear();
        self.items.clear();
        self.last.clear();
    }

    pub fn load(path: &Path) -> History {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("winseek: launch history unreadable ({}), starting empty", e);
                    History::default()
                }
            },
            Err(_) => History::default(), // first run
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, json).map_err(|e| e.to_string())
    }
}

fn bump(map: &mut HashMap<String, u32>, key: String) {
    let at = map.entry(key).or_insert(0);
    *at = (*at + 1).min(MAX_COUNT);
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000;

    #[test]
    fn every_prefix_learns() {
        let mut h = History::default();
        h.record_at("mpv", "app:mpv", T0);
        h.record_at("mpv", "app:mpv", T0);
        let frec = (2f64.sqrt() * FREC_SCALE).round() as u32;
        for q in ["m", "mp", "mpv"] {
            assert_eq!(h.boost_at(q, "app:mpv", T0), 2 * PAIR_WEIGHT + frec);
        }
        assert_eq!(h.boost_at("mpvx", "app:mpv", T0), frec); // item frecency applies to any query
        assert_eq!(h.boost_at("mpv", "app:other", T0), 0); // never picked
    }

    #[test]
    fn frecency_decays_with_age() {
        let mut h = History::default();
        for _ in 0..16 {
            h.record_at("c", "app:chrome", T0);
        }
        let today = h.boost_at("zzz", "app:chrome", T0);
        let month = h.boost_at("zzz", "app:chrome", T0 + 40 * 86_400);
        let year = h.boost_at("zzz", "app:chrome", T0 + 400 * 86_400);
        assert_eq!(today, 24); // sqrt(16) * 6
        assert!(today > month && month > year && year > 0);
    }

    #[test]
    fn frequent_beats_once() {
        let mut h = History::default();
        h.record_at("a", "app:once", T0);
        for _ in 0..25 {
            h.record_at("b", "app:often", T0);
        }
        assert!(h.boost_at("zzz", "app:often", T0) > h.boost_at("zzz", "app:once", T0));
    }

    #[test]
    fn old_files_without_timestamps_still_load() {
        let json = br#"{"pairs":{},"items":{"app:x":4}}"#;
        let h: History = serde_json::from_slice(json).unwrap();
        assert!(h.boost_at("zzz", "app:x", T0) > 0);
    }

    #[test]
    fn boost_is_capped_and_folds() {
        let mut h = History::default();
        for _ in 0..100 {
            h.record_at("Café", "app:x", T0);
        }
        assert_eq!(h.boost_at("café", "app:x", T0), MAX_BOOST);
        assert_eq!(h.boost_at("CAFÉ", "app:x", T0), MAX_BOOST); // folds to the same prefix
        assert!(h.boost_at("z", "app:x", T0) > 0); // item frecency alone still boosts
    }

    #[test]
    fn long_queries_share_one_prefix() {
        let mut h = History::default();
        h.record_at("a-very-long-query-beyond-sixteen", "app:x", T0);
        let frec = FREC_SCALE.round() as u32;
        assert_eq!(h.boost_at("a-very-long-query-beyond-sixteen!", "app:x", T0), PAIR_WEIGHT + frec);
    }

    #[test]
    fn clear_resets() {
        let mut h = History::default();
        h.record_at("m", "app:x", T0);
        h.clear();
        assert_eq!(h.boost_at("m", "app:x", T0), 0);
    }

    #[test]
    fn serde_round_trip() {
        let mut h = History::default();
        h.record_at("mp", "app:mpv", T0);
        let json = serde_json::to_vec(&h).unwrap();
        let back: History = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.boost_at("mp", "app:mpv", T0), PAIR_WEIGHT + FREC_SCALE.round() as u32);
    }
}
