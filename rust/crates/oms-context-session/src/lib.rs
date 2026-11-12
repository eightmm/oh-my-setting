#![forbid(unsafe_code)]

use oms_context::{Json, SessionJsonError, parse_session_json};
use oms_json::{PyString, Value as PyValue};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Failure to validate input or to process a row without losing information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A supplied option or input size is invalid.
    Invalid(&'static str),
    /// The parser or encoder could not process a valid row.
    Infrastructure,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) => f.write_str(s),
            Self::Infrastructure => f.write_str("session projection infrastructure failure"),
        }
    }
}
impl std::error::Error for Error {}
impl From<&'static str> for Error {
    fn from(s: &'static str) -> Self {
        Self::Invalid(s)
    }
}
static NULL: Json = Json::Null;
fn field<'a>(v: &'a Json, name: &str) -> &'a Json {
    match v {
        Json::Object(items) => items
            .iter()
            .find(|(k, _)| k == name.as_bytes())
            .map_or(&NULL, |(_, v)| v),
        _ => &NULL,
    }
}
fn array(v: &Json) -> Option<&[Json]> {
    if let Json::Array(a) = v {
        Some(a)
    } else {
        None
    }
}
fn bytes(v: &Json) -> Option<&[u8]> {
    if let Json::String(s) = v {
        Some(s)
    } else {
        None
    }
}
fn text(v: &Json) -> Option<&str> {
    std::str::from_utf8(bytes(v)?).ok()
}
fn is_text(v: &Json, s: &str) -> bool {
    bytes(v) == Some(s.as_bytes())
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => n.parse::<f64>() != Ok(0.0),
        Json::String(s) => !s.is_empty(),
        Json::Array(a) => !a.is_empty(),
        Json::Object(o) => !o.is_empty(),
    }
}
fn py_string(b: &[u8]) -> PyString {
    let mut points = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let first = b[i];
        let (mut n, len) = if first < 128 {
            (first as u32, 1)
        } else if first < 224 {
            ((first & 31) as u32, 2)
        } else if first < 240 {
            ((first & 15) as u32, 3)
        } else {
            ((first & 7) as u32, 4)
        };
        for &part in &b[i + 1..i + len] {
            n = (n << 6) | (part & 63) as u32;
        }
        points.push(n);
        i += len;
    }
    PyString::from_codepoints(points).expect("parser emits valid code points")
}
fn codec(root: &Json) -> Result<PyValue, Error> {
    enum Frame<'a> {
        Node(&'a Json),
        Array(usize),
        Object(Vec<PyString>),
    }
    let mut frames = vec![Frame::Node(root)];
    let mut values = Vec::new();
    while let Some(frame) = frames.pop() {
        match frame {
            Frame::Node(v) => match v {
                Json::Null => values.push(PyValue::Null),
                Json::Bool(b) => values.push(PyValue::Bool(*b)),
                Json::String(s) => values.push(PyValue::String(py_string(s))),
                Json::Number(n)
                    if n.contains(['.', 'e', 'E'])
                        || matches!(n.as_str(), "NaN" | "Infinity" | "-Infinity") =>
                {
                    values.push(PyValue::Float(
                        n.parse().map_err(|_| Error::Infrastructure)?,
                    ))
                }
                Json::Number(n) => values.push(
                    PyValue::integer(if n == "-0" { "0" } else { n })
                        .map_err(|_| Error::Infrastructure)?,
                ),
                Json::Array(a) => {
                    frames.push(Frame::Array(a.len()));
                    frames.extend(a.iter().rev().map(Frame::Node));
                }
                Json::Object(o) => {
                    frames.push(Frame::Object(o.iter().map(|(k, _)| py_string(k)).collect()));
                    frames.extend(o.iter().rev().map(|(_, v)| Frame::Node(v)));
                }
            },
            Frame::Array(len) => {
                let items = values.split_off(values.len() - len);
                values.push(PyValue::Array(items));
            }
            Frame::Object(keys) => {
                let items = values.split_off(values.len() - keys.len());
                values.push(PyValue::Object(keys.into_iter().zip(items).collect()));
            }
        }
    }
    values.pop().ok_or(Error::Infrastructure)
}
fn read_row(line: &[u8]) -> Result<Option<Json>, Error> {
    match parse_session_json(line) {
        Ok(v @ Json::Object(_)) => Ok(Some(v)),
        Ok(_) | Err(SessionJsonError::Invalid | SessionJsonError::Limit) => Ok(None),
        Err(SessionJsonError::Infrastructure) => Err(Error::Infrastructure),
    }
}

pub const MAX_LEDGER_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_KEYS: usize = 4096;

#[derive(Default)]
struct Aggregate {
    calls: u64,
    bytes: u64,
    largest: u64,
}

fn label(value: &Json, limit: usize) -> Option<&str> {
    use unicode_general_category::{GeneralCategory as C, get_general_category};
    let text = text(value)?;
    if text.is_empty() || text.chars().count() > limit {
        return None;
    }
    text.chars()
        .all(|c| {
            matches!(
                get_general_category(c),
                C::UppercaseLetter
                    | C::LowercaseLetter
                    | C::TitlecaseLetter
                    | C::ModifierLetter
                    | C::OtherLetter
                    | C::DecimalNumber
                    | C::LetterNumber
                    | C::OtherNumber
            ) || "_.:+() /-".contains(c)
        })
        .then_some(text)
}

fn add(table: &mut BTreeMap<String, Aggregate>, key: &str, size: u64) {
    let key = if !table.contains_key(key) && table.len() >= MAX_KEYS {
        "other"
    } else {
        key
    };
    let item = table.entry(key.to_owned()).or_default();
    item.calls += 1;
    item.bytes += size;
    item.largest = item.largest.max(size);
}

fn ranked(table: &BTreeMap<String, Aggregate>, name: &str, limit: usize) -> Value {
    let mut entries: Vec<_> = table.iter().collect();
    entries.sort_by(|a, b| b.1.bytes.cmp(&a.1.bytes).then_with(|| a.0.cmp(b.0)));
    let items: Vec<Value> = entries
        .iter()
        .take(limit)
        .map(|(key, v)| {
            let mut row = json!({"calls": v.calls, "bytes": v.bytes, "largest": v.largest});
            row[name] = json!(key);
            row
        })
        .collect();
    json!({"total": entries.len(), "omitted": entries.len().saturating_sub(limit), "items": items})
}

/// Project a caller-acquired bounded hook ledger without returning injected text.
/// An absent ledger is represented by an empty slice; acquisition errors belong to the adapter.
/// Returns `Error::Invalid` for invalid options or oversize input and `Error::Infrastructure`
/// if the native parser fails internally.
pub fn inspect_injections(
    data: &[u8],
    session: Option<&str>,
    limit: usize,
) -> Result<Value, Error> {
    if !(1..=100).contains(&limit) {
        return Err(Error::Invalid("--limit must be between 1 and 100"));
    }
    if session.is_some_and(|s| label(&Json::String(s.as_bytes().to_vec()), 64).is_none()) {
        return Err(Error::Invalid(
            "--session-hash must be a recorded session hash",
        ));
    }
    if data.len() > MAX_LEDGER_BYTES {
        return Err(Error::Invalid(
            "hook event ledger is larger than 67108864 bytes",
        ));
    }
    let (mut kinds, mut sessions, mut sources) =
        (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let (mut rows, mut invalid) = (0_u64, 0_u64);
    for line in data.split_inclusive(|b| *b == b'\n') {
        let Some(row) = read_row(line)? else {
            invalid += 1;
            continue;
        };
        if !is_text(field(&row, "action"), "injection") {
            continue;
        }
        let kind = label(field(&row, "status"), 40);
        let size = match field(&row, "bytes") {
            Json::Number(n) if !n.contains(['.', 'e', 'E']) => n.parse::<u64>().ok(),
            _ => None,
        }
        .filter(|n| *n > 0 && *n <= (1_u64 << 40));
        let (Some(kind), Some(size)) = (kind, size) else {
            invalid += 1;
            continue;
        };
        let owner = label(field(&row, "session"), 64);
        if session.is_some() && owner != session {
            continue;
        }
        rows += 1;
        add(&mut kinds, kind, size);
        add(&mut sessions, owner.unwrap_or("unknown"), size);
        add(
            &mut sources,
            label(field(&row, "source"), 20).unwrap_or("unrecorded"),
            size,
        );
    }
    Ok(json!({
        "schema": 1, "kind": "oms-context-injections", "unit": "utf-8 bytes",
        "ledger": ".oms/hooks/events.jsonl", "session": session,
        "injections": rows, "invalid_rows": invalid,
        "bytes": kinds.values().map(|v| v.bytes).sum::<u64>(),
        "by_hook": ranked(&kinds, "hook", limit),
        "by_session_start_source": ranked(&sources, "source", limit),
        "by_session": ranked(&sessions, "session", limit),
        "not_counted": ["skill-router state hints", "fail-ledger hook advice", "syntax-guard hook findings", "git-move-guard refusals", "provider-native context (rules, skills, tool output)"],
        "note": "Bytes OMS hooks printed for the model, recorded without their text. Not tokens, not a bill and not the provider's context window; rows older than the hook ledger's retention are gone."
    }))
}

pub const MAX_TRANSCRIPT_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_STEPS: usize = 20_000;
pub const MAX_TIMELINE_ROWS: usize = 200;

fn transcript_rows(
    data: &[u8],
) -> Result<impl Iterator<Item = Result<Option<Json>, Error>> + '_, Error> {
    if data.len() > MAX_TRANSCRIPT_BYTES {
        return Err(Error::Invalid("transcript is larger than 268435456 bytes"));
    }
    Ok(data.split_inclusive(|b| *b == b'\n').map(read_row))
}
fn result_bytes(value: &Json) -> usize {
    if let Some(s) = bytes(value) {
        return py_string(s).utf8_with_replacement_len();
    }
    array(value).map_or(0, |items| {
        items
            .iter()
            .map(|v| {
                bytes(v)
                    .or_else(|| bytes(field(v, "text")))
                    .map_or(0, |s| py_string(s).utf8_with_replacement_len())
            })
            .sum()
    })
}

fn safe_relative_label(label: &str) -> bool {
    !label.is_empty()
        && !label.starts_with('/')
        && !label.chars().any(|c| c < ' ')
        && !label.split('/').any(|part| part == "..")
}

/// Account for transcript results. `resolve` must return the realpath-checked repository-relative
/// display label, or None for invalid/outside paths. The caller must resolve symlinks and check
/// containment; this function additionally rejects unsafe lexical labels returned by the caller.
/// Returns `Error::Invalid` for invalid options or oversize input and `Error::Infrastructure`
/// if the native parser fails internally.
pub fn inspect_session<F>(data: &[u8], limit: usize, resolve: F) -> Result<Value, Error>
where
    F: Fn(&str) -> Option<String>,
{
    if !(1..=100).contains(&limit) {
        return Err(Error::Invalid("--limit must be between 1 and 100"));
    }
    let mut calls: BTreeMap<Vec<u8>, String> = BTreeMap::new();
    let mut tools: BTreeMap<String, Aggregate> = BTreeMap::new();
    let mut reads: BTreeMap<String, usize> = BTreeMap::new();
    let (mut outside, mut lines, mut unread, mut results, mut unmatched) = (0, 0, 0, 0, 0);
    for row in transcript_rows(data)? {
        lines += 1;
        let Some(row) = row? else {
            unread += 1;
            continue;
        };
        let Some(content) = array(field(field(&row, "message"), "content")) else {
            continue;
        };
        for item in content {
            if is_text(field(item, "type"), "tool_use")
                && bytes(field(item, "id")).is_some()
                && calls.len() < (1 << 20)
            {
                let name = label(field(item, "name"), 80).unwrap_or("unnamed");
                calls.insert(
                    bytes(field(item, "id")).unwrap().to_owned(),
                    name.to_owned(),
                );
                if name == "Read" || name == "NotebookRead" {
                    let target = field(item, "input");
                    let file = field(target, "file_path");
                    let raw = text(if truthy(file) {
                        file
                    } else {
                        field(target, "notebook_path")
                    });
                    let path = raw
                        .filter(|s| {
                            !s.is_empty()
                                && s.chars().count() <= 4096
                                && !s.chars().any(|c| c < ' ')
                        })
                        .and_then(&resolve)
                        .filter(|s| safe_relative_label(s));
                    if let Some(path) = path {
                        if reads.contains_key(&path) || reads.len() < MAX_KEYS {
                            *reads.entry(path).or_default() += 1;
                        }
                    } else {
                        outside += 1;
                    }
                }
            } else if is_text(field(item, "type"), "tool_result") {
                results += 1;
                let name = bytes(field(item, "tool_use_id"))
                    .and_then(|id| calls.get(id))
                    .map(String::as_str)
                    .unwrap_or_else(|| {
                        unmatched += 1;
                        "unmatched"
                    });
                add(
                    &mut tools,
                    name,
                    result_bytes(field(item, "content")) as u64,
                );
            }
        }
    }
    let mut repeated: Vec<_> = reads.iter().filter(|(_, n)| **n > 1).collect();
    repeated.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let repeated_items: Vec<_> = repeated
        .iter()
        .take(limit)
        .map(|(path, count)| json!({"path":path,"reads":count}))
        .collect();
    let mut by_tool = ranked(&tools, "tool", limit);
    for item in by_tool["items"].as_array_mut().unwrap() {
        let calls = item.as_object_mut().unwrap().remove("calls").unwrap();
        item["results"] = calls;
    }
    Ok(
        json!({"schema":1,"kind":"oms-context-session","unit":"utf-8 bytes",
        "format":if !calls.is_empty() || results > 0 {"claude-jsonl"} else {"unknown"},
        "lines":lines,"unread_lines":unread,"tool_calls":calls.len(),"tool_results":results,"unmatched_results":unmatched,
        "result_bytes":tools.values().map(|v|v.bytes).sum::<u64>(),"by_tool":by_tool,
        "file_reads":{"distinct_in_repository":reads.len(),"outside_repository":outside,"repeated":{"total":repeated.len(),"omitted":repeated.len().saturating_sub(limit),"items":repeated_items}},
        "note":"Tool-result text as it was recorded, in bytes; the provider may have cleared or summarized it since. Not tokens, not a bill and not the current context window. Only Claude Code JSONL transcripts are read; any other file reports format unknown."}),
    )
}

fn iso_clock(text: &str, offset: bool) -> Option<i64> {
    let (whole, fraction) = text.split_once(['.', ',']).unwrap_or((text, ""));
    let parts: Vec<&str> = if whole.contains(':') {
        whole.split(':').collect()
    } else {
        whole
            .as_bytes()
            .chunks(2)
            .map(std::str::from_utf8)
            .collect::<Result<_, _>>()
            .ok()?
    };
    let hours: i64 = parts.first()?.parse().ok()?;
    let minutes: i64 = parts.get(1).unwrap_or(&"0").parse().ok()?;
    let seconds: i64 = parts.get(2).unwrap_or(&"0").parse().ok()?;
    if !offset && (hours > 23 || minutes > 59 || seconds > 59) {
        return None;
    }
    let mut micros = 0_i64;
    for digit in fraction.bytes().take(6) {
        micros = micros * 10 + i64::from(digit - b'0');
    }
    for _ in fraction.len().min(6)..6 {
        micros *= 10;
    }
    Some(((hours * 60 + minutes) * 60 + seconds) * 1_000_000 + micros)
}

// Python divides the integer microsecond count before rounding to binary64.
fn epoch_seconds(micros: i64) -> f64 {
    if micros == 0 {
        return 0.0;
    }
    let n = i128::from(micros).abs();
    let at_least = |e: i32| {
        if e >= 0 {
            n >= (1_000_000_i128 << e)
        } else {
            (n << -e) >= 1_000_000
        }
    };
    let mut exponent = -20;
    while at_least(exponent + 1) {
        exponent += 1;
    }
    let numerator = n << (52 - exponent);
    let mut significand = numerator / 1_000_000;
    let remainder = numerator % 1_000_000;
    if remainder > 500_000 || (remainder == 500_000 && significand % 2 != 0) {
        significand += 1;
    }
    let value = significand as f64 * 2_f64.powi(exponent - 52);
    if micros < 0 { -value } else { value }
}

fn when(value: &Json) -> Option<f64> {
    use chrono::{Datelike, NaiveDate, Weekday};
    use regex::Regex;
    use std::sync::OnceLock;
    static ISO: OnceLock<Regex> = OnceLock::new();
    let s = text(value)?;
    if !(10..=40).contains(&s.chars().count()) {
        return None;
    }
    let replaced = s.replace('Z', "+00:00");
    let re = ISO.get_or_init(|| Regex::new(concat!(
        r"^(?P<date>[0-9]{4}-[0-9]{2}-[0-9]{2}|[0-9]{8}|[0-9]{4}-W[0-9]{2}(?:-[1-7])?|[0-9]{4}W[0-9]{2}[1-7]?)",
        r"(?:(?s:.)(?P<time>[0-9]{2}(?::[0-9]{2}(?::[0-9]{2})?|[0-9]{2}(?:[0-9]{2})?)?(?:[.,][0-9]+)?)",
        r"(?P<sep>[^0-9+\-])?(?P<zone>[+-][0-9]{2}(?::[0-9]{2}(?::[0-9]{2})?|[0-9]{2}(?:[0-9]{2})?)?(?:[.,][0-9]+)?)?)?\z"
    )).expect("static ISO expression"));
    let captures = re.captures(&replaced)?;
    let date = &captures["date"];
    let day = if date.contains('W') {
        let compact = date.replace('-', "");
        let year = compact[0..4].parse().ok()?;
        let week = compact[5..7].parse().ok()?;
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
        NaiveDate::from_isoywd_opt(year, week, weekday)?
    } else if date.contains('-') {
        NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?
    } else {
        NaiveDate::parse_from_str(date, "%Y%m%d").ok()?
    };
    if !(1..=9999).contains(&day.year()) {
        return None;
    }
    if captures.name("sep").is_some() && captures.name("zone").is_none() {
        return None;
    }
    if captures.name("sep").is_some()
        && captures
            .name("time")
            .is_some_and(|t| t.as_str().contains(['.', ',']))
    {
        return None;
    }
    let time = captures
        .name("time")
        .map_or(Some(0), |v| iso_clock(v.as_str(), false))?;
    let zone = match captures.name("zone").map(|v| v.as_str()) {
        None => 0,
        Some(z) => {
            let offset = iso_clock(&z[1..], true)?;
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
    let midnight = day.and_hms_opt(0, 0, 0)?.and_utc().timestamp_micros();
    let total = midnight + time - zone;
    Some(epoch_seconds(total))
}

/// Project bounded Claude JSONL calls and results without retaining argument or result bodies.
/// Returns `Error::Invalid` for invalid options or oversize input and `Error::Infrastructure`
/// if parsing or encoding cannot be completed.
pub fn inspect_timeline(data: &[u8], rows: usize) -> Result<Value, Error> {
    use sha2::{Digest, Sha256};
    if !(1..=MAX_TIMELINE_ROWS).contains(&rows) {
        return Err(Error::Invalid("timeline rows must be between 1 and 200"));
    }
    let mut steps: Vec<Value> = Vec::new();
    let mut stamps: Vec<Option<f64>> = Vec::new();
    let mut ids: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    let mut seen: BTreeMap<(String, Vec<u8>), usize> = BTreeMap::new();
    let (mut lines, mut unread, mut unmatched, mut dropped) = (0, 0, 0, 0);
    for row in transcript_rows(data)? {
        lines += 1;
        let Some(row) = row? else {
            unread += 1;
            continue;
        };
        let stamp = when(field(&row, "timestamp"));
        let Some(content) = array(field(field(&row, "message"), "content")) else {
            continue;
        };
        for item in content {
            if is_text(field(item, "type"), "tool_use") && bytes(field(item, "id")).is_some() {
                if steps.len() >= MAX_STEPS {
                    dropped += 1;
                    continue;
                }
                let name = label(field(item, "name"), 80).unwrap_or("unnamed");
                let args = oms_json::session_call_key_json_bytes(&codec(field(item, "input"))?)
                    .map_err(|_| Error::Infrastructure)?;
                let key = (name.to_owned(), Sha256::digest(&args).to_vec());
                let step = steps.len() + 1;
                let repeat = seen.get(&key).copied();
                seen.entry(key).or_insert(step);
                ids.insert(bytes(field(item, "id")).unwrap().to_owned(), step - 1);
                steps.push(json!({"step":step,"tool":name,"duration_ms":null,"ok":null,"error":false,"argument_bytes":args.len(),"result_bytes":null,"repeat_of":repeat}));
                stamps.push(stamp);
            } else if is_text(field(item, "type"), "tool_result") {
                let Some(index) = bytes(field(item, "tool_use_id"))
                    .and_then(|id| ids.get(id))
                    .copied()
                else {
                    unmatched += 1;
                    continue;
                };
                let failed = matches!(field(item, "is_error"), Json::Bool(true));
                steps[index]["ok"] = json!(!failed);
                steps[index]["error"] = json!(failed);
                steps[index]["result_bytes"] = json!(result_bytes(field(item, "content")));
                if let (Some(end), Some(start)) = (stamp, stamps[index])
                    && end >= start
                {
                    steps[index]["duration_ms"] =
                        json!(((end - start) * 1000.0).round_ties_even() as i64);
                }
            }
        }
    }
    let first = steps.iter().find(|s| s["error"] == true).cloned();
    let mut shown = steps.iter().take(rows).cloned().collect::<Vec<_>>();
    if let Some(first) = &first
        && first["step"].as_u64().unwrap() as usize > rows
    {
        shown[rows - 1] = first.clone();
    }
    Ok(
        json!({"schema":1,"kind":"oms-context-timeline","unit":"utf-8 bytes","format":if steps.is_empty(){"unknown"}else{"claude-jsonl"},
        "lines":lines,"unread_lines":unread,"unmatched_results":unmatched,"steps_total":steps.len(),"steps_not_recorded":dropped,
        "errors":steps.iter().filter(|s|s["error"] == true).count(),"pending":steps.iter().filter(|s|s["ok"].is_null()).count(),
        "steps_after_first_error":first.as_ref().map(|s|steps.len() - s["step"].as_u64().unwrap() as usize),"first_error":first,
        "steps_omitted":steps.len()-shown.len(),"steps":shown,
        "note":"Tool names, order, recorded timestamps and byte sizes only; arguments and outputs are not returned. `first_error` is the first result the provider flagged is_error, not a cause. Durations are the gap between the call row and its result row; null when not recorded. `repeat_of` names an earlier step with the same tool and identical arguments. Only Claude Code JSONL transcripts are read; any other file reports format unknown. Observation only, never verification or approval evidence."}),
    )
}
