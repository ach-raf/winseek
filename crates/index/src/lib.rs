//! winseek-index — Tier 0 name index (pure Rust, no Win32).
//!
//! * [`store`]: SoA record store, builder, live mutation, name-sorted order
//! * [`fold`]: case folding
//! * [`query`]: query parser + parallel matcher
//! * [`rank`]: Best match scoring of name results (how the typed word sits in the name, then
//!   folder / recency / depth / build folders / name length)
//! * [`snapshot`]: `WSIX` persistence

mod chunks;
pub mod fold;
pub mod query;
pub mod rank;
pub mod snapshot;
pub mod store;
pub mod usage;

pub use query::{Cond, MetaKey, Order, Query, RankSet, SearchOptions, SearchResult, SlotSet, Term};
pub use rank::{
    age_days, is_noise_dir, now_filetime, rank_key, rank_score, MatchClass, RankFacts, RankWeights,
    RECENCY_DAYS, SCORE_MIN,
};
pub use snapshot::SnapshotError;
pub use store::{
    norm_frn, Index, IndexBuilder, VolKind, VolumeMeta, FIRST_SYNTHETIC_ID, FRN_MASK, NONE,
};
pub use usage::{
    category, category_of_key, ChildUsage, DirList, Rollup, TypeUsage, UsageTouch, CATEGORIES,
};

#[cfg(test)]
mod tests;
