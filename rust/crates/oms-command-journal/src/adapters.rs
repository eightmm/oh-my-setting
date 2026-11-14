#![forbid(unsafe_code)]

use crate::sanitize::{
    DETAIL_CHAR_LIMIT, VALUE_CHAR_LIMIT, bounded_summary, diagnostic_os_str, os_str_to_utf8,
    sanitize_text,
};
use crate::tz::{parse_rfc3339_utc, utc_rfc3339};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const KNOWN_SOURCE_TYPES: &[&str] = &[
    "agent-task",
    "panel-result",
    "autopilot",
    "goal-drive",
    "handoff",
    "intent",
    "oms-run",
    "patch-admit",
    "patch-land",
    "peer-review",
    "session-handoff",
    "run-ledger",
    "run-capsule",
    "run-reconcile",
    "experiment-board",
    "ci-status",
];

#[derive(Clone, Debug, Default)]
pub struct ObserveOverrides {
    pub source_type: String,
    pub source_file: Option<PathBuf>,
    pub record_path: Option<PathBuf>,
    pub event_type: Option<String>,
    pub source_id: Option<String>,
    pub operation_id: Option<String>,
    pub occurred_at: Option<String>,
    pub verification_status: Option<String>,
    pub outcome: Option<String>,
    pub outcome_status: Option<String>,
    pub decision: Option<String>,
    pub blocker: Option<String>,
    pub next_action: Option<String>,
    pub operation: Option<String>,
}

fn read_regular_text(path: &Path) -> Result<String, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| {
        format!(
            "failed to inspect {}: {e}",
            diagnostic_os_str(path.as_os_str())
        )
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!(
            "refusing non-regular source file {}",
            diagnostic_os_str(path.as_os_str())
        ));
    }
    let bytes = fs::read(path).map_err(|e| {
        format!(
            "failed reading {}: {e}",
            diagnostic_os_str(path.as_os_str())
        )
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn build_source_payload(
    repo_root: &Path,
    overrides: &ObserveOverrides,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let source_type = overrides.source_type.trim();
    if source_type.is_empty() {
        return Err("source_type is required".to_string());
    }

    if let Some(vs) = overrides.verification_status.as_deref() {
        let trimmed = vs.trim();
        if !trimmed.is_empty() && !crate::schema::is_valid_verification_status(trimmed) {
            return Err(format!(
                "invalid --verification-status '{trimmed}'; allowed values: {}",
                crate::schema::allowed_verification_statuses_text()
            ));
        }
    }

    let record_path_label = resolve_record_path_label(repo_root, overrides.record_path.as_deref())?;

    let mut payload = if let Some(sf) = overrides.source_file.as_deref() {
        let content = read_regular_text(sf)?;
        parse_adapter_payload(
            repo_root,
            source_type,
            sf,
            &content,
            overrides,
            record_path_label.as_deref(),
            now,
        )?
    } else {
        let mut base = Map::new();
        base.insert(
            "event_type".into(),
            Value::String(
                overrides
                    .event_type
                    .clone()
                    .unwrap_or_else(|| "agent_state".to_string()),
            ),
        );
        let mut src = Map::new();
        src.insert("type".into(), Value::String(source_type.to_string()));
        if let Some(sid) = overrides
            .source_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            src.insert("id".into(), Value::String(sid.trim().to_string()));
        }
        if let Some(rp) = record_path_label.as_deref() {
            src.insert("record_path".into(), Value::String(rp.to_string()));
        }
        base.insert("source".into(), Value::Object(src));
        Value::Object(base)
    };

    // Apply CLI explicit overrides cleanly
    if let Some(obj) = payload.as_object_mut() {
        if let Some(et) = overrides
            .event_type
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            obj.insert("event_type".into(), Value::String(et.trim().to_string()));
        }
        if let Some(occ) = overrides
            .occurred_at
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let parsed = parse_rfc3339_utc(occ)?;
            obj.insert("occurred_at".into(), Value::String(utc_rfc3339(parsed)));
        } else if !obj.contains_key("occurred_at") {
            obj.insert("occurred_at".into(), Value::String(utc_rfc3339(now)));
        }
        if let Some(vs) = overrides
            .verification_status
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            obj.insert(
                "verification_status".into(),
                Value::String(vs.trim().to_string()),
            );
        }
        if let Some(sid) = overrides
            .source_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let src = obj
                .entry("source")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(smap) = src.as_object_mut() {
                smap.insert("id".into(), Value::String(sid.trim().to_string()));
            }
        }
        if let Some(rp) = record_path_label.as_deref() {
            let src = obj
                .entry("source")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(smap) = src.as_object_mut() {
                smap.insert("record_path".into(), Value::String(rp.to_string()));
            }
        }
        if let Some(op_id) = overrides
            .operation_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let corr = obj
                .entry("correlation")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(cmap) = corr.as_object_mut() {
                cmap.insert(
                    "operation_id".into(),
                    Value::String(op_id.trim().to_string()),
                );
            }
        }
        if let Some(dec) = overrides
            .decision
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            obj.insert("decision".into(), Value::String(dec.trim().to_string()));
        }
        if let Some(blk) = overrides
            .blocker
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            obj.insert("blocker".into(), Value::String(blk.trim().to_string()));
        }
        if let Some(na) = overrides
            .next_action
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            obj.insert("next_action".into(), Value::String(na.trim().to_string()));
        }
        if overrides.outcome.is_some() || overrides.outcome_status.is_some() {
            let out_entry = obj
                .entry("outcome")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(omap) = out_entry.as_object_mut() {
                if let Some(text) = overrides
                    .outcome
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                {
                    omap.insert("summary".into(), Value::String(text.trim().to_string()));
                }
                if let Some(st) = overrides
                    .outcome_status
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                {
                    omap.insert("status".into(), Value::String(st.trim().to_string()));
                }
            }
        }
        if let Some(op) = overrides
            .operation
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let src = obj
                .entry("source")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(smap) = src.as_object_mut() {
                smap.insert("command".into(), Value::String(op.trim().to_string()));
            }
        }
    }

    Ok(payload)
}

fn parse_adapter_payload(
    repo_root: &Path,
    source_type: &str,
    source_file: &Path,
    content: &str,
    overrides: &ObserveOverrides,
    record_path_label: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let rel_ref = if let Some(rp_path) = overrides.record_path.as_deref() {
        if let Some(stripped) = strip_relative_utf8(repo_root, rp_path) {
            stripped
        } else if let Some(rp) = record_path_label {
            rp.to_string()
        } else {
            relative_ref(repo_root, source_file)?
        }
    } else {
        relative_ref(repo_root, source_file)?
    };
    let trimmed = content.trim();

    if source_type == "session-handoff" {
        return adapt_session_handoff(source_file, overrides, &rel_ref, content, now);
    }
    if source_type == "peer-review" {
        return adapt_peer_review(repo_root, source_file, overrides, &rel_ref, content, now);
    }
    if source_type == "agent-task" && !trimmed.starts_with('{') {
        return adapt_agent_task_markdown(source_file, &rel_ref, content, overrides, now);
    }

    // Try single JSON object or last non-empty JSONL object
    let record_opt = parse_json_or_last_jsonl(trimmed);
    let Some(record) = record_opt else {
        if KNOWN_SOURCE_TYPES.contains(&source_type) {
            return adapt_markdown_fallback(
                source_type,
                source_file,
                overrides,
                &rel_ref,
                content,
                now,
            );
        }
        return Err(format!(
            "unsupported source_type '{source_type}': source file '{}' is not a valid JSON record",
            diagnostic_os_str(source_file.as_os_str())
        ));
    };

    // If caller already supplied canonical-like event_type + source object
    if record.get("event_type").is_some()
        && record
            .get("source")
            .and_then(Value::as_object)
            .and_then(|s| s.get("type"))
            .is_some()
    {
        return Ok(record);
    }

    match source_type {
        "run-ledger" => adapt_run_ledger(&record, &rel_ref, now),
        "run-capsule" => adapt_run_capsule(&record, &rel_ref, now),
        "ci-status" => adapt_ci_status(&record, &rel_ref, now),
        "experiment-board" => adapt_experiment_board(&record, &rel_ref, now),
        "panel-result" => adapt_panel_result(&record, &rel_ref, now),
        "agent-task" => adapt_json_generic("agent-task", "agent_state", &record, &rel_ref, now),
        "autopilot" => adapt_json_generic("autopilot", "agent_state", &record, &rel_ref, now),
        "goal-drive" => adapt_json_generic("goal-drive", "agent_state", &record, &rel_ref, now),
        "handoff" => adapt_json_generic("handoff", "handoff", &record, &rel_ref, now),
        "intent" => adapt_json_generic("intent", "decision", &record, &rel_ref, now),
        "oms-run" => adapt_json_generic("oms-run", "run_finished", &record, &rel_ref, now),
        "patch-admit" => adapt_json_generic("patch-admit", "validation", &record, &rel_ref, now),
        "patch-land" => adapt_json_generic("patch-land", "run_finished", &record, &rel_ref, now),
        "run-reconcile" => {
            adapt_json_generic("run-reconcile", "run_recovered", &record, &rel_ref, now)
        }
        _ => adapt_json_generic(source_type, "agent_state", &record, &rel_ref, now),
    }
}

fn parse_json_or_last_jsonl(trimmed: &str) -> Option<Value> {
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        if val.is_object() {
            return Some(val);
        }
    }
    for line in trimmed.lines().rev() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(l) {
            if val.is_object() {
                return Some(val);
            }
        }
    }
    None
}

fn strip_relative_utf8(repo_root: &Path, path: &Path) -> Option<String> {
    let abs_path = fs::canonicalize(path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|p| fs::canonicalize(p).ok())
            .map(|p| path.file_name().map_or_else(|| p.clone(), |f| p.join(f)))
            .unwrap_or_else(|| path.to_path_buf())
    });
    let abs_repo = fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    if let Ok(rel) = abs_path.strip_prefix(&abs_repo) {
        if let Some(s) = rel.to_str() {
            return Some(s.to_string());
        }
    }
    if let Ok(rel) = path.strip_prefix(repo_root) {
        if let Some(s) = rel.to_str() {
            return Some(s.to_string());
        }
    }
    None
}

fn resolve_record_path_label(
    repo_root: &Path,
    rp: Option<&Path>,
) -> Result<Option<String>, String> {
    let Some(path) = rp else {
        return Ok(None);
    };
    if path.as_os_str().is_empty() {
        return Ok(None);
    }
    if let Some(s) = path.to_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        return Ok(Some(trimmed.to_string()));
    }
    if let Some(rel) = strip_relative_utf8(repo_root, path) {
        let trimmed = rel.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }
    os_str_to_utf8(path.as_os_str(), "--record-path").map(|s| Some(s.trim().to_string()))
}

fn relative_ref(repo_root: &Path, path: &Path) -> Result<String, String> {
    let abs_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let abs_repo = fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    if let Ok(rel) = abs_path.strip_prefix(&abs_repo) {
        return os_str_to_utf8(rel.as_os_str(), "--source-file").map(ToOwned::to_owned);
    }
    if let Ok(rel) = path.strip_prefix(repo_root) {
        return os_str_to_utf8(rel.as_os_str(), "--source-file").map(ToOwned::to_owned);
    }
    let name = path.file_name().unwrap_or_else(|| path.as_os_str());
    os_str_to_utf8(name, "--source-file").map(ToOwned::to_owned)
}

fn file_stem_for_event(
    path: &Path,
    overrides: &ObserveOverrides,
    fallback: &str,
) -> Result<String, String> {
    if let Some(stem) = path.file_stem() {
        if let Some(s) = stem.to_str() {
            return Ok(s.to_string());
        }
    } else {
        return Ok(fallback.to_string());
    }
    if let Some(sid) = overrides
        .source_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(sid.to_string());
    }
    if let Some(rp_stem) = overrides
        .record_path
        .as_deref()
        .and_then(Path::file_stem)
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(rp_stem.to_string());
    }
    match path.file_stem() {
        Some(stem) => os_str_to_utf8(stem, "--source-file").map(ToOwned::to_owned),
        None => Ok(fallback.to_string()),
    }
}

fn pick_str(obj: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = obj
            .get(*k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(s.to_string());
        }
    }
    None
}

fn pick_occurred_at(obj: &Map<String, Value>, now: DateTime<Utc>) -> String {
    for k in [
        "occurred_at",
        "timestamp",
        "finished_at",
        "completed_at",
        "updated_at",
        "created_at",
        "started_at",
        "ts",
    ] {
        if let Some(raw) = obj.get(k).and_then(Value::as_str) {
            if let Ok(dt) = parse_rfc3339_utc(raw) {
                return utc_rfc3339(dt);
            }
        }
    }
    utc_rfc3339(now)
}

fn adapt_run_ledger(record: &Value, rel_ref: &str, now: DateTime<Utc>) -> Result<Value, String> {
    let obj = record
        .as_object()
        .ok_or_else(|| "run-ledger record must be an object".to_string())?;
    let run_id = pick_str(obj, &["run_id", "id", "entry_id"]).unwrap_or_default();
    let seq = obj
        .get("seq")
        .or_else(|| obj.get("index"))
        .and_then(Value::as_u64);
    let src_id = if !run_id.is_empty() {
        if run_id.starts_with("ledger-") {
            run_id.clone()
        } else {
            format!("ledger-{run_id}")
        }
    } else if let Some(n) = seq {
        format!("ledger-{rel_ref}#{n}")
    } else {
        format!("ledger-{rel_ref}")
    };
    let ev_ref = if !run_id.is_empty() {
        format!("{rel_ref}#{run_id}")
    } else {
        rel_ref.to_string()
    };

    let exit_code = obj
        .get("exit_code")
        .or_else(|| obj.get("returncode"))
        .and_then(Value::as_i64);
    let status = pick_str(obj, &["status", "state"]).unwrap_or_else(|| match exit_code {
        Some(0) => "success".to_string(),
        Some(_) => "failed".to_string(),
        None => "recorded".to_string(),
    });
    let event_type = match status.as_str() {
        "started" | "running" => "run_started",
        "recovered" => "run_recovered",
        _ => "run_finished",
    };
    let summary = pick_str(obj, &["summary", "note", "command", "cmd", "label"])
        .unwrap_or_else(|| format!("Run ledger {status}"));
    let vs = match exit_code {
        Some(0) => "not_verified",
        Some(_) => "failed",
        None => "not_verified",
    };

    let mut out = Map::new();
    out.insert("event_type".into(), Value::String(event_type.into()));
    out.insert(
        "occurred_at".into(),
        Value::String(pick_occurred_at(obj, now)),
    );
    let mut src = Map::new();
    src.insert("type".into(), Value::String("run-ledger".into()));
    src.insert("id".into(), Value::String(src_id));
    src.insert("record_path".into(), Value::String(rel_ref.into()));
    out.insert("source".into(), Value::Object(src));

    let mut outcome = Map::new();
    outcome.insert("summary".into(), Value::String(summary));
    outcome.insert("status".into(), Value::String(status));
    if let Some(ec) = exit_code {
        outcome.insert("exit_code".into(), Value::Number(ec.into()));
    }
    out.insert("outcome".into(), Value::Object(outcome));
    out.insert("verification_status".into(), Value::String(vs.into()));
    out.insert(
        "evidence".into(),
        Value::Array(vec![
            serde_json::json!({"type": "run-ledger", "ref": ev_ref}),
        ]),
    );
    extract_correlation(obj, &mut out);
    Ok(Value::Object(out))
}

fn adapt_run_capsule(record: &Value, rel_ref: &str, now: DateTime<Utc>) -> Result<Value, String> {
    let obj = record
        .as_object()
        .ok_or_else(|| "run-capsule record must be an object".to_string())?;
    let cid = pick_str(obj, &["capsule_id", "run_id", "id"]).unwrap_or_else(|| rel_ref.into());
    let exit_code = obj.get("exit_code").and_then(Value::as_i64);
    let status = pick_str(obj, &["status"]).unwrap_or_else(|| match exit_code {
        Some(0) => "success".to_string(),
        Some(_) => "failed".to_string(),
        None => "recorded".to_string(),
    });
    let summary = pick_str(obj, &["note", "summary", "command"])
        .unwrap_or_else(|| format!("Experiment capsule {cid}"));
    let vs = if status == "failed" || exit_code.is_some_and(|c| c != 0) {
        "failed"
    } else {
        "not_verified"
    };

    let mut out = Map::new();
    out.insert("event_type".into(), Value::String("experiment".into()));
    out.insert(
        "occurred_at".into(),
        Value::String(pick_occurred_at(obj, now)),
    );
    out.insert(
        "source".into(),
        serde_json::json!({"type": "run-capsule", "id": cid, "record_path": rel_ref}),
    );
    out.insert(
        "outcome".into(),
        serde_json::json!({"summary": summary, "status": status}),
    );
    out.insert("verification_status".into(), Value::String(vs.into()));
    out.insert(
        "evidence".into(),
        Value::Array(vec![
            serde_json::json!({"type": "run-capsule", "ref": rel_ref}),
        ]),
    );
    if let Some(m) = obj.get("metrics") {
        out.insert("metrics".into(), m.clone());
    }
    extract_correlation(obj, &mut out);
    Ok(Value::Object(out))
}

fn adapt_ci_status(record: &Value, rel_ref: &str, now: DateTime<Utc>) -> Result<Value, String> {
    let obj = record
        .as_object()
        .ok_or_else(|| "ci-status record must be an object".to_string())?;
    let sha = pick_str(obj, &["sha", "commit", "head_sha"]).unwrap_or_default();
    let short_sha: String = sha.chars().take(7).collect();
    let conclusion = pick_str(obj, &["conclusion", "status", "state"])
        .unwrap_or_else(|| "unknown".into())
        .to_ascii_lowercase();
    let run_id = pick_str(obj, &["run_id", "id"]).unwrap_or_default();
    let workflow = pick_str(obj, &["workflow", "name"]).unwrap_or_else(|| "CI".into());
    let src_id = if !run_id.is_empty() {
        format!("ci-{run_id}")
    } else if !sha.is_empty() {
        format!("ci-{sha}")
    } else {
        format!("ci-{rel_ref}")
    };

    let (vs, out_status) = match conclusion.as_str() {
        "success" | "passed" => ("passed", "success"),
        "failure" | "failed" | "timed_out" | "cancelled" => ("failed", "failed"),
        "in_progress" | "queued" | "running" | "pending" => ("running", "running"),
        _ => ("not_verified", "recorded"),
    };

    let summary = if !short_sha.is_empty() {
        format!("CI {conclusion} for commit {sha} ({workflow})")
    } else {
        format!("CI {conclusion} ({workflow})")
    };

    let mut out = Map::new();
    out.insert("event_type".into(), Value::String("validation".into()));
    out.insert(
        "occurred_at".into(),
        Value::String(pick_occurred_at(obj, now)),
    );
    out.insert(
        "source".into(),
        serde_json::json!({"type": "ci-status", "id": src_id, "record_path": rel_ref}),
    );
    out.insert(
        "outcome".into(),
        serde_json::json!({"summary": summary, "status": out_status}),
    );
    out.insert("verification_status".into(), Value::String(vs.into()));
    if out_status == "failed" {
        out.insert(
            "blocker".into(),
            Value::String(format!("CI {conclusion} for commit {short_sha}")),
        );
    }
    let ev_ref = pick_str(obj, &["url", "html_url"]).unwrap_or_else(|| rel_ref.into());
    out.insert(
        "evidence".into(),
        Value::Array(vec![
            serde_json::json!({"type": "ci-status", "ref": ev_ref}),
        ]),
    );
    extract_correlation(obj, &mut out);
    if !sha.is_empty() {
        let corr = out
            .entry("correlation")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(cmap) = corr.as_object_mut() {
            cmap.entry("commit").or_insert_with(|| Value::String(sha));
        }
    }
    Ok(Value::Object(out))
}

fn adapt_experiment_board(
    record: &Value,
    rel_ref: &str,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let mut base = adapt_json_generic("experiment-board", "experiment", record, rel_ref, now)?;
    if let (Some(obj), Some(rec)) = (base.as_object_mut(), record.as_object()) {
        if let Some(m) = rec.get("metrics") {
            obj.insert("metrics".into(), m.clone());
        }
    }
    Ok(base)
}

fn adapt_panel_result(record: &Value, rel_ref: &str, now: DateTime<Utc>) -> Result<Value, String> {
    let obj = record
        .as_object()
        .ok_or_else(|| "panel-result record must be an object".to_string())?;
    let sid = pick_str(obj, &["panel_id", "review_id", "id"]).unwrap_or_else(|| rel_ref.into());
    let verdict =
        pick_str(obj, &["verdict", "status", "outcome"]).unwrap_or_else(|| "recorded".into());
    let summary = pick_str(obj, &["summary", "synthesis", "note"])
        .unwrap_or_else(|| format!("Panel result ({verdict})"));
    let task_id = pick_str(obj, &["task_id"]);
    let vs = match verdict.to_ascii_lowercase().as_str() {
        "pass" | "passed" | "approve" | "approved" => "passed",
        "fail" | "failed" | "reject" | "rejected" | "block" => "failed",
        _ => "not_verified",
    };

    let mut out = Map::new();
    out.insert("event_type".into(), Value::String("validation".into()));
    out.insert(
        "occurred_at".into(),
        Value::String(pick_occurred_at(obj, now)),
    );
    out.insert(
        "source".into(),
        serde_json::json!({"type": "panel-result", "id": sid, "record_path": rel_ref}),
    );
    out.insert(
        "outcome".into(),
        serde_json::json!({"summary": summary, "status": verdict}),
    );
    out.insert("verification_status".into(), Value::String(vs.into()));
    out.insert(
        "evidence".into(),
        Value::Array(vec![
            serde_json::json!({"type": "panel-result", "ref": rel_ref}),
        ]),
    );
    extract_correlation(obj, &mut out);
    if let Some(tid) = task_id {
        let corr = out
            .entry("correlation")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(cmap) = corr.as_object_mut() {
            cmap.entry("task_id").or_insert_with(|| Value::String(tid));
        }
    }
    Ok(Value::Object(out))
}

fn adapt_json_generic(
    source_type: &str,
    default_event_type: &str,
    record: &Value,
    rel_ref: &str,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let obj = record
        .as_object()
        .ok_or_else(|| format!("{source_type} record must be a JSON object"))?;
    let event_type = pick_str(obj, &["event_type"]).unwrap_or_else(|| default_event_type.into());
    let src_id = pick_str(obj, &["id", "source_id", "run_id", "task_id", "handoff_id"])
        .unwrap_or_else(|| rel_ref.into());
    let summary = pick_str(obj, &["summary", "note", "message", "title", "goal"])
        .unwrap_or_else(|| format!("{source_type} event ({src_id})"));
    let status =
        pick_str(obj, &["status", "state", "verdict"]).unwrap_or_else(|| "recorded".into());
    let vs = pick_str(obj, &["verification_status"])
        .filter(|s| crate::schema::is_valid_verification_status(s))
        .unwrap_or_else(|| "not_verified".into());

    let mut out = Map::new();
    out.insert("event_type".into(), Value::String(event_type));
    out.insert(
        "occurred_at".into(),
        Value::String(pick_occurred_at(obj, now)),
    );
    out.insert(
        "source".into(),
        serde_json::json!({"type": source_type, "id": src_id, "record_path": rel_ref}),
    );
    out.insert(
        "outcome".into(),
        serde_json::json!({"summary": summary, "status": status}),
    );
    out.insert("verification_status".into(), Value::String(vs));
    out.insert(
        "evidence".into(),
        Value::Array(vec![
            serde_json::json!({"type": source_type, "ref": rel_ref}),
        ]),
    );
    for field in [
        "decision",
        "blocker",
        "next_action",
        "note",
        "research",
        "work_item",
    ] {
        if let Some(v) = obj.get(field) {
            out.insert(field.into(), v.clone());
        }
    }
    extract_correlation(obj, &mut out);
    Ok(Value::Object(out))
}

fn adapt_session_handoff(
    source_file: &Path,
    overrides: &ObserveOverrides,
    rel_ref: &str,
    content: &str,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    // Parse JSON or JSONL session handoff/transcript: summary is the last assistant line
    let sid = file_stem_for_event(source_file, overrides, rel_ref)?;

    if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(content.trim()) {
        return adapt_json_generic(
            "session-handoff",
            "handoff",
            &Value::Object(obj),
            rel_ref,
            now,
        );
    }

    let mut last_assistant = String::new();
    let mut session_id = sid.clone();
    let mut occurred = utc_rfc3339(now);

    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(t) {
            if let Some(s) = pick_str(&obj, &["session_id", "thread_id"]) {
                session_id = s;
            }
            if let Some(ts) = pick_str(&obj, &["timestamp", "occurred_at", "ts"]) {
                if let Ok(dt) = parse_rfc3339_utc(&ts) {
                    occurred = utc_rfc3339(dt);
                }
            }
            let role = obj
                .get("role")
                .or_else(|| obj.get("message").and_then(|m| m.get("role")))
                .and_then(Value::as_str)
                .unwrap_or("");
            if role == "assistant" {
                let text = extract_assistant_text(&obj);
                if !text.is_empty() {
                    if let Some(last_line) = text.lines().map(str::trim).rfind(|l| !l.is_empty()) {
                        last_assistant = sanitize_text(last_line, VALUE_CHAR_LIMIT);
                    }
                }
            }
        }
    }

    if last_assistant.is_empty() {
        let fallback_line = content
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("Session handoff");
        last_assistant = sanitize_text(fallback_line, VALUE_CHAR_LIMIT);
    }

    let mut out = Map::new();
    out.insert("event_type".into(), Value::String("handoff".into()));
    out.insert("occurred_at".into(), Value::String(occurred));
    out.insert(
        "source".into(),
        serde_json::json!({"type": "session-handoff", "id": sid, "record_path": rel_ref}),
    );
    out.insert(
        "correlation".into(),
        serde_json::json!({"session_id": session_id}),
    );
    out.insert(
        "outcome".into(),
        serde_json::json!({"summary": last_assistant, "status": "recorded"}),
    );
    out.insert(
        "verification_status".into(),
        Value::String("not_verified".into()),
    );
    out.insert(
        "evidence".into(),
        Value::Array(vec![
            serde_json::json!({"type": "session-handoff", "ref": rel_ref}),
        ]),
    );
    Ok(Value::Object(out))
}

fn extract_assistant_text(obj: &Map<String, Value>) -> String {
    if let Some(s) = obj
        .get("text")
        .or_else(|| obj.get("content"))
        .and_then(Value::as_str)
    {
        return s.to_string();
    }
    if let Some(arr) = obj
        .get("content")
        .or_else(|| obj.get("message").and_then(|m| m.get("content")))
        .and_then(Value::as_array)
    {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(s) = item.get("text").and_then(Value::as_str) {
                parts.push(s.to_string());
            }
        }
        return parts.join("\n");
    }
    String::new()
}

fn adapt_peer_review(
    repo_root: &Path,
    source_file: &Path,
    overrides: &ObserveOverrides,
    rel_ref: &str,
    content: &str,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let trimmed = content.trim();
    if let Some(json_rec) = parse_json_or_last_jsonl(trimmed) {
        return adapt_json_generic("peer-review", "validation", &json_rec, rel_ref, now);
    }
    let stem = file_stem_for_event(source_file, overrides, rel_ref)?;
    let summary = bounded_summary(trimmed, DETAIL_CHAR_LIMIT);
    let summary_text = if summary.is_empty() {
        format!("Peer review ({stem})")
    } else {
        summary
    };
    let task_id = infer_review_task_id(repo_root, source_file, content);

    let mut out = Map::new();
    out.insert("event_type".into(), Value::String("validation".into()));
    out.insert("occurred_at".into(), Value::String(utc_rfc3339(now)));
    out.insert(
        "source".into(),
        serde_json::json!({"type": "peer-review", "id": stem, "record_path": rel_ref}),
    );
    out.insert(
        "outcome".into(),
        serde_json::json!({"summary": summary_text, "status": "recorded"}),
    );
    out.insert(
        "verification_status".into(),
        Value::String("not_verified".into()),
    );
    out.insert(
        "evidence".into(),
        Value::Array(vec![
            serde_json::json!({"type": "peer-review", "ref": rel_ref}),
        ]),
    );
    if let Some(tid) = task_id {
        out.insert("correlation".into(), serde_json::json!({"task_id": tid}));
    }
    Ok(Value::Object(out))
}

fn task_id_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\btask[ _-]?id\b\s*[:=]\s*`?([A-Za-z0-9._-]+)`?").expect("valid regex")
    })
}

pub fn infer_review_task_id(repo_root: &Path, source_file: &Path, content: &str) -> Option<String> {
    if let Some(caps) = task_id_line_re().captures(content) {
        return Some(caps[1].to_string());
    }
    // Check companion scope markdown for _synthesis-review-<stamp>.md
    if let Some(fname) = source_file.file_name().and_then(|n| n.to_str()) {
        if let Some(stamp) = fname
            .strip_prefix("_synthesis-review-")
            .and_then(|s| s.strip_suffix(".md"))
        {
            if let Some(parent) = source_file.parent() {
                for candidate in [
                    parent.join(format!("_scope-{stamp}.md")),
                    parent.join(format!("scope-{stamp}.md")),
                    repo_root
                        .join(".oms/artifacts/review")
                        .join(format!("_scope-{stamp}.md")),
                ] {
                    if let Ok(scope_text) = read_regular_text(&candidate) {
                        if let Some(caps) = task_id_line_re().captures(&scope_text) {
                            return Some(caps[1].to_string());
                        }
                    }
                }
            }
        }
    }
    // Fallback to .oms/task/current.md task_id
    let current_task = repo_root.join(".oms").join("task").join("current.md");
    if let Ok(text) = read_regular_text(&current_task) {
        if let Some(caps) = task_id_line_re().captures(&text) {
            return Some(caps[1].to_string());
        }
    }
    None
}

fn adapt_agent_task_markdown(
    source_file: &Path,
    rel_ref: &str,
    content: &str,
    overrides: &ObserveOverrides,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let mut task_id = String::new();
    let mut goal = String::new();
    let mut status = String::new();
    let mut result_text = String::new();
    let mut decision = String::new();
    let mut blocker = String::new();
    let mut next_action = String::new();
    let mut session_hash = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("task_id:") {
            task_id = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("- task_id:") {
            task_id = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("status:") {
            status = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("- status:") {
            status = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("source_session:") {
            session_hash = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("- Goal:") {
            goal = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("- Result:") {
            result_text = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("- Decision:") {
            decision = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("- Blocker:") {
            blocker = v.trim().to_string();
        } else if let Some(v) = trimmed.strip_prefix("- Next:") {
            next_action = v.trim().to_string();
        }
    }
    if goal.is_empty() {
        if let Some(idx) = content.find("## Goal") {
            let after = &content[idx + 7..];
            for line in after.lines().map(str::trim) {
                if line.starts_with("## ") {
                    break;
                }
                if !line.is_empty() {
                    goal = line.trim_start_matches("- ").to_string();
                    break;
                }
            }
        }
    }
    let summary = if !result_text.is_empty() {
        result_text
    } else if !goal.is_empty() {
        goal
    } else {
        file_stem_for_event(source_file, overrides, "agent-task")?
    };
    let op = overrides
        .operation
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("update");
    let src_id = if !task_id.is_empty() {
        format!("{task_id}:{op}")
    } else {
        rel_ref.to_string()
    };

    let mut out = Map::new();
    out.insert("event_type".into(), Value::String("agent_state".into()));
    out.insert("occurred_at".into(), Value::String(utc_rfc3339(now)));
    out.insert(
        "source".into(),
        serde_json::json!({"type": "agent-task", "id": src_id, "record_path": rel_ref}),
    );
    out.insert(
        "outcome".into(),
        serde_json::json!({
            "summary": summary,
            "status": if status.is_empty() { "recorded".to_string() } else { status }
        }),
    );
    out.insert(
        "verification_status".into(),
        Value::String("not_verified".into()),
    );
    out.insert(
        "evidence".into(),
        Value::Array(vec![
            serde_json::json!({"type": "agent-task", "ref": rel_ref}),
        ]),
    );
    if !decision.is_empty() && decision != "none" {
        out.insert("decision".into(), Value::String(decision));
    }
    if !blocker.is_empty() && blocker != "none" {
        out.insert("blocker".into(), Value::String(blocker));
    }
    if !next_action.is_empty() && next_action != "none" {
        out.insert("next_action".into(), Value::String(next_action));
    }
    let mut corr = Map::new();
    if !task_id.is_empty() {
        corr.insert("task_id".into(), Value::String(task_id));
    }
    if !session_hash.is_empty() {
        corr.insert("session_id".into(), Value::String(session_hash));
    }
    if !corr.is_empty() {
        out.insert("correlation".into(), Value::Object(corr));
    }
    Ok(Value::Object(out))
}

fn adapt_markdown_fallback(
    source_type: &str,
    source_file: &Path,
    overrides: &ObserveOverrides,
    rel_ref: &str,
    content: &str,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let summary = bounded_summary(content, DETAIL_CHAR_LIMIT);
    let sid = file_stem_for_event(source_file, overrides, rel_ref)?;
    let mut out = Map::new();
    out.insert("event_type".into(), Value::String("agent_state".into()));
    out.insert("occurred_at".into(), Value::String(utc_rfc3339(now)));
    out.insert(
        "source".into(),
        serde_json::json!({"type": source_type, "id": sid, "record_path": rel_ref}),
    );
    out.insert(
        "outcome".into(),
        serde_json::json!({
            "summary": if summary.is_empty() { format!("{source_type} observation") } else { summary },
            "status": "recorded"
        }),
    );
    out.insert(
        "verification_status".into(),
        Value::String("not_verified".into()),
    );
    out.insert(
        "evidence".into(),
        Value::Array(vec![
            serde_json::json!({"type": source_type, "ref": rel_ref}),
        ]),
    );
    Ok(Value::Object(out))
}

fn extract_correlation(obj: &Map<String, Value>, out: &mut Map<String, Value>) {
    let mut corr = obj
        .get("correlation")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in [
        "operation_id",
        "task_id",
        "session_id",
        "thread_id",
        "run_id",
        "commit",
        "branch",
        "pr_number",
    ] {
        if let Some(v) = obj.get(key) {
            if !corr.contains_key(key) {
                corr.insert(key.into(), v.clone());
            }
        }
    }
    if !corr.is_empty() {
        out.insert("correlation".into(), Value::Object(corr));
    }
}
