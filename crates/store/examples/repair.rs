//! Check a tier1.sqlite and repair it with [`Store::recover`] (the app does the same on its own
//! when it sees a corruption error). Stop the app first.
//!
//! `cargo run -p winseek-store --example repair -- <path-to-tier1.sqlite>`

use winseek_store::{IndexMeta, Store};

fn main() {
    let path = std::env::args().nth(1).expect("usage: repair <tier1.sqlite>");
    let meta = {
        let c = rusqlite::Connection::open(&path).expect("open");
        c.query_row("SELECT model_id, llama_build, quant, image_budget, dims FROM index_meta WHERE id = 1", [], |r| {
            Ok(IndexMeta { model_id: r.get(0)?, llama_build: r.get(1)?, quant: r.get(2)?, image_budget: r.get(3)?, dims: r.get(4)? })
        })
        .expect("read index_meta")
    };
    let store = Store::open(&path, &meta).expect("open store");
    let before = store.stats().ok();
    let t = std::time::Instant::now();
    let (store, r) = store.recover().expect("recover");
    println!("{r:?} in {:.1}s", t.elapsed().as_secs_f64());
    println!("before: {before:?}");
    println!("after:  {:?}", store.stats().ok());
    println!("parse:  {:?}", store.parse_counts().ok());
}
