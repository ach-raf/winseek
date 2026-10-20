//! Windows settings + classic Control Panel catalog: Flow Launcher's vendored PowerToys list
//! (`resources/windows-settings.json`, MIT — see THIRD_PARTY_NOTICES.md), embedded as-is and
//! converted into launch items. Pure conversion + one small registry read for the OS build.

use serde::Deserialize;
use windows::core::w;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

use super::item::{Kind, LaunchItem, Target};

const RAW: &str = include_str!("../../resources/windows-settings.json");

/// Curated display names for the pages people actually search (task list: keep ~20).
const TITLES: &[(&str, &str)] = &[
    ("AppsAndFeatures", "Apps & features"),
    ("Bluetooth", "Bluetooth & devices"),
    ("Camera", "Camera privacy"),
    ("Colors", "Colors"),
    ("DateAndTime", "Date & time"),
    ("DefaultApps", "Default apps"),
    ("Display", "Display"),
    ("Keyboard", "Keyboard"),
    ("Microphone", "Microphone privacy"),
    ("Mouse", "Mouse"),
    ("NightLightSettings", "Night light"),
    ("Notifications", "Notifications"),
    ("PersonalizationCategory", "Personalization"),
    ("PowerAndSleep", "Power & sleep"),
    ("PrintersAndScanners", "Printers & scanners"),
    ("Privacy", "Privacy"),
    ("Proxy", "Proxy"),
    ("Region", "Region"),
    ("Sound", "Sound"),
    ("Storage", "Storage"),
    ("Taskbar", "Taskbar"),
    ("Vpn", "VPN"),
    ("WiFi", "Wi-Fi"),
    ("WindowsUpdate", "Windows Update"),
];

/// Extra keywords for entry names the plain data under-covers (task list: Colors alts).
const EXTRA_KEYWORDS: &[(&str, &[&str])] = &[("Colors", &["theme", "dark mode", "light mode"])];

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawEntry {
    name: String,
    area: String,
    #[serde(rename = "Type")]
    kind: String,
    command: String,
    alt_names: Option<Vec<String>>,
    icon_glyph: Option<IconGlyph>,
    introduced_in_build: Option<u32>,
    deprecated_in_build: Option<u32>,
    keywords: Option<Vec<Vec<String>>>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IconGlyph {
    glyph: String,
}

/// The catalog for this machine: build-filtered, deduped, converted. Settings + Control Panel
/// rebuild in milliseconds, so this runs synchronously on every app start.
pub fn catalog(build: u32) -> Vec<LaunchItem> {
    let entries: Vec<RawEntry> = serde_json::from_str(RAW)
        .unwrap_or_else(|e| panic!("vendored windows-settings.json does not parse: {e}"));
    catalog_from(entries, build)
}

fn catalog_from(entries: Vec<RawEntry>, build: u32) -> Vec<LaunchItem> {
    let mut items: Vec<LaunchItem> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in entries {
        if e.introduced_in_build.is_some_and(|b| b > build) || e.deprecated_in_build.is_some_and(|b| b <= build) {
            continue;
        }
        if !seen.insert(e.command.to_lowercase()) {
            continue; // same command listed twice: the first (better-placed) entry wins
        }
        let title = TITLES.iter().find(|(n, _)| *n == e.name).map_or_else(|| humanize(&e.name), |(_, t)| t.to_string());
        let area = humanize(&e.area);
        let subtitle = match e.kind.as_str() {
            "ControlPanel" => format!("Control Panel › {area}"),
            "TaskLink" => format!("Task › {area}"),
            _ => format!("Settings › {area}"),
        };
        let mut keywords: Vec<String> = Vec::new();
        if let Some(alts) = &e.alt_names {
            keywords.extend(alts.iter().filter(|a| !a.is_empty()).cloned());
        }
        if let Some(groups) = &e.keywords {
            keywords.extend(groups.iter().flatten().filter(|k| !k.is_empty()).cloned());
        }
        if let Some((_, extra)) = EXTRA_KEYWORDS.iter().find(|(n, _)| *n == e.name) {
            keywords.extend(extra.iter().map(|k| k.to_string()));
        }
        keywords.push(area.clone());
        keywords.push(e.name.clone());
        keywords.retain(|k| !k.is_empty());

        let uri = e.command.to_lowercase().starts_with("ms-settings:") || e.command.to_lowercase().starts_with("shell:");
        let target = if uri {
            Target::Uri { uri: e.command.clone() }
        } else {
            let (exe, args) = split_command(&e.command);
            Target::Exec { exe, args }
        };
        items.push(LaunchItem {
            id: format!("set:{}", e.command),
            kind: Kind::Setting,
            title,
            subtitle,
            keywords,
            target,
            icon: e.icon_glyph.map(|g| g.glyph),
            admin_ok: false,
        });
    }
    eprintln!("winseek: settings catalog {} pages for build {build}", items.len());
    items
}

/// Windows build number (e.g. 19045) from the registry — `GetVersionEx` lies about it.
pub fn os_build() -> u32 {
    let mut buf = [0u16; 32];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if r.0 != 0 {
        eprintln!("winseek: could not read the OS build (error {}); assuming 19041", r.0);
        return 19041; // last version whose settings pages are all still valid everywhere
    }
    let n = (len as usize / 2).saturating_sub(1).min(buf.len());
    String::from_utf16_lossy(&buf[..n]).trim().parse().unwrap_or(19041)
}

/// "AccessWorkOrSchool" -> "Access work or school". Runs of capitals stay one word ("HVCI");
/// digits stay with their word. The first word keeps its case, the rest are lowercased.
pub(crate) fn humanize(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &ch) in chars.iter().enumerate() {
        let prev = if i > 0 { Some(chars[i - 1]) } else { None };
        let next = chars.get(i + 1).copied();
        let boundary = match prev {
            None => false,
            Some(p) if p.is_lowercase() && ch.is_uppercase() => true,
            Some(p) if p.is_alphabetic() && ch.is_ascii_digit() => true,
            Some(p) if p.is_ascii_digit() && ch.is_alphabetic() => true,
            // End of an acronym run: "WiFiSettings" -> Wi Fi Settings, "HSVCIvalues" -> HSVCI values.
            Some(p) if p.is_uppercase() && ch.is_uppercase() && next.is_some_and(|n| n.is_lowercase()) => true,
            _ => false,
        };
        if boundary && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    if words.len() == 1 {
        return words.remove(0);
    }
    let mut out = words[0].clone();
    for w in &words[1..] {
        out.push(' ');
        out.push_str(&w.to_lowercase());
    }
    out
}

/// "control /name Microsoft.DateAndTime" -> ("control.exe", ["/name", "Microsoft.DateAndTime"]).
/// Expands %windir%/%systemroot%, honors quotes; the string never goes near a shell.
fn split_command(command: &str) -> (String, Vec<String>) {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in command.chars() {
        match ch {
            '"' => quoted = !quoted,
            ' ' | '\t' if !quoted => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let mut parts = tokens.into_iter();
    let Some(first) = parts.next() else {
        eprintln!("winseek: empty settings command, dropping");
        return (String::new(), Vec::new());
    };
    let exe = if first.eq_ignore_ascii_case("control") { "control.exe".to_string() } else { expand_root(&first) };
    (exe, parts.collect())
}

/// Expand the %windir% / %systemroot% macros (case-insensitive, ASCII so byte offsets match).
pub(crate) fn expand_root(token: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let low = token.to_lowercase();
    if low.starts_with("%systemroot%") {
        format!("{root}{}", &token[12..])
    } else if low.starts_with("%windir%") {
        format!("{root}{}", &token[8..])
    } else {
        token.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::matcher;

    #[test]
    fn real_file_parses_and_bluetooth_survives_19045() {
        let items = catalog(19045);
        assert!(items.len() > 400, "expected a big catalog, got {}", items.len());
        let bt = items.iter().find(|i| i.target == Target::Uri { uri: "ms-settings:bluetooth".into() }).expect("bluetooth page");
        assert_eq!(bt.title, "Bluetooth & devices");
        assert_eq!(bt.subtitle, "Settings › Devices");
        assert!(bt.icon.is_some());
    }

    #[test]
    fn build_filter_drops_and_keeps() {
        let mk = |name: &str, introduced: Option<u32>, deprecated: Option<u32>| RawEntry {
            name: name.into(),
            area: "System".into(),
            kind: "AppSettingsApp".into(),
            command: format!("ms-settings:{name}"),
            alt_names: None,
            icon_glyph: None,
            introduced_in_build: introduced,
            deprecated_in_build: deprecated,
            keywords: None,
        };
        let entries = vec![mk("Future", Some(22000), None), mk("Old", None, Some(17763)), mk("Current", None, None)];
        let got = catalog_from(entries.clone(), 19045);
        assert_eq!(got.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["set:ms-settings:Current"]);
        let got = catalog_from(entries, 22621);
        assert_eq!(got.len(), 2); // Future + Current, Old stays dropped
    }

    #[test]
    fn split_command_cases() {
        let (exe, args) = split_command("control /name Microsoft.DateAndTime");
        assert_eq!(exe, "control.exe");
        assert_eq!(args, ["/name", "Microsoft.DateAndTime"]);
        let (exe, args) = split_command("%windir%\\system32\\control.exe -name Microsoft.EaseOfAccessCenter /page pageGlobalization");
        assert!(exe.to_lowercase().ends_with("\\control.exe"), "{exe}");
        assert_eq!(args[0], "-name");
        let (exe, args) = split_command(r#""%windir%\syswow64\odbcad32.exe""#);
        assert!(!exe.contains('"') && exe.to_lowercase().ends_with("odbcad32.exe"), "{exe}");
        assert!(args.is_empty());
    }

    #[test]
    fn humanize_cases() {
        assert_eq!(humanize("AccessWorkOrSchool"), "Access work or school");
        assert_eq!(humanize("NightLight"), "Night light");
        assert_eq!(humanize("HVCI"), "HVCI");
        assert_eq!(humanize("TimeAndLanguage"), "Time and language");
        assert_eq!(humanize("Sound"), "Sound");
    }

    /// The two Top-Hit queries from the acceptance list resolve to the right settings pages.
    #[test]
    fn bluetooth_and_dark_score_to_the_right_page() {
        let items = catalog(19045);
        let best_for = |q: &str| {
            items
                .iter()
                .filter_map(|i| matcher::score_item(q, i).map(|m| (m.score, i)))
                .max_by_key(|(s, _)| *s)
                .map(|(_, i)| i)
        };
        assert_eq!(best_for("bluetooth").map(|i| i.id.as_str()), Some("set:ms-settings:bluetooth"));
        let dark = best_for("dark").expect("a page for 'dark'");
        assert_eq!(dark.id, "set:ms-settings:colors");
    }

    /// The modern Settings page outranks its legacy Control Panel twin for the same query.
    #[test]
    fn modern_settings_page_outranks_control_panel_twin() {
        let items = catalog(19045);
        let scored: Vec<(u32, &str)> = items
            .iter()
            .filter(|i| i.title.starts_with("Bluetooth"))
            .filter_map(|i| matcher::score_item("bluetooth", i).map(|m| (m.score, i.id.as_str())))
            .collect();
        assert!(scored.iter().any(|(_, id)| *id == "set:ms-settings:bluetooth"), "{scored:?}");
        let modern = scored.iter().find(|(_, id)| *id == "set:ms-settings:bluetooth").unwrap().0;
        let legacy = scored.iter().find(|(_, id)| id.contains("BluetoothDevices")).map(|(s, _)| *s);
        assert!(legacy.is_none_or(|l| modern > l), "modern {modern} vs legacy {legacy:?}");
    }

    #[test]
    fn os_build_is_sane() {
        assert!(os_build() >= 7600, "build {}", os_build());
    }
}
