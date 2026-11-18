//! Ranking for `code:` searches — the module lives in the store crate
//! (`winseek_store::code_rank`) so the eval harness shapes queries and scores hits with the
//! exact code the app runs; this re-export keeps every search-pipeline call site unchanged.

pub use winseek_store::code_rank::*;
