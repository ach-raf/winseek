//! Copy files to the clipboard as CF_HDROP + `Preferred DropEffect` = COPY, or text as
//! CF_UNICODETEXT with an optional `HTML Format` (CF_HTML) twin for pasting tables into Office.

use super::wide;
use std::time::Duration;
use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY};
use windows::Win32::UI::Shell::DROPFILES;

/// DROPFILES header + double-NUL-terminated wide path list.
fn hdrop_bytes(paths: &[String]) -> Vec<u8> {
    let head = std::mem::size_of::<DROPFILES>();
    let mut b = vec![0u8; head];
    b[..4].copy_from_slice(&(head as u32).to_le_bytes()); // pFiles
    b[16..20].copy_from_slice(&1u32.to_le_bytes()); // fWide
    for p in paths {
        for u in wide(p) {
            b.extend_from_slice(&u.to_le_bytes());
        }
    }
    b.extend_from_slice(&[0, 0]);
    b
}

/// Allocate a movable global with `data`; freed by the caller unless the clipboard takes it.
fn global_with(data: &[u8]) -> Result<HGLOBAL, String> {
    // SAFETY: allocate, lock, copy exactly `data.len()` bytes, unlock.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, data.len()).map_err(|e| e.to_string())?;
        let p = GlobalLock(h);
        if p.is_null() {
            let _ = GlobalFree(Some(h));
            return Err("GlobalLock failed".into());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len());
        let _ = GlobalUnlock(h);
        Ok(h)
    }
}

pub(super) fn open_clipboard(hwnd: HWND) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..10 {
        // SAFETY: plain Win32 call.
        match unsafe { OpenClipboard(Some(hwnd)) } {
            Ok(()) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("clipboard busy: {last}"))
}

pub(super) fn copy_files(hwnd: HWND, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("no paths".into());
    }
    let drop = global_with(&hdrop_bytes(paths))?;
    let effect = match global_with(&DROPEFFECT_COPY.0.to_le_bytes()) {
        Ok(h) => h,
        Err(e) => {
            // SAFETY: `drop` was never handed to the clipboard.
            unsafe {
                let _ = GlobalFree(Some(drop));
            }
            return Err(e);
        }
    };
    if let Err(e) = open_clipboard(hwnd) {
        // SAFETY: neither handle was handed to the clipboard.
        unsafe {
            let _ = GlobalFree(Some(drop));
            let _ = GlobalFree(Some(effect));
        }
        return Err(e);
    }
    // SAFETY: clipboard is open; ownership of each HGLOBAL moves to the system on success.
    unsafe {
        let _ = EmptyClipboard();
        let fmt_effect = RegisterClipboardFormatW(w!("Preferred DropEffect"));
        let r1 = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(drop.0)));
        let r2 = SetClipboardData(fmt_effect, Some(HANDLE(effect.0)));
        if r1.is_err() {
            let _ = GlobalFree(Some(drop));
        }
        if r2.is_err() {
            let _ = GlobalFree(Some(effect));
        }
        let _ = CloseClipboard();
        r1.map(|_| ()).map_err(|e| e.to_string())
    }
}

/// NUL-terminated UTF-16LE bytes (CF_UNICODETEXT).
fn utf16_nul_bytes(s: &str) -> Vec<u8> {
    wide(s).iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// CF_HTML payload for `fragment`: the offset header (fixed-width numbers, byte offsets into this UTF-8
/// string) plus the `<!--StartFragment-->` / `<!--EndFragment-->` markers Office looks for.
fn cf_html(fragment: &str) -> String {
    const PRE: &str = "<html><body>\r\n<!--StartFragment-->";
    const POST: &str = "<!--EndFragment-->\r\n</body></html>";
    // Every number is 10 digits wide, so the header length is known before the numbers are.
    let header = |sh: usize, eh: usize, sf: usize, ef: usize| {
        format!("Version:0.9\r\nStartHTML:{sh:010}\r\nEndHTML:{eh:010}\r\nStartFragment:{sf:010}\r\nEndFragment:{ef:010}\r\n")
    };
    let start_html = header(0, 0, 0, 0).len();
    let start_fragment = start_html + PRE.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + POST.len();
    format!("{}{PRE}{fragment}{POST}", header(start_html, end_html, start_fragment, end_fragment))
}

/// Put `text` (and, when given, `html` as `HTML Format`) on the clipboard.
pub(super) fn copy_text_html(hwnd: HWND, text: &str, html: Option<&str>) -> Result<(), String> {
    let unicode = global_with(&utf16_nul_bytes(text))?;
    let html_mem = match html {
        Some(h) => {
            let mut bytes = cf_html(h).into_bytes();
            bytes.push(0);
            match global_with(&bytes) {
                Ok(m) => Some(m),
                Err(e) => {
                    // SAFETY: `unicode` was never handed to the clipboard.
                    unsafe {
                        let _ = GlobalFree(Some(unicode));
                    }
                    return Err(e);
                }
            }
        }
        None => None,
    };
    if let Err(e) = open_clipboard(hwnd) {
        // SAFETY: neither handle was handed to the clipboard.
        unsafe {
            let _ = GlobalFree(Some(unicode));
            if let Some(m) = html_mem {
                let _ = GlobalFree(Some(m));
            }
        }
        return Err(e);
    }
    // SAFETY: clipboard is open; ownership of each HGLOBAL moves to the system on success.
    unsafe {
        let _ = EmptyClipboard();
        let r1 = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(unicode.0)));
        if r1.is_err() {
            let _ = GlobalFree(Some(unicode));
        }
        if let Some(m) = html_mem {
            let fmt_html = RegisterClipboardFormatW(w!("HTML Format"));
            if SetClipboardData(fmt_html, Some(HANDLE(m.0))).is_err() {
                let _ = GlobalFree(Some(m));
            }
        }
        let _ = CloseClipboard();
        r1.map(|_| ()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::GetClipboardData;
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP};

    #[test]
    fn cf_html_offsets_point_at_the_fragment() {
        for fragment in ["<table><tr><td>a</td></tr></table>", "", "<td>caf\u{e9} \u{65e5}\u{672c}</td>"] {
            let s = cf_html(fragment);
            let num = |key: &str| -> usize {
                let at = s.find(key).unwrap() + key.len();
                s[at..at + 10].parse().unwrap()
            };
            let (sh, eh, sf, ef) = (num("StartHTML:"), num("EndHTML:"), num("StartFragment:"), num("EndFragment:"));
            assert_eq!(eh, s.len(), "EndHTML is the end of the payload");
            assert_eq!(&s[sf..ef], fragment, "the fragment sits between the offsets (byte offsets)");
            assert!(s[sh..].starts_with("<html>"));
            assert!(s[..sf].ends_with("<!--StartFragment-->") && s[ef..].starts_with("<!--EndFragment-->"));
            assert!(s.starts_with("Version:0.9\r\n"));
        }
    }

    #[test]
    fn unicode_text_is_utf16_with_a_terminator() {
        assert_eq!(utf16_nul_bytes("a\u{e9}"), vec![0x61, 0, 0xe9, 0, 0, 0]);
        assert_eq!(utf16_nul_bytes(""), vec![0, 0]);
    }

    #[test]
    fn hdrop_layout() {
        let b = hdrop_bytes(&["C:\\a".into(), "C:\\b".into()]);
        assert_eq!(b.len(), 20 + (4 + 1 + 4 + 1) * 2 + 2);
        assert_eq!(&b[b.len() - 4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn copy_then_read_back() {
        let path = std::env::temp_dir().join("winseek_clip_test.txt");
        std::fs::write(&path, b"x").unwrap();
        let p = path.to_string_lossy().into_owned();
        // SAFETY: STATIC window owned by this test thread; destroyed at the end.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
        }
        .expect("window");
        copy_files(hwnd, std::slice::from_ref(&p)).expect("copy");
        open_clipboard(hwnd).expect("open");
        // SAFETY: clipboard is open; the HDROP stays valid until CloseClipboard.
        let got = unsafe {
            let h = GetClipboardData(CF_HDROP.0 as u32).expect("hdrop");
            let hd = HDROP(h.0);
            let n = DragQueryFileW(hd, 0xFFFF_FFFF, None);
            let mut buf = vec![0u16; 1024];
            let len = DragQueryFileW(hd, 0, Some(&mut buf)) as usize;
            let _ = CloseClipboard();
            (n, String::from_utf16_lossy(&buf[..len]))
        };
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        std::fs::remove_file(&path).unwrap();
        assert_eq!(got.0, 1);
        assert_eq!(got.1, p);
    }
}
