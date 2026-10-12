//! Drag files out of the popup through the shell data object (copy/move/link).

use super::wide;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoTaskMemFree, IDataObject};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    SHCreateShellItemArrayFromIDLists, SHDoDragDrop, SHParseDisplayName, BHID_DataObject,
};

/// Absolute PIDL freed on drop.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        // SAFETY: allocated by SHParseDisplayName, freed once.
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

pub(super) fn start(hwnd: HWND, paths: &[String]) -> Result<String, String> {
    if paths.is_empty() {
        return Err("no paths".into());
    }
    let mut pidls = Vec::with_capacity(paths.len());
    for p in paths {
        let w = wide(p);
        let mut pidl = std::ptr::null_mut();
        // SAFETY: `w` is NUL-terminated; `pidl` receives an owned PIDL.
        unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None) }
            .map_err(|e| format!("{p}: {e}"))?;
        pidls.push(Pidl(pidl));
    }
    let raw: Vec<_> = pidls.iter().map(|p| p.0 as *const ITEMIDLIST).collect();
    // SAFETY: PIDLs stay alive (via `pidls`) until after the drag finishes.
    let effect = unsafe {
        let arr = SHCreateShellItemArrayFromIDLists(&raw).map_err(|e| e.to_string())?;
        let data: IDataObject = arr.BindToHandler(None, &BHID_DataObject).map_err(|e| e.to_string())?;
        SHDoDragDrop(Some(hwnd), &data, None, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK)
            .map_err(|e| e.to_string())?
    };
    Ok(if effect.0 & DROPEFFECT_MOVE.0 != 0 {
        "move"
    } else if effect.0 & DROPEFFECT_COPY.0 != 0 {
        "copy"
    } else if effect.0 & DROPEFFECT_LINK.0 != 0 {
        "link"
    } else {
        "none"
    }
    .into())
}
