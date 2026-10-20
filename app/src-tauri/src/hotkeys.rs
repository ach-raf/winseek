//! Rebindable global hotkeys (PRD RB4): `%LOCALAPPDATA%\winseek\hotkeys.json`
//! `{ popup, library, clipboard }`, each a canonical combo string ("Ctrl+Alt+Space") or null.
//! The strings are the single source of truth; they are parsed to `Shortcut`s only to talk to
//! the global-shortcut plugin, and the handler compares against the *current* bindings.

use std::str::FromStr;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};

use crate::tier1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    /// Show / hide winseek (required — the app is unreachable without one).
    pub popup: Option<String>,
    /// Open the Library window (optional).
    pub library: Option<String>,
    /// Show winseek with the clipboard text as the query (optional, P1).
    pub clipboard: Option<String>,
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings { popup: Some("Alt+Space".into()), library: None, clipboard: None }
    }
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
    tier1::data_root().map(|root| load_from(&root)).unwrap_or_default()
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
    let Some(root) = tier1::data_root() else { return Err("no data dir".into()) };
    save_to(&root, b)
}

pub fn save_to(root: &std::path::Path, b: &Bindings) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(b).map_err(|e| e.to_string())?;
    std::fs::write(path(root), json).map_err(|e| e.to_string())
}

/// "Ctrl+Alt+Space" -> Shortcut. Key names follow the UI keymap (Up, PgUp, Esc, …). A modifier
/// is required unless the key is F13-F24 (which nothing else claims).
pub fn parse(combo: &str) -> Option<Shortcut> {
    let mut parts = combo.split('+');
    let mut mods = Modifiers::empty();
    let mut key = None;
    for part in parts.by_ref() {
        match part {
            "Ctrl" => mods |= Modifiers::CONTROL,
            "Alt" => mods |= Modifiers::ALT,
            "Shift" => mods |= Modifiers::SHIFT,
            "Win" => mods |= Modifiers::SUPER,
            _ => {
                key = Some(part);
                break;
            }
        }
    }
    let key = key?;
    if parts.next().is_some() {
        return None; // two keys after the modifiers
    }
    if mods.is_empty() && !is_function_key(key) {
        return None;
    }
    let code = Code::from_str(&code_name(key)).ok()?;
    Some(Shortcut::new(Some(mods), code))
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (13..=24).contains(&n))
}

/// Keymap name -> keyboard_types `Code` name ("Up" -> "ArrowUp", "A" -> "KeyA").
fn code_name(key: &str) -> String {
    match key {
        "Esc" => "Escape".into(),
        "Up" => "ArrowUp".into(),
        "Down" => "ArrowDown".into(),
        "Left" => "ArrowLeft".into(),
        "Right" => "ArrowRight".into(),
        "PgUp" => "PageUp".into(),
        "PgDn" => "PageDown".into(),
        "Menu" => "ContextMenu".into(),
        "/" => "Slash".into(),
        "," => "Comma".into(),
        "=" => "Equal".into(),
        "-" => "Minus".into(),
        other => {
            if let Some(c) = other.chars().next() {
                if other.len() == 1 && c.is_ascii_uppercase() {
                    return format!("Key{c}");
                }
                if c.is_ascii_digit() && other.len() == 1 {
                    return format!("Digit{c}");
                }
            }
            other.into()
        }
    }
}

/// The canonical keymap name of a shortcut's key ("KeyA" -> "A", "ArrowUp" -> "Up").
fn key_name(code: &Code) -> String {
    let n = code.to_string();
    match n.as_str() {
        "Escape" => "Esc".into(),
        "ArrowUp" => "Up".into(),
        "ArrowDown" => "Down".into(),
        "ArrowLeft" => "Left".into(),
        "ArrowRight" => "Right".into(),
        "PageUp" => "PgUp".into(),
        "PageDown" => "PgDn".into(),
        "ContextMenu" => "Menu".into(),
        "Slash" => "/".into(),
        "Comma" => ",".into(),
        "Equal" => "=".into(),
        "Minus" => "-".into(),
        other => {
            if let Some(c) = other.strip_prefix("Key") {
                if c.len() == 1 {
                    return c.into();
                }
            }
            if let Some(c) = other.strip_prefix("Digit") {
                if c.len() == 1 {
                    return c.into();
                }
            }
            other.into()
        }
    }
}

/// Shortcut -> the canonical combo string, the twin of [`parse`].
pub fn combo(sc: &Shortcut) -> String {
    let mut out = String::new();
    let m = sc.mods;
    if m.contains(Modifiers::CONTROL) {
        out.push_str("Ctrl+");
    }
    if m.contains(Modifiers::ALT) {
        out.push_str("Alt+");
    }
    if m.contains(Modifiers::SHIFT) {
        out.push_str("Shift+");
    }
    if m.contains(Modifiers::SUPER) {
        out.push_str("Win+");
    }
    out.push_str(&key_name(&sc.key));
    out
}

const WHICH: [&str; 3] = ["popup", "library", "clipboard"];

/// Load, manage and register. A saved combo that fails to register falls back to the default
/// (logged): the popup hotkey is the only way in, it must keep working.
pub fn init(app: &AppHandle) {
    let mut b = load();
    for which in WHICH {
        let Some(combo_str) = field(&b, which).clone() else { continue };
        let sc = match parse(&combo_str) {
            Some(sc) => sc,
            None => {
                eprintln!("winseek: hotkey {which}={combo_str:?} is not a valid combo; ignored");
                continue;
            }
        };
        if let Err(e) = app.global_shortcut().register(sc) {
            eprintln!("winseek: could not register {which} hotkey {combo_str:?} ({e})");
            if which == "popup" {
                let default = Bindings::default().popup.unwrap();
                match parse(&default) {
                    Some(sc) if app.global_shortcut().register(sc).is_ok() => {
                        eprintln!("winseek: fell back to the default popup hotkey ({default})");
                        b.popup = Some(default);
                    }
                    _ => eprintln!("winseek: the default popup hotkey failed too; Alt+Space may be taken"),
                }
            }
        }
    }
    app.manage(Hotkeys(Mutex::new(b)));
    refresh_tray(app);
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
        let tip = format!("winseek ({})", b.popup.as_deref().unwrap_or("no hotkey"));
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
            format!("\"{c}\" is not a valid shortcut — combine Ctrl, Alt, Shift or Win with a key (or F13-F24)")
        })?),
    };
    if which == "popup" && new_sc.is_none() {
        return Err("winseek needs a popup hotkey; pick another combination instead of clearing it".into());
    }
    if let Some(old_str) = &old {
        if let Some(sc) = parse(old_str) {
            let _ = app.global_shortcut().unregister(sc);
        }
    }
    if let Some(sc) = &new_sc {
        if let Err(e) = app.global_shortcut().register(*sc) {
            // Put the old one back before reporting.
            if let Some(old_str) = &old {
                if let Some(sc) = parse(old_str) {
                    let _ = app.global_shortcut().register(sc);
                }
            }
            return Err(format!("That shortcut is taken by another app ({e}). The previous shortcut still works."));
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
    let text = crate::shell::popup_hwnd(app).ok().and_then(|(h, _)| crate::shell::clipboard_text(h));
    crate::popup::show(app);
    if let Some(t) = text.filter(|t| !t.trim().is_empty()) {
        let _ = app.emit_to(crate::popup::LABEL, "set-query", serde_json::json!({ "query": t }));
    }
}

#[tauri::command]
pub async fn hotkeys_get(app: AppHandle) -> Result<Bindings, String> {
    Ok(app.state::<Hotkeys>().current())
}

#[tauri::command]
pub async fn hotkeys_set(app: AppHandle, which: String, combo: Option<String>) -> Result<Bindings, String> {
    tauri::async_runtime::spawn_blocking(move || set(&app, &which, combo))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        let sc = parse("Alt+Space").unwrap();
        assert_eq!(sc.mods, Modifiers::ALT);
        assert_eq!(sc.key, Code::Space);
        let sc = parse("Ctrl+Alt+Shift+F13").unwrap();
        assert_eq!(sc.key, Code::F13);
        // letters and digits by keymap name
        assert_eq!(parse("Ctrl+L").unwrap().key, Code::KeyL);
        assert_eq!(parse("Ctrl+1").unwrap().key, Code::Digit1);
    }

    #[test]
    fn parse_rejects() {
        // no modifier, not an F13+ key
        assert_eq!(parse("Space"), None);
        assert_eq!(parse("A"), None);
        // garbage
        assert_eq!(parse("Ctrl+"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("Ctrl+Alt"), None, "modifiers only");
        assert_eq!(parse("Ctrl+1+2"), None, "two main keys");
        // F13-F24 are legal bare
        assert!(parse("F13").is_some());
    }

    #[test]
    fn roundtrip_through_combo() {
        for c in ["Alt+Space", "Ctrl+Alt+P", "Ctrl+Shift+F24", "Win+Space", "Ctrl+,"] {
            assert_eq!(combo(&parse(c).unwrap()), c);
        }
    }

    #[test]
    fn bindings_roundtrip_and_defaults() {
        let tmp = crate::tier1::test_tmp();
        // Missing file -> defaults; the default popup hotkey is always present.
        let b = load_from(tmp.path());
        assert_eq!(b.popup.as_deref(), Some("Alt+Space"));
        assert_eq!(b.library, None);
        let b = Bindings { popup: Some("Ctrl+Alt+Space".into()), library: Some("Ctrl+Shift+L".into()), clipboard: None };
        save_to(tmp.path(), &b).unwrap();
        assert_eq!(load_from(tmp.path()), b);
        // A corrupt file falls back to the defaults instead of silencing the app.
        std::fs::write(path(tmp.path()), b"{ broken").unwrap();
        assert_eq!(load_from(tmp.path()), Bindings::default());
    }
}
