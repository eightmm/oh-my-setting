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
    let parents = crate::control::lineage(
        repo,
        array(&attempts),
        &request.main_attempt_id,
        service,
        stop,
        false,
    )?;
    let related: Vec<_> = array(&attempts)
        .iter()
        .filter(|v| {
            v["tool"] != "panel-main"
                && (v["task_id"] == request.task_id
                    || (v["task_id"].as_str().is_none_or(str::is_empty)
                        && parents.contains(&text(v, "parent_attempt_id"))))
        })
        .cloned()
        .collect();
    require(!related.is_empty(), "task has no recorded attempts")?;
    require(
        related
            .iter()
            .all(|v| descends(v, array(&attempts), &parents)),
        "task includes attempts owned by another main",
    )?;
    require(
        related.iter().all(|v| {
            v["terminal"] == true
                || (v["state"] == "review"
                    && crate::control::stopped(repo, v, array(&attempts)).is_ok())
        }),
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
        if task["claimed_by_participant"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
        {
            let participant = text(&main["refs"], "panel_room_participant");
            require(
                task["claimed_by_participant"] == participant,
                "plan claimant differs from current main",
            )?;
            let room = query(
                service,
                NativeCommand::Room,
                repo,
                &[
                    "show",
                    "--id",
                    &text(&main["refs"], "panel_room_id"),
                    "--json",
                ],
                stop,
            )?;
            require(
                room["closed"] == false
                    && array(&room["participants"]).iter().any(|v| {
                        v["participant"] == participant
                            && v["joined"] == true
                            && v["role"] == "main"
                    }),
                "current main is not joined to the task room",
            )?;
        }
    }
    if request.outcome == "accepted" && request.landed_commit.is_none() {
        require(
            task["state"] == "done",
            "accepted requires native plan completion/publication proof",
        )?;
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
        if row["task_id"] == request.task_id
            || related.iter().any(|v| v["attempt_id"] == row["attempt_id"])
        {
            result_rows.push(row);
        }
    }
    let selection = crate::control::select(
        repo,
        related
            .iter()
            .filter(|v| v["refs"]["panel_role"] == "worker" && v["refs"]["panel_access"] == "write")
            .cloned()
            .collect(),
        array(&attempts),
        &result_rows,
        Some(&request.main_attempt_id),
        service,
        stop,
    )?;
    if request.outcome == "accepted" || request.landed_commit.is_some() {
        for worker in &selection.workers {
            require(
                worker["refs"]["panel_purpose"] != "investigate",
                "report-only findings cannot be accepted",
            )?;
            let admission = crate::control::admission(&result_rows, worker)
                .ok_or_else(|| Error::new("write worker lacks matching landed patch evidence"))?;
            let patch = result_rows
                .iter()
                .rev()
                .find(|v| v["kind"] == "delegate" && v["attempt_id"] == worker["attempt_id"])
                .ok_or_else(|| Error::new("worker patch unavailable"))?;
            let bytes = verified_patch(&store, patch)?;
            require(
                admission["patch_sha256"] == crate::digest(&bytes),
                "admission bytes changed",
            )?;
            crate::control::review_gate(&result_rows, &request.task_id, &patch["patch_sha256"])?;
            let scopes = crate::control::scopes(repo, worker, service, stop)?;
            for path in crate::files::patch_paths(&bytes)? {
                require(
                    scopes.iter().any(|v| oms_command_room::overlaps(v, &path)),
                    "accepted patch leaves declared scope",
                )?;
            }
        }
    }
    let binding = crate::control::plan_binding(&plan, &request.task_id)?;
    Ok(
        serde_json::json!({"main":main,"binding":binding,"task":task,"related_attempts":related,
        "related_rows":result_rows,"evidence":evidence,"superseded_attempts":selection.superseded,
        "admission_event_ids":selection.workers.iter().filter_map(|v| crate::control::admission(&result_rows,v))
            .map(|v| &v["event_id"]).collect::<Vec<_>>()}),
    )
}
fn descends(row: &Value, attempts: &[Value], parents: &std::collections::BTreeSet<String>) -> bool {
    let mut cursor = text(row, "attempt_id");
    let mut seen = std::collections::BTreeSet::new();
    while !parents.contains(&cursor) {
        if cursor.is_empty() || !seen.insert(cursor.clone()) {
            return false;
        }
        let Some(row) = attempts.iter().find(|v| v["attempt_id"] == cursor) else {
            return false;
        };
        cursor = text(row, "parent_attempt_id");
    }
    true
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
                .is_some_and(|v| !v.trim().is_empty())),
        "accepted requires evidence and a passing verifier",
    )?;
    let store = Store::open(repo)?;
    let summary = safe_summary(&store, &request.summary_file)?;
    let execution = std::fs::canonicalize(&request.execution_repo).map_err(Error::new)?;
    crate::control::review_readers(repo, &execution)?;
    let mut resolved = request.clone();
    if requires_acceptance && resolved.evidence.is_empty() {
        let snapshot = service.finalize_authority(repo, request, stop)?;
        resolved.evidence = automatic_evidence(&store, &snapshot)?;
    }
    let request = &resolved;
    let before = {
        let _tx = store.transaction("panel-results", Duration::from_secs(10))?;
        service.finalize_authority(repo, request, stop)?
    };
    let landed = if request.landed_commit.is_some() {
        Some(crate::acceptance::validate_landed(
            repo, request, service, stop,
        )?)
    } else {
        None
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
    let mut after = service.finalize_authority(repo, request, stop)?;
    require(
        after == before,
        "result authority/evidence changed during verification",
    )?;
    if let Some(landed) = landed {
        require(
            crate::acceptance::validate_landed(repo, request, service, stop)? == landed,
            "landed acceptance changed during verification",
        )?;
        crate::acceptance::finish_landed(repo, request, &before, service, stop)?;
        after = service.finalize_authority(repo, request, stop)?;
    }
    let mut recent = array(&after["related_attempts"]).iter().collect::<Vec<_>>();
    recent.sort_by_key(|v| (text(v, "created_at"), text(v, "attempt_id")));
    let recent = &recent[recent.len().saturating_sub(20)..];
    let mut payload = serde_json::json!({"schema":1,"kind":"oms-panel-result","task_id":request.task_id,"owner":request.owner,
        "outcome":request.outcome,"summary":summary,"summary_sha256":crate::digest(summary.as_bytes()),"verification":verification,
        "evidence":after["evidence"],"main_attempt_id":request.main_attempt_id,"plan_binding":after["binding"],
        "superseded_attempts":after["superseded_attempts"],"admission_event_ids":after["admission_event_ids"],
        "attempt_ids":recent.iter().filter_map(|v| v["attempt_id"].as_str()).collect::<Vec<_>>()});
    let raw = oms_json::parse_strict(&crate::value::encoded(&payload)?).map_err(Error::new)?;
    let revision = oms_json::projection_cache_digest(&raw).map_err(Error::new)?[..24].to_owned();
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
    payload["settlement"] = settle(repo, &payload, &after, service, stop);
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
    let plan = store
        .json(Path::new(".oms/plan/tasks.json"), 4 * MAX_RESULT, true)?
        .unwrap_or(Value::Null);
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
    newest
        .into_iter()
        .filter_map(
            |(task, value)| match crate::control::plan_binding(&plan, &task) {
                Ok(binding)
                    if binding.is_null()
                        || value["plan_binding"].is_null()
                        || value["plan_binding"] == binding =>
                {
                    Some(Ok(value))
                }
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            },
        )
        .collect()
}
fn settle(
    repo: &Path,
    payload: &Value,
    snapshot: &Value,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Value {
    let mut report =
        serde_json::json!({"candidates":[],"pending":[],"applied":[],"errors":[],"executes":true});
    for row in array(&snapshot["related_attempts"]).iter().filter(|v| {
        v["state"] == "review"
            && v["refs"]["panel_role"] == "worker"
            && v["refs"]["panel_access"] == "write"
    }) {
        let ident = text(row, "attempt_id");
        let superseded = array(&payload["superseded_attempts"])
            .iter()
            .any(|v| v["attempt_id"] == ident);
        let outcome = text(payload, "outcome");
        let (target, reason) = if superseded {
            ("cancelled", "owner_superseded")
        } else {
            match outcome.as_str() {
                "accepted" => ("done", "owner_accepted"),
                "completed" => ("done", "owner_completed"),
                "rejected" => ("cancelled", "owner_rejected"),
                _ => {
                    report["pending"].as_array_mut().unwrap().push(ident.into());
                    continue;
                }
            }
        };
        report["candidates"]
            .as_array_mut()
            .unwrap()
            .push(ident.clone().into());
        let apply = (|| -> Result<()> {
            let sequence = row["sequence"]
                .as_u64()
                .ok_or_else(|| Error::new("worker sequence unavailable"))?;
            let key = format!(
                "review-settle-{}",
                crate::digest(
                    format!("{}:{}", text(payload, "task_id"), text(payload, "revision"))
                        .as_bytes()
                )
            );
            service
                .command(
                    NativeCommand::Events,
                    repo,
                    &crate::services::args(&[
                        "transition",
                        "--attempt",
                        &ident,
                        "--state",
                        target,
                        "--expected-state",
                        "review",
                        "--expected-seq",
                        &sequence.to_string(),
                        "--actor",
                        "panel-review-settlement",
                        "--reason-code",
                        reason,
                        "--idempotency-key",
                        &key,
                    ]),
                    stop,
                )?
                .success()
        })();
        match apply {
            Ok(()) => report["applied"].as_array_mut().unwrap().push(ident.into()),
            Err(e) => report["errors"].as_array_mut().unwrap().push(serde_json::json!({"attempt_id":ident,"error":crate::eventlog::clean(&e.message,240)})),
        }
    }
    report
}

fn automatic_evidence(store: &Store, snapshot: &Value) -> Result<Vec<PathBuf>> {
    let ids = array(&snapshot["admission_event_ids"]);
    require(
        !ids.is_empty(),
        "accepted requires evidence when admissions are missing or ambiguous",
    )?;
    let rows = array(&snapshot["related_rows"]);
    let mut evidence = std::collections::BTreeSet::new();
    for id in ids {
        let admission = rows
            .iter()
            .find(|v| v["event_id"] == *id)
            .ok_or_else(|| Error::new("admission evidence unavailable"))?;
        require(
            admission["patch_sha256"]
                .as_str()
                .is_some_and(|v| crate::hex(v, 64)),
            "invalid admission patch identity",
        )?;
        let candidates = rows
            .iter()
            .filter(|v| {
                v["kind"] == "patch-admit"
                    && v["exit"] == 0
                    && v["patch_sha256"] == admission["patch_sha256"]
                    && (v["task_id"].is_null()
                        || v["task_id"] == ""
                        || v["task_id"] == admission["task_id"])
                    && (admission["plan_id"].is_null() || v["plan_id"] == admission["plan_id"])
            })
            .filter(|v| indexed_artifact(store, v).is_ok())
            .map(|v| PathBuf::from(text(v, "artifact")))
            .collect::<std::collections::BTreeSet<_>>();
        require(
            candidates.len() == 1,
            "accepted requires evidence when admission artifacts are missing or ambiguous",
        )?;
        evidence.extend(candidates);
    }
    require(
        evidence.len() <= 8,
        "use at most eight evidence references; provide evidence explicitly",
    )?;
    Ok(evidence.into_iter().collect())
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
        let result_row = indexed
            .iter()
            .rev()
            .find(|v| v["task_id"] == task && v["kind"] == "panel-result");
        let mut issue = Value::Null;
        let result = match result_row.map(|v| load_result(&store, v)).transpose() {
            Ok(v) => v.unwrap_or(Value::Null),
            Err(_) => {
                issue = "result or delivery evidence unavailable".into();
                Value::Null
            }
        };
        let mut metadata_attempts = array(&attempts)
            .iter()
            .filter(|v| v["task_id"] == task)
            .collect::<Vec<_>>();
        let mut matching = metadata_attempts.clone();
        if matching.is_empty() && result["main_attempt_id"].is_string() {
            matching = array(&attempts)
                .iter()
                .filter(|v| {
                    v["parent_attempt_id"] == result["main_attempt_id"]
                        && v["task_id"].as_str().is_none_or(str::is_empty)
                })
                .collect();
        }
        let adopted = matching
            .iter()
            .map(|v| text(v, "attempt_id"))
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(member) = &request.room_participant {
            metadata_attempts.retain(|v| v["refs"]["panel_room_participant"] == *member);
            matching.retain(|v| v["refs"]["panel_room_participant"] == *member);
        }
        let linked = matching
            .iter()
            .map(|v| text(v, "attempt_id"))
            .collect::<std::collections::BTreeSet<_>>();
        let rows = indexed
            .iter()
            .filter(|v| {
                (v["task_id"] == task || adopted.contains(&text(v, "attempt_id")))
                    && (request.room_participant.is_none()
                        || linked.contains(&text(v, "attempt_id")))
            })
            .collect::<Vec<_>>();
        if request.room_participant.is_some()
            && request.task_id.is_none()
            && metadata_attempts.is_empty()
        {
            continue;
        }
        passed += 1;
        if request.task_id.is_none() && passed <= request.offset {
            continue;
        }
        if request.task_id.is_none() && output.len() >= request.limit {
            capped = true;
            break;
        }
        let last = metadata_attempts
            .last()
            .copied()
            .cloned()
            .unwrap_or(Value::Null);
        let metadata = &last["refs"];
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
                "artifact_sha256",
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
            if (request.task_id.is_some() || request.room_participant.is_some())
                && ["ask", "call", "consult", "delegate", "review"]
                    .contains(&text(row, "kind").as_str())
                && row["artifact"].is_string()
                && !["empty", "no_answer"].contains(&text(row, "answer_quality").as_str())
            {
                match indexed_artifact(&store, row).and_then(|v| answer(&v)) {
                    Ok(answer) => {
                        call.insert("answer".into(), answer.into());
                        call.insert("answer_truncated".into(), false.into());
                    }
                    Err(_) => {
                        call.insert("answer".into(), "Answer evidence unavailable".into());
                    }
                }
            }
            calls.push(Value::Object(call));
        }
        let worker_rows = matching.iter().map(|v| (*v).clone()).collect::<Vec<_>>();
        let mut superseded = array(&result["superseded_attempts"])
            .iter()
            .map(|v| text(v, "attempt_id"))
            .collect::<std::collections::BTreeSet<_>>();
        for (start, worker) in worker_rows.iter().enumerate() {
            if (start + 1..worker_rows.len()).any(|end| {
                crate::control::continues(&worker_rows, start, end)
                    && calls
                        .iter()
                        .any(|v| v["attempt_id"] == worker_rows[end]["attempt_id"])
            }) {
                superseded.insert(text(worker, "attempt_id"));
            }
        }
        for call in &mut calls {
            if superseded.contains(&text(call, "attempt_id")) {
                call["superseded"] = true.into();
            }
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
                .filter(|v| {
                    kinds.contains(&text(v, "kind").as_str())
                        && !v[exit_key].is_null()
                        && v["superseded"] != true
                        && (v[exit_key] == 0
                            || !retired(repo, v, exit_key, &worker_rows, &calls, service, stop))
                })
                .collect::<Vec<_>>();
            let failed = values.iter().any(|v| v[exit_key] != 0);
            let last = values.last();
            stages[stage] = serde_json::json!({"state":if failed {"failed"} else if values.is_empty() {"not_run"} else {"passed"},"attempt_id":last.map(|v| &v["attempt_id"]),"exit":last.map(|v| &v[exit_key])});
        }
        let review_rows = rows
            .iter()
            .filter(|v| {
                ["review", "review-outcome"].contains(&text(v, "kind").as_str())
                    && !superseded.contains(&text(v, "attempt_id"))
            })
            .copied()
            .collect::<Vec<_>>();
        let latest_run = review_rows
            .iter()
            .enumerate()
            .max_by_key(|(i, v)| (review_run_order(&review_run(v)), *i))
            .map(|(_, v)| review_run(v));
        let review = review_rows.iter().rev().find(|v| {
            v["kind"] == "review-outcome"
                && latest_run.as_ref().is_some_and(|run| review_run(v) == *run)
        });
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
                if !review_rows.is_empty() {
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
        if let Some(stage) = failed {
            let key = if stage == "verify" {
                "verify_exit"
            } else {
                "exit"
            };
            if let Some(call) = calls
                .iter()
                .find(|v| v["superseded"] != true && v[key].as_i64().is_some_and(|n| n != 0))
                && let Some(ending) = call_ending(call)
            {
                let (state, next) = match ending {
                    "cancelled" => (format!("{stage} cancelled"), "none".into()),
                    "guard_stop" => (
                        format!("{stage} guard stop"),
                        "inspect guard stop and salvage partial work".into(),
                    ),
                    "timed_out" => (
                        "timed out".into(),
                        if metadata["panel_access"] == "read" {
                            "continue with --continue TASK_ID".into()
                        } else {
                            "salvage (--salvage TASK_ID); --continue if empty, else inspect and re-dispatch with recovered patch".into()
                        },
                    ),
                    "verify_failed" => ("verify failed".into(), "retry verify".into()),
                    "verify_timed_out" => ("verify timed out".into(), "retry verify".into()),
                    "provider_error" => {
                        (format!("{stage} provider error"), format!("retry {stage}"))
                    }
                    _ => (representative.clone(), action.clone()),
                };
                representative = state;
                action = next;
            }
        }
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
        let admitted = result.is_null() && awaiting_land(&task, &worker_rows, &indexed);
        let retained = calls.len().saturating_sub(8);
        let title = metadata["panel_label"]
            .as_str()
            .or_else(|| result["summary"].as_str().and_then(|v| v.lines().next()))
            .unwrap_or_default();
        let mut projected = serde_json::json!({"task_id":task,"title":crate::eventlog::clean(title,160),"model":metadata["panel_model"],"role":metadata["panel_role"],"location":metadata["panel_location"],"process_state":last["state"],"outcome":result["outcome"],"admitted":admitted,"verification":result["verification"],"acceptance":outcome == "accepted","summary":result["summary"],"revision":result["revision"],"artifact":result_row.map(|v| &v["artifact"]),"evidence":result.get("evidence").cloned().unwrap_or_else(|| serde_json::json!([])),"calls":&calls[retained..],"delivery":delivery.as_ref().and_then(|v| v["receipt"]["status"].as_str()).unwrap_or("not_requested"),"delivery_unknown":delivery.as_ref().is_some_and(|v| v["receipt"]["delivery_unknown"] == true),"issue":issue,"missing_result":result.is_null(),"stages":stages,"representative_state":representative,"next_action":action,"termination_reason":termination_reason});
        if request.task_id.is_some()
            && let Some(delta) = round_delta(&store, &worker_rows, &rows)
        {
            projected["round_delta"] = delta;
        }
        output.push(projected);
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
/// Explicitly reconcile a saved acceptance. This never reapplies the patch.
pub fn recover_accept(
    repo: &Path,
    request: &AcceptRequest,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    validate_accept(request)?;
    crate::acceptance::recover(repo, request, service, stop)
}

fn retired(
    repo: &Path,
    call: &Value,
    key: &str,
    workers: &[Value],
    calls: &[Value],
    service: &dyn Services,
    stop: &AtomicBool,
) -> bool {
    let Some(index) = workers.iter().position(|v| {
        v["attempt_id"] == call["attempt_id"]
            && v["refs"]["panel_role"] == "worker"
            && v["refs"]["panel_access"] == "write"
    }) else {
        return false;
    };
    let old = &workers[index];
    let Ok(scope) = crate::control::scopes(repo, old, service, stop) else {
        return false;
    };
    !scope.is_empty()
        && workers[index + 1..].iter().any(|later| {
            later["refs"]["panel_purpose"] == old["refs"]["panel_purpose"]
                && calls.iter().any(|v| {
                    v["kind"] == "delegate" && v["attempt_id"] == later["attempt_id"] && v[key] == 0
                })
                && crate::control::scopes(repo, later, service, stop)
                    .is_ok_and(|new| scope.iter().all(|v| new.contains(v)))
        })
}
fn review_run(row: &Value) -> String {
    if let Some(run) = row["review"]["run"].as_str().filter(|v| !v.is_empty()) {
        return run.into();
    }
    if let Ok(pattern) = regex::Regex::new(r"-(\d{8}T\d{6}Z-\d+)(?:-r\d+)?(?:\.md)?$")
        && let Some(run) = pattern.captures(&text(row, "artifact"))
    {
        return run[1].into();
    }
    [
        "run_id",
        "operation_id",
        "attempt_id",
        "event_id",
        "artifact",
    ]
    .iter()
    .find_map(|key| {
        row[*key]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    })
    .unwrap_or_default()
}
fn review_run_order(run: &str) -> (String, usize, String) {
    let Ok(pattern) = regex::Regex::new(r"^(\d{8}T\d{6}Z)-(\d+)") else {
        unreachable!()
    };
    let Some(captures) = pattern.captures(run) else {
        return (String::new(), 0, String::new());
    };
    let number = captures[2].trim_start_matches('0');
    (captures[1].into(), number.len(), number.into())
}

fn call_ending(row: &Value) -> Option<&'static str> {
    if crate::state::LIVE_STATES.contains(&text(row, "state").as_str()) {
        return None;
    }
    let code = row["exit"].as_i64();
    let reason = text(row, "reason_code");
    if row
        .get("guard")
        .is_some_and(|v| !v.is_null() && v != false && v != "")
        || reason == "worker_authority_violation"
    {
        return Some("guard_stop");
    }
    if row["state"] == "cancelled"
        || code.is_some_and(|v| [129, 130, 143].contains(&v))
        || ["cancelled_by_owner", "manual_clear"].contains(&reason.as_str())
    {
        return Some("cancelled");
    }
    if row["state"] == "timed_out" || (code == Some(124) && row["provider_error_exit"].is_null()) {
        return Some("timed_out");
    }
    if row["verify_exit"] == 124 && (reason == "verification_failed" || code == Some(0)) {
        return Some("verify_timed_out");
    }
    if reason == "verification_failed"
        || (code == Some(0)
            && (row["verify_exit"].as_i64().is_some_and(|v| v != 0)
                || row["verify_status"] == "failed"))
    {
        return Some("verify_failed");
    }
    if code.is_some_and(|v| v != 0) {
        return Some(if row["provider_error_exit"].is_null() {
            "failed"
        } else {
            "provider_error"
        });
    }
    if row["state"] == "failed" {
        Some("failed")
    } else {
        None
    }
}
fn answer(bytes: &[u8]) -> Result<String> {
    let raw = std::str::from_utf8(bytes).map_err(Error::new)?;
    let mut body = String::new();
    let mut reading = false;
    for line in raw.lines() {
        if line == "## Output" {
            reading = true;
            body.clear();
            continue;
        }
        if line == "## Exit" {
            reading = false;
        }
        if reading {
            body.push_str(line);
            body.push('\n');
        }
    }
    let mut selected = String::new();
    let mut in_answer = false;
    for line in body.lines() {
        if line.starts_with("model-result:")
            || [
                "tokens used",
                "usage detail:",
                "served model",
                "cost usd",
                "## Verify",
            ]
            .contains(&line)
        {
            break;
        }
        if line.starts_with("model-route:") {
            continue;
        }
        if ["## Answer", "## Findings"].contains(&line) && !in_answer {
            selected.clear();
            in_answer = true;
        }
        selected.push_str(line);
        selected.push('\n');
    }
    Ok(crate::value::redact(selected.as_bytes())?.trim().into())
}
fn awaiting_land(task: &str, attempts: &[Value], rows: &[Value]) -> bool {
    let patches = rows
        .iter()
        .filter(|v| {
            v["task_id"] == task
                && ["delegate", "ask", "call", "consult", "review"]
                    .contains(&text(v, "kind").as_str())
                && v["patch"].is_string()
                && v["patch_sha256"]
                    .as_str()
                    .is_some_and(|v| crate::hex(v, 64) && v != crate::digest(b""))
        })
        .collect::<Vec<_>>();
    let landed = |patch: &Value| {
        rows.iter().any(|v| {
            v["task_id"] == task
                && ["patch-land", "patch-admit"].contains(&text(v, "kind").as_str())
                && v["patch_sha256"] == patch["patch_sha256"]
        })
    };
    let accepted = |patch: &Value| {
        rows.iter().rev().find(|v| {
            v["task_id"] == task
                && v["kind"] == "panel-accept"
                && v["attempt_id"] == patch["attempt_id"]
                && v["patch_sha256"] == patch["patch_sha256"]
        })
    };
    let pending = patches.iter().any(|patch| {
        !landed(patch)
            && accepted(patch).is_none()
            && !attempts
                .iter()
                .position(|v| v["attempt_id"] == patch["attempt_id"])
                .is_some_and(|start| {
                    (start + 1..attempts.len()).any(|end| {
                        crate::control::continues(attempts, start, end)
                            && patches.iter().any(|later| {
                                later["attempt_id"] == attempts[end]["attempt_id"]
                                    && (landed(later) || accepted(later).is_some())
                            })
                    })
                })
    });
    if pending {
        return false;
    }
    let latest = patches
        .iter()
        .filter(|v| !landed(v))
        .filter_map(|v| accepted(v))
        .map(|v| text(v, "ts"))
        .max();
    latest.is_some_and(|stamp| {
        !attempts.iter().any(|v| {
            v["refs"]["panel_access"] == "write"
                && text(v, "created_at") >= stamp
                && !rows.iter().any(|r| {
                    r["kind"] == "panel-accept"
                        && r["task_id"] == task
                        && r["attempt_id"] == v["attempt_id"]
                })
        })
    })
}
type Balance = std::collections::BTreeMap<String, std::collections::BTreeMap<Vec<u8>, i64>>;
fn patch_balance(bytes: &[u8]) -> Result<Balance> {
    require(
        bytes.is_empty() || bytes.ends_with(b"\n"),
        "truncated patch text",
    )?;
    let mut files = Balance::new();
    let mut current = String::new();
    let (mut old, mut new) = (0u64, 0u64);
    let mut last: Option<(Vec<u8>, i64)> = None;
    let header =
        regex::Regex::new(r"^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@").map_err(Error::new)?;
    for line in bytes.split_inclusive(|v| *v == b'\n') {
        if line.starts_with(b"\\ No newline at end of file") {
            let (value, sign) = last
                .take()
                .ok_or_else(|| Error::new("invalid newline marker"))?;
            if sign != 0 {
                let counts = files
                    .get_mut(&current)
                    .ok_or_else(|| Error::new("missing patch path"))?;
                *counts.entry(value.clone()).or_default() -= sign;
                *counts.entry(value[..value.len() - 1].to_vec()).or_default() += sign;
            }
        } else if old > 0 || new > 0 {
            let (before, after, sign) = match line.first() {
                Some(b' ') => (1, 1, 0),
                Some(b'-') => (1, 0, -1),
                Some(b'+') => (0, 1, 1),
                _ => return Err(Error::new("invalid patch hunk")),
            };
            old = old
                .checked_sub(before)
                .ok_or_else(|| Error::new("invalid old hunk length"))?;
            new = new
                .checked_sub(after)
                .ok_or_else(|| Error::new("invalid new hunk length"))?;
            let value = line[1..].to_vec();
            *files
                .get_mut(&current)
                .ok_or_else(|| Error::new("missing patch path"))?
                .entry(value.clone())
                .or_default() += sign;
            last = Some((value, sign));
        } else if line.starts_with(b"diff --git ") {
            let paths = crate::files::patch_paths(line)?;
            require(
                paths.len() == 1 && !files.contains_key(&paths[0]),
                "unsupported patch paths",
            )?;
            current = paths[0].clone();
            files.insert(current.clone(), Default::default());
            last = None;
        } else if line.starts_with(b"@@ ") {
            let raw = std::str::from_utf8(line).map_err(Error::new)?;
            let captures = header
                .captures(raw)
                .ok_or_else(|| Error::new("invalid patch hunk header"))?;
            let count = |index| {
                captures
                    .get(index)
                    .map(|v| v.as_str().parse::<u64>().map_err(Error::new))
                    .unwrap_or(Ok(1))
            };
            old = count(1)?;
            new = count(2)?;
            last = None;
        } else {
            require(
                [
                    b"index ".as_slice(),
                    b"--- ",
                    b"+++ ",
                    b"new file mode ",
                    b"deleted file mode ",
                ]
                .iter()
                .any(|v| line.starts_with(v)),
                "unsupported patch format",
            )?;
        }
    }
    require(old == 0 && new == 0, "truncated patch hunk")?;
    Ok(files)
}
fn round_delta(store: &Store, attempts: &[Value], rows: &[&Value]) -> Option<Value> {
    let writes = attempts
        .iter()
        .filter(|v| v["refs"]["panel_access"] == "write")
        .enumerate()
        .filter_map(|(i, v)| {
            rows.iter()
                .rev()
                .find(|row| {
                    row["kind"] == "delegate"
                        && row["attempt_id"] == v["attempt_id"]
                        && row["patch"].is_string()
                        && row["patch_sha256"].is_string()
                })
                .map(|row| (i + 1, *row))
        })
        .collect::<Vec<_>>();
    if writes.len() < 2 {
        return None;
    }
    let [(number, before), (_, after)] = writes[writes.len() - 2..] else {
        unreachable!()
    };
    Some((|| -> Result<Value> {
        let before = verified_patch(store,before)?; let after = verified_patch(store,after)?;
        if before.len() + after.len() > 2*1024*1024 { return Ok(serde_json::json!({"issue":"skipped (too large)"})); }
        let before = patch_balance(&before)?; let after = patch_balance(&after)?;
        let paths = before.keys().chain(after.keys()).collect::<std::collections::BTreeSet<_>>();
        let mut files = Vec::new();
        for path in paths {
            match (before.get(path),after.get(path)) {
                (None,Some(_)) => files.push(serde_json::json!({"path":path,"status":"new in this round"})),
                (Some(_),None) => files.push(serde_json::json!({"path":path,"status":"dropped this round"})),
                (Some(old),Some(new)) => {
                    let lines = old.keys().chain(new.keys()).collect::<std::collections::BTreeSet<_>>();
                    let deltas = lines.into_iter().map(|v| new.get(v).copied().unwrap_or(0)-old.get(v).copied().unwrap_or(0)).collect::<Vec<_>>();
                    let added: i64 = deltas.iter().filter(|v| **v > 0).sum();
                    let removed: i64 = -deltas.iter().filter(|v| **v < 0).sum::<i64>();
                    if added > 0 || removed > 0 { files.push(serde_json::json!({"path":path,"added":added,"removed":removed})); }
                }, _ => {}
            }
        }
        Ok(serde_json::json!({"since_round":number,"count":files.len(),"files":&files[..files.len().min(20)]}))
    })().unwrap_or_else(|_| serde_json::json!({"issue":"unavailable"})))
}

pub fn indexed_artifact(store: &Store, row: &Value) -> Result<Vec<u8>> {
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
    if let Some(prior) = last_delivery(store, &rows, &task, &revision)?
        && (prior["receipt"]["persisted"] == true || !retry)
    {
        return Ok(prior);
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
