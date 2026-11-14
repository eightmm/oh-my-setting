//! Owner-scoped result publication with two-phase verifier rechecks.
use crate::services::{ExecutionRequest, NativeCommand, query};
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;
pub const MAX_RESULT: usize = 1024 * 1024;
pub const MAX_DRAFT: usize = 4096;
pub const PATCH_MAX: usize = 16 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct FinalizeRequest {
    pub owner: String,
    pub task_id: String,
    pub main_attempt_id: String,
    pub summary_file: PathBuf,
    pub outcome: String,
    pub verify: Option<String>,
    pub evidence: Vec<PathBuf>,
    pub verify_timeout: Duration,
    pub execution_repo: PathBuf,
    pub notify: bool,
    pub landed_commit: Option<String>,
}
#[derive(Clone, Debug)]
pub struct AcceptRequest {
    pub owner: String,
    pub task_id: String,
    pub main_attempt_id: String,
    pub message: String,
    pub verify: String,
    pub execution_repo: PathBuf,
    pub expected_patch_sha256: Option<String>,
    pub allow_verifier_change: bool,
    pub allow_floor_change: Option<String>,
}
pub fn verified_patch(store: &Store, row: &Value) -> Result<Vec<u8>> {
    let path = row["patch"]
        .as_str()
        .ok_or_else(|| Error::new("patch path missing"))?;
    require(
        path.starts_with(".oms/artifacts/")
            && crate::hex(row["patch_sha256"].as_str().unwrap_or_default(), 64),
        "invalid indexed patch identity",
    )?;
    let body = store.read(Path::new(path), PATCH_MAX, false)?;
    require(
        crate::digest(&body) == row["patch_sha256"],
        "indexed patch digest changed",
    )?;
    Ok(body)
}
pub fn safe_summary(store: &Store, path: &Path) -> Result<String> {
    let body = crate::storage::input(store.repo(), path, MAX_DRAFT)?;
    let text = String::from_utf8(body).map_err(Error::new)?;
    require(
        !text.trim().is_empty() && !text.contains('\0'),
        "summary is empty or contains NUL",
    )?;
    crate::outbound(text.as_bytes())?;
    Ok(text)
}
pub fn evidence_reference(store: &Store, path: &Path) -> Result<Value> {
    let bytes = store.read(path, MAX_RESULT, false)?;
    Ok(serde_json::json!({"path":path,"sha256":crate::digest(&bytes),"bytes":bytes.len()}))
}
pub fn finalize_snapshot<S: Services + ?Sized>(
    repo: &Path,
    request: &FinalizeRequest,
    service: &S,
    stop: &AtomicBool,
) -> Result<Value> {
    let attempts = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let main = array(&attempts)
        .iter()
        .find(|v| v["attempt_id"] == request.main_attempt_id)
        .ok_or_else(|| Error::new("finalizing main is not recorded"))?;
    require(
        main["tool"] == "panel-main"
            && main["provider"] == request.owner
            && main["terminal"] == false,
        "finalizing main identity is not live/owned",
    )?;
    let related: Vec<_> = array(&attempts)
        .iter()
        .filter(|v| v["task_id"] == request.task_id)
        .cloned()
        .collect();
    require(!related.is_empty(), "task has no recorded attempts")?;
    require(
        related
            .iter()
            .filter(|v| v["refs"]["panel_role"] == "worker")
            .all(|v| v["parent_attempt_id"] == request.main_attempt_id),
        "task includes workers owned by another main",
    )?;
    require(
        related.iter().all(|v| v["terminal"] == true),
        "task has live or unconfirmed attempts",
    )?;
    let store = Store::open(repo)?;
    let plan = store
        .json(Path::new(".oms/plan/tasks.json"), 4 * 1024 * 1024, true)?
        .unwrap_or(Value::Null);
    let task = plan["tasks"][&request.task_id].clone();
    if !task.is_null() {
        require(
            task["owner"]
                .as_str()
                .is_none_or(|v| v == request.owner || v == request.main_attempt_id),
            "plan task owner differs",
        )?;
    }
    if request.outcome == "accepted" || request.landed_commit.is_some() {
        require(
            task["state"] == "done",
            "accepted requires native plan completion/publication proof",
        )?;
        if let Some(commit) = &request.landed_commit {
            require(
                task["landed_commit"] == *commit || task["completion"]["landed_commit"] == *commit,
                "landed commit does not match canonical plan completion",
            )?;
        }
    }
    let evidence = request
        .evidence
        .iter()
        .map(|p| evidence_reference(&store, p))
        .collect::<Result<Vec<_>>>()?;
    let index = store.read(
        Path::new(".oms/artifacts/index.jsonl"),
        16 * 1024 * 1024,
        true,
    )?;
    let mut result_rows = vec![];
    for line in index.split(|&b| b == b'\n').filter(|v| !v.is_empty()) {
        let row = crate::decode(line, MAX_RESULT)?;
        if row["task_id"] == request.task_id {
            result_rows.push(row);
        }
    }
    Ok(
        serde_json::json!({"main":main,"binding":task,"related_attempts":related,"related_rows":result_rows,"evidence":evidence}),
    )
}
pub fn finalize(
    repo: &Path,
    request: &FinalizeRequest,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    crate::identifier(&request.task_id)?;
    crate::identifier(&request.main_attempt_id)?;
    require(
        ["codex", "claude", "pi", "antigravity"].contains(&request.owner.as_str())
            && ["completed", "accepted", "failed", "rejected"].contains(&request.outcome.as_str()),
        "invalid owner result",
    )?;
    require(
        !request.verify_timeout.is_zero()
            && request.verify_timeout <= Duration::from_secs(86400)
            && request.evidence.len() <= 8,
        "invalid verify timeout/evidence count",
    )?;
    require(
        request.landed_commit.as_deref().is_none_or(|v| {
            (crate::hex(v, 40) || crate::hex(v, 64))
                && ["accepted", "completed"].contains(&request.outcome.as_str())
        }),
        "invalid landed commit/outcome",
    )?;
    let requires_acceptance = request.outcome == "accepted" || request.landed_commit.is_some();
    require(
        !requires_acceptance
            || (request
                .verify
                .as_ref()
                .is_some_and(|v| !v.trim().is_empty())
                && !request.evidence.is_empty()),
        "accepted requires evidence and a passing verifier",
    )?;
    let store = Store::open(repo)?;
    let summary = safe_summary(&store, &request.summary_file)?;
    let before = {
        let _tx = store.transaction("panel-results", Duration::from_secs(10))?;
        service.finalize_authority(repo, request, stop)?
    };
    let mut verification = serde_json::json!({"status":"not_run"});
    if let Some(command) = &request.verify {
        require(
            !command.trim().is_empty() && !command.contains('\0'),
            "verifier is blank or contains NUL",
        )?;
        let mut execution = ExecutionRequest::new(
            vec!["bash".into(), "-c".into(), command.into()],
            &request.execution_repo,
        );
        execution.timeout = request.verify_timeout;
        execution.output_limit = 8192;
        let result = service.execute(&execution, stop)?;
        let passed = result.code == 0 && result.cleanup_complete && result.output_complete;
        verification = serde_json::json!({"status":if passed {"passed"} else {"failed"},"exit":result.code,"command_sha256":crate::digest(command.as_bytes())});
        if requires_acceptance {
            require(
                passed,
                "accepted verifier failed or process cleanup is unconfirmed",
            )?;
        }
    }
    crate::checkpoint(stop)?;
    let tx = store.transaction("panel-results", Duration::from_secs(10))?;
    let after = service.finalize_authority(repo, request, stop)?;
    require(
        after == before,
        "result authority/evidence changed during verification",
    )?;
    let mut payload = serde_json::json!({"schema":1,"kind":"oms-panel-result","task_id":request.task_id,"owner":request.owner,
        "outcome":request.outcome,"summary":summary,"summary_sha256":crate::digest(summary.as_bytes()),"verification":verification,
        "evidence":after["evidence"],"main_attempt_id":request.main_attempt_id,"plan_binding":after["binding"],
        "attempt_ids":array(&after["related_attempts"]).iter().filter_map(|v| v["attempt_id"].as_str()).collect::<Vec<_>>()});
    let revision = crate::digest(&crate::value::encoded(&payload)?)[..24].to_owned();
    payload["revision"] = revision.clone().into();
    let relative = format!(
        ".oms/artifacts/panel-results/result-{}-{revision}.json",
        request.task_id
    );
    tx.write_json(Path::new(&relative), &payload, true, MAX_RESULT)?;
    // Artifact registration is a separate durable producer operation. A failure
    // leaves the immutable artifact, never authorization to repeat verification.
    let mut registration = payload.clone();
    registration["artifact"] = relative.clone().into();
    if !index_rows(repo)
        .map_err(|e| Error {
            publication_uncertain: true,
            ..e
        })?
        .iter()
        .any(|row| {
            row["kind"] == "panel-result"
                && row["task_id"] == request.task_id
                && row["artifact"] == relative
        })
    {
        service
            .register_result(repo, &registration, stop)
            .map_err(|e| Error {
                code: e.code,
                publication_uncertain: true,
                message: format!(
                    "result {relative} published; registration pending: {}",
                    e.message
                ),
            })?;
    }
    resolve_failed_outcomes(repo, request, &relative, &revision, service, stop).map_err(|e| {
        Error {
            publication_uncertain: true,
            ..e
        }
    })?;
    payload["artifact"] = relative.into();
    if request.notify {
        let delivery = deliver(&store, &tx, &payload, false, service, stop).map_err(|e| Error {
            publication_uncertain: true,
            ..e
        })?;
        payload["delivery"] = delivery["receipt"].clone();
    }
    Ok(payload)
}
pub fn accept(
    repo: &Path,
    request: &AcceptRequest,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    validate_accept(request)?;
    service.accept(repo, request, stop)
}
pub(crate) fn validate_accept(request: &AcceptRequest) -> Result<()> {
    crate::parent_only()?;
    crate::identifier(&request.task_id)?;
    crate::identifier(&request.main_attempt_id)?;
    require(
        ["codex", "claude", "pi", "antigravity"].contains(&request.owner.as_str()),
        "invalid accept owner",
    )?;
    require(
        !request.message.contains('\0')
            && !request.verify.contains('\0')
            && request.message.len() <= crate::dispatch::MAX_PROMPT
            && request.verify.len() <= crate::dispatch::MAX_PROMPT,
        "accept input is oversized or contains NUL",
    )?;
    require(
        !request.message.trim().is_empty() && !request.verify.trim().is_empty(),
        "accept requires a commit message and verifier",
    )?;
    require(
        request
            .expected_patch_sha256
            .as_deref()
            .is_none_or(|v| crate::hex(v, 64)),
        "expected patch digest must be lowercase SHA-256",
    )?;
    require(
        request
            .allow_floor_change
            .as_ref()
            .is_none_or(|v| !v.trim().is_empty() && !v.chars().any(char::is_control)),
        "floor-change reason must be a nonblank line",
    )?;
    crate::outbound(request.message.as_bytes())?;
    Ok(())
}
pub fn outcomes(repo: &Path, tasks: &[String]) -> Result<Vec<Value>> {
    let store = Store::open(repo)?;
    let raw = oms_command_telemetry::store::read_index(repo, None, true)
        .map_err(crate::services::telemetry_error)?;
    let mut newest = std::collections::BTreeMap::new();
    for line in raw.split(|&v| v == b'\n').filter(|v| !v.is_empty()) {
        let row = crate::decode(line, MAX_RESULT)?;
        let task = text(&row, "task_id");
        if row["kind"] != "panel-result" || !tasks.contains(&task) {
            continue;
        }
        let value = load_result(&store, &row)?;
        newest.insert(task, value);
    }
    Ok(newest.into_values().collect())
}
pub(crate) fn index_rows(repo: &Path) -> Result<Vec<Value>> {
    let raw = oms_command_telemetry::store::read_index(repo, None, true)
        .map_err(crate::services::telemetry_error)?;
    raw.split(|&v| v == b'\n')
        .filter(|v| !v.is_empty())
        .map(|v| crate::decode(v, MAX_RESULT))
        .collect()
}
fn resolve_failed_outcomes(
    repo: &Path,
    request: &FinalizeRequest,
    relative: &str,
    revision: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let rows = index_rows(repo)?;
    let Some(boundary) = rows.iter().rposition(|v| {
        v["kind"] == "panel-result" && v["task_id"] == request.task_id && v["artifact"] == relative
    }) else {
        return Ok(());
    };
    let attempts = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let linked = array(&attempts)
        .iter()
        .filter(|v| v["task_id"] == request.task_id)
        .filter_map(|v| v["attempt_id"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let resolved = rows
        .iter()
        .filter(|v| v["kind"] == "artifact-resolution")
        .filter_map(|v| v["resolves_event_id"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let ids = rows[..boundary]
        .iter()
        .filter(|v| {
            v["kind"] != "artifact-resolution"
                && v["kind"] != "panel-result"
                && v["exit"].as_i64().is_some_and(|n| n != 0)
                && (v["task_id"] == request.task_id
                    || (v["task_id"].as_str().is_none_or(str::is_empty)
                        && v["attempt_id"]
                            .as_str()
                            .is_some_and(|id| linked.contains(id))))
        })
        .filter_map(|v| v["event_id"].as_str())
        .filter(|id| !resolved.contains(id))
        .collect::<Vec<_>>();
    let reason = format!(
        "owner finalize {}: result revision {revision}",
        request.outcome
    );
    for batch in ids.chunks(100) {
        let raw = oms_command_telemetry::store::read_index(repo, None, true)
            .map_err(crate::services::telemetry_error)?;
        let current = raw
            .split(|v| *v == b'\n')
            .filter(|v| !v.is_empty())
            .map(|v| crate::decode(v, MAX_RESULT))
            .collect::<Result<Vec<_>>>()?;
        let done = current
            .iter()
            .filter(|v| v["kind"] == "artifact-resolution")
            .filter_map(|v| v["resolves_event_id"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let mut lines = Vec::new();
        for id in batch {
            if done.contains(id) {
                continue;
            }
            let target = current
                .iter()
                .find(|v| v["event_id"] == *id)
                .ok_or_else(|| Error::new("failed outcome disappeared before resolution"))?;
            require(
                target["schema"] == 1
                    && target["exit"].as_u64().is_some_and(|v| v > 0)
                    && ["operation_id", "artifact_id", "ts", "kind"]
                        .iter()
                        .all(|key| target[*key].as_str().is_some_and(|v| !v.is_empty()))
                    && target["provider"].is_string(),
                "resolve target must be a complete schema-1 failure; migrate legacy evidence first",
            )?;
            let row = serde_json::json!({"schema":1,"kind":"artifact-resolution","event_id":format!("evt_resolve_{}",crate::value::unique()?),"operation_id":target["operation_id"],"artifact_id":target["artifact_id"],"ts":crate::now(),"provider":request.owner,"exit":0,"parent_event_id":id,"resolves_event_id":id,"resolution":"resolved","reason":reason});
            lines.push(crate::value::encoded(&row)?);
        }
        let borrowed = lines.iter().map(Vec::as_slice).collect::<Vec<_>>();
        oms_command_telemetry::store::append_rows(
            repo,
            None,
            &borrowed,
            &oms_command_telemetry::store::AppendOptions {
                expected: Some(raw),
                ..Default::default()
            },
            stop,
        )
        .map_err(crate::services::telemetry_error)?;
    }
    Ok(())
}
#[derive(Clone, Debug)]
pub struct ResultsRequest {
    pub task_id: Option<String>,
    pub room_participant: Option<String>,
    pub offset: usize,
    pub limit: usize,
    pub include_preflight: bool,
}
impl Default for ResultsRequest {
    fn default() -> Self {
        Self {
            task_id: None,
            room_participant: None,
            offset: 0,
            limit: 20,
            include_preflight: false,
        }
    }
}
pub fn results(
    repo: &Path,
    task: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    results_page(
        repo,
        &ResultsRequest {
            task_id: task.map(str::to_owned),
            ..Default::default()
        },
        service,
        stop,
    )
}
fn load_result(store: &Store, row: &Value) -> Result<Value> {
    let path = text(row, "artifact");
    require(
        path.starts_with(".oms/artifacts/panel-results/"),
        "result artifact outside result store",
    )?;
    let body = store.read(Path::new(&path), MAX_RESULT, false)?;
    require(
        row["artifact_sha256"]
            .as_str()
            .is_some_and(|v| crate::hex(v, 64) && crate::digest(&body) == v),
        "result artifact digest differs",
    )?;
    let value = crate::decode(&body, MAX_RESULT)?;
    require(
        value["schema"] == 1
            && value["kind"] == "oms-panel-result"
            && value["task_id"] == row["task_id"],
        "invalid result identity",
    )?;
    require(
        value["revision"]
            .as_str()
            .is_some_and(|v| crate::hex(v, 24))
            && value["summary"].as_str().is_some_and(|summary| {
                value["summary_sha256"] == crate::digest(summary.as_bytes())
            }),
        "invalid result revision or summary digest",
    )?;
    Ok(value)
}
/// Canonical task rows, paged after participant filtering. Missing or corrupt
/// evidence is surfaced in `issue`; it cannot manufacture an owner outcome.
pub fn results_page(
    repo: &Path,
    request: &ResultsRequest,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    require(request.limit > 0, "invalid results page")?;
    if let Some(task) = &request.task_id {
        crate::identifier(task)?;
    }
    if let Some(member) = &request.room_participant {
        crate::identifier(member)?;
    }
    let mut args = crate::services::args(&["list", "--json"]);
    if request.task_id.is_none() {
        args.extend(["--limit".into(), "300".into()]);
    }
    let attempts = service
        .command(NativeCommand::Events, repo, &args, stop)?
        .json()?;
    require(attempts.is_array(), "invalid lifecycle attempt projection")?;
    let indexed = index_rows(repo)?;
    let store = Store::open(repo)?;
    let mut latest = Vec::new();
    let mut chronology = array(&attempts)
        .iter()
        .chain(indexed.iter())
        .collect::<Vec<_>>();
    chronology.sort_by_key(|v| {
        v["updated_at"]
            .as_str()
            .or_else(|| v["ts"].as_str())
            .unwrap_or_default()
    });
    for row in chronology {
        let task = text(row, "task_id");
        if crate::identifier(&task).is_err()
            || (!request.include_preflight && task.starts_with("preflight-"))
        {
            continue;
        }
        latest.retain(|v| v != &task);
        latest.push(task);
    }
    latest.reverse();
    let mut output = Vec::new();
    let mut passed = 0;
    let mut capped = false;
    for task in latest {
        if request.task_id.as_ref().is_some_and(|v| v != &task) {
            continue;
        }
        let matching = array(&attempts)
            .iter()
            .filter(|v| {
                v["task_id"] == task
                    && request
                        .room_participant
                        .as_ref()
                        .is_none_or(|p| v["refs"]["panel_room_participant"] == *p)
            })
            .collect::<Vec<_>>();
        if request.room_participant.is_some() && matching.is_empty() {
            continue;
        }
        let linked = matching
            .iter()
            .filter_map(|v| v["attempt_id"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let rows = indexed
            .iter()
            .filter(|v| {
                v["task_id"] == task
                    && (request.room_participant.is_none()
                        || v["attempt_id"].as_str().is_some_and(|a| linked.contains(a)))
            })
            .collect::<Vec<_>>();
        passed += 1;
        if request.task_id.is_none() && passed <= request.offset {
            continue;
        }
        if request.task_id.is_none() && output.len() >= request.limit {
            capped = true;
            break;
        }
        let last = matching.last().copied().cloned().unwrap_or(Value::Null);
        let metadata = &last["refs"];
        let result_row = indexed
            .iter()
            .rev()
            .find(|v| v["task_id"] == task && v["kind"] == "panel-result");
        let mut issue = Value::Null;
        let result = match result_row.map(|v| load_result(&store, v)).transpose() {
            Ok(value) => value.unwrap_or(Value::Null),
            Err(_) => {
                issue = "result or delivery evidence unavailable".into();
                Value::Null
            }
        };
        let mut calls = Vec::new();
        for row in rows.iter().rev().take(30).rev() {
            if ![
                "ask",
                "call",
                "consult",
                "delegate",
                "review",
                "ask-synthesis",
                "patch-admit",
                "patch-land",
            ]
            .contains(&text(row, "kind").as_str())
            {
                continue;
            }
            let mut call = serde_json::Map::new();
            for key in [
                "kind",
                "exit",
                "verify_exit",
                "verify_status",
                "state",
                "reason_code",
                "guard",
                "provider_error_exit",
                "artifact",
                "patch",
                "patch_sha256",
                "event_id",
                "selected_model",
                "requested_model",
                "served_model",
                "attempt_id",
                "tokens",
                "provider_usage",
                "base_sha",
                "answer_quality",
            ] {
                if let Some(value) = row.get(key) {
                    call.insert(key.into(), value.clone());
                }
            }
            let attempt = matching
                .iter()
                .find(|v| v["attempt_id"] == row["attempt_id"]);
            if let Some(alias) = attempt.and_then(|v| v["refs"]["panel_alias"].as_str()) {
                call.insert("alias".into(), alias.into());
            }
            if row["patch"].is_string() {
                let changes =
                    verified_patch(&store, row).and_then(|body| crate::files::patch_changes(&body));
                call.insert(
                    "changes".into(),
                    match changes {
                        Ok(v) => serde_json::json!({"files":v}),
                        Err(_) => serde_json::json!({"issue":"patch evidence unavailable"}),
                    },
                );
            }
            calls.push(Value::Object(call));
        }
        let mut stages = serde_json::json!({});
        for (stage, kinds, exit_key) in [
            (
                "implement",
                &["delegate", "call", "ask", "consult"][..],
                "exit",
            ),
            ("verify", &["delegate", "review"][..], "verify_exit"),
        ] {
            let values = calls
                .iter()
                .filter(|v| kinds.contains(&text(v, "kind").as_str()) && !v[exit_key].is_null())
                .collect::<Vec<_>>();
            let failed = values.iter().any(|v| v[exit_key] != 0);
            let last = values.last();
            stages[stage] = serde_json::json!({"state":if failed {"failed"} else if values.is_empty() {"not_run"} else {"passed"},"attempt_id":last.map(|v| &v["attempt_id"]),"exit":last.map(|v| &v[exit_key])});
        }
        let review = rows.iter().rev().find(|v| v["kind"] == "review-outcome");
        let review_state = review
            .map(|v| {
                if v["review"]["overall"] == 2 {
                    "inconclusive"
                } else if v["exit"].as_i64().is_some_and(|n| n != 0)
                    || v["review"]["gate_verify_exit"]
                        .as_i64()
                        .is_some_and(|n| n != 0)
                    || v["review"]["overall"] == 1
                {
                    "failed"
                } else if v["exit"] == 0 && v["review"]["overall"] == 0 {
                    "passed"
                } else {
                    "inconclusive"
                }
            })
            .unwrap_or_else(|| {
                if calls.iter().any(|v| v["kind"] == "review") {
                    "inconclusive"
                } else {
                    "not_run"
                }
            });
        stages["review"] = serde_json::json!({"state":review_state,"attempt_id":review.map(|v| &v["attempt_id"]),"exit":review.map(|v| &v["exit"])});
        let outcome = result["outcome"].as_str().unwrap_or("pending");
        stages["main_decision"] = serde_json::json!({"state":outcome});
        let failed = ["implement", "verify", "review", "main_decision"]
            .into_iter()
            .find(|stage| stages[*stage]["state"] == "failed");
        let (mut representative, mut action) = if let Some(stage) = failed {
            (format!("{stage} failed"), format!("retry {stage}"))
        } else if review_state == "inconclusive" {
            ("review inconclusive".into(), "retry review".into())
        } else {
            (
                outcome.into(),
                if outcome == "pending" {
                    "record main decision".into()
                } else {
                    "none".into()
                },
            )
        };
        let terminal_failure = ["cancelled", "failed", "abandoned", "timed_out"]
            .contains(&text(&last, "state").as_str());
        let termination_reason = if terminal_failure {
            last["reason_code"].clone()
        } else {
            Value::Null
        };
        if result.is_object() {
            representative = format!("main_decision {outcome}");
            action = "none".into();
        } else if terminal_failure && failed.is_none() && review_state != "inconclusive" {
            representative = termination_reason
                .as_str()
                .unwrap_or_else(|| last["state"].as_str().unwrap_or("unknown"))
                .into();
            action = if last["state"] == "failed" {
                "inspect failure and retry"
            } else {
                "salvage or re-dispatch"
            }
            .into();
        }
        let delivery = if result.is_object() {
            match last_delivery(
                &store,
                &indexed,
                &task,
                result["revision"].as_str().unwrap_or_default(),
            ) {
                Ok(value) => value,
                Err(_) => {
                    issue = "result or delivery evidence unavailable".into();
                    None
                }
            }
        } else {
            None
        };
        let admitted = result.is_null()
            && indexed
                .iter()
                .any(|v| v["task_id"] == task && v["kind"] == "panel-accept");
        let retained = calls.len().saturating_sub(8);
        let title = metadata["panel_label"]
            .as_str()
            .or_else(|| result["summary"].as_str().and_then(|v| v.lines().next()))
            .unwrap_or_default();
        output.push(serde_json::json!({"task_id":task,"title":crate::eventlog::clean(title,160),"model":metadata["panel_model"],"role":metadata["panel_role"],"location":metadata["panel_location"],"process_state":last["state"],"outcome":result["outcome"],"admitted":admitted,"verification":result["verification"],"acceptance":outcome == "accepted","summary":result["summary"],"revision":result["revision"],"artifact":result_row.map(|v| &v["artifact"]),"evidence":result.get("evidence").cloned().unwrap_or_else(|| serde_json::json!([])),"calls":&calls[retained..],"delivery":delivery.as_ref().and_then(|v| v["receipt"]["status"].as_str()).unwrap_or("not_requested"),"delivery_unknown":delivery.as_ref().is_some_and(|v| v["receipt"]["delivery_unknown"] == true),"issue":issue,"missing_result":result.is_null(),"stages":stages,"representative_state":representative,"next_action":action,"termination_reason":termination_reason}));
    }
    let missing_task = request.task_id.is_some() && output.is_empty();
    Ok(
        serde_json::json!({"schema":1,"kind":"oms-panel-results","rows":output,"coverage":format!("{}; all retained artifact events; previously pruned evidence unavailable",if request.task_id.is_some() { "all retained attempts" } else { "latest 300 attempts; attempt coverage may be incomplete" }),"truncated":capped || missing_task,"offset":request.offset,"omitted":if capped { Value::Null } else { 0.into() },"omitted_range":if capped { serde_json::json!([request.offset+output.len(),null]) } else { Value::Null }}),
    )
}

/// Native acceptance implementation shared by default and subcommand services.
pub fn accept_native<S: Services + ?Sized>(
    repo: &Path,
    request: &AcceptRequest,
    service: &S,
    stop: &AtomicBool,
) -> Result<Value> {
    validate_accept(request)?;
    crate::acceptance::run(repo, request, service, stop)
}

pub(crate) fn indexed_artifact(store: &Store, row: &Value) -> Result<Vec<u8>> {
    let path = text(row, "artifact");
    require(
        path.starts_with(".oms/artifacts/"),
        "indexed artifact outside evidence store",
    )?;
    let body = store.read(Path::new(&path), MAX_RESULT, false)?;
    require(
        row["artifact_sha256"]
            .as_str()
            .is_some_and(|v| crate::hex(v, 64) && crate::digest(&body) == v),
        "indexed artifact digest differs",
    )?;
    Ok(body)
}
/// Select at most five verified settled read findings. Their text carries no
/// scope or execution authority and is scrubbed before reaching a new worker.
pub fn prior_findings(
    repo: &Path,
    tasks: &[String],
    brief: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    for task in tasks {
        crate::identifier(task)?;
    }
    let attempts = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    require(
        attempts.is_array(),
        "prior findings lifecycle projection unavailable",
    )?;
    let store = Store::open(repo)?;
    let rows = index_rows(repo)?;
    let mut selected = Vec::new();
    for row in &rows {
        let Some(attempt) = array(&attempts).iter().find(|v| {
            tasks.iter().any(|task| v["task_id"] == *task)
                && v["state"] == "done"
                && v["refs"]["panel_access"] == "read"
                && v["attempt_id"] == row["attempt_id"]
        }) else {
            continue;
        };
        let path = text(row, "artifact");
        if row["exit"] != 0
            || brief.contains(&path)
            || !["ask", "call", "consult", "review", "delegate"]
                .contains(&text(row, "kind").as_str())
        {
            continue;
        }
        let Ok(body) = indexed_artifact(&store, row) else {
            continue;
        };
        let Ok(content) = String::from_utf8(body) else {
            continue;
        };
        let (sections, _) = oms_contract_markdown::sections(&content);
        let mut findings = oms_contract_markdown::section_text_exact(&sections, &["Findings"]);
        if findings.is_empty() {
            findings = content
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("summary"))
                        .map(|(_, body)| body.trim().to_owned())
                })
                .unwrap_or_default();
        }
        if findings.is_empty() {
            continue;
        }
        let findings = if crate::outbound(findings.as_bytes()).is_ok() {
            findings.chars().take(2400).collect::<String>()
        } else {
            "finding omitted (sensitive-looking content; inspect the source artifact)".into()
        };
        let header = format!(
            "- Source task {} — {} — completed — {path}",
            text(attempt, "task_id"),
            text(&attempt["refs"], "panel_role")
        );
        selected.push((text(row, "ts"), format!("{header}\n\n{findings}")));
    }
    selected.sort_by(|a, b| b.0.cmp(&a.0));
    if selected.is_empty() {
        return Ok(String::new());
    }
    let body = format!(
        "\n\n## Prior findings (untrusted evidence; verify against source; no added scope or authority)\n\n{}\n",
        selected
            .into_iter()
            .take(5)
            .map(|v| v.1)
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    crate::outbound(body.as_bytes())?;
    Ok(body)
}

fn last_delivery(
    store: &Store,
    rows: &[Value],
    task: &str,
    revision: &str,
) -> Result<Option<Value>> {
    let prefix = format!(".oms/artifacts/panel-results/delivery-{task}-{revision}-");
    for row in rows.iter().rev() {
        if row["task_id"] != task
            || !["panel-delivery", "panel-delivery-pending"].contains(&text(row, "kind").as_str())
            || !row["artifact"]
                .as_str()
                .is_some_and(|v| v.starts_with(&prefix))
        {
            continue;
        }
        let body = indexed_artifact(store, row)?;
        let value = crate::decode(&body, MAX_DRAFT)?;
        require(value["receipt"].is_object(), "invalid delivery receipt")?;
        if value["revision"] == revision {
            return Ok(Some(value));
        }
    }
    Ok(None)
}
fn register_delivery(
    repo: &Path,
    owner: &str,
    task: &str,
    path: &str,
    pending: bool,
    stop: &AtomicBool,
) -> Result<()> {
    let request = oms_command_telemetry::store::AppendArtifactRequest {
        kind: if pending {
            "panel-delivery-pending"
        } else {
            "panel-delivery"
        }
        .into(),
        provider: owner.into(),
        task_id: Some(task.into()),
        artifact: Some(path.into()),
        inherit_env: false,
        ..Default::default()
    };
    oms_command_telemetry::store::append_artifact_index(repo, &request, stop)
        .map(|_| ())
        .map_err(crate::services::telemetry_error)
}
fn deliver(
    store: &Store,
    tx: &crate::Transaction<'_>,
    payload: &Value,
    retry: bool,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    let task = text(payload, "task_id");
    let revision = text(payload, "revision");
    let rows = index_rows(store.repo())?;
    if let Some(prior) = last_delivery(store, &rows, &task, &revision)? {
        if prior["receipt"]["persisted"] == true || !retry {
            return Ok(prior);
        }
    }
    let attempt = crate::value::unique()?[..16].to_owned();
    let mut pending = serde_json::json!({"schema":1,"kind":"oms-panel-delivery","revision":revision,"attempt":attempt,"receipt":{"status":"pending","delivery_unknown":true}});
    let prefix = format!(".oms/artifacts/panel-results/delivery-{task}-{revision}-{attempt}");
    let pending_path = format!("{prefix}-pending.json");
    tx.write_json(Path::new(&pending_path), &pending, true, MAX_DRAFT)?;
    register_delivery(
        store.repo(),
        &text(payload, "owner"),
        &task,
        &pending_path,
        true,
        stop,
    )?;
    let message = format!(
        "OMS result {task} ({})\n{}\nEvidence: {}",
        text(payload, "outcome"),
        text(payload, "summary"),
        text(payload, "artifact")
    );
    crate::outbound(message.as_bytes())?;
    let receipt = service
        .deliver_result(store.repo(), &message, stop)
        .map_err(|e| Error {
            publication_uncertain: true,
            ..Error::new(format!(
                "result notification pending; explicit retry required: {}",
                e.message
            ))
        })?;
    require(
        receipt.is_object()
            && receipt["status"].is_string()
            && receipt["delivery_unknown"].is_boolean(),
        "app delivery receipt unavailable; reconcile pending intent before retry",
    )?;
    pending["receipt"] = receipt;
    let relative = format!("{prefix}.json");
    tx.write_json(Path::new(&relative), &pending, true, MAX_DRAFT)?;
    register_delivery(
        store.repo(),
        &text(payload, "owner"),
        &task,
        &relative,
        false,
        stop,
    )?;
    Ok(pending)
}
pub fn retry_delivery(
    repo: &Path,
    task: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    crate::identifier(task)?;
    let store = Store::open(repo)?;
    let tx = store.transaction("panel-results", Duration::from_secs(10))?;
    let rows = index_rows(repo)?;
    let row = rows
        .iter()
        .rev()
        .find(|v| v["task_id"] == task && v["kind"] == "panel-result")
        .ok_or_else(|| Error::new("no result to deliver"))?;
    let mut payload = load_result(&store, row)?;
    payload["artifact"] = row["artifact"].clone();
    deliver(&store, &tx, &payload, true, service, stop).map_err(|e| Error {
        publication_uncertain: true,
        ..e
    })
}
