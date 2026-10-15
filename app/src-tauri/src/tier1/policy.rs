//! Good-citizen pause policy (PLAN section 7): Tier 1 indexing pauses on battery and while a
//! full-screen game / presentation is running, and resumes by itself. The user's own pause wins.
//!
//! `QUNS_BUSY` alone is not a game: Windows reports it for any borderless full-screen window, so a
//! video in mpv or a browser in F11 paused indexing for hours. Borderless full-screen pauses only
//! when the foreground process works the GPU like a game (3D engine busy, no video decoding).

use std::sync::Arc;
use std::time::Duration;

use windows::core::w;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE, QUNS_BUSY, QUNS_PRESENTATION_MODE,
    QUNS_RUNNING_D3D_FULL_SCREEN,
};

use super::Tier1;

pub const POLL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    User,
    Battery,
    Fullscreen,
}

impl PauseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            PauseReason::User => "user",
            PauseReason::Battery => "battery",
            PauseReason::Fullscreen => "fullscreen",
        }
    }

    /// Storage code for the automatic reasons (0 = none). `User` is kept in its own flag.
    pub fn code(self) -> u8 {
        match self {
            PauseReason::User => 0,
            PauseReason::Battery => 1,
            PauseReason::Fullscreen => 2,
        }
    }

    pub fn from_code(c: u8) -> Option<PauseReason> {
        match c {
            1 => Some(PauseReason::Battery),
            2 => Some(PauseReason::Fullscreen),
            _ => None,
        }
    }
}

/// A borderless full-screen window counts as a game from this much 3D engine use (percent).
/// Measured: mpv playing a video full screen with its GPU shaders ~39%; games sit near 100%.
pub const GAME_3D_MIN: f64 = 50.0;

/// GPU use of one process over the last poll, percent per engine type (summed over its engines).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GpuUse {
    pub three_d: f64,
    pub video_decode: f64,
}

/// What the machine looks like right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// `SYSTEM_POWER_STATUS.ACLineStatus`: 0 offline, 1 online, 255 unknown.
    pub ac_line_status: u8,
    /// `SYSTEM_POWER_STATUS.BatteryFlag`: bit 7 (128) = no system battery.
    pub battery_flag: u8,
    /// `SHQueryUserNotificationState` value, or `None` when the call failed.
    pub notification: Option<i32>,
    /// GPU use of the foreground process; only measured while `notification` is `QUNS_BUSY`.
    pub fg_gpu: Option<GpuUse>,
}

pub fn on_battery(s: &Sample) -> bool {
    s.ac_line_status == 0 && s.battery_flag & 128 == 0
}

/// Does this process use the GPU like a game (not like a video player)?
pub fn game_like(g: &GpuUse) -> bool {
    g.three_d >= GAME_3D_MIN && g.video_decode < 1.0
}

/// Exclusive full-screen D3D and presentation mode always pause; a borderless full-screen window
/// (`QUNS_BUSY`) only when the foreground process looks like a game. Unknown GPU use: keep indexing.
pub fn fullscreen_busy(s: &Sample) -> bool {
    match s.notification {
        Some(n) if n == QUNS_RUNNING_D3D_FULL_SCREEN.0 || n == QUNS_PRESENTATION_MODE.0 => true,
        Some(n) if n == QUNS_BUSY.0 => s.fg_gpu.as_ref().is_some_and(game_like),
        _ => false,
    }
}

/// The automatic pause reason for a sample: battery first, then full-screen.
pub fn auto_reason(s: &Sample) -> Option<PauseReason> {
    if on_battery(s) {
        Some(PauseReason::Battery)
    } else if fullscreen_busy(s) {
        Some(PauseReason::Fullscreen)
    } else {
        None
    }
}

/// Final reason reported to the UI: the user's pause wins over an automatic one.
pub fn combine(user_paused: bool, auto: Option<PauseReason>) -> Option<PauseReason> {
    if user_paused {
        Some(PauseReason::User)
    } else {
        auto
    }
}

/// `(pid, engine type)` of a `GPU Engine` counter instance, e.g.
/// `pid_16084_luid_0x00000000_0x000139FB_phys_0_eng_0_engtype_3D` -> `(16084, "3D")`.
pub fn parse_engine_instance(name: &str) -> Option<(u32, &str)> {
    let rest = name.strip_prefix("pid_")?;
    let pid = rest.split('_').next()?.parse().ok()?;
    let ty = &name[name.find("engtype_")? + "engtype_".len()..];
    Some((pid, ty))
}

/// `GPU Engine(*)\Utilization Percentage` (PDH). A rate counter: each collect yields the use since
/// the previous one, so the first reading of a busy spell only primes it.
pub struct GpuCounters {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    primed: bool,
}

impl GpuCounters {
    pub fn open() -> Option<GpuCounters> {
        let mut query = PDH_HQUERY::default();
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: out-pointers to locals; the query is closed in Drop (or right here on failure).
        unsafe {
            if PdhOpenQueryW(None, 0, &mut query) != 0 {
                return None;
            }
            if PdhAddEnglishCounterW(query, w!(r"\GPU Engine(*)\Utilization Percentage"), 0, &mut counter) != 0 {
                PdhCloseQuery(query);
                return None;
            }
        }
        Some(GpuCounters { query, counter, primed: false })
    }

    /// Use of `pid` since the last call; `None` on the first call of a spell or when PDH fails.
    pub fn read(&mut self, pid: u32) -> Option<GpuUse> {
        // SAFETY: valid query handle owned by self.
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            self.primed = false;
            return None;
        }
        if !std::mem::replace(&mut self.primed, true) {
            return None;
        }
        let (mut bytes, mut count) = (0u32, 0u32);
        // SAFETY: size query without a buffer, then a buffer of at least `bytes` bytes, 8-aligned;
        // the item names point into that buffer and are read before it is dropped.
        unsafe {
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, None) != PDH_MORE_DATA {
                return None;
            }
            let mut buf = vec![0u64; (bytes as usize).div_ceil(8)];
            let items = buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, Some(items)) != 0 {
                return None;
            }
            let mut out = GpuUse::default();
            for item in std::slice::from_raw_parts(items, count as usize) {
                let Ok(name) = item.szName.to_string() else { continue };
                let Some((p, ty)) = parse_engine_instance(&name) else { continue };
                if p != pid || item.FmtValue.CStatus != 0 {
                    continue;
                }
                let v = item.FmtValue.Anonymous.doubleValue;
                if ty.eq_ignore_ascii_case("3D") {
                    out.three_d += v;
                } else if ty.eq_ignore_ascii_case("VideoDecode") {
                    out.video_decode += v;
                }
            }
            Some(out)
        }
    }
}

impl Drop for GpuCounters {
    fn drop(&mut self) {
        // SAFETY: handle from PdhOpenQueryW, closed once.
        unsafe { PdhCloseQuery(self.query) };
    }
}

fn foreground_pid() -> Option<u32> {
    // SAFETY: plain Win32 queries with an out-pointer to a local.
    unsafe {
        let h = GetForegroundWindow();
        if h.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        (pid != 0).then_some(pid)
    }
}

/// Sample the machine. `gpu` keeps the PDH query between polls (opened on first need).
pub fn sample(gpu: &mut Option<GpuCounters>) -> Sample {
    let mut p = SYSTEM_POWER_STATUS::default();
    // On failure keep "unknown" AC so we never pause on a broken probe.
    let (ac, flag) = match unsafe { GetSystemPowerStatus(&mut p) } {
        Ok(()) => (p.ACLineStatus, p.BatteryFlag),
        Err(_) => (255, 0),
    };
    let notification = unsafe { SHQueryUserNotificationState() }.ok().map(|s: QUERY_USER_NOTIFICATION_STATE| s.0);
    let fg_gpu = if notification == Some(QUNS_BUSY.0) {
        if gpu.is_none() {
            *gpu = GpuCounters::open();
        }
        gpu.as_mut().zip(foreground_pid()).and_then(|(g, pid)| g.read(pid))
    } else {
        if let Some(g) = gpu.as_mut() {
            g.primed = false; // the next busy spell measures a fresh interval
        }
        None
    };
    Sample { ac_line_status: ac, battery_flag: flag, notification, fg_gpu }
}

/// Poll every [`POLL`] and apply the automatic reason to the worker. Runs forever.
pub fn spawn(t: Arc<Tier1>) {
    let _ = std::thread::Builder::new().name("tier1-policy".into()).spawn(move || {
        let mut gpu = None;
        loop {
            t.set_auto_pause(auto_reason(&sample(&mut gpu)));
            std::thread::sleep(POLL);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Shell::{QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_NOT_PRESENT, QUNS_QUIET_TIME};

    fn s(ac: u8, flag: u8, n: Option<i32>) -> Sample {
        Sample { ac_line_status: ac, battery_flag: flag, notification: n, fg_gpu: None }
    }

    fn busy_with(three_d: f64, video_decode: f64) -> Sample {
        Sample { fg_gpu: Some(GpuUse { three_d, video_decode }), ..s(1, 0, Some(QUNS_BUSY.0)) }
    }

    #[test]
    fn battery_only_when_offline_with_a_battery() {
        assert!(on_battery(&s(0, 1, None)));
        assert!(!on_battery(&s(1, 1, None)));
        assert!(!on_battery(&s(255, 0, None))); // unknown: never pause
        assert!(!on_battery(&s(0, 128, None))); // no system battery
    }

    #[test]
    fn fullscreen_states() {
        for st in [QUNS_RUNNING_D3D_FULL_SCREEN, QUNS_PRESENTATION_MODE] {
            assert!(fullscreen_busy(&s(1, 0, Some(st.0))), "{}", st.0);
        }
        // borderless full screen with unknown GPU use: keep indexing
        for st in [QUNS_BUSY, QUNS_ACCEPTS_NOTIFICATIONS, QUNS_NOT_PRESENT, QUNS_QUIET_TIME, QUNS_APP] {
            assert!(!fullscreen_busy(&s(1, 0, Some(st.0))), "{}", st.0);
        }
        assert!(!fullscreen_busy(&s(1, 0, None)));
    }

    #[test]
    fn borderless_fullscreen_pauses_only_for_game_like_gpu_use() {
        assert!(!fullscreen_busy(&busy_with(39.0, 12.0)), "mpv playing a video full screen");
        assert!(!fullscreen_busy(&busy_with(80.0, 5.0)), "heavy shaders, still a video");
        assert!(!fullscreen_busy(&busy_with(3.0, 0.0)), "browser in F11");
        assert!(fullscreen_busy(&busy_with(97.0, 0.0)), "borderless game");
        assert!(fullscreen_busy(&busy_with(GAME_3D_MIN, 0.0)));
    }

    #[test]
    fn engine_instance_names() {
        assert_eq!(parse_engine_instance("pid_16084_luid_0x00000000_0x000139FB_phys_0_eng_0_engtype_3D"), Some((16084, "3D")));
        assert_eq!(parse_engine_instance("pid_7_luid_0x0_0x1_phys_0_eng_3_engtype_VideoDecode"), Some((7, "VideoDecode")));
        assert_eq!(parse_engine_instance("_Total"), None);
        assert_eq!(parse_engine_instance("pid_x_engtype_3D"), None);
    }

    /// `cargo test -p winseek-app gpu_counters_live -- --ignored --nocapture` with a video playing.
    #[test]
    #[ignore]
    fn gpu_counters_live() {
        let mut g = GpuCounters::open().expect("PDH GPU Engine counters");
        let pid = foreground_pid().unwrap();
        assert!(g.read(pid).is_none(), "the first read only primes");
        std::thread::sleep(Duration::from_secs(2));
        eprintln!("foreground pid {pid}: {:?}", g.read(pid));
    }

    #[test]
    fn auto_reason_priority_and_resume() {
        let busy = Some(QUNS_RUNNING_D3D_FULL_SCREEN.0);
        assert_eq!(auto_reason(&s(0, 1, busy)), Some(PauseReason::Battery));
        assert_eq!(auto_reason(&s(1, 1, busy)), Some(PauseReason::Fullscreen));
        assert_eq!(auto_reason(&s(1, 1, Some(QUNS_ACCEPTS_NOTIFICATIONS.0))), None);
    }

    #[test]
    fn user_pause_wins() {
        assert_eq!(combine(true, Some(PauseReason::Battery)), Some(PauseReason::User));
        assert_eq!(combine(true, None), Some(PauseReason::User));
        assert_eq!(combine(false, Some(PauseReason::Fullscreen)), Some(PauseReason::Fullscreen));
        assert_eq!(combine(false, None), None);
    }

    #[test]
    fn codes_round_trip() {
        for r in [PauseReason::Battery, PauseReason::Fullscreen] {
            assert_eq!(PauseReason::from_code(r.code()), Some(r));
        }
        assert_eq!(PauseReason::from_code(0), None);
        assert_eq!(PauseReason::User.as_str(), "user");
    }
}
