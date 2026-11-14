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
    let relative = if path.is_absolute() {
        path.strip_prefix(store.repo())
            .map_err(|_| Error::new("summary must be inside selected repository"))?
    } else {
        path
    };
    let body = store.read(relative, MAX_DRAFT, false)?;
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
        let passed = result.code == 0 && result.cleanup_complete;
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
    service
        .register_result(repo, &registration, stop)
        .map_err(|e| Error {
            publication_uncertain: true,
            ..Error::new(format!(
                "result {relative} published; registration pending: {}",
                e.message
            ))
        })?;
    payload["artifact"] = relative.into();
    if request.notify {
        payload["delivery"] = serde_json::json!({"status":"pending","retry_required":true});
    }
    Ok(payload)
}
pub fn accept(
    repo: &Path,
    request: &AcceptRequest,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    crate::identifier(&request.task_id)?;
    crate::identifier(&request.main_attempt_id)?;
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
    service.accept(repo, request, stop)
}
pub fn outcomes(repo: &Path, tasks: &[String]) -> Result<Vec<Value>> {
    let store = Store::open(repo)?;
    let raw = store.read(
        Path::new(".oms/artifacts/index.jsonl"),
        16 * 1024 * 1024,
        true,
    )?;
    let mut newest = std::collections::BTreeMap::new();
    for line in raw.split(|&v| v == b'\n').filter(|v| !v.is_empty()) {
        let row = crate::decode(line, MAX_RESULT)?;
        let task = text(&row, "task_id");
        if row["kind"] != "panel-result" || !tasks.contains(&task) {
            continue;
        }
        let path = row["artifact"]
            .as_str()
            .ok_or_else(|| Error::new("result artifact path missing"))?;
        require(
            path.starts_with(".oms/artifacts/panel-results/"),
            "result artifact path outside result store",
        )?;
        let value = store
            .json(Path::new(path), MAX_RESULT, false)?
            .ok_or_else(|| Error::new("indexed result missing"))?;
        require(
            value["schema"] == 1 && value["kind"] == "oms-panel-result" && value["task_id"] == task,
            "invalid result identity",
        )?;
        newest.insert(task, value);
    }
    Ok(newest.into_values().collect())
}
pub fn results(
    repo: &Path,
    task: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    let attempts = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let calls: Vec<_> = array(&attempts)
        .iter()
        .filter(|v| {
            task.is_none_or(|t| v["task_id"] == t) && !text(&v["refs"], "panel_role").is_empty()
        })
        .cloned()
        .collect();
    let tasks = calls
        .iter()
        .map(|v| text(v, "task_id"))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    Ok(
        serde_json::json!({"schema":1,"kind":"oms-panel-results","generated_at":crate::now(),"calls":calls,"outcomes":outcomes(repo,&tasks)?}),
    )
}
