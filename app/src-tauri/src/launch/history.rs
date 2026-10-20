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
const ITEM_WEIGHT: u32 = 2;
const MAX_BOOST: u32 = 200;
/// Prefixes longer than 16 chars are the same query, nobody retypes that.
const PREFIX_CHARS: usize = 16;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct History {
    /// key = `<folded prefix 16 chars>\u{1}<item id>`.
    pairs: HashMap<String, u32>,
    items: HashMap<String, u32>,
}

impl History {
    pub fn record(&mut self, query: &str, id: &str) {
        let q: String = fold(query).chars().take(PREFIX_CHARS).collect();
        for len in 1..=q.chars().count() {
            let prefix: String = q.chars().take(len).collect();
            bump(&mut self.pairs, format!("{prefix}\u{1}{id}"));
        }
        bump(&mut self.items, id.to_string());
    }

    pub fn boost(&self, query: &str, id: &str) -> u32 {
        let item_count = self.items.get(id).copied().unwrap_or(0);
        if self.pairs.is_empty() {
            return (item_count * ITEM_WEIGHT).min(MAX_BOOST);
        }
        let q: String = fold(query).chars().take(PREFIX_CHARS).collect();
        let mut key = String::with_capacity(q.len() + id.len() + 1);
        key.push_str(&q);
        key.push('\u{1}');
        key.push_str(id);
        let pair = self.pairs.get(&key).copied().unwrap_or(0);
        (pair * PAIR_WEIGHT + item_count * ITEM_WEIGHT).min(MAX_BOOST)
    }

    pub fn clear(&mut self) {
        self.pairs.clear();
        self.items.clear();
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

    #[test]
    fn every_prefix_learns() {
        let mut h = History::default();
        h.record("mpv", "app:mpv");
        h.record("mpv", "app:mpv");
        assert_eq!(h.boost("m", "app:mpv"), 2 * PAIR_WEIGHT + 2 * ITEM_WEIGHT);
        assert_eq!(h.boost("mp", "app:mpv"), 2 * PAIR_WEIGHT + 2 * ITEM_WEIGHT);
        assert_eq!(h.boost("mpv", "app:mpv"), 2 * PAIR_WEIGHT + 2 * ITEM_WEIGHT);
        assert_eq!(h.boost("mpvx", "app:mpv"), 2 * ITEM_WEIGHT); // item counter applies to any query
        assert_eq!(h.boost("mpv", "app:other"), 0); // never picked: no pair, no item counter
    }

    #[test]
    fn boost_is_capped_and_folds() {
        let mut h = History::default();
        for _ in 0..100 {
            h.record("Café", "app:x");
        }
        assert_eq!(h.boost("café", "app:x"), MAX_BOOST);
        assert_eq!(h.boost("CAFÉÉÉ", "app:x"), MAX_BOOST); // folded prefix of the same picks
        // The item counter alone also boosts (Flow's hash(item)); 100 picks cap it out.
        assert_eq!(h.boost("z", "app:x"), MAX_BOOST);
    }

    #[test]
    fn long_queries_share_one_prefix() {
        let mut h = History::default();
        h.record("a-very-long-query-beyond-sixteen", "app:x");
        assert_eq!(h.boost("a-very-long-query-beyond-sixteen!", "app:x"), PAIR_WEIGHT + ITEM_WEIGHT);
    }

    #[test]
    fn clear_resets() {
        let mut h = History::default();
        h.record("m", "app:x");
        h.clear();
        assert_eq!(h.boost("m", "app:x"), 0);
    }

    #[test]
    fn serde_round_trip() {
        let mut h = History::default();
        h.record("mp", "app:mpv");
        let json = serde_json::to_vec(&h).unwrap();
        let back: History = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.boost("mp", "app:mpv"), PAIR_WEIGHT + ITEM_WEIGHT);
    }
}
