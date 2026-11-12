use crate::source::{Hit, NameSource, SearchResult, VolumeStatus};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

impl AppState {
    pub fn new(source: Arc<dyn NameSource>) -> Self {
        AppState {
            source: RwLock::new(source),
            paused: AtomicBool::new(false),
            latest_search: Arc::default(),
            search_gate: Arc::default(),
        }
    }
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
    /// (query, filter, sort) of the newest `search` call. Searches queue on the one searchd
    /// connection; a queued search for a query the user has already typed past is dropped
    /// instead of run (typing "report" must not run "r", "re", "rep", ... one after another).
    latest_search: Arc<Mutex<String>>,
    search_gate: Arc<Mutex<()>>,
}

/// Run blocking work off the main thread. A plain (non-async) Tauri command runs *on* it, so a
/// slow pipe or store call would freeze the whole window, typing included.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[tauri::command]
pub async fn search(
    state: tauri::State<'_, AppState>,
    query: String,
    offset: usize,
    limit: usize,
    sort: Option<String>,
    filter: Option<String>,
) -> Result<SearchResult, String> {
    let src = state.source();
    let filter = filter.unwrap_or_else(|| "all".into());
    let sort = sort.unwrap_or_else(|| "rel".into());
    let key = format!("{query}\u{1}{filter}\u{1}{sort}");
    *lock(&state.latest_search) = key.clone();
    let (latest, gate) = (state.latest_search.clone(), state.search_gate.clone());
    blocking(move || {
        let _turn = lock(&gate);
        if *lock(&latest) != key {
            return SearchResult { total: 0, hits: vec![], elapsed_us: 0 }; // superseded: the UI drops it anyway
        }
        src.search(&query, &filter, offset, limit.min(5000), &sort)
    })
    .await
}

#[tauri::command]
pub async fn status(state: tauri::State<'_, AppState>) -> Result<Vec<VolumeStatus>, String> {
    let src = state.source();
    let paused = state.paused.load(Ordering::Relaxed);
    blocking(move || {
        let mut v = src.status();
        if paused {
            for s in &mut v {
                s.state = "paused".into();
            }
        }
        v
    })
    .await
}

/// Capacity of one drive, for the sidebar's "what is left on each drive" card.
#[derive(serde::Serialize)]
pub struct DriveSpace {
    pub letter: String,
    pub total: u64,
    pub free: u64,
}

fn space_of(letter: char) -> Option<DriveSpace> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    let root: Vec<u16> = format!("{}:\\", letter.to_ascii_uppercase()).encode_utf16().chain(std::iter::once(0)).collect();
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: `root` is NUL-terminated and outlives the call; the out pointers are valid u64s.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), Some(&mut free), Some(&mut total), None) }.ok()?;
    (total > 0).then(|| DriveSpace { letter: letter.to_ascii_uppercase().to_string(), total, free })
}

/// Free and total bytes of the given drive letters (a drive that does not answer is left out).
#[tauri::command]
pub async fn drive_space(letters: Vec<String>) -> Result<Vec<DriveSpace>, String> {
    blocking(move || letters.iter().filter_map(|l| l.chars().next().and_then(space_of)).collect()).await
}

#[tauri::command]
pub async fn recent(state: tauri::State<'_, AppState>, limit: usize) -> Result<Vec<Hit>, String> {
    let src = state.source();
    blocking(move || src.recent(limit.min(500))).await
}

#[tauri::command(async)]
pub fn open_path(path: String) -> Result<(), String> {
    // `explorer <path>` opens files with the default handler and folders in Explorer.
    Command::new("explorer.exe").arg(&path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn reveal_path(path: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // raw_arg: explorer wants `/select,"C:\a b\c"` verbatim.
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{path}\""))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Windows Terminal in `dir` (Ctrl+Shift+T). `wt.exe` from PATH; a plain `cmd.exe` keeping the
/// directory when it is not installed. Both are spawned as argv arrays, never a shell string.
#[tauri::command(async)]
pub fn open_terminal(dir: String) -> Result<(), String> {
    match Command::new("wt.exe").args(["-d", &dir]).spawn() {
        Ok(_) => Ok(()),
        Err(wt_err) => Command::new("cmd.exe")
            .args(["/K", "cd", "/d", &dir])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("no terminal: wt.exe ({wt_err}) and cmd.exe ({e}) both failed")),
    }
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
