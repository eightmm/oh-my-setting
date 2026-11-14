#![forbid(unsafe_code)]
//! Read-only canonical status validation and schema-2 work envelope projections.
use oms_context::{Json, parse_experiment_json, parse_json};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use unicode_general_category::{GeneralCategory, get_general_category};

/// A malformed input or unsupported report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkViewError(pub String);
impl fmt::Display for WorkViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for WorkViewError {}
type Result<T> = std::result::Result<T, WorkViewError>;
fn err(s: impl Into<String>) -> WorkViewError {
    WorkViewError(s.into())
}
fn invalid(s: &str) -> WorkViewError {
    err(format!("invalid work snapshot: {s}"))
}
static NULL: Json = Json::Null;
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    if let Json::Object(a) = v {
        a.iter()
            .find(|(key, _)| key == k.as_bytes())
            .map(|(_, v)| v)
            .unwrap_or(&NULL)
    } else {
        &NULL
    }
}

fn status_payload(raw: &[u8], message: &str) -> Result<Json> {
    // Status commands use Python's JSON dialect, including last-key-wins and
    // lossless surrogate strings. Keep unknown fields for the envelope adapter.
    let payload = parse_experiment_json(raw, 4 * 1024 * 1024).map_err(|_| err(message))?;
    if !matches!(payload, Json::Object(_))
        || status_number(field(&payload, "schema")).as_deref() != Some("1")
        || !matches!(field(&payload, "present"), Json::Bool(true))
    {
        return Err(err(message));
    }
    Ok(payload)
}

fn status_count(value: &Json) -> Option<&str> {
    match value {
        Json::Number(n) if n == "-0" => Some("0"),
        Json::Number(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => Some(n),
        _ => None,
    }
}

fn status_number(value: &Json) -> Option<String> {
    if let Some(n) = status_count(value) {
        return Some(n.to_owned());
    }
    match value {
        Json::Bool(b) => Some(u8::from(*b).to_string()),
        Json::Number(n) if n.contains(['.', 'e', 'E']) => {
            let number = n.parse::<f64>().ok()?;
            (number.is_finite() && number >= 0.0 && number.fract() == 0.0).then(|| {
                if number == 0.0 {
                    "0".into()
                } else {
                    format!("{number:.0}")
                }
            })
        }
        _ => None,
    }
}

fn status_sum(left: &str, right: &str) -> String {
    // Counts are Python integers: arithmetic must not narrow them to u64.
    let mut a = left.bytes().rev();
    let mut b = right.bytes().rev();
    let mut carry = 0;
    let mut sum = Vec::new();
    loop {
        let (x, y) = (a.next(), b.next());
        if x.is_none() && y.is_none() && carry == 0 {
            break;
        }
        let digit = x.map_or(0, |n| n - b'0') + y.map_or(0, |n| n - b'0') + carry;
        sum.push(b'0' + digit % 10);
        carry = digit / 10;
    }
    sum.reverse();
    String::from_utf8(sum).unwrap()
}

/// Validate a present `agent-task status --json` snapshot for a native envelope.
/// Preserves unknown fields, number spellings and WTF-8 strings. Status and
/// verification strings are not enums here; the envelope adapter normalizes them.
/// Input uses the canonical JSON dialect, bounded to 4 MiB and 1024 nesting levels.
/// Absence, malformed input and invalid field types are errors, never empty state.
pub fn task_status_snapshot(raw: &[u8]) -> Result<Json> {
    let message = "canonical task status projection is unavailable or invalid";
    let payload = status_payload(raw, message)?;
    if !matches!(field(&payload, "status"), Json::String(_))
        || !matches!(field(&payload, "verification"), Json::String(_))
        || !matches!(field(&payload, "stale"), Json::Bool(_))
    {
        return Err(err(message));
    }
    Ok(payload)
}

/// Validate a present `agent-plan status --json` snapshot against the physical
/// plan's projected task IDs. Counts and actionable membership must agree;
/// cancelled tasks close work but do not establish verified completion.
/// Preserves unknown fields and uses the same bounded JSON dialect as
/// [`task_status_snapshot`]. This validates data, not freshness or authority.
pub fn plan_status_snapshot(raw: &[u8], task_ids: &[&str]) -> Result<Json> {
    let message = "canonical plan status projection is unavailable or invalid";
    let payload = status_payload(raw, message)?;
    let counts = [
        "task_count",
        "verified_count",
        "cancelled_count",
        "closed_count",
        "unfinished_count",
    ]
    .map(|key| status_count(field(&payload, key)).ok_or_else(|| err(message)));
    let [total, verified, cancelled, closed, unfinished] = counts;
    let (total, verified, cancelled, closed, unfinished) =
        (total?, verified?, cancelled?, closed?, unfinished?);
    for key in ["nonempty", "all_done", "all_closed", "has_unfinished"] {
        if !matches!(field(&payload, key), Json::Bool(_)) {
            return Err(err(message));
        }
    }
    let by_state = field(&payload, "by_state");
    let contract = field(&payload, "contract");
    if !matches!(by_state, Json::Object(_)) || !matches!(contract, Json::Object(_)) {
        return Err(err(message));
    }
    for key in ["bound", "satisfied", "project_present", "project_healthy"] {
        if !matches!(field(contract, key), Json::Bool(_)) {
            return Err(err(message));
        }
    }
    for key in [
        "project_state",
        "blocker",
        "expected_spec_sha256",
        "current_spec_sha256",
    ] {
        if !matches!(field(contract, key), Json::String(_)) {
            return Err(err(message));
        }
    }
    for key in ["actionable", "stale", "stale_review"] {
        if !matches!(field(&payload, key), Json::Array(_)) {
            return Err(err(message));
        }
    }
    let Json::Array(actionable) = field(&payload, "actionable") else {
        unreachable!()
    };
    let mut seen = BTreeSet::new();
    for item in actionable {
        if let Json::String(id) = item {
            if !seen.insert(id) {
                return Err(err(message));
            }
        } else {
            return Err(err(message));
        }
    }
    let state_count = |key: &str| {
        if let Json::Object(fields) = by_state
            && !fields.iter().any(|(name, _)| name == key.as_bytes())
        {
            Some("0".to_owned())
        } else {
            status_number(field(by_state, key))
        }
    };
    if state_count("done").as_deref() != Some(verified)
        || state_count("cancelled").as_deref() != Some(cancelled)
        || status_sum(verified, cancelled) != closed
        || status_sum(unfinished, closed) != total
        || field(&payload, "all_done") != &Json::Bool(total != "0" && verified == total)
        || field(&payload, "all_closed") != &Json::Bool(total != "0" && unfinished == "0")
        || field(&payload, "has_unfinished") != &Json::Bool(unfinished != "0")
    {
        return Err(err("canonical plan status counts are inconsistent"));
    }
    if total != task_ids.len().to_string()
        || field(&payload, "nonempty") != &Json::Bool(!task_ids.is_empty())
    {
        return Err(err(
            "canonical plan status disagrees with the physical plan task set",
        ));
    }
    let ids = task_ids
        .iter()
        .map(|id| id.as_bytes())
        .collect::<BTreeSet<_>>();
    if seen.iter().any(|id| !ids.contains(id.as_slice())) {
        return Err(err(
            "canonical plan status names an unknown actionable task",
        ));
    }
    Ok(payload)
}

fn evidence_ordinal(value: &Json) -> (bool, &str) {
    let integer = match value {
        Json::Bool(true) => "1",
        Json::Number(n)
            if !n.strip_prefix('-').unwrap_or(n).is_empty()
                && n.strip_prefix('-')
                    .unwrap_or(n)
                    .bytes()
                    .all(|b| b.is_ascii_digit()) =>
        {
            n.as_str()
        }
        _ => "0",
    };
    let digits = integer
        .strip_prefix('-')
        .unwrap_or(integer)
        .trim_start_matches('0');
    if digits.is_empty() {
        (false, "0")
    } else {
        (integer.starts_with('-'), digits)
    }
}

fn evidence_timestamp(row: &Json) -> &[u8] {
    match field(row, "ts") {
        Json::String(s) => s.as_slice(),
        _ => &[],
    }
}

fn evidence_item_cmp(left: &Json, right: &Json) -> std::cmp::Ordering {
    evidence_timestamp(left)
        .cmp(evidence_timestamp(right))
        .then_with(|| {
            let (a_negative, a) = evidence_ordinal(field(left, "_ordinal"));
            let (b_negative, b) = evidence_ordinal(field(right, "_ordinal"));
            b_negative.cmp(&a_negative).then_with(|| {
                let magnitude = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
                if a_negative {
                    magnitude.reverse()
                } else {
                    magnitude
                }
            })
        })
}

fn evidence_has(row: &Json, key: &str, value: &str) -> bool {
    matches!(field(row, key), Json::String(s) if s == value.as_bytes())
}

fn evidence_precedence(items: &[&Json]) -> Option<&'static str> {
    ["failed", "verified", "inconclusive", "skipped_with_reason"]
        .into_iter()
        .find(|status| items.iter().any(|row| evidence_has(row, "status", status)))
}

/// Project one criterion's normalized evidence for the pending native runtime
/// coverage producer, matching `oms_runtime.evidence._project_status`.
/// The producer supplies its retained evidence window (currently the latest 20)
/// after classifying receipts and establishing freshness; this does neither.
/// Rows must be objects with string timestamps (absent means empty). Timestamps
/// sort lexically, including WTF-8, then by arbitrary-size signed integer ordinal;
/// absent/noninteger ordinals mean zero, as in the canonical Python projection.
/// Unknown statuses default to inconclusive; unknown signals do not qualify as
/// execution or parent attestation. Advisory success must already be downgraded
/// by the producer's receipt classification. This only reads the supplied
/// rows and confers no execution, admission or completion authority.
pub fn criterion_evidence_status(items: &[Json]) -> Result<&'static str> {
    for row in items {
        object(row, "criterion evidence row")?;
        if let Json::Object(fields) = row
            && fields.iter().any(|(key, _)| key == b"ts")
            && !matches!(field(row, "ts"), Json::String(_))
        {
            return Err(invalid("criterion evidence timestamp"));
        }
    }
    if items.is_empty() {
        return Ok("missing");
    }
    let mut fresh = items
        .iter()
        .filter(|row| !evidence_has(row, "status", "stale"))
        .collect::<Vec<_>>();
    fresh.sort_by(|a, b| evidence_item_cmp(a, b));
    let Some(latest) = fresh.last() else {
        return Ok("stale");
    };
    let latest_items = fresh
        .iter()
        .copied()
        .filter(|row| evidence_item_cmp(row, latest).is_eq())
        .collect::<Vec<_>>();
    if latest_items
        .iter()
        .any(|row| evidence_has(row, "status", "failed"))
    {
        return Ok("failed");
    }
    let executed = |row: &&Json| {
        evidence_has(row, "signal", "execution_feedback")
            || evidence_has(row, "signal", "parent_attestation")
    };
    if latest_items
        .iter()
        .all(|row| evidence_has(row, "signal", "advisory"))
    {
        let executed_items = fresh.iter().copied().filter(executed).collect::<Vec<_>>();
        if let Some(last) = executed_items.last() {
            let tied = executed_items
                .iter()
                .copied()
                .filter(|row| evidence_item_cmp(row, last).is_eq())
                .collect::<Vec<_>>();
            if let Some(status) = evidence_precedence(&tied) {
                return Ok(status);
            }
        } else if items
            .iter()
            .any(|row| evidence_has(row, "status", "stale") && executed(&row))
        {
            return Ok("stale");
        }
    }
    Ok(evidence_precedence(&latest_items).unwrap_or("inconclusive"))
}
fn object<'a>(v: &'a Json, name: &str) -> Result<&'a Json> {
    if matches!(v, Json::Object(_)) {
        Ok(v)
    } else {
        Err(invalid(name))
    }
}
fn rows<'a>(v: &'a Json, name: &str) -> Result<&'a [Json]> {
    match v {
        Json::Array(a) if a.len() <= 4096 => Ok(a),
        _ => Err(err(format!("invalid or oversized work snapshot: {name}"))),
    }
}
fn chars(bytes: &[u8]) -> Vec<char> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0xed && i + 2 < bytes.len() && (0xa0..=0xbf).contains(&bytes[i + 1]) {
            out.push('\0');
            i += 3;
        } else {
            let tail = &bytes[i..];
            match std::str::from_utf8(tail) {
                Ok(s) => {
                    out.extend(s.chars());
                    break;
                }
                Err(e) => {
                    let n = e.valid_up_to();
                    if n > 0 {
                        out.extend(std::str::from_utf8(&tail[..n]).unwrap().chars());
                        i += n;
                    } else {
                        out.push('\u{fffd}');
                        i += e.error_len().unwrap_or(1);
                    }
                }
            }
        }
    }
    out
}
fn text(v: &Json, name: &str, max: usize) -> Result<String> {
    if let Json::String(b) = v {
        let c = chars(b);
        if c.len() > max {
            return Err(invalid(name));
        }
        Ok(c.into_iter()
            .map(|c| {
                if matches!(c, '\t' | '\r' | '\n') {
                    ' '
                } else if matches!(
                    get_general_category(c),
                    GeneralCategory::Control
                        | GeneralCategory::Format
                        | GeneralCategory::Surrogate
                        | GeneralCategory::PrivateUse
                        | GeneralCategory::Unassigned
                        | GeneralCategory::LineSeparator
                        | GeneralCategory::ParagraphSeparator
                        | GeneralCategory::SpaceSeparator
                ) && c != ' '
                {
                    '?'
                } else {
                    c
                }
            })
            .collect())
    } else {
        Err(invalid(name))
    }
}
fn id(v: &Json, name: &str, optional: bool) -> Result<String> {
    if optional && matches!(v, Json::Null) {
        return Ok(String::new());
    }
    if let Json::String(b) = v {
        if optional && b.is_empty() {
            return Ok(String::new());
        }
        if !b.is_empty()
            && b.len() <= 160
            && b.iter()
                .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(c))
        {
            return Ok(String::from_utf8(b.clone()).unwrap());
        }
    }
    Err(invalid(name))
}
fn digest(v: &Json, name: &str, optional: bool) -> Result<String> {
    if optional && matches!(v, Json::Null) {
        return Ok(String::new());
    }
    if let Json::String(b) = v {
        if optional && b.is_empty() {
            return Ok(String::new());
        }
        if b.len() == 64
            && b.iter()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
        {
            return Ok(String::from_utf8(b.clone()).unwrap());
        }
    }
    Err(invalid(name))
}
fn boolean(v: &Json, name: &str) -> Result<bool> {
    if let Json::Bool(b) = v {
        Ok(*b)
    } else {
        Err(invalid(name))
    }
}
// CPython's scanner permits one trailing byte before the timezone marker;
// the offset scanner instead requires the entire component to be consumed.
fn iso_clock(input: &str, offset: bool) -> Option<i64> {
    let bytes = input.as_bytes();
    let mut values = [0_i64; 3];
    let mut pos = 0;
    let mut separated = false;
    let mut fraction = None;
    for (i, value) in values.iter_mut().enumerate() {
        let pair = bytes.get(pos..pos + 2)?;
        if !pair.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *value = i64::from(pair[0] - b'0') * 10 + i64::from(pair[1] - b'0');
        pos += 2;
        let Some(&next) = bytes.get(pos) else { break };
        if i == 0 {
            separated = next == b':';
        }
        if pos + 1 >= bytes.len() {
            if offset {
                return None;
            }
            break;
        }
        if separated && next == b':' {
            pos += 1;
        } else if next == b'.' || next == b',' {
            pos += 1;
            fraction = Some(pos);
            break;
        } else if separated {
            return None;
        }
        if i == 2 {
            fraction = Some(pos);
        }
    }
    let mut micros = 0;
    if let Some(start) = fraction {
        let digits = bytes.get(start..)?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        for d in digits.iter().take(6) {
            micros = micros * 10 + i64::from(*d - b'0');
        }
        for _ in digits.len().min(6)..6 {
            micros *= 10;
        }
    }
    let [h, m, sec] = values;
    if !offset && (h > 23 || m > 59 || sec > 59) {
        return None;
    }
    Some(((h * 60 + m) * 60 + sec) * 1_000_000 + micros)
}
fn date_separator(chars: &[char]) -> Option<usize> {
    let n = chars.len();
    if n < 8 {
        return None;
    }
    let digit = |i: usize| chars.get(i).is_some_and(char::is_ascii_digit);
    if chars[4] == '-' {
        if chars[5] == 'W' {
            if n > 8 && chars[8] == '-' {
                if n == 9 {
                    return None;
                }
                if n > 10 && digit(10) {
                    Some(8)
                } else {
                    Some(10)
                }
            } else {
                Some(8)
            }
        } else {
            Some(10)
        }
    } else if chars[4] == 'W' {
        let mut i = 7;
        while i < n && digit(i) {
            i += 1;
        }
        if i < 9 {
            Some(i)
        } else if i % 2 == 0 {
            Some(7)
        } else {
            Some(8)
        }
    } else {
        Some(8)
    }
}
fn time(v: &Json) -> Result<i64> {
    use chrono::{Datelike, NaiveDate, Weekday};
    let input = text(v, "generated_at", 80)?.replace('Z', "+00:00");
    let result = (|| -> Option<i64> {
        let chars = input.chars().collect::<Vec<_>>();
        let sep = date_separator(&chars)?;
        if sep >= chars.len() {
            return None;
        }
        let date = chars[..sep].iter().collect::<String>();
        let rest = chars[sep + 1..].iter().collect::<String>();
        if !date.is_ascii() || !rest.is_ascii() {
            return None;
        }
        let d = date.as_bytes();
        let digits = |a: usize, b: usize| {
            d.get(a..b)
                .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
        };
        let day = if d.get(4) == Some(&b'W') || d.get(4..6) == Some(b"-W") {
            let extended = d.get(4) == Some(&b'-');
            let week_start = if extended { 6 } else { 5 };
            let weekday_start = week_start + 2;
            if !digits(0, 4) || !digits(week_start, weekday_start) {
                return None;
            }
            let weekday = if d.len() == weekday_start {
                b'1'
            } else if extended && d.len() == weekday_start + 2 && d[weekday_start] == b'-' {
                d[weekday_start + 1]
            } else if !extended && d.len() == weekday_start + 1 {
                d[weekday_start]
            } else {
                return None;
            };
            let year: i32 = date[0..4].parse().ok()?;
            if !(1..=9999).contains(&year) {
                return None;
            }
            let week = date[week_start..weekday_start].parse().ok()?;
            let weekday = match weekday {
                b'1' => Weekday::Mon,
                b'2' => Weekday::Tue,
                b'3' => Weekday::Wed,
                b'4' => Weekday::Thu,
                b'5' => Weekday::Fri,
                b'6' => Weekday::Sat,
                b'7' => Weekday::Sun,
                _ => return None,
            };
            NaiveDate::from_isoywd_opt(year, week, weekday)?
        } else {
            let (month_start, day_start) = if d.len() == 10 && d[4] == b'-' && d[7] == b'-' {
                (5, 8)
            } else if d.len() == 8 {
                (4, 6)
            } else {
                return None;
            };
            if !digits(0, 4)
                || !digits(month_start, month_start + 2)
                || !digits(day_start, day_start + 2)
            {
                return None;
            }
            NaiveDate::from_ymd_opt(
                date[0..4].parse().ok()?,
                date[month_start..month_start + 2].parse().ok()?,
                date[day_start..day_start + 2].parse().ok()?,
            )?
        };
        if !(1..=9999).contains(&day.year()) {
            return None;
        }
        let tz = rest.find(['+', '-'])?;
        let clock = &rest[..tz];
        let value = iso_clock(clock, false)?;
        let zone = &rest[tz..];
        let offset = iso_clock(&zone[1..], true)?;
        if offset >= 86_400_000_000 {
            return None;
        }
        let offset = if offset < 1_000_000 {
            0
        } else if zone.starts_with('-') {
            -offset
        } else {
            offset
        };
        let instant = day.and_hms_opt(0, 0, 0)?.and_utc().timestamp_micros() + value - offset;
        (-62_135_596_800_000_000..=253_402_300_799_999_999)
            .contains(&instant)
            .then_some(instant)
    })();
    result.ok_or_else(|| err("work snapshot needs a timezone-aware generated_at"))
}
fn unique(v: &[Value], key: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for row in v {
        if !seen.insert(row[key].as_str().unwrap_or_default().to_owned()) {
            return Err(err(format!("work snapshot has duplicate {key}")));
        }
    }
    Ok(())
}
fn validate(raw: &Json) -> Result<Value> {
    object(raw, "root")?;
    if !matches!(field(raw,"schema"),Json::Number(n) if n=="2") {
        return Err(err("unsupported TaskEnvelope schema (expected 2)"));
    }
    let instant = time(field(raw, "generated_at"))?;
    let generated = match field(raw, "generated_at") {
        Json::String(b) => String::from_utf8(b.clone()).ok(),
        _ => unreachable!(),
    };
    let state = digest(field(raw, "state_digest"), "state_digest", false)?;
    let plan = object(field(raw, "plan"), "plan")?;
    let present = boolean(field(plan, "present"), "plan.present")?;
    let pid = id(field(plan, "plan_id"), "plan_id", true)?;
    let task = object(field(raw, "task"), "task")?;
    let tid = id(field(task, "task_id"), "active task id", true)?;
    let mut tasks = Vec::new();
    for row in rows(field(plan, "tasks"), "tasks")? {
        object(row, "task row")?;
        let depends = rows(field(row, "depends"), "depends")?
            .iter()
            .map(|d| id(d, "dependency", false))
            .collect::<Result<Vec<_>>>()?;
        if depends.iter().collect::<BTreeSet<_>>().len() != depends.len() {
            return Err(err("work snapshot has duplicate dependency"));
        }
        tasks.push(json!({"id":id(field(row,"id"),"task id",false)?,"title":text(field(row,"title"),"task title",4096)?,"state":text(field(row,"state"),"task state",80)?,"depends":depends,"verify_digest":digest(field(row,"verify_digest"),"verify digest",true)?,"verify_present":boolean(field(row,"verify_present"),"verify_present")?}));
    }
    unique(&tasks, "id")?;
    let empty = Json::Array(vec![]);
    let a = match plan {
        Json::Object(m) if !m.iter().any(|(k, _)| k == b"actionable") => &empty,
        _ => field(plan, "actionable"),
    };
    let actionable = rows(a, "actionable")?
        .iter()
        .map(|a| id(a, "actionable task", false))
        .collect::<Result<Vec<_>>>()?;
    if actionable.iter().collect::<BTreeSet<_>>().len() != actionable.len()
        || actionable
            .iter()
            .any(|a| !tasks.iter().any(|t| t["id"] == *a))
    {
        return Err(invalid("actionable task set"));
    }
    if !present && (!pid.is_empty() || !tasks.is_empty() || !actionable.is_empty()) {
        return Err(err("absent plan carries live task state"));
    }
    let empty_obj = Json::Object(vec![]);
    let contract = match plan {
        Json::Object(m) if !m.iter().any(|(k, _)| k == b"contract") => &empty_obj,
        _ => field(plan, "contract"),
    };
    object(contract, "contract")?;
    let mut cv = json!({});
    for k in ["bound", "satisfied"] {
        if let Json::Object(m) = contract
            && m.iter().any(|(key, _)| key == k.as_bytes())
        {
            cv[k] = json!(boolean(field(contract, k), &format!("contract.{k}"))?)
        }
    }
    let empty_str = Json::String(vec![]);
    let get_default = |v: &Json, k: &str| {
        if let Json::Object(m) = v {
            m.iter().any(|(key, _)| key == k.as_bytes())
        } else {
            false
        }
    };
    cv["blocker"] = json!(text(
        if get_default(contract, "blocker") {
            field(contract, "blocker")
        } else {
            &empty_str
        },
        "contract blocker",
        300
    )?);
    let mut criteria = Vec::new();
    for row in rows(field(raw, "criteria"), "criteria")? {
        object(row, "criterion")?;
        let status = text(field(row, "status"), "criterion status", 4096)?;
        if ![
            "verified",
            "failed",
            "missing",
            "stale",
            "inconclusive",
            "skipped_with_reason",
        ]
        .contains(&status.as_str())
        {
            return Err(invalid("criterion status"));
        }
        let mut evidence = Vec::new();
        for item in rows(field(row, "evidence"), "evidence")? {
            object(item, "evidence row")?;
            let mut e = json!({});
            for k in [
                "evidence_ref",
                "kind",
                "status",
                "signal",
                "source",
                "support",
                "ts",
            ] {
                e[k] = json!(text(
                    if get_default(item, k) {
                        field(item, k)
                    } else {
                        &empty_str
                    },
                    &format!("evidence {k}"),
                    4096
                )?)
            }
            evidence.push(e)
        }
        criteria.push(json!({"id":id(field(row,"id"),"criterion id",false)?,"text":text(field(row,"text"),"criterion text",4096)?,"source":text(field(row,"source"),"criterion source",80)?,"status":status,"plan_task_id":id(field(row,"plan_task_id"),"plan task link",true)?,"plan_id":id(field(row,"plan_id"),"criterion plan id",true)?,"active_task_id":id(field(row,"active_task_id"),"active task link",true)?,"evidence":evidence}));
    }
    unique(&criteria, "id")?;
    let mut actions = Vec::new();
    for row in rows(field(raw, "next_actions"), "next_actions")? {
        object(row, "action")?;
        let priority = match field(row, "priority") {
            Json::Number(n) if !n.contains(['.', 'e', 'E']) => {
                n.parse::<i32>().ok().filter(|n| (0..=1000).contains(n))
            }
            _ => None,
        }
        .ok_or_else(|| invalid("action priority"))?;
        actions.push(json!({"id":id(field(row,"id"),"action id",false)?,"priority":priority,"authority":text(field(row,"authority"),"action authority",160)?,"reason":text(field(row,"reason"),"action reason",4096)?,"command":text(field(row,"command"),"action command",4096)?}));
    }
    unique(&actions, "id")?;
    let mut sources = Vec::new();
    for row in rows(field(raw, "sources"), "sources")? {
        object(row, "source")?;
        sources.push(json!({"path":text(field(row,"path"),"source path",4096)?,"sha256":digest(field(row,"sha256"),"source sha256",true)?}));
    }
    unique(&sources, "path")?;
    let repo = object(field(raw, "repo"), "repo")?;
    let head = match field(repo, "head") {
        Json::Null => Value::Null,
        Json::String(b)
            if (b.len() == 40 || b.len() == 64)
                && b.iter()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c)) =>
        {
            json!(String::from_utf8(b.clone()).unwrap())
        }
        _ => return Err(invalid("head")),
    };
    Ok(
        json!({"_instant":instant,"_raw": match field(raw,"generated_at") {Json::String(b)=>b, _=>unreachable!()},"generated_at":generated,"state_digest":state,"plan_id":pid,"active_task_id":tid,"tasks":tasks,"criteria":criteria,"actions":actions,"actionable":actionable,"contract":cv,"sources":sources,"head":head}),
    )
}
fn parse(raw: &[u8], limit: usize) -> Result<Value> {
    if !(1..=100).contains(&limit) {
        return Err(err("limit must be an integer from 1 to 100"));
    }
    validate(&parse_json(raw).map_err(|e| err(e.replace("context manifest", "work snapshot")))?)
}
fn bounded(mut a: Vec<Value>, limit: usize) -> Value {
    let total = a.len();
    a.truncate(limit);
    json!({"total":total,"items":a,"omitted":total.saturating_sub(limit)})
}
fn base(kind: &str, v: &Value) -> Value {
    json!({"schema":1,"kind":kind,"generated_at":v["generated_at"],"state_digest":v["state_digest"],"plan_id":nullable(&v["plan_id"]),"read_only":true,"authoritative":false,"note":"Read-time projection only. Commands are text, not execution or permission. Missing links are not inferred."})
}
fn nullable(v: &Value) -> Value {
    if v.as_str() == Some("") {
        Value::Null
    } else {
        v.clone()
    }
}
fn array(v: &Value) -> &Vec<Value> {
    v.as_array().unwrap()
}
fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}
fn matrix(v: &Value, limit: usize, filter: Option<&str>) -> Result<Value> {
    let tasks = array(&v["tasks"]);
    if let Some(t) = filter
        && !tasks.iter().any(|x| s(&x["id"]) == t)
        && s(&v["active_task_id"]) != t
    {
        return Err(err("task is absent from this work snapshot"));
    }
    let mut linked = BTreeSet::new();
    let mut rows = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    for c in array(&v["criteria"]) {
        let mut tid = s(&c["plan_task_id"]);
        let mut task = tasks.iter().find(|t| s(&t["id"]) == tid);
        let link = if !tid.is_empty()
            && task.is_some()
            && !s(&c["plan_id"]).is_empty()
            && c["plan_id"] == v["plan_id"]
        {
            linked.insert(tid.to_owned());
            "plan-task-lineage"
        } else if !s(&c["active_task_id"]).is_empty() && c["active_task_id"] == v["active_task_id"]
        {
            tid = s(&c["active_task_id"]);
            task = None;
            "active-task-lineage"
        } else {
            tid = "";
            task = None;
            "unrecorded"
        };
        if filter.is_some_and(|f| f != tid) {
            continue;
        }
        *counts.entry(s(&c["status"]).to_owned()).or_default() += 1;
        rows.push(json!({"criterion_id":c["id"],"criterion":c["text"],"status":c["status"],"task_id":if tid.is_empty(){None}else{Some(tid)},"link":link,"task_state":task.map(|t|t["state"].clone()),"verify_digest":task.map(|t|nullable(&t["verify_digest"])).unwrap_or(Value::Null),"evidence":bounded(array(&c["evidence"]).clone(),limit)}));
    }
    let missing = tasks
        .iter()
        .map(|t| s(&t["id"]).to_owned())
        .filter(|id| !linked.contains(id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(Value::String)
        .collect();
    let mut r = base("oms-evidence-matrix", v);
    r["criteria"] = bounded(rows, limit);
    r["status_counts"] = json!(counts);
    r["filter_task"] = json!(filter);
    r["total_declared_criteria"] = json!(array(&v["criteria"]).len());
    r["tasks_without_criterion_link"] = bounded(missing, limit);
    r["plan_acceptance_not_landing"] = json!(true);
    r["limitation"] = json!(
        "No new covers mappings or landed-commit links are invented; existing evidence decides criterion status."
    );
    Ok(r)
}
fn inbox(v: &Value, limit: usize, filter: Option<&str>) -> Result<Value> {
    let tasks = array(&v["tasks"]);
    if filter.is_some_and(|f| !tasks.iter().any(|t| s(&t["id"]) == f)) {
        return Err(err("task is absent from this work snapshot"));
    }
    let contract = &v["contract"];
    let blocked = contract["bound"] == true && contract["satisfied"] == false;
    let mut rows = Vec::new();
    for t in tasks {
        let tid = s(&t["id"]);
        if filter.is_some_and(|f| f != tid) {
            continue;
        }
        let mut waits = Vec::new();
        for d in array(&t["depends"]) {
            let found = tasks.iter().find(|x| x["id"] == *d);
            let state = found.map(|x| s(&x["state"])).unwrap_or("unknown");
            if state != "done" {
                waits.push(json!({"task_id":d,"state":state,"reason":if found.is_some(){"dependency-not-done"}else{"dependency-missing"}}))
            }
        }
        let mut reasons = Vec::new();
        if blocked {
            let b = s(&contract["blocker"]);
            reasons.push(
                json!({"code":"plan-contract-blocked","detail":if b.is_empty(){"unknown"}else{b}}),
            )
        }
        if !waits.is_empty() {
            reasons.push(json!({"code":"dependencies-unfinished","detail":"Cancelled tasks do not satisfy dependencies."}))
        }
        let actionable = array(&v["actionable"]).contains(&t["id"]);
        let contradiction = actionable && (!waits.is_empty() || blocked || t["state"] != "ready");
        if contradiction {
            reasons.push(json!({"code":"conflicting-projection","detail":"Re-read canonical plan state before acting."}))
        }
        if reasons.is_empty() {
            let label = match s(&t["state"]) {
                "ready" => {
                    if actionable {
                        "ready"
                    } else {
                        "not-actionable-reason-unreported"
                    }
                }
                "claimed" | "running" => "attempt-active",
                "review" => "parent-review-pending",
                "landing" => "landing-in-progress",
                "done" => "recorded-done",
                "cancelled" => "cancelled-not-verified",
                _ => "reason-unreported",
            };
            reasons.push(
                json!({"code":label,"detail":"Recorded task state; not a completion receipt."}),
            )
        }
        rows.push(json!({"task_id":tid,"title":t["title"],"state":t["state"],"canonical_actionable":actionable,"conflicting_projection":contradiction,"depends_waiting":bounded(waits,limit),"reasons":reasons,"next_actor":"parent-controller","human_decision_required":null}));
    }
    let mut actions = array(&v["actions"]).clone();
    actions.sort_by(|a, b| {
        b["priority"]
            .as_u64()
            .cmp(&a["priority"].as_u64())
            .then_with(|| s(&a["id"]).cmp(s(&b["id"])))
    });
    for a in &mut actions {
        a["next_actor"] = json!("parent-controller");
        a["human_decision_required"] = Value::Null
    }
    let mut r = base("oms-action-inbox", v);
    r["tasks"] = bounded(rows, limit);
    r["actions"] = bounded(actions, limit);
    r["filter_task"] = json!(filter);
    r["limitation"] = json!(
        "Authority labels do not identify a human. No inferred root cause, claim, retry, dispatch or approval occurs."
    );
    Ok(r)
}
/// A report cannot be encoded losslessly as UTF-8 JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportEncodingError {
    LoneSurrogate,
}
impl std::fmt::Display for ReportEncodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("report timestamp contains a lone surrogate")
    }
}
impl std::error::Error for ReportEncodingError {}
/// A report produced by the work views. Its contents are read-only.
///
/// Arbitrary JSON cannot be passed to the renderer:
/// ```compile_fail
/// let raw = serde_json::json!({"schema": 1, "kind": "other"});
/// let _ = oms_work_inspect::render(&raw);
/// ```
/// A validated report with lossless WTF-8 timestamps.
#[derive(Debug, Clone)]
pub struct Report {
    value: Value,
    current: Vec<u8>,
    before: Option<Vec<u8>>,
}
impl Report {
    /// Borrow serializable JSON, or return an encoding error before producing bytes.
    pub fn as_value(&self) -> std::result::Result<&Value, ReportEncodingError> {
        if std::str::from_utf8(&self.current).is_err()
            || self
                .before
                .as_ref()
                .is_some_and(|v| std::str::from_utf8(v).is_err())
        {
            Err(ReportEncodingError::LoneSurrogate)
        } else {
            Ok(&self.value)
        }
    }
    /// Original generated_at, in WTF-8.
    pub fn generated_at_wtf8(&self) -> &[u8] {
        &self.current
    }
    /// Original baseline_at, in WTF-8, when present.
    pub fn baseline_at_wtf8(&self) -> Option<&[u8]> {
        self.before.as_deref()
    }
}
fn raw_time(v: &Value) -> Vec<u8> {
    array(&v["_raw"])
        .iter()
        .map(|x| x.as_u64().unwrap() as u8)
        .collect()
}
/// Create an evidence matrix from bounded JSON input.
pub fn evidence_matrix(raw: &[u8], limit: usize, task_id: Option<&str>) -> Result<Report> {
    let v = parse(raw, limit)?;
    Ok(Report {
        value: matrix(&v, limit, task_id)?,
        current: raw_time(&v),
        before: None,
    })
}
/// Explain canonical actions without executing them.
pub fn action_inbox(raw: &[u8], limit: usize, task_id: Option<&str>) -> Result<Report> {
    let v = parse(raw, limit)?;
    Ok(Report {
        value: inbox(&v, limit, task_id)?,
        current: raw_time(&v),
        before: None,
    })
}
/// Compare snapshots without inferring intermediate events.
pub fn return_brief(current: &[u8], before: Option<&[u8]>, limit: usize) -> Result<Report> {
    let now = parse(current, limit)?;
    let mut r = base("oms-return-brief", &now);
    r["baseline"] = json!("absent");
    r["history_complete"] = json!(false);
    r["current"] = matrix(&now, limit, None)?;
    r["inbox"] = inbox(&now, limit, None)?;
    r["limitation"] = json!(
        "Snapshot differences, not event history. Repeated states and intermediate failures cannot be reconstructed."
    );
    let Some(before) = before else {
        return Ok(Report {
            value: r,
            current: raw_time(&now),
            before: None,
        });
    };
    let old = parse(before, limit)?;
    let mut reasons = Vec::new();
    if old["_instant"].as_i64() > now["_instant"].as_i64() {
        reasons.push("baseline-in-future")
    }
    if !s(&old["plan_id"]).is_empty() || !s(&now["plan_id"]).is_empty() {
        if s(&old["plan_id"]).is_empty() || old["plan_id"] != now["plan_id"] {
            reasons.push("plan-lineage-changed")
        }
    } else if s(&old["active_task_id"]).is_empty() || old["active_task_id"] != now["active_task_id"]
    {
        reasons.push("task-lineage-unavailable-or-changed")
    }
    if old["active_task_id"] != now["active_task_id"] {
        reasons.push("active-task-changed")
    }
    let sources = |v: &Value| {
        array(&v["sources"])
            .iter()
            .map(|x| (s(&x["path"]).to_owned(), x["sha256"].clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let left = sources(&old);
    let right = sources(&now);
    let mut sc = Vec::new();
    for path in left.keys().chain(right.keys()).collect::<BTreeSet<_>>() {
        if left.get(path) != right.get(path) {
            sc.push(json!({"path":path,"before":left.get(path),"after":right.get(path)}))
        }
    }
    r["baseline"] = json!("compared");
    r["baseline_at"] = old["generated_at"].clone();
    r["baseline_state_digest"] = old["state_digest"].clone();
    r["recorded_scope_matches"] = json!(reasons.is_empty());
    r["mismatch_reasons"] = json!(reasons);
    r["head_changed"] = json!(old["head"] != now["head"]);
    r["source_changes"] = bounded(sc, limit);
    r["same_snapshot_digest"] = json!(old["state_digest"] == now["state_digest"]);
    let mut changes = json!({});
    for (kind, key) in [("tasks", "state"), ("criteria", "status")] {
        let map = |v: &Value| {
            array(&v[kind])
                .iter()
                .map(|x| (s(&x["id"]).to_owned(), x.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let l = map(&old);
        let n = map(&now);
        let added = n
            .keys()
            .filter(|k| !l.contains_key(*k))
            .map(|x| json!(x))
            .collect();
        let removed = l
            .keys()
            .filter(|k| !n.contains_key(*k))
            .map(|x| json!(x))
            .collect();
        let mut updated = Vec::new();
        if reasons.is_empty() {
            for (id, a) in &l {
                if let Some(b) = n.get(id) {
                    let fields = [key, "title", "text", "verify_digest", "evidence"]
                        .into_iter()
                        .filter(|f| a.get(*f) != b.get(*f))
                        .collect::<Vec<_>>();
                    if !fields.is_empty() {
                        updated.push(
                            json!({"id":id,"before":a[key],"after":b[key],"changed_fields":fields}),
                        )
                    }
                }
            }
        }
        changes[kind] = json!({"transitions_suppressed":!reasons.is_empty(),"added":bounded(added,limit),"removed":bounded(removed,limit),"changed":bounded(updated,limit)});
    }
    r["changes"] = changes;
    Ok(Report {
        value: r,
        current: raw_time(&now),
        before: Some(raw_time(&old)),
    })
}
/// Render a report produced by this crate, including its final newline.
pub fn render(r: &Report) -> Result<String> {
    render_inner(&r.value)
}
fn render_inner(r: &Value) -> Result<String> {
    let mut lines = vec![
        s(&r["kind"]).to_owned(),
        "read-only; no approval or verification is created".into(),
        s(&r["note"]).into(),
    ];
    match s(&r["kind"]) {
        "oms-evidence-matrix" => {
            let rows = &r["criteria"];
            lines.push(format!(
                "criteria: {} shown / {}; {} omitted",
                array(&rows["items"]).len(),
                rows["total"],
                rows["omitted"]
            ));
            for row in array(&rows["items"]) {
                lines.push(format!(
                    "{} [{}] task={} link={}",
                    s(&row["criterion_id"]),
                    s(&row["status"]),
                    row["task_id"].as_str().unwrap_or("unknown"),
                    s(&row["link"])
                ));
                lines.push(format!("  {}", s(&row["criterion"])));
                for e in array(&row["evidence"]["items"]) {
                    let er = s(&e["evidence_ref"]);
                    lines.push(format!(
                        "  evidence: {} [{}] {}",
                        if er.is_empty() { "unrecorded" } else { er },
                        s(&e["status"]),
                        s(&e["source"])
                    ))
                }
                if row["evidence"]["omitted"] != 0 {
                    lines.push(format!(
                        "  evidence omitted: {}",
                        row["evidence"]["omitted"]
                    ))
                }
            }
            let a = &r["tasks_without_criterion_link"];
            lines.push(format!(
                "tasks without recorded link: {} ({} shown)",
                a["total"],
                array(&a["items"]).len()
            ));
            for t in array(&a["items"]) {
                lines.push(format!("  {}", s(t)))
            }
        }
        "oms-action-inbox" => {
            for row in array(&r["tasks"]["items"]) {
                lines.push(format!(
                    "{} [{}]: {}",
                    s(&row["task_id"]),
                    s(&row["state"]),
                    s(&row["title"])
                ));
                for reason in array(&row["reasons"]) {
                    lines.push(format!(
                        "  {}: {}",
                        s(&reason["code"]),
                        s(&reason["detail"])
                    ))
                }
                for d in array(&row["depends_waiting"]["items"]) {
                    lines.push(format!(
                        "  waits: {} [{}]",
                        s(&d["task_id"]),
                        s(&d["state"])
                    ))
                }
                if row["depends_waiting"]["omitted"] != 0 {
                    lines.push(format!(
                        "  waiting dependencies omitted: {}",
                        row["depends_waiting"]["omitted"]
                    ))
                }
            }
            lines.push(format!("tasks omitted: {}", r["tasks"]["omitted"]));
            lines.push("Canonical suggested actions (not executed):".into());
            for a in array(&r["actions"]["items"]) {
                lines.push(format!(
                    "  {} [{}]: {} -> {}",
                    s(&a["id"]),
                    s(&a["authority"]),
                    s(&a["reason"]),
                    s(&a["command"])
                ))
            }
            lines.push(format!("actions omitted: {}", r["actions"]["omitted"]))
        }
        "oms-return-brief" => {
            lines.push(format!(
                "baseline: {}; event history: incomplete",
                s(&r["baseline"])
            ));
            if r["baseline"] != "absent" {
                let scope = if r["recorded_scope_matches"] == true {
                    "same recorded lineage".to_owned()
                } else {
                    array(&r["mismatch_reasons"])
                        .iter()
                        .map(s)
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                lines.push(format!("scope: {scope}"));
                for label in ["tasks", "criteria"] {
                    let c = &r["changes"][label];
                    lines.push(format!(
                        "{}{}",
                        label,
                        if c["transitions_suppressed"] == true {
                            " (transitions suppressed)"
                        } else {
                            ""
                        }
                    ));
                    for direction in ["added", "removed"] {
                        let rows = &c[direction];
                        let items = array(&rows["items"])
                            .iter()
                            .map(s)
                            .collect::<Vec<_>>()
                            .join(", ");
                        lines.push(format!(
                            "  {}: {}; {} omitted",
                            direction,
                            if items.is_empty() { "none" } else { &items },
                            rows["omitted"]
                        ))
                    }
                    for row in array(&c["changed"]["items"]) {
                        lines.push(format!(
                            "  {}: {} -> {} ({})",
                            s(&row["id"]),
                            s(&row["before"]),
                            s(&row["after"]),
                            array(&row["changed_fields"])
                                .iter()
                                .map(s)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                    lines.push(format!("  changed omitted: {}", c["changed"]["omitted"]))
                }
                for row in array(&r["source_changes"]["items"]) {
                    lines.push(format!("source changed: {}", s(&row["path"])))
                }
                lines.push(format!(
                    "source changes omitted: {}",
                    r["source_changes"]["omitted"]
                ))
            }
            lines.push(String::new());
            lines.push(render_inner(&r["current"])?);
            lines.push(String::new());
            lines.push(render_inner(&r["inbox"])?)
        }
        _ => return Err(err("unsupported work report kind")),
    }
    lines.push(
        r.get("limitation")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
    );
    Ok(lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_limitation_is_empty() {
        let r = json!({"kind":"oms-evidence-matrix", "note":"note", "criteria":{"total":0,"omitted":0,"items":[]}, "tasks_without_criterion_link":{"total":0,"items":[]}});
        assert!(render_inner(&r).unwrap().ends_with("0 (0 shown)\n\n"));
    }
}
