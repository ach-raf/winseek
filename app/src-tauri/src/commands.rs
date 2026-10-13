use crate::source::{Hit, NameSource, SearchResult, VolumeStatus};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

impl AppState {
    pub fn source(&self) -> Arc<dyn NameSource> {
        self.source.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn set_source(&self, s: Arc<dyn NameSource>) {
        *self.source.write().unwrap_or_else(|e| e.into_inner()) = s;
    }
}

pub struct AppState {
    pub source: RwLock<Arc<dyn NameSource>>,
    pub paused: AtomicBool,
}

#[tauri::command]
pub fn search(
    state: tauri::State<'_, AppState>,
    query: String,
    offset: usize,
    limit: usize,
    sort: Option<String>,
    filter: Option<String>,
) -> SearchResult {
    state.source().search(
        &query,
        filter.as_deref().unwrap_or("all"),
        offset,
        limit.min(5000),
        sort.as_deref().unwrap_or("rel"),
    )
}

#[tauri::command]
pub fn status(state: tauri::State<'_, AppState>) -> Vec<VolumeStatus> {
    let mut v = state.source().status();
    if state.paused.load(Ordering::Relaxed) {
        for s in &mut v {
            s.state = "paused".into();
        }
    }
    v
}

#[tauri::command]
pub fn recent(state: tauri::State<'_, AppState>, limit: usize) -> Vec<Hit> {
    state.source().recent(limit.min(500))
}

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    // `explorer <path>` opens files with the default handler and folders in Explorer.
    Command::new("explorer.exe").arg(&path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // raw_arg: explorer wants `/select,"C:\a b\c"` verbatim.
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{path}\""))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn copy_path(path: String) -> Result<(), String> {
    // clip.exe accepts UTF-16LE with BOM on stdin.
    let mut child = Command::new("clip.exe").stdin(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let mut buf: Vec<u8> = vec![0xFF, 0xFE];
    for u in path.encode_utf16() {
        buf.extend_from_slice(&u.to_le_bytes());
    }
    child.stdin.take().ok_or("no stdin")?.write_all(&buf).map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}
