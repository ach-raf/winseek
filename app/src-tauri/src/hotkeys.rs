//! Rebindable global hotkeys (PRD RB4): `%LOCALAPPDATA%\winseek\hotkeys.json`
//! `{ popup, library, clipboard }`, each a canonical combo string ("Ctrl+Alt+Space") or null.
//! The strings are the single source of truth; they are parsed to `Shortcut`s only to talk to
//! `winhk` (RegisterHotKey), and the handler compares against the *current* bindings.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::tier1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    /// Show / hide winseek (required — the app is unreachable without one).
    pub popup: Option<String>,
    /// Open the Library window (optional).
    pub library: Option<String>,
    /// Show winseek with the clipboard text as the query (optional, P1).
    pub clipboard: Option<String>,
    /// Release the popup + clipboard hotkeys while a full-screen game / presentation runs, so the
    /// game gets every key. Off by default; library hotkey is never paused.
    #[serde(default)]
    pub pause_in_games: bool,
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings {
            popup: Some("Alt+Space".into()),
            library: None,
            clipboard: None,
            pause_in_games: false,
        }
    }
}

/// Hotkey pause. While paused the popup + clipboard shortcuts are *unregistered* (not just
/// ignored) so the keystroke goes to the foreground app untouched. The library hotkey stays live.
static USER_PAUSED: AtomicBool = AtomicBool::new(false);
static AUTO_PAUSED: AtomicBool = AtomicBool::new(false);
/// What is currently true at the OS level (popup/clipboard unregistered).
static RELEASED: AtomicBool = AtomicBool::new(false);
static SYNC: Mutex<()> = Mutex::new(());

pub fn is_released() -> bool {
    RELEASED.load(Ordering::SeqCst)
}

/// Tray label for the user's hotkey pause.
pub fn pause_label(user_paused: bool) -> &'static str {
    if user_paused {
        "Resume popup hotkey"
    } else {
        "Pause popup hotkey"
    }
}

pub struct PauseHotkeyItem(pub tauri::menu::MenuItem<tauri::Wry>);

/// Bring the OS registrations in line with the wanted state. Idempotent; safe from any thread.
pub fn sync(app: &AppHandle) {
    let _g = SYNC.lock().unwrap_or_else(|e| e.into_inner());
    let Some(st) = app.try_state::<Hotkeys>() else {
        return;
    };
    let b = st.current();
    let want = USER_PAUSED.load(Ordering::SeqCst)
        || (b.pause_in_games && AUTO_PAUSED.load(Ordering::SeqCst));
    if want == RELEASED.load(Ordering::SeqCst) {
        return;
    }
    for which in ["popup", "clipboard"] {
        let Some(sc) = field(&b, which).as_deref().and_then(parse) else {
            continue;
        };
        let gs = gs();
        if want {
            let _ = gs.unregister(sc);
        } else if let Err(e) = gs.register(sc) {
            eprintln!("winseek: could not re-register {which} hotkey on resume ({e})");
        }
    }
    RELEASED.store(want, Ordering::SeqCst);
    refresh_tray(app);
}

/// True while the Settings recorder is open.
static RECORDING: AtomicBool = AtomicBool::new(false);
static MENU_APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();
const MENU_GUARD_ID: usize = 0x5753_4B48; // "WSKH"

unsafe extern "system" fn menu_guard_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _id: usize,
    _ref: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::WM_SYSCOMMAND;
    const SC_KEYMENU: usize = 0xF100;
    // Alt+<key> asks for the window menu via WM_SYSCOMMAND/SC_KEYMENU; lparam is the key's char
    // (' ' for Alt+Space). While recording, eat it so the keystroke stays the recorder's.
    if msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) == SC_KEYMENU && RECORDING.load(Ordering::SeqCst)
    {
        // WebView2 never hands Alt+Space to the page as a keydown, so report it ourselves.
        if lparam.0 == 0x20 {
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
            // SAFETY: plain key-state query.
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            if let Some(app) = MENU_APP.get() {
                let _ = app.emit("hotkey-sys-space", serde_json::json!({ "shift": shift }));
            }
        }
        return windows::Win32::Foundation::LRESULT(0);
    }
    // SAFETY: standard fallthrough to the original window procedure.
    unsafe { windows::Win32::UI::Shell::DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Subclass the popup window (once; re-installing the same proc+id is a no-op update).
fn install_menu_guard(app: &AppHandle) {
    let _ = MENU_APP.set(app.clone());
    let Ok((hwnd, _)) = crate::shell::popup_hwnd(app) else {
        return;
    };
    let raw = hwnd.0 as isize;
    let _ = crate::shell::on_main(app, move || {
        let h = windows::Win32::Foundation::HWND(raw as *mut _);
        // SAFETY: called on the thread that owns the window; the proc is a 'static fn.
        unsafe {
            let _ = windows::Win32::UI::Shell::SetWindowSubclass(
                h,
                Some(menu_guard_proc),
                MENU_GUARD_ID,
                0,
            );
        }
        Ok(())
    });
}

/// While the Settings recorder is open every global hotkey is released, so the keystroke the
/// user presses (e.g. the current Alt+Space) reaches the recorder instead of triggering the
/// popup. Turning it off re-registers whatever is wanted (honouring pause).
pub fn capture(app: &AppHandle, on: bool) {
    RECORDING.store(on, Ordering::SeqCst);
    if on {
        install_menu_guard(app);
    }
    let _g = SYNC.lock().unwrap_or_else(|e| e.into_inner());
    let Some(st) = app.try_state::<Hotkeys>() else {
        return;
    };
    let b = st.current();
    let gs = gs();
    for which in WHICH {
        let Some(sc) = field(&b, which).as_deref().and_then(parse) else {
            continue;
        };
        let _ = gs.unregister(sc);
        if !on && (which == "library" || !is_released()) {
            if let Err(e) = gs.register(sc) {
                eprintln!("winseek: could not re-register {which} hotkey after capture ({e})");
            }
        }
    }
}

/// Flip the user's pause (tray). Returns the new state.
pub fn toggle_user_pause(app: &AppHandle) -> bool {
    let now = !USER_PAUSED.load(Ordering::SeqCst);
    USER_PAUSED.store(now, Ordering::SeqCst);
    if let Some(it) = app.try_state::<PauseHotkeyItem>() {
        let _ = it.0.set_text(pause_label(now));
    }
    sync(app);
    now
}

/// Persist the "pause in games" choice and apply it.
pub fn set_pause_in_games(app: &AppHandle, on: bool) -> Result<(), String> {
    let st = app.state::<Hotkeys>();
    let mut b = st.current();
    if b.pause_in_games != on {
        b.pause_in_games = on;
        save(&b)?;
        st.set(b.clone());
        let _ = app.emit("hotkeys-changed", &b);
    }
    sync(app);
    Ok(())
}

/// Watch for full-screen games (read-only OS query, every few seconds) and release / restore the
/// hotkeys. Does nothing unless the user opted in.
fn spawn_game_watch(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("hotkey-game-watch".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let on = app
                .try_state::<Hotkeys>()
                .is_some_and(|s| s.current().pause_in_games);
            let game = on && crate::tier1::policy::game_fullscreen_now();
            if AUTO_PAUSED.swap(game, Ordering::SeqCst) != game || (!on && is_released()) {
                sync(&app);
            }
        });
}

pub struct Hotkeys(Mutex<Bindings>);

impl Hotkeys {
    pub fn current(&self) -> Bindings {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn set(&self, b: Bindings) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = b;
    }
}

pub fn path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("hotkeys.json")
}

/// Missing file -> defaults. Corrupt -> defaults too: a broken file must not silence the popup.
pub fn load() -> Bindings {
    tier1::data_root()
        .map(|root| load_from(&root))
        .unwrap_or_default()
}

pub fn load_from(root: &std::path::Path) -> Bindings {
    match std::fs::read(path(root)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!("winseek: hotkeys.json is not valid ({e}); using defaults");
            Bindings::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Bindings::default(),
        Err(e) => {
            eprintln!("winseek: hotkeys.json unreadable ({e}); using defaults");
            Bindings::default()
        }
    }
}

pub fn save(b: &Bindings) -> Result<(), String> {
    let Some(root) = tier1::data_root() else {
        return Err("no data dir".into());
    };
    save_to(&root, b)
}

pub fn save_to(root: &std::path::Path, b: &Bindings) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(b).map_err(|e| e.to_string())?;
    std::fs::write(path(root), json).map_err(|e| e.to_string())
}

pub use crate::winhk::{parse, Shortcut};

/// OS-level (un)registration, via [`crate::winhk`].
struct Gs;
impl Gs {
    fn register(&self, sc: Shortcut) -> Result<(), String> {
        crate::winhk::register(sc)
    }
    fn unregister(&self, sc: Shortcut) -> Result<(), String> {
        crate::winhk::unregister(sc);
        Ok(())
    }
}
fn gs() -> Gs {
    Gs
}

/// A hotkey press: run whichever binding it matches. Reads the CURRENT bindings, never a
/// captured constant — Settings can rebind while the app runs (PRD RB4).
pub fn dispatch(app: &AppHandle, sc: Shortcut) {
    let Some(st) = app.try_state::<Hotkeys>() else {
        return;
    };
    let b = st.current();
    let is = |c: &Option<String>| c.as_deref().and_then(parse) == Some(sc);
    if is(&b.popup) {
        crate::popup::toggle(app);
    } else if is(&b.library) {
        if let Err(e) = crate::libwindow::open(app) {
            eprintln!("winseek: could not open the library: {e}");
        }
    } else if is(&b.clipboard) {
        show_with_clipboard(app);
    }
}

const WHICH: [&str; 3] = ["popup", "library", "clipboard"];

/// Load, manage and register. A saved combo that fails to register falls back to the default
/// (logged): the popup hotkey is the only way in, it must keep working.
pub fn init(app: &AppHandle) {
    let handle = app.clone();
    crate::winhk::start(move |sc| dispatch(&handle, sc));
    let mut b = load();
    for which in WHICH {
        let Some(combo_str) = field(&b, which).clone() else {
            continue;
        };
        let sc = match parse(&combo_str) {
            Some(sc) => sc,
            None => {
                eprintln!("winseek: hotkey {which}={combo_str:?} is not a valid combo; ignored");
                continue;
            }
        };
        if let Err(e) = gs().register(sc) {
            eprintln!("winseek: could not register {which} hotkey {combo_str:?} ({e})");
            if which == "popup" {
                let default = Bindings::default().popup.unwrap();
                match parse(&default) {
                    Some(sc) if gs().register(sc).is_ok() => {
                        eprintln!("winseek: fell back to the default popup hotkey ({default})");
                        b.popup = Some(default);
                    }
                    _ => eprintln!(
                        "winseek: the default popup hotkey failed too; Alt+Space may be taken"
                    ),
                }
            }
        }
    }
    app.manage(Hotkeys(Mutex::new(b)));
    refresh_tray(app);
    spawn_game_watch(app.clone());
}

fn field<'a>(b: &'a Bindings, which: &str) -> &'a Option<String> {
    match which {
        "popup" => &b.popup,
        "library" => &b.library,
        _ => &b.clipboard,
    }
}

fn set_field(b: &mut Bindings, which: &str, v: Option<String>) {
    match which {
        "popup" => b.popup = v,
        "library" => b.library = v,
        _ => b.clipboard = v,
    }
}

fn refresh_tray(app: &AppHandle) {
    let b = app.state::<Hotkeys>().current();
    if let Some(t) = app.tray_by_id(crate::watcher::TRAY_ID) {
        let tip = if is_released() {
            "winseek (hotkey paused)".to_string()
        } else {
            format!("winseek ({})", b.popup.as_deref().unwrap_or("no hotkey"))
        };
        let _ = t.set_tooltip(Some(tip));
    }
}

/// Swap one binding: unregister old -> register new -> on failure re-register old and report;
/// persist only on success. Returns the full set (the UI replaces its state with it).
pub fn set(app: &AppHandle, which: &str, combo_str: Option<String>) -> Result<Bindings, String> {
    if !WHICH.contains(&which) {
        return Err(format!("unknown hotkey {which}"));
    }
    let st = app.state::<Hotkeys>();
    let mut b = st.current();
    let old = field(&b, which).clone();
    if old == combo_str {
        return Ok(b);
    }
    let new_sc = match combo_str.as_deref() {
        None => None,
        Some(c) => Some(parse(c).ok_or_else(|| {
            format!("\"{c}\" is not a valid shortcut — combine Ctrl, Alt or Win with a key (Shift alone can't be global), or use F13-F24 on its own")
        })?),
    };
    if which == "popup" && new_sc.is_none() {
        return Err(
            "winseek needs a popup hotkey; pick another combination instead of clearing it".into(),
        );
    }
    // While popup/clipboard are released (pause) nothing is registered at the OS level: just
    // validate + save; `sync` registers the new combo on resume.
    let live = which == "library" || !is_released();
    if let Some(old_str) = old.as_ref().filter(|_| live) {
        if let Some(sc) = parse(old_str) {
            let _ = gs().unregister(sc);
        }
    }
    if let Some(sc) = new_sc.as_ref().filter(|_| live) {
        if let Err(e) = gs().register(*sc) {
            // Put the old one back before reporting.
            if let Some(old_str) = &old {
                if let Some(sc) = parse(old_str) {
                    let _ = gs().register(sc);
                }
            }
            return Err(format!(
                "That shortcut is already taken by another app or by Windows ({e}). The previous shortcut still works."
            ));
        }
    }
    set_field(&mut b, which, combo_str);
    save(&b)?;
    st.set(b.clone());
    refresh_tray(app);
    let _ = app.emit("hotkeys-changed", &b);
    Ok(b)
}

/// The clipboard hotkey (P1): show the popup with the clipboard text as the query. A read
/// failure still shows the popup — only the pre-filled query is lost.
pub fn show_with_clipboard(app: &AppHandle) {
    let text = crate::shell::popup_hwnd(app)
        .ok()
        .and_then(|(h, _)| crate::shell::clipboard_text(h));
    crate::popup::show(app);
    if let Some(t) = text.filter(|t| !t.trim().is_empty()) {
        let _ = app.emit_to(
            crate::popup::LABEL,
            "set-query",
            serde_json::json!({ "query": t }),
        );
    }
}

#[tauri::command]
pub async fn hotkeys_get(app: AppHandle) -> Result<Bindings, String> {
    Ok(app.state::<Hotkeys>().current())
}

#[tauri::command]
pub async fn hotkeys_capture(app: AppHandle, on: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || capture(&app, on))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn hotkeys_set(
    app: AppHandle,
    which: String,
    combo: Option<String>,
) -> Result<Bindings, String> {
    tauri::async_runtime::spawn_blocking(move || set(&app, &which, combo))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_hotkeys_json_without_pause_field_loads_with_it_off() {
        let b: Bindings =
            serde_json::from_str(r#"{"popup":"Alt+Space","library":null,"clipboard":null}"#)
                .unwrap();
        assert!(!b.pause_in_games);
        assert_eq!(pause_label(true), "Resume popup hotkey");
        assert_eq!(pause_label(false), "Pause popup hotkey");
    }

    #[test]
    fn bindings_roundtrip_and_defaults() {
        let tmp = crate::tier1::test_tmp();
        // Missing file -> defaults; the default popup hotkey is always present.
        let b = load_from(tmp.path());
        assert_eq!(b.popup.as_deref(), Some("Alt+Space"));
        assert_eq!(b.library, None);
        let b = Bindings {
            popup: Some("Ctrl+Alt+Space".into()),
            library: Some("Ctrl+Shift+L".into()),
            clipboard: None,
            pause_in_games: true,
        };
        save_to(tmp.path(), &b).unwrap();
        assert_eq!(load_from(tmp.path()), b);
        // A corrupt file falls back to the defaults instead of silencing the app.
        std::fs::write(path(tmp.path()), b"{ broken").unwrap();
        assert_eq!(load_from(tmp.path()), Bindings::default());
    }
}
