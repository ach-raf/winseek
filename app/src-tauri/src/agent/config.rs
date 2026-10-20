//! Agent access config: one file, one switch. The server is strictly read-only (PRD intro),
//! so there is no access-level enum — only on/off plus which files' contents may be read.
//! `%LOCALAPPDATA%\winseek\agent_config.json` `{ "enabled": bool, "content": "indexed"|"any" }`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentScope {
    /// Only files under the Tier 1 include roots (what "Search by meaning" indexes).
    Indexed,
    /// Any file winseek can open.
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    pub enabled: bool,
    pub content: ContentScope,
}

impl Default for AgentConfig {
    fn default() -> Self {
        // Off by default (PRD A4): an agent pipe must never exist until the user asks for it.
        AgentConfig { enabled: false, content: ContentScope::Indexed }
    }
}

pub fn path(root: &Path) -> PathBuf {
    root.join("agent_config.json")
}

/// Missing file -> defaults (off). Corrupt or unreadable -> off with a log line: a broken file
/// must never open the agent pipe by accident.
pub fn load(root: &Path) -> AgentConfig {
    match std::fs::read(path(root)) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("winseek: agent config is not valid JSON ({e}); agent access stays off");
                AgentConfig::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AgentConfig::default(), // first run
        Err(e) => {
            eprintln!("winseek: agent config unreadable ({e}); agent access stays off");
            AgentConfig::default()
        }
    }
}

pub fn save(root: &Path, cfg: &AgentConfig) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path(root), json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier1::test_tmp;

    #[test]
    fn roundtrip_and_defaults() {
        let tmp = test_tmp();
        let root = tmp.path().join("cfg");
        assert_eq!(load(&root), AgentConfig::default(), "missing file -> off");
        let cfg = AgentConfig { enabled: true, content: ContentScope::Any };
        save(&root, &cfg).unwrap();
        assert_eq!(load(&root), cfg);
    }

    #[test]
    fn corrupt_file_stays_disabled() {
        let tmp = test_tmp();
        let root = tmp.path().join("cfg-bad");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(path(&root), b"{ \"enabled\": ").unwrap();
        assert_eq!(load(&root), AgentConfig::default());
        // A missing value also falls back: no partial states.
        std::fs::write(path(&root), b"{ \"content\": \"any\" }").unwrap();
        assert_eq!(load(&root), AgentConfig::default());
    }
}
