//! Admission-safe dispatch previews, WIP accounting and durable call reservations.
use crate::routing::{Route, WRITERS};
use crate::services::{DispatchLaunch, DispatchProof, ExecutionRequest, NativeCommand, query};
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
    let slots = occupied_write_slots(
        attempts,
        calls,
        owner_matches_attempt,
        owner_matches_reservation,
    );
    (slots.active + slots.unconfirmed, slots.awaiting)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteSlots {
    pub active: usize,
    pub awaiting: usize,
    pub unconfirmed: usize,
}

pub(crate) fn write_slots_for_main(
    attempts: &[Value],
    calls: &serde_json::Map<String, Value>,
    main: &str,
) -> WriteSlots {
    occupied_write_slots(
        attempts,
        calls,
        |row| row["parent_attempt_id"] == main,
        |entry| entry["main_attempt"] == main,
    )
}

fn occupied_write_slots(
    attempts: &[Value],
    calls: &serde_json::Map<String, Value>,
    owner_matches_attempt: impl Fn(&Value) -> bool,
    owner_matches_reservation: impl Fn(&Value) -> bool,
) -> WriteSlots {
    let mut slots = WriteSlots::default();
    let mut seen_members = std::collections::HashSet::new();
    for att in attempts {
        if att["refs"]["panel_access"] != "write" || !owner_matches_attempt(att) {
            continue;
        }
        let state = text(att, "state");
        if state == "review" {
            slots.awaiting += 1;
        } else if crate::state::LIVE_STATES.contains(&state.as_str()) {
            slots.active += 1;
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
            slots.unconfirmed += 1;
        }
    }
    slots
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
const PROOF_RECORD: usize = 65536;

fn file_generation(meta: &std::fs::Metadata) -> [i128; 5] {
    use std::os::unix::fs::MetadataExt;
    [
        meta.dev().into(),
        meta.ino().into(),
        meta.size().into(),
        i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
    ]
}

fn bound_artifact(store: &Store, name: &Path, expected: &[u8]) -> Result<[i128; 5]> {
    use rustix::fs::{Mode, OFlags, open, openat};
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    crate::storage::path(name)?;
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let read = || -> Result<std::fs::File> {
        let mut dir = open(store.repo(), dir_flags, Mode::empty()).map_err(Error::new)?;
        for component in name.parent().into_iter().flat_map(Path::components) {
            dir = openat(&dir, component.as_os_str(), dir_flags, Mode::empty())
                .map_err(Error::new)?;
        }
        openat(
            &dir,
            name.file_name()
                .ok_or_else(|| Error::new("missing dispatch anchor name"))?,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(Error::new)
    };
    let valid = |m: &std::fs::Metadata| {
        m.is_file()
            && m.uid() == rustix::process::getuid().as_raw()
            && m.nlink() == 1
            && m.mode() & 0o077 == 0
            && m.len() == expected.len() as u64
    };
    let file = read()?;
    let before = file.metadata().map_err(Error::new)?;
    require(valid(&before), "unsafe dispatch anchor")?;
    let mut bytes = Vec::new();
    (&file)
        .take(PROOF_RECORD as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::new)?;
    let after = file.metadata().map_err(Error::new)?;
    let named = read()?.metadata().map_err(Error::new)?;
    let identity = file_generation(&before);
    require(
        bytes == expected
            && valid(&after)
            && valid(&named)
            && file_generation(&after) == identity
            && file_generation(&named) == identity,
        "dispatch anchor changed",
    )?;
    Ok(identity)
}

fn membership(
    launch: &DispatchLaunch,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    let room = query(
        service,
        NativeCommand::Room,
        &launch.repo,
        &["show", "--id", &launch.room, "--exact", "--json"],
        stop,
    )?;
    require(
        room["id"] == launch.room && room["closed"] == false && room["successor"].is_null(),
        "dispatch Room changed",
    )?;
    let joined = array(&room["participants"]);
    let mut mains = joined
        .iter()
        .filter(|v| v["joined"] == true && v["role"] == "main");
    let main = mains
        .next()
        .ok_or_else(|| Error::new("dispatch Main unavailable"))?;
    let mut members = joined.iter().filter(|v| v["participant"] == launch.member);
    let member = members
        .next()
        .ok_or_else(|| Error::new("dispatch member unavailable"))?;
    require(
        mains.next().is_none()
            && main["participant"] == launch.main
            && main["provider"] == launch.main_provider
            && (main["parent"].is_null() || main["parent"] == "")
            && members.next().is_none()
            && member["joined"] == true
            && member["provider"] == launch.provider
            && member["role"] == "worker"
            && member["parent"] == launch.main
            // Native joins can omit the task on their roster row. The
            // immutable reservation and exact worker Events row bind it.
            && (member["task_id"].is_null() || member["task_id"] == launch.task),
        "dispatch membership changed",
    )?;
    Ok(crate::digest(&crate::value::encoded(&serde_json::json!(
        [main, member]
    ))?))
}

fn main_row_matches(row: &Value, launch: &DispatchLaunch) -> bool {
    row["tool"] == "panel-main"
        && row["provider"] == launch.main_provider
        && row["attempt_id"] == launch.main_attempt
        && row["created_at"] == launch.main_created
        && row["terminal"] == false
        && crate::state::LIVE_STATES.contains(&text(row, "state").as_str())
        && row["refs"]["panel_room_id"] == launch.room
        && row["refs"]["panel_role"] == "main"
        && crate::state::main_participant(row) == launch.main
}

fn create_proof_file(root: &oms_safe_fs::Root, name: &Path, body: Vec<u8>) -> Result<()> {
    root.atomic_mutate(
        name,
        oms_safe_fs::MutationOptions {
            read: oms_safe_fs::ByteReadOptions {
                missing_ok: true,
                max_bytes: PROOF_RECORD,
            },
            expected_exists: Some(false),
            max_output: PROOF_RECORD,
            ..Default::default()
        },
        |_| Ok::<_, Error>(body),
    )
    .map_err(|_| Error::new("private dispatch proof write unavailable"))?;
    Ok(())
}

fn check_proof_membership(
    launch: &DispatchLaunch,
    proof: &DispatchProof,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let attempts = query(
        service,
        NativeCommand::Events,
        &launch.repo,
        &["list", "--json"],
        stop,
    )?;
    let mut mains = array(&attempts)
        .iter()
        .filter(|v| v["attempt_id"] == launch.main_attempt);
    let mut workers = array(&attempts)
        .iter()
        .filter(|v| v["refs"]["panel_room_participant"] == launch.member);
    require(
        mains
            .next()
            .is_some_and(|row| main_row_matches(row, launch))
            && mains.next().is_none()
            && workers.next().is_some_and(|row| {
                row["attempt_id"] == proof.attempt
                    && row["parent_attempt_id"] == launch.main_attempt
                    && row["task_id"] == launch.task
                    && row["provider"] == launch.provider
                    && row["tool"] == "peer-delegate"
                    && row["refs"]["panel_room_id"] == launch.room
                    && row["refs"]["panel_role"] == "worker"
                    && row["refs"]["panel_access"] == "write"
                    && row["terminal"] == false
                    && row["state"] == "review"
            })
            && workers.next().is_none()
            && membership(launch, service, stop)? == launch.membership_sha256,
        "dispatch ownership changed",
    )
}

/// All historical files are optional compatibility evidence. The supervisor
/// record commits the proof last so partial publication cannot admit cleanup.
fn publish_stop_proof(
    store: &Store,
    launch: &DispatchLaunch,
    proof: &DispatchProof,
    reservation: &[u8],
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    use rustix::fs::{Mode, OFlags, fsync, mkdirat, open};
    require(
        cfg!(target_os = "linux")
            && proof.launch == *launch
            && proof.provider.pid > 0
            && proof.provider.pid != proof.stage.pid
            && !proof.provider.birth.is_empty()
            && !proof.stage.birth.is_empty()
            && oms_supervisor::process::PhysicalCall::new(&proof.call).is_ok(),
        "dispatch custody unavailable",
    )?;
    crate::checkpoint(stop)?;
    let owner = crate::terminals::process(std::process::id())?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(Error::new)?;
    let boot = boot.trim();
    require(
        [&owner.birth, &proof.stage.birth, &proof.provider.birth]
            .into_iter()
            .all(|birth| birth.strip_prefix(boot).is_some_and(|b| b.starts_with(':')))
            && owner.pid != proof.stage.pid
            && owner.pid != proof.provider.pid,
        "dispatch process generation changed",
    )?;
    check_proof_membership(launch, proof, service, stop)?;

    let modern = format!(
        ".oms/artifacts/panel-dispatch/{}-launch.json",
        launch.member
    );
    let recheck = || -> Result<()> {
        require(
            bound_artifact(store, Path::new(&modern), reservation)?
                == launch.reservation_generation
                && launch.reservation_sha256 == crate::digest(reservation),
            "dispatch reservation generation changed",
        )?;
        crate::checkpoint(stop)
    };
    recheck()?;
    let private_store = crate::terminals::controller_store(store.repo(), true)?;
    let directory = private_store.repo().join(&launch.member);
    let private = oms_safe_fs::Root::open(private_store.repo())
        .map_err(|_| Error::new("private dispatch root unavailable"))?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let parent = open(private_store.repo(), flags, Mode::empty()).map_err(Error::new)?;
    let parent_before = rustix::fs::fstat(&parent).map_err(Error::new)?;
    mkdirat(&parent, launch.member.as_str(), Mode::from_raw_mode(0o700)).map_err(Error::new)?;
    fsync(&parent).map_err(Error::new)?;
    let member_stat = |fd: &rustix::fd::OwnedFd| {
        rustix::fs::statat(
            fd,
            launch.member.as_str(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(Error::new)
    };
    let private_dir = |info: &rustix::fs::Stat| {
        rustix::fs::FileType::from_raw_mode(info.st_mode) == rustix::fs::FileType::Directory
            && info.st_uid == rustix::process::getuid().as_raw()
            && info.st_mode & 0o077 == 0
    };
    let member_before = member_stat(&parent)?;
    require(
        private_dir(&member_before),
        "unsafe private dispatch member",
    )?;

    let controller = serde_json::json!({"pid":owner.pid, "identity":owner.birth});
    let stage = serde_json::json!({"pid":proof.stage.pid, "identity":proof.stage.birth});
    let identity = serde_json::json!({
        "schema":1, "repo":launch.repo, "room":launch.room, "participant":launch.member,
        "parent":launch.main, "task_id":launch.task, "nonce":launch.nonce,
        "boot_id":boot, "controller":controller, "root":directory
    });
    let make_envelope = |record| {
        serde_json::json!({
            "schema":1, "repo": launch.repo, "participant": launch.member, "launch": identity,
            "boot_id":boot, "writer": controller, "supervisor": stage, "record":record
        })
    };
    let call = serde_json::json!({
        "schema":2, "kind":"oms-panel-call", "room":launch.room, "participant":launch.member,
        "parent":launch.main, "task_id":launch.task, "nonce":launch.nonce, "backend":"subreaper"
    });
    let body = crate::control::python_bytes(&call)?;
    let public = format!(".oms/artifacts/panel-{}.json", launch.member);
    let repo_root = oms_safe_fs::Root::open(store.repo())
        .map_err(|_| Error::new("dispatch repository unavailable"))?;
    create_proof_file(&repo_root, Path::new(&public), body.clone())?;
    let generation = bound_artifact(store, Path::new(&public), &body)?;
    let file = |suffix| Path::new(&launch.member).join(format!("{}-{suffix}.json", launch.member));
    create_proof_file(
        &private,
        &Path::new(&launch.member).join("identity.json"),
        crate::value::encoded(&identity)?,
    )?;
    create_proof_file(
        &private,
        &file("launch"),
        crate::value::encoded(&make_envelope(serde_json::json!({
            "call":call,"generation":generation
        })))?,
    )?;
    create_proof_file(
        &private,
        &file("process"),
        crate::value::encoded(&make_envelope(serde_json::json!({
            "pid":proof.provider.pid, "identity":proof.provider.birth, "group":proof.provider.pid
        })))?,
    )?;
    check_proof_membership(launch, proof, service, stop)?;
    recheck()?;
    let named = open(private_store.repo(), flags, Mode::empty()).map_err(Error::new)?;
    let parent_after = rustix::fs::fstat(&named).map_err(Error::new)?;
    let member_after = member_stat(&named)?;
    require(
        bound_artifact(store, Path::new(&public), &body)? == generation
            && (parent_before.st_dev, parent_before.st_ino)
                == (parent_after.st_dev, parent_after.st_ino)
            && (member_before.st_dev, member_before.st_ino)
                == (member_after.st_dev, member_after.st_ino)
            && private_dir(&member_after),
        "dispatch anchor changed before supervisor publication",
    )?;
    crate::checkpoint(stop)?;
    create_proof_file(
        &private,
        &file("supervisor"),
        crate::value::encoded(&make_envelope(serde_json::json!({
            "launch_sha256":crate::digest(&body), "pid":proof.provider.pid,
            "all_reaped":true,"exit":proof.exit
        })))?,
    )
}

// Room validates labels against its 120-character plain-text projection.
fn room_label(label: &str) -> bool {
    use unicode_general_category::{GeneralCategory as G, get_general_category};
    label.chars().count() <= 120
        && !label.starts_with(' ')
        && !label.ends_with(' ')
        && !label.contains("  ")
        && label.chars().all(|c| {
            !matches!(c, '\u{fe0e}' | '\u{fe0f}' | '\u{200d}')
                && (!c.is_whitespace() || c == ' ')
                && !matches!(
                    get_general_category(c),
                    G::LineSeparator
                        | G::ParagraphSeparator
                        | G::Control
                        | G::Format
                        | G::PrivateUse
                        | G::Surrogate
                        | G::Unassigned
                )
                && (get_general_category(c) != G::SpaceSeparator || c == ' ')
        })
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
    resolved.route.freeze_snapshot()?;
    let route_snapshot_sha256 = resolved.route.snapshot_sha256()?;
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
    let preview = serde_json::json!({"schema":1,"kind":"oms-panel-route","route":request.route,"route_snapshot_sha256":route_snapshot_sha256,"task_id":request.task_id,"command":native.verb(),"argv":argv.iter().map(|v| v.to_string_lossy()).collect::<Vec<_>>(),"overlaps":overlaps,"continuation_note":continuation_note,"carried_patch":request.carried_patch,"executes":false});
    if request.dry_run {
        return Ok(preview);
    }
    let room_caller = if let Some(room) = &request.room {
        let caller = request
            .main
            .as_deref()
            .ok_or_else(|| Error::new("room dispatch needs an active main"))?;
        let mains = crate::state::active_mains(array(&attempts), room, &request.route.owner);
        let selected = mains
            .iter()
            .find(|v| {
                (v["refs"]["panel_room_participant"] == caller
                    || (v["refs"].get("panel_room_participant").is_none()
                        && crate::state::main_participant(v) == caller))
                    && request
                        .main_attempt
                        .as_ref()
                        .is_some_and(|a| v["attempt_id"] == *a)
            })
            .ok_or_else(|| Error::new("room caller is not the selected owner main"))?;
        if selected["refs"].get("panel_room_participant").is_none() {
            crate::control::lineage(repo, array(&attempts), caller, service, stop, false)?;
        }
        Some((room, caller))
    } else {
        None
    };
    let member = format!("call-{}", crate::value::unique()?);
    let launch = serde_json::json!({"schema":2,"member":member,"task_id":request.task_id,"main":request.main,"main_attempt":request.main_attempt,
        "room":request.room,"route":request.route,"route_snapshot_sha256":route_snapshot_sha256,"scopes":scopes,"verify":request.verify,"state":"reserved","execution_repo":execution.repo(),"created_at":crate::now()});
    let reservation = crate::value::encoded(&launch)?;
    let reservation_generation = {
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
        tx.artifact(&member, "launch", &reservation)
            .map_err(reservation_error)?;
        registry["calls"][&member] = launch.clone();
        tx.replace(snapshot.as_deref(), &crate::value::encoded(&registry)?)
            .map_err(reservation_error)?;
        (cfg!(target_os = "linux")
            && native == NativeCommand::Delegate
            && request.route.access == "write"
            && matches!(
                request.route.candidate.provider.as_str(),
                "codex" | "claude" | "antigravity"
            ))
        .then(|| {
            let name = format!(".oms/artifacts/panel-dispatch/{member}-launch.json");
            bound_artifact(&store, Path::new(&name), &reservation).ok()
        })
        .flatten()
    };
    if let Some((room, caller)) = room_caller {
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
        if let Some(label) = request.label.as_deref().filter(|label| room_label(label)) {
            join.push(format!("--label={label}").into());
        }
        for scope in &scopes {
            join.extend(["--owns".into(), scope.into()]);
        }
        // Room join rechecks scopes atomically under the canonical room writer lock.
        service
            .command(NativeCommand::Room, repo, &join, stop)?
            .success()?;
    }
    let mut execution_request = ExecutionRequest::new(argv, execution.repo());
    execution_request.environment = oms_command_peer::dispatch_context::DispatchContext {
        task_id: request.task_id.clone(),
        parent_attempt_id: request.main_attempt.clone(),
        room_id: request.room.clone(),
        panel: Some(oms_command_peer::dispatch_context::PanelDispatch {
            role: Some(request.route.kind.clone()),
            strategy: Some(request.route.role.clone()),
            owner: Some(request.route.owner.clone()),
            purpose: Some(request.route.purpose.clone()),
            workload: Some(request.route.workload.clone()),
            access: Some(request.route.access.clone()),
            label: request.label.clone(),
            alias: request.alias.clone(),
            participant: request.room.as_ref().map(|_| member.clone()),
            continues: request
                .carried_patch
                .as_ref()
                .map(|patch| patch.attempt.clone()),
            ..Default::default()
        }),
    }
    .environment();
    execution_request.environment.extend([
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
    ]);
    execution_request.remove_environment = crate::services::args(&[
        "OMS_ATTEMPT_ID",
        "OMS_PANEL_NO_WEB",
        "OMS_RESUME_USAGE_BASELINE",
        "OMS_PANEL_CONTINUES",
        "OMS_PANEL_LABEL",
        "OMS_PANEL_ALIAS",
        "OMS_PANEL_MAIN_PARTICIPANT",
    ]);
    if request.room.is_some() {
        execution_request.environment.push((
            "OMS_ROOM_ADMITTED_PARTICIPANT".into(),
            member.clone().into(),
        ));
    }
    if let Some(main) = &request.main {
        execution_request
            .environment
            .push(("OMS_PANEL_MAIN_PARTICIPANT".into(), main.into()));
    }
    execution_request.environment.push((
        "OMS_PANEL_GRAPH_SCOPES".into(),
        serde_json::to_string(&scopes).map_err(Error::new)?.into(),
    ));
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
    if request.no_web && request.route.role == "researcher" {
        execution_request
            .environment
            .push(("OMS_PANEL_NO_WEB".into(), "1".into()));
    }
    let protected = reservation_generation.and_then(|generation| {
        let (room, main) = room_caller?;
        let main_attempt = request.main_attempt.as_ref()?;
        let mut mains = array(&attempts)
            .iter()
            .filter(|row| row["attempt_id"] == *main_attempt);
        let row = mains.next()?;
        let mut binding = DispatchLaunch {
            nonce: crate::value::unique().ok()?,
            repo: store.repo().to_owned(),
            execution: execution.repo().to_owned(),
            room: room.to_owned(),
            main: main.to_owned(),
            main_attempt: main_attempt.to_owned(),
            main_created: row["created_at"].as_str()?.to_owned(),
            main_provider: request.route.owner.clone(),
            member: member.clone(),
            task: request.task_id.clone(),
            provider: request.route.candidate.provider.clone(),
            membership_sha256: String::new(),
            reservation_sha256: crate::digest(&reservation),
            reservation_generation: generation,
        };
        if mains.next().is_some() || !main_row_matches(row, &binding) {
            return None;
        }
        binding.membership_sha256 = membership(&binding, service, stop).ok()?;
        let path = format!(".oms/artifacts/panel-dispatch/{member}-launch.json");
        (bound_artifact(&store, Path::new(&path), &reservation).ok()? == generation)
            .then_some(binding)
    });
    let (output, proof) = service.dispatch_context(native, &execution_request, protected.as_ref(), stop).map_err(|e| Error {
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
        let receipt = serde_json::json!({"schema":1,"member":member,"task_id":request.task_id,"exit":output.code,"cleanup_complete":output.cleanup_complete,"output_complete":output.output_complete,"state":state,"route_snapshot":request.route.snapshot,"route_snapshot_sha256":route_snapshot_sha256});
        tx.artifact(&member, "process", &crate::value::encoded(&receipt)?)
            .map_err(reservation_error)?;
        if state == "awaiting"
            && output.code == 0
            && let Some((launch, proof)) = protected.as_ref().zip(proof.as_ref())
        {
            let _ = publish_stop_proof(&store, launch, proof, &reservation, service, stop);
        }
        Ok(receipt)
    };
    settlement().map_err(|error| Error {
        publication_uncertain: true,
        ..error
    })
}

#[allow(clippy::too_many_arguments)]
fn continuation_lineage<'a>(
    repo: &Path,
    attempts: &'a [Value],
    owner: &str,
    main: Option<&str>,
    main_attempt: &str,
    room: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<(&'a Value, std::collections::BTreeSet<String>)> {
    let parents = crate::control::lineage(repo, attempts, main_attempt, service, stop, false)?;
    let selected = attempts
        .iter()
        .find(|v| v["attempt_id"] == main_attempt)
        .ok_or_else(|| Error::new("owning main absent"))?;
    if selected["refs"].get("panel_room_participant").is_none() {
        require(
            selected["provider"] == owner
                && room.is_none_or(|r| selected["refs"]["panel_room_id"] == r)
                && main.is_none_or(|m| crate::state::main_participant(selected) == m),
            "native continuation main does not match the dispatch owner, room or participant",
        )?;
    }
    Ok((selected, parents))
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
    let (selected_main, parents) = continuation_lineage(
        repo,
        array(attempts),
        &request.route.owner,
        request.main.as_deref(),
        main,
        request.room.as_deref(),
        service,
        stop,
    )?;
    let proof_attempts = crate::results::attempts_with_main_binding(array(attempts), selected_main);
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
                        && crate::control::stopped(repo, v, &proof_attempts).is_ok())
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
                crate::salvage::verified_empty(repo, last, &proof_attempts, service, stop)?,
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
                    && main.is_none_or(|m| crate::state::main_participant(v) == m)
                    && room_id.is_none_or(|r| v["refs"]["panel_room_id"] == r)
            })
            .max_by_key(|v| (text(v, "created_at"), text(v, "attempt_id")))
            .and_then(|v| v["attempt_id"].as_str().map(str::to_owned))
    });
    let parents: std::collections::BTreeSet<String> = if let Some(att) = main_id.as_deref() {
        continuation_lineage(repo, attempt_rows, owner, main, att, room_id, service, stop)?.1
    } else if main.is_none() && room_id.is_none() {
        attempt_rows
            .iter()
            .filter(|v| {
                (v["tool"] == "panel-main" || v["refs"]["panel_role"] == "main")
                    && v["refs"]["panel_room_participant"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty())
                    && v["refs"]["panel_owner"]
                        .as_str()
                        .or_else(|| v["provider"].as_str())
                        == Some(owner)
            })
            .filter_map(|v| v["attempt_id"].as_str().map(str::to_owned))
            .collect()
    } else {
        return Ok(None);
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
                snapshot: None,
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
            join: RefCell<Vec<OsString>>,
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
                args: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                let stdout = match command {
                    NativeCommand::Events if self.mode.starts_with("label") => serde_json::to_vec(&json!([{
                        "tool":"panel-main", "provider":"antigravity", "terminal":false,
                        "attempt_id":"att_main_1", "refs":{"panel_room_id":"room-1", "panel_room_participant":"main-a"}
                    }])).unwrap(),
                    NativeCommand::Events => b"[]".to_vec(),
                    NativeCommand::Room if args[0] == "show" => b"{\"participants\":[]}".to_vec(),
                    NativeCommand::Room if args[0] == "join" => {
                        *self.join.borrow_mut() = args.to_vec();
                        vec![]
                    }
                    _ => panic!("unexpected command"),
                };
                Ok(CommandOutput {
                    stdout,
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
                if matches!(self.mode, "metadata" | "label") {
                    let environment = |name: &str| {
                        request
                            .environment
                            .iter()
                            .find_map(|(key, value)| (key == name).then(|| value.to_str().unwrap()))
                    };
                    for (name, expected) in [
                        ("OMS_PANEL_DISPATCH", "1"),
                        ("OMS_TASK_ID", "t1"),
                        ("OMS_PANEL_MAIN_ATTEMPT", "att_main_1"),
                        ("OMS_PANEL_ROLE", "worker"),
                        ("OMS_PANEL_STRATEGY", "worker"),
                        ("OMS_PANEL_OWNER", "antigravity"),
                        ("OMS_PANEL_PURPOSE", "explain"),
                        ("OMS_PANEL_WORKLOAD", "routine"),
                        ("OMS_PANEL_ACCESS", "read"),
                        ("OMS_PANEL_MAIN_PARTICIPANT", "main-a"),
                    ] {
                        assert_eq!(environment(name), Some(expected), "{name}");
                    }
                    assert_eq!(environment("OMS_PANEL_ALIAS"), None);
                    assert_eq!(environment("OMS_PANEL_CONTINUES"), None);
                    assert_eq!(environment("OMS_PLAN_LEASE_ID"), None);
                    assert_eq!(
                        environment("OMS_PANEL_LABEL"),
                        (self.mode == "label").then_some("Fix dispatch labels")
                    );
                    assert_eq!(
                        environment("OMS_ROOM_ID"),
                        (self.mode == "label").then_some("room-1")
                    );
                    for name in ["OMS_PANEL_ALIAS", "OMS_PANEL_LABEL", "OMS_PANEL_CONTINUES"] {
                        assert!(request.remove_environment.iter().any(|key| key == name));
                    }
                }
                if self.mode.starts_with("label") {
                    let participant = request
                        .environment
                        .iter()
                        .find(|(key, _)| key == "OMS_ROOM_PARTICIPANT")
                        .unwrap();
                    let admitted = request
                        .environment
                        .iter()
                        .find(|(key, _)| key == "OMS_ROOM_ADMITTED_PARTICIPANT")
                        .unwrap();
                    assert_eq!(participant.1, OsString::from(&member));
                    assert_eq!(admitted.1, OsString::from(&member));
                }
                match self.mode {
                    "missing" => {
                        registry["calls"].as_object_mut().unwrap().remove(&member);
                    }
                    "null" => registry["calls"] = Value::Null,
                    "array" => registry["calls"] = json!([]),
                    "schema" => registry["schema"] = 2.into(),
                    "route" => registry["calls"][&member]["route"]["owner"] = "other".into(),
                    "snapshot" => {
                        registry["calls"][&member]["route"]["snapshot"]["catalog"]["reported_probe"] =
                            "forged".into()
                    }
                    "metadata" => {
                        registry["opaque"] = json!({"retain":"foreign metadata"});
                        registry["calls"][&member]["extension"] = json!({"retained":true});
                        registry["calls"]["other-call"] =
                            json!({"state":"unknown","route":{"access":"write"}});
                    }
                    "cancel" => interrupted.store(true, std::sync::atomic::Ordering::Release),
                    "error" | "policy" | "label" | "label_none" | "label_long"
                    | "label_unicode" | "label_unplain" | "label_dash" | "label_option" => {}
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
            "missing",
            "null",
            "array",
            "schema",
            "route",
            "snapshot",
            "metadata",
            "cancel",
            "error",
            "policy",
            "label",
            "label_none",
            "label_long",
            "label_unicode",
            "label_unplain",
            "label_dash",
            "label_option",
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
            if mode.starts_with("label") {
                request.room = Some("room-1".into());
                request.label = match mode {
                    "label" => Some("Fix dispatch labels".into()),
                    "label_long" => Some("x".repeat(121)),
                    "label_unicode" => Some("한".repeat(120)),
                    "label_unplain" => Some("two  spaces".into()),
                    "label_dash" => Some("-fix auth".into()),
                    "label_option" => Some("--json".into()),
                    _ => None,
                };
            }
            if mode == "policy" {
                request.route = crate::routing::allocate(
                    &crate::routing::Policy::bundled().unwrap(),
                    "antigravity",
                    "worker",
                    None,
                    "read",
                    None,
                    &crate::routing::RoutingInputs {
                        installed: Some(["claude".into()].into()),
                        catalogs: std::collections::BTreeMap::from([(
                            "claude".into(),
                            vec!["claude-sonnet-5-6".into()],
                        )]),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
            let host = Host {
                mode,
                calls: Cell::new(0),
                published: RefCell::new(vec![]),
                join: RefCell::new(vec![]),
            };
            let result = dispatch(&root.0, &request, &host, &AtomicBool::new(false));
            assert_eq!(host.calls.get(), 1, "{mode}: {result:?}");
            let path = root
                .0
                .join(".oms/artifacts/panel-dispatch/reservations.json");
            if mode.starts_with("label") {
                let join = host.join.borrow();
                let value = |flag: &str| {
                    join.windows(2)
                        .find(|pair| pair[0] == flag)
                        .map(|pair| pair[1].to_string_lossy().into_owned())
                };
                let member = result.as_ref().unwrap()["member"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                assert_eq!(value("--participant"), Some(member));
                assert_eq!(value("--task-id"), Some("t1".into()));
                assert_eq!(value("--parent"), Some("main-a".into()));
                assert_eq!(
                    join.iter()
                        .filter_map(|arg| arg.to_str().and_then(|arg| arg.strip_prefix("--label=")))
                        .collect::<Vec<_>>(),
                    match mode {
                        "label" | "label_unicode" | "label_dash" | "label_option" =>
                            vec![request.label.as_deref().unwrap()],
                        _ => vec![],
                    }
                );
                assert!(!join.iter().any(|arg| arg == "--label" || arg == "--json"));
            }
            if matches!(mode, "metadata" | "cancel" | "policy") || mode.starts_with("label") {
                let receipt = result.unwrap();
                let registry: Value =
                    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                assert_eq!(receipt["state"], "awaiting");
                let admitted = &registry["calls"][receipt["member"].as_str().unwrap()];
                assert_eq!(receipt["route_snapshot"], admitted["route"]["snapshot"]);
                assert_eq!(
                    receipt["route_snapshot_sha256"],
                    admitted["route_snapshot_sha256"]
                );
                assert_eq!(receipt["route_snapshot"]["served_model"], Value::Null);
                if mode == "policy" {
                    assert_eq!(receipt["route_snapshot"]["policy_schema"], 2);
                    assert_eq!(receipt["route_snapshot"]["configured"]["model"], "sonnet");
                    assert_eq!(receipt["route_snapshot"]["resolved"]["model"], "sonnet");
                    assert!(receipt["route_snapshot"]["policy_sha256"].is_string());
                } else {
                    assert_eq!(receipt["route_snapshot"]["policy_sha256"], Value::Null);
                }
                assert_eq!(
                    receipt["route_snapshot"]["capabilities"]["effort_support"],
                    "unknown"
                );
                if mode == "cancel" {
                    assert_eq!(receipt["exit"], 130);
                    continue;
                }
                if mode == "policy" || mode.starts_with("label") {
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
        for label in ["bad\nlabel".to_owned(), "x".repeat(481)] {
            let mut request = sample_request("att_main_1");
            request.label = Some(label);
            let host = Host {
                mode: "label",
                calls: Cell::new(0),
                published: RefCell::new(vec![]),
                join: RefCell::new(vec![]),
            };
            assert!(dispatch(Path::new("."), &request, &host, &AtomicBool::new(false)).is_err());
            assert_eq!(host.calls.get(), 0);
            assert!(host.join.borrow().is_empty());
        }
    }

    #[test]
    fn continuation_defaults_require_exact_native_main_binding() {
        use crate::services::{CommandOutput, PeerExecutor};

        struct Host {
            room: Value,
        }
        impl PeerExecutor for Host {
            fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                panic!("continuation defaults must not execute a command");
            }
        }
        impl Services for Host {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                args: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                let value = match command {
                    NativeCommand::Events => {
                        assert_eq!(args, &["list", "--json"]);
                        json!([
                            {
                                "attempt_id": "att-native-main",
                                "tool": "panel-main", "provider": "codex",
                                "state": "working", "terminal": false,
                                "refs": {"panel_role": "main", "panel_room_id": "room-1"}
                            },
                            {
                                "attempt_id": "att-owned-worker", "parent_attempt_id": "att-native-main",
                                "task_id": "native-continuation", "tool": "peer-delegate",
                                "state": "done", "terminal": true,
                                "refs": {
                                    "panel_role": "worker", "panel_access": "write",
                                    "panel_room_participant": "call-native-continuation"
                                }
                            },
                            {
                                "attempt_id": "att-legacy-main",
                                "tool": "panel-main", "provider": "codex",
                                "state": "done", "terminal": true,
                                "refs": {
                                    "panel_role": "main", "panel_room_id": "room-other",
                                    "panel_room_participant": "legacy-main"
                                }
                            },
                            {
                                "attempt_id": "att-legacy-worker", "parent_attempt_id": "att-legacy-main",
                                "task_id": "native-continuation", "state": "done", "terminal": true,
                                "refs": {
                                    "panel_role": "worker", "panel_access": "write",
                                    "panel_room_participant": "call-legacy-continuation"
                                }
                            }
                        ])
                    }
                    NativeCommand::Room => {
                        assert!(
                            args == ["show", "--id", "room-1", "--exact", "--json"]
                                || args == ["show", "--id", "room-1", "--json"]
                        );
                        self.room.clone()
                    }
                    _ => panic!("unexpected continuation service {command:?}"),
                };
                Ok(CommandOutput {
                    stdout: serde_json::to_vec(&value).unwrap(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..CommandOutput::default()
                })
            }
        }

        let mut host = Host {
            room: json!({
                "id": "room-1", "closed": false,
                "participants": [
                    {"participant": "att-native-main", "provider": "codex", "role": "main", "joined": true},
                    {"participant": "call-native-continuation", "role": "worker", "owns": ["src/"]}
                ]
            }),
        };
        let stop = AtomicBool::new(false);
        let read = |host: &Host, owner, main, attempt, room| {
            continuation_defaults(
                Path::new("."),
                owner,
                main,
                attempt,
                "native-continuation",
                room,
                host,
                &stop,
            )
        };
        let exact = Some("att-native-main");
        let room = Some("room-1");
        let defaults = read(&host, "codex", exact, None, room).unwrap().unwrap();
        assert_eq!(defaults.attempt_id, "att-owned-worker");
        assert_eq!(defaults.member, "call-native-continuation");
        assert!(
            read(&host, "codex", Some("other-main"), None, room)
                .unwrap()
                .is_none()
        );
        for (owner, main, room) in [
            ("claude", exact, room),
            ("codex", Some("other-main"), room),
            ("codex", exact, Some("room-other")),
        ] {
            assert!(
                read(&host, owner, main, exact, room)
                    .unwrap_err()
                    .message
                    .contains("native continuation main does not match")
            );
        }
        host.room["participants"][0]["joined"] = false.into();
        assert!(
            read(&host, "codex", exact, None, room)
                .unwrap_err()
                .message
                .contains("native main Room membership is unconfirmed")
        );
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
