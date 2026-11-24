//! Owner-scoped result publication with two-phase verifier rechecks.
use crate::services::{ExecutionRequest, NativeCommand, query};
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use std::borrow::Cow;
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
/// Use only for private launch evidence after proving the Main's lineage.
pub(crate) fn attempts_with_main_binding<'a>(
    attempts: &'a [Value],
    main: &Value,
) -> Cow<'a, [Value]> {
    if main["tool"] != "panel-main" || main["refs"].get("panel_room_participant").is_some() {
        return Cow::Borrowed(attempts);
    }
    let Some(index) = attempts.iter().position(|row| row == main) else {
        return Cow::Borrowed(attempts);
    };
    let mut projected = attempts.to_vec();
    projected[index]["refs"]["panel_room_participant"] =
        crate::state::main_participant(main).into();
    Cow::Owned(projected)
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
    let requires_acceptance = request.outcome == "accepted" || request.landed_commit.is_some();
    let attempts_slice = array(&attempts);
    let mut summary_ancestry =
        (!requires_acceptance).then(|| SummaryAncestry::new(attempts_slice, &parents));
    let mut related = Vec::new();
    let mut certified_inferred = Vec::new();
    for v in attempts_slice {
        if v["tool"] == "panel-main" {
            continue;
        }
        let certified = if v["task_id"] == request.task_id {
            Some(false)
        } else if v["task_id"].as_str().is_none_or(str::is_empty) {
            if requires_acceptance {
                parents
                    .contains(&text(v, "parent_attempt_id"))
                    .then_some(false)
            } else {
                (summary_ancestry
                    .as_mut()
                    .and_then(|ancestry| ancestry.task_for(v))
                    == Some(request.task_id.as_str()))
                .then_some(true)
            }
        } else {
            None
        };
        if let Some(certified) = certified {
            related.push(v.clone());
            certified_inferred.push(certified);
        }
    }
    require(!related.is_empty(), "task has no recorded attempts")?;
    require(
        related
            .iter()
            .zip(&certified_inferred)
            .all(|(v, &certified)| certified || descends(v, attempts_slice, &parents)),
        "task includes attempts owned by another main",
    )?;
    const MAX_FINALIZE_BLOCKERS: usize = 4;
    let mut launch_attempts = None;
    let mut blockers = Vec::new();
    let mut more = false;
    for attempt in &related {
        if attempt["terminal"] == true
            || (attempt["state"] == "review"
                && crate::control::stopped(
                    repo,
                    attempt,
                    launch_attempts
                        .get_or_insert_with(|| attempts_with_main_binding(array(&attempts), main)),
                )
                .is_ok())
        {
            continue;
        }
        if blockers.len() == MAX_FINALIZE_BLOCKERS {
            more = true;
            break;
        }
        let id = attempt["attempt_id"]
            .as_str()
            .filter(|id| {
                crate::native::operations::projection::id(id)
                    && crate::outbound(id.as_bytes()).is_ok()
            })
            .unwrap_or("unreported");
        let scope = if attempt["task_id"] == request.task_id {
            "task"
        } else {
            "inherited taskless descendant"
        };
        let reason = match attempt["state"].as_str() {
            Some("review") => Cow::Borrowed("missing verified whole-tree cleanup"),
            Some(state) if crate::state::LIVE_STATES.contains(&state) => {
                Cow::Owned(format!("lifecycle state {state}"))
            }
            _ => Cow::Borrowed("lifecycle state unknown"),
        };
        blockers.push(format!("{scope} attempt {id}, {reason}"));
    }
    if !blockers.is_empty() {
        if more {
            blockers.push("more remain".into());
        }
        return Err(Error::new(format!(
            "task has live or unconfirmed attempts ({})",
            blockers.join(" · ")
        )));
    }
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
            let participant = crate::state::main_participant(main);
            require(
                task["claimed_by_participant"] == participant,
                "plan claimant differs from current main",
            )?;
            // Lineage proves exact current Room membership for an implicit participant.
            if main["refs"].get("panel_room_participant").is_some() {
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
        launch_attempts.get_or_insert_with(|| attempts_with_main_binding(array(&attempts), main)),
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

#[derive(Clone, Copy)]
enum Ancestry<'a> {
    Invalid,
    Certified(Option<&'a str>),
}

struct SummaryAncestry<'a> {
    by_id: std::collections::HashMap<&'a str, Option<&'a Value>>,
    parents: &'a std::collections::BTreeSet<String>,
    memo: std::collections::HashMap<&'a str, Ancestry<'a>>,
}

impl<'a> SummaryAncestry<'a> {
    fn new(attempts: &'a [Value], parents: &'a std::collections::BTreeSet<String>) -> Self {
        let mut by_id = std::collections::HashMap::with_capacity(attempts.len());
        for row in attempts {
            if let Some(id) = row["attempt_id"].as_str().filter(|v| !v.is_empty()) {
                by_id
                    .entry(id)
                    .and_modify(|slot| *slot = None)
                    .or_insert(Some(row));
            }
        }
        Self {
            by_id,
            parents,
            memo: std::collections::HashMap::new(),
        }
    }

    fn task_for(&mut self, row: &'a Value) -> Option<&'a str> {
        let start_id = row["attempt_id"].as_str().filter(|v| !v.is_empty())?;
        let mut cursor = start_id;
        let mut path: Vec<(&'a str, Option<&'a str>)> = Vec::new();
        let mut in_path = std::collections::HashSet::new();
        let terminal = loop {
            if let Some(&cached) = self.memo.get(cursor) {
                break cached;
            }
            if self.parents.contains(cursor) {
                break if matches!(self.by_id.get(cursor), Some(Some(_))) {
                    Ancestry::Certified(None)
                } else {
                    Ancestry::Invalid
                };
            }
            if !in_path.insert(cursor) {
                break Ancestry::Invalid;
            }
            match self.by_id.get(cursor) {
                Some(Some(node)) => {
                    if node["tool"] == "panel-main" {
                        break Ancestry::Invalid;
                    }
                    let own_task = node["task_id"].as_str().filter(|v| !v.is_empty());
                    path.push((cursor, own_task));
                    if let Some(parent) =
                        node["parent_attempt_id"].as_str().filter(|v| !v.is_empty())
                    {
                        cursor = parent;
                    } else {
                        break Ancestry::Invalid;
                    }
                }
                Some(None) | None => break Ancestry::Invalid,
            }
        };
        let mut resolved = terminal;
        for (node_id, own_task) in path.into_iter().rev() {
            resolved = match resolved {
                Ancestry::Invalid => Ancestry::Invalid,
                Ancestry::Certified(above) => Ancestry::Certified(own_task.or(above)),
            };
            self.memo.insert(node_id, resolved);
        }
        match resolved {
            Ancestry::Certified(task) => task,
            Ancestry::Invalid => None,
        }
    }
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
        let tx = store.transaction("panel-results", Duration::from_secs(10))?;
        let snap = service.finalize_authority(repo, request, stop)?;
        tx.check()?;
        snap
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
        tx.check()?;
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
    let rows = index_rows(repo)?;
    let plan = store
        .json(Path::new(".oms/plan/tasks.json"), 4 * MAX_RESULT, true)?
        .unwrap_or(Value::Null);
    outcomes_from(&store, &plan, &rows, tasks)
}
/// Same as `outcomes` over rows and plan the caller already read.
pub fn outcomes_from(
    store: &Store,
    plan: &Value,
    rows: &[Value],
    tasks: &[String],
) -> Result<Vec<Value>> {
    outcomes_cached(store, plan, rows, tasks, None)
}
/// `outcomes_from` reusing a verified result payload while the row digest and
/// the artifact file stamp are unchanged.
pub fn outcomes_cached(
    store: &Store,
    plan: &Value,
    rows: &[Value],
    tasks: &[String],
    cache: Option<(&crate::cache::Cache, &Path)>,
) -> Result<Vec<Value>> {
    let wanted: std::collections::HashSet<&str> = tasks.iter().map(String::as_str).collect();
    let mut latest: std::collections::BTreeMap<String, &Value> = std::collections::BTreeMap::new();
    for row in rows {
        if row["kind"] != "panel-result" {
            continue;
        }
        let task = text(row, "task_id");
        if wanted.contains(task.as_str()) {
            latest.insert(task, row);
        }
    }
    let mut newest = std::collections::BTreeMap::new();
    for (task, row) in latest {
        let value = match cache {
            Some((cache, repo)) => {
                let key = format!("result:{task}");
                let print = format!(
                    "{}:{}",
                    text(row, "artifact_sha256"),
                    crate::cache::stamp(&repo.join(text(row, "artifact")))
                );
                match cache.read(&key, &print) {
                    Some(v) => v,
                    None => {
                        let v = load_result(store, row)?;
                        cache.insert(key, print, v.clone())?;
                        v
                    }
                }
            }
            None => load_result(store, row)?,
        };
        newest.insert(task, value);
    }
    newest
        .into_iter()
        .filter_map(
            |(task, value)| match crate::control::plan_binding(plan, &task) {
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
pub fn index_rows(repo: &Path) -> Result<Vec<Value>> {
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

const MAX_NATIVE_RETURNS: usize = 20;

fn native_action(kind: &str, reason: &str, commands: Vec<Value>) -> Value {
    serde_json::json!({"kind":kind,"reason":reason,"commands":commands})
}

fn native_guidance(operation: &Value, report: &mut Value, plan: &Value) -> Result<Value> {
    use crate::native_contract::NextAction;
    let inspect = || serde_json::json!({"argv":["oms","panel","--results","--task-id",operation["task_id"],"--json"]});
    let resolution = text(&operation["refs"], "native_resolution");
    let sha = text(report, "artifact_sha256");
    let resolved = text(&operation["refs"], "native_resolution_result_sha256");
    require(
        [
            "awaiting-report",
            "pending",
            "request-revision",
            "blocked",
            "reviewed",
            "retiring",
            "retired",
        ]
        .contains(&resolution.as_str())
            && if sha.is_empty() {
                ["awaiting-report", "retiring", "retired"].contains(&resolution.as_str())
                    && resolved.is_empty()
            } else {
                resolution != "awaiting-report"
                    && (resolved.is_empty() || resolved == sha)
                    && (resolution != "pending" || resolved.is_empty())
                    && (!["request-revision", "blocked", "reviewed"].contains(&resolution.as_str())
                        || resolved == sha)
            },
        "native resolution does not name this exact retained result",
    )?;
    report["resolution"] = resolution.clone().into();
    let task = array(&plan["tasks"])
        .iter()
        .find(|row| row["id"] == report["task_id"]);
    let same = task.filter(|task| {
        plan["plan_id"] == report["plan_id"]
            && task["lease_id"] == report["lease_id"]
            && task["lease_epoch"] == report["lease_epoch"]
            && task["provider"] == report["owner"]
            && task["claimed_by_participant"]
                == report
                    .get("parent_participant")
                    .unwrap_or(&report["parent_attempt_id"])
                    .clone()
    });
    let Some(task) = same else {
        report["plan_state"] = "unknown".into();
        return Ok(native_action(
            "inspect",
            "Current canonical task or lease is unavailable or changed",
            vec![inspect()],
        ));
    };
    report["plan_state"] = task["state"].clone();
    let reconcile = |decision: &str| serde_json::json!({"argv":["oms","panel","--native-reconcile",operation["attempt_id"],"--result-sha256",sha,"--decision",decision]});
    if matches!(task["state"].as_str(), Some("done" | "cancelled"))
        && task["completion_verified"] != false
    {
        let ready = serde_json::json!({"argv":["oms","agent-plan","ready","--json"]});
        return Ok(
            if resolution == "retired" && operation["terminal"] == true {
                native_action(
                    "next",
                    "Canonical task is settled; inspect the next ready task",
                    vec![ready],
                )
            } else if sha.is_empty() {
                native_action(
                    "retire",
                    "Canonical task is settled without a result; the next bound assignment must reconcile its reservation",
                    vec![ready],
                )
            } else {
                native_action(
                    "retire",
                    "Canonical task is settled; retire this assignment reservation before the next task",
                    vec![reconcile("reviewed")],
                )
            },
        );
    }
    if !matches!(
        task["state"].as_str(),
        Some("claimed" | "running" | "review")
    ) || task["claim_expired"] == true
        || task["contract_stale"] == true
        || task["completion_verified"] == false
        || resolution == "retiring"
        || resolution == "retired"
    {
        return Ok(native_action(
            "inspect",
            "Canonical task state needs reconciliation before another native action",
            vec![inspect()],
        ));
    }
    if sha.is_empty() {
        let action = match operation["refs"]["native_transport"].as_str() {
            Some("intent") => NextAction::Send,
            Some("submitted") => NextAction::Wait,
            _ => NextAction::Reconcile,
        };
        let mut next = native_action(action.name(), action.reason(), vec![inspect()]);
        if report["pending_input"] == true && operation["refs"]["native_transport"] != "intent" {
            next = native_action(
                "import",
                "An untrusted native return manifest is present; Main must import and validate it",
                vec![
                    serde_json::json!({"argv":["oms","panel","--orchestrator","codex","--task-id",report["task_id"],"--native-result",report["input_paths"]["manifest"]]}),
                ],
            );
        }
        return Ok(next);
    }
    if resolution == "request-revision" {
        return Ok(native_action(
            "continue",
            "Main requested another round of the same frozen task and lease",
            vec![
                serde_json::json!({"argv":["oms","panel","--orchestrator","codex","--task-id",report["task_id"],"--after-native-result",sha,"--task","<reviewed-revision-brief>"],"required_input":["task"]}),
            ],
        ));
    }
    if resolution == "reviewed" {
        return Ok(if report["patch"].is_object() {
            if task["state"] != "review"
                || task["review_lease_id"] != report["lease_id"]
                || task["artifact"] != report["artifact"]
                || task["patch"] != report["patch"]["path"]
            {
                native_action(
                    "inspect",
                    "Main review is recorded but its canonical Plan patch binding needs reconciliation",
                    vec![reconcile("reviewed")],
                )
            } else {
                native_action(
                    "admit",
                    "Main reviewed this candidate; run canonical admission from a clean integration checkout",
                    vec![
                        serde_json::json!({"argv":["oms","patch-admit","--patch",report["patch"]["path"],"--plan-task",report["task_id"],"--repo","<clean-integration-checkout>"],"required_input":["repo"]}),
                    ],
                )
            }
        } else {
            native_action(
                "verify",
                "Main reviewed report-only claims; completion still needs the canonical verified Plan path",
                vec![
                    serde_json::json!({"argv":["oms","agent-plan","evidence-snapshot","--id",report["task_id"]]}),
                ],
            )
        });
    }
    let action = if resolution == "blocked" || report["return_state"] == "blocked" {
        NextAction::ResolveBlocker
    } else if report["return_state"] == "needs-decision" {
        NextAction::Decide
    } else {
        NextAction::Review
    };
    Ok(native_action(
        action.name(),
        action.reason(),
        vec![
            reconcile("request-revision"),
            reconcile("blocked"),
            reconcile("reviewed"),
        ],
    ))
}

fn result_participant(operation: &Value, participant: &str) -> bool {
    operation["refs"]["panel_room_participant"] == participant
        || (operation["tool"] == "panel-native-assignment"
            && operation["refs"].get("native_return_schema").is_some()
            && operation["attempt_id"] == participant)
}

fn native_succession_guidance(
    store: &Store,
    operation: &Value,
    report: &mut Value,
    attempts: &Value,
) -> Result<Option<Value>> {
    let succession = crate::native_succession::metadata(store, operation)?;
    let custodian = succession.as_ref().map_or_else(
        || text(operation, "parent_attempt_id"),
        |record| text(record, "main_attempt_id"),
    );
    report["custodian_attempt_id"] = custodian.clone().into();
    report["succession"] = succession.unwrap_or(Value::Null);
    let mains: Vec<_> = array(attempts)
        .iter()
        .filter(|row| {
            row["tool"] == "panel-main"
                && row["refs"]["panel_room_id"] == operation["refs"]["panel_room_id"]
                && row["terminal"] != true
        })
        .collect();
    if mains.len() == 1 && mains[0]["attempt_id"] == custodian {
        return Ok(None);
    }
    // Legacy/offline result views have no current Main. They may still describe
    // the original contract, but never invent a successor from that absence.
    if mains.is_empty() {
        return Ok(None);
    }
    let inspect = serde_json::json!({"argv":["oms","panel","--results","--task-id",operation["task_id"],"--json"]});
    let old = array(attempts)
        .iter()
        .find(|row| row["attempt_id"] == custodian);
    if mains.len() != 1
        || !old.is_some_and(|old| {
            old["terminal"] == true
                && old["state"] == "abandoned"
                && old["reason_code"] == "main_swapped"
        })
        || crate::state::main_participant(mains[0]) != report["parent_participant"]
    {
        return Ok(Some(native_action(
            "inspect",
            "Native owner succession is unconfirmed; inspect the canonical Main handoff",
            vec![inspect],
        )));
    }
    let mut argv = serde_json::json!([
        "oms",
        "panel",
        "--native-adopt",
        operation["attempt_id"],
        "--assignment-sha256",
        report["binding_sha256"]
    ]);
    if let Some(digest) = report["artifact_sha256"].as_str() {
        argv.as_array_mut()
            .unwrap()
            .extend(["--result-sha256".into(), digest.into()]);
    }
    Ok(Some(native_action(
        "adopt",
        "A successor Main must explicitly adopt this exact frozen assignment before continuing",
        vec![serde_json::json!({"argv":argv})],
    )))
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
    let mut seen = std::collections::HashSet::new();
    for row in chronology.into_iter().rev() {
        let task = text(row, "task_id");
        if crate::identifier(&task).is_err()
            || (!request.include_preflight && task.starts_with("preflight-"))
        {
            continue;
        }
        if seen.insert(task.clone()) {
            latest.push(task);
        }
    }
    let mut by_task: std::collections::HashMap<String, Vec<usize>> = Default::default();
    let mut by_attempt: std::collections::HashMap<String, Vec<usize>> = Default::default();
    let mut result_at: std::collections::HashMap<String, usize> = Default::default();
    for (i, row) in indexed.iter().enumerate() {
        let task = text(row, "task_id");
        let attempt = text(row, "attempt_id");
        if row["kind"] == "panel-result" {
            result_at.insert(task.clone(), i);
        }
        by_task.entry(task).or_default().push(i);
        by_attempt.entry(attempt).or_default().push(i);
    }
    let mut output = Vec::new();
    let mut passed = 0;
    let mut capped = false;
    let mut native_remaining = MAX_NATIVE_RETURNS;
    let mut native_plan = None;
    let mut saw_typed = false;
    for task in latest {
        if request.task_id.as_ref().is_some_and(|v| v != &task) {
            continue;
        }
        let mut metadata_attempts = array(&attempts)
            .iter()
            .filter(|v| v["task_id"] == task)
            .collect::<Vec<_>>();
        if let Some(member) = &request.room_participant {
            metadata_attempts.retain(|v| result_participant(v, member));
        }
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
        let mut issue = Value::Null;
        let result_row = result_at.get(&task).map(|i| &indexed[*i]);
        let result = match result_row.map(|v| load_result(&store, v)).transpose() {
            Ok(v) => v.unwrap_or(Value::Null),
            Err(_) => {
                issue = "result or delivery evidence unavailable".into();
                Value::Null
            }
        };
        let mut matching = array(&attempts)
            .iter()
            .filter(|v| v["task_id"] == task)
            .collect::<Vec<_>>();
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
            matching.retain(|v| result_participant(v, member));
        }
        let linked = matching
            .iter()
            .map(|v| text(v, "attempt_id"))
            .collect::<std::collections::BTreeSet<_>>();
        let mut positions = by_task.get(&task).cloned().unwrap_or_default();
        for id in &adopted {
            positions.extend(by_attempt.get(id).into_iter().flatten());
        }
        positions.sort_unstable();
        positions.dedup();
        let rows = positions
            .into_iter()
            .map(|i| &indexed[i])
            .filter(|v| {
                request.room_participant.is_none() || linked.contains(&text(v, "attempt_id"))
            })
            .collect::<Vec<_>>();
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
                match indexed_answer(&store, row) {
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
        let native = metadata_attempts.iter().rfind(|v| {
            v["tool"] == "panel-native-assignment"
                && (v["refs"]["native_assignment_schema"] == "1"
                    || v["refs"].get("native_return_schema").is_some())
        });
        let typed = native.is_some_and(|v| v["refs"].get("native_return_schema").is_some());
        saw_typed |= typed;
        if request.task_id.is_some() || typed {
            // A retained owner report is separate from native input transport.
            // Neither proves consumption, completion or the Main's acceptance.
            projected["native_assignment"] = native.map_or(Value::Null, |v| {
                    let recorded = v["refs"]["native_transport"].as_str();
                    let valid = recorded
                        .filter(|t| ["intent", "sending", "submitted", "unknown"].contains(t));
                    let label = match valid {
                        Some("intent") => "Intent recorded",
                        Some("submitted") => "Submitted",
                        _ => "Submission unconfirmed",
                    };
                    let mut entry = serde_json::json!({"attempt_id":v["attempt_id"],"transport":valid.unwrap_or("unknown"),"transport_label":label,"process_state":v["state"],"consumption":"unconfirmed","acceptance":outcome});
                    if valid.is_none() {
                        entry["transport_issue"] = "invalid transport state".into();
                    }
                    if !typed { match crate::native_results::read(&store, v) {
                        Ok(Some(report)) => entry["report"] = report,
                        Ok(None) => {}
                        Err(error) => entry["report"] = serde_json::json!({
                            "status":"unavailable", "acceptance_eligible":false,
                            "issue":crate::eventlog::clean(&error.message,240)}),
                    }}
                    entry
                });
            if typed {
                let operation = native.unwrap();
                let observation = if operation["refs"]["native_return_schema"] != "1" {
                    Err(Error::busy("native return schema is unsupported"))
                } else if native_remaining == 0 {
                    Err(Error::busy(
                        "native return read limit reached; inspect this task directly",
                    ))
                } else {
                    native_remaining -= 1;
                    crate::native_results::metadata(&store, operation).and_then(|mut report| {
                        let plan = native_plan.get_or_insert_with(|| {
                            query(
                                service,
                                NativeCommand::Plan,
                                repo,
                                &["list", "--json"],
                                stop,
                            )
                            .unwrap_or(Value::Null)
                        });
                        let next = native_guidance(operation, &mut report, plan)?;
                        let next =
                            native_succession_guidance(&store, operation, &mut report, &attempts)?
                                .unwrap_or(next);
                        Ok((report, next))
                    })
                };
                let (report,next) = observation.unwrap_or_else(|error| {
                    let state = if error.code == 75 { "unknown" } else { "invalid" };
                    let reason = if error.code == 75 { "native return evidence is unavailable or beyond this bounded view" } else { "native return binding, identity or result evidence is invalid" };
                    let reason = if error.message == "native return read limit reached; inspect this task directly" || error.message == "native return schema is unsupported" { error.message.as_str() } else { reason };
                    (serde_json::json!({"status":"unavailable","return_state":state,"acceptance_eligible":false,"issue":reason}),
                        native_action("inspect",reason,vec![serde_json::json!({"argv":["oms","panel","--results","--task-id",task,"--json"]})]))
                });
                let entry = &mut projected["native_assignment"];
                entry["return_state"] = report["return_state"].clone();
                entry["resolution"] = report["resolution"].clone();
                entry["verification"] = "unverified".into();
                entry["child_cleanup"] = "unsupported".into();
                entry["report"] = report;
                entry["next_action"] = next;
            }
            // Only an undecided task without a failure moves its next step to the
            // recorded transport; nothing here resends or retries.
            let next = if typed {
                projected["native_assignment"]["next_action"]["reason"].as_str()
            } else {
                match projected["native_assignment"]["transport"].as_str() {
                    Some("intent") => Some("review unsent native assignment intent"),
                    Some("submitted") => {
                        match projected["native_assignment"]["report"]["status"].as_str() {
                            Some("reported") => Some(
                                "inspect native report; reconcile completion, verification and cleanup before acceptance",
                            ),
                            Some("unavailable") => {
                                Some("inspect unavailable native result evidence")
                            }
                            _ => Some("await native assignment result evidence"),
                        }
                    }
                    Some(_) => {
                        Some("inspect and reconcile unconfirmed native send before any retry")
                    }
                    None => None,
                }
            };
            if let Some(next) = next
                && projected["representative_state"] == "pending"
            {
                projected["next_action"] = next.into();
            }
        }
        output.push(projected);
    }
    let missing_task = request.task_id.is_some() && output.is_empty();
    let native_coverage = if saw_typed {
        "; native returns: at most 20 rows, 64 KiB each of binding/result/source; no candidate or verification bodies"
    } else {
        ""
    };
    Ok(
        serde_json::json!({"schema":1,"kind":"oms-panel-results","rows":output,"coverage":format!("{}; all retained artifact events; previously pruned evidence unavailable{native_coverage}",if request.task_id.is_some() { "all retained attempts" } else { "latest 300 attempts; attempt coverage may be incomplete" }),"truncated":capped || missing_task,"offset":request.offset,"omitted":if capped { Value::Null } else { 0.into() },"omitted_range":if capped { serde_json::json!([request.offset+output.len(),null]) } else { Value::Null }}),
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
/// Re-read native task, owner, candidate and review evidence at the landing
/// boundary. The caller must also match the patch and admission policy to the
/// fence; the supplied snapshot does not authorize a different landing.
pub fn verify_accept_fence<S: Services + ?Sized>(
    repo: &Path,
    execution: &Path,
    fence: &Value,
    service: &S,
    stop: &AtomicBool,
) -> Result<()> {
    crate::acceptance::verify_landing_fence(repo, execution, fence, service, stop)
}
/// Checks Git content against the parent acceptance. The native caller must
/// authenticate publication receipts separately; these arguments grant no authority.
pub fn verify_publication_contents<S: Services + ?Sized>(
    execution: &Path,
    published: &str,
    record: &Value,
    patch: &[u8],
    push_base: &str,
    service: &S,
    stop: &AtomicBool,
) -> Result<()> {
    crate::acceptance::verify_publication_contents(
        execution, published, record, patch, push_base, service, stop,
    )
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

/// Extract the same scrubbed answer used by the terminal result reader from
/// an indexed artifact whose location and content digest have been verified.
pub fn indexed_answer(store: &Store, row: &Value) -> Result<String> {
    answer(&indexed_artifact(store, row)?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandOutput, PeerExecutor};
    use serde_json::json;
    use std::ffi::OsString;

    const MAIN: &str = "att_11111111111111111111111111111111";
    const ROOM: &str = "room-finalization";
    const TASK: &str = "task-finalization";
    const WORKER: &str = "call-finalization";

    struct ClaimedTask(Vec<Value>);

    impl PeerExecutor for ClaimedTask {
        fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            panic!("authority snapshot must not execute a command")
        }
    }

    impl Services for ClaimedTask {
        fn command(
            &self,
            command: NativeCommand,
            _: &Path,
            args: &[OsString],
            _: &AtomicBool,
        ) -> Result<CommandOutput> {
            let value = match command {
                NativeCommand::Events => {
                    assert!(
                        args == ["list", "--json"] || args == ["list", "--json", "--limit", "300"],
                        "{args:?}"
                    );
                    let mut attempts = vec![json!({
                        "attempt_id": MAIN, "tool": "panel-main", "provider": "codex",
                        "state": "working", "terminal": false, "refs": {
                            "panel_role": "main", "panel_owner": "codex", "panel_room_id": ROOM
                        }
                    })];
                    attempts.extend_from_slice(&self.0);
                    Value::Array(attempts)
                }
                NativeCommand::Room => {
                    assert_eq!(args, &["show", "--id", ROOM, "--exact", "--json"]);
                    json!({
                        "id": ROOM, "closed": false, "participants": [
                            {"participant": MAIN, "provider": "codex", "role": "main", "joined": true},
                            {"participant": WORKER, "provider": "codex", "role": "worker", "joined": true}
                        ]
                    })
                }
                _ => panic!("unexpected authority query {command:?}"),
            };
            Ok(CommandOutput {
                stdout: serde_json::to_vec(&value).unwrap(),
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            })
        }
    }

    fn finalization_request(repo: &Path) -> FinalizeRequest {
        FinalizeRequest {
            owner: "codex".into(),
            task_id: TASK.into(),
            main_attempt_id: MAIN.into(),
            summary_file: "summary.md".into(),
            outcome: "completed".into(),
            verify: None,
            evidence: vec![],
            verify_timeout: Duration::from_secs(1),
            execution_repo: repo.to_owned(),
            notify: false,
            landed_commit: None,
        }
    }

    fn worker(attempt: &str, access: &str, state: &str) -> Value {
        json!({
            "attempt_id": attempt, "tool": "peer-delegate", "parent_attempt_id": MAIN,
            "task_id": TASK, "state": state, "terminal": state == "done",
            "refs": {"panel_role": "worker", "panel_access": access,
                "panel_room_id": ROOM, "panel_room_participant": attempt}
        })
    }

    #[test]
    fn claimed_plan_finalization_uses_proven_native_main_participant() {
        let repo = crate::git::Scratch::new().unwrap();
        let plan_file = repo.0.join(".oms/plan/tasks.json");
        std::fs::create_dir_all(plan_file.parent().unwrap()).unwrap();
        let mut plan = json!({
            "plan_id": format!("plan_{}", "1".repeat(32)),
            "tasks": {(TASK): {
                "id": TASK, "state": "running", "provider": "codex", "owner": MAIN,
                "claimed_by_participant": MAIN, "lease_id": format!("lease_{}", "2".repeat(32))
            }}
        });
        let save = |plan: &Value| {
            std::fs::write(&plan_file, serde_json::to_vec(plan).unwrap()).unwrap();
        };
        save(&plan);
        let request = finalization_request(&repo.0);
        let service = ClaimedTask(vec![worker(WORKER, "read", "done")]);
        let snapshot = || finalize_snapshot(&repo.0, &request, &service, &AtomicBool::new(false));
        let result = snapshot().unwrap();
        assert_eq!(result["binding"]["plan_id"], plan["plan_id"]);
        assert_eq!(result["main"]["attempt_id"], MAIN);
        assert_eq!(result["related_attempts"][0]["attempt_id"], WORKER);
        let raw = [
            result["main"].clone(),
            result["related_attempts"][0].clone(),
        ];
        let projected = attempts_with_main_binding(&raw, &raw[0]);
        assert_eq!(projected[0]["refs"]["panel_room_participant"], MAIN);
        assert_eq!(projected[1], raw[1]);
        assert!(raw[0]["refs"].get("panel_room_participant").is_none());
        assert_eq!(
            std::fs::read(&plan_file).unwrap(),
            serde_json::to_vec(&plan).unwrap()
        );
        assert!(!repo.0.join(".oms/artifacts").exists());

        plan["tasks"][TASK]["claimed_by_participant"] = WORKER.into();
        save(&plan);
        assert_eq!(
            snapshot().unwrap_err().message,
            "plan claimant differs from current main"
        );
    }

    #[test]
    fn finalization_names_bounded_proven_blockers_without_writing() {
        let repo = crate::git::Scratch::new().unwrap();
        let request = finalization_request(&repo.0);
        let ack_path = repo.0.join(format!(
            ".oms/artifacts/panel-dispatch/{WORKER}-process.json"
        ));
        std::fs::create_dir_all(ack_path.parent().unwrap()).unwrap();
        let route = json!({"owner":"codex", "role":"worker", "access":"write"});
        let ack = crate::value::encoded(&json!({
            "schema":1, "member":WORKER, "task_id":TASK, "exit":0,
            "cleanup_complete":true, "output_complete":true, "state":"awaiting",
            "route_snapshot_sha256":crate::digest(&crate::value::encoded(&route).unwrap()),
            "route_snapshot":route
        }))
        .unwrap();
        std::fs::write(&ack_path, &ack).unwrap();
        let plan_file = repo.0.join(".oms/plan/tasks.json");
        std::fs::create_dir_all(plan_file.parent().unwrap()).unwrap();
        let plan = json!({
            "plan_id": format!("plan_{}", "1".repeat(32)),
            "tasks": {(TASK): {
                "id": TASK, "state": "running", "provider": "codex", "owner": MAIN,
                "claimed_by_participant": MAIN, "lease_id": format!("lease_{}", "2".repeat(32))
            }}
        });
        let plan_bytes = serde_json::to_vec(&plan).unwrap();
        std::fs::write(&plan_file, &plan_bytes).unwrap();
        fn snapshot_tree(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap())
                .collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    snapshot_tree(root, &path, out);
                } else {
                    out.push((
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        std::fs::read(&path).unwrap(),
                    ));
                }
            }
        }
        let snapshot = || {
            let mut files = Vec::new();
            snapshot_tree(&repo.0, &repo.0, &mut files);
            files.sort_by(|a, b| a.0.cmp(&b.0));
            files
        };
        let initial_snapshot = snapshot();
        let mut accepted = request.clone();
        accepted.outcome = "accepted".into();
        let mut landed = request.clone();
        landed.landed_commit = Some("a".repeat(40));
        let refusal_for = |req: &FinalizeRequest, attempts, expected: String| {
            let err = finalize_snapshot(
                &repo.0,
                req,
                &ClaimedTask(attempts),
                &AtomicBool::new(false),
            )
            .unwrap_err();
            assert_eq!(err, Error::new(expected));
            err
        };
        let refusal = |attempts, expected: String| refusal_for(&request, attempts, expected);
        let accepted_refusal =
            |attempts, expected: String| refusal_for(&accepted, attempts, expected);
        let read = worker(WORKER, "read", "working");
        let review_id = "call-finalization:review";
        let mut review = worker(review_id, "write", "review");
        review["refs"]["panel_room_participant"] = WORKER.into();
        let mut read_review = worker("call-read-review", "read", "review");
        read_review["refs"]["panel_room_participant"] = WORKER.into();
        let later = worker("call-later", "write", "running");
        let done = worker("call-done", "write", "done");
        refusal(
            vec![done.clone(), read.clone(), later.clone()],
            format!(
                "task has live or unconfirmed attempts (task attempt {WORKER}, lifecycle state working · task attempt call-later, lifecycle state running)"
            ),
        );
        // A zero exit with complete output and cleanup in the public process
        // acknowledgement cannot substitute for protected whole-tree evidence.
        refusal(
            vec![review.clone()],
            format!(
                "task has live or unconfirmed attempts (task attempt {review_id}, missing verified whole-tree cleanup)"
            ),
        );
        refusal(
            vec![read_review],
            "task has live or unconfirmed attempts (task attempt call-read-review, missing verified whole-tree cleanup)"
                .to_owned(),
        );
        let mut taskless = worker(WORKER, "write", "waiting_approval");
        taskless["task_id"] = Value::Null;
        accepted_refusal(
            vec![taskless.clone()],
            format!(
                "task has live or unconfirmed attempts (inherited taskless descendant attempt {WORKER}, lifecycle state waiting_approval)"
            ),
        );
        refusal_for(
            &landed,
            vec![done.clone(), taskless.clone()],
            format!(
                "task has live or unconfirmed attempts (inherited taskless descendant attempt {WORKER}, lifecycle state waiting_approval)"
            ),
        );
        let taskless_review_id = "call-taskless-review";
        let mut taskless_review = worker(taskless_review_id, "write", "review");
        taskless_review["task_id"] = "".into();
        taskless_review["refs"]["panel_room_participant"] = WORKER.into();
        accepted_refusal(
            vec![
                done.clone(),
                review.clone(),
                taskless_review.clone(),
                taskless.clone(),
            ],
            format!(
                "task has live or unconfirmed attempts (task attempt {review_id}, missing verified whole-tree cleanup · inherited taskless descendant attempt {taskless_review_id}, missing verified whole-tree cleanup · inherited taskless descendant attempt {WORKER}, lifecycle state waiting_approval)"
            ),
        );
        let exact_cap_err = accepted_refusal(
            vec![
                done.clone(),
                read.clone(),
                review.clone(),
                taskless_review.clone(),
                taskless.clone(),
            ],
            format!(
                "task has live or unconfirmed attempts (task attempt {WORKER}, lifecycle state working · task attempt {review_id}, missing verified whole-tree cleanup · inherited taskless descendant attempt {taskless_review_id}, missing verified whole-tree cleanup · inherited taskless descendant attempt {WORKER}, lifecycle state waiting_approval)"
            ),
        );
        assert!(!exact_cap_err.message.contains("more remain"));
        let mut fifth = worker("call-fifth", "write", "blocked");
        fifth["task_id"] = Value::Null;
        let sixth = worker("call-sixth", "write", "verifying");
        let overflow_err = accepted_refusal(
            vec![
                done.clone(),
                review,
                taskless_review.clone(),
                read.clone(),
                later,
                fifth,
                sixth,
            ],
            format!(
                "task has live or unconfirmed attempts (task attempt {review_id}, missing verified whole-tree cleanup · inherited taskless descendant attempt {taskless_review_id}, missing verified whole-tree cleanup · task attempt {WORKER}, lifecycle state working · task attempt call-later, lifecycle state running · more remain)"
            ),
        );
        assert!(!overflow_err.message.contains("call-fifth"));
        assert!(!overflow_err.message.contains("call-sixth"));
        // Summary-only outcomes exclude unrelated taskless siblings of Main and
        // taskless descendants of other tasks, while still blocking on proven
        // taskless descendants of this task.
        let mut other_task_root = worker("call-other-task", "read", "working");
        other_task_root["task_id"] = "other-task".into();
        let mut other_task_child = worker("call-other-child", "read", "working");
        other_task_child["task_id"] = Value::Null;
        other_task_child["parent_attempt_id"] = "call-other-task".into();
        for outcome in ["completed", "failed", "rejected"] {
            let mut summary_req = request.clone();
            summary_req.outcome = outcome.into();
            let snap = finalize_snapshot(
                &repo.0,
                &summary_req,
                &ClaimedTask(vec![
                    done.clone(),
                    taskless.clone(),
                    taskless_review.clone(),
                    other_task_root.clone(),
                    other_task_child.clone(),
                ]),
                &AtomicBool::new(false),
            )
            .unwrap();
            let related = array(&snap["related_attempts"]);
            assert_eq!(related.len(), 1);
            assert_eq!(related[0]["attempt_id"], "call-done");
        }
        let mut task_child = worker("call-task-child", "read", "working");
        task_child["task_id"] = Value::Null;
        task_child["parent_attempt_id"] = "call-done".into();
        let mut task_child_review = worker("call-task-child-review", "read", "review");
        task_child_review["task_id"] = "".into();
        task_child_review["parent_attempt_id"] = "call-done".into();
        task_child_review["refs"]["panel_room_participant"] = WORKER.into();
        let mut task_mid = worker("call-task-mid", "read", "done");
        task_mid["task_id"] = "".into();
        task_mid["parent_attempt_id"] = "call-done".into();
        let mut task_grandchild = worker("call-task-grandchild", "read", "waiting_approval");
        task_grandchild["task_id"] = Value::Null;
        task_grandchild["parent_attempt_id"] = "call-task-mid".into();
        let mut task_grandchild_review = worker("call-task-grandchild-review", "read", "review");
        task_grandchild_review["task_id"] = Value::Null;
        task_grandchild_review["parent_attempt_id"] = "call-task-mid".into();
        task_grandchild_review["refs"]["panel_room_participant"] = WORKER.into();
        for outcome in ["completed", "failed", "rejected"] {
            let mut summary_req = request.clone();
            summary_req.outcome = outcome.into();
            refusal_for(
                &summary_req,
                vec![
                    done.clone(),
                    taskless.clone(),
                    other_task_root.clone(),
                    other_task_child.clone(),
                    task_child.clone(),
                    task_child_review.clone(),
                    task_mid.clone(),
                    task_grandchild.clone(),
                    task_grandchild_review.clone(),
                ],
                "task has live or unconfirmed attempts (inherited taskless descendant attempt call-task-child, lifecycle state working · inherited taskless descendant attempt call-task-child-review, missing verified whole-tree cleanup · inherited taskless descendant attempt call-task-grandchild, lifecycle state waiting_approval · inherited taskless descendant attempt call-task-grandchild-review, missing verified whole-tree cleanup)"
                    .to_owned(),
            );
        }
        // Accepted/landed preserves the direct-Main taskless safeguard and does
        // not widen into nested taskless children via nearest_task_id.
        let done_read = worker("call-done-read", "read", "done");
        let mut nested_child = worker("call-nested-child", "read", "working");
        nested_child["task_id"] = Value::Null;
        nested_child["parent_attempt_id"] = "call-done-read".into();
        let landed_snap = finalize_snapshot(
            &repo.0,
            &landed,
            &ClaimedTask(vec![done_read.clone(), nested_child.clone()]),
            &AtomicBool::new(false),
        )
        .unwrap();
        let landed_related = array(&landed_snap["related_attempts"]);
        assert_eq!(landed_related.len(), 1);
        assert_eq!(landed_related[0]["attempt_id"], "call-done-read");
        refusal_for(
            &accepted,
            vec![done_read, nested_child],
            "accepted requires native plan completion/publication proof".to_owned(),
        );
        // Summary-only ancestry certification refuses to cross a foreign
        // panel-main even when linked to current Main, and rejects cycles,
        // missing parents, and ambiguous duplicate parent IDs.
        let foreign_main = json!({
            "attempt_id": "main-foreign", "tool": "panel-main", "provider": "codex",
            "parent_attempt_id": MAIN, "state": "working", "terminal": false
        });
        let mut done_under_foreign = worker("call-done-under-foreign", "read", "done");
        done_under_foreign["parent_attempt_id"] = "main-foreign".into();
        let mut taskless_under_foreign = worker("call-taskless-under-foreign", "read", "working");
        taskless_under_foreign["task_id"] = Value::Null;
        taskless_under_foreign["parent_attempt_id"] = "call-done-under-foreign".into();
        let foreign_snap = finalize_snapshot(
            &repo.0,
            &request,
            &ClaimedTask(vec![
                foreign_main,
                done_under_foreign,
                taskless_under_foreign,
            ]),
            &AtomicBool::new(false),
        )
        .unwrap();
        let foreign_related = array(&foreign_snap["related_attempts"]);
        assert_eq!(foreign_related.len(), 1);
        assert_eq!(foreign_related[0]["attempt_id"], "call-done-under-foreign");

        let mut cycle_a = worker("call-cycle-a", "read", "working");
        cycle_a["task_id"] = Value::Null;
        cycle_a["parent_attempt_id"] = "call-cycle-b".into();
        let mut cycle_b = worker("call-cycle-b", "read", "working");
        cycle_b["task_id"] = Value::Null;
        cycle_b["parent_attempt_id"] = "call-cycle-a".into();
        let mut orphan = worker("call-orphan", "read", "working");
        orphan["task_id"] = Value::Null;
        orphan["parent_attempt_id"] = "call-missing".into();
        let graph_snap = finalize_snapshot(
            &repo.0,
            &request,
            &ClaimedTask(vec![done.clone(), cycle_a, cycle_b, orphan]),
            &AtomicBool::new(false),
        )
        .unwrap();
        let graph_related = array(&graph_snap["related_attempts"]);
        assert_eq!(graph_related.len(), 1);
        assert_eq!(graph_related[0]["attempt_id"], "call-done");

        let mut cycle_task = worker("call-cycle-task", "read", "done");
        cycle_task["parent_attempt_id"] = "call-cycle-task-parent".into();
        let mut cycle_task_parent = worker("call-cycle-task-parent", "read", "done");
        cycle_task_parent["task_id"] = Value::Null;
        cycle_task_parent["parent_attempt_id"] = "call-cycle-task".into();
        let mut cycle_task_child = worker("call-cycle-task-child", "read", "working");
        cycle_task_child["task_id"] = Value::Null;
        cycle_task_child["parent_attempt_id"] = "call-cycle-task".into();
        let cycle_attempts = vec![cycle_task, cycle_task_parent, cycle_task_child];
        let cycle_parents = std::collections::BTreeSet::from([MAIN.to_owned()]);
        let mut cycle_ancestry = SummaryAncestry::new(&cycle_attempts, &cycle_parents);
        assert_eq!(cycle_ancestry.task_for(&cycle_attempts[2]), None);

        let dup_a = worker("call-dup-parent", "read", "done");
        let dup_b = worker("call-dup-parent", "read", "done");
        let mut taskless_dup = worker("call-taskless-dup", "read", "working");
        taskless_dup["task_id"] = Value::Null;
        taskless_dup["parent_attempt_id"] = "call-dup-parent".into();
        let dup_snap = finalize_snapshot(
            &repo.0,
            &request,
            &ClaimedTask(vec![dup_a, dup_b, taskless_dup]),
            &AtomicBool::new(false),
        )
        .unwrap();
        let dup_related = array(&dup_snap["related_attempts"]);
        assert_eq!(dup_related.len(), 2);
        assert!(
            dup_related
                .iter()
                .all(|row| row["attempt_id"] == "call-dup-parent")
        );

        let mut deep_chain = Vec::with_capacity(1002);
        deep_chain.push(done.clone());
        let mut prev_id = "call-done".to_owned();
        for idx in 0..1000 {
            let id = format!("call-deep-{idx}");
            let mut node = worker(&id, "read", "done");
            node["task_id"] = Value::Null;
            node["parent_attempt_id"] = prev_id.into();
            deep_chain.push(node);
            prev_id = id;
        }
        let deep_snap = finalize_snapshot(
            &repo.0,
            &request,
            &ClaimedTask(deep_chain.clone()),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(array(&deep_snap["related_attempts"]).len(), 1001);
        let mut deep_live = worker("call-deep-live", "read", "working");
        deep_live["task_id"] = Value::Null;
        deep_live["parent_attempt_id"] = prev_id.into();
        deep_chain.push(deep_live);
        refusal(
            deep_chain,
            "task has live or unconfirmed attempts (inherited taskless descendant attempt call-deep-live, lifecycle state working)"
                .to_owned(),
        );
        let mut foreign = worker("call-foreign", "write", "working");
        foreign["parent_attempt_id"] = "foreign-main".into();
        refusal(
            vec![read, foreign],
            "task includes attempts owned by another main".to_owned(),
        );
        refusal(
            vec![worker("/untrusted/attempt", "read", "/untrusted/state")],
            "task has live or unconfirmed attempts (task attempt unreported, lifecycle state unknown)"
                .to_owned(),
        );
        let secret_id = concat!("gh", "p_12345678901234567890abcd");
        assert!(crate::native::operations::projection::id(secret_id));
        let secret_err = refusal(
            vec![worker(secret_id, "read", "working")],
            "task has live or unconfirmed attempts (task attempt unreported, lifecycle state working)"
                .to_owned(),
        );
        assert!(crate::outbound(secret_err.message.as_bytes()).is_ok());
        assert_eq!(std::fs::read(&plan_file).unwrap(), plan_bytes);
        assert_eq!(std::fs::read(&ack_path).unwrap(), ack);
        assert!(!repo.0.join(".oms/artifacts/panel-results").exists());
        assert!(!repo.0.join(".oms/lifecycle").exists());
        assert_eq!(snapshot(), initial_snapshot);
    }

    fn native(attempt: &str, task: &str, transport: Value) -> Value {
        json!({
            "attempt_id": attempt, "tool": "panel-native-assignment", "provider": "codex",
            "parent_attempt_id": MAIN, "task_id": task, "state": "working", "terminal": false,
            "refs": {"native_assignment_schema": "1", "plan_id": format!("plan_{}", "1".repeat(32)),
                "lease_id": format!("lease_{}", "2".repeat(32)), "panel_room_id": ROOM,
                "native_transport": transport, "native_room_message": format!("msg-{attempt}"),
                "native_binding_sha256": "3".repeat(64)}
        })
    }

    fn index(repo: &Path, rows: &[Value]) {
        let raw = rows
            .iter()
            .flat_map(|v| [serde_json::to_vec(v).unwrap(), b"\n".to_vec()])
            .flatten()
            .collect::<Vec<_>>();
        Store::open(repo)
            .unwrap()
            .lock_at(Path::new("lock"), Duration::from_secs(3))
            .unwrap()
            .write(
                Path::new(".oms/artifacts/index.jsonl"),
                &raw,
                false,
                MAX_RESULT,
            )
            .unwrap();
    }

    fn task_view(repo: &Path, task: Option<&str>, attempts: Vec<Value>) -> Value {
        let request = ResultsRequest {
            task_id: task.map(str::to_owned),
            ..Default::default()
        };
        results_page(
            repo,
            &request,
            &ClaimedTask(attempts),
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    struct TypedResults {
        attempts: Vec<Value>,
        plan: Value,
        plan_reads: std::sync::atomic::AtomicUsize,
    }
    impl PeerExecutor for TypedResults {
        fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            panic!("result observation cannot execute a verifier or provider")
        }
    }
    impl Services for TypedResults {
        fn command(
            &self,
            command: NativeCommand,
            repo: &Path,
            args: &[OsString],
            stop: &AtomicBool,
        ) -> Result<CommandOutput> {
            if command == NativeCommand::Plan {
                assert_eq!(args, ["list", "--json"]);
                self.plan_reads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(CommandOutput {
                    stdout: serde_json::to_vec(&self.plan).unwrap(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            } else {
                ClaimedTask(self.attempts.clone()).command(command, repo, args, stop)
            }
        }
    }
    fn typed_view(
        repo: &Path,
        request: ResultsRequest,
        attempts: Vec<Value>,
        tasks: Vec<Value>,
    ) -> Value {
        let host = TypedResults {
            attempts,
            plan: json!({"schema":1,"plan_id":format!("plan_{}","a".repeat(32)),"tasks":tasks}),
            plan_reads: Default::default(),
        };
        let value = results_page(repo, &request, &host, &AtomicBool::new(false)).unwrap();
        assert!(host.plan_reads.load(std::sync::atomic::Ordering::Relaxed) <= 1);
        value
    }

    #[test]
    fn typed_native_returns_match_task_room_and_list_without_acceptance() {
        let repo = crate::git::Scratch::new().unwrap();
        for (outcome, action) in [
            ("reported", "review"),
            ("blocked", "resolve-blocker"),
            ("needs-decision", "decide"),
        ] {
            let (mut operation, task, assignment) =
                crate::native_results::tests::fixture(&repo.0, &repo.0, "native-typed", TASK);
            let mut source = assignment.report_template();
            source["summary"] = "Claimed result for Main review".into();
            source["outcome"] = outcome.into();
            source["checks"] = json!([{"name":"frozen verifier","status":"passed","evidence":[],"note":"Lead claim only"}]);
            crate::native_results::tests::retain(&repo.0, &mut operation, &assignment, &source);
            let mut baseline = None;
            for request in [
                ResultsRequest::default(),
                ResultsRequest {
                    task_id: Some(TASK.into()),
                    ..Default::default()
                },
                ResultsRequest {
                    room_participant: Some("native-typed".into()),
                    ..Default::default()
                },
            ] {
                let view = typed_view(
                    &repo.0,
                    request,
                    vec![operation.clone()],
                    vec![task.clone()],
                );
                let row = &view["rows"][0];
                let entry = &row["native_assignment"];
                if let Some(before) = &baseline {
                    assert_eq!(entry, before);
                } else {
                    baseline = Some(entry.clone());
                }
                assert_eq!(entry["return_state"], outcome);
                assert_eq!(entry["resolution"], "pending");
                assert_eq!(entry["next_action"]["kind"], action);
                assert_eq!(
                    entry["report"]["manifest_template"]["previous_sha256"],
                    operation["refs"]["native_result_sha256"]
                );
                assert_eq!(
                    entry["report"]["source_template"]["previous_report_sha256"],
                    entry["report"]["source"]["sha256"]
                );
                assert_eq!(entry["report"]["return_claims"], source);
                assert_eq!(entry["consumption"], "unconfirmed");
                assert_eq!(entry["verification"], "unverified");
                assert_eq!(entry["acceptance"], "pending");
                assert_eq!(row["outcome"], Value::Null);
                assert_eq!(row["acceptance"], false);
                assert_eq!(row["stages"]["verify"]["state"], "not_run");
                assert_eq!(row["process_state"], "working");
            }
        }
    }

    #[test]
    fn typed_native_projection_bounds_reads_and_distinguishes_absent_unknown_invalid() {
        let repo = crate::git::Scratch::new().unwrap();
        let mut operations = Vec::new();
        let mut tasks = Vec::new();
        for i in 0..=MAX_NATIVE_RETURNS {
            let (operation, task, _) = crate::native_results::tests::fixture(
                &repo.0,
                &repo.0,
                &format!("native-{i}"),
                &format!("task-{i}"),
            );
            operations.push(operation);
            tasks.push(task);
        }
        let view = typed_view(
            &repo.0,
            ResultsRequest {
                limit: MAX_NATIVE_RETURNS + 1,
                ..Default::default()
            },
            operations.clone(),
            tasks.clone(),
        );
        let rows = array(&view["rows"]);
        assert_eq!(rows.len(), MAX_NATIVE_RETURNS + 1);
        assert_eq!(
            rows.iter()
                .filter(|row| row["native_assignment"]["return_state"] == "absent")
                .count(),
            MAX_NATIVE_RETURNS
        );
        let deferred = rows
            .iter()
            .find(|row| row["native_assignment"]["return_state"] == "unknown")
            .unwrap();
        assert!(
            text(&deferred["native_assignment"]["next_action"], "reason").contains("read limit")
        );
        let selected = text(deferred, "task_id");
        assert_eq!(
            typed_view(
                &repo.0,
                ResultsRequest {
                    task_id: Some(selected),
                    ..Default::default()
                },
                operations.clone(),
                tasks.clone()
            )["rows"][0]["native_assignment"]["return_state"],
            "absent"
        );
        assert_eq!(
            typed_view(
                &repo.0,
                ResultsRequest {
                    offset: MAX_NATIVE_RETURNS,
                    limit: 1,
                    ..Default::default()
                },
                operations.clone(),
                tasks.clone()
            )["rows"][0]["native_assignment"]["return_state"],
            "absent"
        );
        for (field, value, state) in [
            ("native_return_schema", "2", "unknown"),
            ("panel_owner", "foreign", "invalid"),
            ("native_result_schema", "1", "invalid"),
        ] {
            let mut operation = operations[0].clone();
            operation["refs"][field] = value.into();
            assert_eq!(
                typed_view(
                    &repo.0,
                    ResultsRequest::default(),
                    vec![operation],
                    tasks.clone()
                )["rows"][0]["native_assignment"]["return_state"],
                state,
                "{field}"
            );
        }
        std::fs::remove_file(repo.0.join(text(&operations[0]["refs"], "native_binding"))).unwrap();
        assert_eq!(
            typed_view(
                &repo.0,
                ResultsRequest::default(),
                vec![operations[0].clone()],
                tasks
            )["rows"][0]["native_assignment"]["return_state"],
            "unknown"
        );
    }

    #[test]
    fn typed_native_guidance_imports_untrusted_input_and_retires_only_canonical_settlement() {
        let repo = crate::git::Scratch::new().unwrap();
        let (mut operation, task, assignment) =
            crate::native_results::tests::fixture(&repo.0, &repo.0, "native-settle", TASK);
        let paths = crate::native_results::input_paths(&assignment);
        crate::native_results::tests::write(
            &repo.0,
            paths["manifest"].as_str().unwrap(),
            b"untrusted pending manifest",
        );
        let view = typed_view(
            &repo.0,
            ResultsRequest::default(),
            vec![operation.clone()],
            vec![task.clone()],
        );
        assert_eq!(
            view["rows"][0]["native_assignment"]["return_state"],
            "absent"
        );
        assert_eq!(
            view["rows"][0]["native_assignment"]["next_action"]["kind"],
            "import"
        );
        assert_eq!(view["rows"][0]["acceptance"], false);
        let mut source = assignment.report_template();
        source["summary"] = "Claimed result".into();
        crate::native_results::tests::retain(&repo.0, &mut operation, &assignment, &source);
        operation["refs"]["native_resolution"] = "reviewed".into();
        operation["refs"]["native_resolution_result_sha256"] =
            operation["refs"]["native_result_sha256"].clone();
        for state in ["done", "cancelled"] {
            let mut current = task.clone();
            current["state"] = state.into();
            let view = typed_view(
                &repo.0,
                ResultsRequest::default(),
                vec![operation.clone()],
                vec![current.clone()],
            );
            let row = &view["rows"][0];
            assert_eq!(row["native_assignment"]["next_action"]["kind"], "retire");
            assert_eq!(
                row["native_assignment"]["next_action"]["commands"][0]["argv"][5],
                operation["refs"]["native_result_sha256"]
            );
            assert_eq!(row["acceptance"], false);
            assert_eq!(row["stages"]["verify"]["state"], "not_run");
            let mut retired = operation.clone();
            retired["terminal"] = true.into();
            retired["refs"]["native_resolution"] = "retired".into();
            assert_eq!(
                typed_view(
                    &repo.0,
                    ResultsRequest::default(),
                    vec![retired],
                    vec![current.clone()]
                )["rows"][0]["native_assignment"]["next_action"]["kind"],
                "next"
            );
            current["completion_verified"] = false.into();
            assert_eq!(
                typed_view(
                    &repo.0,
                    ResultsRequest::default(),
                    vec![operation.clone()],
                    vec![current]
                )["rows"][0]["native_assignment"]["next_action"]["kind"],
                "inspect"
            );
        }
        operation["refs"]["native_resolution_result_sha256"] = "a".repeat(64).into();
        assert_eq!(
            typed_view(
                &repo.0,
                ResultsRequest::default(),
                vec![operation],
                vec![task]
            )["rows"][0]["native_assignment"]["return_state"],
            "invalid"
        );
    }

    #[test]
    fn native_assignment_result_reports_latest_transport_only() {
        let repo = crate::git::Scratch::new().unwrap();
        let unsent = "review unsent native assignment intent";
        let awaiting = "await native assignment result evidence";
        let reconcile = "inspect and reconcile unconfirmed native send before any retry";
        for (recorded, transport, label, next) in [
            (json!("intent"), "intent", "Intent recorded", unsent),
            (
                json!("sending"),
                "sending",
                "Submission unconfirmed",
                reconcile,
            ),
            (json!("submitted"), "submitted", "Submitted", awaiting),
            (
                json!("unknown"),
                "unknown",
                "Submission unconfirmed",
                reconcile,
            ),
            (
                json!("consumed"),
                "unknown",
                "Submission unconfirmed",
                reconcile,
            ),
            (json!(7), "unknown", "Submission unconfirmed", reconcile),
            (Value::Null, "unknown", "Submission unconfirmed", reconcile),
        ] {
            let mut newer_schema = native("native-v2", TASK, "submitted".into());
            newer_schema["refs"]["native_assignment_schema"] = "2".into();
            let attempts = vec![
                native("native-old", TASK, "submitted".into()),
                native("native-new", TASK, recorded.clone()),
                newer_schema,
            ];
            let row = &task_view(&repo.0, Some(TASK), attempts)["rows"][0];
            let issue = (transport == "unknown" && recorded != "unknown")
                .then_some("invalid transport state");
            let mut expected = json!({"attempt_id": "native-new", "transport": transport,
                "transport_label": label, "process_state": "working",
                "consumption": "unconfirmed", "acceptance": "pending"});
            if let Some(issue) = issue {
                expected["transport_issue"] = issue.into();
            }
            assert_eq!(row["native_assignment"], expected, "{recorded}");
            assert_eq!(row["outcome"], Value::Null);
            assert_eq!(row["representative_state"], "pending");
            assert_eq!(row["next_action"], next, "{recorded}");
        }
        // An independent terminal failure keeps its own state and action.
        let mut failed = native("native-new", TASK, "unknown".into());
        failed["state"] = "failed".into();
        let row = &task_view(&repo.0, Some(TASK), vec![failed])["rows"][0];
        assert_eq!(row["representative_state"], "failed");
        assert_eq!(row["next_action"], "inspect failure and retry");
        // Without a native assignment the undecided task still awaits Main.
        let row = &task_view(
            &repo.0,
            Some(TASK),
            vec![worker("call-x", "read", "working")],
        )["rows"][0];
        assert_eq!(row["native_assignment"], Value::Null);
        assert_eq!(row["next_action"], "record main decision");
    }

    #[test]
    fn native_assignment_result_never_confirms_consumption_from_bound_evidence() {
        let repo = crate::git::Scratch::new().unwrap();
        index(
            &repo.0,
            &[
                json!({"schema": 1, "kind": "note", "ts": "2026-01-01T00:00:00Z",
                "task_id": TASK, "attempt_id": "native-a"}),
            ],
        );
        for transport in ["intent", "submitted"] {
            let mut child = worker("call-child", "read", "working");
            child["parent_attempt_id"] = "native-a".into();
            let attempts = vec![native("native-a", TASK, transport.into()), child];
            let entry = &task_view(&repo.0, Some(TASK), attempts)["rows"][0]["native_assignment"];
            assert_eq!(entry["attempt_id"], "native-a");
            assert_eq!(entry["transport"], transport);
            assert_eq!(entry["consumption"], "unconfirmed", "{entry}");
            assert_eq!(entry["acceptance"], "pending");
        }
    }

    #[test]
    fn native_assignment_result_mirrors_finalized_decision_not_transport() {
        for outcome in ["accepted", "rejected"] {
            let repo = crate::git::Scratch::new().unwrap();
            let revision = "4".repeat(24);
            let summary = "Reviewed native assignment";
            let body = serde_json::to_vec(&json!({
                "schema": 1, "kind": "oms-panel-result", "task_id": TASK, "revision": revision,
                "summary": summary, "summary_sha256": crate::digest(summary.as_bytes()),
                "outcome": outcome, "main_attempt_id": MAIN
            }))
            .unwrap();
            let artifact = format!(".oms/artifacts/panel-results/result-{TASK}-{revision}.json");
            Store::open(&repo.0)
                .unwrap()
                .lock_at(Path::new("lock"), Duration::from_secs(3))
                .unwrap()
                .write(Path::new(&artifact), &body, false, MAX_RESULT)
                .unwrap();
            index(
                &repo.0,
                &[
                    json!({"schema": 1, "kind": "panel-result", "ts": "2026-01-01T00:00:00Z",
                    "task_id": TASK, "attempt_id": MAIN, "artifact": artifact,
                    "artifact_sha256": crate::digest(&body)}),
                ],
            );
            let view = task_view(
                &repo.0,
                Some(TASK),
                vec![native("native-a", TASK, "submitted".into())],
            );
            let row = &view["rows"][0];
            assert_eq!(row["outcome"], outcome, "{row}");
            assert_eq!(
                row["representative_state"],
                format!("main_decision {outcome}")
            );
            assert_eq!(row["next_action"], "none");
            assert_eq!(row["stages"]["main_decision"]["state"], outcome);
            let entry = &row["native_assignment"];
            assert_eq!(entry["acceptance"], outcome);
            assert_eq!(entry["transport"], "submitted");
            assert_eq!(entry["consumption"], "unconfirmed");
            // A taskless attempt adopted through main_attempt_id is not this task's.
            let mut adopted = native("native-adopted", TASK, "submitted".into());
            adopted["task_id"] = Value::Null;
            let row = &task_view(&repo.0, Some(TASK), vec![adopted])["rows"][0];
            assert_eq!(row["outcome"], outcome);
            assert_eq!(row["native_assignment"], Value::Null, "{row}");
        }
    }

    #[test]
    fn native_assignment_result_leaves_other_tasks_and_list_view_unchanged() {
        let repo = crate::git::Scratch::new().unwrap();
        let other = "task-other";
        let mut unrelated = worker(WORKER, "read", "working");
        unrelated["task_id"] = other.into();
        let attempts = vec![native("native-a", TASK, "submitted".into()), unrelated];
        let list = task_view(&repo.0, None, attempts.clone());
        let rows = array(&list["rows"]);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|v| v.get("native_assignment").is_none()));
        assert_eq!(
            crate::digest(&serde_json::to_vec(&list).unwrap()),
            "a3f4fc67a2a12a7e3002612dc60a492e790e898c1aac846f5f225c13090ba6a7"
        );
        for task in [TASK, other] {
            let mut row = task_view(&repo.0, Some(task), attempts.clone())["rows"][0].clone();
            let native = row.as_object_mut().unwrap().remove("native_assignment");
            assert_eq!(native.unwrap().is_null(), task == other);
            let mut expected = rows.iter().find(|v| v["task_id"] == task).unwrap().clone();
            if task == TASK {
                // Only the undecided task's next step follows its submitted input.
                assert_eq!(expected["next_action"], "record main decision");
                expected["next_action"] = "await native assignment result evidence".into();
            }
            assert_eq!(row, expected);
        }
    }
}
