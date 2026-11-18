//! Reviewed local acceptance. No push, plan finish or publication is implied.
use crate::results::{AcceptRequest, MAX_RESULT, index_rows, verified_patch};
use crate::services::{ExecutionRequest, NativeCommand, args, query};
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn preflight<S: Services + ?Sized>(
    repo: &Path,
    request: &AcceptRequest,
    service: &S,
    stop: &AtomicBool,
    applied: bool,
) -> Result<Value> {
    let store = Store::open(repo)?;
    let attempts = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    require(attempts.is_array(), "accept lifecycle evidence unavailable")?;
    let main = array(&attempts)
        .iter()
        .find(|v| v["attempt_id"] == request.main_attempt_id)
        .ok_or_else(|| Error::new("owning main absent"))?;
    require(
        main["tool"] == "panel-main"
            && main["provider"] == request.owner
            && main["terminal"] == false,
        "accept requires the current owning main",
    )?;
    let parents = crate::control::swap_parents(
        repo,
        array(&attempts),
        &request.main_attempt_id,
        service,
        stop,
    )?;
    let participant = text(&main["refs"], "panel_room_participant");
    let room_id = text(&main["refs"], "panel_room_id");
    crate::identifier(&participant)?;
    crate::identifier(&room_id)?;
    let room = query(
        service,
        NativeCommand::Room,
        repo,
        &["show", "--id", &room_id, "--json"],
        stop,
    )?;
    require(
        room["closed"] == false
            && array(&room["participants"]).iter().any(|v| {
                v["participant"] == participant && v["joined"] == true && v["role"] == "main"
            }),
        "accept main is not joined to an open room",
    )?;
    let plan = store
        .json(Path::new(".oms/plan/tasks.json"), 4 * MAX_RESULT, false)?
        .ok_or_else(|| Error::new("plan task unavailable"))?;
    let mut task = plan["tasks"][&request.task_id].clone();
    require(task.is_object(), "plan task unavailable")?;
    task["plan_id"] = plan["plan_id"].clone();
    let lease = text(&task, "lease_id");
    let plan_id = text(&task, "plan_id");
    require(
        task["id"] == request.task_id
            && task["claimed_by_participant"] == participant
            && array(&attempts).iter().any(|v| {
                parents.contains(&text(v, "attempt_id")) && v["provider"] == task["provider"]
            })
            && lease
                .strip_prefix("lease_")
                .is_some_and(|v| crate::hex(v, 32))
            && plan_id
                .strip_prefix("plan_")
                .is_some_and(|v| crate::hex(v, 32)),
        "plan task owner/lease does not match the current main",
    )?;
    require(
        ["claimed", "running"].contains(&text(&task, "state").as_str())
            || (applied && ["review", "done"].contains(&text(&task, "state").as_str())),
        "accept requires a claimed/running task; reconcile prior acceptance before retry",
    )?;
    require(
        !request.verify.is_empty()
            && request.verify.replace('\r', "") == text(&task, "verify").replace('\r', ""),
        "--verify does not match the plan task verify contract",
    )?;
    let related = array(&attempts)
        .iter()
        .filter(|v| v["task_id"] == request.task_id && v["tool"] != "panel-main")
        .collect::<Vec<_>>();
    require(
        !related.is_empty()
            && related.iter().all(|v| {
                v["terminal"] == true
                    || (v["state"] == "review"
                        && crate::control::stopped(repo, v, array(&attempts)).is_ok())
            }),
        "task has live or unconfirmed attempts",
    )?;
    let rows = index_rows(repo)?;
    let selection = crate::control::select(
        repo,
        related
            .iter()
            .filter(|v| v["refs"]["panel_role"] == "worker" && v["refs"]["panel_access"] == "write")
            .map(|v| (*v).clone())
            .collect(),
        array(&attempts),
        &rows,
        Some(&request.main_attempt_id),
        service,
        stop,
    )?;
    let workers = selection.workers;
    require(
        workers.len() == 1,
        "accept requires one unique worker candidate for this task",
    )?;
    let worker = &workers[0];
    require(
        parents.contains(&text(worker, "parent_attempt_id"))
            && worker["refs"]["panel_lease_id"] == lease
            && worker["refs"]["panel_purpose"] != "investigate",
        "candidate worker is not bound to this main/lease or is report-only",
    )?;
    let candidates = rows
        .iter()
        .filter(|v| {
            v["kind"] == "delegate"
                && v["task_id"] == request.task_id
                && v["attempt_id"] == worker["attempt_id"]
                && v["patch_sha256"].is_string()
        })
        .collect::<Vec<_>>();
    require(candidates.len() == 1, "candidate is ambiguous")?;
    let candidate = candidates[0];
    require(
        candidate["exit"] == 0
            && candidate["plan_id"] == plan_id
            && request
                .expected_patch_sha256
                .as_ref()
                .is_none_or(|v| candidate["patch_sha256"] == *v),
        "candidate does not match current plan/digest",
    )?;
    if !applied {
        require(
            !rows.iter().any(|v| {
                ["patch-land", "panel-accept"].contains(&text(v, "kind").as_str())
                    && v["task_id"] == request.task_id
                    && v["patch_sha256"] == candidate["patch_sha256"]
                    && v["exit"] == 0
            }),
            "candidate already applied; never reapply blindly",
        )?;
    }
    let body = verified_patch(&store, candidate)?;
    require(!body.is_empty(), "empty candidate cannot be accepted")?;
    let artifact = text(candidate, "artifact");
    require(
        artifact.starts_with(".oms/artifacts/"),
        "candidate artifact outside evidence store",
    )?;
    let evidence = store.read(Path::new(&artifact), MAX_RESULT, false)?;
    require(
        candidate["artifact_sha256"]
            .as_str()
            .is_some_and(|v| crate::hex(v, 64) && crate::digest(&evidence) == v),
        "candidate artifact digest differs",
    )?;
    let review = rows
        .iter()
        .rev()
        .find(|v| {
            v["kind"] == "review-outcome"
                && v["task_id"] == request.task_id
                && v["review"]["candidate"]["patch_sha256"] == candidate["patch_sha256"]
        })
        .ok_or_else(|| Error::new("candidate lacks a passing review gate"))?;
    let seats = array(&review["review"]["seats"]);
    require(
        review["exit"] == 0
            && review["review"]["gate_verify_exit"] == 0
            && !seats.is_empty()
            && seats.iter().all(|v| v["verdict"] == "pass"),
        "candidate lacks a passing review gate",
    )?;
    let worker_room_id = text(&worker["refs"], "panel_room_id");
    let worker_room = if worker_room_id == room_id {
        room
    } else {
        crate::identifier(&worker_room_id)?;
        query(
            service,
            NativeCommand::Room,
            repo,
            &["show", "--id", &worker_room_id, "--json"],
            stop,
        )?
    };
    let member = text(&worker["refs"], "panel_room_participant");
    let declaration = array(&worker_room["participants"])
        .iter()
        .find(|v| v["participant"] == member)
        .ok_or_else(|| Error::new("worker scope declaration unavailable"))?;
    let scopes = array(&declaration["owns"])
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new("malformed worker scope"))
        })
        .collect::<Result<Vec<_>>>()?;
    require(!scopes.is_empty(), "worker scope declaration is empty")?;
    let allowed = crate::control::effective_paths(repo, &task, array(&attempts))?;
    let forbidden = array(&task["forbidden_paths"])
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new("malformed forbidden paths"))
        })
        .collect::<Result<Vec<_>>>()?;
    for path in crate::files::patch_paths(&body)? {
        require(
            scopes
                .iter()
                .any(|scope| oms_command_room::overlaps(scope, &path))
                && !allowed.is_empty()
                && allowed
                    .iter()
                    .any(|scope| oms_command_room::overlaps(scope, &path))
                && !forbidden
                    .iter()
                    .any(|scope| oms_command_room::overlaps(scope, &path)),
            "candidate patch leaves its declared/plan scope",
        )?;
    }
    let gate = serde_json::json!({"event_id":review["event_id"],"patch_sha256":candidate["patch_sha256"],"exit":review["exit"],"gate_verify_exit":review["review"]["gate_verify_exit"],"seat_verdicts":seats.iter().map(|v| &v["verdict"]).collect::<Vec<_>>()});
    Ok(
        serde_json::json!({"task":task,"candidate":candidate,"worker":{"attempt_id":worker["attempt_id"],"task_id":worker["task_id"],"parent_attempt_id":worker["parent_attempt_id"],"refs":worker["refs"]},"review_gate":gate,"allowed_paths":allowed,"superseded_attempts":selection.superseded}),
    )
}
fn recheck<S: Services + ?Sized>(
    repo: &Path,
    request: &AcceptRequest,
    baseline: &Value,
    service: &S,
    stop: &AtomicBool,
) -> Result<()> {
    require(
        preflight(repo, request, service, stop, true)? == *baseline,
        "candidate, digest, lease, task or review gate changed",
    )
}
struct IndexLock(PathBuf);
impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
#[allow(clippy::too_many_arguments, clippy::unnecessary_cast)]
fn exact_commit<S: Services + ?Sized>(
    repo: &Path,
    execution: &Path,
    request: &AcceptRequest,
    baseline: &Value,
    base: &str,
    tree: &str,
    service: &S,
    stop: &AtomicBool,
) -> Result<String> {
    let symbolic = crate::git::run(
        service,
        execution,
        &["symbolic-ref", "-q", "HEAD"],
        None,
        None,
        stop,
    )?;
    require(
        matches!(symbolic.code, 0 | 1)
            && symbolic.cleanup_complete
            && symbolic.output_complete
            && !symbolic.limited,
        "HEAD ref unavailable",
    )?;
    let branch = if symbolic.code == 1 {
        "HEAD".into()
    } else {
        String::from_utf8(symbolic.stdout)
            .map_err(Error::new)?
            .trim()
            .to_owned()
    };
    let raw = crate::git::text(
        service,
        execution,
        &["rev-parse", "--git-path", "index"],
        stop,
    )?;
    require(!raw.is_empty(), "Git index path unavailable")?;
    let path = if Path::new(&raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        execution.join(raw)
    };
    let path = std::fs::canonicalize(
        path.parent()
            .ok_or_else(|| Error::new("Git index parent absent"))?,
    )
    .map_err(Error::new)?
    .join(
        path.file_name()
            .ok_or_else(|| Error::new("Git index name absent"))?,
    );
    let source = std::fs::symlink_metadata(&path).map_err(Error::new)?;
    require(
        source.is_file()
            && source.nlink() == 1
            && source.uid() == rustix::process::getuid().as_raw(),
        "Git index is unsafe",
    )?;
    let lock_path = path.with_file_name(format!(
        "{}.lock",
        path.file_name().unwrap().to_string_lossy()
    ));
    let mut lock = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&lock_path)
        .map_err(|_| Error::busy("Git index lock unavailable; preserve existing lock"))?;
    let _guard = IndexLock(lock_path.clone());
    // Git indexes are binary and intentionally do not cross the outbound boundary.
    let read = || -> Result<Vec<u8>> {
        use std::io::Read;
        let fd = rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(Error::new)?;
        let info = rustix::fs::fstat(&fd).map_err(Error::new)?;
        require(
            info.st_dev as u64 == source.dev()
                && info.st_ino as u64 == source.ino()
                && info.st_size >= 0
                && info.st_size as u64 <= 64 * 1024 * 1024,
            "Git index changed or exceeds bound",
        )?;
        let mut bytes = Vec::new();
        std::fs::File::from(fd)
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(Error::new)?;
        require(
            bytes.len() as u64 == info.st_size as u64,
            "Git index changed while reading",
        )?;
        Ok(bytes)
    };
    let original = read()?;
    lock.write_all(&original)
        .and_then(|()| lock.sync_all())
        .map_err(Error::new)?;
    let expected = crate::git::bytes(
        service,
        execution,
        &["write-tree"],
        None,
        Some(&lock_path),
        stop,
    )?;
    require(
        expected.strip_suffix(b"\n").unwrap_or(&expected) == tree.as_bytes(),
        "locked Git index differs from reviewed tree",
    )?;
    recheck(repo, request, baseline, service, stop)?;
    let commit = crate::git::text(
        service,
        execution,
        &["commit-tree", tree, "-p", base, "-m", &request.message],
        stop,
    )?;
    require(
        (crate::hex(&commit, 40) || crate::hex(&commit, 64))
            && crate::git::text(
                service,
                execution,
                &["rev-parse", &format!("{commit}^{{tree}}")],
                stop,
            )? == tree
            && crate::git::text(
                service,
                execution,
                &["rev-parse", &format!("{commit}^")],
                stop,
            )? == base,
        "created commit differs from reviewed tree/parent",
    )?;
    recheck(repo, request, baseline, service, stop)?;
    require(
        read()? == original
            && std::fs::read(&lock_path).map_err(Error::new)? == original
            && crate::git::text(service, execution, &["rev-parse", "HEAD"], stop)? == base,
        "Git index or HEAD changed before exact publication",
    )?;
    let current = crate::git::run(
        service,
        execution,
        &["symbolic-ref", "-q", "HEAD"],
        None,
        None,
        stop,
    )?;
    require(
        current.cleanup_complete
            && current.output_complete
            && !current.limited
            && matches!(current.code, 0 | 1),
        "HEAD ref unconfirmed",
    )?;
    let current_ref = if current.code == 1 {
        "HEAD".into()
    } else {
        String::from_utf8(current.stdout)
            .map_err(Error::new)?
            .trim()
            .to_owned()
    };
    require(
        current_ref == branch,
        "HEAD ref changed before exact publication",
    )?;
    crate::git::bytes(
        service,
        execution,
        &[
            "update-ref",
            "-m",
            "panel: accept reviewed patch",
            &branch,
            &commit,
            base,
        ],
        None,
        None,
        stop,
    )
    .map_err(|e| Error {
        publication_uncertain: true,
        ..e
    })?;
    Ok(commit)
}
pub(crate) fn run<S: Services + ?Sized>(
    repo: &Path,
    request: &AcceptRequest,
    service: &S,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    for name in [
        "GIT_INDEX_FILE",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
    ] {
        require(
            std::env::var_os(name).is_none_or(|v| v.is_empty()),
            "custom Git repository/index overrides are unsupported for acceptance",
        )?;
    }
    let store = Store::open(repo)?;
    let execution = std::fs::canonicalize(&request.execution_repo).map_err(Error::new)?;
    let digest = crate::digest(execution.as_os_str().as_encoded_bytes());
    let tx = store.transaction(
        &format!("panel-accept-{}", &digest[..24]),
        Duration::from_secs(10),
    )?;
    crate::control::review_readers(repo, &execution)?;
    let base = crate::git::text(service, &execution, &["rev-parse", "HEAD"], stop)?;
    require(
        crate::git::bytes(
            service,
            &execution,
            &["status", "--porcelain", "--untracked-files=all"],
            None,
            None,
            stop,
        )?
        .is_empty(),
        "dirty integration tree; accept refused before application",
    )?;
    let snapshot = preflight(repo, request, service, stop, false)?;
    let candidate = &snapshot["candidate"];
    let patch = verified_patch(&store, candidate)?;
    let scratch = crate::git::Scratch::new()?;
    let index = scratch.0.join("index");
    crate::git::bytes(
        service,
        &execution,
        &["read-tree", &base],
        None,
        Some(&index),
        stop,
    )?;
    crate::git::bytes(
        service,
        &execution,
        &["apply", "--cached", "--binary", "-"],
        Some(&patch),
        Some(&index),
        stop,
    )?;
    let tree = String::from_utf8(crate::git::bytes(
        service,
        &execution,
        &["write-tree"],
        None,
        Some(&index),
        stop,
    )?)
    .map_err(Error::new)?
    .trim()
    .to_owned();
    let record_path = format!(
        ".oms/panel/acceptances/{}-{}.json",
        request.task_id,
        text(candidate, "patch_sha256")
    );
    require(
        tx.json(Path::new(&record_path), MAX_RESULT, true)?
            .is_none(),
        "acceptance recovery record exists; preserve state, inspect it and never reapply blindly",
    )?;
    let mut recovery = serde_json::json!({"schema":1,"task_id":request.task_id,"execution_repo":execution,"snapshot":snapshot,"base":base,"tree":tree,"stage":"application","commit":null,"message":request.message,"verify":request.verify});
    tx.write_json(Path::new(&record_path), &recovery, true, MAX_RESULT)?;
    let outcome = (|| -> Result<Value> {
        let fence = serde_json::json!({"snapshot":snapshot,"owner":request.owner,"task_id":request.task_id,"base":base});
        let mut argv = args(&[
            "--patch",
            store
                .repo()
                .join(text(candidate, "patch"))
                .to_str()
                .ok_or_else(|| Error::new("patch path must be UTF-8"))?,
            "--verify",
            &request.verify,
            "--panel-accept",
            &serde_json::to_string(&fence).map_err(Error::new)?,
        ]);
        if request.allow_verifier_change {
            argv.push("--allow-verifier-change".into());
        }
        if let Some(reason) = &request.allow_floor_change {
            argv.extend(["--allow-floor-change".into(), reason.into()]);
        }
        tx.check()?;
        service
            .command(NativeCommand::Land, &execution, &argv, stop)?
            .success()?;
        recheck(repo, request, &snapshot, service, stop)?;
        require(
            crate::git::text(service, &execution, &["rev-parse", "HEAD"], stop)? == base,
            "HEAD changed after application",
        )?;
        let mut add = vec!["--literal-pathspecs", "add", "--all", "--"];
        let paths = crate::files::patch_paths(&patch)?;
        require(!paths.is_empty(), "candidate patch has no paths")?;
        add.extend(paths.iter().map(String::as_str));
        crate::git::bytes(service, &execution, &add, None, None, stop)?;
        require(
            crate::git::text(service, &execution, &["write-tree"], stop)? == tree
                && crate::git::bytes(
                    service,
                    &execution,
                    &["diff", "--name-only"],
                    None,
                    None,
                    stop,
                )?
                .is_empty()
                && crate::git::bytes(
                    service,
                    &execution,
                    &["ls-files", "--others", "--exclude-standard"],
                    None,
                    None,
                    stop,
                )?
                .is_empty(),
            "applied/index changes differ from reviewed patch",
        )?;
        recovery["stage"] = "commit".into();
        tx.write_json(Path::new(&record_path), &recovery, false, MAX_RESULT)?;
        let commit = exact_commit(
            repo, &execution, request, &snapshot, &base, &tree, service, stop,
        )?;
        recovery["commit"] = commit.clone().into();
        recovery["stage"] = "plan review".into();
        tx.write_json(Path::new(&record_path), &recovery, false, MAX_RESULT)?;
        require(
            crate::git::text(service, &execution, &["rev-parse", "HEAD"], stop)? == commit
                && crate::git::text(service, &execution, &["rev-parse", "HEAD^"], stop)? == base
                && crate::git::text(service, &execution, &["rev-parse", "HEAD^{tree}"], stop)?
                    == tree
                && crate::git::bytes(
                    service,
                    &execution,
                    &["status", "--porcelain", "--untracked-files=all"],
                    None,
                    None,
                    stop,
                )?
                .is_empty(),
            "committed tree or HEAD differs from reviewed application",
        )?;
        recheck(repo, request, &snapshot, service, stop)?;
        tx.check()?;
        complete(
            repo, &execution, request, &snapshot, &base, &tree, &commit, service, stop,
        )?;
        recovery["stage"] = "recorded".into();
        tx.write_json(Path::new(&record_path), &recovery, false, MAX_RESULT)?;
        Ok(
            serde_json::json!({"sha":commit,"task_id":request.task_id,"patch_sha256":candidate["patch_sha256"]}),
        )
    })();
    outcome.map_err(|e| Error { publication_uncertain:true,..Error::new(format!("accept {} failed; preserve state, never reapply blindly; reconcile {record_path} and patch-land --recover: {}",text(&recovery,"stage"),e.message)) })
}

#[allow(clippy::too_many_arguments)]
fn complete<S: Services + ?Sized>(
    repo: &Path,
    execution: &Path,
    request: &AcceptRequest,
    snapshot: &Value,
    base: &str,
    tree: &str,
    commit: &str,
    service: &S,
    stop: &AtomicBool,
) -> Result<()> {
    let store = Store::open(repo)?;
    let candidate = &snapshot["candidate"];
    let record = serde_json::json!({"schema":1,"task_id":request.task_id,"plan_id":snapshot["task"]["plan_id"],"lease_id":snapshot["task"]["lease_id"],"candidate_event_id":candidate["event_id"],"candidate_attempt_id":snapshot["worker"]["attempt_id"],"patch_sha256":candidate["patch_sha256"],"artifact":candidate["artifact"],"patch":candidate["patch"],"commit":commit,"tree":tree,"base":base,"review_gate":snapshot["review_gate"]});
    let mut review = ExecutionRequest::new(
        args(&[
            "review",
            "--id",
            &request.task_id,
            "--lease-id",
            &text(&snapshot["task"], "lease_id"),
            "--artifact",
            &text(candidate, "artifact"),
            "--patch",
            &text(candidate, "patch"),
        ]),
        execution,
    );
    review.environment.push((
        "OMS_PANEL_ACCEPT_RECORD_JSON".into(),
        serde_json::to_string(&record).map_err(Error::new)?.into(),
    ));
    if ["claimed", "running"].contains(&text(&snapshot["task"], "state").as_str()) {
        service
            .command_context(NativeCommand::Plan, &review, stop)?
            .success()?;
        let plan = store
            .json(Path::new(".oms/plan/tasks.json"), 4 * MAX_RESULT, false)?
            .unwrap();
        let task = &plan["tasks"][&request.task_id];
        require(
            task["panel_accept"] == record
                && task["state"] == "review"
                && task["review_lease_id"] == snapshot["task"]["lease_id"],
            "native plan review did not record the exact acceptance",
        )?;
    } else {
        require(
            snapshot["task"]["panel_accept"] == record
                && snapshot["task"]["review_lease_id"] == snapshot["task"]["lease_id"],
            "plan review does not match interrupted acceptance",
        )?;
    }
    if index_rows(repo)?.iter().any(|v| {
        v["kind"] == "panel-accept"
            && v["task_id"] == request.task_id
            && v["patch_sha256"] == candidate["patch_sha256"]
            && v["base_sha"] == commit
            && v["exit"] == 0
    }) {
        return Ok(());
    }
    let mut registration = oms_command_telemetry::store::AppendArtifactRequest {
        kind: "panel-accept".into(),
        provider: request.owner.clone(),
        patch: Some(store.repo().join(text(candidate, "patch"))),
        task_id: Some(request.task_id.clone()),
        inherit_env: false,
        ..Default::default()
    };
    for (name, value) in [
        ("OMS_INDEX_PLAN_ID", text(&snapshot["task"], "plan_id")),
        (
            "OMS_INDEX_ATTEMPT_ID",
            text(&snapshot["worker"], "attempt_id"),
        ),
        ("OMS_INDEX_BASE_SHA", commit.to_owned()),
    ] {
        registration.env_overrides.insert(name.into(), value);
    }
    oms_command_telemetry::store::append_artifact_index(repo, &registration, stop)
        .map_err(crate::services::telemetry_error)?;
    Ok(())
}

fn replay<S: Services + ?Sized>(
    execution: &Path,
    base: &str,
    patch: &[u8],
    service: &S,
    stop: &AtomicBool,
) -> Result<String> {
    let scratch = crate::git::Scratch::new()?;
    let index = scratch.0.join("index");
    crate::git::bytes(
        service,
        execution,
        &["read-tree", base],
        None,
        Some(&index),
        stop,
    )?;
    crate::git::bytes(
        service,
        execution,
        &["apply", "--cached", "--binary", "-"],
        Some(patch),
        Some(&index),
        stop,
    )?;
    String::from_utf8(crate::git::bytes(
        service,
        execution,
        &["write-tree"],
        None,
        Some(&index),
        stop,
    )?)
    .map(|v| v.trim().to_owned())
    .map_err(Error::new)
}

pub(crate) fn recover<S: Services + ?Sized>(
    repo: &Path,
    request: &AcceptRequest,
    service: &S,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    let digest = request
        .expected_patch_sha256
        .as_deref()
        .ok_or_else(|| Error::new("recovery requires the exact saved patch digest"))?;
    let execution = std::fs::canonicalize(&request.execution_repo).map_err(Error::new)?;
    let store = Store::open(repo)?;
    let key = crate::digest(execution.as_os_str().as_encoded_bytes());
    let tx = store.transaction(
        &format!("panel-accept-{}", &key[..24]),
        Duration::from_secs(10),
    )?;
    crate::control::review_readers(repo, &execution)?;
    let path = format!(".oms/panel/acceptances/{}-{digest}.json", request.task_id);
    let mut saved = tx
        .json(Path::new(&path), MAX_RESULT, false)?
        .ok_or_else(|| Error::new("acceptance recovery record unavailable"))?;
    require(
        saved["schema"] == 1
            && saved["task_id"] == request.task_id
            && saved["execution_repo"] == execution.to_string_lossy().as_ref()
            && saved["message"] == request.message
            && saved["verify"] == request.verify,
        "acceptance recovery request differs from saved intent",
    )?;
    let snapshot = preflight(repo, request, service, stop, true)?;
    for field in [
        "candidate",
        "worker",
        "review_gate",
        "allowed_paths",
        "superseded_attempts",
    ] {
        require(
            snapshot[field] == saved["snapshot"][field],
            "acceptance authority changed before recovery",
        )?;
    }
    let mut prior_task = saved["snapshot"]["task"].clone();
    let mut current_task = snapshot["task"].clone();
    for field in [
        "state",
        "artifact",
        "patch",
        "review_lease_id",
        "panel_accept",
        "updated_at",
    ] {
        prior_task
            .as_object_mut()
            .ok_or_else(|| Error::new("saved task unavailable"))?
            .remove(field);
        current_task
            .as_object_mut()
            .ok_or_else(|| Error::new("current task unavailable"))?
            .remove(field);
    }
    require(
        prior_task == current_task,
        "task contract changed before recovery",
    )?;
    let base = text(&saved, "base");
    let tree = text(&saved, "tree");
    require(
        [&base, &tree]
            .iter()
            .all(|v| crate::hex(v, 40) || crate::hex(v, 64)),
        "malformed saved Git identity",
    )?;
    let patch = verified_patch(&store, &snapshot["candidate"])?;
    require(
        replay(&execution, &base, &patch, service, stop)? == tree,
        "saved tree differs from reviewed patch",
    )?;
    require(
        crate::git::text(service, &execution, &["write-tree"], stop)? == tree
            && crate::git::bytes(
                service,
                &execution,
                &["diff", "--name-only"],
                None,
                None,
                stop,
            )?
            .is_empty()
            && crate::git::bytes(
                service,
                &execution,
                &["ls-files", "--others", "--exclude-standard"],
                None,
                None,
                stop,
            )?
            .is_empty(),
        "recovery index or worktree differs from reviewed tree",
    )?;
    let head = crate::git::text(service, &execution, &["rev-parse", "HEAD"], stop)?;
    let commit = if head == base {
        require(
            saved["commit"].is_null(),
            "saved acceptance commit no longer names HEAD",
        )?;
        tx.check()?;
        exact_commit(
            repo, &execution, request, &snapshot, &base, &tree, service, stop,
        )?
    } else {
        require(
            saved["commit"].is_null() || saved["commit"] == head,
            "saved acceptance commit differs from HEAD",
        )?;
        require(
            crate::git::text(service, &execution, &["rev-parse", "HEAD^"], stop)? == base
                && crate::git::text(service, &execution, &["rev-parse", "HEAD^{tree}"], stop)?
                    == tree,
            "HEAD is not the exact interrupted acceptance",
        )?;
        if saved["commit"].is_null() {
            require(
                crate::git::text(
                    service,
                    &execution,
                    &["show", "-s", "--format=%B", "HEAD"],
                    stop,
                )?
                .trim()
                    == request.message.trim(),
                "unrecorded acceptance commit message differs from saved intent",
            )?;
        }
        head
    };
    saved["commit"] = commit.clone().into();
    saved["stage"] = "plan review".into();
    tx.write_json(Path::new(&path), &saved, false, MAX_RESULT)?;
    recheck(repo, request, &snapshot, service, stop)?;
    tx.check()?;
    complete(
        repo, &execution, request, &snapshot, &base, &tree, &commit, service, stop,
    )
    .map_err(|e| Error {
        publication_uncertain: true,
        ..e
    })?;
    saved["stage"] = "recorded".into();
    tx.write_json(Path::new(&path), &saved, false, MAX_RESULT)?;
    Ok(
        serde_json::json!({"sha":commit,"task_id":request.task_id,"patch_sha256":digest,"recovered":true}),
    )
}

fn accepted_product<S: Services + ?Sized>(
    execution: &Path,
    record: &Value,
    patch: &[u8],
    service: &S,
    stop: &AtomicBool,
) -> Result<()> {
    let base = text(record, "base");
    let tree = text(record, "tree");
    let accepted = text(record, "commit");
    require(
        record["schema"] == 1
            && record["patch_sha256"] == crate::digest(patch)
            && [&base, &tree, &accepted]
                .iter()
                .all(|v| crate::hex(v, 40) || crate::hex(v, 64)),
        "malformed accepted Git or patch identity",
    )?;
    require(
        replay(execution, &base, patch, service, stop)? == tree,
        "accepted tree differs from reviewed patch",
    )?;
    let found = crate::git::run(
        service,
        execution,
        &["cat-file", "-e", &format!("{accepted}^{{commit}}")],
        None,
        None,
        stop,
    )?;
    require(
        found.cleanup_complete && found.output_complete && !found.limited,
        "accepted commit probe incomplete",
    )?;
    if found.code == 0 {
        let parents = crate::git::text(
            service,
            execution,
            &["rev-list", "--parents", "-n", "1", &accepted],
            stop,
        )?;
        let parents = parents.split_whitespace().collect::<Vec<_>>();
        require(
            parents == [accepted.as_str(), base.as_str()]
                && crate::git::text(
                    service,
                    execution,
                    &["rev-parse", &format!("{accepted}^{{tree}}")],
                    stop,
                )? == tree,
            "accepted commit differs from recorded tree/base",
        )?;
    }
    Ok(())
}

pub(crate) fn verify_publication_contents<S: Services + ?Sized>(
    execution: &Path,
    published: &str,
    record: &Value,
    patch: &[u8],
    push_base: &str,
    service: &S,
    stop: &AtomicBool,
) -> Result<()> {
    require(
        record.is_object()
            && (crate::hex(published, 40) || crate::hex(published, 64))
            && (crate::hex(push_base, 40) || crate::hex(push_base, 64))
            && published != push_base,
        "publication product needs exact accepted identities and nonempty push base",
    )?;
    accepted_product(execution, record, patch, service, stop)?;
    crate::git::bytes(
        service,
        execution,
        &[
            "cat-file",
            "-e",
            &format!("{}^{{commit}}", text(record, "commit")),
        ],
        None,
        None,
        stop,
    )?;
    publication_contains(
        execution, published, record, push_base, patch, service, stop,
    )
}

pub(crate) fn validate_landed<S: Services + ?Sized>(
    repo: &Path,
    request: &crate::results::FinalizeRequest,
    service: &S,
    stop: &AtomicBool,
) -> Result<Value> {
    let commit = request
        .landed_commit
        .as_deref()
        .ok_or_else(|| Error::new("landed commit missing"))?;
    let verify = request
        .verify
        .clone()
        .ok_or_else(|| Error::new("landed acceptance verifier missing"))?;
    let accept = AcceptRequest {
        owner: request.owner.clone(),
        task_id: request.task_id.clone(),
        main_attempt_id: request.main_attempt_id.clone(),
        message: "reconcile publication".into(),
        verify,
        execution_repo: request.execution_repo.clone(),
        expected_patch_sha256: None,
        allow_verifier_change: false,
        allow_floor_change: None,
    };
    let snapshot = preflight(repo, &accept, service, stop, true)?;
    let task = &snapshot["task"];
    let candidate = &snapshot["candidate"];
    let execution = std::fs::canonicalize(&request.execution_repo).map_err(Error::new)?;
    let store = Store::open(repo)?;
    let patch = verified_patch(&store, candidate)?;
    let record = &task["panel_accept"];
    let finished = task["state"] == "done" && task["landing"]["sha"] == commit;
    require(
        ["claimed", "running", "review"].contains(&text(task, "state").as_str()) || finished,
        "landing does not match current task state",
    )?;
    if ["review", "done"].contains(&text(task, "state").as_str()) {
        require(
            task["review_lease_id"] == task["lease_id"]
                && task["artifact"] == candidate["artifact"]
                && task["patch"] == candidate["patch"],
            "reviewed task lease or candidate changed",
        )?;
    }
    if record.is_object() {
        require(
            record["schema"] == 1
                && record["task_id"] == request.task_id
                && ["plan_id", "lease_id"]
                    .iter()
                    .all(|key| record[*key] == task[*key])
                && record["candidate_event_id"] == candidate["event_id"]
                && record["candidate_attempt_id"] == snapshot["worker"]["attempt_id"]
                && ["patch_sha256", "artifact", "patch"]
                    .iter()
                    .all(|key| record[*key] == candidate[*key])
                && record["review_gate"] == snapshot["review_gate"],
            "stored acceptance identity changed",
        )?;
        accepted_product(&execution, record, &patch, service, stop)?;
    }
    // Exercise the native finish proof against a private plan before moving the
    // canonical task. The native domain owns publication receipt authentication.
    let mut shadow = store
        .json(Path::new(".oms/plan/tasks.json"), 4 * MAX_RESULT, false)?
        .unwrap();
    let mut shadow_task = shadow["tasks"][&request.task_id].clone();
    shadow_task["state"] = "review".into();
    shadow_task["review_lease_id"] = task["lease_id"].clone();
    shadow_task["artifact"] = candidate["artifact"].clone();
    shadow_task["patch"] = candidate["patch"].clone();
    shadow["tasks"] = serde_json::json!({request.task_id.clone():shadow_task});
    let scratch = crate::git::Scratch::new()?;
    let shadow_store = Store::open(&scratch.0)?;
    shadow_store
        .lock_at(Path::new("lock"), Duration::from_secs(3))?
        .write_json(Path::new("tasks.json"), &shadow, false, 4 * MAX_RESULT)?;
    let shadow_path = scratch.0.join("tasks.json");
    let mut finish = args(&["finish", "--file"]);
    finish.push(shadow_path.into_os_string());
    finish.extend(args(&[
        "--id",
        &request.task_id,
        "--lease-id",
        &text(task, "lease_id"),
        "--landed-commit",
        commit,
        "--artifact",
        &text(candidate, "artifact"),
        "--patch",
        &text(candidate, "patch"),
    ]));
    service
        .command(NativeCommand::Plan, &execution, &finish, stop)?
        .success()?;
    let proof = shadow_store
        .json(Path::new("tasks.json"), 4 * MAX_RESULT, false)?
        .unwrap();
    let landing = &proof["tasks"][&request.task_id]["landing"];
    require(
        proof["tasks"][&request.task_id]["state"] == "done" && landing["sha"] == commit,
        "native finish did not return a validated publication",
    )?;
    let receipt = service.publication_receipt(
        repo,
        &execution,
        commit,
        &text(landing, "receipt_sha256"),
        stop,
    )?;
    require(
        crate::digest(&receipt) == landing["receipt_sha256"],
        "publication receipt changed",
    )?;
    let receipt = crate::decode(&receipt, MAX_RESULT)?;
    let base = text(&receipt["push"], "base");
    require(
        (crate::hex(&base, 40) || crate::hex(&base, 64)) && base != commit,
        "exact replay requires a nonempty publication push base",
    )?;
    publication_contains(&execution, commit, record, &base, &patch, service, stop)?;
    Ok(snapshot)
}

fn publication_contains<S: Services + ?Sized>(
    execution: &Path,
    commit: &str,
    record: &Value,
    base: &str,
    patch: &[u8],
    service: &S,
    stop: &AtomicBool,
) -> Result<()> {
    if let Some(accepted) = record["commit"].as_str() {
        let ancestor = crate::git::run(
            service,
            execution,
            &["merge-base", "--is-ancestor", accepted, commit],
            None,
            None,
            stop,
        )?;
        require(
            ancestor.cleanup_complete && ancestor.output_complete && !ancestor.limited,
            "publication ancestry probe incomplete",
        )?;
        if ancestor.code == 0 {
            return Ok(());
        }
    }
    let paths = crate::files::patch_paths(patch)?;
    require(!paths.is_empty(), "reviewed patch has no publication paths")?;
    let range = if record.is_object() {
        commit.to_owned()
    } else {
        format!("{base}..{commit}")
    };
    let mut argv = vec![
        "--literal-pathspecs",
        "log",
        "--no-merges",
        "--format=%H",
        "--max-count=10000",
        &range,
        "--",
    ];
    argv.extend(paths.iter().map(String::as_str));
    let history = crate::git::text(service, execution, &argv, stop)?;
    require(
        history.lines().count() < 10000,
        "publication replay history exceeds bound",
    )?;
    for sha in history.lines() {
        let parent =
            crate::git::text(service, execution, &["rev-parse", &format!("{sha}^")], stop)?;
        let tree = crate::git::text(
            service,
            execution,
            &["rev-parse", &format!("{sha}^{{tree}}")],
            stop,
        )?;
        let Ok(replayed) = replay(execution, &parent, patch, service, stop) else {
            continue;
        };
        if record.is_object() && replayed == tree {
            return Ok(());
        }
        if record.is_null() {
            let changes = |left: &str, right: &str| -> Result<Vec<u8>> {
                let mut argv = vec![
                    "--literal-pathspecs",
                    "diff",
                    "--no-ext-diff",
                    "--raw",
                    left,
                    right,
                    "--",
                ];
                argv.extend(paths.iter().map(String::as_str));
                crate::git::bytes(service, execution, &argv, None, None, stop)
            };
            if !changes(&parent, &tree)?.is_empty()
                && changes(&replayed, &tree)?.is_empty()
                && changes(&replayed, commit)?.is_empty()
            {
                return Ok(());
            }
        }
    }
    Err(Error::new(
        "published SHA does not contain an exact replay of the reviewed patch",
    ))
}

pub(crate) fn finish_landed<S: Services + ?Sized>(
    repo: &Path,
    request: &crate::results::FinalizeRequest,
    before: &Value,
    service: &S,
    stop: &AtomicBool,
) -> Result<()> {
    let execution = std::fs::canonicalize(&request.execution_repo).map_err(Error::new)?;
    let store = Store::open(repo)?;
    let key = crate::digest(execution.as_os_str().as_encoded_bytes());
    let tx = store.transaction(
        &format!("panel-accept-{}", &key[..24]),
        Duration::from_secs(10),
    )?;
    let snapshot = validate_landed(repo, request, service, stop)?;
    let mut task = snapshot["task"].clone();
    task.as_object_mut().unwrap().remove("plan_id");
    require(
        task == before["task"],
        "landed task authority changed during verification",
    )?;
    if task["state"] == "done" {
        return Ok(());
    }
    tx.check()?;
    for verb in if task["state"] == "claimed" || task["state"] == "running" {
        &["review", "finish"][..]
    } else {
        &["finish"][..]
    } {
        let mut argv = args(&[
            verb,
            "--id",
            &request.task_id,
            "--lease-id",
            &text(&task, "lease_id"),
            "--artifact",
            &text(&snapshot["candidate"], "artifact"),
            "--patch",
            &text(&snapshot["candidate"], "patch"),
        ]);
        if *verb == "finish" {
            argv.extend(args(&[
                "--landed-commit",
                request.landed_commit.as_deref().unwrap(),
            ]));
        }
        service
            .command(NativeCommand::Plan, &execution, &argv, stop)?
            .success()?;
    }
    Ok(())
}
