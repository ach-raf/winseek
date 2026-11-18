//! Date / size range parsing for the agent's search filters (pure; the adapter injects the
//! clock and the timezone offset so tests are deterministic). Weeks start Monday.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};

/// `[start_ms, end_ms)` in unix milliseconds for an ISO date (`YYYY-MM-DD`) or one of
/// `today|yesterday|this_week|last_week|this_month|this_year`, resolved in local time.
pub fn date_range(spec: &str, now_ms: i64, tz_offset_min: i32) -> Result<(i64, i64), String> {
    let tz = i64::from(tz_offset_min) * 60_000;
    // The shift trick: read the local wall clock as UTC, so plain UTC civil dates are local dates.
    // (The adapter passes the machine's current offset, not a full zone: DST shifts land in the
    // offset at call time, and a range never spans more than a year.)
    let local = Timestamp::from_millisecond(now_ms + tz).map_err(|e| format!("bad clock: {e}"))?;
    let today = local.to_zoned(TimeZone::UTC).date();
    let midnight = |d: Date| -> Result<i64, String> {
        Ok(d.to_zoned(TimeZone::UTC)
            .map_err(|e| e.to_string())?
            .timestamp()
            .as_millisecond()
            - tz)
    };
    // Monday of this week (0 = Monday .. 6 = Sunday, per jiff's weekday).
    let monday = today
        .checked_sub(Span::new().days(i16::from(today.weekday().to_monday_zero_offset())))
        .map_err(|e| e.to_string())?;
    match spec.trim().to_lowercase().as_str() {
        "today" => Ok((midnight(today)?, midnight(day_after(today)?)?)),
        "yesterday" => Ok((
            midnight(today.yesterday().map_err(|e| e.to_string())?)?,
            midnight(today)?,
        )),
        "this_week" => Ok((midnight(monday)?, midnight(day_after(today)?)?)),
        "last_week" => Ok((
            midnight(
                monday
                    .checked_sub(Span::new().days(7))
                    .map_err(|e| e.to_string())?,
            )?,
            midnight(monday)?,
        )),
        "this_month" => {
            let (y, m) = (today.year(), today.month());
            Ok((
                midnight(month_start(y, m)?)?,
                midnight(next_month_start(y, m)?)?,
            ))
        }
        "this_year" => {
            let y = today.year();
            Ok((
                midnight(month_start(y, 1)?)?,
                midnight(next_year_start(y)?)?,
            ))
        }
        other => {
            let d = parse_ymd(other)?;
            Ok((midnight(d)?, midnight(day_after(d)?)?))
        }
    }
}

fn month_start(y: i16, m: i8) -> Result<Date, String> {
    Date::new(y, m, 1).map_err(|e| format!("date out of range: {e}"))
}

fn next_month_start(y: i16, m: i8) -> Result<Date, String> {
    if m == 12 {
        month_start(y.checked_add(1).ok_or("date out of range")?, 1)
    } else {
        month_start(y, m + 1)
    }
}

fn next_year_start(y: i16) -> Result<Date, String> {
    month_start(y.checked_add(1).ok_or("date out of range")?, 1)
}

fn day_after(d: Date) -> Result<Date, String> {
    d.tomorrow().map_err(|e| format!("date out of range: {e}"))
}

fn parse_ymd(s: &str) -> Result<Date, String> {
    let bad = || format!("bad date \"{s}\" (want YYYY-MM-DD or a word like this_week)");
    let mut it = s.split('-');
    let y: i16 = it.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let m: i8 = it.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let d: i8 = it.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    if it.next().is_some() {
        return Err(bad());
    }
    // jiff rejects month 13, day 32 and Feb 30 here — the old hand-rolled arithmetic
    // silently normalised them.
    Date::new(y, m, d).map_err(|_| bad())
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
            let split = t
                .char_indices()
                .find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == ','));
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
                other => {
                    return Err(format!(
                        "bad size unit \"{other}\" (want KB/MB/GB/TB or bytes)"
                    ))
                }
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

    fn day_ms(y: i16, m: i8, d: i8) -> i64 {
        jiff::civil::date(y, m, d)
            .to_zoned(TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_millisecond()
    }

    #[test]
    fn iso_date_range_is_the_local_day() {
        let (from, to) = date_range("2026-10-20", NOW, TZ).unwrap();
        assert_eq!(
            (from, to),
            (
                day_ms(2026, 10, 20) - 3_600_000,
                day_ms(2026, 10, 21) - 3_600_000
            )
        );
        assert!(date_range("2026-13-01", NOW, TZ).is_err());
        assert!(date_range("gibberish", NOW, TZ).is_err());
    }

    #[test]
    fn relative_words() {
        // Tuesday: this_week starts Monday 2026-10-19, last_week 2026-10-12.
        let (f, t) = date_range("today", NOW, TZ).unwrap();
        assert_eq!(
            (f, t),
            (
                day_ms(2026, 10, 20) - 3_600_000,
                day_ms(2026, 10, 21) - 3_600_000
            )
        );
        let (f, _) = date_range("yesterday", NOW, TZ).unwrap();
        assert_eq!(f, day_ms(2026, 10, 19) - 3_600_000);
        let (f, t) = date_range("this_week", NOW, TZ).unwrap();
        assert_eq!(
            (f, t),
            (
                day_ms(2026, 10, 19) - 3_600_000,
                day_ms(2026, 10, 21) - 3_600_000
            )
        );
        let (f, t) = date_range("last_week", NOW, TZ).unwrap();
        assert_eq!(
            (f, t),
            (
                day_ms(2026, 10, 12) - 3_600_000,
                day_ms(2026, 10, 19) - 3_600_000
            )
        );
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
        assert_eq!(
            (f, t),
            (
                day_ms(2026, 10, 20) + 5 * 3_600_000,
                day_ms(2026, 10, 21) + 5 * 3_600_000
            )
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size(&serde_json::json!(1024)).unwrap(), 1024);
        assert_eq!(
            parse_size(&serde_json::json!("10MB")).unwrap(),
            10 * 1024 * 1024
        );
        assert_eq!(
            parse_size(&serde_json::json!("1.5 GB")).unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );
        assert_eq!(parse_size(&serde_json::json!("512")).unwrap(), 512);
        assert!(parse_size(&serde_json::json!("10 XB")).is_err());
        assert!(parse_size(&serde_json::json!(true)).is_err());
    }
}
