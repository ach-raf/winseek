//! Windows service mode and `--install` / `--uninstall`. Zero-dependency raw Win32 FFI.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::log::{error, info};
use crate::{Args, Mode, STOP};

pub const SERVICE_NAME: &str = "winseek-searchd";
const DISPLAY_NAME: &str = "winseek name index";
const DESCRIPTION: &str = "Indexes NTFS file names (MFT + USN journal) for winseek instant search.";
/// SYSTEM + Administrators only; protected so nothing is inherited (snapshot = every filename).
pub const DATA_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
/// Interactive users may query and start the service, not stop or configure it.
pub const SERVICE_SDDL: &str =
    "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPLOCRRC;;;IU)";

const SERVICE_WIN32_OWN_PROCESS: u32 = 0x10;
const SERVICE_AUTO_START: u32 = 2;
const SERVICE_ERROR_NORMAL: u32 = 1;
const SERVICE_ALL_ACCESS: u32 = 0xF01FF;
const SC_MANAGER_ALL_ACCESS: u32 = 0xF003F;
const SERVICE_STOPPED: u32 = 1;
const SERVICE_START_PENDING: u32 = 2;
const SERVICE_STOP_PENDING: u32 = 3;
const SERVICE_RUNNING: u32 = 4;
const SERVICE_ACCEPT_STOP: u32 = 1;
const SERVICE_ACCEPT_PRESHUTDOWN: u32 = 0x100;
const SERVICE_CONTROL_STOP: u32 = 1;
const SERVICE_CONTROL_INTERROGATE: u32 = 4;
const SERVICE_CONTROL_SHUTDOWN: u32 = 5;
const SERVICE_CONTROL_PRESHUTDOWN: u32 = 0xF;
const SERVICE_CONFIG_DESCRIPTION: u32 = 1;
const SERVICE_CONFIG_FAILURE_ACTIONS: u32 = 2;
const SERVICE_CONFIG_FAILURE_ACTIONS_FLAG: u32 = 4;
const SC_ACTION_RESTART: u32 = 1;
const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
const DACL_SECURITY_INFORMATION: u32 = 4;
const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
const SE_FILE_OBJECT: u32 = 1;
const SDDL_REVISION_1: u32 = 1;
const TOKEN_QUERY: u32 = 8;
const TOKEN_ELEVATION: u32 = 20;
const WAIT_HINT_MS: u32 = 30_000;

type Sc = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct ServiceStatus {
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit_code: u32,
    service_specific_exit_code: u32,
    check_point: u32,
    wait_hint: u32,
}

#[repr(C)]
struct ServiceTableEntry {
    name: *const u16,
    proc_: Option<extern "system" fn(u32, *mut *mut u16)>,
}

#[repr(C)]
struct ServiceDescription {
    description: *mut u16,
}

#[repr(C)]
struct ScAction {
    kind: u32,
    delay: u32,
}

#[repr(C)]
struct ServiceFailureActions {
    reset_period: u32,
    reboot_msg: *mut u16,
    command: *mut u16,
    c_actions: u32,
    actions: *mut ScAction,
}

#[repr(C)]
struct FailureActionsFlag {
    flag: i32,
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenSCManagerW(machine: *const u16, db: *const u16, access: u32) -> Sc;
    fn OpenServiceW(scm: Sc, name: *const u16, access: u32) -> Sc;
    fn CreateServiceW(
        scm: Sc,
        name: *const u16,
        display: *const u16,
        access: u32,
        service_type: u32,
        start_type: u32,
        error_control: u32,
        bin_path: *const u16,
        load_order_group: *const u16,
        tag_id: *mut u32,
        dependencies: *const u16,
        start_name: *const u16,
        password: *const u16,
    ) -> Sc;
    fn ChangeServiceConfigW(
        svc: Sc,
        service_type: u32,
        start_type: u32,
        error_control: u32,
        bin_path: *const u16,
        load_order_group: *const u16,
        tag_id: *mut u32,
        dependencies: *const u16,
        start_name: *const u16,
        password: *const u16,
        display: *const u16,
    ) -> i32;
    fn ChangeServiceConfig2W(svc: Sc, info_level: u32, info: *mut c_void) -> i32;
    fn SetServiceObjectSecurity(svc: Sc, info: u32, sd: *mut c_void) -> i32;
    fn StartServiceW(svc: Sc, argc: u32, argv: *const *const u16) -> i32;
    fn ControlService(svc: Sc, control: u32, status: *mut ServiceStatus) -> i32;
    fn QueryServiceStatus(svc: Sc, status: *mut ServiceStatus) -> i32;
    fn DeleteService(svc: Sc) -> i32;
    fn CloseServiceHandle(h: Sc) -> i32;
    fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntry) -> i32;
    fn RegisterServiceCtrlHandlerExW(
        name: *const u16,
        handler: extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32,
        ctx: *mut c_void,
    ) -> isize;
    fn SetServiceStatus(h: isize, status: *const ServiceStatus) -> i32;
    fn SetNamedSecurityInfoW(
        name: *const u16,
        obj_type: u32,
        info: u32,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> u32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl: *const u16,
        revision: u32,
        sd: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn GetSecurityDescriptorDacl(
        sd: *mut c_void,
        present: *mut i32,
        dacl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn OpenProcessToken(process: isize, access: u32, token: *mut isize) -> i32;
    fn GetTokenInformation(token: isize, class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn CloseHandle(h: isize) -> i32;
    fn LocalFree(p: *mut c_void) -> *mut c_void;
}

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

fn os_err(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

pub fn is_elevated() -> bool {
    let mut tok = 0isize;
    // SAFETY: out-pointers valid; token closed below.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return false;
        }
        let (mut elevated, mut n) = (0u32, 0u32);
        let ok = GetTokenInformation(tok, TOKEN_ELEVATION, (&mut elevated as *mut u32).cast(), 4, &mut n);
        CloseHandle(tok);
        ok != 0 && elevated != 0
    }
}

/// `"<exe>" --service [--volumes C,D]` (path always quoted).
pub fn bin_path(exe: &Path, volumes: Option<&[char]>) -> String {
    let mut s = format!("\"{}\" --service", exe.display());
    if let Some(v) = volumes {
        let l: Vec<String> = v.iter().map(char::to_string).collect();
        s.push_str(&format!(" --volumes {}", l.join(",")));
    }
    s
}

fn install_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into())).join("winseek")
}

// ---------------------------------------------------------------- SCM helpers

struct Handle(Sc);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: handle owned by us.
        unsafe { CloseServiceHandle(self.0) };
    }
}

fn open_scm() -> Result<Handle, String> {
    // SAFETY: null machine/db = local active database.
    let h = unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_ALL_ACCESS) };
    if h.is_null() {
        return Err(os_err("OpenSCManager"));
    }
    Ok(Handle(h))
}

fn open_service(scm: &Handle) -> Result<Option<Handle>, String> {
    let n = wide(SERVICE_NAME);
    // SAFETY: valid handle and string.
    let h = unsafe { OpenServiceW(scm.0, n.as_ptr(), SERVICE_ALL_ACCESS) };
    if h.is_null() {
        let e = std::io::Error::last_os_error();
        return if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) {
            Ok(None)
        } else {
            Err(format!("OpenService: {e}"))
        };
    }
    Ok(Some(Handle(h)))
}

fn query_state(svc: &Handle) -> Result<u32, String> {
    let mut st = ServiceStatus::default();
    // SAFETY: valid handle and out-struct.
    if unsafe { QueryServiceStatus(svc.0, &mut st) } == 0 {
        return Err(os_err("QueryServiceStatus"));
    }
    Ok(st.current_state)
}

/// Ask the service to stop (if running) and poll every 250 ms for up to 30 s.
fn stop_and_wait(svc: &Handle) -> Result<(), String> {
    if query_state(svc)? == SERVICE_STOPPED {
        return Ok(());
    }
    info!("stopping service {SERVICE_NAME}");
    let mut st = ServiceStatus::default();
    // SAFETY: valid handle and out-struct.
    if unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut st) } == 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_SERVICE_NOT_ACTIVE) {
            return Err(format!("ControlService(stop): {e}"));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if query_state(svc)? == SERVICE_STOPPED {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("service did not stop within 30 s".into())
}

/// Parse SDDL into a self-relative SD (free with `LocalFree`).
fn sddl_to_sd(sddl: &str) -> Result<*mut c_void, String> {
    let w = wide(sddl);
    let mut sd: *mut c_void = null_mut();
    // SAFETY: out-pointer valid.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut())
    };
    if ok == 0 {
        return Err(os_err("ConvertStringSecurityDescriptor"));
    }
    Ok(sd)
}

fn apply_data_dacl(dir: &Path) -> Result<(), String> {
    let sd = sddl_to_sd(DATA_SDDL)?;
    let (mut present, mut defaulted, mut dacl) = (0i32, 0i32, null_mut());
    // SAFETY: sd valid until LocalFree; dacl points into sd.
    let r = unsafe {
        if GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) == 0 || present == 0 {
            let e = os_err("GetSecurityDescriptorDacl");
            LocalFree(sd);
            return Err(e);
        }
        let w = wide(dir);
        let r = SetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        );
        LocalFree(sd);
        r
    };
    if r != 0 {
        return Err(format!("SetNamedSecurityInfo({}): error {r}", dir.display()));
    }
    Ok(())
}

fn configure(svc: &Handle) -> Result<(), String> {
    let mut desc = wide(DESCRIPTION);
    let mut d = ServiceDescription { description: desc.as_mut_ptr() };
    let mut acts = [
        ScAction { kind: SC_ACTION_RESTART, delay: 5_000 },
        ScAction { kind: SC_ACTION_RESTART, delay: 30_000 },
        ScAction { kind: SC_ACTION_RESTART, delay: 60_000 },
    ];
    let mut fa = ServiceFailureActions {
        reset_period: 86_400,
        reboot_msg: null_mut(),
        command: null_mut(),
        c_actions: 3,
        actions: acts.as_mut_ptr(),
    };
    let mut flag = FailureActionsFlag { flag: 1 };
    // SAFETY: structs outlive the calls.
    unsafe {
        if ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_DESCRIPTION, (&mut d as *mut ServiceDescription).cast()) == 0 {
            return Err(os_err("ChangeServiceConfig2(description)"));
        }
        if ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_FAILURE_ACTIONS, (&mut fa as *mut ServiceFailureActions).cast())
            == 0
        {
            return Err(os_err("ChangeServiceConfig2(failure actions)"));
        }
        // Also restart after a nonzero exit (e.g. startup error), not only crashes.
        if ChangeServiceConfig2W(
            svc.0,
            SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
            (&mut flag as *mut FailureActionsFlag).cast(),
        ) == 0
        {
            return Err(os_err("ChangeServiceConfig2(failure flag)"));
        }
        let sd = sddl_to_sd(SERVICE_SDDL)?;
        let ok = SetServiceObjectSecurity(svc.0, DACL_SECURITY_INFORMATION, sd);
        let e = std::io::Error::last_os_error();
        LocalFree(sd);
        if ok == 0 {
            return Err(format!("SetServiceObjectSecurity: {e}"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- install / uninstall

/// Entry for `--install` / `--uninstall`. Returns the process exit code.
pub fn manage(args: &Args) -> i32 {
    if !is_elevated() {
        eprintln!("searchd: --install/--uninstall must be run elevated (as administrator)");
        return 5;
    }
    let data = crate::default_data_dir();
    crate::log::init(&args.log);
    let r = if args.mode == Mode::Install { install(args.volumes.as_deref(), &data) } else { uninstall() };
    match r {
        Ok(()) => 0,
        Err(e) => {
            error!("{e}");
            1
        }
    }
}

pub fn install(volumes: Option<&[char]>, data: &Path) -> Result<(), String> {
    let src = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let scm = open_scm()?;
    let existing = open_service(&scm)?;
    if let Some(s) = &existing {
        stop_and_wait(s)?;
    }
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let dest = dir.join("searchd.exe");
    let same = matches!((std::fs::canonicalize(&src), std::fs::canonicalize(&dest)), (Ok(a), Ok(b)) if a == b);
    if !same {
        std::fs::copy(&src, &dest).map_err(|e| format!("copy {} -> {}: {e}", src.display(), dest.display()))?;
        info!("copied {} -> {}", src.display(), dest.display());
    }
    std::fs::create_dir_all(data).map_err(|e| format!("create {}: {e}", data.display()))?;
    apply_data_dacl(data)?;

    let bp = wide(bin_path(&dest, volumes));
    let (name, display, sys) = (wide(SERVICE_NAME), wide(DISPLAY_NAME), wide("LocalSystem"));
    let svc = match existing {
        Some(s) => {
            // SAFETY: valid handle and strings; nulls = unchanged.
            let ok = unsafe {
                ChangeServiceConfigW(
                    s.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    bp.as_ptr(),
                    null(),
                    null_mut(),
                    null(),
                    sys.as_ptr(),
                    null(),
                    display.as_ptr(),
                )
            };
            if ok == 0 {
                return Err(os_err("ChangeServiceConfig"));
            }
            info!("updated service {SERVICE_NAME}");
            s
        }
        None => {
            // SAFETY: valid handle and strings; nulls = defaults (LocalSystem).
            let h = unsafe {
                CreateServiceW(
                    scm.0,
                    name.as_ptr(),
                    display.as_ptr(),
                    SERVICE_ALL_ACCESS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    bp.as_ptr(),
                    null(),
                    null_mut(),
                    null(),
                    null(),
                    null(),
                )
            };
            if h.is_null() {
                return Err(os_err("CreateService"));
            }
            info!("created service {SERVICE_NAME}");
            Handle(h)
        }
    };
    configure(&svc)?;
    // SAFETY: valid handle.
    if unsafe { StartServiceW(svc.0, 0, null()) } == 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING) {
            return Err(format!("StartService: {e}"));
        }
    }
    info!("service {SERVICE_NAME} installed and started");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let scm = open_scm()?;
    match open_service(&scm)? {
        None => info!("service {SERVICE_NAME} is not installed"),
        Some(s) => {
            stop_and_wait(&s)?;
            // SAFETY: valid handle.
            if unsafe { DeleteService(s.0) } == 0 {
                return Err(os_err("DeleteService"));
            }
            info!("service {SERVICE_NAME} deleted");
        }
    }
    let dir = install_dir();
    match std::fs::remove_file(dir.join("searchd.exe")) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&dir);
            info!("removed {}", dir.display());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => info!("could not remove {}: {e} (ignored)", dir.join("searchd.exe").display()),
    }
    Ok(())
}

// ---------------------------------------------------------------- service mode

static ARGS: OnceLock<Args> = OnceLock::new();
static STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);
static CHECKPOINT: AtomicU32 = AtomicU32::new(0);
static STOPPING: AtomicBool = AtomicBool::new(false);
static STOP_DONE: AtomicBool = AtomicBool::new(false);
static BUMPER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

fn report(state: u32, accepted: u32, exit: u32, specific: u32, wait_hint: u32) {
    let cp = if matches!(state, SERVICE_START_PENDING | SERVICE_STOP_PENDING) {
        CHECKPOINT.fetch_add(1, Ordering::SeqCst) + 1
    } else {
        0
    };
    let st = ServiceStatus {
        service_type: SERVICE_WIN32_OWN_PROCESS,
        current_state: state,
        controls_accepted: accepted,
        win32_exit_code: exit,
        service_specific_exit_code: specific,
        check_point: cp,
        wait_hint,
    };
    // SAFETY: handle from RegisterServiceCtrlHandlerExW; struct valid.
    unsafe { SetServiceStatus(STATUS_HANDLE.load(Ordering::SeqCst), &st) };
}

/// STOP_PENDING now, then keep bumping the checkpoint every 2 s until the service is done.
fn begin_stop_pending() {
    if STOPPING.swap(true, Ordering::SeqCst) {
        return;
    }
    report(SERVICE_STOP_PENDING, 0, 0, 0, WAIT_HINT_MS);
    let h = std::thread::spawn(|| {
        let mut last = Instant::now();
        while !STOP_DONE.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
            if last.elapsed() >= Duration::from_secs(2) {
                report(SERVICE_STOP_PENDING, 0, 0, 0, WAIT_HINT_MS);
                last = Instant::now();
            }
        }
    });
    *BUMPER.lock().unwrap() = Some(h);
}

extern "system" fn ctrl_handler(ctrl: u32, _event: u32, _data: *mut c_void, _ctx: *mut c_void) -> u32 {
    match ctrl {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_PRESHUTDOWN | SERVICE_CONTROL_SHUTDOWN => {
            info!("service control {ctrl}: stopping");
            STOP.store(true, Ordering::SeqCst);
            begin_stop_pending();
            0
        }
        SERVICE_CONTROL_INTERROGATE => 0,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = wide(SERVICE_NAME);
    // SAFETY: valid string and handler.
    let h = unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), ctrl_handler, null_mut()) };
    if h == 0 {
        error!("{}", os_err("RegisterServiceCtrlHandlerEx"));
        return;
    }
    STATUS_HANDLE.store(h, Ordering::SeqCst);
    report(SERVICE_START_PENDING, 0, 0, 0, WAIT_HINT_MS);
    let args = ARGS.get().expect("args set before dispatch");
    let was_ready = AtomicBool::new(false);
    let code = crate::run(args, &STOP, &|| {
        was_ready.store(true, Ordering::SeqCst);
        report(SERVICE_RUNNING, SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_PRESHUTDOWN, 0, 0, 0);
    });
    if !was_ready.load(Ordering::SeqCst) {
        error!("startup failed (exit {code}); see log");
    }
    STOP_DONE.store(true, Ordering::SeqCst);
    if let Some(b) = BUMPER.lock().unwrap().take() {
        let _ = b.join();
    }
    if code == 0 {
        report(SERVICE_STOPPED, 0, 0, 0, 0);
    } else {
        report(SERVICE_STOPPED, 0, ERROR_SERVICE_SPECIFIC_ERROR, code as u32, 0);
    }
}

/// Hand the thread to the SCM; returns when the service has stopped.
pub fn run_dispatcher(args: Args) -> i32 {
    let _ = ARGS.set(args);
    let name = wide(SERVICE_NAME);
    let table = [
        ServiceTableEntry { name: name.as_ptr(), proc_: Some(service_main) },
        ServiceTableEntry { name: null(), proc_: None },
    ];
    // SAFETY: table is NULL-terminated and outlives the call.
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        error!("{} (--service must be started by the service control manager)", os_err("StartServiceCtrlDispatcher"));
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_path_quotes() {
        let exe = Path::new(r"C:\Program Files\winseek\searchd.exe");
        assert_eq!(bin_path(exe, None), r#""C:\Program Files\winseek\searchd.exe" --service"#);
        assert_eq!(
            bin_path(exe, Some(&['C', 'D'])),
            r#""C:\Program Files\winseek\searchd.exe" --service --volumes C,D"#
        );
    }

    #[test]
    fn sddl_parses() {
        for s in [DATA_SDDL, SERVICE_SDDL] {
            let sd = sddl_to_sd(s).unwrap_or_else(|e| panic!("{s}: {e}"));
            // SAFETY: allocated by the converter.
            unsafe { LocalFree(sd) };
        }
    }

    /// Needs admin and replaces any real install. Run: cargo test -p searchd -- --ignored
    #[test]
    #[ignore]
    fn install_uninstall_round_trip() {
        let data = std::env::temp_dir().join("winseek-svc-test");
        install(None, &data).unwrap();
        let scm = open_scm().unwrap();
        assert!(open_service(&scm).unwrap().is_some());
        uninstall().unwrap();
        assert!(open_service(&scm).unwrap().is_none());
        uninstall().unwrap();
    }
}
