//! Native shell context menu (IContextMenu) shown at a point in the popup.

use super::{fileop, shell_item, MenuGuard};
use windows::core::{Interface, PCSTR, PCWSTR, PSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
use windows::Win32::UI::Shell::{
    BHID_SFUIObject, DefSubclassProc, IContextMenu, IContextMenu2, IContextMenu3, RemoveWindowSubclass,
    SetWindowSubclass, CMF_CANRENAME, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_PTINVOKE, SEE_MASK_UNICODE,
    CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, GCS_VERBA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, SetForegroundWindow, TrackPopupMenuEx, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR,
};

const SUBCLASS_ID: usize = 0x5753_4D45; // "WSME"
const ID_FIRST: u32 = 1;
const ID_LAST: u32 = 0x7FFF;

/// Menu-message targets for owner-drawn / lazily populated shell submenus (Send to, Open with).
struct MsgTarget {
    cm2: Option<IContextMenu2>,
    cm3: Option<IContextMenu3>,
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    refdata: usize,
) -> LRESULT {
    if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
        // SAFETY: refdata points at the MsgTarget owned by `show`, alive while the subclass is installed.
        let t = unsafe { &*(refdata as *const MsgTarget) };
        let mut res = LRESULT(0);
        if let Some(cm3) = &t.cm3 {
            // SAFETY: forwarding the original message parameters.
            if unsafe { cm3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut res)) }.is_ok() {
                return res;
            }
        } else if let Some(cm2) = &t.cm2 {
            // SAFETY: as above.
            if unsafe { cm2.HandleMenuMsg(msg, wparam, lparam) }.is_ok() {
                return LRESULT(0);
            }
        }
    }
    // SAFETY: standard fallthrough to the original window procedure.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Removes the popup subclass on every exit path.
struct Subclass(HWND);

impl Subclass {
    fn install(hwnd: HWND, target: &MsgTarget) -> Result<Self, String> {
        // SAFETY: `target` outlives the guard (declared before it in `show`).
        let ok = unsafe {
            SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, target as *const MsgTarget as usize)
        };
        if ok.as_bool() {
            Ok(Self(hwnd))
        } else {
            Err("SetWindowSubclass failed".into())
        }
    }
}

impl Drop for Subclass {
    fn drop(&mut self) {
        // SAFETY: removes the subclass installed in `install`.
        unsafe {
            let _ = RemoveWindowSubclass(self.0, Some(subclass_proc), SUBCLASS_ID);
        }
    }
}

fn verb_of(cm: &IContextMenu, offset: usize) -> String {
    let mut buf = [0u8; 128];
    // SAFETY: buffer length is passed as cchmax.
    let r = unsafe { cm.GetCommandString(offset, GCS_VERBA, None, PSTR(buf.as_mut_ptr()), buf.len() as u32) };
    if r.is_err() {
        return String::new();
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).to_lowercase()
}

/// `x`,`y` are physical client-area pixels of `hwnd`.
pub(super) fn show(hwnd: HWND, path: &str, x: f64, y: f64) -> Result<String, String> {
    let item = shell_item(path)?;
    // SAFETY: valid item; the bound handler is an owned COM object.
    let cm: IContextMenu = unsafe { item.BindToHandler(None, &BHID_SFUIObject) }.map_err(|e| e.to_string())?;
    let target = MsgTarget { cm2: cm.cast().ok(), cm3: cm.cast().ok() };

    // SAFETY: GetKeyState is a plain query.
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let mut flags = CMF_NORMAL | CMF_CANRENAME;
    if shift {
        flags |= CMF_EXTENDEDVERBS;
    }

    let mut pt = POINT { x: x.round() as i32, y: y.round() as i32 };
    // SAFETY: menu is owned by MenuGuard; subclass removed by Subclass drop; all COM calls on the main STA.
    let cmd = unsafe {
        let menu = MenuGuard(CreatePopupMenu().map_err(|e| e.to_string())?);
        cm.QueryContextMenu(menu.0, 0, ID_FIRST, ID_LAST, flags).ok().map_err(|e| e.to_string())?;
        let _ = ClientToScreen(hwnd, &mut pt);
        let _sub = Subclass::install(hwnd, &target)?;
        let _ = SetForegroundWindow(hwnd);
        TrackPopupMenuEx(menu.0, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.x, pt.y, hwnd, None).0 as u32
    };
    if cmd == 0 {
        return Ok("cancelled".into());
    }
    let offset = cmd - ID_FIRST;
    match verb_of(&cm, offset as usize).as_str() {
        "rename" => return Ok("rename".into()),
        "delete" => {
            fileop::recycle(hwnd, &[path.to_string()])?;
            return Ok("deleted".into());
        }
        _ => {}
    }
    let verb = offset as usize as *const u8;
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: SEE_MASK_UNICODE | CMIC_MASK_PTINVOKE,
        hwnd,
        lpVerb: PCSTR(verb),
        lpVerbW: PCWSTR(verb as *const u16),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: pt,
        ..Default::default()
    };
    // SAFETY: EX struct is layout-compatible with CMINVOKECOMMANDINFO (cbSize says which).
    unsafe { cm.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO) }.map_err(|e| e.to_string())?;
    Ok("invoked".into())
}
