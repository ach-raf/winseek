//! Launcher result item: one app, settings page or system command (pure serde types only).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    App,
    Setting,
    Command,
}

/// How the item is started. Variant tag is short (`t`) because every hit ships to the UI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Target {
    /// `shell:AppsFolder\<aumid>`; `path` = resolved exe when known (used for admin / reveal).
    Aumid { aumid: String, path: Option<String> },
    /// exe / lnk / appref-ms.
    Path { path: String, args: Option<String> },
    /// `ms-settings:...`, `steam://...`, `shell:RecycleBinFolder`.
    Uri { uri: String },
    /// `control.exe /name ...`, `rundll32 ...` — started via std::process::Command, never a shell.
    Exec { exe: String, args: Vec<String> },
    /// Internal system command id (`lock`, `shutdown`, ...).
    Cmd { name: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LaunchItem {
    /// Stable across rescans: `app:<aumid|lower path>`, `set:<command>`, `cmd:<name>`.
    pub id: String,
    pub kind: Kind,
    /// Display name (localized).
    pub title: String,
    /// "Start menu", "Store app", "Settings › Devices", ...
    pub subtitle: String,
    /// Extra match candidates: exe stem, alt names, area, keywords.
    pub keywords: Vec<String>,
    pub target: Target,
    /// Icon key for wsicon (task 6); setting glyph for settings.
    pub icon: Option<String>,
    /// Run as administrator is meaningful (Win32 only).
    pub admin_ok: bool,
}
