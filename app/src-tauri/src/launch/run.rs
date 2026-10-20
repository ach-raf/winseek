//! Starting launch items (Win32): ShellExecuteExW for apps + settings URIs + files,
//! std::process::Command for parsed `control.exe ...` lines and internal commands.
//! Runs on a worker thread with COM initialised (shell: URIs need it) — see launch_run.

use std::process::Command;

use windows::core::{HRESULT, PCWSTR};
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::item::{LaunchItem, Target};
use crate::shell::{wide, ComGuard};

/// Launch (or open/run) one catalog item. `admin` uses the `runas` verb (Win32 targets only).
pub fn run_item(item: &LaunchItem, admin: bool) -> Result<(), String> {
    let _com = ComGuard::init(); // shell: URIs resolve through the shell, which wants COM
    eprintln!("winseek: launch run {} (admin={admin})", item.id);
    match &item.target {
        Target::Aumid { aumid, path } => {
            if admin {
                // UWP/AUMID launch has no elevation story; run the exe when we know it.
                let exe = path.as_ref().ok_or("This app can't run as administrator")?;
                return shell_execute(exe, "", Some("runas"));
            }
            shell_execute(&format!("shell:AppsFolder\\{aumid}"), "", None)
        }
        Target::Path { path, args } => shell_execute(path, args.as_deref().unwrap_or(""), admin.then_some("runas")),
        Target::Uri { uri } => shell_execute(uri, "", None),
        Target::Exec { exe, args } => Command::new(exe)
            .args(args)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("start {exe}: {e}")),
        Target::Cmd { name } => super::commands::run(&format!("cmd:{name}")),
    }
}

fn shell_execute(file: &str, params: &str, verb: Option<&str>) -> Result<(), String> {
    let file_w = wide(file);
    let params_w = wide(params);
    let verb_w = verb.map(wide);
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb_w.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
        lpFile: PCWSTR(file_w.as_ptr()),
        lpParameters: if params.is_empty() { PCWSTR::null() } else { PCWSTR(params_w.as_ptr()) },
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: all pointed-to strings outlive the call (`*_w` bindings above).
    match unsafe { ShellExecuteExW(&mut sei) } {
        Ok(()) => Ok(()),
        // The user answered No to the UAC prompt: not an error, and nothing launched.
        Err(e) if e.code() == HRESULT(0x8007_04C7u32 as i32) => {
            eprintln!("winseek: launch cancelled by the user (UAC)");
            Ok(())
        }
        Err(e) => Err(format!("start {file}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure check of the pieces around run_item: the AUMID file string the shell expects.
    #[test]
    fn aumid_shell_path() {
        let aumid = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        assert_eq!(format!("shell:AppsFolder\\{aumid}"), "shell:AppsFolder\\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App");
    }

    /// Live check (manual): renders nothing, launches Calculator once.
    #[test]
    #[ignore]
    fn launches_calculator() {
        let item = LaunchItem {
            id: "app:calc".into(),
            kind: super::super::item::Kind::App,
            title: "Calculator".into(),
            subtitle: "test".into(),
            keywords: vec![],
            target: Target::Aumid { aumid: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(), path: None },
            icon: None,
            admin_ok: false,
        };
        run_item(&item, false).expect("calculator launches");
    }
}
