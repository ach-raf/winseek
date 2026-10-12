//! Recycle / restore / rename through the shell (undoable, no confirmation UI).

use super::{shell_item, wide, MenuGuard};
use std::path::Path;
use windows::core::{Interface, GUID, PCSTR, PCWSTR, PWSTR};
use windows::Win32::Foundation::{FILETIME, HWND, PROPERTYKEY};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, BHID_SFUIObject, FileOperation, FOLDERID_RecycleBinFolder, IContextMenu, IEnumShellItems,
    IFileOperation, IShellItem, IShellItem2, SHGetKnownFolderItem, CMF_NORMAL, SEE_MASK_FLAG_NO_UI,
    CMINVOKECOMMANDINFO, FILEOPERATION_FLAGS, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION,
    FOF_NOERRORUI, FOF_SILENT, KF_FLAG_DEFAULT, SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
};
use windows::Win32::UI::WindowsAndMessaging::{CreatePopupMenu, SW_SHOWNORMAL};

/// Shell "Deleted from" / "Date deleted" columns of Recycle Bin items.
const PKEY_DELETED_FROM: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x9B174B33_40FF_11D2_A27E_00C04FC30871), pid: 2 };
const PKEY_DATE_DELETED: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x9B174B33_40FF_11D2_A27E_00C04FC30871), pid: 3 };

fn file_operation(hwnd: HWND, flags: FILEOPERATION_FLAGS) -> Result<IFileOperation, String> {
    // SAFETY: standard COM activation on an initialised apartment.
    unsafe {
        let fo: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        fo.SetOperationFlags(flags).map_err(|e| e.to_string())?;
        if !hwnd.is_invalid() {
            let _ = fo.SetOwnerWindow(hwnd);
        }
        Ok(fo)
    }
}

fn perform(fo: &IFileOperation) -> Result<(), String> {
    // SAFETY: operations were queued on this object.
    unsafe {
        fo.PerformOperations().map_err(|e| e.to_string())?;
        if fo.GetAnyOperationsAborted().map_err(|e| e.to_string())?.as_bool() {
            return Err("operation aborted".into());
        }
    }
    Ok(())
}

pub(super) fn recycle(hwnd: HWND, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let flags = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOFX_RECYCLEONDELETE;
    let fo = file_operation(hwnd, flags)?;
    for p in paths {
        let item = shell_item(p)?;
        // SAFETY: valid item; no progress sink.
        unsafe { fo.DeleteItem(&item, None) }.map_err(|e| e.to_string())?;
    }
    perform(&fo)
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err("invalid name".into());
    }
    if name.chars().any(|c| c.is_control() || "\\/:*?\"<>|".contains(c)) {
        return Err("name contains invalid characters".into());
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err("name cannot end with a space or dot".into());
    }
    Ok(())
}

pub(super) fn rename(hwnd: HWND, path: &str, new_name: &str) -> Result<String, String> {
    validate_name(new_name)?;
    let parent = Path::new(path).parent().ok_or("path has no parent")?;
    let item = shell_item(path)?;
    let fo = file_operation(hwnd, FOF_ALLOWUNDO | FOF_NOERRORUI | FOF_SILENT)?;
    let nw = wide(new_name);
    // SAFETY: `nw` is NUL-terminated and outlives the call.
    unsafe { fo.RenameItem(&item, PCWSTR(nw.as_ptr()), None) }.map_err(|e| e.to_string())?;
    perform(&fo)?;
    Ok(parent.join(new_name).to_string_lossy().into_owned())
}

fn take_string(p: PWSTR) -> String {
    // SAFETY: shell-allocated NUL-terminated string, freed once here.
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn norm(p: &str) -> String {
    p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Original full path of a Recycle Bin item, if resolvable.
fn original_path(item: &IShellItem) -> Option<String> {
    let it2: IShellItem2 = item.cast().ok()?;
    // SAFETY: valid item; strings are freed by take_string.
    unsafe {
        let from = take_string(it2.GetString(&PKEY_DELETED_FROM).ok()?);
        let disp = take_string(item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?);
        // Some systems return the full original path here; keep only the leaf.
        let disp = disp.rsplit(['\\', '/']).next().unwrap_or_default().to_string();
        // Display name may omit a hidden extension; the $R backing file keeps it.
        let ext = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()
            .map(take_string)
            .and_then(|r| Path::new(&r).extension().map(|e| format!(".{}", e.to_string_lossy())))
            .unwrap_or_default();
        let name = if !ext.is_empty() && !disp.to_lowercase().ends_with(&ext.to_lowercase()) {
            format!("{disp}{ext}")
        } else {
            disp
        };
        Some(format!("{}\\{}", from.trim_end_matches('\\'), name))
    }
}

fn deleted_at(item: &IShellItem) -> u64 {
    let Ok(it2) = item.cast::<IShellItem2>() else { return 0 };
    // SAFETY: valid item2.
    let ft: FILETIME = unsafe { it2.GetFileTime(&PKEY_DATE_DELETED) }.unwrap_or_default();
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

fn undelete(item: &IShellItem) -> Result<(), String> {
    // SAFETY: context menu of a bin item; the menu is only queried to initialise verbs.
    unsafe {
        let cm: IContextMenu = item.BindToHandler(None, &BHID_SFUIObject).map_err(|e| e.to_string())?;
        let menu = MenuGuard(CreatePopupMenu().map_err(|e| e.to_string())?);
        let _ = cm.QueryContextMenu(menu.0, 0, 1, 0x7FFF, CMF_NORMAL);
        let info = CMINVOKECOMMANDINFO {
            cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
            fMask: SEE_MASK_FLAG_NO_UI,
            lpVerb: PCSTR(c"undelete".as_ptr() as *const u8),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        cm.InvokeCommand(&info).map_err(|e| e.to_string())
    }
}

/// Restore the most recently deleted bin entry for each path; returns how many were restored.
pub(super) fn restore_recycled(paths: &[String]) -> Result<usize, String> {
    // SAFETY: enumerating the Recycle Bin shell folder.
    let items: Vec<IShellItem> = unsafe {
        let bin: IShellItem =
            SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, None).map_err(|e| e.to_string())?;
        let en: IEnumShellItems = bin.BindToHandler(None, &BHID_EnumItems).map_err(|e| e.to_string())?;
        let mut v = Vec::new();
        loop {
            let mut one = [None];
            let mut got = 0u32;
            if en.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            v.extend(one[0].take());
        }
        v
    };
    let originals: Vec<(String, &IShellItem)> =
        items.iter().filter_map(|i| original_path(i).map(|p| (norm(&p), i))).collect();
    let mut restored = 0;
    for p in paths {
        let want = norm(p);
        let best = originals.iter().filter(|(o, _)| *o == want).max_by_key(|(_, i)| deleted_at(i));
        if let Some((_, item)) = best {
            if undelete(item).is_ok() {
                restored += 1;
            }
        }
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::ComGuard;

    #[test]
    fn rejects_bad_names() {
        for bad in ["", ".", "..", "a/b", "a\\b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "x.", "x ", "a\tb"] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        for ok in ["a.txt", "my file (1).md", ".gitignore", "é.txt"] {
            assert!(validate_name(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn rename_in_temp() {
        let _c = ComGuard::init();
        let dir = std::env::temp_dir();
        let a = dir.join(format!("winseek_ren_a_{}.txt", std::process::id()));
        let b = format!("winseek_ren_b_{}.txt", std::process::id());
        std::fs::write(&a, b"x").unwrap();
        let r = rename(HWND::default(), &a.to_string_lossy(), &b);
        let newp = dir.join(&b);
        let ok = r.as_deref() == Ok(newp.to_string_lossy().as_ref()) && newp.exists() && !a.exists();
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&newp);
        assert!(ok, "{r:?}");
        assert!(rename(HWND::default(), "C:\\x\\y.txt", "a/b").is_err());
    }

    #[test]
    fn recycle_restore_round_trip() {
        let _c = ComGuard::init();
        let p = std::env::temp_dir().join(format!("winseek_recycle_test_{}.txt", std::process::id()));
        std::fs::write(&p, b"recycle me").unwrap();
        let ps = p.to_string_lossy().into_owned();
        let r = recycle(HWND::default(), std::slice::from_ref(&ps));
        let gone = !p.exists();
        let n = if gone { restore_recycled(std::slice::from_ref(&ps)) } else { Ok(0) };
        let back = p.exists();
        let _ = std::fs::remove_file(&p);
        assert!(r.is_ok(), "{r:?}");
        assert!(gone, "file still present after recycle");
        assert_eq!(n, Ok(1));
        assert!(back, "file not restored");
    }
}
