//! winseek-llama: prebuilt llama.cpp manager (pinned manifest, GPU detect,
//! resumable verified download, zip install, llama-server spawn/health).
//! See docs/PLAN.md "llama.cpp distribution".

pub mod backend;
pub mod embed;
pub mod fetch;
pub mod import;
pub mod install;
pub mod manifest;
pub mod parse;
pub mod prep;
pub mod reaper;
pub mod server;

use std::fmt;

pub use backend::{detect_backend, Backend, Detected};
pub use embed::EmbedClient;
pub use fetch::{fetch, fetch_with, rewrite_hf, FetchOptions, sha256_file, sha256_hex, Progress};
pub use parse::{Element, ParseClient, Parsed, Table, PARSE_PROMPT_VERSION};
pub use manifest::{Manifest, ModelProfile, ModelRole};
pub use reaper::{Clock, IdleReaper, IdleSlot, SystemClock};
pub use server::{fallback_chain, fallback_order, LlamaServer, Profile, Role, SpawnConfig};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Http(String),
    Sha256Mismatch { expected: String, actual: String },
    /// The manifest still carries a `TODO-pin-in-CI` hash.
    UnpinnedHash(String),
    Manifest(String),
    Zip(String),
    Spawn(String),
    HealthTimeout,
    NotFound(String),
    /// Download made too little progress within the stall window.
    Stalled(String),
    /// Embedding request/response problem (bad dims, malformed reply).
    Embed(String),
    /// Parser request/response problem (malformed reply, no usable elements).
    Parse(String),
    /// Aborted by the caller (a download's cancel flag); partial files are kept.
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Http(e) => write!(f, "http: {e}"),
            Error::Sha256Mismatch { expected, actual } => {
                write!(f, "sha256 mismatch: expected {expected}, got {actual}")
            }
            Error::UnpinnedHash(u) => write!(f, "manifest hash for {u} is not pinned"),
            Error::Manifest(e) => write!(f, "manifest: {e}"),
            Error::Zip(e) => write!(f, "zip: {e}"),
            Error::Spawn(e) => write!(f, "spawn: {e}"),
            Error::HealthTimeout => write!(f, "llama-server did not become healthy in time"),
            Error::NotFound(e) => write!(f, "not found: {e}"),
            Error::Stalled(e) => write!(f, "stalled: {e}"),
            Error::Embed(e) => write!(f, "embed: {e}"),
            Error::Parse(e) => write!(f, "parse: {e}"),
            Error::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
