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
            && task["provider"] == request.owner
            && lease
                .strip_prefix("lease_")
                .is_some_and(|v| crate::hex(v, 32))
            && plan_id
                .strip_prefix("plan_")
                .is_some_and(|v| crate::hex(v, 32)),
        "plan task owner/lease does not match the current main",
    )?;
    require(
        ["claimed", "running"].contains(&text(&task, "state").as_str()),
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
        !related.is_empty() && related.iter().all(|v| v["terminal"] == true),
        "task has live or unconfirmed attempts",
    )?;
    let workers = related
        .iter()
        .filter(|v| v["refs"]["panel_role"] == "worker" && v["refs"]["panel_access"] == "write")
        .collect::<Vec<_>>();
    require(
        workers.len() == 1,
        "accept requires one unique worker candidate for this task",
    )?;
    let worker = *workers[0];
    require(
        worker["parent_attempt_id"] == request.main_attempt_id
            && worker["refs"]["panel_lease_id"] == lease
            && worker["refs"]["panel_purpose"] != "investigate",
        "candidate worker is not bound to this main/lease or is report-only",
    )?;
    let rows = index_rows(repo)?;
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
    let allowed = array(&task["allowed_paths"])
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new("malformed allowed paths"))
        })
        .collect::<Result<Vec<_>>>()?;
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
        serde_json::json!({"task":task,"candidate":candidate,"worker":{"attempt_id":worker["attempt_id"],"task_id":worker["task_id"],"parent_attempt_id":worker["parent_attempt_id"],"refs":worker["refs"]},"review_gate":gate,"allowed_paths":allowed}),
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
            info.st_dev == source.dev()
                && info.st_ino == source.ino()
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
        &text(candidate, "patch_sha256")
    );
    require(
        tx.json(Path::new(&record_path), MAX_RESULT, true)?
            .is_none(),
        "acceptance recovery record exists; preserve state, inspect it and never reapply blindly",
    )?;
    let mut recovery = serde_json::json!({"schema":1,"task_id":request.task_id,"execution_repo":execution,"snapshot":snapshot,"base":base,"tree":tree,"stage":"application","commit":null});
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
            &execution,
        );
        review.environment.push((
            "OMS_PANEL_ACCEPT_RECORD_JSON".into(),
            serde_json::to_string(&record).map_err(Error::new)?.into(),
        ));
        service
            .command_context(NativeCommand::Plan, &review, stop)?
            .success()?;
        recovery["stage"] = "recorded".into();
        recovery["acceptance"] = record;
        tx.write_json(Path::new(&record_path), &recovery, false, MAX_RESULT)?;
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
            ("OMS_INDEX_BASE_SHA", commit.clone()),
        ] {
            registration.env_overrides.insert(name.into(), value);
        }
        oms_command_telemetry::store::append_artifact_index(repo, &registration, stop)
            .map_err(crate::services::telemetry_error)?;
        Ok(
            serde_json::json!({"sha":commit,"task_id":request.task_id,"patch_sha256":candidate["patch_sha256"]}),
        )
    })();
    outcome.map_err(|e| Error { publication_uncertain:true,..Error::new(format!("accept {} failed; preserve state, never reapply blindly; reconcile {record_path} and patch-land --recover: {}",text(&recovery,"stage"),e.message)) })
}
