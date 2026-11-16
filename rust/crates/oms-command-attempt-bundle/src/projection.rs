use crate::value::{V, add, obj};
use chrono::{NaiveDate, NaiveDateTime};
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

pub(crate) fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
fn state(value: &str) -> bool {
    matches!(
        value,
        "queued"
            | "starting"
            | "working"
            | "waiting_input"
            | "waiting_approval"
            | "verifying"
            | "review"
            | "blocked"
            | "done"
            | "failed"
            | "cancelled"
            | "timed_out"
            | "abandoned"
    )
}
fn terminal(value: &str) -> bool {
    matches!(
        value,
        "done" | "failed" | "cancelled" | "timed_out" | "abandoned"
    )
}
fn transition(old: &str, new: &str) -> bool {
    match old {
        "queued" => matches!(
            new,
            "starting" | "blocked" | "failed" | "timed_out" | "cancelled"
        ),
        "starting" => matches!(
            new,
            "working" | "blocked" | "failed" | "timed_out" | "cancelled"
        ),
        "working" => matches!(
            new,
            "waiting_input"
                | "waiting_approval"
                | "verifying"
                | "review"
                | "blocked"
                | "failed"
                | "timed_out"
                | "cancelled"
        ),
        "waiting_input" | "waiting_approval" => matches!(
            new,
            "working" | "blocked" | "failed" | "timed_out" | "cancelled"
        ),
        "verifying" => matches!(
            new,
            "review" | "working" | "blocked" | "failed" | "timed_out" | "cancelled"
        ),
        "review" => matches!(new, "done" | "working" | "blocked" | "failed" | "cancelled"),
        "blocked" => matches!(
            new,
            "working" | "failed" | "timed_out" | "cancelled" | "abandoned"
        ),
        _ => false,
    }
}

pub(crate) fn stamp(value: &V) -> Option<i64> {
    let text = value.text()?;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)^([0-9]{4})-([0-9]{1,2})-([0-9]{1,2})t([0-9]{1,2}):([0-9]{1,2}):([0-9]{1,2})z$",
        )
        .expect("constant timestamp regex")
    });
    let c = re.captures(text)?;
    let year: i32 = c[1].parse().ok()?;
    if year < 1 {
        return None;
    }
    NaiveDate::from_ymd_opt(year, c[2].parse().ok()?, c[3].parse().ok()?)?
        .and_hms_opt(c[4].parse().ok()?, c[5].parse().ok()?, c[6].parse().ok()?)
        .map(|v: NaiveDateTime| v.and_utc().timestamp())
}
fn safe(value: &V, label: &str) -> Result<(), String> {
    if id(&value.py()) {
        Ok(())
    } else {
        Err(format!(
            "{label} must match [A-Za-z0-9][A-Za-z0-9._:-]{{0,159}}"
        ))
    }
}
fn identity(v: &V) -> String {
    match v {
        V::Str(s) => format!("s:{}", String::from_utf8_lossy(s)),
        V::Null => "null".into(),
        V::Bool(b) => format!("n:{}", usize::from(*b)),
        V::Num(n) => {
            if n.contains(['.', 'e', 'E']) {
                let f = n.parse::<f64>().unwrap_or(f64::NAN);
                if f.is_finite() && f.fract() == 0.0 {
                    format!("n:{f:.0}")
                } else {
                    format!("n:{f}")
                }
            } else {
                format!("n:{n}")
            }
        }
        _ => format!("invalid:{}", v.py()),
    }
}
fn hex(v: &V, len: usize, nonzero: bool) -> bool {
    v.text().is_some_and(|s| {
        s.len() == len
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && (!nonzero || s.bytes().any(|b| b != b'0'))
    })
}
fn validate(row: &V, number: usize) -> Result<(), String> {
    let w = format!("event row {number}");
    if identity(row.get("schema")) != "n:1" {
        return Err(format!("{w} has unsupported schema"));
    }
    for key in ["event_id", "attempt_id"] {
        safe(&row.default(key, ""), key)?;
    }
    if stamp(row.get("ts")).is_none() {
        return Err(format!(
            "invalid UTC timestamp: {}",
            row.default("ts", "").py()
        ));
    }
    if !row.get("seq").integer(true) {
        return Err(format!("{w} has invalid seq"));
    }
    let kind = row.get("event_type").text().unwrap_or("");
    if !matches!(
        kind,
        "attempt.created" | "attempt.state_changed" | "attempt.heartbeat" | "attempt.usage"
    ) {
        return Err(format!("{w} has invalid event_type"));
    }
    for key in [
        "run_id",
        "task_id",
        "parent_attempt_id",
        "provider",
        "tool",
        "idempotency_key",
    ] {
        if row.get(key).truth() {
            safe(row.get(key), key)?;
        }
    }
    if row.get("reason_code").truth() {
        let s = row.get("reason_code").py();
        let b = s.as_bytes();
        if b.is_empty()
            || b.len() > 64
            || !(b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
            || !b
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._:-".contains(b))
        {
            return Err("reason_code must match [a-z0-9][a-z0-9._:-]{0,63}".into());
        }
    }
    for key in ["from_state", "to_state"] {
        if row.get(key) != &V::Null && !row.get(key).text().is_some_and(state) {
            return Err(format!("{w} has invalid {key}"));
        }
    }
    if row.has("expected_state")
        && (kind != "attempt.state_changed" || !row.get("expected_state").text().is_some_and(state))
    {
        return Err(format!("{w} has invalid expected_state"));
    }
    if row.has("expected_seq")
        && (kind != "attempt.state_changed" || !row.get("expected_seq").integer(true))
    {
        return Err(format!("{w} has invalid expected_seq"));
    }
    let usage = row.get("usage");
    if usage.truth() {
        if !matches!(usage, V::Obj(_)) {
            return Err(format!("{w} usage is not an object"));
        }
        for k in ["tokens", "duration_ms", "cost_microusd"] {
            if !usage.default(k, 0usize).integer(false) {
                return Err(format!("{w} has invalid usage.{k}"));
            }
        }
    }
    let refs = row.get("refs");
    if refs.truth() {
        let V::Obj(items) = refs else {
            return Err(format!("{w} refs is not an object"));
        };
        for (k, v) in items {
            safe(&V::Str(k.clone()), "ref name")?;
            let Some(bytes) = v.bytes() else {
                return Err(format!(
                    "{w} has invalid ref {}",
                    String::from_utf8_lossy(k)
                ));
            };
            let count = bytes.iter().filter(|b| **b & 0xc0 != 0x80).count();
            if count > 240 || bytes.contains(&b'\n') || bytes.contains(&b'\r') {
                return Err(format!(
                    "{w} has invalid ref {}",
                    String::from_utf8_lossy(k)
                ));
            }
            if bytes.starts_with(b"/")
                || bytes.starts_with(b"\\")
                || (bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'/' | b'\\'))
                || bytes.split(|b| *b == b'/').any(|p| p == b"..")
            {
                return Err(format!("{w} contains an absolute or escaping ref"));
            }
        }
    }
    if ["trace_id", "span_id", "parent_span_id", "trace_flags"]
        .iter()
        .any(|k| row.has(k))
        && (!hex(row.get("trace_id"), 32, true)
            || !hex(row.get("span_id"), 16, true)
            || !hex(&row.default("trace_flags", "00"), 2, false)
            || (row.get("parent_span_id") != &V::Null && !hex(row.get("parent_span_id"), 16, true)))
    {
        return Err(format!("{w} has invalid persisted trace context"));
    }
    if ["traceparent", "tracestate", "baggage"]
        .iter()
        .any(|k| row.has(k))
    {
        return Err(format!("{w} stores forbidden raw trace context"));
    }
    Ok(())
}
fn dictionary(value: &V) -> Result<V, String> {
    match value {
        V::Obj(_) => Ok(value.clone()),
        V::Str(s) if s.is_empty() => Ok(obj([])),
        V::Arr(a) => {
            let mut map = BTreeMap::new();
            for v in a {
                let V::Arr(pair) = v else {
                    return Err(
                        "cannot convert dictionary update sequence element to a sequence".into(),
                    );
                };
                if pair.len() != 2 {
                    return Err("dictionary update sequence element has invalid length".into());
                }
                let Some(key) = pair[0].bytes() else {
                    return Err("dictionary key is not a string".into());
                };
                map.insert(key.to_vec(), pair[1].clone());
            }
            Ok(V::Obj(map))
        }
        _ => Err("value is not a dictionary".into()),
    }
}
/// Validate every event before the caller selects an attempt. Values retain their Python JSON types.
pub(crate) fn project_values(rows: &[V]) -> Result<BTreeMap<String, V>, String> {
    let mut attempts: BTreeMap<String, V> = BTreeMap::new();
    let mut events = HashSet::new();
    let mut keys = HashSet::new();
    for (index, row) in rows.iter().enumerate() {
        validate(row, index + 1)?;
        let event = row.get("event_id");
        if !events.insert(identity(event)) {
            return Err(format!("duplicate lifecycle event_id: {}", event.py()));
        }
        let raw = row.get("attempt_id");
        let key = identity(raw);
        let current = attempts.get_mut(&key);
        let expected = current
            .as_ref()
            .map_or("1".into(), |v| add(&v.get("sequence").py(), "1"));
        if row.get("seq").py() != expected {
            return Err(format!(
                "attempt {} sequence is {}, expected {expected}",
                raw.py(),
                row.get("seq").py()
            ));
        }
        let idem = row.get("idempotency_key");
        if idem.truth() && !keys.insert((key.clone(), identity(idem))) {
            return Err(format!(
                "duplicate idempotency key was appended: {}",
                idem.py()
            ));
        }
        let kind = row.get("event_type").text().unwrap_or("");
        if kind == "attempt.created" {
            if current.is_some() {
                return Err(format!("attempt {} was created twice", raw.py()));
            }
            if row.get("from_state") != &V::Null || row.get("to_state").text() != Some("queued") {
                return Err("attempt.created must transition null -> queued".into());
            }
            let zero = obj([
                ("tokens", 0usize.into()),
                ("duration_ms", 0usize.into()),
                ("cost_microusd", 0usize.into()),
            ]);
            let mut attempt = obj([
                ("schema", 1usize.into()),
                ("attempt_id", raw.clone()),
                ("state", "queued".into()),
                ("sequence", row.get("seq").clone()),
                ("created_at", row.get("ts").clone()),
                ("updated_at", row.get("ts").clone()),
                ("last_heartbeat_at", row.get("ts").clone()),
                ("reason_code", row.default("reason_code", "")),
                ("usage", zero.clone()),
                ("usage_reports", zero),
                ("budget", dictionary(&row.default("budget", obj([])))?),
                ("refs", dictionary(&row.default("refs", obj([])))?),
                ("terminal", false.into()),
            ]);
            for k in [
                "run_id",
                "task_id",
                "parent_attempt_id",
                "provider",
                "tool",
                "base_sha",
            ] {
                attempt.set(k, row.default(k, ""));
            }
            attempts.insert(key, attempt);
            continue;
        }
        let a =
            current.ok_or_else(|| format!("event precedes attempt.created for {}", raw.py()))?;
        if kind == "attempt.state_changed" {
            let old = a.get("state").text().unwrap_or("");
            let new = row.get("to_state").text().unwrap_or("");
            if row.get("from_state").text() != Some(old) {
                return Err(format!(
                    "attempt {} from_state does not match projection",
                    raw.py()
                ));
            }
            if terminal(old) || !transition(old, new) {
                return Err(format!(
                    "illegal lifecycle transition: {old} -> {}",
                    row.get("to_state").py()
                ));
            }
            a.set("state", new);
            a.set("terminal", terminal(new));
            a.set("reason_code", row.default("reason_code", ""));
            if new == "review" {
                a.set("review_entered_at", row.get("ts").clone());
            }
        } else if kind == "attempt.usage" {
            let usage = row.default("usage", obj([]));
            let V::Obj(items) = usage else {
                return Err("usage is not an object".into());
            };
            for (k, value) in items {
                let k = std::str::from_utf8(&k).map_err(|_| "unknown usage key")?;
                if !a.get("usage").has(k) {
                    return Err(format!("unknown usage key: {k}"));
                }
                let old = a.get("usage").get(k).py();
                let n = add(&old, &value.py());
                let mut usage = a.get("usage").clone();
                usage.set(k, V::Num(n));
                a.set("usage", usage);
                let n = add(&a.get("usage_reports").get(k).py(), "1");
                let mut reports = a.get("usage_reports").clone();
                reports.set(k, V::Num(n));
                a.set("usage_reports", reports);
            }
        }
        a.set("sequence", row.get("seq").clone());
        a.set("updated_at", row.get("ts").clone());
        if matches!(kind, "attempt.heartbeat" | "attempt.state_changed") {
            a.set("last_heartbeat_at", row.get("ts").clone());
        }
        if let V::Obj(items) = row.get("refs") {
            let mut refs = a.get("refs").clone();
            if let V::Obj(ref mut map) = refs {
                map.extend(items.clone());
            }
            a.set("refs", refs);
        }
    }
    Ok(attempts)
}
pub(crate) fn select<'a>(attempts: &'a BTreeMap<String, V>, id: &str) -> Option<&'a V> {
    attempts.get(&format!("s:{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(extra: &str) -> V {
        V::parse(format!(r#"{{"schema":1,"event_id":"e1","attempt_id":"a1","seq":1,"ts":"2026-01-01T00:00:00Z","event_type":"attempt.created","from_state":null,"to_state":"queued"{extra}}}"#).as_bytes()).unwrap()
    }
    #[test]
    fn validates_unrelated_rows_and_every_field() {
        for extra in [
            r#", "refs":{"path":"../escape"}"#,
            r#", "refs":{"path":"/absolute"}"#,
            r#", "refs":{"path":"x\nline"}"#,
            r#", "usage":{"tokens":-1}"#,
            r#", "usage":{"tokens":true}"#,
            r#", "usage":{"duration_ms":0.0}"#,
            r#", "trace_id":"123""#,
            r#", "expected_seq":0"#,
            r#", "expected_state":"working""#,
            r#", "reason_code":"Bad""#,
            r#", "provider":"!""#,
            r#", "traceparent":null"#,
        ] {
            assert!(project_values(&[row(extra)]).is_err(), "accepted {extra}");
        }
        let mut bad = row("");
        bad.set("attempt_id", "other");
        bad.set("seq", 2usize);
        assert!(project_values(&[row(""), bad]).is_err());
    }
    #[test]
    fn usage_refs_and_projection_are_lossless() {
        let huge = "9".repeat(100);
        let created = row(r#", "refs":{"text":"상대/경로"},"budget":{"data":"retained"}"#);
        let mut usage = row("");
        usage.set("event_id", "e2");
        usage.set("event_type", "attempt.usage");
        usage.set("seq", 2usize);
        usage.set("usage", obj([("tokens", V::Num(huge.clone()))]));
        usage.set("refs", obj([("other", "pointer".into())]));
        let a = project_values(&[created, usage]).unwrap();
        let a = select(&a, "a1").unwrap();
        assert_eq!(a.get("usage").get("tokens"), &V::Num(huge));
        assert_eq!(a.get("usage_reports").get("tokens"), &V::Num("1".into()));
        assert_eq!(a.get("refs").get("text").text(), Some("상대/경로"));
        assert_eq!(a.get("refs").get("other").text(), Some("pointer"));
        assert_eq!(a.get("budget").get("data").text(), Some("retained"));
    }
    #[test]
    fn each_transition_is_enforced() {
        let states = [
            "queued",
            "starting",
            "working",
            "waiting_input",
            "waiting_approval",
            "verifying",
            "review",
            "blocked",
            "done",
            "failed",
            "cancelled",
            "timed_out",
            "abandoned",
        ];
        // Reach each nonterminal state with canonical transitions, then try every destination.
        for old in states {
            let path: Vec<&str> = match old {
                "queued" => vec![],
                "starting" => vec!["starting"],
                "working" => vec!["starting", "working"],
                "waiting_input" | "waiting_approval" | "verifying" | "review" => {
                    vec!["starting", "working", old]
                }
                "blocked" => vec!["blocked"],
                "done" => vec!["starting", "working", "review", "done"],
                _ => vec![old],
            };
            for new in states {
                let mut rows = vec![row("")];
                let mut previous = "queued";
                for (i, state) in path.iter().chain(std::iter::once(&new)).enumerate() {
                    let mut r = row("");
                    r.set("event_id", format!("e{}", i + 2));
                    r.set("seq", i + 2);
                    r.set("event_type", "attempt.state_changed");
                    r.set("from_state", previous);
                    r.set("to_state", *state);
                    rows.push(r);
                    previous = state;
                }
                assert_eq!(
                    project_values(&rows).is_ok(),
                    transition(old, new),
                    "{old} -> {new}"
                );
            }
        }
    }
    #[test]
    fn numeric_types_schema_and_legacy_timestamps() {
        let empty = row(r#", "refs":"", "budget":"""#);
        assert!(project_values(&[empty]).is_ok());
        for v in [V::Bool(true), V::Num("1.0".into()), V::Num("1e0".into())] {
            let mut r = row("");
            r.set("schema", v);
            assert!(project_values(&[r]).is_ok());
        }
        for seq in [
            V::Bool(true),
            V::Num("0".into()),
            V::Num("1.0".into()),
            V::Num("NaN".into()),
            V::Num("9".repeat(160)),
        ] {
            let mut r = row("");
            r.set("seq", seq);
            assert!(project_values(&[r]).is_err());
        }
        let mut r = row("");
        r.set("ts", "2026-1-1t0:0:0z");
        assert!(project_values(&[r]).is_ok());
        for ts in [
            "0000-01-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-01-01T24:00:00Z",
        ] {
            let mut r = row("");
            r.set("ts", ts);
            assert!(project_values(&[r]).is_err());
        }
    }
    #[test]
    fn ref_and_trace_edges() {
        for n in [0, 240] {
            let mut r = row("");
            r.set("refs", obj([("path", V::from("한".repeat(n)))]));
            assert!(project_values(&[r]).is_ok());
        }
        let mut r = row("");
        r.set("refs", obj([("path", V::from("x".repeat(241)))]));
        assert!(project_values(&[r]).is_err());
        let mut r = row("");
        r.set("trace_id", "1".repeat(32));
        r.set("span_id", "1".repeat(16));
        assert!(project_values(&[r.clone()]).is_ok());
        r.set("parent_span_id", V::Null);
        assert!(project_values(&[r.clone()]).is_ok());
        r.set("trace_flags", "00");
        assert!(project_values(&[r.clone()]).is_ok());
        r.set("trace_id", "0".repeat(32));
        assert!(project_values(&[r]).is_err());
    }
}
