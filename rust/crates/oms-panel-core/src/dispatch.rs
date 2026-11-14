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
            let relative = if p.is_absolute() {
                p.strip_prefix(store.repo())
                    .map_err(|_| Error::new("brief must be inside execution repository"))?
            } else {
                p
            };
            let body = store.read(relative, MAX_PROMPT, false)?;
            require(!body.contains(&0), "brief contains NUL")?;
            crate::outbound(&body)?;
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
    let (native, argv) = command(request, &execution)?;
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
    let preview = serde_json::json!({"schema":1,"kind":"oms-panel-route","route":request.route,"task_id":request.task_id,"command":native.verb(),"argv":argv.iter().map(|v| v.to_string_lossy()).collect::<Vec<_>>(),"overlaps":overlaps,"executes":false});
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
    let output = service.command_context(native, &execution_request, stop)?;
    let tx = store.transaction("panel-dispatch", Duration::from_secs(10))?;
    let mut registry = tx
        .json(
            Path::new(".oms/artifacts/panel-dispatch/reservations.json"),
            4 * 1024 * 1024,
            false,
        )?
        .ok_or_else(|| Error::new("dispatch reservation disappeared"))?;
    let state = if output.cleanup_complete && output.output_complete {
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
