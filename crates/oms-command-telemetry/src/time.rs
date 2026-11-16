use chrono::{Datelike, NaiveDate, Weekday};
use regex::Regex;
use std::sync::LazyLock;

static ISO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
    r"^(?P<date>[0-9]{4}-[0-9]{2}-[0-9]{2}|[0-9]{8}|[0-9]{4}-W[0-9]{2}(?:-[1-7])?|[0-9]{4}W[0-9]{2}[1-7]?)",
    r"(?:(?s:.)(?P<time>[0-9]{2}(?::[0-9]{2}(?::[0-9]{2})?|[0-9]{2}(?:[0-9]{2})?)?(?:[.,][0-9]+)?)",
    r"(?P<zone>[+-][0-9]{2}(?::[0-9]{2}(?::[0-9]{2})?|[0-9]{2}(?:[0-9]{2})?)?(?:[.,][0-9]+)?)?)?\z"
)).unwrap()
});

fn clock(text: &str, offset: bool) -> Option<i64> {
    let (whole, fraction) = text.split_once(['.', ',']).unwrap_or((text, ""));
    let parts: Vec<_> = if whole.contains(':') {
        whole.split(':').collect()
    } else {
        whole
            .as_bytes()
            .chunks(2)
            .map(std::str::from_utf8)
            .collect::<Result<_, _>>()
            .ok()?
    };
    let h: i64 = parts.first()?.parse().ok()?;
    let m: i64 = parts.get(1).unwrap_or(&"0").parse().ok()?;
    let s: i64 = parts.get(2).unwrap_or(&"0").parse().ok()?;
    if !offset && (h > 23 || m > 59 || s > 59) {
        return None;
    }
    let mut micros = 0;
    for d in fraction.bytes().take(6) {
        micros = micros * 10 + i64::from(d - b'0');
    }
    for _ in fraction.len().min(6)..6 {
        micros *= 10;
    }
    Some(((h * 60 + m) * 60 + s) * 1_000_000 + micros)
}

/// Python datetime.fromisoformat's calendar/basic/week dates and optional UTC offset.
pub(crate) fn timestamp(value: &str) -> Option<i64> {
    let normalized = value.strip_suffix('Z').map(|s| format!("{s}+00:00"));
    let c = ISO.captures(normalized.as_deref().unwrap_or(value))?;
    let date = &c["date"];
    let day = if date.contains('W') {
        let compact = date.replace('-', "");
        let weekday = match compact.as_bytes().get(7).copied().unwrap_or(b'1') {
            b'1' => Weekday::Mon,
            b'2' => Weekday::Tue,
            b'3' => Weekday::Wed,
            b'4' => Weekday::Thu,
            b'5' => Weekday::Fri,
            b'6' => Weekday::Sat,
            b'7' => Weekday::Sun,
            _ => return None,
        };
        NaiveDate::from_isoywd_opt(
            compact[..4].parse().ok()?,
            compact[5..7].parse().ok()?,
            weekday,
        )?
    } else {
        let compact = date.replace('-', "");
        NaiveDate::from_ymd_opt(
            compact[..4].parse().ok()?,
            compact[4..6].parse().ok()?,
            compact[6..].parse().ok()?,
        )?
    };
    if !(1..=9999).contains(&day.year()) {
        return None;
    }
    let time = c
        .name("time")
        .map_or(Some(0), |v| clock(v.as_str(), false))?;
    let offset = match c.name("zone").map(|v| v.as_str()) {
        None => 0,
        Some(z) => {
            let offset = clock(&z[1..], true)?;
            if offset >= 86_400_000_000 {
                return None;
            }
            if offset < 1_000_000 {
                0
            } else if z.starts_with('-') {
                -offset
            } else {
                offset
            }
        }
    };
    Some(day.and_hms_opt(0, 0, 0)?.and_utc().timestamp_micros() + time - offset)
}
pub(crate) fn is_day(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes().iter().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}
pub(crate) fn bound(value: Option<&str>, end: bool) -> Result<Option<i64>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if is_day(value) {
        let stamp = timestamp(value)
            .ok_or_else(|| format!("artifact telemetry: not a calendar date: {value}"))?;
        let stamp = stamp + if end { 86_400_000_000 } else { 0 };
        valid_utc(stamp)?;
        Ok(Some(stamp))
    } else {
        let stamp = timestamp(value).ok_or_else(|| {
            format!("artifact telemetry: expected YYYY-MM-DD or an ISO-8601 time: {value}")
        })?;
        valid_utc(stamp)?;
        Ok(Some(stamp))
    }
}
fn valid_utc(stamp: i64) -> Result<chrono::DateTime<chrono::Utc>, String> {
    chrono::DateTime::from_timestamp_micros(stamp)
        .filter(|d| (1..=9999).contains(&d.year()))
        .ok_or_else(|| "artifact telemetry: UTC date is out of range".into())
}
pub(crate) fn day(stamp: i64) -> Result<String, String> {
    let date = valid_utc(stamp)?;
    // Python's libc-backed strftime does not pad %Y on GNU hosts.
    #[cfg(target_os = "macos")]
    let year = format!("{:04}", date.year());
    #[cfg(not(target_os = "macos"))]
    let year = date.year().to_string();
    Ok(format!("{year}-{:02}-{:02}", date.month(), date.day()))
}
