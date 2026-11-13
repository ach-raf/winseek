//! Open a file at a position in an external app: PDF at a page, media at a time, code at a line.
//!
//! The app is the user's default handler for the extension when that handler is one we know how
//! to steer; otherwise the first known app that is installed (VLC / mpv / MPC-HC, VS Code /
//! Notepad++); otherwise the plain default open, reported as "not positioned" so the UI can say so.
//! Arguments are always passed as an argv array (never through a shell), and only absolute
//! paths that do not look like an option are accepted.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where to land in the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// 1-based PDF page.
    Page(u32),
    /// Media position in seconds.
    Seconds(f64),
    /// 1-based line.
    Line(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum App {
    Vlc,
    Mpv,
    MpcHc,
    Acrobat,
    Sumatra,
    /// Edge / Chrome / Brave / Firefox: the PDF viewer honours `#page=N` on a file URL.
    Browser,
    Code,
    NotepadPp,
}

impl App {
    fn label(self) -> &'static str {
        match self {
            App::Vlc => "VLC",
            App::Mpv => "mpv",
            App::MpcHc => "MPC-HC",
            App::Acrobat => "Adobe Acrobat",
            App::Sumatra => "SumatraPDF",
            App::Browser => "your browser",
            App::Code => "VS Code",
            App::NotepadPp => "Notepad++",
        }
    }

    fn handles(self, t: &Target) -> bool {
        match t {
            Target::Page(_) => matches!(self, App::Acrobat | App::Sumatra | App::Browser),
            Target::Seconds(_) => matches!(self, App::Vlc | App::Mpv | App::MpcHc),
            Target::Line(_) => matches!(self, App::Code | App::NotepadPp),
        }
    }

    /// Where an installed copy usually lives, as `(environment variable, relative path)`.
    fn install_spots(self) -> &'static [(&'static str, &'static str)] {
        match self {
            App::Vlc => &[("ProgramFiles", r"VideoLAN\VLC\vlc.exe"), ("ProgramFiles(x86)", r"VideoLAN\VLC\vlc.exe")],
            App::Mpv => &[("ProgramFiles", r"mpv\mpv.exe"), ("USERPROFILE", r"scoop\apps\mpv\current\mpv.exe"), ("LOCALAPPDATA", r"Programs\mpv\mpv.exe")],
            App::MpcHc => &[
                ("ProgramFiles", r"MPC-HC\mpc-hc64.exe"),
                ("ProgramFiles(x86)", r"MPC-HC\mpc-hc.exe"),
                ("ProgramFiles(x86)", r"K-Lite Codec Pack\MPC-HC64\mpc-hc64.exe"),
            ],
            App::Code => &[("LOCALAPPDATA", r"Programs\Microsoft VS Code\Code.exe"), ("ProgramFiles", r"Microsoft VS Code\Code.exe")],
            App::NotepadPp => &[("ProgramFiles", r"Notepad++\notepad++.exe"), ("ProgramFiles(x86)", r"Notepad++\notepad++.exe")],
            App::Acrobat | App::Sumatra | App::Browser => &[],
        }
    }
}

/// Fallback order per target kind when the default handler is not steerable.
fn fallbacks(t: &Target) -> &'static [App] {
    match t {
        Target::Page(_) => &[],
        Target::Seconds(_) => &[App::Vlc, App::Mpv, App::MpcHc],
        Target::Line(_) => &[App::Code, App::NotepadPp],
    }
}

/// Recognise a handler by its exe name.
fn classify(exe: &str) -> Option<App> {
    let stem = Path::new(exe).file_stem()?.to_str()?.to_ascii_lowercase();
    Some(match stem.as_str() {
        "vlc" => App::Vlc,
        "mpv" => App::Mpv,
        "mpc-hc" | "mpc-hc64" => App::MpcHc,
        "acrord32" | "acrobat" => App::Acrobat,
        "sumatrapdf" => App::Sumatra,
        "msedge" | "chrome" | "brave" | "firefox" => App::Browser,
        "code" => App::Code,
        "notepad++" => App::NotepadPp,
        _ => return None,
    })
}

/// Seconds as a short decimal ("83", "83.5").
fn secs(s: f64) -> String {
    let mut t = format!("{:.3}", s.max(0.0));
    while t.ends_with('0') {
        t.pop();
    }
    if t.ends_with('.') {
        t.pop();
    }
    t
}

/// `file:///C:/a%20b.pdf` (percent-encodes everything outside the unreserved set and `/` `:`).
fn file_url(path: &str) -> String {
    let mut out = String::from("file:///");
    for b in path.replace('\\', "/").bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn args(app: App, path: &str, t: &Target) -> Vec<String> {
    match (app, t) {
        (App::Vlc, Target::Seconds(s)) => vec![format!("--start-time={}", secs(*s)), path.into()],
        (App::Mpv, Target::Seconds(s)) => vec![format!("--start={}", secs(*s)), path.into()],
        (App::MpcHc, Target::Seconds(s)) => vec![path.into(), "/start".into(), ((s.max(0.0) * 1000.0).round() as u64).to_string()],
        (App::Acrobat, Target::Page(p)) => vec!["/A".into(), format!("page={p}"), path.into()],
        (App::Sumatra, Target::Page(p)) => vec!["-page".into(), p.to_string(), path.into()],
        (App::Browser, Target::Page(p)) => vec![format!("{}#page={p}", file_url(path))],
        (App::Code, Target::Line(l)) => vec!["--goto".into(), format!("{path}:{l}")],
        (App::NotepadPp, Target::Line(l)) => vec![format!("-n{l}"), path.into()],
        _ => vec![path.into()],
    }
}

/// What to run.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub app: &'static str,
}

/// Choose the app for `target`. `default_exe` is the user's default handler for the file's
/// extension; `env` reads an environment variable; `exists` checks a file.
pub fn plan(path: &str, target: &Target, default_exe: Option<&str>, env: &dyn Fn(&str) -> Option<String>, exists: &dyn Fn(&Path) -> bool) -> Option<Plan> {
    if let Some(exe) = default_exe {
        if let Some(app) = classify(exe).filter(|a| a.handles(target)) {
            return Some(Plan { exe: PathBuf::from(exe), args: args(app, path, target), app: app.label() });
        }
    }
    for &app in fallbacks(target) {
        for (var, rel) in app.install_spots() {
            let Some(root) = env(var).filter(|r| !r.is_empty()) else { continue };
            let exe = Path::new(&root).join(rel);
            if exists(&exe) {
                return Some(Plan { exe, args: args(app, path, target), app: app.label() });
            }
        }
    }
    None
}

/// Reject what must never reach a process argument: relative paths, option-looking paths, control characters.
pub fn check_path(path: &str) -> Result<(), String> {
    if path.starts_with('-') || path.chars().any(|c| c.is_control()) {
        return Err("not a usable path".into());
    }
    if !Path::new(path).is_absolute() {
        return Err(format!("{path}: not an absolute path"));
    }
    Ok(())
}

/// The target a command asked for. Page wins over time over line.
pub fn target_of(page: Option<u32>, seconds: Option<f64>, line: Option<u32>) -> Result<Option<Target>, String> {
    if let Some(p) = page.filter(|p| *p > 0) {
        return Ok(Some(Target::Page(p)));
    }
    if let Some(s) = seconds {
        if !s.is_finite() || s < 0.0 {
            return Err("seconds must be a non-negative number".into());
        }
        return Ok(Some(Target::Seconds(s)));
    }
    Ok(line.filter(|l| *l > 0).map(Target::Line))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Opened {
    /// The app that was steered to the position; `None` when the file was opened the plain way.
    pub app: Option<&'static str>,
    pub positioned: bool,
}

/// The user's default handler exe for a file's extension (`AssocQueryString`).
fn default_handler(path: &str) -> Option<String> {
    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::UI::Shell::{AssocQueryStringW, ASSOCF_NONE, ASSOCSTR_EXECUTABLE};
    let ext = Path::new(path).extension()?.to_str()?;
    let key: Vec<u16> = format!(".{ext}").encode_utf16().chain(Some(0)).collect();
    let mut buf = [0u16; 520];
    let mut len = buf.len() as u32;
    // SAFETY: `key` is NUL-terminated; `buf` and `len` describe the output buffer.
    let hr = unsafe { AssocQueryStringW(ASSOCF_NONE, ASSOCSTR_EXECUTABLE, PCWSTR(key.as_ptr()), w!("open"), Some(PWSTR(buf.as_mut_ptr())), &mut len) };
    if hr.is_err() {
        return None;
    }
    let n = (len as usize).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// Plain default open (what Enter does).
fn plain_open(path: &str) -> Result<(), String> {
    Command::new("explorer.exe").arg(path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

pub fn open_at(path: &str, target: Option<Target>) -> Result<Opened, String> {
    check_path(path)?;
    let Some(target) = target else {
        plain_open(path)?;
        return Ok(Opened { app: None, positioned: false });
    };
    let default = default_handler(path);
    let env = |k: &str| std::env::var(k).ok();
    match plan(path, &target, default.as_deref(), &env, &|p| p.is_file()) {
        Some(p) => {
            Command::new(&p.exe).args(&p.args).spawn().map_err(|e| format!("{}: {e}", p.exe.display()))?;
            Ok(Opened { app: Some(p.app), positioned: true })
        }
        None => {
            plain_open(path)?;
            Ok(Opened { app: None, positioned: false })
        }
    }
}

/// Open a file at a PDF page, a media time or a line, in the user's default app when it can be
/// steered, else in VLC / mpv / MPC-HC / VS Code / Notepad++ when installed, else the plain way.
#[tauri::command(async)]
pub fn open_at_position(path: String, page: Option<u32>, seconds: Option<f64>, line: Option<u32>) -> Result<Opened, String> {
    open_at(&path, target_of(page, seconds, line)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }
    fn nothing(_: &Path) -> bool {
        false
    }

    #[test]
    fn default_player_is_steered_when_known() {
        let p = plan(r"D:\v\a b.mkv", &Target::Seconds(83.5), Some(r"C:\Program Files\VideoLAN\VLC\vlc.exe"), &no_env, &nothing).unwrap();
        assert_eq!(p.app, "VLC");
        assert_eq!(p.args, vec!["--start-time=83.5".to_string(), r"D:\v\a b.mkv".to_string()]);
        let p = plan(r"D:\v\a.mp4", &Target::Seconds(10.0), Some(r"C:\tools\MPV.EXE"), &no_env, &nothing).unwrap();
        assert_eq!((p.app, p.args[0].as_str()), ("mpv", "--start=10"));
        let p = plan(r"D:\v\a.mp4", &Target::Seconds(1.25), Some(r"C:\x\mpc-hc64.exe"), &no_env, &nothing).unwrap();
        assert_eq!(p.args, vec![r"D:\v\a.mp4".to_string(), "/start".into(), "1250".into()]);
    }

    #[test]
    fn unsteerable_default_falls_back_to_an_installed_player() {
        let env = |k: &str| (k == "ProgramFiles").then(|| r"C:\PF".to_string());
        let there = |p: &Path| p == Path::new(r"C:\PF").join(r"VideoLAN\VLC\vlc.exe");
        let p = plan(r"D:\v\a.mp4", &Target::Seconds(5.0), Some(r"C:\Windows\system32\wmplayer.exe"), &env, &there).unwrap();
        assert_eq!(p.app, "VLC");
        assert!(plan(r"D:\v\a.mp4", &Target::Seconds(5.0), None, &env, &nothing).is_none());
    }

    #[test]
    fn code_goes_to_the_line() {
        let p = plan(r"D:\p\m.rs", &Target::Line(42), Some(r"C:\Users\u\AppData\Local\Programs\Microsoft VS Code\Code.exe"), &no_env, &nothing).unwrap();
        assert_eq!(p.args, vec!["--goto".to_string(), r"D:\p\m.rs:42".to_string()]);
        let env = |k: &str| (k == "ProgramFiles").then(|| r"C:\PF".to_string());
        let there = |p: &Path| p.ends_with(r"Notepad++\notepad++.exe");
        let p = plan(r"D:\p\m.rs", &Target::Line(7), Some(r"C:\Windows\notepad.exe"), &env, &there).unwrap();
        assert_eq!((p.app, p.args[0].as_str()), ("Notepad++", "-n7"));
    }

    #[test]
    fn pdf_page_by_viewer() {
        let a = plan(r"D:\d\r.pdf", &Target::Page(3), Some(r"C:\Program Files\Adobe\Acrobat DC\Acrobat\Acrobat.exe"), &no_env, &nothing).unwrap();
        assert_eq!(a.args, vec!["/A".to_string(), "page=3".into(), r"D:\d\r.pdf".into()]);
        let s = plan(r"D:\d\r.pdf", &Target::Page(3), Some(r"C:\x\SumatraPDF.exe"), &no_env, &nothing).unwrap();
        assert_eq!(s.args, vec!["-page".to_string(), "3".into(), r"D:\d\r.pdf".into()]);
        let b = plan(r"D:\d\my r#1.pdf", &Target::Page(9), Some(r"C:\Edge\msedge.exe"), &no_env, &nothing).unwrap();
        assert_eq!(b.args, vec!["file:///D:/d/my%20r%231.pdf#page=9".to_string()]);
        // Unknown PDF app: nothing to steer, the caller opens the plain way.
        assert!(plan(r"D:\d\r.pdf", &Target::Page(3), Some(r"C:\x\foxit.exe"), &no_env, &nothing).is_none());
        // A media player is never chosen for a page.
        assert!(plan(r"D:\d\r.pdf", &Target::Page(3), Some(r"C:\x\vlc.exe"), &no_env, &nothing).is_none());
    }

    #[test]
    fn seconds_format_and_target_choice() {
        assert_eq!(secs(83.0), "83");
        assert_eq!(secs(0.5), "0.5");
        assert_eq!(secs(-3.0), "0");
        assert_eq!(target_of(Some(2), Some(9.0), Some(4)).unwrap(), Some(Target::Page(2)));
        assert_eq!(target_of(None, Some(9.0), Some(4)).unwrap(), Some(Target::Seconds(9.0)));
        assert_eq!(target_of(Some(0), None, Some(4)).unwrap(), Some(Target::Line(4)));
        assert_eq!(target_of(None, None, None).unwrap(), None);
        assert!(target_of(None, Some(f64::NAN), None).is_err());
        assert!(target_of(None, Some(-1.0), None).is_err());
    }

    #[test]
    fn paths_must_be_absolute_and_not_options() {
        assert!(check_path(r"D:\a\b.pdf").is_ok());
        assert!(check_path(r"\\srv\share\b.pdf").is_ok());
        assert!(check_path("b.pdf").is_err());
        assert!(check_path("--new-window").is_err());
        assert!(check_path("D:\\a\nb").is_err());
    }
}
