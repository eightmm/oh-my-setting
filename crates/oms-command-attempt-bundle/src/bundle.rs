use crate::{
    projection::{id, stamp},
    source,
    value::{V, obj},
};
use oms_safe_fs::Root;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const EVENTS: &str = ".oms/lifecycle/events.jsonl";
const ARTIFACTS: &str = ".oms/artifacts/index.jsonl";
const FAILURES: &str = ".oms/failures.jsonl";
const NOTICE: &str = "Observation only: recorded facts and pointers, no commands, prompts, answers or tool output. Never verification, approval or a statement of cause.";
const ROW_FIELDS: &[&str] = &[
    "event_id",
    "ts",
    "kind",
    "provider",
    "exit",
    "verify_exit",
    "task_id",
    "run_id",
    "role",
    "model_class",
    "requested_model",
    "selected_model",
    "fallback_model",
    "fallback_used",
    "fallback_reason",
    "reasoning_effort",
    "answer_quality",
    "prompt_sha256",
    "prompt_bytes",
    "parent_event_id",
    "artifact",
    "patch",
    "patch_sha256",
    "tokens",
    "duration_s",
    "status",
    "covers",
];
fn names(refs: &V, limit: usize, width: usize) -> V {
    if let V::Obj(m) = refs {
        V::Arr(
            m.keys()
                .take(limit)
                .map(|k| V::Str(k.clone()).short(width))
                .collect(),
        )
    } else {
        V::Arr(vec![])
    }
}
fn lifecycle(rows: &[V], attempt: &V) -> V {
    let mine: Vec<_> = rows
        .iter()
        .filter(|r| r.get("attempt_id") == attempt.get("attempt_id"))
        .collect();
    let mut events = vec![];
    let mut previous = None;
    let mut gap = 0;
    for row in mine.iter().take(200) {
        if let Some(t) = stamp(row.get("ts")) {
            if let Some(p) = previous {
                gap = gap.max(t - p);
            }
            previous = Some(t);
        }
        let mut event = obj([]);
        for k in [
            "seq",
            "ts",
            "event_type",
            "from_state",
            "to_state",
            "reason_code",
            "event_id",
        ] {
            let v = row.get(k);
            if *v != V::Null && *v != V::from("") {
                event.set(k, v.clone());
            }
        }
        if matches!(row.get("refs"), V::Obj(_)) && row.get("refs").truth() {
            event.set("ref_names", names(row.get("refs"), 16, 60));
        }
        events.push(event);
    }
    let mut out = obj([
        ("source", EVENTS.into()),
        ("reason_code", attempt.get("reason_code").or(V::Null)),
        ("events_total", mine.len().into()),
        ("events_omitted", mine.len().saturating_sub(200).into()),
        ("longest_gap_s", gap.into()),
        ("events", V::Arr(events)),
        ("ref_names", names(attempt.get("refs"), 32, usize::MAX)),
    ]);
    for k in ["state", "terminal", "created_at", "updated_at", "usage"] {
        out.set(k, attempt.get(k).clone());
    }
    out
}
fn unreadable(source: &str) -> V {
    obj([
        ("source", source.into()),
        ("readable", false.into()),
        ("rows", V::Arr(vec![])),
    ])
}
fn section(source: &str, mut rows: Vec<V>) -> V {
    let total = rows.len();
    rows.truncate(50);
    obj([
        ("source", source.into()),
        ("readable", true.into()),
        ("rows_total", total.into()),
        ("rows_omitted", total.saturating_sub(50).into()),
        ("rows", V::Arr(rows)),
    ])
}
fn review(value: &V) -> V {
    if !matches!(value, V::Obj(_)) {
        return V::Null;
    }
    let seats = value.get("seats");
    let mut out = obj([(
        "seats",
        if let V::Arr(a) = seats {
            a.len().into()
        } else {
            V::Null
        },
    )]);
    for k in ["verdict", "outcome", "status", "decision"] {
        if matches!(value.get(k), V::Str(_)) {
            out.set(k, value.get(k).short(40));
        }
    }
    if let V::Arr(a) = seats {
        let mut verdicts = vec![];
        for seat in a.iter().take(8).filter(|s| matches!(s, V::Obj(_))) {
            for k in ["verdict", "outcome", "status"] {
                if *seat.get(k) != V::Null {
                    verdicts.push(V::from(seat.get(k).py()).short(40));
                }
            }
        }
        verdicts.truncate(16);
        out.set("seat_verdicts", V::Arr(verdicts));
    }
    out
}
fn artifacts(root: &Root, attempt: &V) -> V {
    let Ok(rows) = source::rows(root, Path::new(ARTIFACTS), false) else {
        return unreadable(ARTIFACTS);
    };
    let own: Vec<_> = rows
        .iter()
        .filter(|r| r.get("attempt_id") == attempt.get("attempt_id"))
        .collect();
    let ids: Vec<_> = own
        .iter()
        .map(|r| r.get("event_id"))
        .filter(|v| v.truth())
        .collect();
    let follow: Vec<_> = rows
        .iter()
        .filter(|r| {
            r.get("attempt_id") != attempt.get("attempt_id")
                && ids.contains(&r.get("parent_event_id"))
        })
        .collect();
    let mut shown = vec![];
    for (how, group) in [("attempt_id", own), ("answers parent_event_id", follow)] {
        for row in group {
            let mut item = obj([]);
            for k in ROW_FIELDS {
                if row.has(k) {
                    let v = row.get(k).short(200);
                    if v != V::Null {
                        item.set(k, v);
                    }
                }
            }
            item.set("linked_by", how);
            if row.get("result_lint").truth() {
                item.set("result_lint", row.get("result_lint").short(200));
            }
            let r = review(row.get("review"));
            if r != V::Null {
                item.set("review", r);
            }
            shown.push(item);
        }
    }
    shown.sort_by_key(|r| r.default("ts", "").py());
    section(ARTIFACTS, shown)
}
fn ledger_fingerprint(row: &V) -> Option<&str> {
    let key = if row.has("observed_fingerprint") {
        if row.get("event").text() != Some("verified") || row.has("fingerprint") {
            return None;
        }
        "observed_fingerprint"
    } else {
        "fingerprint"
    };
    row.get(key).text().filter(|s| !s.trim().is_empty())
}
fn reference_verification(row: &V) -> Option<V> {
    let evidence = row.get("verification");
    let digest = |value: &V| {
        value
            .text()
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    if !matches!(row.get("schema"), V::Num(schema) if schema == "2")
        || row.get("agent").text().is_none_or(|a| a.trim().is_empty())
        || row.get("ts").text().is_none_or(|ts| ts.trim().is_empty())
        || ledger_fingerprint(row).is_none()
        || evidence.get("scope").text() != Some("agent-task-reference-v1")
        || evidence.get("action").text() != Some("verification-rerun")
        || !evidence.get("active_task_id").text().is_some_and(id)
        || !["snapshot_attempt_id", "outcome_attempt_id"]
            .iter()
            .all(|k| evidence.get(k).text().is_some_and(id))
        || !["snapshot_sha256", "outcome_sha256", "command_sha256"]
            .iter()
            .all(|k| digest(evidence.get(k)))
        || !matches!(
            (row.get("event").text(), evidence.get("result").text()),
            (Some("fail"), Some("failed")) | (Some("resolved" | "verified"), Some("accepted"))
        )
        || !(matches!(evidence.get("prior_failures"), V::Null)
            || matches!(evidence.get("prior_failures"), V::Arr(keys) if keys.len() <= 128 && keys.iter().all(digest)))
    {
        return None;
    }
    let mut out = obj([]);
    for key in [
        "scope",
        "active_task_id",
        "action",
        "snapshot_attempt_id",
        "snapshot_sha256",
        "outcome_attempt_id",
        "outcome_sha256",
        "command_sha256",
        "result",
        "prior_failures",
    ] {
        out.set(key, evidence.get(key).clone());
    }
    // Receipt scope is retained exactly; arbitrarily large/missing state is
    // unknown rather than silently abbreviated into a different identity.
    if evidence.get("result").text() == Some("accepted") {
        let state = evidence
            .get("verified_state")
            .text()
            .filter(|state| !state.is_empty() && state.len() <= 512)?;
        out.set("verified_state", state);
    }
    Some(out)
}
fn failures(root: &Root, attempt: &V, cwd: &Path) -> V {
    let override_ = std::env::var_os("OMS_FAIL_LEDGER")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let path = override_
        .map(|p| if p.is_absolute() { p } else { cwd.join(p) })
        .unwrap_or_else(|| PathBuf::from(FAILURES));
    let Ok(rows) = source::rows(root, &path, false) else {
        return unreadable(FAILURES);
    };
    let first = stamp(attempt.get("created_at"));
    let last = stamp(attempt.get("updated_at"));
    let mut found = vec![];
    let mut correlated = false;
    for (i, row) in rows.iter().enumerate() {
        let evidence = reference_verification(row);
        correlated |= evidence.is_some();
        let exact = evidence.as_ref().is_some_and(|v| {
            ["snapshot_attempt_id", "outcome_attempt_id"]
                .iter()
                .any(|key| v.get(key) == attempt.get("attempt_id"))
        });
        let window = matches!((stamp(row.get("ts")), first, last), (Some(t), Some(first), Some(last)) if t >= first && t <= last);
        if exact || window {
            let mut item = obj([("line", (i + 1).into())]);
            if let Some(fp) = ledger_fingerprint(row) {
                item.set("fingerprint", V::from(fp).short(80));
            }
            for k in ["ts", "kind", "event", "exit", "agent"] {
                if row.has(k) {
                    let v = row.get(k).short(80);
                    if v != V::Null {
                        item.set(k, v);
                    }
                }
            }
            if let Some(evidence) = evidence {
                item.set("verification", evidence);
                item.set(
                    "correlation",
                    if exact {
                        "native-reference-attempt"
                    } else {
                        "time-window-only"
                    },
                );
            }
            found.push(item);
        }
    }
    if correlated {
        for item in &mut found {
            if !item.has("correlation") {
                item.set("correlation", "time-window-only");
            }
        }
        // Reserve the bounded view for exact evidence before unrelated time
        // candidates, preserving ledger order within each correlation group.
        found
            .sort_by_key(|item| item.get("correlation").text() != Some("native-reference-attempt"));
    }
    let mut out = section(FAILURES, found);
    out.set("match", if correlated {
        "native reference-operation IDs match exactly; other rows are time-window candidates that may belong to other work; neither proves a repair cause or current verification"
    } else {
        "rows stamped inside the attempt's time window; the ledger records no attempt id, so these may belong to other work"
    });
    out
}
fn pointer(root: &Root, attempt: &V, explicit: Option<&Path>) -> (Option<PathBuf>, V) {
    use std::os::unix::ffi::OsStringExt;
    if let Some(p) = explicit.filter(|p| !p.as_os_str().is_empty()) {
        return (Some(p.to_path_buf()), "--transcript".into());
    }
    let participant = attempt
        .get("refs")
        .get("panel_room_participant")
        .or(attempt.get("attempt_id").clone());
    let Some(p) = participant.text().filter(|p| id(p)) else {
        return (None, V::Null);
    };
    let relative = format!(".oms/hooks/panel-activity/{p}.json");
    if let Ok(bytes) = source::read(root, Path::new(&relative), usize::MAX, false)
        && let Ok(row) = V::parse(&bytes)
        && row.get("attempt") == attempt.get("attempt_id")
        && let Some(bytes) = row.get("transcript_path").bytes().filter(|b| !b.is_empty())
    {
        return (
            Some(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))),
            relative.into(),
        );
    }
    (None, V::Null)
}
pub(crate) fn build(
    root: &Root,
    attempt: &V,
    rows: &[V],
    explicit: Option<&Path>,
    cwd: &Path,
    cancel: &AtomicBool,
) -> Result<V, String> {
    let (path, origin) = pointer(root, attempt, explicit);
    let mut timeline = obj([("source", origin.clone()), ("available", false.into())]);
    if let Some(path) = path {
        let path = source::expand(&path);
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        match oms_context_source::inspect_timeline(&path, 50, || cancel.load(Ordering::Relaxed)) {
            Ok(v) => {
                timeline = V::parse(v.to_string().as_bytes())?;
                timeline.set("source", origin);
                timeline.set("available", true);
            }
            Err(e) => {
                use oms_context_source::Error;
                let reason = match e {
                    Error::Missing | Error::Unreadable => {
                        "transcript is missing or unreadable".into()
                    }
                    Error::Unsafe => "transcript is not a regular file".into(),
                    Error::Oversize => "transcript is larger than 268435456 bytes".into(),
                    _ => e.to_string(),
                };
                timeline.set("reason", V::from(reason).short(160));
            }
        }
    } else {
        timeline.set(
            "reason",
            "no transcript path recorded for this attempt; pass --transcript PATH",
        );
    }
    let mut bundle = obj([
        ("schema", 1usize.into()),
        ("kind", "oms-attempt-bundle".into()),
        ("attempt_id", attempt.get("attempt_id").clone()),
        ("notice", NOTICE.into()),
        ("lifecycle", lifecycle(rows, attempt)),
        ("artifact_rows", artifacts(root, attempt)),
        ("fail_ledger", failures(root, attempt, cwd)),
        ("transcript_timeline", timeline),
    ]);
    for k in ["run_id", "task_id", "provider", "tool"] {
        bundle.set(k, attempt.get(k).or(V::Null));
    }
    Ok(bundle)
}
const SCAN_ERROR: &str = "secret scan unavailable; nothing printed";
fn scan(bytes: &[u8], env: &[(&[u8], &[u8])]) -> Result<bool, String> {
    let mut input = bytes.to_vec();
    input.push(b'\n');
    oms_scrub::scan_secret(&input, env)
        .map(|hits| !hits.is_empty())
        .map_err(|_| SCAN_ERROR.into())
}
/// Native output deliberately tightens legacy withholding: surviving hits in any
/// encoded JSON or rendered text, and scanner errors, release no output.
pub(crate) fn checked_output(output: Vec<u8>, env: &[(&[u8], &[u8])]) -> Result<Vec<u8>, String> {
    if scan(&output, env)? {
        return Err(SCAN_ERROR.into());
    }
    Ok(output)
}
pub(crate) fn scanned(bundle: &mut V, env: &[(&[u8], &[u8])]) -> Result<(), String> {
    let whole = bundle
        .default_json(false)
        .map_err(|_| SCAN_ERROR.to_owned())?
        .into_bytes();
    if !scan(&whole, env)? {
        return Ok(());
    }
    fn visit(v: &mut V, env: &[(&[u8], &[u8])]) -> Result<(), String> {
        match v {
            V::Str(b) => {
                if !b.is_empty() && scan(b, env)? {
                    *v = "[withheld: secret-like]".into();
                }
            }
            V::Arr(a) => {
                for v in a {
                    visit(v, env)?;
                }
            }
            V::Obj(o) => {
                for v in o.values_mut() {
                    visit(v, env)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(bundle, env)?;
    // Leaf replacement cannot repair hits in keys, numeric values or JSON escapes.
    // Require a clean whole value and also check the final JSON/text bytes at release.
    let final_json = bundle
        .default_json(false)
        .map_err(|_| SCAN_ERROR.to_owned())?
        .into_bytes();
    checked_output(final_json, env).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scanner_boundaries_fail_closed() {
        let pairs: Vec<_> = (0..4096)
            .map(|_| (b"name".as_slice(), b"x".as_slice()))
            .collect();
        let clean = obj([("value", "plain".into())]);
        assert!(scanned(&mut clean.clone(), &pairs).is_ok());
        let mut oversized = pairs;
        oversized.push((b"more", b"x"));
        assert!(scanned(&mut clean.clone(), &oversized).is_err());
        assert!(scanned(&mut clean.clone(), &[(b"", b"x")]).is_err());
    }
}
