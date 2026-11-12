//! Date / size range parsing for the agent's search filters (pure; the adapter injects the
//! clock and the timezone offset so tests are deterministic). Weeks start Monday.

/// `[start_ms, end_ms)` in unix milliseconds for an ISO date (`YYYY-MM-DD`) or one of
/// `today|yesterday|this_week|last_week|this_month|this_year`, resolved in local time.
pub fn date_range(spec: &str, now_ms: i64, tz_offset_min: i32) -> Result<(i64, i64), String> {
    const DAY: i64 = 86_400_000;
    let tz = i64::from(tz_offset_min) * 60_000;
    let local = now_ms + tz;
    let today = local.div_euclid(DAY); // local day number (1970-01-01 = 0, a Thursday)
    let day_start = |d: i64| d * DAY - tz;
    let monday = today - (today + 3).rem_euclid(7); // day 0 was Thursday -> +3
    match spec.trim().to_lowercase().as_str() {
        "today" => Ok((day_start(today), day_start(today + 1))),
        "yesterday" => Ok((day_start(today - 1), day_start(today))),
        "this_week" => Ok((day_start(monday), day_start(today + 1))),
        "last_week" => Ok((day_start(monday - 7), day_start(monday))),
        "this_month" => {
            let (y, m, _) = civil_from_days(today);
            Ok((day_start(days_from_civil(y, m, 1)), day_start(days_from_civil(y, m + 1, 1))))
        }
        "this_year" => {
            let (y, _, _) = civil_from_days(today);
            Ok((day_start(days_from_civil(y, 1, 1)), day_start(days_from_civil(y + 1, 1, 1))))
        }
        other => {
            let (y, m, d) = parse_ymd(other)?;
            let from = days_from_civil(y, m, d);
            // The civil calendar is continuous: the next day is just from + 1.
            Ok((day_start(from), day_start(from + 1)))
        }
    }
}

fn parse_ymd(s: &str) -> Result<(i64, u32, u32), String> {
    let bad = || format!("bad date \"{s}\" (want YYYY-MM-DD or a word like this_week)");
    let mut it = s.split('-');
    let y: i64 = it.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let m: u32 = it.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let d: u32 = it.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    Ok((y, m, d))
}

/// Howard Hinnant's civil calendar algorithms (public domain), the smallest correct way to do
/// month / year boundaries without pulling chrono into the crate.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Bytes, or a string like `1024`, `"10MB"`, `"1.5 GB"` (1024-based; B/KB/MB/GB/TB).
pub fn parse_size(v: &serde_json::Value) -> Result<u64, String> {
    match v {
        serde_json::Value::Number(x) => x
            .as_u64()
            .ok_or_else(|| format!("size {x} is not a whole number of bytes")),
        serde_json::Value::String(s) => {
            let t = s.trim();
            let bad = || format!("bad size \"{s}\" (want bytes or \"10MB\")");
            let split = t.char_indices().find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == ','));
            let (num, unit) = match split {
                None => return t.replace(',', "").parse::<u64>().map_err(|_| bad()),
                Some((i, _)) => t.split_at(i),
            };
            let num: f64 = num.replace(',', "").parse().map_err(|_| bad())?;
            let mult: f64 = match unit.trim().trim_end_matches(['B', 'b']).trim() {
                "" | "K" | "k" => 1024.0,
                "M" | "m" => 1024.0 * 1024.0,
                "G" | "g" => 1024.0 * 1024.0 * 1024.0,
                "T" | "t" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
                other => return Err(format!("bad size unit \"{other}\" (want KB/MB/GB/TB or bytes)")),
            };
            Ok((num * mult) as u64)
        }
        other => Err(format!("bad size {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-20 is a Tuesday. 12:00 local, tz +60 (CET).
    const NOW: i64 = 1_792_497_600_000; // 2026-10-20T12:00:00Z
    const TZ: i32 = 60;

    fn day_ms(y: i64, m: u32, d: u32) -> i64 {
        days_from_civil(y, m, d) * 86_400_000
    }

    #[test]
    fn iso_date_range_is_the_local_day() {
        let (from, to) = date_range("2026-10-20", NOW, TZ).unwrap();
        assert_eq!((from, to), (day_ms(2026, 10, 20) - 3_600_000, day_ms(2026, 10, 21) - 3_600_000));
        assert!(date_range("2026-13-01", NOW, TZ).is_err());
        assert!(date_range("gibberish", NOW, TZ).is_err());
    }

    #[test]
    fn relative_words() {
        // Tuesday: this_week starts Monday 2026-10-19, last_week 2026-10-12.
        let (f, t) = date_range("today", NOW, TZ).unwrap();
        assert_eq!((f, t), (day_ms(2026, 10, 20) - 3_600_000, day_ms(2026, 10, 21) - 3_600_000));
        let (f, _) = date_range("yesterday", NOW, TZ).unwrap();
        assert_eq!(f, day_ms(2026, 10, 19) - 3_600_000);
        let (f, t) = date_range("this_week", NOW, TZ).unwrap();
        assert_eq!((f, t), (day_ms(2026, 10, 19) - 3_600_000, day_ms(2026, 10, 21) - 3_600_000));
        let (f, t) = date_range("last_week", NOW, TZ).unwrap();
        assert_eq!((f, t), (day_ms(2026, 10, 12) - 3_600_000, day_ms(2026, 10, 19) - 3_600_000));
        let (f, _) = date_range("this_month", NOW, TZ).unwrap();
        assert_eq!(f, day_ms(2026, 10, 1) - 3_600_000);
        let (f, _) = date_range("this_year", NOW, TZ).unwrap();
        assert_eq!(f, day_ms(2026, 1, 1) - 3_600_000);
        // An ISO date is always one local day, even across month ends (Feb 2026 has 28 days).
        let (f, t) = date_range("2026-02-28", NOW, TZ).unwrap();
        assert_eq!(t - f, 86_400_000);
        let (f, t) = date_range("this_month", NOW, TZ).unwrap();
        assert_eq!(t - f, 86_400_000 * 31);
    }

    #[test]
    fn negative_tz_moves_the_day_boundary() {
        // 2026-10-20T12:00Z at UTC-300 is still 2026-10-20 07:00 local; "today" starts there.
        let (f, t) = date_range("today", 1_792_473_600_000, -300).unwrap();
        assert_eq!((f, t), (day_ms(2026, 10, 20) + 5 * 3_600_000, day_ms(2026, 10, 21) + 5 * 3_600_000));
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size(&serde_json::json!(1024)).unwrap(), 1024);
        assert_eq!(parse_size(&serde_json::json!("10MB")).unwrap(), 10 * 1024 * 1024);
        assert_eq!(parse_size(&serde_json::json!("1.5 GB")).unwrap(), (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_size(&serde_json::json!("512")).unwrap(), 512);
        assert!(parse_size(&serde_json::json!("10 XB")).is_err());
        assert!(parse_size(&serde_json::json!(true)).is_err());
    }
}
