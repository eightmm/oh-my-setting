//! Admission-safe dispatch previews, WIP accounting and durable call reservations.
use crate::routing::{Route, WRITERS};
use crate::services::{ExecutionRequest, NativeCommand, query};
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;
pub const WIP_LIMIT: usize = 6;
pub const MAX_PROMPT: usize = 65536;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CarriedPatch {
    pub patch: String,
    pub sha256: String,
    pub base: String,
    pub attempt: String,
}
#[derive(Clone, Debug)]
pub struct DispatchRequest {
    pub route: Route,
    pub task_id: String,
    pub prompt: Option<String>,
    pub brief: Option<PathBuf>,
    pub verify: Option<String>,
    pub scopes: Vec<String>,
    pub room: Option<String>,
    pub main: Option<String>,
    pub main_attempt: Option<String>,
    pub continue_task: Option<String>,
    pub base: Option<String>,
    pub carried_patch: Option<CarriedPatch>,
    pub allow_stale_base: bool,
    pub from_task: Vec<String>,
    pub resume_session: Option<String>,
    pub candidate: Option<crate::review::Candidate>,
    pub dry_run: bool,
    pub execution_repo: PathBuf,
    pub label: Option<String>,
    pub alias: Option<String>,
    pub no_web: bool,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Overlap {
    pub participant: String,
    pub role: String,
    pub task_id: String,
    pub scope: String,
}
pub fn scope_overlaps(state: &Value, caller: &str, scopes: &[String]) -> Result<Vec<Overlap>> {
    require(
        state["participants"].is_array(),
        "room participants unavailable",
    )?;
    let members = array(&state["participants"]);
    let joined: std::collections::BTreeSet<_> = members
        .iter()
        .filter(|v| v["joined"] == true)
        .filter_map(|v| v["participant"].as_str())
        .collect();
    let mut found = vec![];
    for other in members {
        let ident = text(other, "participant");
        let parent = text(other, "parent");
        if !joined.contains(ident.as_str()) || ident == caller {
            continue;
        }
        if !parent.is_empty() {
            let result = &state["call_results"][&ident];
            if !joined.contains(parent.as_str())
                || result["kind"] == "call-result"
                || result["read"] == true
            {
                continue;
            }
        }
        for theirs in array(&other["owns"]) {
            let theirs = theirs
                .as_str()
                .ok_or_else(|| Error::new("malformed scope declaration"))?;
            if scopes
                .iter()
                .any(|mine| oms_command_room::overlaps(mine, theirs))
            {
                found.push(Overlap {
                    participant: ident.clone(),
                    role: text(other, "role"),
                    task_id: other["task_id"].as_str().unwrap_or(&ident).into(),
                    scope: theirs.into(),
                });
            }
        }
    }
    Ok(found)
}
pub fn check_wip(running: usize, awaiting: usize, continuing: bool) -> Result<()> {
    require(
        continuing || running.saturating_add(awaiting) < WIP_LIMIT,
        &format!(
            "WIP limit 6 reached: {running} running, {awaiting} awaiting decision; review first"
        ),
    )
}
pub fn command(request: &DispatchRequest, store: &Store) -> Result<(NativeCommand, Vec<OsString>)> {
    let route = &request.route;
    crate::routing::valid_route(&route.candidate)?;
    require(
        request.prompt.as_ref().is_some_and(|v| !v.is_empty()) != request.brief.is_some(),
        "provide exactly one prompt or scoped brief file",
    )?;
    let brief = request
        .brief
        .as_ref()
        .map(|p| {
            let body = crate::storage::input(store.repo(), p, MAX_PROMPT)?;
            String::from_utf8(body).map_err(Error::new)
        })
        .transpose()?;
    if let Some(prompt) = &request.prompt {
        require(
            prompt.len() <= MAX_PROMPT && !prompt.contains('\0'),
            "prompt exceeds panel input contract",
        )?;
        crate::outbound(prompt.as_bytes())?;
    }
    let verify = request.verify.as_deref().filter(|v| !v.trim().is_empty());
    require(
        route.access != "write"
            || (request.brief.is_some()
                && verify.is_some()
                && WRITERS.contains(&route.role.as_str())),
        "write roles need a scoped brief and verifier",
    )?;
    require(
        request.resume_session.is_none() || WRITERS.contains(&route.role.as_str()),
        "only write-capable roles continue sessions",
    )?;
    if route.role == "researcher" {
        require(
            route.access == "read"
                && route.purpose == "research"
                && route.candidate.effort == "low"
                && crate::routing::route_family(&route.candidate.provider, &route.candidate.model)
                    .is_some_and(|v| ["luna", "haiku"].contains(&v.0.as_str()))
                && verify.is_none()
                && request.resume_session.is_none(),
            "researchers are pinned cheap read-only lookups without gates/continuation",
        )?;
    }
    let mut argv;
    let native;
    if route.role == "reviewer" {
        require(verify.is_some(), "reviewers need a verifier")?;
        native = NativeCommand::Review;
        let target = format!(
            "{}:model={}",
            route.candidate.provider, route.candidate.model
        );
        argv = crate::services::args(&[
            "--providers",
            &target,
            "--gate",
            "--verify",
            verify.unwrap(),
            "--prompt",
            brief
                .as_deref()
                .or(request.prompt.as_deref())
                .unwrap_or_default(),
        ]);
    } else if route.role == "advisor" {
        require(verify.is_none(), "advisors run no verifier")?;
        native = NativeCommand::Advise;
        argv = crate::services::args(&[
            "--to",
            &route.candidate.provider,
            "--strategy",
            "advisor",
            "--no-failures",
            "--no-task",
            "--no-memory",
        ]);
        if let Some(p) = &request.brief {
            argv.extend([
                "--prompt-file".into(),
                store.repo().join(p).into_os_string(),
            ]);
        } else {
            argv.extend([
                "--prompt".into(),
                request.prompt.as_deref().unwrap_or_default().into(),
            ]);
        }
    } else {
        require(
            verify.is_none() || route.access == "write",
            "only reviewers run verify among read roles",
        )?;
        native = NativeCommand::Delegate;
        let profile = if route.role == "worker" {
            if route.purpose == "implement" {
                "implementation-worker"
            } else {
                "analysis-worker"
            }
        } else {
            &route.role
        };
        argv = crate::services::args(&[
            "--to",
            &route.candidate.provider,
            "--role",
            profile,
            "--no-memory",
            "--no-task",
            "--graph-context",
        ]);
        if let Some(p) = &request.brief {
            argv.extend(["--brief-file".into(), store.repo().join(p).into_os_string()]);
        } else {
            argv.extend([
                "--prompt".into(),
                request.prompt.as_deref().unwrap_or_default().into(),
            ]);
        }
        if route.access == "read" {
            argv.push("--read-only".into());
        }
        if route.access == "write" && route.purpose == "investigate" {
            argv.push("--allow-no-change".into());
        }
        if let Some(v) = verify {
            argv.extend(["--verify".into(), v.into()]);
        }
        argv.extend(["--task-id".into(), request.task_id.clone().into()]);
        if let Some(v) = &request.resume_session {
            argv.extend(["--resume-session".into(), v.into()]);
        }
    }
    if route.role != "reviewer" && route.candidate.model != "provider-default" {
        argv.extend(["--model".into(), route.candidate.model.clone().into()]);
    }
    if route.candidate.provider != "opencode" {
        argv.extend([
            "--reasoning-effort".into(),
            route.candidate.effort.clone().into(),
        ]);
    }
    if let Some(candidate) = &request.candidate {
        candidate.validate()?;
        require(
            route.role == "reviewer" && route.access == "read" && verify.is_some(),
            "candidate requires a read reviewer and verifier",
        )?;
        argv.extend(crate::services::args(&[
            "--candidate-patch",
            &candidate.patch,
            "--candidate-sha256",
            &candidate.sha256,
            "--candidate-base",
            &candidate.base,
        ]));
    }
    if native == NativeCommand::Delegate {
        if let Some(base) = &request.base {
            require(
                (7..=64).contains(&base.len()) && crate::hex(base, base.len()),
                "base must be a lowercase Git object identity",
            )?;
            argv.extend(["--base".into(), base.into()]);
        }
        if let Some(carried) = &request.carried_patch {
            require(
                request.route.access == "write",
                "carried patches require write access",
            )?;
            crate::identifier(&carried.attempt)?;
            crate::review::Candidate {
                patch: carried.patch.clone(),
                sha256: carried.sha256.clone(),
                base: carried.base.clone(),
            }
            .validate()?;
            argv.extend(crate::services::args(&[
                "--carried-patch",
                &carried.patch,
                "--carried-patch-sha256",
                &carried.sha256,
                "--carried-patch-base",
                &carried.base,
                "--carried-patch-attempt",
                &carried.attempt,
            ]));
        }
    }
    Ok((native, argv))
}
pub fn dispatch(
    repo: &Path,
    request: &DispatchRequest,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::checkpoint(stop)?;
    if !request.dry_run {
        crate::parent_only()?;
    }
    crate::identifier(&request.task_id)?;
    if let Some(label) = &request.label {
        require(
            !label.trim().is_empty() && label.len() <= 480 && !label.chars().any(char::is_control),
            "label must be a bounded nonblank line",
        )?;
        crate::outbound(label.as_bytes())?;
    }
    for ident in [&request.room, &request.main, &request.main_attempt]
        .into_iter()
        .flatten()
    {
        crate::identifier(ident)?;
    }
    if let Some(alias) = &request.alias {
        require(
            !alias.is_empty()
                && alias.len() <= 8
                && alias
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || v == b'-'),
            "alias must be 1-8 letters, digits or hyphens",
        )?;
    }
    let store = Store::open(repo)?;
    let execution = Store::open(&request.execution_repo)?;
    let mut scopes = request
        .scopes
        .iter()
        .map(|v| oms_contract_markdown::scope::normalize(v).map_err(Error::new))
        .collect::<Result<Vec<_>>>()?;
    require(
        scopes.is_empty() || request.route.access == "write",
        "scope requires a write role",
    )?;
    scopes.sort();
    scopes.dedup();
    if request.route.access == "write" && scopes.is_empty() {
        scopes.push(".".into());
    }
    if request.continue_task.is_some() {
        require(
            WRITERS.contains(&request.route.role.as_str())
                && request.brief.is_some()
                && request.prompt.is_none(),
            "continue takes a write-capable role and brief file",
        )?;
        require(
            request.continue_task.as_deref() == Some(&request.task_id),
            "continue must retain task ID",
        )?;
    }
    for task in &request.from_task {
        crate::identifier(task)?;
    }
    require(
        request.from_task.is_empty() || request.brief.is_some(),
        "from-task requires a brief file",
    )?;
    let attempts = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    require(attempts.is_array(), "lifecycle projection unavailable")?;
    let mut resolved = request.clone();
    let continuation_note = if request.continue_task.is_some() {
        continuation(repo, &mut resolved, &attempts, service, stop)?
    } else {
        require(
            request.carried_patch.is_none(),
            "carried patches require a recorded continuation",
        )?;
        String::new()
    };
    let request = &resolved;
    let (native, mut argv) = command(request, &execution)?;
    let scratch = if request.dry_run {
        None
    } else {
        Some(crate::git::Scratch::new()?)
    };
    if let Some(brief) = &request.brief {
        let mut body = crate::storage::input(execution.repo(), brief, MAX_PROMPT)?;
        if !request.from_task.is_empty() {
            let brief_text = std::str::from_utf8(&body).map_err(Error::new)?;
            let findings = crate::results::prior_findings(
                repo,
                &request.from_task,
                brief_text,
                service,
                stop,
            )?;
            body.extend(findings.as_bytes());
        }
        require(
            body.len() <= MAX_PROMPT,
            "brief plus prior findings exceeds panel input contract",
        )?;
        if let Some(scratch) = &scratch {
            let captured = scratch.0.join("brief.md");
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&captured)
                .map_err(Error::new)?;
            file.write_all(&body).map_err(Error::new)?;
            if let Some(position) = argv
                .iter()
                .position(|v| v == "--brief-file" || v == "--prompt-file")
            {
                argv[position + 1] = captured.into_os_string();
            } else if let Some(position) = argv.iter().position(|v| v == "--prompt") {
                argv[position + 1] = String::from_utf8(body).map_err(Error::new)?.into();
            }
        }
    }
    if let Some(carried) = &request.carried_patch {
        if let Some(position) = argv.iter().position(|v| v == "--carried-patch") {
            argv[position + 1] = store.repo().join(&carried.patch).into_os_string();
        }
    }
    if crate::routing::INDEPENDENT.contains(&request.route.role.as_str()) {
        for task in request
            .from_task
            .iter()
            .chain(std::iter::once(&request.task_id))
        {
            for writer in array(&attempts)
                .iter()
                .filter(|v| v["task_id"] == *task && v["refs"]["panel_access"] == "write")
            {
                let provider = writer["refs"]["panel_provider"]
                    .as_str()
                    .or_else(|| writer["provider"].as_str())
                    .unwrap_or_default();
                let family = service.model_family(
                    provider,
                    writer["refs"]["panel_model"].as_str().unwrap_or_default(),
                )?;
                let judge = service.model_family(
                    &request.route.candidate.provider,
                    &request.route.candidate.model,
                )?;
                require(
                    family != judge
                        || request.route.judge_note.as_deref()
                            == Some("same-family judge (owner-pinned)"),
                    "from-task requires independent model-family judgment",
                )?;
            }
        }
    }
    let room = if let Some(room) = &request.room {
        crate::identifier(room)?;
        query(
            service,
            NativeCommand::Room,
            repo,
            &["show", "--id", room, "--json"],
            stop,
        )?
    } else {
        serde_json::json!({})
    };
    let overlaps = if request.room.is_some() && request.route.access == "write" {
        scope_overlaps(&room, request.main.as_deref().unwrap_or_default(), &scopes)?
    } else {
        vec![]
    };
    require(
        overlaps.is_empty(),
        "Scope overlap: write dispatch refused; wait for delivery/read or release declarations",
    )?;
    let preview = serde_json::json!({"schema":1,"kind":"oms-panel-route","route":request.route,"task_id":request.task_id,"command":native.verb(),"argv":argv.iter().map(|v| v.to_string_lossy()).collect::<Vec<_>>(),"overlaps":overlaps,"continuation_note":continuation_note,"carried_patch":request.carried_patch,"executes":false});
    if request.dry_run {
        return Ok(preview);
    }
    let member = format!("call-{}", crate::value::unique()?);
    let relative = format!(".oms/artifacts/panel-dispatch/{member}-launch.json");
    {
        let tx = store.transaction("panel-dispatch", Duration::from_secs(10))?;
        let mut registry = tx
            .json(
                Path::new(".oms/artifacts/panel-dispatch/reservations.json"),
                4 * 1024 * 1024,
                true,
            )?
            .unwrap_or_else(|| serde_json::json!({"schema":1,"calls":{}}));
        require(
            registry["schema"] == 1 && registry["calls"].is_object(),
            "invalid dispatch reservations",
        )?;
        let calls = registry["calls"].as_object().unwrap();
        let running = calls
            .values()
            .filter(|v| v["state"] == "reserved" || v["state"] == "running")
            .count();
        let awaiting = calls.values().filter(|v| v["state"] == "awaiting").count();
        if request.route.access == "write" {
            check_wip(running, awaiting, request.continue_task.is_some())?;
        }
        let launch = serde_json::json!({"schema":2,"member":member,"task_id":request.task_id,"main":request.main,"main_attempt":request.main_attempt,
            "room":request.room,"route":request.route,"scopes":scopes,"state":"reserved","execution_repo":execution.repo(),"created_at":crate::now()});
        tx.write_json(Path::new(&relative), &launch, true, 65536)?;
        registry["calls"][&member] = launch;
        tx.write_json(
            Path::new(".oms/artifacts/panel-dispatch/reservations.json"),
            &registry,
            false,
            4 * 1024 * 1024,
        )?;
    }
    if let Some(room) = &request.room {
        let caller = request
            .main
            .as_deref()
            .ok_or_else(|| Error::new("room dispatch needs an active main"))?;
        let mains = crate::state::active_mains(array(&attempts), room, &request.route.owner);
        require(
            mains.iter().any(|v| {
                v["refs"]["panel_room_participant"] == caller
                    && request
                        .main_attempt
                        .as_ref()
                        .is_some_and(|a| v["attempt_id"] == *a)
            }),
            "room caller is not the selected owner main",
        )?;
        let mut join = crate::services::args(&[
            "join",
            "--id",
            room,
            "--participant",
            &member,
            "--provider",
            &request.route.candidate.provider,
            "--role",
            &request.route.kind,
            "--parent",
            caller,
            "--task-id",
            &request.task_id,
        ]);
        for scope in &scopes {
            join.extend(["--owns".into(), scope.into()]);
        }
        // Room join rechecks scopes atomically under the canonical room writer lock.
        service
            .command(NativeCommand::Room, repo, &join, stop)?
            .success()?;
    }
    let mut execution_request = ExecutionRequest::new(argv, execution.repo());
    execution_request.environment = vec![
        ("OMS_PANEL_DISPATCH".into(), "1".into()),
        ("OMS_PANEL_ROLE".into(), request.route.kind.clone().into()),
        (
            "OMS_PANEL_STRATEGY".into(),
            request.route.role.clone().into(),
        ),
        ("OMS_TASK_ID".into(), request.task_id.clone().into()),
        ("OMS_PANEL_REPO".into(), store.repo().into()),
        ("OMS_ROOM_REPO".into(), store.repo().into()),
    ];
    execution_request.remove_environment = crate::services::args(&[
        "OMS_ATTEMPT_ID",
        "OMS_PANEL_NO_WEB",
        "OMS_RESUME_USAGE_BASELINE",
        "OMS_PANEL_CONTINUES",
    ]);
    if let Some(room) = &request.room {
        execution_request.environment.extend([
            ("OMS_ROOM_ID".into(), room.into()),
            ("OMS_ROOM_PARTICIPANT".into(), member.clone().into()),
            (
                "OMS_ROOM_ADMITTED_PARTICIPANT".into(),
                member.clone().into(),
            ),
        ]);
    }
    for (name, value) in [
        ("OMS_PANEL_LABEL", request.label.as_deref()),
        ("OMS_PANEL_ALIAS", request.alias.as_deref()),
        ("OMS_PANEL_MAIN_PARTICIPANT", request.main.as_deref()),
        (
            "OMS_PANEL_CONTINUES",
            request.carried_patch.as_ref().map(|v| v.attempt.as_str()),
        ),
    ] {
        execution_request.remove_environment.push(name.into());
        if let Some(value) = value {
            execution_request
                .environment
                .push((name.into(), value.into()));
        }
    }
    execution_request.environment.extend([
        (
            "OMS_PANEL_ACCESS".into(),
            request.route.access.clone().into(),
        ),
        ("OMS_PANEL_OWNER".into(), request.route.owner.clone().into()),
        (
            "OMS_PANEL_PURPOSE".into(),
            request.route.purpose.clone().into(),
        ),
        (
            "OMS_PANEL_GRAPH_SCOPES".into(),
            serde_json::to_string(&scopes).map_err(Error::new)?.into(),
        ),
    ]);
    execution_request
        .remove_environment
        .push("OMS_PANEL_CONTINUATION".into());
    if let Some(carried) = &request.carried_patch {
        let context =
            serde_json::json!({"patch":carried.patch,"sha256":carried.sha256,"base":carried.base});
        execution_request.environment.push((
            "OMS_PANEL_CONTINUATION".into(),
            serde_json::to_string(&context).map_err(Error::new)?.into(),
        ));
    }
    if request.continue_task.is_some() {
        let parents = crate::control::lineage(
            repo,
            array(&attempts),
            request.main_attempt.as_deref().unwrap(),
            service,
            stop,
            false,
        )?;
        let previous = array(&attempts)
            .iter()
            .filter(|v| {
                v["task_id"] == request.task_id
                    && v["refs"]["panel_role"] == "worker"
                    && parents.contains(&text(v, "parent_attempt_id"))
            })
            .max_by_key(|v| (text(v, "created_at"), text(v, "attempt_id")))
            .ok_or_else(|| Error::new("continuation predecessor unavailable"))?;
        execution_request
            .environment
            .retain(|(name, _)| name != "OMS_PANEL_CONTINUES");
        execution_request.environment.push((
            "OMS_PANEL_CONTINUES".into(),
            text(previous, "attempt_id").into(),
        ));
    }
    if let Some(main) = &request.main_attempt {
        execution_request
            .environment
            .push(("OMS_PANEL_MAIN_ATTEMPT".into(), main.into()));
    }
    if request.no_web && request.route.role == "researcher" {
        execution_request
            .environment
            .push(("OMS_PANEL_NO_WEB".into(), "1".into()));
    }
    if let Some(account) = &request.route.account {
        let config = crate::routing::Accounts::configured()?
            .ok_or_else(|| Error::new("selected account configuration disappeared"))?;
        execution_request
            .environment
            .push(("CODEX_HOME".into(), config.home(account)?.into()));
    }
    let output = service.command_context(native, &execution_request, stop).map_err(|e| Error {
        publication_uncertain:true,
        ..Error::new(format!("dispatch {member} execution unconfirmed; preserve reservation and inspect before retry: {}",e.message))
    })?;
    let tx = store.transaction("panel-dispatch", Duration::from_secs(10))?;
    let mut registry = tx
        .json(
            Path::new(".oms/artifacts/panel-dispatch/reservations.json"),
            4 * 1024 * 1024,
            false,
        )?
        .ok_or_else(|| Error::new("dispatch reservation disappeared"))?;
    let state = if output.cleanup_complete && output.output_complete && !output.limited {
        "awaiting"
    } else {
        "unknown"
    };
    registry["calls"][&member]["state"] = state.into();
    registry["calls"][&member]["exit"] = output.code.into();
    tx.write_json(
        Path::new(".oms/artifacts/panel-dispatch/reservations.json"),
        &registry,
        false,
        4 * 1024 * 1024,
    )?;
    let receipt = serde_json::json!({"schema":1,"member":member,"task_id":request.task_id,"exit":output.code,"cleanup_complete":output.cleanup_complete,"output_complete":output.output_complete,"state":state});
    tx.write_json(
        Path::new(&format!(
            ".oms/artifacts/panel-dispatch/{member}-process.json"
        )),
        &receipt,
        true,
        65536,
    )?;
    Ok(receipt)
}

/// Resolve only the newest terminal worker owned by this main. Unpaired salvage
/// or an unverified outcome refuses rather than silently discarding prior work.
pub fn continuation(
    repo: &Path,
    request: &mut DispatchRequest,
    attempts: &Value,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    let main = request
        .main_attempt
        .as_deref()
        .ok_or_else(|| Error::new("continue requires the owning main attempt"))?;
    let parents = crate::control::lineage(repo, array(attempts), main, service, stop, false)?;
    let mut earlier = array(attempts)
        .iter()
        .filter(|v| {
            v["task_id"] == request.task_id
                && parents.contains(&text(v, "parent_attempt_id"))
                && v["refs"]["panel_role"] == "worker"
        })
        .collect::<Vec<_>>();
    earlier.sort_by_key(|v| (text(v, "created_at"), text(v, "attempt_id")));
    require(
        !earlier.is_empty()
            && earlier.iter().all(|v| {
                v["terminal"] == true
                    || (v["state"] == "review"
                        && crate::control::stopped(repo, v, array(attempts)).is_ok())
            }),
        "prior worker absent or termination unconfirmed",
    )?;
    let last = earlier[earlier.len() - 1];
    let attempt = text(last, "attempt_id");
    crate::identifier(&attempt)?;
    let rows = crate::results::index_rows(repo)?;
    let latest = rows.iter().rev().find(|v| {
        v["task_id"] == request.task_id && v["attempt_id"] == attempt && v["kind"] == "delegate"
    });
    let eligible = latest.is_some_and(|v| {
        v.get("guard")
            .is_none_or(|g| g.is_null() || g == false || g == "")
            && v["reason_code"] != "worker_authority_violation"
            && (v["exit"] == 0
                || (last["reason_code"] == "verification_failed"
                    && v["verify_exit"]
                        .as_i64()
                        .is_some_and(|n| (1..124).contains(&n))))
    });
    let store = Store::open(repo)?;
    let selected = if last["state"] != "done" && !eligible {
        if let Some(meta) = crate::salvage::continuation_patch(&store, &request.task_id, &attempt)?
        {
            Some(meta)
        } else {
            require(
                crate::salvage::verified_empty(repo, last, array(attempts), service, stop)?,
                "stopped write work is not transferred by --continue; salvage and inspect the stopped attempt first",
            )?;
            None
        }
    } else {
        require(
            eligible,
            "previous delegate outcome cannot be verified; continuation refused",
        )?;
        latest.cloned()
    };
    let mut note = String::new();
    let mut carried = None;
    if let Some(row) = selected {
        let body = crate::results::verified_patch(&store, &row)?;
        if !body.is_empty() {
            crate::outbound(&body)?;
            let base = text(&row, "base_sha");
            require(
                (7..=64).contains(&base.len()) && crate::hex(&base, base.len()),
                "previous patch has no recorded base; continuation refused",
            )?;
            if crate::git::applies(service, &request.execution_repo, &body, true, stop)? {
                note = "previous patch is already part of HEAD; continue from it".into();
            } else {
                let head = crate::git::text(
                    service,
                    &request.execution_repo,
                    &["rev-parse", "--verify", "HEAD^{commit}"],
                    stop,
                )?;
                let resolved = crate::git::run(
                    service,
                    &request.execution_repo,
                    &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
                    None,
                    None,
                    stop,
                )?;
                require(
                    resolved.cleanup_complete && resolved.output_complete && !resolved.limited,
                    "continuation base resolution incomplete",
                )?;
                if resolved.code == 0 {
                    let base_full = String::from_utf8(resolved.stdout).map_err(Error::new)?;
                    if base_full.trim() != head {
                        let clean = crate::git::applies(
                            service,
                            &request.execution_repo,
                            &body,
                            false,
                            stop,
                        )?;
                        require(
                            request.allow_stale_base || clean,
                            "continued patch base differs from HEAD and does not apply cleanly; start a fresh dispatch with --from-task or explicitly use --allow-stale-base",
                        )?;
                        note = format!(
                            "continued patch base {base} differs from HEAD {head}; {}",
                            if request.allow_stale_base {
                                "--allow-stale-base was given"
                            } else {
                                "carried patch applies cleanly"
                            }
                        );
                    }
                } else {
                    note = format!(
                        "recorded continuation base {base} is not in this checkout; drift was not checked"
                    );
                }
                carried = Some(CarriedPatch {
                    patch: text(&row, "patch"),
                    sha256: text(&row, "patch_sha256"),
                    base,
                    attempt: attempt.clone(),
                });
            }
        }
    }
    require(
        request
            .carried_patch
            .as_ref()
            .is_none_or(|v| Some(v) == carried.as_ref()),
        "explicit carried patch differs from recorded continuation",
    )?;
    request.carried_patch = carried;
    // Native session storage and account provenance are independent authority;
    // without an injected proof, a fresh worker receives the verified patch.
    require(
        request.resume_session.is_none(),
        "native resume requires independently verified session/account provenance",
    )?;
    Ok(note)
}
