#![forbid(unsafe_code)]

use chrono::{
    DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone as _, Timelike,
    Utc, Weekday,
};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub enum TimeZoneSpec {
    Utc,
    Fixed(FixedOffset, String),
    Tzif(TzifZone),
    SystemLocal,
}

#[derive(Clone, Debug)]
pub struct TzifZone {
    pub name: String,
    transitions: Vec<i64>,
    type_indices: Vec<u8>,
    types: Vec<TtInfo>,
    posix_tz: Option<PosixTz>,
}

#[derive(Clone, Copy, Debug)]
struct TtInfo {
    utoff: i32,
}

#[derive(Clone, Debug)]
struct PosixTz {
    std_offset_west: i32, // seconds west of UTC in POSIX notation (positive = west, negative = east)
    dst: Option<PosixDst>,
}

#[derive(Clone, Debug)]
struct PosixDst {
    dst_offset_west: i32,
    start: PosixRule,
    end: PosixRule,
}

#[derive(Clone, Debug)]
enum PosixRule {
    JulianOneBased(u16, i32), // J1..J365 (Feb 29 never counted), seconds from midnight
    JulianZeroBased(u16, i32), // 0..365 (Feb 29 counted), seconds from midnight
    Mwd(u8, u8, u8, i32),     // month 1..12, week 1..5 (5=last), day 0(Sun)..6(Sat), secs
}

impl TimeZoneSpec {
    pub fn name(&self) -> &str {
        match self {
            TimeZoneSpec::Utc => "UTC",
            TimeZoneSpec::Fixed(_, name) => name.as_str(),
            TimeZoneSpec::Tzif(z) => z.name.as_str(),
            TimeZoneSpec::SystemLocal => "system-local",
        }
    }

    pub fn offset_seconds_at(&self, dt: DateTime<Utc>) -> i32 {
        match self {
            TimeZoneSpec::Utc => 0,
            TimeZoneSpec::Fixed(off, _) => off.local_minus_utc(),
            TimeZoneSpec::Tzif(z) => z.offset_seconds_at(dt.timestamp()),
            TimeZoneSpec::SystemLocal => system_local_zone()
                .map(|z| z.offset_seconds_at(dt.timestamp()))
                .unwrap_or(0),
        }
    }

    pub fn to_local_naive(&self, dt: DateTime<Utc>) -> NaiveDateTime {
        let off = self.offset_seconds_at(dt);
        let fixed = FixedOffset::east_opt(off)
            .unwrap_or_else(|| FixedOffset::east_opt(0).expect("zero offset"));
        dt.with_timezone(&fixed).naive_local()
    }

    pub fn local_date_and_week(&self, dt: DateTime<Utc>) -> (NaiveDate, String, String) {
        let naive = self.to_local_naive(dt);
        let date = naive.date();
        let iso = date.iso_week();
        (
            date,
            format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day()),
            format!("{:04}-W{:02}", iso.year(), iso.week()),
        )
    }

    pub fn next_local_midnight_epoch(&self, now: DateTime<Utc>) -> i64 {
        let local_date = self.to_local_naive(now).date();
        let next_date = local_date.succ_opt().unwrap_or(local_date);
        let target_naive = next_date
            .and_hms_opt(0, 0, 0)
            .unwrap_or_else(|| self.to_local_naive(now));
        match self {
            TimeZoneSpec::Utc => {
                DateTime::<Utc>::from_naive_utc_and_offset(target_naive, Utc).timestamp()
            }
            TimeZoneSpec::Fixed(off, _) => {
                let Some(dt) = off.from_local_datetime(&target_naive).earliest() else {
                    return now.timestamp() + 86400;
                };
                dt.timestamp()
            }
            TimeZoneSpec::SystemLocal => system_local_zone()
                .and_then(|z| z.midnight_epoch(next_date))
                .unwrap_or_else(|| {
                    DateTime::<Utc>::from_naive_utc_and_offset(target_naive, Utc).timestamp()
                }),
            TimeZoneSpec::Tzif(z) => z
                .midnight_epoch(next_date)
                .unwrap_or_else(|| now.timestamp() + 86400),
        }
    }
}

pub fn resolve_timezone(requested: Option<&str>) -> TimeZoneSpec {
    let env_tz = std::env::var("OMS_WORK_JOURNAL_TIMEZONE").ok();
    let candidate = requested
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| env_tz.as_deref().map(str::trim).filter(|s| !s.is_empty()));

    if let Some(name) = candidate {
        if let Some(spec) = resolve_named_timezone(name) {
            return spec;
        }
        return TimeZoneSpec::SystemLocal;
    }

    if let Some(system_name) = detect_system_timezone_name() {
        if let Some(spec) = resolve_named_timezone(&system_name) {
            return spec;
        }
    }

    TimeZoneSpec::SystemLocal
}

fn resolve_named_timezone(name: &str) -> Option<TimeZoneSpec> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    if matches!(trimmed, "UTC" | "Etc/UTC" | "Etc/GMT" | "GMT" | "Z") {
        return Some(TimeZoneSpec::Utc);
    }
    if trimmed.eq_ignore_ascii_case("system-local") {
        return Some(TimeZoneSpec::SystemLocal);
    }
    if !is_safe_zone_name(trimmed) {
        return None;
    }
    for base in [
        "/usr/share/zoneinfo",
        "/usr/lib/zoneinfo",
        "/var/db/timezone/zoneinfo",
        "/etc/zoneinfo",
    ] {
        let path = Path::new(base).join(trimmed);
        if let Ok(bytes) = fs::read(&path) {
            if let Some(zone) = parse_tzif(trimmed, &bytes) {
                return Some(TimeZoneSpec::Tzif(zone));
            }
        }
    }
    if let Some(posix) = parse_posix_tz(trimmed) {
        return Some(TimeZoneSpec::Tzif(TzifZone {
            name: trimmed.to_string(),
            transitions: Vec::new(),
            type_indices: Vec::new(),
            types: vec![TtInfo {
                utoff: -posix.std_offset_west,
            }],
            posix_tz: Some(posix),
        }));
    }
    None
}

fn is_safe_zone_name(name: &str) -> bool {
    if name.starts_with('/') || name.contains('\\') || name.contains('\0') {
        return false;
    }
    for seg in name.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return false;
        }
        if !seg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'.'))
        {
            return false;
        }
    }
    true
}

fn detect_system_timezone_name() -> Option<String> {
    if let Ok(tz_env) = std::env::var("TZ") {
        let clean = tz_env.trim().trim_start_matches(':');
        if !clean.is_empty() {
            return Some(clean.to_string());
        }
    }
    if let Ok(text) = fs::read_to_string("/etc/timezone") {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Ok(target) = fs::read_link("/etc/localtime") {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let raw = target.as_os_str().as_bytes();
            for prefix in [
                b"/usr/share/zoneinfo/".as_slice(),
                b"/var/db/timezone/zoneinfo/".as_slice(),
                b"/etc/zoneinfo/".as_slice(),
            ] {
                if let Some(idx) = raw.windows(prefix.len()).position(|w| w == prefix) {
                    let rest = &raw[idx + prefix.len()..];
                    if !rest.is_empty() {
                        if let Ok(s) = std::str::from_utf8(rest) {
                            return Some(s.to_string());
                        }
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            let text = target.to_string_lossy();
            for prefix in [
                "/usr/share/zoneinfo/",
                "/var/db/timezone/zoneinfo/",
                "/etc/zoneinfo/",
            ] {
                if let Some(idx) = text.find(prefix) {
                    let rest = &text[idx + prefix.len()..];
                    if !rest.is_empty() {
                        return Some(rest.to_string());
                    }
                }
            }
        }
    }
    None
}

impl TzifZone {
    pub fn offset_seconds_at(&self, epoch: i64) -> i32 {
        if !self.transitions.is_empty() {
            if let Some(last) = self.transitions.last().copied() {
                if epoch >= last {
                    if let Some(posix) = &self.posix_tz {
                        return posix.offset_seconds_at(epoch);
                    }
                }
            }
            let pos = self.transitions.partition_point(|&t| t <= epoch);
            let idx = if pos == 0 {
                0
            } else {
                usize::from(*self.type_indices.get(pos - 1).unwrap_or(&0))
            };
            if let Some(tt) = self.types.get(idx) {
                return tt.utoff;
            }
        }
        if let Some(posix) = &self.posix_tz {
            return posix.offset_seconds_at(epoch);
        }
        self.types.first().map(|t| t.utoff).unwrap_or(0)
    }

    fn midnight_epoch(&self, date: NaiveDate) -> Option<i64> {
        let naive = date.and_hms_opt(0, 0, 0)?;
        let approx_utc = DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc).timestamp();
        let off1 = self.offset_seconds_at(approx_utc);
        let cand1 = approx_utc - i64::from(off1);
        let off2 = self.offset_seconds_at(cand1);
        let cand2 = approx_utc - i64::from(off2);
        Some(cand2)
    }
}

fn parse_tzif(name: &str, data: &[u8]) -> Option<TzifZone> {
    if data.len() < 44 || &data[0..4] != b"TZif" {
        return None;
    }
    let version = data[4];
    let v1_body_start = 44usize;
    let ttisutcnt = read_be_u32(data, 20)? as usize;
    let ttisstdcnt = read_be_u32(data, 24)? as usize;
    let leapcnt = read_be_u32(data, 28)? as usize;
    let timecnt = read_be_u32(data, 32)? as usize;
    let typecnt = read_be_u32(data, 36)? as usize;
    let charcnt = read_be_u32(data, 40)? as usize;

    let v1_len = timecnt
        .checked_mul(5)?
        .checked_add(typecnt.checked_mul(6)?)?
        .checked_add(charcnt)?
        .checked_add(leapcnt.checked_mul(8)?)?
        .checked_add(ttisstdcnt)?
        .checked_add(ttisutcnt)?;

    if version == 0 {
        // Parse v1 block directly
        let mut cursor = v1_body_start;
        if cursor + v1_len > data.len() {
            return None;
        }
        let mut transitions = Vec::with_capacity(timecnt);
        for _ in 0..timecnt {
            transitions.push(i64::from(read_be_i32(data, cursor)?));
            cursor += 4;
        }
        let type_indices = data.get(cursor..cursor + timecnt)?.to_vec();
        cursor += timecnt;
        let mut types = Vec::with_capacity(typecnt);
        for _ in 0..typecnt {
            let utoff = read_be_i32(data, cursor)?;
            types.push(TtInfo { utoff });
            cursor += 6;
        }
        return Some(TzifZone {
            name: name.to_string(),
            transitions,
            type_indices,
            types,
            posix_tz: None,
        });
    }

    // Parse v2/v3/v4 header right after v1 block
    let h2 = v1_body_start.checked_add(v1_len)?;
    if data.len() < h2 + 44 || &data[h2..h2 + 4] != b"TZif" {
        return None;
    }
    let ttisutcnt2 = read_be_u32(data, h2 + 20)? as usize;
    let ttisstdcnt2 = read_be_u32(data, h2 + 24)? as usize;
    let leapcnt2 = read_be_u32(data, h2 + 28)? as usize;
    let timecnt2 = read_be_u32(data, h2 + 32)? as usize;
    let typecnt2 = read_be_u32(data, h2 + 36)? as usize;
    let charcnt2 = read_be_u32(data, h2 + 40)? as usize;

    let mut cursor = h2 + 44;
    let mut transitions = Vec::with_capacity(timecnt2);
    for _ in 0..timecnt2 {
        transitions.push(read_be_i64(data, cursor)?);
        cursor += 8;
    }
    let type_indices = data.get(cursor..cursor + timecnt2)?.to_vec();
    cursor += timecnt2;
    let mut types = Vec::with_capacity(typecnt2);
    for _ in 0..typecnt2 {
        let utoff = read_be_i32(data, cursor)?;
        types.push(TtInfo { utoff });
        cursor += 6;
    }
    cursor = cursor
        .checked_add(charcnt2)?
        .checked_add(leapcnt2.checked_mul(12)?)?
        .checked_add(ttisstdcnt2)?
        .checked_add(ttisutcnt2)?;

    let mut posix_tz = None;
    if cursor < data.len() {
        if let Ok(footer) = std::str::from_utf8(&data[cursor..]) {
            let trimmed = footer.trim();
            if !trimmed.is_empty() {
                posix_tz = parse_posix_tz(trimmed);
            }
        }
    }

    Some(TzifZone {
        name: name.to_string(),
        transitions,
        type_indices,
        types,
        posix_tz,
    })
}

fn read_be_u32(data: &[u8], off: usize) -> Option<u32> {
    let slice: [u8; 4] = data.get(off..off + 4)?.try_into().ok()?;
    Some(u32::from_be_bytes(slice))
}

fn read_be_i32(data: &[u8], off: usize) -> Option<i32> {
    let slice: [u8; 4] = data.get(off..off + 4)?.try_into().ok()?;
    Some(i32::from_be_bytes(slice))
}

fn read_be_i64(data: &[u8], off: usize) -> Option<i64> {
    let slice: [u8; 8] = data.get(off..off + 8)?.try_into().ok()?;
    Some(i64::from_be_bytes(slice))
}

fn parse_posix_tz(spec: &str) -> Option<PosixTz> {
    let bytes = spec.as_bytes();
    let mut pos = skip_tz_abbr(bytes, 0)?;
    let (std_offset_west, next_pos) = parse_posix_offset(bytes, pos)?;
    pos = next_pos;
    if pos >= bytes.len() {
        return Some(PosixTz {
            std_offset_west,
            dst: None,
        });
    }
    let Some(dst_after_abbr) = skip_tz_abbr(bytes, pos) else {
        return None;
    };
    pos = dst_after_abbr;
    let dst_offset_west = if pos < bytes.len() && bytes[pos] != b',' {
        let (off, np) = parse_posix_offset(bytes, pos)?;
        pos = np;
        off
    } else {
        std_offset_west - 3600
    };
    if pos >= bytes.len() || bytes[pos] != b',' {
        return Some(PosixTz {
            std_offset_west,
            dst: None,
        });
    }
    let rest = &spec[pos + 1..];
    let mut parts = rest.split(',');
    let start_str = parts.next()?;
    let end_str = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let start = parse_posix_rule(start_str)?;
    let end = parse_posix_rule(end_str)?;
    Some(PosixTz {
        std_offset_west,
        dst: Some(PosixDst {
            dst_offset_west,
            start,
            end,
        }),
    })
}

fn skip_tz_abbr(bytes: &[u8], mut pos: usize) -> Option<usize> {
    if pos >= bytes.len() {
        return None;
    }
    if bytes[pos] == b'<' {
        pos += 1;
        while pos < bytes.len() && bytes[pos] != b'>' {
            pos += 1;
        }
        if pos >= bytes.len() {
            return None;
        }
        return Some(pos + 1);
    }
    let start = pos;
    while pos < bytes.len() && bytes[pos].is_ascii_alphabetic() {
        pos += 1;
    }
    if pos - start >= 3 { Some(pos) } else { None }
}

fn parse_posix_offset(bytes: &[u8], mut pos: usize) -> Option<(i32, usize)> {
    let sign = if pos < bytes.len() && bytes[pos] == b'-' {
        pos += 1;
        -1
    } else if pos < bytes.len() && bytes[pos] == b'+' {
        pos += 1;
        1
    } else {
        1
    };
    let (h, np) = parse_uint(bytes, pos)?;
    pos = np;
    let mut m = 0i32;
    let mut s = 0i32;
    if pos < bytes.len() && bytes[pos] == b':' {
        let (mv, np2) = parse_uint(bytes, pos + 1)?;
        m = mv;
        pos = np2;
        if pos < bytes.len() && bytes[pos] == b':' {
            let (sv, np3) = parse_uint(bytes, pos + 1)?;
            s = sv;
            pos = np3;
        }
    }
    Some((sign * (h * 3600 + m * 60 + s), pos))
}

fn parse_uint(bytes: &[u8], mut pos: usize) -> Option<(i32, usize)> {
    let start = pos;
    let mut val = 0i32;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        val = val
            .checked_mul(10)?
            .checked_add(i32::from(bytes[pos] - b'0'))?;
        pos += 1;
    }
    if pos == start { None } else { Some((val, pos)) }
}

fn parse_posix_rule(rule: &str) -> Option<PosixRule> {
    let (date_part, time_secs) = if let Some((d, t)) = rule.split_once('/') {
        let (off, _) = parse_posix_offset(t.as_bytes(), 0)?;
        (d, off)
    } else {
        (rule, 2 * 3600)
    };
    if let Some(rest) = date_part.strip_prefix('M') {
        let mut nums = rest.split('.');
        let m: u8 = nums.next()?.parse().ok()?;
        let w: u8 = nums.next()?.parse().ok()?;
        let d: u8 = nums.next()?.parse().ok()?;
        if nums.next().is_some() || !(1..=12).contains(&m) || !(1..=5).contains(&w) || d > 6 {
            return None;
        }
        return Some(PosixRule::Mwd(m, w, d, time_secs));
    }
    if let Some(rest) = date_part.strip_prefix('J') {
        let day: u16 = rest.parse().ok()?;
        if !(1..=365).contains(&day) {
            return None;
        }
        return Some(PosixRule::JulianOneBased(day, time_secs));
    }
    let day: u16 = date_part.parse().ok()?;
    if day > 365 {
        return None;
    }
    Some(PosixRule::JulianZeroBased(day, time_secs))
}

impl PosixTz {
    fn offset_seconds_at(&self, epoch: i64) -> i32 {
        let std_utoff = -self.std_offset_west;
        let Some(dst) = &self.dst else {
            return std_utoff;
        };
        let Some(dt_utc) = DateTime::<Utc>::from_timestamp(epoch, 0) else {
            return std_utoff;
        };
        let year = dt_utc.year();
        let dst_utoff = -dst.dst_offset_west;
        let start_utc = dst.start.to_utc_epoch(year, std_utoff);
        let end_utc = dst.end.to_utc_epoch(year, dst_utoff);
        let in_dst = if start_utc < end_utc {
            epoch >= start_utc && epoch < end_utc
        } else {
            // Southern hemisphere
            epoch >= start_utc || epoch < end_utc
        };
        if in_dst { dst_utoff } else { std_utoff }
    }
}

impl PosixRule {
    fn to_utc_epoch(&self, year: i32, local_utoff: i32) -> i64 {
        let date = match *self {
            PosixRule::JulianOneBased(day, _) => {
                let is_leap = NaiveDate::from_ymd_opt(year, 2, 29).is_some();
                let ord = if is_leap && day > 59 {
                    u32::from(day) + 1
                } else {
                    u32::from(day)
                };
                NaiveDate::from_yo_opt(year, ord)
            }
            PosixRule::JulianZeroBased(day, _) => NaiveDate::from_yo_opt(year, u32::from(day) + 1),
            PosixRule::Mwd(month, week, target_dow, _) => {
                let first = NaiveDate::from_ymd_opt(year, u32::from(month), 1);
                first.and_then(|f| {
                    let target = match target_dow {
                        0 => Weekday::Sun,
                        1 => Weekday::Mon,
                        2 => Weekday::Tue,
                        3 => Weekday::Wed,
                        4 => Weekday::Thu,
                        5 => Weekday::Fri,
                        _ => Weekday::Sat,
                    };
                    let first_dow = f.weekday().num_days_from_sunday();
                    let t_dow = target.num_days_from_sunday();
                    let delta = (t_dow + 7 - first_dow) % 7;
                    let mut day_num = 1 + delta + u32::from(week - 1) * 7;
                    let days_in_month = days_in_month(year, month);
                    while day_num > days_in_month {
                        day_num -= 7;
                    }
                    NaiveDate::from_ymd_opt(year, u32::from(month), day_num)
                })
            }
        };
        let time_secs = match *self {
            PosixRule::JulianOneBased(_, s)
            | PosixRule::JulianZeroBased(_, s)
            | PosixRule::Mwd(_, _, _, s) => s,
        };
        let base_date = date.unwrap_or_else(|| NaiveDate::from_ymd_opt(year, 1, 1).expect("jan 1"));
        let base_ts = DateTime::<Utc>::from_naive_utc_and_offset(
            base_date.and_hms_opt(0, 0, 0).expect("midnight"),
            Utc,
        )
        .timestamp();
        base_ts + i64::from(time_secs) - i64::from(local_utoff)
    }
}

fn days_in_month(year: i32, month: u8) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if NaiveDate::from_ymd_opt(year, 2, 29).is_some() {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

pub fn parse_rfc3339_utc(raw: &str) -> Result<DateTime<Utc>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("occurred_at cannot be empty".to_string());
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    // Support space separator or +HHMM / -HHMM / missing seconds just like Python's fromisoformat
    let normalized = if trimmed.ends_with('Z') || trimmed.ends_with('z') {
        format!("{}+00:00", &trimmed[..trimmed.len() - 1])
    } else {
        trimmed.to_string()
    };
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%dT%H:%M:%S%:z",
        "%Y-%m-%dT%H:%M%:z",
        "%Y-%m-%d %H:%M%:z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%d %H:%M:%S%.f%z",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%d %H:%M:%S%z",
    ] {
        if let Ok(dt) = DateTime::parse_from_str(&normalized, fmt) {
            return Ok(dt.with_timezone(&Utc));
        }
    }
    Err(format!("invalid timezone-aware ISO timestamp: {raw}"))
}

pub fn utc_rfc3339(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn parse_iso_week_start_date(period: &str) -> Option<NaiveDate> {
    let (year_s, week_s) = period.split_once("-W")?;
    if week_s.len() != 2 {
        return None;
    }
    let year: i32 = year_s.parse().ok()?;
    let week: u32 = week_s.parse().ok()?;
    NaiveDate::from_isoywd_opt(year, week, Weekday::Mon)
}

pub fn iso_week_range(period: &str) -> Option<(String, String)> {
    let start = parse_iso_week_start_date(period)?;
    let end = start + chrono::Duration::days(6);
    Some((
        format!(
            "{:04}-{:02}-{:02}",
            start.year(),
            start.month(),
            start.day()
        ),
        format!("{:04}-{:02}-{:02}", end.year(), end.month(), end.day()),
    ))
}

pub fn parse_date_ymd(raw: &str) -> Option<NaiveDate> {
    let trimmed = raw.trim();
    if trimmed.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").ok()
}

pub fn parse_http_date_epoch(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc2822(trimmed) {
        return Some(dt.timestamp());
    }
    // IMF-fixdate with explicit GMT
    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%a, %d %b %Y %H:%M:%S GMT") {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc).timestamp());
    }
    // RFC 850
    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%A, %d-%b-%y %H:%M:%S GMT") {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc).timestamp());
    }
    // asctime
    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%a %b %e %H:%M:%S %Y") {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc).timestamp());
    }
    None
}

fn system_local_zone() -> Option<TzifZone> {
    if let Some(name) = detect_system_timezone_name() {
        if let Some(TimeZoneSpec::Tzif(zone)) = resolve_named_timezone(&name) {
            return Some(zone);
        }
    }
    if let Ok(bytes) = fs::read("/etc/localtime") {
        if let Some(zone) = parse_tzif("system-local", &bytes) {
            return Some(zone);
        }
    }
    None
}

pub fn current_utc_now() -> DateTime<Utc> {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    DateTime::<Utc>::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos())
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("epoch"))
}

pub fn format_hhmm(dt: DateTime<Utc>, tz: &TimeZoneSpec) -> String {
    let naive = tz.to_local_naive(dt);
    format!("{:02}:{:02}", naive.hour(), naive.minute())
}

pub fn naive_date_to_string(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

pub fn make_utc_datetime(
    year: i32,
    month: u32,
    day: u32,
    h: u32,
    m: u32,
    s: u32,
) -> Option<DateTime<Utc>> {
    let d = NaiveDate::from_ymd_opt(year, month, day)?;
    let t = NaiveTime::from_hms_opt(h, m, s)?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDateTime::new(d, t),
        Utc,
    ))
}
