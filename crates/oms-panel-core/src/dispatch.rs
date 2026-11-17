//! Admission-safe dispatch previews, WIP accounting and durable call reservations.
use crate::routing::{Route, WRITERS};
use crate::services::{ExecutionRequest, NativeCommand, query};
use crate::storage::reservation_error;
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use oms_command_telemetry::store::dispatch::MAX_REGISTRY;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
pub const WIP_LIMIT: usize = 10;
pub const MAX_WIP_LIMIT: usize = 24;
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
fn parse_wip_limit(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&v| (1..=MAX_WIP_LIMIT).contains(&v))
        .unwrap_or(WIP_LIMIT)
}
pub fn effective_wip_limit() -> usize {
    parse_wip_limit(std::env::var("OMS_PANEL_WIP_LIMIT").ok().as_deref())
}
pub fn check_wip(running: usize, awaiting: usize, continuing: bool) -> Result<()> {
    check_wip_with_limit(running, awaiting, continuing, effective_wip_limit())
}
pub fn check_wip_with_limit(
    running: usize,
    awaiting: usize,
    continuing: bool,
    limit: usize,
) -> Result<()> {
    require(
        continuing || running.saturating_add(awaiting) < limit,
        &format!(
            "WIP limit {limit} reached: {running} running, {awaiting} awaiting decision; review first"
        ),
    )
}
fn wip_counts(
    request: &DispatchRequest,
    attempts: &[Value],
    calls: &mut serde_json::Map<String, Value>,
) -> (usize, usize) {
    let mut by_member: std::collections::BTreeMap<String, &Value> =
        std::collections::BTreeMap::new();
    for att in attempts {
        let member = text(&att["refs"], "panel_room_participant");
        if !member.is_empty() {
            by_member.insert(member, att);
        }
    }
    for (call_member, entry) in calls.iter_mut() {
        let state = text(entry, "state");
        // Only a completed native acknowledgement enters awaiting. Reserved
        // and running calls may have lost their execution/cleanup response.
        if state != "awaiting" {
            continue;
        }
        let att = by_member.get(call_member.as_str()).copied();
        let terminal =
            att.is_some_and(|a| crate::state::TERMINAL_STATES.contains(&text(a, "state").as_str()));
        let awaiting_non_review_write = state == "awaiting"
            && !att.is_some_and(|a| a["refs"]["panel_access"] == "write" && a["state"] == "review");
        if terminal || awaiting_non_review_write {
            entry["state"] = "settled".into();
        }
    }
    let owner_matches_attempt = |v: &Value| -> bool {
        if let Some(ma) = &request.main_attempt {
            v["parent_attempt_id"] == *ma
        } else if let Some(m) = &request.main {
            v["refs"]["panel_main_participant"] == *m
                || v["refs"]["panel_owner"] == request.route.owner
        } else {
            v["refs"]["panel_owner"] == request.route.owner
        }
    };
    let owner_matches_reservation = |entry: &Value| -> bool {
        if let Some(ma) = &request.main_attempt {
            entry["main_attempt"] == *ma
        } else if let Some(m) = &request.main {
            entry["main"] == *m || entry["route"]["owner"] == request.route.owner
        } else {
            entry["route"]["owner"] == request.route.owner
        }
    };
    let mut running = 0usize;
    let mut awaiting = 0usize;
    let mut seen_members = std::collections::HashSet::new();
    for att in attempts {
        if att["refs"]["panel_access"] != "write" || !owner_matches_attempt(att) {
            continue;
        }
        let state = text(att, "state");
        if state == "review" {
            awaiting += 1;
        } else if crate::state::LIVE_STATES.contains(&state.as_str()) {
            running += 1;
        } else {
            continue;
        }
        let member = text(&att["refs"], "panel_room_participant");
        if !member.is_empty() {
            seen_members.insert(member);
        }
    }
    for (call_member, entry) in calls.iter() {
        if entry["route"]["access"] != "write"
            || !owner_matches_reservation(entry)
            || seen_members.contains(call_member.as_str())
        {
            continue;
        }
        // Unknown spawn/cleanup keeps its slot until native process evidence
        // reconciles it; a task terminal event alone does not prove cleanup.
        if matches!(
            entry["state"].as_str(),
            Some("reserved" | "running" | "unknown")
        ) {
            running += 1;
        }
    }
    (running, awaiting)
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
    if route.account_pinned
        && let Some(account) = &route.account
    {
        argv.extend(["--account".into(), account.clone().into()]);
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
    if let Some(carried) = &request.carried_patch
        && let Some(position) = argv.iter().position(|v| v == "--carried-patch")
    {
        argv[position + 1] = store.repo().join(&carried.patch).into_os_string();
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
    let launch = serde_json::json!({"schema":2,"member":member,"task_id":request.task_id,"main":request.main,"main_attempt":request.main_attempt,
        "room":request.room,"route":request.route,"scopes":scopes,"verify":request.verify,"state":"reserved","execution_repo":execution.repo(),"created_at":crate::now()});
    {
        let tx = store.dispatch_transaction(stop)?;
        let snapshot = tx.snapshot().map_err(reservation_error)?;
        let mut registry = snapshot
            .as_deref()
            .map(|raw| crate::decode(raw, MAX_REGISTRY))
            .transpose()?
            .unwrap_or_else(|| serde_json::json!({"schema":1,"calls":{}}));
        require(
            registry["schema"] == 1 && registry["calls"].is_object(),
            "invalid dispatch reservations",
        )?;
        let calls = registry["calls"].as_object_mut().unwrap();
        let (running, awaiting) = wip_counts(request, array(&attempts), calls);
        if request.route.access == "write" {
            check_wip(running, awaiting, request.continue_task.is_some())?;
        }
        tx.artifact(&member, "launch", &crate::value::encoded(&launch)?)
            .map_err(reservation_error)?;
        registry["calls"][&member] = launch.clone();
        tx.replace(snapshot.as_deref(), &crate::value::encoded(&registry)?)
            .map_err(reservation_error)?;
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
        (
            "OMS_PANEL_EXPLICIT_TARGET".into(),
            if request.route.explicit_target {
                "1"
            } else {
                "0"
            }
            .into(),
        ),
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
            "OMS_PANEL_WORKLOAD".into(),
            request.route.workload.clone().into(),
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
    let output = service.command_context(native, &execution_request, stop).map_err(|e| Error {
        publication_uncertain:true,
        code:e.code,
        ..Error::new(format!("dispatch {member} execution unconfirmed; preserve reservation and inspect before retry: {}",e.message))
    })?;
    // Execution has returned; persist its acknowledgement even after cancellation.
    let settlement_stop = AtomicBool::new(false);
    let settlement = || -> Result<Value> {
        let tx = store.dispatch_transaction(&settlement_stop)?;
        let snapshot = tx
            .snapshot()
            .map_err(reservation_error)?
            .ok_or_else(|| Error::new("dispatch reservation disappeared"))?;
        let mut registry = crate::decode(&snapshot, MAX_REGISTRY)?;
        let entry = registry
            .get("calls")
            .and_then(Value::as_object)
            .and_then(|calls| calls.get(&member));
        require(
            registry["schema"] == 1
                && entry.is_some_and(|entry| {
                    entry.is_object()
                        && matches!(
                            entry["state"].as_str(),
                            Some("reserved" | "running" | "unknown")
                        )
                        && launch
                            .as_object()
                            .unwrap()
                            .iter()
                            .all(|(key, value)| key == "state" || entry.get(key) == Some(value))
                }),
            "dispatch settlement lost its admitted identity; preserve state and reconcile before retry",
        )?;
        let state = if output.cleanup_complete && output.output_complete && !output.limited {
            "awaiting"
        } else {
            "unknown"
        };
        registry["calls"][&member]["state"] = state.into();
        registry["calls"][&member]["exit"] = output.code.into();
        tx.replace(Some(&snapshot), &crate::value::encoded(&registry)?)
            .map_err(reservation_error)?;
        let receipt = serde_json::json!({"schema":1,"member":member,"task_id":request.task_id,"exit":output.code,"cleanup_complete":output.cleanup_complete,"output_complete":output.output_complete,"state":state});
        tx.artifact(&member, "process", &crate::value::encoded(&receipt)?)
            .map_err(reservation_error)?;
        Ok(receipt)
    };
    settlement().map_err(|error| Error {
        publication_uncertain: true,
        ..error
    })
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

#[derive(Debug, Clone, Default)]
pub struct ContinuationDefaults {
    pub attempt_id: String,
    pub member: String,
    pub role: String,
    pub scopes: Vec<String>,
    pub workload: Option<String>,
    pub verify: Option<String>,
    pub purpose: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
}

/// Inherit role, recorded scopes, workload, verifier and purpose from the newest
/// owned write dispatch for this owner lineage and task. Preserves empty recorded
/// scope lists rather than silently widening them to repository-wide `"."`.
#[allow(clippy::too_many_arguments)]
pub fn continuation_defaults(
    repo: &Path,
    owner: &str,
    main: Option<&str>,
    main_attempt: Option<&str>,
    task_id: &str,
    room_id: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<ContinuationDefaults>> {
    crate::identifier(owner)?;
    crate::identifier(task_id)?;
    let store = Store::open(repo)?;
    let attempts = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let attempt_rows = array(&attempts);
    let main_id = main_attempt.map(str::to_owned).or_else(|| {
        attempt_rows
            .iter()
            .filter(|v| {
                (v["tool"] == "panel-main" || v["refs"]["panel_role"] == "main")
                    && !crate::state::TERMINAL_STATES.contains(&text(v, "state").as_str())
                    && v["refs"]["panel_owner"]
                        .as_str()
                        .or_else(|| v["provider"].as_str())
                        == Some(owner)
                    && main.is_none_or(|m| v["refs"]["panel_room_participant"] == m)
                    && room_id.is_none_or(|r| v["refs"]["panel_room_id"] == r)
            })
            .max_by_key(|v| (text(v, "created_at"), text(v, "attempt_id")))
            .and_then(|v| v["attempt_id"].as_str().map(str::to_owned))
    });
    let parents: std::collections::BTreeSet<String> = if let Some(att) = main_id.as_deref() {
        crate::control::lineage(repo, attempt_rows, att, service, stop, false)?
    } else {
        attempt_rows
            .iter()
            .filter(|v| {
                (v["tool"] == "panel-main" || v["refs"]["panel_role"] == "main")
                    && v["refs"]["panel_owner"]
                        .as_str()
                        .or_else(|| v["provider"].as_str())
                        == Some(owner)
            })
            .filter_map(|v| v["attempt_id"].as_str().map(str::to_owned))
            .collect()
    };

    let mut owned = attempt_rows
        .iter()
        .filter(|v| {
            let role = text(&v["refs"], "panel_role");
            let write_like = v["refs"]["panel_access"] == "write"
                || (!role.is_empty() && WRITERS.contains(&role.as_str()));
            v["task_id"] == task_id && write_like && parents.contains(&text(v, "parent_attempt_id"))
        })
        .collect::<Vec<_>>();
    owned.sort_by_key(|v| (text(v, "created_at"), text(v, "attempt_id")));

    let room_participants = if let Some(room) = room_id {
        let state = query(
            service,
            NativeCommand::Room,
            repo,
            &["show", "--id", room, "--json"],
            stop,
        )?;
        array(&state["participants"])
            .iter()
            .filter(|v| !text(v, "participant").is_empty())
            .map(|v| (text(v, "participant"), v.clone()))
            .collect::<std::collections::BTreeMap<_, _>>()
    } else {
        std::collections::BTreeMap::new()
    };

    if let Some(row) = owned.into_iter().next_back() {
        let refs = &row["refs"];
        let member = text(refs, "panel_room_participant");
        let role = text(refs, "panel_role");
        let role = if role.is_empty() {
            "worker".to_owned()
        } else {
            role
        };

        let public_launch = if !member.is_empty() {
            store
                .json(
                    Path::new(&format!(
                        ".oms/artifacts/panel-dispatch/{member}-launch.json"
                    )),
                    65536,
                    true,
                )
                .ok()
                .flatten()
        } else {
            None
        };
        let private_launch = if !member.is_empty() {
            crate::control::private_record(repo, &member, "launch").ok()
        } else {
            None
        };

        let extract_scopes = |v: &Value| -> Option<Vec<String>> {
            v.get("scopes").and_then(|s| s.as_array()).map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
        };

        let scopes = private_launch
            .as_ref()
            .and_then(|v| extract_scopes(v).or_else(|| extract_scopes(&v["rework"])))
            .or_else(|| public_launch.as_ref().and_then(extract_scopes))
            .or_else(|| room_participants.get(&member).and_then(extract_scopes))
            .unwrap_or_default();

        let pick = |key: &str, ref_key: &str| -> Option<String> {
            private_launch
                .as_ref()
                .and_then(|v| {
                    v["route"][key]
                        .as_str()
                        .or_else(|| v[key].as_str())
                        .or_else(|| v["route"]["candidate"][key].as_str())
                })
                .or_else(|| {
                    public_launch.as_ref().and_then(|v| {
                        v["route"][key]
                            .as_str()
                            .or_else(|| v[key].as_str())
                            .or_else(|| v["route"]["candidate"][key].as_str())
                    })
                })
                .or_else(|| refs[ref_key].as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };

        let verify = private_launch
            .as_ref()
            .and_then(|v| v["verify"].as_str())
            .or_else(|| public_launch.as_ref().and_then(|v| v["verify"].as_str()))
            .filter(|v| !v.is_empty())
            .map(str::to_owned);

        // `panel_account` is `provider:id` or a legacy bare `id`.
        let raw = refs["panel_account"].as_str().unwrap_or_default();
        let (account_provider, account) = match raw.split_once(':') {
            Some((p, id)) if !p.is_empty() && !id.is_empty() => {
                (Some(p.to_owned()), Some(id.to_owned()))
            }
            _ => (None, Some(raw.to_owned()).filter(|v| !v.is_empty())),
        };
        let recorded = pick("provider", "panel_provider")
            .or_else(|| row["provider"].as_str().map(str::to_owned));
        // A binding to another provider than the recorded one is inconsistent: drop it.
        let account = account.filter(|_| {
            account_provider.is_none() || recorded.is_none() || account_provider == recorded
        });
        return Ok(Some(ContinuationDefaults {
            attempt_id: text(row, "attempt_id"),
            member,
            role,
            scopes,
            workload: pick("workload", "panel_workload"),
            verify,
            purpose: pick("purpose", "panel_purpose"),
            provider: pick("provider", "panel_provider")
                .or_else(|| row["provider"].as_str().map(str::to_owned))
                .or(account_provider),
            model: pick("model", "panel_model"),
            effort: pick("effort", "panel_effort"),
            account,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::Candidate;
    use serde_json::json;
    use std::time::Duration;

    fn sample_request(main_attempt: &str) -> DispatchRequest {
        DispatchRequest {
            route: Route {
                candidate: Candidate {
                    provider: "claude".into(),
                    model: "claude-sonnet-5-5".into(),
                    effort: "medium".into(),
                    public_only: false,
                    access: None,
                },
                role: "builder".into(),
                kind: "worker".into(),
                owner: "antigravity".into(),
                workload: "routine".into(),
                access: "write".into(),
                purpose: "implement".into(),
                judged_family: None,
                judge_note: None,
                account: None,
                account_reason: None,
                account_pinned: false,
                explicit_target: false,
            },
            task_id: "t1".into(),
            prompt: Some("task".into()),
            brief: None,
            verify: Some("true".into()),
            scopes: vec!["src/".into()],
            room: Some("room-1".into()),
            main: Some("main-a".into()),
            main_attempt: Some(main_attempt.into()),
            continue_task: None,
            base: None,
            carried_patch: None,
            allow_stale_base: false,
            from_task: vec![],
            resume_session: None,
            candidate: None,
            dry_run: true,
            execution_repo: PathBuf::from("."),
            label: None,
            alias: None,
            no_web: false,
        }
    }

    #[test]
    fn wip_limit_defaults_bounds_and_checks() {
        assert_eq!(WIP_LIMIT, 10);
        assert_eq!(parse_wip_limit(None), 10);
        assert_eq!(parse_wip_limit(Some("12")), 12);
        assert_eq!(parse_wip_limit(Some("24")), 24);
        assert_eq!(parse_wip_limit(Some("0")), 10);
        assert_eq!(parse_wip_limit(Some("25")), 10);
        assert_eq!(parse_wip_limit(Some("notanumber")), 10);

        check_wip_with_limit(5, 4, false, WIP_LIMIT).unwrap();
        let err = check_wip_with_limit(6, 4, false, WIP_LIMIT).unwrap_err();
        assert!(
            err.message
                .contains("WIP limit 10 reached: 6 running, 4 awaiting")
        );
        check_wip_with_limit(10, 5, true, WIP_LIMIT).unwrap();
    }

    #[test]
    fn wip_counts_excludes_readonly_settled_and_other_mains_without_review_double_count() {
        let req = sample_request("att_main_1");
        let attempts = vec![
            // Read-only call under our main must be excluded.
            json!({
                "attempt_id": "att_ro",
                "parent_attempt_id": "att_main_1",
                "state": "working",
                "refs": {"panel_access": "read", "panel_room_participant": "call-ro"}
            }),
            // Settled write worker under our main must release WIP.
            json!({
                "attempt_id": "att_done",
                "parent_attempt_id": "att_main_1",
                "state": "done",
                "refs": {"panel_access": "write", "panel_room_participant": "call-done"}
            }),
            // Other main's write worker must not count against us.
            json!({
                "attempt_id": "att_other",
                "parent_attempt_id": "att_main_2",
                "state": "working",
                "refs": {"panel_access": "write", "panel_room_participant": "call-other"}
            }),
            // Live write worker (running).
            json!({
                "attempt_id": "att_live",
                "parent_attempt_id": "att_main_1",
                "state": "working",
                "refs": {"panel_access": "write", "panel_room_participant": "call-live"}
            }),
            // Write worker in review (awaiting only, not running).
            json!({
                "attempt_id": "att_rev",
                "parent_attempt_id": "att_main_1",
                "state": "review",
                "refs": {"panel_access": "write", "panel_room_participant": "call-rev"}
            }),
        ];
        let mut calls_val = json!({
            "call-ro": {"state": "awaiting", "main_attempt": "att_main_1", "route": {"access": "read", "owner": "antigravity"}},
            "call-done": {"state": "awaiting", "main_attempt": "att_main_1", "route": {"access": "write", "owner": "antigravity"}},
            "call-stale-no-att": {"state": "awaiting", "main_attempt": "att_main_1", "route": {"access": "write", "owner": "antigravity"}},
            "call-inflight": {"state": "reserved", "main_attempt": "att_main_1", "route": {"access": "write", "owner": "antigravity"}},
            "call-unconfirmed": {"state": "unknown", "main_attempt": "att_main_1", "route": {"access": "write", "owner": "antigravity"}},
            "call-other-inflight": {"state": "reserved", "main_attempt": "att_main_2", "route": {"access": "write", "owner": "antigravity"}},
            "call-live": {"state": "running", "main_attempt": "att_main_1", "route": {"access": "write", "owner": "antigravity"}},
            "call-rev": {"state": "awaiting", "main_attempt": "att_main_1", "route": {"access": "write", "owner": "antigravity"}}
        });
        let calls = calls_val.as_object_mut().unwrap();
        let (running, awaiting) = wip_counts(&req, &attempts, calls);
        assert_eq!(running, 3); // att_live + call-inflight + unknown launch/cleanup
        assert_eq!(awaiting, 1); // att_rev only
        assert_eq!(calls["call-ro"]["state"], "settled");
        assert_eq!(calls["call-done"]["state"], "settled");
        assert_eq!(calls["call-stale-no-att"]["state"], "settled");
        assert_eq!(calls["call-rev"]["state"], "awaiting");
        assert_eq!(calls["call-inflight"]["state"], "reserved");
        assert_eq!(calls["call-unconfirmed"]["state"], "unknown");
        // A lifecycle terminal alone does not settle an unconfirmed process.
        calls.insert(
            "call-done".into(),
            json!({"state":"unknown", "main_attempt":"att_main_1", "route":{"access":"write"}}),
        );
        assert_eq!(wip_counts(&req, &attempts, calls), (4, 1));
        for state in ["reserved", "running"] {
            calls["call-done"]["state"] = state.into();
            assert_eq!(wip_counts(&req, &attempts, calls), (4, 1));
            assert_eq!(
                calls["call-done"]["state"], state,
                "a terminal attempt cannot settle unacknowledged native execution"
            );
        }
    }

    #[test]
    fn dispatch_settlement_rechecks_the_admitted_identity_and_preserves_foreign_state() {
        use crate::services::{CommandOutput, PeerExecutor};
        use std::cell::{Cell, RefCell};
        use std::os::unix::fs::DirBuilderExt;
        struct Directory(PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        struct Host {
            mode: &'static str,
            calls: Cell<usize>,
            published: RefCell<Vec<u8>>,
        }
        impl PeerExecutor for Host {
            fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                panic!("no process is needed");
            }
        }
        impl Services for Host {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                _: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                assert_eq!(command, NativeCommand::Events);
                Ok(CommandOutput {
                    stdout: b"[]".to_vec(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
            fn command_context(
                &self,
                command: NativeCommand,
                request: &ExecutionRequest,
                interrupted: &AtomicBool,
            ) -> Result<CommandOutput> {
                assert_eq!(command, NativeCommand::Delegate);
                self.calls.set(self.calls.get() + 1);
                let store = Store::open(&request.cwd)?;
                let tx = store.transaction("panel-dispatch", Duration::from_secs(1))?;
                let path = Path::new(".oms/artifacts/panel-dispatch/reservations.json");
                let mut registry = tx.json(path, 4 * 1024 * 1024, false)?.unwrap();
                let member = registry["calls"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .next()
                    .unwrap()
                    .clone();
                match self.mode {
                    "missing" => {
                        registry["calls"].as_object_mut().unwrap().remove(&member);
                    }
                    "null" => registry["calls"] = Value::Null,
                    "array" => registry["calls"] = json!([]),
                    "schema" => registry["schema"] = 2.into(),
                    "route" => registry["calls"][&member]["route"]["owner"] = "other".into(),
                    "metadata" => {
                        registry["opaque"] = json!({"retain":"foreign metadata"});
                        registry["calls"][&member]["extension"] = json!({"retained":true});
                        registry["calls"]["other-call"] =
                            json!({"state":"unknown","route":{"access":"write"}});
                    }
                    "cancel" => interrupted.store(true, std::sync::atomic::Ordering::Release),
                    "error" => {}
                    _ => unreachable!(),
                }
                tx.write_json(path, &registry, false, 4 * 1024 * 1024)?;
                *self.published.borrow_mut() = store.read(path, 4 * 1024 * 1024, false)?;
                if self.mode == "error" {
                    return Err(Error {
                        code: 125,
                        ..Error::new("native acknowledgement unavailable")
                    });
                }
                Ok(CommandOutput {
                    code: if self.mode == "cancel" { 130 } else { 0 },
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }
        for mode in [
            "missing", "null", "array", "schema", "route", "metadata", "cancel", "error",
        ] {
            let root = std::env::temp_dir().join(format!(
                "oms-dispatch-settlement-{}",
                crate::value::unique().unwrap()
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            let root = Directory(std::fs::canonicalize(root).unwrap());
            let mut request = sample_request("att_main_1");
            request.dry_run = false;
            request.execution_repo = root.0.clone();
            request.route.role = "worker".into();
            request.route.access = "read".into();
            request.route.purpose = "explain".into();
            request.verify = None;
            request.scopes.clear();
            request.room = None;
            let host = Host {
                mode,
                calls: Cell::new(0),
                published: RefCell::new(vec![]),
            };
            let result = dispatch(&root.0, &request, &host, &AtomicBool::new(false));
            assert_eq!(host.calls.get(), 1);
            let path = root
                .0
                .join(".oms/artifacts/panel-dispatch/reservations.json");
            if matches!(mode, "metadata" | "cancel") {
                let receipt = result.unwrap();
                let registry: Value =
                    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                assert_eq!(receipt["state"], "awaiting");
                if mode == "cancel" {
                    assert_eq!(receipt["exit"], 130);
                    continue;
                }
                assert_eq!(registry["opaque"]["retain"], "foreign metadata");
                assert_eq!(
                    registry["calls"][receipt["member"].as_str().unwrap()]["extension"]["retained"],
                    true
                );
                assert_eq!(registry["calls"]["other-call"]["state"], "unknown");
            } else {
                let error = result.expect_err(mode);
                if mode == "error" {
                    assert_eq!(error.code, 125);
                }
                assert!(
                    error.publication_uncertain,
                    "{mode}: execution already occurred"
                );
                assert_eq!(std::fs::read(path).unwrap(), *host.published.borrow());
            }
        }
    }

    #[test]
    fn pinned_account_is_forwarded_and_unpinned_is_not() {
        let store = Store::open(Path::new(".")).unwrap();
        let mut req = sample_request("att_main_1");
        req.route.role = "worker".into();
        req.route.access = "read".into();
        req.route.purpose = "explain".into();
        req.verify = None;
        req.brief = None;
        req.route.account = Some("work".into());
        let account = |req: &DispatchRequest| {
            let (_, argv) = command(req, &store).unwrap();
            argv.iter()
                .position(|v| v == "--account")
                .map(|i| argv[i + 1].clone())
        };
        assert!(!req.route.explicit_target);
        assert_eq!(account(&req), None);
        req.route.account_pinned = true;
        assert_eq!(account(&req), Some("work".into()));
        req.route.account = None;
        assert_eq!(account(&req), None);
    }
}
