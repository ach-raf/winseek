//! Agent access: the app-side half of the MCP server. The `winseek-mcp` shim connects to a
//! per-user pipe served here ([`server`]); every request is dispatched to the same functions
//! the Tauri commands use, strictly read-only (see `tools`, `dispatch`).

pub mod config;
pub mod dates;
pub mod dispatch;
pub mod log;
pub mod scan;
pub mod server;
pub mod tools;

use config::AgentConfig;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct AgentState {
    root: PathBuf,
    cfg: Mutex<AgentConfig>,
    log: Arc<log::Log>,
}

impl AgentState {
    pub fn load() -> Self {
        let root = crate::tier1::data_root().unwrap_or_else(std::env::temp_dir);
        let cfg = config::load(&root);
        let l = Arc::new(log::Log::new(&root));
        if let Err(e) = log::trim(&log::path(&root), log::MAX_LINES) {
            eprintln!("winseek: agent log trim failed: {e}");
        }
        eprintln!("winseek: agent access {}", if cfg.enabled { "on" } else { "off" });
        AgentState { root, cfg: Mutex::new(cfg), log: l }
    }

    /// Read per request, so flipping the toggle in Settings takes effect without a restart.
    pub fn config(&self) -> AgentConfig {
        *lock(&self.cfg)
    }

    pub fn set_config(&self, cfg: AgentConfig) -> Result<(), String> {
        config::save(&self.root, &cfg)?;
        *lock(&self.cfg) = cfg;
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpPath {
    pub path: String,
    pub exists: bool,
}

#[tauri::command]
pub async fn agent_get_config(state: tauri::State<'_, Arc<AgentState>>) -> Result<AgentConfig, String> {
    Ok(state.config())
}

#[tauri::command]
pub async fn agent_set_config(state: tauri::State<'_, Arc<AgentState>>, cfg: AgentConfig) -> Result<AgentConfig, String> {
    let st = Arc::clone(state.inner());
    crate::commands::blocking(move || st.set_config(cfg))
        .await
        .map_err(|e| e.to_string())??;
    Ok(state.config())
}

#[tauri::command]
pub async fn agent_log(state: tauri::State<'_, Arc<AgentState>>, limit: usize) -> Result<Vec<log::Entry>, String> {
    let log = state.log.clone();
    crate::commands::blocking(move || log.tail(limit)).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn agent_clear_log(state: tauri::State<'_, Arc<AgentState>>) -> Result<(), String> {
    let log = state.log.clone();
    crate::commands::blocking(move || log.clear()).await.map_err(|e| e.to_string())?
}

/// Where the shim lives next to the app (the Settings copy-ready snippets point at it).
#[tauri::command]
pub async fn agent_mcp_path() -> Result<McpPath, String> {
    let p = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("winseek-mcp.exe");
    Ok(McpPath { path: p.to_string_lossy().into_owned(), exists: p.exists() })
}
