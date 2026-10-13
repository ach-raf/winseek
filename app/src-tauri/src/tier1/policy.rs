//! Good-citizen pause policy (PLAN section 7): Tier 1 indexing pauses on battery and while a
//! full-screen game / presentation is running, and resumes by itself. The user's own pause wins.

use std::sync::Arc;
use std::time::Duration;

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
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

/// What the machine looks like right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// `SYSTEM_POWER_STATUS.ACLineStatus`: 0 offline, 1 online, 255 unknown.
    pub ac_line_status: u8,
    /// `SYSTEM_POWER_STATUS.BatteryFlag`: bit 7 (128) = no system battery.
    pub battery_flag: u8,
    /// `SHQueryUserNotificationState` value, or `None` when the call failed.
    pub notification: Option<i32>,
}

pub fn on_battery(s: &Sample) -> bool {
    s.ac_line_status == 0 && s.battery_flag & 128 == 0
}

pub fn fullscreen_busy(s: &Sample) -> bool {
    matches!(s.notification, Some(n) if n == QUNS_BUSY.0 || n == QUNS_RUNNING_D3D_FULL_SCREEN.0 || n == QUNS_PRESENTATION_MODE.0)
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

pub fn sample() -> Sample {
    let mut p = SYSTEM_POWER_STATUS::default();
    // On failure keep "unknown" AC so we never pause on a broken probe.
    let (ac, flag) = match unsafe { GetSystemPowerStatus(&mut p) } {
        Ok(()) => (p.ACLineStatus, p.BatteryFlag),
        Err(_) => (255, 0),
    };
    let notification = unsafe { SHQueryUserNotificationState() }.ok().map(|s: QUERY_USER_NOTIFICATION_STATE| s.0);
    Sample { ac_line_status: ac, battery_flag: flag, notification }
}

/// Poll every [`POLL`] and apply the automatic reason to the worker. Runs forever.
pub fn spawn(t: Arc<Tier1>) {
    let _ = std::thread::Builder::new().name("tier1-policy".into()).spawn(move || loop {
        t.set_auto_pause(auto_reason(&sample()));
        std::thread::sleep(POLL);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Shell::{QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_NOT_PRESENT, QUNS_QUIET_TIME};

    fn s(ac: u8, flag: u8, n: Option<i32>) -> Sample {
        Sample { ac_line_status: ac, battery_flag: flag, notification: n }
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
        for st in [QUNS_BUSY, QUNS_RUNNING_D3D_FULL_SCREEN, QUNS_PRESENTATION_MODE] {
            assert!(fullscreen_busy(&s(1, 0, Some(st.0))), "{}", st.0);
        }
        for st in [QUNS_ACCEPTS_NOTIFICATIONS, QUNS_NOT_PRESENT, QUNS_QUIET_TIME, QUNS_APP] {
            assert!(!fullscreen_busy(&s(1, 0, Some(st.0))), "{}", st.0);
        }
        assert!(!fullscreen_busy(&s(1, 0, None)));
    }

    #[test]
    fn auto_reason_priority_and_resume() {
        let busy = Some(QUNS_BUSY.0);
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
