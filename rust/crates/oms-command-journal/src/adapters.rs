#![forbid(unsafe_code)]

use crate::json::{JsonExt, Map, Value, json};
use crate::sanitize::{canonical_json_bytes, sanitize_text, sha256_hex};
use crate::schema::{json_truthy, python_str, python_str_checked};
use crate::tz::utc_rfc3339;
use chrono::{DateTime, Utc};
use regex::Regex;
use std::path::{Path, PathBuf};

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

fn read_source(path: &Path) -> Result<Vec<u8>, String> {
    let (root, relative) = crate::schema::storage_root(path)?;
    root.read_path(
        &relative,
        2 * 1024 * 1024,
        oms_safe_fs::Policy {
            single_link: true,
            owner: None,
            max_components: None,
        },
    )
    .map_err(|e| match e {
        oms_safe_fs::Error::Oversize => "source record exceeds Work Journal bound".into(),
        _ => format!("source record unavailable: {e:?}"),
    })
}
fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = read_source(path)?;
    let value: Value = crate::json::from_slice(&bytes).map_err(|_| "source record is not JSON")?;
    if !value.is_object() {
        return Err("source record is not an object".into());
    }
    Ok(value)
}
fn nonempty(value: &Value) -> Option<&Value> {
    json_truthy(value).then_some(value)
}
fn text(value: &Value) -> Result<String, String> {
    Ok(nonempty(value)
        .map(python_str_checked)
        .transpose()?
        .unwrap_or_default())
}
fn first<'a>(values: &[&'a Value]) -> Value {
    values
        .iter()
        .find(|v| json_truthy(v))
        .map(|v| (*v).copy())
        .unwrap_or(Value::Null)
}
fn relative(repo: &Path, path: &Path) -> Option<String> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let root = std::fs::canonicalize(repo).ok()?;
    let rel = canonical.strip_prefix(root).ok()?;
    Some(crate::sanitize::diagnostic_os_str(rel.as_os_str()))
}
fn source_ref(repo: &Path, path: &Path, id: &str) -> String {
    relative(repo, path)
        .map(|p| format!("{p}#{id}"))
        .unwrap_or_else(|| id.into())
}
fn zero_exit(value: &Value) -> bool {
    match value {
        Value::Integer(n) => n.as_decimal() == "0",
        Value::Float(n) => *n == 0.0,
        Value::Bool(b) => !b,
        _ => false,
    }
}

fn command_summary(value: &Value) -> Result<Value, String> {
    let Some(parts) = value.as_array() else {
        return Ok(Value::Null);
    };
    let parts: Vec<_> = parts
        .iter()
        .take(12)
        .map(|p| python_str_checked(p).map(|text| sanitize_text(&text, 160)))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    let summary = sanitize_text(&parts.join(" "), 500);
    Ok(if summary.is_empty() {
        Value::Null
    } else {
        Value::String(summary.as_str().into())
    })
}
fn metrics(value: &Value, id: &str) -> Vec<Value> {
    value
        .as_object()
        .into_iter()
        .flat_map(|obj| obj.iter())
        .take(64)
        .map(|(k, v)| json!({"name":k,"value":v,"evidence_ref":id}))
        .collect()
}

fn handoff_summary(path: &Path) -> Result<String, String> {
    let maximum = 2 * 1024 * 1024;
    let bytes = match crate::files::read_prefix(path, 4 * maximum + 4) {
        Ok(bytes) => bytes,
        Err(_) => return Ok("Session handoff captured".into()),
    };
    let valid = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) => {
            let valid = std::str::from_utf8(&bytes[..error.valid_up_to()])
                .map_err(|_| "handoff is not UTF-8")?;
            if valid.chars().count() < maximum {
                return Err("handoff is not UTF-8".into());
            }
            valid
        }
    };
    let end = valid
        .char_indices()
        .nth(maximum)
        .map(|(index, _)| index)
        .unwrap_or(valid.len());
    let text = &valid[..end];
    let historical = "## Original request (historical)";
    let modern = crate::unicode::text_lines(text).any(|line| line == historical);
    let stop = Regex::new(
        r"^## (?:Goal|Recent user turns.*|Files touched|Resume contract|Open dissents)$",
    )
    .expect("pattern");
    let heading = Regex::new(r"^#{1,6}\s+").expect("pattern");
    let mut inside = false;
    let mut lines = Vec::new();
    for line in crate::unicode::text_lines(text) {
        if line == "## Last assistant summary" {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line == historical || (!modern && stop.is_match(line)) {
            break;
        }
        let clean = heading.replace(line.trim(), "");
        let clean = clean.trim_matches(['*', ' ']);
        if !clean.is_empty() {
            lines.push(clean.to_owned());
        }
    }
    let joined = lines.join(" ");
    let mut end = joined.len();
    for (i, ch) in joined.char_indices() {
        if matches!(ch, '.' | '!' | '?' | '。')
            && joined[i + ch.len_utf8()..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
        {
            end = i + ch.len_utf8();
            break;
        }
    }
    let clean = sanitize_text(&joined[..end].replace(" | ", " "), 120);
    Ok(if clean.is_empty() {
        "Session handoff captured".into()
    } else {
        clean
    })
}

pub fn infer_review_task_id(
    _repo_root: &Path,
    source_file: &Path,
    _content: &str,
) -> Option<String> {
    let name = source_file.file_name()?.to_str()?;
    let pattern = Regex::new(r"-(\d{8}T\d{6}Z-\d+)\.md$").expect("pattern");
    let captures = pattern.captures(name)?;
    let row = read_json(
        &source_file
            .parent()?
            .join(format!("_scope-{}.json", &captures[1])),
    )
    .ok()?;
    let mut pending = vec![&row];
    while let Some(node) = pending.pop() {
        match node {
            Value::Object(map) => {
                if let Some(id) = map
                    .get("task_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    return Some(sanitize_text(&id, 160));
                }
                pending.extend(map.iter().rev().map(|(_, value)| value));
            }
            Value::Array(items) => pending.extend(items.iter().rev()),
            _ => {}
        }
    }
    None
}

fn last_section(text: &str, section: &str) -> Option<String> {
    let stamped = Regex::new(r"^-\s+\d{4}-\d\d-\d\dT[0-9:]+Z\s+\[[^\]]+\]\s*").expect("pattern");
    let mut inside = false;
    let mut value = None;
    for line in crate::unicode::text_lines(text) {
        if line == section {
            inside = true;
            continue;
        }
        if inside && line.starts_with("## ") {
            break;
        }
        if inside && !line.trim().is_empty() {
            let clean = stamped.replace(line.trim(), "");
            value = Some(clean.strip_prefix("- ").unwrap_or(&clean).trim().to_owned());
        }
    }
    value
}
fn task_payload(path: &Path, operation: &str, occurred: &str) -> Result<Value, String> {
    let bytes = read_source(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let pattern = Regex::new(r"^-\s+([a-z_]+):\s*(.*)$").expect("pattern");
    let mut metadata = Map::new();
    for line in crate::unicode::text_lines(&text).take_while(|l| !l.starts_with("## ")) {
        if let Some(c) = pattern.captures(line) {
            metadata.insert(c[1].into(), Value::String(c[2].trim().into()));
        }
    }
    let task_id = metadata
        .get("task_id")
        .filter(|v| json_truthy(v))
        .map(python_str)
        .unwrap_or_else(|| format!("task-{}", &sha256_hex(text.as_bytes())[..16]));
    let goal = last_section(&text, "## Goal");
    let result = last_section(&text, "## Current State");
    let decision = last_section(&text, "## Decisions");
    let blocker = last_section(&text, "## Last Failure");
    let next = last_section(&text, "## Next Step");
    let verification = last_section(&text, "## Verification");
    let status = metadata
        .get("status")
        .map(|v| v.copy())
        .unwrap_or(Value::Null);
    let stable = json!({"task_id":task_id,"operation":operation,"goal":goal,"result":result,"decision":decision,"blocker":blocker,"next":next,"verification":verification,"status":status});
    let id = format!(
        "{task_id}:{operation}:{}",
        &sha256_hex(&canonical_json_bytes(&stable)?)[..16]
    );
    let verified = status.as_str().as_deref() == Some("verified");
    let verification_status = if operation == "verify" {
        if verified {
            "passed"
        } else if verification.as_ref().is_some_and(|v| v.contains("SKIPPED")) {
            "skipped"
        } else {
            "failed"
        }
    } else if operation == "close" && verified {
        "passed"
    } else {
        "not_verified"
    };
    let summary = result
        .or(goal.clone())
        .unwrap_or_else(|| format!("Agent State {operation}"));
    let mut payload = json!({"event_type":if operation=="close" {"task_outcome"} else {"agent_state"},"occurred_at":occurred,
        "source":{"type":"agent-task","id":id},"correlation":{"task_id":task_id,"operation_id":id},
        "outcome":{"summary":summary,"status":nonempty(&status).map(|v| v.copy()).unwrap_or(json!(operation))},
        "verification_status":verification_status,"evidence":[{"type":"agent-state","ref":task_id}]});
    for (key, value) in [
        ("goal", goal),
        ("decision", decision),
        ("blocker", blocker),
        ("next_action", next),
    ] {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            *payload.at_mut(key) = json!(value);
        }
    }
    Ok(payload)
}

pub fn build_source_payload(
    repo: &Path,
    o: &ObserveOverrides,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let path = o
        .source_file
        .as_deref()
        .ok_or("--source-file is required")?;
    let kind = o.source_type.as_str();
    let evidence = o.record_path.as_deref().unwrap_or(path);
    let occurred = o.occurred_at.clone().unwrap_or_else(|| utc_rfc3339(now));
    let supplied_id = o.source_id.as_deref().filter(|s| !s.is_empty());
    let mut payload = if kind == "agent-task" {
        task_payload(path, o.operation.as_deref().unwrap_or("update"), &occurred)?
    } else if [
        "autopilot",
        "goal-drive",
        "handoff",
        "intent",
        "oms-run",
        "patch-admit",
        "patch-land",
        "peer-review",
        "session-handoff",
    ]
    .contains(&kind)
    {
        let sid = supplied_id.map(str::to_owned).unwrap_or_else(|| {
            path.file_stem()
                .map(crate::sanitize::diagnostic_os_str)
                .unwrap_or_default()
        });
        let event = match kind {
            "autopilot" | "goal-drive" | "oms-run" | "patch-land" => "phase_outcome",
            "handoff" | "session-handoff" => "handoff",
            "intent" => "annotation",
            "patch-admit" => "patch_admit",
            _ => "patch_review",
        };
        let summary = if let Some(summary) = &o.outcome {
            summary.clone()
        } else if ["handoff", "session-handoff"].contains(&kind) {
            handoff_summary(path)?
        } else {
            kind.replace('-', " ")
        };
        let mut payload = json!({"event_type":event,"occurred_at":occurred,"source":{"type":kind,"id":sid},
            "outcome":{"summary":summary,"status":o.outcome_status.as_deref().unwrap_or("recorded")},
            "verification_status":o.verification_status.as_deref().unwrap_or(if ["handoff","session-handoff"].contains(&kind){"not_applicable"}else{"not_verified"}),
            "evidence":[{"type":kind,"ref":sid}]});
        if kind == "peer-review" {
            if let Some(task) = infer_review_task_id(repo, path, "") {
                *payload.at_mut("correlation") = json!({"task_id":task});
                *payload.at_mut("outcome").at_mut("summary") = json!(format!("{summary}: {task}"));
            }
        }
        if let Some(relative) = relative(repo, evidence) {
            *payload.at_mut("refs") = json!([{"type":kind,"path":relative,"id":sid}]);
        }
        payload
    } else {
        let row = read_json(path)?;
        let occurred = nonempty(&row.at("ts"))
            .map(|v| v.copy())
            .unwrap_or(json!(occurred));
        match kind {
            "panel-result" => {
                let sid = supplied_id.map(str::to_owned).map(Ok).unwrap_or_else(|| {
                    text(&first(&[
                        &row.at("revision"),
                        &row.at("task_id"),
                        &json!(
                            path.file_stem()
                                .map(crate::sanitize::diagnostic_os_str)
                                .unwrap_or_default()
                        ),
                    ]))
                })?;
                let task = sanitize_text(&text(&row.at("task_id"))?, 160);
                let summary =
                    sanitize_text(text(&row.at("summary"))?.lines().next().unwrap_or(""), 120);
                let status = nonempty(&row.at("outcome"))
                    .map(|v| v.copy())
                    .unwrap_or(json!("completed"));
                let panel_summary = if let Some(summary) = &o.outcome {
                    summary.clone()
                } else {
                    format!(
                        "Task {} {}: {summary}",
                        if task.is_empty() { "unknown" } else { &task },
                        python_str_checked(&status)?
                    )
                };
                json!({"event_type":"phase_outcome","occurred_at":o.occurred_at.clone().unwrap_or_else(||utc_rfc3339(now)),"source":{"type":kind,"id":sid},"correlation":{"task_id":task},
                    "outcome":{"summary":panel_summary,"status":status},
                    "verification_status":"not_verified","evidence":[{"type":kind,"ref":sid}]})
            }
            "run-ledger" => {
                let sid = if let Some(id) = supplied_id {
                    id.to_owned()
                } else if json_truthy(&row.at("id")) {
                    python_str_checked(&row.at("id"))?
                } else {
                    let stable: Map = crate::json::object(
                        [
                            "git_sha",
                            "dirty_hash",
                            "cmd",
                            "exit",
                            "duration_s",
                            "note",
                            "slurm_job_id",
                        ]
                        .into_iter()
                        .map(|k| (k.into(), row.at(k).copy())),
                    );
                    format!(
                        "legacy-{}",
                        &sha256_hex(&canonical_json_bytes(&Value::Object(stable))?)[..24]
                    )
                };
                let exit = &row.at("exit");
                let research = &row.at("research");
                let note = sanitize_text(&text(&row.at("note"))?, 800);
                let summary = if json_truthy(&research.at("question")) {
                    format!(
                        "Research run: {}",
                        python_str_checked(&research.at("question"))?
                    )
                } else if !note.is_empty() {
                    note
                } else {
                    format!("Command completed with exit {}", python_str_checked(exit)?)
                };
                let mut payload = json!({"event_type":"run","occurred_at":occurred,"source":{"type":kind,"id":sid},"correlation":{"run_id":row.at("run_id"),"operation_id":row.at("operation_id")},
                    "outcome":{"summary":summary,"status":if zero_exit(exit){"success"}else{"failure"}},"verification_status":if !exit.is_null()&&!zero_exit(exit){"failed"}else{"not_verified"},
                    "evidence":[{"type":"run-ledger","ref":source_ref(repo,evidence,&sid)}],"provenance":{"code_commit":row.at("git_sha"),"job_id":row.at("slurm_job_id"),"command_summary":command_summary(&row.at("cmd"))?,"exit_code":exit,"duration_seconds":row.at("duration_s"),
                    "hypothesis":research.at("hypothesis"),"prediction":research.at("prediction"),"baseline":research.at("baseline"),"primary_metric":research.at("metric"),"success_criterion":research.at("success"),"independent_change":research.at("change")}});
                if json_truthy(&research.at("question")) {
                    *payload.at_mut("goal") = research.at("question").copy();
                }
                let metrics = metrics(&row.at("metrics"), &sid);
                if !metrics.is_empty() {
                    *payload.at_mut("metrics") = json!(metrics);
                }
                payload
            }
            "run-capsule" => {
                let sid = supplied_id
                    .map(str::to_owned)
                    .map(Ok)
                    .unwrap_or_else(|| text(&row.at("id")))?;
                let result = &row.at("result");
                let exit = &result.at("exit");
                let mut payload = json!({"event_type":"experiment","occurred_at":occurred,"source":{"type":kind,"id":sid},"correlation":{"run_id":sid},
                    "outcome":{"summary":nonempty(&row.at("note")).map(|v| v.copy()).unwrap_or(json!("Experiment run completed")),"status":if zero_exit(exit){"success"}else{"failure"}},
                    "verification_status":if !exit.is_null()&&!zero_exit(exit){"failed"}else{"not_verified"},"evidence":[{"type":"capsule","ref":source_ref(repo,evidence,&sid)}],
                    "provenance":{"run_id":sid,"job_id":row.at("slurm_job_id"),"seed":row.at("seeds").as_array().and_then(|a|a.first()),"code_commit":first(&[&row.at("git").at("commit"),&row.at("git").at("commit_short")]),
                    "command_summary":command_summary(&row.at("command"))?,"exit_code":exit,"duration_seconds":result.at("duration_s"),"gpu":row.at("env").at("gpu"),"execution_environment":row.at("env").at("platform")}});
                let refs: Vec<Value> = [("configs", "config"), ("outputs", "artifact")]
                    .into_iter()
                    .flat_map(|(field, kind)| {
                        row.at(field)
                            .as_array()
                            .into_iter()
                            .flatten()
                            .take(64)
                            .filter(|r| r.is_object())
                            .map(move |r| json!({"type":kind,"path":r.at("path"),"id":r.at("sha256")}))
                    })
                    .collect();
                if !refs.is_empty() {
                    *payload.at_mut("refs") = json!(refs);
                }
                if result.at("metrics").is_object() {
                    *payload.at_mut("metrics") = json!(metrics(&result.at("metrics"), &sid));
                }
                payload
            }
            "run-reconcile" => {
                let sid = supplied_id
                    .map(str::to_owned)
                    .map(Ok)
                    .unwrap_or_else(|| text(&row.at("job_id")))?;
                let state = text(&row.at("state"))?;
                json!({"event_type":"job","occurred_at":o.occurred_at.clone().map(|s| Value::String(s.as_str().into())).unwrap_or_else(||nonempty(&row.at("reconciled_at")).map(|v| v.copy()).unwrap_or(json!(utc_rfc3339(now)))),"source":{"type":kind,"id":sid},
                    "outcome":{"summary":format!("Job {sid} finished: {state}"),"status":if state=="COMPLETED"{"success"}else{"failure"}},"verification_status":if state=="COMPLETED"{"not_verified"}else{"failed"},
                    "evidence":[{"type":"job-reconcile","ref":source_ref(repo,evidence,&sid)}],"provenance":{"job_id":sid,"exit_code":row.at("exit_code"),"duration_seconds":row.at("elapsed")}})
            }
            "experiment-board" => {
                let id = text(&row.at("id"))?;
                let state = text(&row.at("status"))?;
                let sid = supplied_id
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{id}:{state}"));
                let result = first(&[&row.at("result"), &row.at("reason")]);
                let mut payload = json!({"event_type":"experiment","occurred_at":occurred,"source":{"type":kind,"id":sid},"outcome":{"summary":if json_truthy(&result){format!("Experiment {id} {state}: {}",python_str_checked(&result)?)}else{format!("Experiment {id} {state}")},"status":state},
                    "verification_status":"not_verified","evidence":[{"type":kind,"ref":source_ref(repo,evidence,&sid)}],"provenance":{"job_id":row.at("job"),"verdict":row.at("result")}});
                for (field, key) in [("reason", "blocker"), ("next", "next_action")] {
                    if json_truthy(&row.at(field)) {
                        *payload.at_mut(key) = row.at(field).copy();
                    }
                }
                payload
            }
            "ci-status" => {
                let conclusion = text(&first(&[&row.at("conclusion"), &row.at("status")]))?;
                let run = first(&[&row.at("databaseId"), &row.at("run_id"), &row.at("id")]);
                let url = &row.at("url");
                let attempt = &row.at("attempt");
                let sid = if let Some(id) = supplied_id {
                    id.to_owned()
                } else if json_truthy(&run) || json_truthy(url) {
                    let identity = if json_truthy(&run) {
                        format!(
                            "{}:run:{}",
                            python_str_checked(&row.at("sha"))?,
                            python_str_checked(&run)?
                        )
                    } else {
                        format!(
                            "{}:run-url:{}",
                            python_str_checked(&row.at("sha"))?,
                            python_str_checked(url)?
                        )
                    };
                    if !attempt.is_null() {
                        format!(
                            "{identity}:attempt:{}:{conclusion}",
                            python_str_checked(attempt)?
                        )
                    } else {
                        format!("{identity}:{conclusion}")
                    }
                } else {
                    format!("{}:{conclusion}", python_str_checked(&row.at("sha"))?)
                };
                let failed = ["failure", "timed_out", "cancelled", "startup_failure"]
                    .contains(&conclusion.as_str());
                let (status, verification) = if conclusion == "success" {
                    ("success", "passed")
                } else if failed {
                    ("failure", "failed")
                } else {
                    ("pending", "not_verified")
                };
                let mut refs = vec![
                    json!({"type":"commit","id":row.at("sha")}),
                    json!({"type":"ci-run","url":url}),
                ];
                if json_truthy(&row.at("pr_number")) || json_truthy(&row.at("pr_url")) {
                    refs.push(
                        json!({"type":"pull_request","id":row.at("pr_number"),"url":row.at("pr_url")}),
                    );
                }
                json!({"event_type":"ci","occurred_at":occurred,"source":{"type":kind,"id":sid},"outcome":{"summary":format!("CI {conclusion} for commit {}",python_str_checked(&row.at("sha"))?),"status":status},"verification_status":verification,
                    "refs":refs,"evidence":[{"type":"ci","ref":source_ref(repo,evidence,&sid)}],"provenance":{"workflow":row.at("workflow"),"attempt":attempt,"run_id":run}})
            }
            _ => {
                let sid = supplied_id
                    .map(str::to_owned)
                    .map(Ok)
                    .unwrap_or_else(|| text(&first(&[&row.at("id"), &row.at("event_id")])))?;
                json!({"event_type":o.event_type.as_deref().unwrap_or("annotation"),"occurred_at":occurred,"source":{"type":kind,"id":sid},"outcome":{"summary":o.outcome.clone().map(|s| Value::String(s.as_str().into())).unwrap_or_else(||nonempty(&row.at("summary")).map(|v| v.copy()).unwrap_or(json!(kind))),
                    "status":o.outcome_status.clone().map(|s| Value::String(s.as_str().into())).unwrap_or_else(||nonempty(&row.at("status")).map(|v| v.copy()).unwrap_or(json!("recorded")))},"verification_status":"not_verified","evidence":[{"type":kind,"ref":sid}]})
            }
        }
    };
    for (field, value) in [
        ("event_type", &o.event_type),
        ("occurred_at", &o.occurred_at),
        ("verification_status", &o.verification_status),
        ("decision", &o.decision),
        ("blocker", &o.blocker),
        ("next_action", &o.next_action),
    ] {
        if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
            *payload.at_mut(field) = json!(value);
        }
    }
    if let Some(value) = supplied_id {
        *payload.at_mut("source").at_mut("id") = json!(value);
    }
    if let Some(value) = o.operation_id.as_ref().filter(|v| !v.is_empty()) {
        if !payload.at("correlation").is_object() {
            *payload.at_mut("correlation") = json!({});
        }
        *payload.at_mut("correlation").at_mut("operation_id") = json!(value);
    }
    if let Some(value) = o.outcome.as_ref().filter(|v| !v.is_empty()) {
        *payload.at_mut("outcome").at_mut("summary") = json!(if kind == "peer-review"
            && json_truthy(&payload.at("correlation").at("task_id"))
        {
            format!(
                "{value}: {}",
                python_str_checked(&payload.at("correlation").at("task_id"))?
            )
        } else {
            value.clone()
        });
    }
    if let Some(value) = o.outcome_status.as_ref().filter(|v| !v.is_empty()) {
        *payload.at_mut("outcome").at_mut("status") = json!(value);
    }
    Ok(payload)
}

pub(crate) fn enrich_reviews(repo: &Path, events: Vec<Value>) -> Vec<Value> {
    events
        .into_iter()
        .map(|mut event| {
            if event.at("source").at("type").as_str().as_deref() == Some("peer-review")
                && !json_truthy(&event.at("correlation").at("task_id"))
            {
                let path = event
                    .at("refs")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|r| {
                        r.at("type").as_str().as_deref() == Some("peer-review")
                            && json_truthy(&r.at("path"))
                    })
                    .and_then(|r| r.at("path").as_str());
                if let Some(task) =
                    path.and_then(|path| infer_review_task_id(repo, &repo.join(path), ""))
                {
                    if !event.at("correlation").is_object() {
                        *event.at_mut("correlation") = json!({});
                    }
                    *event.at_mut("correlation").at_mut("task_id") = json!(task);
                    let summary = if json_truthy(&event.at("outcome").at("summary")) {
                        python_str(&event.at("outcome").at("summary"))
                    } else {
                        "Peer review outcome".into()
                    };
                    if !event.at("outcome").is_object() {
                        *event.at_mut("outcome") = json!({});
                    }
                    *event.at_mut("outcome").at_mut("summary") =
                        json!(if summary.ends_with(&format!(": {task}")) {
                            summary
                        } else {
                            format!("{summary}: {task}")
                        });
                }
            }
            event
        })
        .collect()
}
