//! Global hotkeys straight on `RegisterHotKey`, the way Flow Launcher / PowerToys Run do it.
//!
//! The tauri global-shortcut plugin (global-hotkey crate) names keys by *physical position*
//! (`Code::KeyZ`) and then hard-codes that to the US-layout virtual key `VK_Z`, but Windows
//! matches hotkeys by *virtual key*, which follows the user's layout. On AZERTY the key
//! labelled W was recorded as "Alt+Z" and registered as the key labelled Z — and it has no code
//! at all for the ISO `<` key. Here a combo names the **keycap** ("Alt+W" = the key that says W),
//! which is exactly the virtual key, on every layout.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, VkKeyScanW, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_APP, WM_HOTKEY,
};

/// A parsed combo: `MOD_*` bits + a Windows virtual-key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub mods: u32,
    pub vk: u32,
}

const M_ALT: u32 = MOD_ALT.0;
const M_CTRL: u32 = MOD_CONTROL.0;
const M_SHIFT: u32 = MOD_SHIFT.0;
const M_WIN: u32 = MOD_WIN.0;

/// "Ctrl+Alt+Space" -> Shortcut. Needs Ctrl/Alt/Win behind the key unless it is F13-F24
/// (Flow.Launcher's rule: a bare or Shift-only global key would swallow typing everywhere).
pub fn parse(combo: &str) -> Option<Shortcut> {
    let mut parts = combo.split('+');
    let mut mods = 0u32;
    let mut key = None;
    for part in parts.by_ref() {
        match part {
            "Ctrl" => mods |= M_CTRL,
            "Alt" => mods |= M_ALT,
            "Shift" => mods |= M_SHIFT,
            "Win" => mods |= M_WIN,
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
    let real = mods & (M_CTRL | M_ALT | M_WIN) != 0;
    if !real && !is_function_key(key) {
        return None;
    }
    Some(Shortcut { mods, vk: vk_of(key)? })
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (13..=24).contains(&n))
}

/// Keycap name -> virtual key.
fn vk_of(key: &str) -> Option<u32> {
    let named = match key {
        "Esc" | "Escape" => 0x1B,
        "Space" => 0x20,
        "Enter" => 0x0D,
        "Tab" => 0x09,
        "Backspace" => 0x08,
        "Delete" => 0x2E,
        "Insert" => 0x2D,
        "Home" => 0x24,
        "End" => 0x23,
        "PgUp" | "PageUp" => 0x21,
        "PgDn" | "PageDown" => 0x22,
        "Left" => 0x25,
        "Up" => 0x26,
        "Right" => 0x27,
        "Down" => 0x28,
        "Menu" | "ContextMenu" => 0x5D,
        "PrintScreen" => 0x2C,
        "Pause" => 0x13,
        "ScrollLock" => 0x91,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    // F1..F24
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    // numpad digits are stored as Num0..Num9 so "+" stays the only separator
    if let Some(d) = key.strip_prefix("Num").and_then(|n| n.parse::<u32>().ok()) {
        if d <= 9 {
            return Some(0x60 + d);
        }
    }
    let mut chars = key.chars();
    let (c, rest) = (chars.next()?, chars.next());
    if rest.is_some() {
        return None;
    }
    // letters and digits: the virtual key IS the (upper-case) character on every layout
    if c.is_ascii_uppercase() || c.is_ascii_digit() {
        return Some(c as u32);
    }
    // punctuation: ask the active layout which key types this character
    if c.is_ascii() && !c.is_ascii_control() {
        // SAFETY: plain query of the calling thread's keyboard layout.
        let r = unsafe { VkKeyScanW(c as u16) };
        if r != -1 {
            return Some((r as u16 & 0xFF) as u32);
        }
    }
    None
}

enum Cmd {
    Register(Shortcut, Sender<Result<(), String>>),
    Unregister(Shortcut, Sender<()>),
}

struct Worker {
    tid: u32,
    tx: Mutex<Sender<Cmd>>,
}

static WORKER: OnceLock<Worker> = OnceLock::new();
const WM_CMD: u32 = WM_APP + 1;

/// Start the hotkey thread (once). `on_hotkey` runs on that thread for every press.
pub fn start(on_hotkey: impl Fn(Shortcut) + Send + 'static) {
    if WORKER.get().is_some() {
        return;
    }
    let (tx, rx) = channel::<Cmd>();
    let (ready_tx, ready_rx) = channel::<u32>();
    std::thread::Builder::new()
        .name("winseek-hotkeys".into())
        .spawn(move || {
            // Force the message queue into existence before announcing the thread id.
            let mut msg = MSG::default();
            // SAFETY: standard message-loop calls on this thread only.
            unsafe {
                let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                let _ = ready_tx.send(GetCurrentThreadId());
            }
            let mut ids: HashMap<Shortcut, i32> = HashMap::new();
            let mut by_id: HashMap<i32, Shortcut> = HashMap::new();
            let mut next_id = 1i32;
            loop {
                // SAFETY: as above.
                let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if got.0 <= 0 {
                    break;
                }
                if msg.message == WM_HOTKEY {
                    if let Some(sc) = by_id.get(&(msg.wParam.0 as i32)) {
                        on_hotkey(*sc);
                    }
                    continue;
                }
                if msg.message != WM_CMD {
                    continue;
                }
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        Cmd::Register(sc, reply) => {
                            if ids.contains_key(&sc) {
                                let _ = reply.send(Ok(()));
                                continue;
                            }
                            let id = next_id;
                            next_id += 1;
                            // SAFETY: registers on the thread that pumps its messages.
                            let r = unsafe {
                                RegisterHotKey(
                                    None,
                                    id,
                                    HOT_KEY_MODIFIERS(sc.mods) | MOD_NOREPEAT,
                                    sc.vk,
                                )
                            };
                            let _ = reply.send(match r {
                                Ok(()) => {
                                    ids.insert(sc, id);
                                    by_id.insert(id, sc);
                                    Ok(())
                                }
                                Err(e) => Err(e.message()),
                            });
                        }
                        Cmd::Unregister(sc, reply) => {
                            if let Some(id) = ids.remove(&sc) {
                                by_id.remove(&id);
                                // SAFETY: same thread that registered it.
                                let _ = unsafe { UnregisterHotKey(None, id) };
                            }
                            let _ = reply.send(());
                        }
                    }
                }
            }
        })
        .expect("spawn hotkey thread");
    if let Ok(tid) = ready_rx.recv() {
        let _ = WORKER.set(Worker { tid, tx: Mutex::new(tx) });
    }
}

fn send(cmd: Cmd) -> bool {
    let Some(w) = WORKER.get() else {
        return false;
    };
    let tx = w.tx.lock().unwrap_or_else(|e| e.into_inner());
    if tx.send(cmd).is_err() {
        return false;
    }
    // SAFETY: wakes the hotkey thread's message loop.
    unsafe { PostThreadMessageW(w.tid, WM_CMD, WPARAM(0), LPARAM(0)).is_ok() }
}

/// Register at the OS level. `Err` carries Windows' own reason (e.g. already taken by another app).
pub fn register(sc: Shortcut) -> Result<(), String> {
    let (rtx, rrx) = channel();
    if !send(Cmd::Register(sc, rtx)) {
        return Err("the hotkey thread is not running".into());
    }
    rrx.recv().map_err(|e| e.to_string())?
}

pub fn unregister(sc: Shortcut) {
    let (rtx, rrx) = channel();
    if send(Cmd::Unregister(sc, rtx)) {
        let _ = rrx.recv();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_named_keys_are_virtual_keys() {
        assert_eq!(parse("Alt+W"), Some(Shortcut { mods: M_ALT, vk: 'W' as u32 }));
        assert_eq!(parse("Alt+Space").unwrap().vk, 0x20);
        assert_eq!(parse("Ctrl+Alt+Shift+F13").unwrap().vk, 0x7C);
        assert_eq!(parse("Ctrl+1").unwrap().vk, '1' as u32);
        assert_eq!(parse("Ctrl+Num5").unwrap().vk, 0x65);
        assert_eq!(parse("Win+Up").unwrap().vk, 0x26);
    }

    #[test]
    fn punctuation_resolves_through_the_layout() {
        // whatever layout runs the tests, these characters have a key
        for c in ["Alt+<", "Ctrl+;", "Alt+[", "Ctrl+,", "Alt+."] {
            assert!(parse(c).is_some(), "{c}");
        }
    }

    #[test]
    fn rejects_unsafe_or_garbage() {
        assert_eq!(parse("Space"), None);
        assert_eq!(parse("A"), None);
        assert_eq!(parse("Shift+A"), None, "a global Shift+A swallows capital A");
        assert_eq!(parse("Shift+F5"), None);
        assert_eq!(parse("Ctrl+"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("Ctrl+Alt"), None, "modifiers only");
        assert_eq!(parse("Ctrl+1+2"), None, "two main keys");
        assert!(parse("F13").is_some());
    }
}
