//! winseek-index — Tier 0 name index (pure Rust, no Win32).
//!
//! * [`store`]: SoA record store, builder, live mutation, name-sorted order
//! * [`fold`]: case folding
//! * [`query`]: query parser + parallel matcher
//! * [`snapshot`]: `WSIX` persistence

mod chunks;
pub mod fold;
pub mod query;
pub mod snapshot;
pub mod store;
pub mod usage;

pub use query::{Cond, MetaKey, Order, Query, RankSet, SearchOptions, SearchResult, Term};
pub use snapshot::SnapshotError;
pub use store::{
    norm_frn, Index, IndexBuilder, VolKind, VolumeMeta, FIRST_SYNTHETIC_ID, FRN_MASK, NONE,
};
pub use usage::{
    category, category_of_key, ChildUsage, DirList, Rollup, TypeUsage, UsageTouch, CATEGORIES,
};

#[cfg(test)]
mod tests;
