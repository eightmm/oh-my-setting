#![forbid(unsafe_code)]

use oms_command_plan::services::{GroupMembership, GroupResolver};
use oms_command_plan::{Services, UnavailableServices, owns, run, run_with_io, run_with_services};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let n = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("oms-plan-cli-{}-{seq}-{n}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        Self(fs::canonicalize(d).unwrap())
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `Context::from_environment` reads OMS_* variables; rerun in a child with
/// them removed so a harness/worker environment cannot change the outcome.
fn isolated(name: &str) -> bool {
    if std::env::vars_os().all(|(k, _)| !k.to_string_lossy().starts_with("OMS_")) {
        return false;
    }
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", name, "--test-threads=1", "--nocapture"]);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("OMS_") {
            cmd.env_remove(k);
        }
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    print!("{}", String::from_utf8_lossy(&out.stdout));
    true
}

fn fixture_git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"]);
    command
}

fn git(repo: &Path, args: &[&str]) {
    let ok = fixture_git(repo)
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

fn repo(tmp: &Tmp) -> PathBuf {
    repo_at(tmp, "repo")
}

fn repo_at(tmp: &Tmp, name: &str) -> PathBuf {
    let r = tmp.0.join(name);
    fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q"]);
    fs::write(r.join("a.txt"), "x").unwrap();
    git(&r, &["add", "."]);
    git(&r, &["commit", "-qm", "init"]);
    r
}

fn args(a: &[&str]) -> Vec<OsString> {
    a.iter().map(OsString::from).collect()
}

/// Account proof for group-writable fixtures (umask 002 hosts): the invoking
/// user is the sole member of every group.
struct Host;
impl Services for Host {
    fn group_membership_resolver(&self) -> Option<GroupResolver> {
        fn resolve(gid: u32) -> oms_command_plan::Result<GroupMembership> {
            let uid = rustix::process::getuid().as_raw();
            Ok(GroupMembership {
                gid,
                explicit_member_uids: vec![uid],
                primary_member_uids: vec![uid],
            })
        }
        Some(resolve)
    }
}

fn go(cwd: &Path, a: &[&str]) -> (i32, String, String) {
    go_with(&Host, cwd, a)
}

fn go_with(services: &dyn Services, cwd: &Path, a: &[&str]) -> (i32, String, String) {
    let stop = AtomicBool::new(false);
    let (c, o, e) = run_with_services(&args(a), cwd, &stop, services);
    (
        c,
        String::from_utf8(o).unwrap(),
        String::from_utf8(e).unwrap(),
    )
}

#[test]
fn owns_only_agent_plan() {
    assert!(owns("agent-plan", &[]));
    assert!(!owns("agent-events", &args(&["init"])));
    assert!(!owns("", &[]));
}

#[test]
fn status_snapshot_for_rechecks_after_projecting_expected_plan() {
    use oms_command_plan::{Action, Context, Plan, Request, json};
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt;

    if isolated("status_snapshot_for_rechecks_after_projecting_expected_plan") {
        return;
    }
    thread_local! {
        static GROUP_CHECKS: Cell<usize> = const { Cell::new(0) };
        static CHANGE_PLAN: Cell<Option<PathBuf>> = const { Cell::new(None) };
    }
    struct StatusHost;
    impl Services for StatusHost {
        fn group_membership_resolver(&self) -> Option<GroupResolver> {
            Some(|gid| {
                let count = GROUP_CHECKS.get() + 1;
                GROUP_CHECKS.set(count);
                // The first proof for the issue cache occurs inside status rendering.
                if count == 3
                    && let Some(path) = CHANGE_PLAN.take()
                {
                    fs::write(path, br#"{"schema":3,"tasks":{}}"#).unwrap();
                }
                Host.group_membership_resolver().unwrap()(gid)
            })
        }
    }

    let tmp = Tmp::new();
    let repo = repo(&tmp);
    let active = repo.join(".oms/plan/tasks.json");
    let cache = repo.join(oms_github::freshness::CACHE_REL);
    fs::create_dir_all(active.parent().unwrap()).unwrap();
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    for path in [
        &repo,
        &repo.join(".oms"),
        active.parent().unwrap(),
        cache.parent().unwrap(),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let raw = br#"{"schema":3,"tasks":{"a":{"id":"a","state":"ready","depends":[],"issue":"org/repo#1"}}}"#;
    fs::write(&active, raw).unwrap();
    fs::write(&cache, b"").unwrap();
    for path in [&active, &cache] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o660)).unwrap();
    }

    let context = Context::from_environment(&Request::new(Action::Status), &repo).unwrap();
    let stop = AtomicBool::new(false);
    let plan = Plan::open(context, &StatusHost, &stop).unwrap();
    let status = plan.status_snapshot_for(raw).unwrap();
    assert_eq!(status.text("plan_sha256"), json::sha(raw));
    assert_eq!(status.get("task_count").int().unwrap(), 1);
    // Two proofs per Plan load plus two for the cache read.
    assert_eq!(GROUP_CHECKS.replace(0), 6);
    assert_eq!(status, plan.status_snapshot().unwrap());
    assert_eq!(GROUP_CHECKS.replace(0), 4);

    let stale = plan.status_snapshot_for(b"stale").unwrap_err();
    assert_eq!(
        (stale.code, stale.message.as_str()),
        (2, "plan changed during projection")
    );
    assert_eq!(GROUP_CHECKS.replace(0), 2);

    CHANGE_PLAN.set(Some(active));
    let drift = plan.status_snapshot_for(raw).unwrap_err();
    assert_eq!(drift, stale);
    assert_eq!(GROUP_CHECKS.get(), 6);
    assert!(CHANGE_PLAN.take().is_none());
}

#[test]
fn brief_file_round_trip_and_bounds() {
    if isolated("brief_file_round_trip_and_bounds") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let file = tmp.0.join("brief.txt");
    let path = file.to_str().unwrap();
    fs::write(&file, "first work\nsecond line").unwrap();
    assert_eq!(go(&r, &["init", "--goal", "brief test"]).0, 0);
    let added = go(
        &r,
        &["add", "--id", "a", "--title", "work", "--brief-file", path],
    );
    assert_eq!(added.0, 0, "{:?}", added);
    for action in ["brief", "show"] {
        let out = go(&r, &[action, "--id", "a"]);
        assert_eq!(out.0, 0, "{:?}", out);
        assert!(out.1.contains("first work"));
    }
    let raw = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let doc = oms_command_plan::json::parse(&raw).unwrap();
    let task_hash = oms_command_plan::json::hash(doc.get("tasks").get("a")).unwrap();
    let plan_hash = oms_command_plan::json::sha(&raw);
    fs::write(&file, "amended work").unwrap();
    let amended = go(
        &r,
        &[
            "amend",
            "--id",
            "a",
            "--expected-plan-sha256",
            &plan_hash,
            "--expected-task-sha256",
            &task_hash,
            "--reason",
            "more detail",
            "--brief-file",
            path,
        ],
    );
    assert_eq!(amended.0, 0, "{:?}", amended);
    for action in ["brief", "show"] {
        assert!(go(&r, &[action, "--id", "a"]).1.contains("amended work"));
    }
    let raw = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let doc = oms_command_plan::json::parse(&raw).unwrap();
    let task_hash = oms_command_plan::json::hash(doc.get("tasks").get("a")).unwrap();
    let plan_hash = oms_command_plan::json::sha(&raw);
    let history = doc.get("tasks").get("a").get("history").array().unwrap();
    let amendment = history
        .iter()
        .find(|h| h.text("kind") == "amended")
        .unwrap();
    assert_eq!(
        amendment.get("before").text("brief"),
        "first work\nsecond line"
    );
    assert_eq!(amendment.get("after").text("brief"), "amended work");
    let sensitive: String = ["sk-", &"a".repeat(48)].concat();
    for bytes in [
        vec![b'x'; 65537],
        vec![0xff],
        vec![],
        b" \n\t".to_vec(),
        sensitive.into_bytes(),
    ] {
        fs::write(&file, bytes).unwrap();
        assert_ne!(
            go(
                &r,
                &["add", "--id", "b", "--title", "bad", "--brief-file", path]
            )
            .0,
            0
        );
        assert_ne!(
            go(
                &r,
                &[
                    "amend",
                    "--id",
                    "a",
                    "--expected-plan-sha256",
                    &plan_hash,
                    "--expected-task-sha256",
                    &task_hash,
                    "--reason",
                    "bad input",
                    "--brief-file",
                    path
                ]
            )
            .0,
            0
        );
        assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), raw);
    }
    #[cfg(unix)]
    {
        let link = tmp.0.join("brief-link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_ne!(
            go(
                &r,
                &[
                    "add",
                    "--id",
                    "b",
                    "--title",
                    "bad",
                    "--brief-file",
                    link.to_str().unwrap()
                ]
            )
            .0,
            0
        );
    }
    assert_eq!(go(&r, &["add", "--id", "0", "--title", "no brief"]).0, 0);
    let before = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let status = go(&r, &["status", "--json"]);
    assert_eq!(status.0, 0, "{status:?}");
    let status = oms_command_plan::json::parse(status.1.as_bytes()).unwrap();
    let readiness = status.get("task_readiness").array().unwrap();
    let missing = readiness.iter().find(|row| row.text("id") == "0").unwrap();
    assert_eq!(missing.text("reason"), "brief-required");
    assert_eq!(
        missing.get("selection_eligible"),
        &oms_command_plan::json::Value::Bool(false)
    );
    let ready = readiness.iter().find(|row| row.text("id") == "a").unwrap();
    assert_eq!(ready.text("reason"), "selection-ready");
    assert_eq!(
        ready.get("selection_eligible"),
        &oms_command_plan::json::Value::Bool(true)
    );
    assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), before);
    // Optional readiness must not turn a previously readable legacy status
    // into a failure, or imply eligibility from unusable selector inputs.
    let mut legacy = oms_command_plan::json::parse(&before).unwrap();
    let mut tasks = legacy.get("tasks").clone();
    let mut task = tasks.get("a").clone();
    task.set("allowed_paths", oms_command_plan::json::number(1))
        .unwrap();
    tasks.set("a", task).unwrap();
    legacy.set("tasks", tasks).unwrap();
    let legacy_raw = oms_command_plan::json::canonical(&legacy).unwrap();
    fs::write(r.join(".oms/plan/tasks.json"), &legacy_raw).unwrap();
    let status = go(&r, &["status", "--json"]);
    assert_eq!(status.0, 0, "{status:?}");
    let status = oms_command_plan::json::parse(status.1.as_bytes()).unwrap();
    assert_eq!(
        status.get("task_readiness"),
        &oms_command_plan::json::Value::Null
    );
    assert_eq!(
        fs::read(r.join(".oms/plan/tasks.json")).unwrap(),
        legacy_raw
    );
    fs::write(r.join(".oms/plan/tasks.json"), &before).unwrap();
    let preview = go(&r, &["next", "--provider", "codex", "--json"]);
    assert_eq!(preview.0, 0, "{preview:?}");
    assert_eq!(
        oms_command_plan::json::parse(preview.1.as_bytes())
            .unwrap()
            .text("id"),
        "a",
        "a non-claiming selector must skip brief-less tasks too"
    );
    let next = go(&r, &["next", "--claim", "--provider", "codex", "--json"]);
    assert_eq!(next.0, 0, "{next:?}");
    assert_eq!(
        oms_command_plan::json::parse(next.1.as_bytes())
            .unwrap()
            .text("id"),
        "a"
    );
    fs::write(&file, "Implement dependent work").unwrap();
    assert_eq!(
        go(
            &r,
            &[
                "add",
                "--id",
                "dependent",
                "--title",
                "dependent",
                "--depends",
                "a",
                "--brief-file",
                path
            ]
        )
        .0,
        0
    );
    let status = go(&r, &["status", "--json"]);
    assert_eq!(status.0, 0, "{status:?}");
    let status = oms_command_plan::json::parse(status.1.as_bytes()).unwrap();
    let readiness = status.get("task_readiness").array().unwrap();
    let claimed = readiness.iter().find(|row| row.text("id") == "a").unwrap();
    assert_eq!(claimed.text("reason"), "attempt-active");
    assert_eq!(
        claimed.get("selection_eligible"),
        &oms_command_plan::json::Value::Bool(false)
    );
    let waiting = readiness
        .iter()
        .find(|row| row.text("id") == "dependent")
        .unwrap();
    assert_eq!(waiting.text("reason"), "dependencies-unfinished");
    assert_eq!(
        waiting.get("selection_eligible"),
        &oms_command_plan::json::Value::Bool(false)
    );
    // Batch rows carry a brief, and a replay with a different brief is not "already applied".
    let batch = |brief: &str| {
        let body = format!(
            r#"{{"schema":1,"kind":"agent-plan-batch","tasks":[{{"id":"bt","title":"batch work","brief":"{brief}"}}]}}"#
        );
        let file = tmp.0.join("batch.json");
        fs::write(&file, &body).unwrap();
        let plan = oms_command_plan::json::sha(fs::read(r.join(".oms/plan/tasks.json")).unwrap());
        go(
            &r,
            &[
                "add-batch",
                "--tasks",
                file.to_str().unwrap(),
                "--expected-tasks-sha256",
                &oms_command_plan::json::sha(body.as_bytes()),
                "--expected-plan-sha256",
                &plan,
            ],
        )
    };
    let added = batch("batch brief");
    assert_eq!(added.0, 0, "{added:?}");
    assert!(go(&r, &["brief", "--id", "bt"]).1.contains("batch brief"));
    let replay = batch("batch brief");
    assert_eq!(replay.0, 0, "{replay:?}");
    assert!(replay.1.contains("already applied"), "{replay:?}");
    let changed = batch("other brief");
    assert_ne!(changed.0, 0, "{changed:?}");
    assert!(changed.2.contains("does not match batch"), "{changed:?}");
}

#[test]
fn delegate_binding_keeps_the_issued_lease_and_fences_native_review() {
    if isolated("delegate_binding_keeps_the_issued_lease_and_fences_native_review") {
        return;
    }
    use json::{number, object, string};
    use oms_command_plan::{Action, Context, DelegateBinding, Plan, Request, json};
    for scenario in [
        "claim",
        "legacy-finish",
        "finish",
        "release",
        "stale-admission",
    ] {
        let next = scenario != "claim";
        let tmp = Tmp::new();
        let r = repo(&tmp);
        let stop = AtomicBool::new(false);
        let context =
            Context::from_environment_at(&Request::new(Action::Status), &r, &r, None).unwrap();
        let plan = Plan::open(context, &Host, &stop).unwrap();
        plan.execute(&Request::new(Action::Init).with("goal", "exercise the real delegate lease"))
            .unwrap();
        let publish_contract = |version: &str| {
            plan.execute(
                &Request::new(Action::ContractPropose)
                    .with("id", "api")
                    .with("summary", version),
            )
            .unwrap();
            plan.execute(
                &Request::new(Action::ContractAccept)
                    .with("id", "api")
                    .with("version", version),
            )
            .unwrap();
        };
        fs::write(r.join("brief.txt"), "Implement the fixture task").unwrap();
        let mut task = Request::new(Action::Add)
            .with("brief-file", "brief.txt")
            .with("id", "t1")
            .with("title", "write a.txt")
            .with("allowed", "a.txt")
            .with("verify", "true");
        if scenario == "stale-admission" {
            publish_contract("1");
            task = task.with("contracts", "api@v1");
        }
        plan.execute(&task).unwrap();
        let issue = || {
            let req = if next {
                Request::new(Action::Next).flag("claim")
            } else {
                Request::new(Action::Claim).with("id", "t1")
            };
            let response = plan
                .execute(&req.with("provider", "claude").flag("json"))
                .unwrap();
            json::parse(&response.stdout).unwrap()
        };
        let issued = issue();
        let canonical = plan.load().unwrap();
        assert_eq!(
            issued.text("task_sha256"),
            json::hash(canonical.task("t1").unwrap()).unwrap()
        );
        assert_eq!(issued.get("plan_id"), canonical.plan.get("plan_id"));
        assert!(canonical.task("t1").unwrap().opt("plan_id").is_none());
        let bind = |task: &json::Value| {
            DelegateBinding::parse(
                &json::canonical(&object([
                    ("schema", number(1)),
                    ("plan_id", canonical.plan.get("plan_id").clone()),
                    ("task_id", task.get("id").clone()),
                    ("task_sha256", task.get("task_sha256").clone()),
                    ("lease_id", task.get("lease_id").clone()),
                    ("lease_epoch", task.get("lease_epoch").clone()),
                    (
                        "base_sha",
                        string(plan.git().text(&["rev-parse", "HEAD"]).unwrap()),
                    ),
                ]))
                .unwrap(),
            )
            .unwrap()
        };
        let initial = bind(&issued);
        let start = Request::new(Action::Start)
            .with("id", "t1")
            .with("lease-id", initial.lease_id());
        let running = plan.execute_bound_task(&start, &initial, None).unwrap();
        assert!(
            plan.check_delegate_binding(&initial).is_err(),
            "Start must explicitly advance the issued hash"
        );
        assert_eq!(
            plan.check_delegate_binding(&running).unwrap().text("state"),
            "running"
        );
        assert!(
            plan.execute_bound_task(
                &Request::new(Action::Release)
                    .with("id", "t1")
                    .with("lease-id", running.lease_id()),
                &running,
                None
            )
            .is_err(),
            "the bridge is not a generic mutation API"
        );
        fs::create_dir_all(r.join(".oms/artifacts")).unwrap();
        fs::write(r.join(".oms/artifacts/result.md"), "Actual native result\n").unwrap();
        fs::write(
            r.join(".oms/artifacts/result.patch"),
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-x\n+y\n",
        )
        .unwrap();
        let review = Request::new(Action::Review)
            .with("id", "t1")
            .with("lease-id", running.lease_id())
            .with("artifact", ".oms/artifacts/result.md")
            .with("patch", ".oms/artifacts/result.patch");
        let index = object([
            ("schema", number(1)),
            ("kind", string("delegate")),
            ("event_id", string("evt-native")),
            ("attempt_id", string("attempt-native")),
            ("operation_id", string("op-native")),
            ("task_id", string("t1")),
            ("provider", string("claude")),
            ("base_sha", string(initial.base_sha())),
            ("exit", number(0)),
            ("verify_exit", number(0)),
            ("verification", string("passed")),
            ("state", string("review")),
            ("no_change", json::Value::Bool(false)),
            (
                "plan_binding",
                json::parse(&initial.encode().unwrap()).unwrap(),
            ),
            ("artifact", string(".oms/artifacts/result.md")),
            (
                "artifact_sha256",
                string(json::sha(
                    fs::read(r.join(".oms/artifacts/result.md")).unwrap(),
                )),
            ),
            ("patch", string(".oms/artifacts/result.patch")),
            (
                "patch_sha256",
                string(json::sha(
                    fs::read(r.join(".oms/artifacts/result.patch")).unwrap(),
                )),
            ),
        ]);
        // The optional exact Main evidence contract composes with the native
        // delegate capture: path relocation must retain both proofs and hashes.
        let review = if next {
            review
                .with(
                    "expected-review-artifact-sha256",
                    index.text("artifact_sha256"),
                )
                .with("expected-review-patch-sha256", index.text("patch_sha256"))
        } else {
            review
        };
        let mut index_bytes = json::canonical(&index).unwrap();
        index_bytes.push(b'\n');
        fs::write(r.join(".oms/artifacts/index.jsonl"), &index_bytes).unwrap();
        stop.store(true, Ordering::Relaxed);
        assert!(
            plan.execute_bound_task(&review, &running, Some(&index))
                .is_err()
        );
        stop.store(false, Ordering::Relaxed);
        assert_eq!(
            plan.load().unwrap().task("t1").unwrap().text("state"),
            "running"
        );
        for flag in [
            "expected-review-artifact-sha256",
            "expected-review-patch-sha256",
        ] {
            assert!(
                plan.execute_bound_task(
                    &review.clone().with(flag, "0".repeat(64)),
                    &running,
                    Some(&index),
                )
                .unwrap_err()
                .message
                .contains("explicit Review digest differs")
            );
        }
        if !next {
            let oversized = vec![b'A'; oms_command_plan::storage::MAX_JSON + 1];
            fs::write(r.join(".oms/artifacts/result.md"), &oversized).unwrap();
            let mut large_index = index.clone();
            large_index
                .set("artifact_sha256", string(json::sha(&oversized)))
                .unwrap();
            let mut large_index_bytes = json::canonical(&large_index).unwrap();
            large_index_bytes.push(b'\n');
            fs::write(r.join(".oms/artifacts/index.jsonl"), large_index_bytes).unwrap();
            assert!(
                plan.execute_bound_task(&review, &running, Some(&large_index))
                    .unwrap_err()
                    .message
                    .contains("InputTooLarge")
            );
            assert!(!r.join(".oms/plan/delegate-results").exists());
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "running"
            );
            fs::write(r.join(".oms/artifacts/index.jsonl"), &index_bytes).unwrap();
        }
        fs::write(
            r.join(".oms/artifacts/result.md"),
            "changed after verification\n",
        )
        .unwrap();
        assert!(
            plan.execute_bound_task(&review, &running, Some(&index))
                .unwrap_err()
                .message
                .contains("bytes changed")
        );
        fs::write(r.join(".oms/artifacts/result.md"), "Actual native result\n").unwrap();
        plan.execute_bound_task(&review, &running, Some(&index))
            .unwrap();
        assert_eq!(
            plan.load()
                .unwrap()
                .task("t1")
                .unwrap()
                .text("review_lease_id"),
            running.lease_id()
        );
        let retained = plan.load().unwrap().task("t1").unwrap().clone();
        assert!(
            retained
                .text("patch")
                .contains(".oms/plan/delegate-results/")
        );
        assert!(retained.opt("delegate_result").is_some());
        for (field, digest) in [("artifact", "artifact_sha256"), ("patch", "patch_sha256")] {
            let reference = retained.get("review_evidence").get(field);
            assert_eq!(reference.get("path"), retained.get(field));
            assert_eq!(reference.get("sha256"), index.get(digest));
        }
        let direct_finish = Request::new(Action::Finish)
            .with("id", "t1")
            .with("lease-id", running.lease_id())
            .with("landed-commit", initial.base_sha());
        assert!(
            plan.execute(&direct_finish)
                .unwrap_err()
                .message
                .contains("requires patch-land")
        );
        assert_eq!(plan.load().unwrap().task("t1").unwrap(), &retained);
        fs::write(
            r.join(".oms/artifacts/result.md"),
            "original path later changed\n",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(retained.text("artifact")).unwrap(),
            "Actual native result\n"
        );
        assert_eq!(
            plan.execute(&Request::new(Action::EvidenceSnapshot).with("id", "t1"))
                .unwrap()
                .code,
            0,
            "evidence reads follow the retained paths, not the later changed source"
        );
        let land = Request::new(Action::Land)
            .with("id", "t1")
            .with("lease-id", running.lease_id())
            .with("expected-review-lease-id", running.lease_id())
            .with("expected-review-patch", retained.text("patch"))
            .with("expected-review-patch-sha256", index.text("patch_sha256"))
            .with("expected-review-verify", "true")
            .with("expected-review-executor-id", "")
            .with("expected-review-executor-soul-sha256", "");
        let retained_patch = fs::read(retained.text("patch")).unwrap();
        fs::write(retained.text("patch"), "mutated retained patch\n").unwrap();
        assert!(
            plan.execute(&land)
                .unwrap_err()
                .message
                .contains("reviewed patch bytes changed")
        );
        fs::write(retained.text("patch"), &retained_patch).unwrap();
        let retained_index_path = r.join(retained.get("delegate_result").get("index").text("path"));
        let retained_index = fs::read(&retained_index_path).unwrap();
        fs::write(&retained_index_path, "mutated retained index\n").unwrap();
        assert!(
            plan.execute(&land)
                .unwrap_err()
                .message
                .contains("retained native delegate index changed")
        );
        fs::write(&retained_index_path, retained_index).unwrap();
        if scenario == "legacy-finish" {
            plan.execute(&land).unwrap();
            let landing = plan.load().unwrap().task("t1").unwrap().clone();
            assert_eq!(landing.text("state"), "landing");
            let relocated = ".oms/artifacts/landed-delegate.patch";
            let finish = Request::new(Action::Finish)
                .with("id", "t1")
                .with("lease-id", running.lease_id())
                .with("patch", relocated)
                .with(
                    "expected-landing-receipt-sha256",
                    json::receipt_digest(&landing).unwrap(),
                );
            fs::write(r.join(relocated), "different patch\n").unwrap();
            assert!(
                plan.execute(&finish)
                    .unwrap_err()
                    .message
                    .contains("reviewed patch bytes changed")
            );
            assert_eq!(plan.load().unwrap().task("t1").unwrap(), &landing);
            fs::write(r.join(relocated), &retained_patch).unwrap();
            plan.execute(&finish).unwrap();
            let done = plan.load().unwrap().task("t1").unwrap().clone();
            assert_eq!(done.text("state"), "done");
            assert_eq!(done.text("patch"), relocated);
            assert_eq!(done.get("review_evidence"), retained.get("review_evidence"));
            assert_eq!(done.get("delegate_result"), retained.get("delegate_result"));
            plan.execute(&Request::new(Action::EvidenceSnapshot).with("id", "t1"))
                .unwrap();
            continue;
        }
        if next {
            let shown = plan
                .execute(&Request::new(Action::EvidenceSnapshot).with("id", "t1"))
                .unwrap();
            let shown = json::parse(&shown.stdout).unwrap();
            let binding = plan
                .landing_review_fence(&shown, running.base_sha())
                .unwrap();
            plan.check_landing_review(&binding).unwrap();
            if scenario == "stale-admission" {
                publish_contract("2");
                let err = plan.check_landing_review(&binding).unwrap_err();
                assert!(
                    err.message.contains("stale before admission"),
                    "{}",
                    err.message
                );
                assert_eq!(
                    plan.load().unwrap().task("t1").unwrap().text("state"),
                    "review"
                );
                continue;
            }
            let exact = land.clone().with(
                "expected-review-binding",
                String::from_utf8(binding.encode().unwrap()).unwrap(),
            );
            let repair = Request::new(Action::Repair)
                .with("id", "t1")
                .with("lease-id", running.lease_id())
                .with(
                    "expected-review-binding",
                    String::from_utf8(binding.encode().unwrap()).unwrap(),
                );
            let report = fs::read(retained.text("artifact")).unwrap();
            fs::write(
                retained.text("artifact"),
                b"replaced after review preflight\n",
            )
            .unwrap();
            let err = plan.execute(&repair).unwrap_err();
            assert!(
                err.message.contains("reviewed artifact bytes changed"),
                "{}",
                err.message
            );
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "review"
            );
            let err = plan.execute(&exact).unwrap_err();
            assert!(
                err.message.contains("reviewed artifact bytes changed"),
                "{}",
                err.message
            );
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "review"
            );
            fs::write(retained.text("artifact"), report).unwrap();
            let patch = fs::read(retained.text("patch")).unwrap();
            fs::write(retained.text("patch"), b"replaced same-lease patch\n").unwrap();
            let err = plan.execute(&repair).unwrap_err();
            assert!(
                err.message.contains("reviewed patch bytes changed"),
                "{}",
                err.message
            );
            let unchanged = plan.load().unwrap();
            assert_eq!(unchanged.task("t1").unwrap().text("state"), "review");
            assert!(unchanged.task("t1").unwrap().get("repair_count").is_int(0));
            fs::write(retained.text("patch"), patch).unwrap();
            plan.execute(&exact).unwrap();
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "landing"
            );
            let landing = plan.load().unwrap().task("t1").unwrap().clone();
            let release = Request::new(Action::Release)
                .with("id", "t1")
                .with("lease-id", running.lease_id());
            assert!(
                plan.execute(&release)
                    .unwrap_err()
                    .message
                    .contains("expected-landing-receipt-sha256")
            );
            let release = release.with(
                "expected-landing-receipt-sha256",
                json::receipt_digest(&landing).unwrap(),
            );
            assert!(
                plan.execute(&release)
                    .unwrap_err()
                    .message
                    .contains("exact review binding")
            );
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "landing"
            );
            if scenario == "release" {
                plan.execute(&release.with(
                    "expected-review-binding",
                    String::from_utf8(binding.encode().unwrap()).unwrap(),
                ))
                .unwrap();
                let released = plan.load().unwrap();
                let task = released.task("t1").unwrap();
                assert_eq!(task.text("state"), "ready");
                assert!(task.opt("landing_review_sha256").is_none());
                continue;
            }
            let mut done = landing.clone();
            let frozen = r.join(".oms/artifacts/frozen.patch");
            fs::write(&frozen, fs::read(retained.text("patch")).unwrap()).unwrap();
            done.set("patch", string(frozen.to_str().unwrap())).unwrap();
            let finish = Request::new(Action::Finish)
                .with("id", "t1")
                .with("lease-id", running.lease_id())
                .with("patch", frozen.as_os_str())
                .with(
                    "expected-landing-receipt-sha256",
                    json::receipt_digest(&landing).unwrap(),
                );
            assert!(
                plan.execute(&finish)
                    .unwrap_err()
                    .message
                    .contains("exact review binding")
            );
            let finish = finish
                .with(
                    "expected-review-binding",
                    String::from_utf8(binding.encode().unwrap()).unwrap(),
                )
                .with(
                    "expected-landing-done-receipt-sha256",
                    json::receipt_digest(&done).unwrap(),
                );
            let report = fs::read(retained.text("artifact")).unwrap();
            fs::write(retained.text("artifact"), b"changed while landing\n").unwrap();
            assert!(
                plan.execute(&finish)
                    .unwrap_err()
                    .message
                    .contains("reviewed artifact bytes changed")
            );
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "landing"
            );
            fs::write(retained.text("artifact"), report).unwrap();
            plan.execute(&finish).unwrap();
            let completed = plan.load().unwrap();
            assert!(
                plan.check_landing_done(&completed, "t1", None, index.text("patch_sha256"))
                    .is_err()
            );
            plan.check_landing_done(&completed, "t1", Some(&binding), index.text("patch_sha256"))
                .unwrap();
            continue;
        }
        if next {
            let shown = plan
                .execute(&Request::new(Action::EvidenceSnapshot).with("id", "t1"))
                .unwrap();
            let shown = json::parse(&shown.stdout).unwrap();
            let binding = plan
                .landing_review_fence(&shown, running.base_sha())
                .unwrap();
            plan.check_landing_review(&binding).unwrap();
            if scenario == "stale-admission" {
                publish_contract("2");
                let err = plan.check_landing_review(&binding).unwrap_err();
                assert!(
                    err.message.contains("stale before admission"),
                    "{}",
                    err.message
                );
                assert_eq!(
                    plan.load().unwrap().task("t1").unwrap().text("state"),
                    "review"
                );
                continue;
            }
            let exact = land.clone().with(
                "expected-review-binding",
                String::from_utf8(binding.encode().unwrap()).unwrap(),
            );
            let repair = Request::new(Action::Repair)
                .with("id", "t1")
                .with("lease-id", running.lease_id())
                .with(
                    "expected-review-binding",
                    String::from_utf8(binding.encode().unwrap()).unwrap(),
                );
            let report = fs::read(retained.text("artifact")).unwrap();
            fs::write(
                retained.text("artifact"),
                b"replaced after review preflight\n",
            )
            .unwrap();
            let err = plan.execute(&repair).unwrap_err();
            assert!(
                err.message
                    .contains("Plan landing review, report, patch or base changed"),
                "{}",
                err.message
            );
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "review"
            );
            let err = plan.execute(&exact).unwrap_err();
            assert!(
                err.message
                    .contains("Plan landing review, report, patch or base changed"),
                "{}",
                err.message
            );
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "review"
            );
            fs::write(retained.text("artifact"), report).unwrap();
            let patch = fs::read(retained.text("patch")).unwrap();
            fs::write(retained.text("patch"), b"replaced same-lease patch\n").unwrap();
            let err = plan.execute(&repair).unwrap_err();
            assert!(
                err.message
                    .contains("Plan landing review, report, patch or base changed"),
                "{}",
                err.message
            );
            let unchanged = plan.load().unwrap();
            assert_eq!(unchanged.task("t1").unwrap().text("state"), "review");
            assert!(unchanged.task("t1").unwrap().get("repair_count").is_int(0));
            fs::write(retained.text("patch"), patch).unwrap();
            plan.execute(&exact).unwrap();
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "landing"
            );
            let landing = plan.load().unwrap().task("t1").unwrap().clone();
            let release = Request::new(Action::Release)
                .with("id", "t1")
                .with("lease-id", running.lease_id());
            assert!(
                plan.execute(&release)
                    .unwrap_err()
                    .message
                    .contains("expected-landing-receipt-sha256")
            );
            let release = release.with(
                "expected-landing-receipt-sha256",
                json::receipt_digest(&landing).unwrap(),
            );
            assert!(
                plan.execute(&release)
                    .unwrap_err()
                    .message
                    .contains("exact review binding")
            );
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "landing"
            );
            if scenario == "release" {
                plan.execute(&release.with(
                    "expected-review-binding",
                    String::from_utf8(binding.encode().unwrap()).unwrap(),
                ))
                .unwrap();
                let released = plan.load().unwrap();
                let task = released.task("t1").unwrap();
                assert_eq!(task.text("state"), "ready");
                assert!(task.opt("landing_review_sha256").is_none());
                continue;
            }
            let mut done = landing.clone();
            let frozen = r.join(".oms/artifacts/frozen.patch");
            fs::write(&frozen, fs::read(retained.text("patch")).unwrap()).unwrap();
            done.set("patch", string(frozen.to_str().unwrap())).unwrap();
            let finish = Request::new(Action::Finish)
                .with("id", "t1")
                .with("lease-id", running.lease_id())
                .with("patch", frozen.as_os_str())
                .with(
                    "expected-landing-receipt-sha256",
                    json::receipt_digest(&landing).unwrap(),
                );
            assert!(
                plan.execute(&finish)
                    .unwrap_err()
                    .message
                    .contains("exact review binding")
            );
            let finish = finish
                .with(
                    "expected-review-binding",
                    String::from_utf8(binding.encode().unwrap()).unwrap(),
                )
                .with(
                    "expected-landing-done-receipt-sha256",
                    json::receipt_digest(&done).unwrap(),
                );
            let report = fs::read(retained.text("artifact")).unwrap();
            fs::write(retained.text("artifact"), b"changed while landing\n").unwrap();
            assert!(
                plan.execute(&finish)
                    .unwrap_err()
                    .message
                    .contains("Finish evidence")
            );
            assert_eq!(
                plan.load().unwrap().task("t1").unwrap().text("state"),
                "landing"
            );
            fs::write(retained.text("artifact"), report).unwrap();
            plan.execute(&finish).unwrap();
            let completed = plan.load().unwrap();
            assert!(
                plan.check_landing_done(&completed, "t1", None, index.text("patch_sha256"))
                    .is_err()
            );
            plan.check_landing_done(&completed, "t1", Some(&binding), index.text("patch_sha256"))
                .unwrap();
            continue;
        }
        plan.execute(
            &Request::new(Action::Repair)
                .with("id", "t1")
                .with("lease-id", running.lease_id())
                .with("artifact", ".oms/artifacts/result.md"),
        )
        .unwrap();
        let repair = json::parse(
            &plan
                .execute(&Request::new(Action::EvidenceSnapshot).with("id", "t1"))
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert!(repair.get("repair_count").is_int(1));
        let repair_running = plan
            .execute_bound_task(&start, &bind(&repair), None)
            .unwrap();
        plan.execute(
            &Request::new(Action::Release)
                .with("id", "t1")
                .with("lease-id", repair_running.lease_id()),
        )
        .unwrap();
        let replacement = issue();
        assert_ne!(replacement.text("lease_id"), repair_running.lease_id());
        assert!(
            plan.execute_bound_task(&review, &repair_running, Some(&index))
                .is_err()
        );
        let after = plan.load().unwrap();
        assert_eq!(
            after.task("t1").unwrap().text("lease_id"),
            replacement.text("lease_id")
        );
        assert_eq!(after.task("t1").unwrap().text("state"), "claimed");
        assert!(
            fs::read_to_string(r.join(".oms/plan/late-results.jsonl"))
                .unwrap()
                .contains("stale_delegate_binding")
        );
        // A real owner amendment after releasing the old lease cannot be
        // mistaken for a continuation of the captured scope.
        plan.execute(
            &Request::new(Action::Release)
                .with("id", "t1")
                .with("lease-id", replacement.text("lease_id")),
        )
        .unwrap();
        let ready = plan.load().unwrap();
        plan.execute(
            &Request::new(Action::Amend)
                .with("id", "t1")
                .with(
                    "expected-plan-sha256",
                    json::sha(ready.raw.as_ref().unwrap()),
                )
                .with(
                    "expected-task-sha256",
                    json::hash(ready.task("t1").unwrap()).unwrap(),
                )
                .with("allowed", "other.txt")
                .with("reason", "change the authorized scope"),
        )
        .unwrap();
        assert!(plan.check_delegate_binding(&repair_running).is_err());
        let ready = plan.load().unwrap();
        let decision = b"Owner cancelled this fixture task.\n";
        fs::write(r.join(".oms/plan/cancel.txt"), decision).unwrap();
        plan.execute(
            &Request::new(Action::Cancel)
                .with("id", "t1")
                .with(
                    "expected-plan-sha256",
                    json::sha(ready.raw.as_ref().unwrap()),
                )
                .with(
                    "expected-task-sha256",
                    json::hash(ready.task("t1").unwrap()).unwrap(),
                )
                .with("expected-state", "ready")
                .with("expected-lease-id", "none")
                .with("operator-consent", "cancel")
                .with("reason", "cancel this fixture")
                .with("decision-artifact", ".oms/plan/cancel.txt")
                .with("expected-decision-artifact-sha256", json::sha(decision)),
        )
        .unwrap();
        assert_eq!(
            plan.load().unwrap().task("t1").unwrap().text("state"),
            "cancelled"
        );
        assert!(plan.check_delegate_binding(&repair_running).is_err());
    }
    // The native fence samples actual time inside its lock, even when a caller
    // accidentally retains the Context used when the lease was issued.
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let stop = AtomicBool::new(false);
    let mut context =
        Context::from_environment_at(&Request::new(Action::Status), &r, &r, None).unwrap();
    context.ts = "2000-01-01T00:00:00Z".into();
    let plan = Plan::open(context, &Host, &stop).unwrap();
    plan.execute(&Request::new(Action::Init).with("goal", "expired lease"))
        .unwrap();
    plan.execute(
        &Request::new(Action::Add)
            .with("id", "t1")
            .with("title", "write a.txt")
            .with("allowed", "a.txt")
            .with("verify", "true"),
    )
    .unwrap();
    let issued = json::parse(
        &plan
            .execute(
                &Request::new(Action::Claim)
                    .with("id", "t1")
                    .with("provider", "claude")
                    .with("ttl", "1")
                    .flag("json"),
            )
            .unwrap()
            .stdout,
    )
    .unwrap();
    let binding = DelegateBinding::parse(
        &json::canonical(&object([
            ("schema", number(1)),
            ("plan_id", issued.get("plan_id").clone()),
            ("task_id", string("t1")),
            ("task_sha256", issued.get("task_sha256").clone()),
            ("lease_id", issued.get("lease_id").clone()),
            ("lease_epoch", issued.get("lease_epoch").clone()),
            (
                "base_sha",
                string(plan.git().text(&["rev-parse", "HEAD"]).unwrap()),
            ),
        ]))
        .unwrap(),
    )
    .unwrap();
    assert!(
        plan.check_delegate_binding(&binding)
            .unwrap_err()
            .message
            .contains("expired")
    );
}

#[test]
fn help_and_parse_errors() {
    if isolated("help_and_parse_errors") {
        return;
    }
    let tmp = Tmp::new();
    for a in [&["--help"][..], &["-h"], &["list", "--help"]] {
        let (c, o, e) = go(&tmp.0, a);
        assert_eq!((c, e.as_str()), (0, ""), "{a:?}");
        assert!(o.starts_with("Usage: agent-plan.sh"), "{a:?}");
    }
    // `--goal` consumes `-h` as its operand, exactly like the shell parser.
    assert_eq!(go(&tmp.0, &["--goal", "-h"]).0, 2);
    // Options only (or nothing at all): usage on stderr, exit 2, empty stdout.
    for a in [&[][..], &["--json"], &["--repo", "x", "--check"]] {
        let (c, o, e) = go(&tmp.0, a);
        assert_eq!((c, o.as_str()), (2, ""), "{a:?}");
        assert!(e.starts_with("Usage:"), "{a:?}: {e}");
    }
    for a in [
        &["--bogus", "-h"][..],
        &["nonsense"],
        &["nonsense", "--help"],
        &["-x", "--help"],
        &["list", "show", "--help"],
        &["--priority", "abc", "--help"],
        &["--ttl", "abc", "--help"],
        &["--ttl", "", "--help"],
        &["--ttl", "99999999999999999999999", "--help"],
        &["--limit", "0", "--help"],
        &["--limit", "-1", "--help"],
        &["--max-tasks", "0", "--help"],
        &["--max-tasks", "13", "--help"],
        &["--landed-commit", "", "--help"],
        &["--depends", "a", "--depends", "b", "--help"],
        &["--allowed", "a", "--allowed", "b", "--help"],
        &["--forbidden", "a", "--forbidden", "b", "--help"],
        &["list", "show"],
        &["--repo"],
    ] {
        let (c, o, e) = go(&tmp.0, a);
        assert_eq!((c, o.as_str()), (2, ""), "{a:?}");
        assert!(e.starts_with("error: "), "{a:?}: {e}");
    }
}

#[test]
fn interrupted_before_work_fails_closed() {
    if isolated("interrupted_before_work_fails_closed") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let stop = AtomicBool::new(true);
    let (c, o, e) = run(
        &args(&["--repo", r.to_str().unwrap(), "list"]),
        &tmp.0,
        &stop,
    );
    assert_eq!((c, o.is_empty()), (2, true));
    assert!(String::from_utf8(e).unwrap().contains("interrupted"));
    // Interruption wins over path/Git work: a missing --repo is not reached.
    let (c, o, e) = run(&args(&["--repo", "missing", "list"]), &tmp.0, &stop);
    assert_eq!((c, o.is_empty()), (2, true));
    assert_eq!(String::from_utf8(e).unwrap(), "error: interrupted\n");
}

#[test]
fn lifecycle_from_unrelated_cwd() {
    if isolated("lifecycle_from_unrelated_cwd") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let other = tmp.0.join("elsewhere");
    fs::create_dir_all(&other).unwrap();
    let rs = r.to_str().unwrap();
    let base = |rest: &[&str]| {
        let mut v = vec!["--repo", rs];
        v.extend_from_slice(rest);
        go(&other, &v)
    };
    let (c, _, e) = base(&["init", "--goal", "ship it"]);
    assert_eq!(c, 0, "{e}");
    assert!(r.join(".oms/plan").exists());
    assert!(
        !other.join(".oms").exists(),
        "state must follow --repo, not cwd"
    );
    let (c, _, e) = base(&["add", "--id", "t1", "--title", "first", "--verify", "true"]);
    assert_eq!(c, 0, "{e}");
    let (c, o, e) = base(&["show", "--id", "t1"]);
    assert_eq!(c, 0, "{e}");
    assert!(o.contains("\"t1\""), "{o}");
    let (c, o, _) = base(&["list"]);
    assert_eq!(c, 0);
    assert!(o.contains("t1"));
    let (c, o, _) = base(&["ready"]);
    assert_eq!(c, 0);
    assert!(o.contains("t1"));
    let (c, o, e) = base(&["show", "--id", "missing"]);
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.starts_with("error: "));
    let (c, o, e) = base(&["retire", "--check"]);
    assert_eq!(c, 0, "{e}");
    assert!(!o.is_empty());
    assert!(!other.join(".oms").exists());
}

#[test]
fn relative_proposal_and_file_resolve_against_cwd_arg() {
    if isolated("relative_proposal_and_file_resolve_against_cwd_arg") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    // Relative to the supplied cwd, not the process cwd: a missing proposal fails
    // with a plain refusal rather than reading anything from the test process dir.
    let (c, o, e) = go(
        &r,
        &["--repo", rs, "apply-proposal", "--proposal", "nope.json"],
    );
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.starts_with("error: "), "{e}");
    // --file is cwd-relative: it lands under the supplied cwd, never the process cwd.
    let work = tmp.0.join("work");
    fs::create_dir_all(work.join("rel")).unwrap();
    let (c, _, e) = go(
        &work,
        &[
            "--repo",
            rs,
            "--file",
            "rel/plan.json",
            "init",
            "--goal",
            "g",
        ],
    );
    assert_eq!(c, 0, "{e}");
    assert!(work.join("rel/plan.json").exists());
}

#[test]
fn unavailable_services_fail_closed_with_io_split() {
    if isolated("unavailable_services_fail_closed_with_io_split") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(
        go(
            &tmp.0,
            &["--repo", rs, "init", "--goal", "g", "--accept", "true"]
        )
        .0,
        0
    );
    let lint = |verify: &str| {
        let a = args(&[
            "--repo",
            rs,
            "lint-verify",
            "--verify",
            verify,
            "--allowed",
            "a.rs",
        ]);
        let stop = AtomicBool::new(false);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let c = run_with_io(&a, &tmp.0, &stop, &UnavailableServices, &mut out, &mut err);
        let (c2, o2, e2) = run_with_services(&a, &tmp.0, &stop, &UnavailableServices);
        assert_eq!((c, &out, &err), (c2, &o2, &e2));
        (
            c,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    // A floor hit is typed output on stdout with empty stderr.
    let (c, o, e) = lint("cat a.rs");
    assert_eq!((c, e.as_str()), (2, ""));
    assert!(o.starts_with("floor_incompatible_verifier: a.rs"), "{o}");
    let (c, o, e) = lint("true");
    assert_eq!(
        (c, o.starts_with("lint-verify: ok"), e.as_str()),
        (0, true, "")
    );
    // Service-backed actions refuse: exit 2, empty stdout, `error:` on stderr.
    let (c, o, e) = go_with(&UnavailableServices, &tmp.0, &["--repo", rs, "accept"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // Provider normalisation is a native service: unknown names are refused on claim.
    assert_eq!(
        go(&tmp.0, &["--repo", rs, "add", "--id", "t1", "--title", "x"]).0,
        0
    );
    let (c, o, e) = go_with(
        &UnavailableServices,
        &tmp.0,
        &["--repo", rs, "claim", "--id", "t1", "--provider", "bogus"],
    );
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // retro is read-only and works on an initialised plan; an empty repo refuses.
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "retro"]);
    assert_eq!(c, 0, "{e}");
    assert!(!o.is_empty());
    let empty = repo_at(&tmp, "empty");
    let (c, o, e) = go(&tmp.0, &["--repo", empty.to_str().unwrap(), "retro"]);
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.contains("no readable plan"), "{e}");
}

#[test]
fn claim_block_from_unrelated_cwd() {
    if isolated("claim_block_from_unrelated_cwd") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    let (c, _, e) = go(
        &tmp.0,
        &[
            "--repo", rs, "add", "--id", "t1", "--title", "x", "--verify", "true",
        ],
    );
    assert_eq!(c, 0, "{e}");
    for step in [
        &["claim", "--id", "t1", "--provider", "codex"][..],
        &["block", "--id", "t1", "--reason", "waiting"],
        &["show", "--id", "t1"],
    ] {
        let mut v = vec!["--repo", rs];
        v.extend_from_slice(step);
        let (c, _, e) = go(&tmp.0, &v);
        assert_eq!(c, 0, "{step:?}: {e}");
    }
    let (_, o, _) = go(&tmp.0, &["--repo", rs, "show", "--id", "t1"]);
    assert!(o.contains("blocked"), "{o}");
    // A task already blocked cannot be claimed again: refused, state unchanged.
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "claim", "--id", "t1"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
}

const OWNER: &str = "owner_0123456789abcdef0123456789abcdef";

#[test]
fn repo_subdirectory_resolves_to_worktree_root() {
    if isolated("repo_subdirectory_resolves_to_worktree_root") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let sub = r.join("src");
    fs::create_dir_all(&sub).unwrap();
    let subs = sub.to_str().unwrap();
    // Without --repo the cwd itself is the default and still normalises.
    assert_eq!(go(&sub, &["init", "--goal", "g"]).0, 0);
    assert!(r.join(".oms/plan").exists() && !sub.join(".oms").exists());
    let (c, _, e) = go(
        &tmp.0,
        &["--repo", subs, "add", "--id", "t1", "--title", "x"],
    );
    assert_eq!(c, 0, "{e}");
    for step in [&["list"][..], &["ready"], &["show", "--id", "t1"]] {
        let (c, o, e) = go(&sub, step);
        assert_eq!(c, 0, "{step:?}: {e}");
        assert!(o.contains("t1"), "{step:?}: {o}");
        let mut v = vec!["--repo", subs];
        v.extend_from_slice(step);
        let (c, o, e) = go(&tmp.0, &v);
        assert_eq!(c, 0, "{v:?}: {e}");
        assert!(o.contains("t1"), "{v:?}: {o}");
    }
    assert!(!sub.join(".oms").exists());
    // Non-git directories resolve to themselves; missing ones are refused.
    let plain = tmp.0.join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert_eq!(go(&plain, &["init", "--goal", "g"]).0, 0);
    assert!(plain.join(".oms/plan").exists());
    let (c, o, e) = go(&tmp.0, &["--repo", "missing", "list"]);
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.starts_with("error: "));
}

/// Child half of `state_repo_env_defaults_the_repository`; it needs
/// OMS_STATE_REPO in its environment, which the parent test supplies.
#[test]
#[ignore = "driven by state_repo_env_defaults_the_repository"]
fn state_env_child() {
    let tmp = PathBuf::from(std::env::var_os("CLI_TEST_TMP").unwrap());
    let work = tmp.join("work");
    let (c, _, e) = go(&work, &["init", "--goal", "g"]);
    assert_eq!(c, 0, "{e}");
    let (c, o, e) = go(&work, &["add", "--id", "t1", "--title", "x"]);
    assert_eq!(c, 0, "{e}: {o}");
    let (c, o, e) = go(&work, &["list"]);
    assert_eq!(c, 0, "{e}");
    assert!(o.contains("t1"));
    assert!(tmp.join("repo/.oms/plan").exists());
    assert!(!work.join(".oms").exists());
    assert!(!tmp.join("repo/src/.oms").exists());
}

#[test]
fn state_repo_env_defaults_the_repository() {
    if isolated("state_repo_env_defaults_the_repository") {
        return;
    }
    // Absolute, and relative to the supplied cwd (never the process cwd).
    for value in ["ABS", "../repo", "ABSSUB", "../repo/src"] {
        let tmp = Tmp::new();
        let r = repo(&tmp);
        fs::create_dir_all(tmp.0.join("work")).unwrap();
        fs::create_dir_all(r.join("src")).unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "state_env_child",
            "--ignored",
            "--test-threads=1",
        ]);
        cmd.env("CLI_TEST_TMP", &tmp.0);
        cmd.env(
            "OMS_STATE_REPO",
            match value {
                "ABS" => r.to_str().unwrap().to_owned(),
                "ABSSUB" => r.join("src").to_str().unwrap().to_owned(),
                v => v.to_owned(),
            },
        );
        let out = cmd.output().unwrap();
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "{value}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn services_state_root_hint_places_shared_plan() {
    if isolated("services_state_root_hint_places_shared_plan") {
        return;
    }
    struct Scratch(PathBuf);
    impl Services for Scratch {
        fn group_membership_resolver(&self) -> Option<GroupResolver> {
            Host.group_membership_resolver()
        }
        fn state_root(&self, _: &Path) -> oms_command_plan::Result<Option<PathBuf>> {
            Ok(Some(self.0.clone()))
        }
    }
    let tmp = Tmp::new();
    let main = repo_at(&tmp, "main");
    let scratch = repo_at(&tmp, "scratch");
    let svc = Scratch(main.clone());
    let sc = scratch.to_str().unwrap();
    let (c, _, e) = go_with(&svc, &tmp.0, &["--repo", sc, "init", "--goal", "g"]);
    assert_eq!(c, 0, "{e}");
    assert!(main.join(".oms/plan").exists() && !scratch.join(".oms").exists());
}

#[test]
fn symlinked_cwd_markers_dir_and_file() {
    if isolated("symlinked_cwd_markers_dir_and_file") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    let link = tmp.0.join("link");
    std::os::unix::fs::symlink(&r, &link).unwrap();
    for dir in [
        ".oms/delegations",
        "./.oms/delegations",
        "../repo/.oms/delegations",
    ] {
        let (c, o, e) = go(
            &link,
            &[
                "--repo",
                rs,
                "recover-owner",
                "--owner-id",
                OWNER,
                "--markers-dir",
                dir,
            ],
        );
        assert_eq!(
            (c, o.as_str()),
            (0, "plan: recovered (none)\n"),
            "{dir}: {e}"
        );
    }
    let (c, o, e) = go(
        &link,
        &[
            "--repo",
            rs,
            "recover-owner",
            "--owner-id",
            OWNER,
            "--markers-dir",
            "other",
        ],
    );
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.contains("repo-local"), "{e}");
    // --file under a symlinked cwd whose directory does not exist yet.
    let file = r.join("newdir/plan.json");
    let (c, _, e) = go(&link, &["--repo", rs, "--file", "newdir/plan.json", "list"]);
    assert_eq!(c, 0, "{e}");
    let (c, o, e) = go(
        &link,
        &[
            "--repo",
            rs,
            "--file",
            "newdir/plan.json",
            "init",
            "--goal",
            "g",
        ],
    );
    assert_eq!(c, 0, "{e}");
    assert!(o.contains(file.to_str().unwrap()), "{o}");
    assert!(file.exists());
}

/// Child half of the ambient-GIT_* test: it runs with GIT_DIR/GIT_WORK_TREE
/// pointing at another repository.
#[test]
#[ignore = "driven by ambient_git_environment_cannot_hijack_a_plain_directory"]
fn ambient_git_child() {
    let tmp = PathBuf::from(std::env::var_os("CLI_TEST_TMP").unwrap());
    let plain = tmp.join("plain");
    let (c, _, e) = go(
        &tmp,
        &["--repo", plain.to_str().unwrap(), "init", "--goal", "g"],
    );
    assert_eq!(c, 0, "{e}");
    assert!(plain.join(".oms/plan").exists());
    assert!(!tmp.join("other/.oms").exists());
}

#[test]
fn ambient_git_environment_cannot_hijack_a_plain_directory() {
    if isolated("ambient_git_environment_cannot_hijack_a_plain_directory") {
        return;
    }
    let tmp = Tmp::new();
    let other = repo_at(&tmp, "other");
    fs::create_dir_all(tmp.0.join("plain")).unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ambient_git_child",
            "--ignored",
            "--test-threads=1",
        ])
        .env("CLI_TEST_TMP", &tmp.0)
        .env("GIT_DIR", other.join(".git"))
        .env("GIT_WORK_TREE", &other)
        .output()
        .unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn symlinked_leaves_and_plan_directory_fail_closed() {
    if isolated("symlinked_leaves_and_plan_directory_fail_closed") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    let real = tmp.0.join("real.json");
    fs::copy(r.join(".oms/plan/tasks.json"), &real).unwrap();
    // --file leaf is a symlink to a valid plan: never dereferenced.
    let leaf = tmp.0.join("leaf.json");
    std::os::unix::fs::symlink(&real, &leaf).unwrap();
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "--file", "leaf.json", "list"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // --proposal leaf is a symlink.
    let (c, o, e) = go(
        &tmp.0,
        &["--repo", rs, "apply-proposal", "--proposal", "leaf.json"],
    );
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    // The default plan path: tasks.json as a symlink.
    let tasks = r.join(".oms/plan/tasks.json");
    fs::remove_file(&tasks).unwrap();
    std::os::unix::fs::symlink(&real, &tasks).unwrap();
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "list"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    // `.oms/plan` itself is a symlink.
    fs::remove_file(&tasks).unwrap();
    let moved = tmp.0.join("moved-plan");
    fs::rename(r.join(".oms/plan"), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, r.join(".oms/plan")).unwrap();
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "list"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // A symlinked markers directory is refused rather than dereferenced.
    fs::remove_file(r.join(".oms/plan")).unwrap();
    fs::rename(&moved, r.join(".oms/plan")).unwrap();
    fs::create_dir_all(tmp.0.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(tmp.0.join("elsewhere"), r.join(".oms/delegations")).unwrap();
    let (c, o, e) = go(
        &tmp.0,
        &[
            "--repo",
            rs,
            "recover-owner",
            "--owner-id",
            OWNER,
            "--markers-dir",
            r.join(".oms/delegations").to_str().unwrap(),
        ],
    );
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
}

#[test]
fn parent_traversal_stops_at_the_root() {
    if isolated("parent_traversal_stops_at_the_root") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    // Far more `..` than the cwd has components; the process cwd differs.
    let up = "../".repeat(64);
    let rel = format!("{up}{}", r.strip_prefix("/").unwrap().display());
    let (c, _, e) = go(&tmp.0, &["--repo", &rel, "init", "--goal", "g"]);
    assert_eq!(c, 0, "{e}");
    assert!(r.join(".oms/plan").exists());
    let shallow = Path::new("/");
    let (c, _, e) = go(
        shallow,
        &[
            "--repo",
            r.strip_prefix("/").unwrap().to_str().unwrap(),
            "list",
        ],
    );
    assert_eq!(c, 0, "{e}");
}

#[test]
fn context_and_execute_agree_when_cwd_is_not_the_process_cwd() {
    if isolated("context_and_execute_agree_when_cwd_is_not_the_process_cwd") {
        return;
    }
    use oms_command_plan::{Context, Plan, Request};
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    let target = tmp.0.join("target");
    fs::create_dir_all(r.join("src")).unwrap();
    fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, tmp.0.join("link")).unwrap();
    let stop = AtomicBool::new(false);
    let req = Request::parse(&args(&[
        "--repo",
        "repo/src/..",
        "--file",
        "link/a/b/plan.json",
        "list",
    ]))
    .unwrap();
    let ctx = Context::from_environment_at(&req, &tmp.0, &tmp.0, None).unwrap();
    assert_eq!(ctx.cwd, tmp.0);
    assert_eq!(ctx.execution_repo, r);
    assert_eq!(ctx.plan_file, target.join("a/b/plan.json"));
    let resp = Plan::open(ctx, &Host, &stop)
        .unwrap()
        .execute(&req)
        .unwrap();
    assert_eq!(resp.code, 0);
    // A different spelling of another repository is refused, not trusted.
    let other = repo_at(&tmp, "other");
    let mut bad = req.clone();
    bad.repo = Some(other);
    let ctx = Context::from_environment_at(&req, &tmp.0, &tmp.0, None).unwrap();
    let err = Plan::open(ctx, &Host, &stop)
        .unwrap()
        .execute(&bad)
        .unwrap_err();
    assert!(
        err.message.contains("differs from context"),
        "{}",
        err.message
    );
    // A symlink followed by `..` steps out of the physical target, not the
    // spelling: /A/link -> /B/sub, so /A/link/.. is /B.
    let a = tmp.0.join("A");
    let b = repo_at(&tmp, "B");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(b.join("sub")).unwrap();
    std::os::unix::fs::symlink(b.join("sub"), a.join("link")).unwrap();
    let a_s = a.to_str().unwrap();
    let via = format!("{a_s}/link/..");
    let req = Request::parse(&args(&[
        "--repo",
        &via,
        "--file",
        &format!("{a_s}/link/../plan.json"),
        "list",
    ]))
    .unwrap();
    let ctx = Context::from_environment_at(&req, &tmp.0, &tmp.0, None).unwrap();
    assert_eq!(ctx.execution_repo, b);
    assert_eq!(ctx.plan_file, b.join("plan.json"));
    assert_eq!(
        Plan::open(ctx, &Host, &stop)
            .unwrap()
            .execute(&req)
            .unwrap()
            .code,
        0
    );
    fs::create_dir_all(b.join("newdir")).unwrap();
    for file in ["A/link/../plan.json", "A/link/../newdir/../plan.json"] {
        let req = Request::parse(&args(&["--repo", "A/link/..", "--file", file, "list"])).unwrap();
        let ctx = Context::from_environment_at(&req, &tmp.0, &tmp.0, None).unwrap();
        assert_eq!(
            (ctx.execution_repo.as_path(), ctx.plan_file.as_path()),
            (b.as_path(), b.join("plan.json").as_path()),
            "{file}"
        );
        assert_eq!(
            Plan::open(ctx, &Host, &stop)
                .unwrap()
                .execute(&req)
                .unwrap()
                .code,
            0
        );
    }
    // End to end: state lands in B, never in A.
    let (c, _, e) = go(&tmp.0, &["--repo", &via, "init", "--goal", "g"]);
    assert_eq!(c, 0, "{e}");
    assert!(b.join(".oms/plan").exists() && !a.join(".oms").exists());
    let (c, o, e) = go(
        &tmp.0,
        &[
            "--repo",
            &via,
            "recover-owner",
            "--owner-id",
            OWNER,
            "--markers-dir",
            "A/link/../.oms/delegations",
        ],
    );
    assert_eq!((c, o.as_str()), (0, "plan: recovered (none)\n"), "{e}");
    // A symlinked leaf reached through `link/..` is still not followed.
    fs::copy(b.join(".oms/plan/tasks.json"), tmp.0.join("real.json")).unwrap();
    std::os::unix::fs::symlink(tmp.0.join("real.json"), b.join("leaf.json")).unwrap();
    let (c, o, e) = go(
        &tmp.0,
        &["--repo", &via, "--file", "A/link/../leaf.json", "list"],
    );
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // `..` still clamps at the root: `/..` stays `/`.
    let (c, _, e) = go(
        Path::new("/"),
        &["--repo", &format!("../../..{}", r.display()), "list"],
    );
    assert_eq!(c, 0, "{e}");
}

#[test]
fn invalid_parent_traversal_and_relative_cwd() {
    if isolated("invalid_parent_traversal_and_relative_cwd") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    fs::write(r.join("file"), "x").unwrap();
    std::os::unix::fs::symlink(r.join("absent"), r.join("dangling")).unwrap();
    for part in ["file", "dangling", "missing"] {
        let path = format!("{part}/..");
        assert_ne!(go(&r, &["--repo", &path, "list"]).0, 0, "{part}");
    }
    let process = std::env::current_dir().unwrap();
    assert_eq!(
        go(
            Path::new("."),
            &["--repo", process.to_str().unwrap(), "list"]
        )
        .0,
        0
    );
    assert_ne!(go(Path::new("missing-cwd-for-plan"), &["list"]).0, 0);
}

#[test]
fn merged_dependencies_scheduling_and_late_results() {
    if isolated("merged_dependencies_scheduling_and_late_results") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    let run = |rest: &[&str]| {
        let mut command = vec!["--repo", rs];
        command.extend_from_slice(rest);
        go(&tmp.0, &command)
    };
    assert_eq!(run(&["init", "--goal", "g"]).0, 0);
    let (code, _, err) = run(&[
        "add",
        "--id",
        "bad",
        "--title",
        "bad",
        "--depends",
        "missing",
    ]);
    assert_eq!(code, 2);
    assert!(err.contains("missing"), "{err}");
    let (code, _, err) = run(&["add", "--id", "self", "--title", "bad", "--depends", "self"]);
    assert_eq!(code, 2);
    assert!(err.contains("cycle"), "{err}");
    let brief_path = tmp.0.join("scheduler-brief.txt");
    fs::write(&brief_path, "Perform the scoped scheduler fixture work").unwrap();
    for (id, priority) in [("a", "-1"), ("b", "+2")] {
        let (code, _, err) = run(&[
            "add",
            "--id",
            id,
            "--title",
            id,
            "--allowed",
            id,
            "--priority",
            priority,
            "--resources",
            "gpu,gpu",
            "--brief-file",
            brief_path.to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "{err}");
    }
    let (code, out, err) = run(&["ready"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "b\na\n");
    let (code, out, err) = run(&["ready", "--json", "--max-writers", "0"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"capacity_full\": true"), "{out}");
    assert_eq!(run(&["next", "--max-review-backlog", "0"]).0, 3);
    let (code, _, err) = run(&["claim", "--id", "a", "--provider", "codex"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = run(&["ready"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.is_empty(), "shared resource should block b: {out}");
    let (code, out, err) = run(&["status", "--json"]);
    assert_eq!(code, 0, "{err}");
    let status = oms_command_plan::json::parse(out.as_bytes()).unwrap();
    let blocked = status
        .get("task_readiness")
        .array()
        .unwrap()
        .iter()
        .find(|row| row.text("id") == "b")
        .unwrap();
    assert_eq!(blocked.text("reason"), "scope-conflict");
    assert_eq!(
        blocked.get("selection_eligible"),
        &oms_command_plan::json::Value::Bool(false)
    );
    // `ready` reports scheduling inputs; claimed `a` still filters `b`.
    for (id, extra) in [
        ("c", &["--role", "builder", "--resources", "cpu,mem"][..]),
        ("d", &[]),
    ] {
        let mut add = vec!["add", "--id", id, "--title", id, "--allowed", id];
        add.extend_from_slice(extra);
        assert_eq!(run(&add).0, 0);
    }
    let (code, out, err) = run(&["ready", "--json"]);
    assert_eq!(code, 0, "{err}");
    let ready = oms_command_plan::json::parse(out.as_bytes()).unwrap();
    let rows = ready.get("tasks").array().unwrap();
    assert_eq!(rows.len(), 2, "{out}");
    assert_eq!(rows[0].text("role"), "builder", "{out}");
    assert_eq!(rows[0].get("resources").strings().unwrap(), ["cpu", "mem"]);
    assert_eq!(rows[1].text("role"), "", "{out}");
    assert!(
        rows[1].get("resources").strings().unwrap().is_empty(),
        "{out}"
    );
    let (code, _, err) = run(&["review", "--id", "a"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = run(&["ready", "--json", "--max-review-backlog", "1"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"backpressure\": true"), "{out}");
    let (code, _, err) = run(&["next", "--max-review-backlog", "1"]);
    assert_eq!(code, 3);
    assert!(err.contains("review backlog backpressure"), "{err}");
    let before = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let (code, _, err) = run(&[
        "review",
        "--id",
        "a",
        "--lease-id",
        "old",
        "--artifact",
        "result",
    ]);
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("stale lease"), "{err}");
    assert_eq!(before, fs::read(r.join(".oms/plan/tasks.json")).unwrap());
    let ledger = fs::read_to_string(r.join(".oms/plan/late-results.jsonl")).unwrap();
    assert!(ledger.contains("\"supplied_lease_id\":\"old\""), "{ledger}");
}

#[test]
fn reviewed_evidence_refuses_substitution_and_keeps_exact_frozen_finish() {
    if isolated("reviewed_evidence_refuses_substitution_and_keeps_exact_frozen_finish") {
        return;
    }
    use oms_command_plan::{Action, Context, Plan, Request, json};
    let tmp = Tmp::new();
    let repo = repo(&tmp);
    let stop = AtomicBool::new(false);
    let request = Request::new(Action::Init).with("goal", "Bind reviewed evidence");
    let context = Context::from_environment_at(&request, &repo, &repo, None).unwrap();
    let plan = Plan::open(context, &Host, &stop).unwrap();
    plan.execute(&request).unwrap();
    plan.execute(
        &Request::new(Action::Add)
            .with("id", "exact")
            .with("title", "Exact external candidate")
            .with("allowed", "a.txt")
            .with("verify", "true"),
    )
    .unwrap();
    plan.execute(
        &Request::new(Action::Claim)
            .with("id", "exact")
            .with("provider", "codex"),
    )
    .unwrap();
    fs::create_dir_all(repo.join(".oms/artifacts")).unwrap();
    let artifact = ".oms/artifacts/native-result.json";
    let patch = ".oms/artifacts/native-candidate.patch";
    let report = b"Reviewed native result\n";
    let original =
        b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-x\n+reviewed\n";
    fs::write(repo.join(artifact), report).unwrap();
    fs::write(repo.join(patch), original).unwrap();
    let lease = plan
        .load()
        .unwrap()
        .task("exact")
        .unwrap()
        .text("lease_id")
        .to_owned();
    let review = Request::new(Action::Review)
        .with("id", "exact")
        .with("lease-id", &lease)
        .with("artifact", artifact)
        .with("patch", patch)
        .with("expected-review-artifact-sha256", json::sha(report))
        .with("expected-review-patch-sha256", json::sha(original));
    let claimed = fs::read(repo.join(".oms/plan/tasks.json")).unwrap();
    for missing in [
        "expected-review-artifact-sha256",
        "expected-review-patch-sha256",
    ] {
        let mut partial = review.clone();
        partial.values.remove(missing);
        assert!(plan.execute(&partial).is_err());
        assert_eq!(
            fs::read(repo.join(".oms/plan/tasks.json")).unwrap(),
            claimed
        );
    }
    for (field, value) in [
        ("expected-review-patch-sha256", "invalid".into()),
        ("expected-review-artifact-sha256", "0".repeat(64)),
        ("patch", ".oms/artifacts/missing.patch".into()),
    ] {
        assert!(plan.execute(&review.clone().with(field, value)).is_err());
        assert_eq!(
            fs::read(repo.join(".oms/plan/tasks.json")).unwrap(),
            claimed
        );
    }
    plan.execute(&review).unwrap();
    plan.execute(
        &Request::new(Action::Repair)
            .with("id", "exact")
            .with("lease-id", &lease),
    )
    .unwrap();
    assert!(
        plan.load()
            .unwrap()
            .task("exact")
            .unwrap()
            .opt("review_evidence")
            .is_none()
    );
    plan.execute(&review).unwrap();
    let land = |bytes: &[u8]| {
        Request::new(Action::Land)
            .with("id", "exact")
            .with("lease-id", &lease)
            .with("expected-review-patch", patch)
            .with("expected-review-patch-sha256", json::sha(bytes))
            .with("expected-review-verify", "true")
            .with("expected-review-executor-id", "")
            .with("expected-review-executor-soul-sha256", "")
            .with("expected-review-lease-id", &lease)
    };
    let before = fs::read(repo.join(".oms/plan/tasks.json")).unwrap();
    let snapshot = Request::new(Action::EvidenceSnapshot).with("id", "exact");
    plan.execute(&snapshot).unwrap();
    for bad in [json::Value::Null, json::object([])] {
        let mut malformed = json::parse(&before).unwrap();
        let mut tasks = malformed.get("tasks").clone();
        let mut task = tasks.get("exact").clone();
        task.set("review_evidence", bad).unwrap();
        tasks.set("exact", task).unwrap();
        malformed.set("tasks", tasks).unwrap();
        fs::write(
            repo.join(".oms/plan/tasks.json"),
            json::encoded(&malformed).unwrap(),
        )
        .unwrap();
        assert!(plan.execute(&snapshot).is_err());
        assert!(plan.execute(&land(original)).is_err());
    }
    fs::write(repo.join(".oms/plan/tasks.json"), &before).unwrap();
    let substituted = b"different candidate bytes\n";
    fs::write(repo.join(patch), substituted).unwrap();
    assert!(plan.execute(&snapshot).is_err());
    assert!(
        plan.execute(&land(substituted)).is_err(),
        "a newly hashed patch must not replace the bytes bound by the original review"
    );
    assert_eq!(fs::read(repo.join(".oms/plan/tasks.json")).unwrap(), before);
    fs::write(repo.join(patch), original).unwrap();
    fs::write(repo.join(artifact), b"substituted report\n").unwrap();
    assert!(plan.execute(&land(original)).is_err());
    assert_eq!(fs::read(repo.join(".oms/plan/tasks.json")).unwrap(), before);
    fs::write(repo.join(artifact), report).unwrap();
    plan.execute(&land(original)).unwrap();
    let landing = plan.load().unwrap().task("exact").unwrap().clone();
    let frozen = ".oms/artifacts/frozen.patch";
    let finish = Request::new(Action::Finish)
        .with("id", "exact")
        .with("lease-id", &lease)
        .with("patch", frozen)
        .with(
            "expected-landing-receipt-sha256",
            json::receipt_digest(&landing).unwrap(),
        );
    fs::write(repo.join(frozen), substituted).unwrap();
    assert!(plan.execute(&finish).is_err());
    assert_eq!(plan.load().unwrap().task("exact").unwrap(), &landing);
    fs::write(repo.join(frozen), original).unwrap();
    plan.execute(&finish).unwrap();
    let done = plan.load().unwrap().task("exact").unwrap().clone();
    assert_eq!(done.text("state"), "done");
    assert_eq!(done.text("patch"), frozen);
    assert_eq!(done.get("review_evidence"), landing.get("review_evidence"));
    plan.execute(&snapshot).unwrap();
    plan.execute(
        &Request::new(Action::Block)
            .with("id", "exact")
            .with("lease-id", &lease)
            .with("reason", "owner withdrew this task"),
    )
    .unwrap();
    let blocked = plan.load().unwrap().task("exact").unwrap().clone();
    plan.execute(&snapshot).unwrap();
    let decision_path = ".oms/artifacts/cancel-decision.md";
    let decision = b"Owner withdrew the reviewed task\n";
    fs::write(repo.join(decision_path), decision).unwrap();
    plan.execute(
        &Request::new(Action::Cancel)
            .with("id", "exact")
            .with("operator-consent", "cancel")
            .with("expected-state", "blocked")
            .with("expected-lease-id", &lease)
            .with(
                "expected-plan-sha256",
                json::sha(fs::read(repo.join(".oms/plan/tasks.json")).unwrap()),
            )
            .with("expected-task-sha256", json::hash(&blocked).unwrap())
            .with("decision-artifact", decision_path)
            .with("expected-decision-artifact-sha256", json::sha(decision))
            .with("reason", "owner withdrew this task"),
    )
    .unwrap();
    fs::remove_file(repo.join(artifact)).unwrap();
    fs::remove_file(repo.join(frozen)).unwrap();
    let cancelled = json::parse(&plan.execute(&snapshot).unwrap().stdout).unwrap();
    assert_eq!(cancelled.text("state"), "cancelled");
    assert_eq!(
        cancelled.get("review_evidence"),
        landing.get("review_evidence")
    );
    assert!(plan.execute(&land(original)).is_err());
}

#[test]
fn panel_acceptance_checks_canonical_claimed_and_running_expiry() {
    if isolated("panel_acceptance_checks_canonical_claimed_and_running_expiry") {
        return;
    }
    use oms_command_plan::{Action, Context, Plan, Request, json};
    let tmp = Tmp::new();
    let repo = repo(&tmp);
    assert_eq!(go(&repo, &["init", "--goal", "g"]).0, 0);
    fs::write(repo.join("brief.txt"), "expiry fixture work").unwrap();
    assert_eq!(
        go(
            &repo,
            &[
                "add",
                "--id",
                "a",
                "--title",
                "a",
                "--brief-file",
                "brief.txt"
            ]
        )
        .0,
        0
    );
    let stop = AtomicBool::new(false);
    let claim = Request::new(Action::Claim)
        .with("id", "a")
        .with("provider", "codex")
        .with("ttl", "3600");
    let mut context = Context::from_environment_at(&claim, &repo, &repo, None).unwrap();
    context.panel_main_attempt = format!("att_{}", "1".repeat(32));
    context.participant = "main-fixture".into();
    context.ts = "2026-11-21T00:00:00Z".into();
    let active = Plan::open(context.clone(), &Host, &stop).unwrap();
    active.execute(&claim).unwrap();
    context.ts = "2026-11-21T02:11:48Z".into();
    let expired = Plan::open(context, &Host, &stop).unwrap();
    let query = |action, options: &[(&str, &str)]| {
        let req = options
            .iter()
            .fold(Request::new(action), |req, &(key, value)| {
                req.with(key, value)
            });
        expired.execute(&req).unwrap()
    };
    let read =
        |action, options: &[(&str, &str)]| json::parse(&query(action, options).stdout).unwrap();
    for state in ["claimed", "running"] {
        if state == "running" {
            active
                .execute(&Request::new(Action::Start).with("id", "a"))
                .unwrap();
        }
        let snapshot = active.load().unwrap();
        let mut expected = snapshot.task("a").unwrap().clone();
        assert_eq!(expected.text("state"), state);
        expected
            .set("plan_id", snapshot.plan.get("plan_id").clone())
            .unwrap();
        active.verify_panel_review_lease(&expected).unwrap();
        assert!(
            expired
                .verify_panel_review_lease(&expected)
                .unwrap_err()
                .message
                .contains("expired")
        );
        let plan_file = repo.join(".oms/plan/tasks.json");
        let original = fs::read(&plan_file).unwrap();
        let show = read(Action::Show, &[("id", "a")]);
        assert_eq!(show.text("state"), "stale");
        assert_eq!(show.text("stored_state"), state);
        assert_eq!(show.get("claim_expired"), &json::Value::Bool(true));
        assert_eq!(show.get("claim_age_s"), &json::number(7908));
        let list = read(Action::List, &[("json", "1")]);
        let listed_task = &list.get("tasks").array().unwrap()[0];
        assert_eq!(listed_task.text("state"), "stale");
        assert_eq!(listed_task.get("claim_expired"), &json::Value::Bool(true));
        assert_eq!(
            read(Action::List, &[("json", "1"), ("state", "stale")])
                .get("tasks")
                .array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            String::from_utf8(query(Action::List, &[]).stdout)
                .unwrap()
                .contains("EXPIRED")
        );
        let status = read(Action::Status, &[("json", "1")]);
        assert_eq!(status.get("stale").array().unwrap()[0].text("id"), "a");
        assert_eq!(status.get("by_state").get("stale"), &json::number(1));
        assert!(status.get("by_state").opt(state).is_none());
        let readiness = &status.get("task_readiness").array().unwrap()[0];
        assert_eq!(readiness.text("reason"), "claim-expired");
        assert_eq!(
            readiness.get("selection_eligible"),
            &json::Value::Bool(state == "claimed")
        );
        let brief = String::from_utf8(query(Action::Brief, &[("id", "a")]).stdout).unwrap();
        assert!(brief.contains("state: stale [claim EXPIRED"));
        assert_eq!(brief.contains("; claimable"), state == "claimed");
        let ready = read(Action::Ready, &[("json", "1")]);
        assert_eq!(
            ready.get("tasks").array().unwrap().len(),
            usize::from(state == "claimed")
        );
        assert_eq!(
            query(Action::Next, &[]).code,
            if state == "claimed" { 0 } else { 3 }
        );
        if state == "running" {
            assert!(expired.execute(&claim).is_err());
            assert!(
                String::from_utf8(query(Action::Reclaim, &[]).stdout)
                    .unwrap()
                    .contains("reclaimed 0")
            );
        }
        fs::create_dir_all(repo.join(".oms/delegations")).unwrap();
        let marker_file = repo.join(".oms/delegations/worker.json");
        for (pid, lease_id, protected) in [
            (std::process::id(), expected.get("lease_id").clone(), true),
            (0, expected.get("lease_id").clone(), true),
            (
                std::process::id(),
                json::string(format!("lease_{}", "3".repeat(32))),
                false,
            ),
        ] {
            let marker = json::object([
                ("schema", json::number(1)),
                ("id", json::string("worker")),
                ("pid", json::number(pid)),
                ("task_id", json::string("a")),
                ("lease_id", lease_id),
            ]);
            fs::write(&marker_file, json::canonical(&marker).unwrap()).unwrap();
            assert_eq!(
                expired.verify_panel_review_lease(&expected).is_ok(),
                protected
            );
            let projected_state = if protected { state } else { "stale" };
            let shown = read(Action::Show, &[("id", "a")]);
            assert_eq!(shown.text("state"), projected_state);
            assert_eq!(shown.get("claim_expired"), &json::Value::Bool(!protected));
            let listed = read(Action::List, &[("json", "1")]);
            assert_eq!(
                listed.get("tasks").array().unwrap()[0].text("state"),
                projected_state
            );
            let status = read(Action::Status, &[("json", "1")]);
            assert_eq!(
                status.get("by_state").get(projected_state),
                &json::number(1)
            );
            assert_eq!(status.get("stale").array().unwrap().is_empty(), protected);
            if protected {
                assert!(
                    String::from_utf8(query(Action::Reclaim, &[("include-running", "1")]).stdout)
                        .unwrap()
                        .contains("reclaimed 0")
                );
            }
        }
        fs::remove_file(marker_file).unwrap();
        assert_eq!(fs::read(&plan_file).unwrap(), original);
        expected
            .set(
                "lease_id",
                json::string(format!("lease_{}", "2".repeat(32))),
            )
            .unwrap();
        assert!(
            active
                .verify_panel_review_lease(&expected)
                .unwrap_err()
                .message
                .contains("lease changed")
        );
    }
    assert!(!repo.join(".oms/plan/late-results.jsonl").exists());
}

#[test]
fn versioned_contracts_gate_scheduling() {
    if isolated("versioned_contracts_gate_scheduling") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    let run = |rest: &[&str]| {
        let mut command = vec!["--repo", rs];
        command.extend_from_slice(rest);
        go(&tmp.0, &command)
    };
    let ok = |rest: &[&str]| {
        let (code, out, err) = run(rest);
        assert_eq!(code, 0, "{rest:?}: {err}");
        out
    };
    ok(&["init", "--goal", "g"]);
    ok(&["contract-propose", "--id", "api", "--summary", "v1"]);
    ok(&["contract-accept", "--id", "api", "--version", "1"]);
    ok(&[
        "add",
        "--id",
        "downstream",
        "--title",
        "work",
        "--contracts",
        "api@v1",
    ]);
    assert!(ok(&["ready"]).contains("downstream"));
    ok(&["claim", "--id", "downstream", "--provider", "codex"]);
    ok(&["release", "--id", "downstream"]);
    ok(&["contract-propose", "--id", "api", "--summary", "v2"]);
    assert!(ok(&["ready"]).contains("downstream"));
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "contract_child_accept", "--ignored"])
        .env("OMS_HARNESS_CHILD", "1")
        .env("CONTRACT_TEST_REPO", &r)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    ok(&["contract-accept", "--id", "api", "--version", "2"]);
    let show = ok(&["show", "--id", "downstream", "--json"]);
    assert!(show.contains("\"contract_stale\": true"), "{show}");
    assert!(
        show.contains("\"stale_contracts\": [\n    \"api\""),
        "{show}"
    );
    assert!(!ok(&["ready"]).contains("downstream"));
    let status = ok(&["status", "--json"]);
    assert!(
        status.contains("\"contract_stale_tasks\": [\"downstream\"]")
            && status.contains("\"contract_count\": 1"),
        "{status}"
    );
    let (code, _, err) = run(&["claim", "--id", "downstream", "--provider", "codex"]);
    assert_eq!(code, 2);
    assert!(err.contains("contract_stale"), "{err}");
    ok(&["refresh-contracts", "--task", "downstream"]);
    assert!(ok(&["show", "--id", "downstream"]).contains("api@v2"));
    assert!(ok(&["ready"]).contains("downstream"));
    ok(&["claim", "--id", "downstream", "--provider", "codex"]);
    ok(&[
        "review",
        "--id",
        "downstream",
        "--artifact",
        "v2-review.md",
        "--patch",
        "v2.diff",
    ]);
    let noop = ok(&["refresh-contracts", "--task", "downstream"]);
    assert!(noop.contains("\"state\": \"review\""), "{noop}");
    assert!(noop.contains("\"artifact\": \"v2-review.md\""), "{noop}");
    assert!(noop.contains("\"patch\": \"v2.diff\""), "{noop}");
    ok(&["contract-propose", "--id", "api", "--summary", "v3"]);
    ok(&["contract-accept", "--id", "api", "--version", "3"]);
    let refreshed = ok(&["refresh-contracts", "--task", "downstream"]);
    assert!(refreshed.contains("api@v3"), "{refreshed}");
    assert!(refreshed.contains("\"state\": \"ready\""), "{refreshed}");
    assert!(refreshed.contains("\"artifact\": \"\""), "{refreshed}");
    assert!(refreshed.contains("\"patch\": \"\""), "{refreshed}");
    assert!(
        refreshed.contains("\"review_lease_id\": \"\""),
        "{refreshed}"
    );
    ok(&["claim", "--id", "downstream", "--provider", "codex"]);
    ok(&[
        "review",
        "--id",
        "downstream",
        "--artifact",
        "v3-review.md",
        "--patch",
        "v3.diff",
    ]);
    ok(&[
        "block",
        "--id",
        "downstream",
        "--reason",
        "waiting on upstream fixture",
    ]);
    ok(&["contract-propose", "--id", "api", "--summary", "v4"]);
    ok(&["contract-accept", "--id", "api", "--version", "4"]);
    let blk = ok(&["refresh-contracts", "--task", "downstream"]);
    assert!(blk.contains("api@v4"), "{blk}");
    assert!(blk.contains("\"state\": \"blocked\""), "{blk}");
    assert!(
        blk.contains("\"reason\": \"waiting on upstream fixture\""),
        "{blk}"
    );
    assert!(blk.contains("\"artifact\": \"\""), "{blk}");
    assert!(blk.contains("\"patch\": \"\""), "{blk}");
    assert!(!ok(&["ready"]).contains("downstream"));
    assert_eq!(
        run(&["add", "--id", "bad", "--title", "bad", "--priority", "abc"]).0,
        2
    );
}

#[test]
fn team_contracts_preserve_parent_lineage_and_generation() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if std::env::var_os("OMS_TEAM_GIT_FIXTURE_BIN").is_none() {
        let fixture = Tmp::new();
        let bin = fixture.0.join("bin");
        fs::create_dir(&bin).unwrap();
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join("git"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        let script = format!(
            "#!/bin/sh\n\
case \"${{OMS_PLAN_GIT_IO_CASE-}}\" in\n\
  echo) cat; printf tail; printf ignored >&2; exit 7;;\n\
  held-pipe) /bin/sleep 5 & printf tail; exit 0;;\n\
  blocked-input) printf entered > \"${{0%/*}}/io-entered\"; exec /bin/sleep 5;;\n\
  closed-input) exec 0<&-; printf tail; exit 0;;\n\
  signal) kill -TERM $$;;\n\
esac\n\
case \"$*\" in *'rev-parse HEAD'*) if [ -f \"${{0%/*}}/gate\" ]; then printf entered > \"${{0%/*}}/entered\"; while [ -f \"${{0%/*}}/gate\" ]; do /bin/sleep 0.02; done; fi;; esac\nexec '{}' \"$@\"\n",
            real.to_str().unwrap().replace('\'', "'\\''")
        );
        fs::write(bin.join("git"), script).unwrap();
        fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "team_contracts_preserve_parent_lineage_and_generation",
            "--nocapture",
        ]);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("OMS_") {
                command.env_remove(key);
            }
        }
        command.env("XDG_STATE_HOME", fixture.0.join("state"));
        command.env("OMS_TEAM_GIT_FIXTURE_BIN", &bin).env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        print!("{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    use oms_command_plan::json::{Value, number, object, string};
    use oms_command_plan::services::{LifecycleGuard, ProjectedLifecycle, ProjectedRoom};
    use oms_command_plan::{Action, Context, Plan, Request, json};
    static REFUSE_ORDINARY_GROUP: AtomicBool = AtomicBool::new(false);
    struct Guard<'a>(&'a AtomicBool);
    impl LifecycleGuard for Guard<'_> {
        fn check(&self) -> oms_command_plan::Result<()> {
            if self.0.load(std::sync::atomic::Ordering::Acquire) {
                return Err(oms_command_plan::Error {
                    code: 2,
                    message: "fixture lifecycle guard changed".into(),
                });
            }
            Ok(())
        }
    }
    #[derive(Default)]
    struct TeamHost {
        lifecycle_reads: std::sync::atomic::AtomicUsize,
        room_reads: std::sync::atomic::AtomicUsize,
        changed_guard: AtomicBool,
        expire_during_projection: AtomicU64,
        proof_gid: AtomicU64,
        group_reads: std::sync::atomic::AtomicUsize,
        change_group_at: std::sync::atomic::AtomicUsize,
        change_policy_at: std::sync::atomic::AtomicUsize,
        policy_path: std::sync::Mutex<Option<PathBuf>>,
    }
    impl Services for TeamHost {
        fn group_membership_resolver(&self) -> Option<GroupResolver> {
            fn ordinary(gid: u32) -> oms_command_plan::Result<GroupMembership> {
                if REFUSE_ORDINARY_GROUP.load(Ordering::Acquire) {
                    return Err(oms_command_plan::Error {
                        code: 2,
                        message: "ordinary group lookup in bounded Team admission".into(),
                    });
                }
                Host.group_membership_resolver().unwrap()(gid)
            }
            Some(ordinary)
        }
        fn group_membership_before(
            &self,
            gid: u32,
            _: std::time::Instant,
            _: &AtomicBool,
        ) -> oms_command_plan::Result<GroupMembership> {
            let mut proof = Host.group_membership_resolver().unwrap()(gid)?;
            if u64::from(gid) + 1 == self.proof_gid.load(std::sync::atomic::Ordering::Acquire) {
                let ordinal = self
                    .group_reads
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                    + 1;
                if self
                    .change_group_at
                    .load(std::sync::atomic::Ordering::Acquire)
                    == ordinal
                {
                    proof
                        .primary_member_uids
                        .push(proof.primary_member_uids[0].wrapping_add(1));
                }
                if self
                    .change_policy_at
                    .load(std::sync::atomic::Ordering::Acquire)
                    == ordinal
                {
                    fs::write(
                        self.policy_path.lock().unwrap().as_ref().unwrap(),
                        b"{\"changed\":true}\n",
                    )
                    .unwrap();
                }
            }
            Ok(proof)
        }
        fn lifecycle_writer(
            &self,
            _: &Path,
        ) -> oms_command_plan::Result<Box<dyn LifecycleGuard + '_>> {
            Ok(Box::new(Guard(&self.changed_guard)))
        }
        fn lifecycle_writer_before(
            &self,
            _: &Path,
            _: std::time::Instant,
        ) -> oms_command_plan::Result<Box<dyn LifecycleGuard + '_>> {
            Ok(Box::new(Guard(&self.changed_guard)))
        }
        fn project_lifecycle(&self, raw: &[u8]) -> oms_command_plan::Result<ProjectedLifecycle> {
            self.lifecycle_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let until = self
                .expire_during_projection
                .swap(0, std::sync::atomic::Ordering::AcqRel);
            while SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                < until
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let terminal = std::str::from_utf8(raw).unwrap().contains("cancelled");
            Ok(ProjectedLifecycle {
                source_sha256: json::sha(raw),
                attempts: object([(
                    "main-a",
                    object([
                        ("attempt_id", string("main-a")),
                        ("tool", string("panel-main")),
                        ("parent_attempt_id", string("")),
                        ("terminal", Value::Bool(terminal)),
                        (
                            "state",
                            string(if terminal { "cancelled" } else { "working" }),
                        ),
                        (
                            "refs",
                            object([("panel_room_id", string("room-aaaaaaaaaaaa"))]),
                        ),
                    ]),
                )]),
            })
        }
        fn project_room(&self, room: &str, raw: &[u8]) -> oms_command_plan::Result<ProjectedRoom> {
            self.room_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(ProjectedRoom {
                source_sha256: json::sha(raw),
                state: object([
                    ("schema", number(1)),
                    ("id", string(room)),
                    ("closed", Value::Bool(false)),
                    (
                        "participants",
                        Value::Array(vec![object([
                            ("participant", string("main-a")),
                            ("role", string("main")),
                            ("joined", Value::Bool(true)),
                        ])]),
                    ),
                ]),
            })
        }
        fn supervise(
            &self,
            command: &oms_command_plan::services::SupervisedCommand,
        ) -> oms_command_plan::Result<oms_command_plan::services::Execution> {
            use oms_supervisor::process::{Progress, Reactor, Request, Scope, StopReason};
            let mut reactor = Reactor::new();
            let mut request = Request::new(command.argv.clone());
            request.cwd = Some(command.cwd.clone());
            request.scope = Scope::ProcessGroup;
            request.deadline = Some(std::time::Instant::now() + command.wall);
            request.environment = command.environment.clone();
            request.remove_environment = command
                .remove_environment
                .iter()
                .map(OsString::from)
                .collect();
            request.capture.max_bytes = command.output_limit;
            let job = reactor
                .start(request)
                .map_err(|e| oms_command_plan::Error {
                    code: 2,
                    message: e.to_string(),
                })?;
            loop {
                match reactor
                    .poll(job, std::time::Duration::from_millis(10))
                    .map_err(|e| oms_command_plan::Error {
                        code: 2,
                        message: e.to_string(),
                    })? {
                    Progress::Pending(_) => {}
                    Progress::Complete(done) => {
                        return Ok(oms_command_plan::services::Execution {
                            exit: done
                                .exit
                                .map(|exit| exit.shell_code().try_into().unwrap_or(125))
                                .unwrap_or(125),
                            runner_exit: 0,
                            timed_out: done.stop == Some(StopReason::Deadline),
                            output_limited: done.stdout.limited,
                            launch_error: false,
                            supervision_error: !done.cleanup_complete() || !done.output_complete(),
                            output: done.stdout.bytes,
                            duration_s: done.duration.as_secs(),
                        });
                    }
                }
            }
        }
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let stop = AtomicBool::new(false);
    // Reuse the isolated Git shim for the default query path as well as the
    // admission deadline path exercised below.
    let mut proof = oms_command_plan::repository::Git {
        repo: &r,
        interrupted: &stop,
        group_resolver: None,
        deadline: None,
        group_snapshot: None,
    };
    let query =
        |proof: &oms_command_plan::repository::Git<'_>, case: &str, input: Option<&[u8]>, limit| {
            proof.output(
                &[],
                input,
                &[("OMS_PLAN_GIT_IO_CASE".into(), case.into())],
                limit,
            )
        };
    let input = vec![b'x'; 1024 * 1024];
    for bounded in [false, true] {
        proof.deadline =
            bounded.then(|| std::time::Instant::now() + std::time::Duration::from_secs(5));
        for bytes in [None, Some(&[][..]), Some(input.as_slice())] {
            let mut expected = bytes.unwrap_or_default().to_vec();
            expected.extend_from_slice(b"tail");
            let output = query(&proof, "echo", bytes, expected.len()).unwrap();
            assert_eq!(output.code, 7);
            assert_eq!(output.stdout, expected);
        }
        assert_eq!(
            query(&proof, "echo", Some(&[]), 3).err().unwrap().message,
            "Git output exceeds bound"
        );
        assert_eq!(
            query(&proof, "signal", None, 16).unwrap().code,
            if bounded { 143 } else { 128 }
        );
        let closed = query(&proof, "closed-input", Some(&input), 16);
        if bounded {
            assert_eq!(closed.unwrap().stdout, b"tail");
        } else {
            assert_eq!(closed.err().unwrap().message, "Git input unavailable");
        }
    }
    proof.deadline = None;
    let started = std::time::Instant::now();
    assert_eq!(
        query(&proof, "held-pipe", None, 16).unwrap().stdout,
        b"tail"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    let git_bin = PathBuf::from(std::env::var_os("OMS_TEAM_GIT_FIXTURE_BIN").unwrap());
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !git_bin.join("io-entered").is_file() && std::time::Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            stop.store(true, Ordering::Release);
        });
        let started = std::time::Instant::now();
        let refused = query(&proof, "blocked-input", Some(&input), 16)
            .err()
            .unwrap();
        assert_eq!(refused.code, 2, "{refused}");
        assert_eq!(refused.message, "local Git proof interrupted or timed out");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    });
    assert!(git_bin.join("io-entered").is_file());
    stop.store(false, Ordering::Release);
    fs::remove_file(git_bin.join("io-entered")).unwrap();
    let host = TeamHost::default();
    let call = |values: &[&str], child: bool| {
        let req = Request::parse(&args(values)).unwrap();
        let mut ctx = Context::from_environment(&req, &r).unwrap();
        ctx.panel_main_attempt = "main-a".into();
        ctx.room_id = "room-aaaaaaaaaaaa".into();
        ctx.participant = "main-a".into();
        ctx.child = child;
        Plan::open(ctx, &host, &stop).unwrap().execute(&req)
    };
    call(&["init", "--goal", "bounded fixture"], false).unwrap();
    call(
        &[
            "add",
            "--id",
            "task-a",
            "--title",
            "team fixture",
            "--allowed",
            "src/",
            "--verify",
            "true",
        ],
        false,
    )
    .unwrap();
    assert_eq!(
        call(&["team-list", "--json"], true).unwrap().stdout,
        b"[]\n"
    );
    fs::create_dir_all(r.join(".oms/lifecycle")).unwrap();
    fs::create_dir_all(r.join(".oms/threads")).unwrap();
    let history = concat!(
        "{\"schema\":1,\"event_id\":\"evt-owner\",\"attempt_id\":\"main-a\",\"seq\":1,\"ts\":\"2026-01-01T00:00:00Z\",\"event_type\":\"attempt.created\",\"provider\":\"codex\",\"tool\":\"panel-main\",\"refs\":{\"panel_role\":\"main\",\"panel_room_id\":\"room-aaaaaaaaaaaa\",\"panel_room_participant\":\"main-a\",\"panel_owner\":\"codex\"}}\n",
        "{\"schema\":1,\"event_id\":\"evt-start\",\"attempt_id\":\"main-a\",\"seq\":2,\"ts\":\"2026-01-01T00:00:01Z\",\"event_type\":\"attempt.state_changed\",\"to_state\":\"working\"}\n"
    );
    fs::write(r.join(".oms/lifecycle/events.jsonl"), history).unwrap();
    fs::write(
        r.join(".oms/threads/room-aaaaaaaaaaaa.jsonl"),
        b"{\"schema\":1,\"thread\":\"room-aaaaaaaaaaaa\",\"seq\":1,\"ts\":\"2026-01-01T00:00:00Z\",\"room_event\":{\"kind\":\"join\",\"participant\":\"main-a\",\"provider\":\"codex\",\"role\":\"main\"}}\n",
    )
    .unwrap();
    fs::write(r.join("team-policy.json"), b"{}\n").unwrap();
    fs::set_permissions(
        r.join("team-policy.json"),
        fs::Permissions::from_mode(0o664),
    )
    .unwrap();
    host.proof_gid.store(
        u64::from(fs::metadata(r.join("team-policy.json")).unwrap().gid()) + 1,
        std::sync::atomic::Ordering::Release,
    );
    *host.policy_path.lock().unwrap() = Some(r.join("team-policy.json"));
    let plan = json::parse(&fs::read(r.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    let task = plan.get("tasks").get("task-a");
    let head = Command::new("git")
        .arg("-C")
        .arg(&r)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    let policy = json::sha(b"{}\n");
    let mut spec = json::parse(include_bytes!("fixtures/team-spec.json")).unwrap();
    for (key, value) in [
        ("room_id", string("room-aaaaaaaaaaaa")),
        ("plan_id", plan.get("plan_id").clone()),
        ("task_sha256", string(json::hash(task).unwrap())),
        ("objective_sha256", string(json::sha(b"bounded fixture"))),
        (
            "criterion_sha256",
            string(oms_command_plan::team::criterion_digest(&plan, task).unwrap()),
        ),
        (
            "base_sha",
            string(std::str::from_utf8(&head).unwrap().trim()),
        ),
        ("policy_sha256", string(&policy)),
    ] {
        spec.set(key, value).unwrap();
    }
    let mut members = spec.get("members").array().unwrap().to_vec();
    for member in &mut members {
        let mut route = member.get("route").clone();
        route.set("policy_sha256", string(&policy)).unwrap();
        member.set("route", route).unwrap();
    }
    spec.set("members", Value::Array(members)).unwrap();
    let write =
        |spec: &Value| fs::write(r.join("team.json"), json::canonical(spec).unwrap()).unwrap();
    let register = ["team-register", "--team-spec", "team.json", "--json"];
    for (id, allowed, forbidden, team_allow, team_deny) in [
        ("task-glob", "src/?", "", vec!["src/a"], vec![]),
        (
            "task-deny",
            "src",
            "src/private",
            vec!["src"],
            vec!["src/???????"],
        ),
        ("task-empty", "src", "", vec![], vec![]),
    ] {
        call(
            &[
                "add",
                "--id",
                id,
                "--title",
                "scope containment",
                "--allowed",
                allowed,
                "--forbidden",
                forbidden,
                "--verify",
                "true",
            ],
            false,
        )
        .unwrap();
        let parent = json::parse(&fs::read(r.join(".oms/plan/tasks.json")).unwrap()).unwrap();
        let task = parent.get("tasks").get(id);
        let mut invalid = spec.clone();
        invalid.set("task_id", string(id)).unwrap();
        invalid
            .set("task_sha256", string(json::hash(task).unwrap()))
            .unwrap();
        invalid
            .set(
                "allowed_paths",
                Value::Array(team_allow.into_iter().map(string).collect()),
            )
            .unwrap();
        invalid
            .set(
                "forbidden_paths",
                Value::Array(team_deny.into_iter().map(string).collect()),
            )
            .unwrap();
        write(&invalid);
        let error = call(&register, false).unwrap_err();
        assert!(error.message.contains("scope"), "{id}: {error}");
    }
    for (key, bad) in [
        ("schema", number(2)),
        ("root_owner_generation", string("evt-recycled")),
        ("expires_at", string("2000-01-01T00:00:00Z")),
        ("policy_sha256", string("0".repeat(64))),
        ("root_owner", string("foreign-main")),
    ] {
        let mut invalid = spec.clone();
        invalid.set(key, bad).unwrap();
        write(&invalid);
        assert!(call(&register, false).is_err(), "{key}");
    }
    call(
        &[
            "add",
            "--id",
            "unrestricted",
            "--title",
            "parent without an allow restriction",
            "--verify",
            "true",
        ],
        false,
    )
    .unwrap();
    let parent = json::parse(&fs::read(r.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    let unrestricted = parent.get("tasks").get("unrestricted");
    for (id, allowed) in [
        ("team-no-restriction", vec![]),
        ("team-narrow-restriction", vec!["src"]),
    ] {
        let mut allowed_spec = spec.clone();
        allowed_spec.set("id", string(id)).unwrap();
        allowed_spec.set("task_id", string("unrestricted")).unwrap();
        allowed_spec
            .set("task_sha256", string(json::hash(unrestricted).unwrap()))
            .unwrap();
        allowed_spec
            .set(
                "allowed_paths",
                Value::Array(allowed.into_iter().map(string).collect()),
            )
            .unwrap();
        write(&allowed_spec);
        call(&register, false).unwrap();
    }
    for (key, bad) in [
        ("logical_parent", string("worker-a")),
        ("root_owner", string("foreign-main")),
        ("id", string("main-a")),
    ] {
        let mut invalid = spec.clone();
        let mut members = invalid.get("members").array().unwrap().to_vec();
        members[0].set(key, bad).unwrap();
        invalid.set("members", Value::Array(members)).unwrap();
        assert!(
            oms_command_plan::team::TeamSpec::parse(&json::canonical(&invalid).unwrap()).is_err()
        );
    }
    write(&spec);
    assert!(call(&register, true).is_err());
    let enrolled = call(&register, false).unwrap();
    let record = json::parse(&enrolled.stdout).unwrap();
    let digest = record.get("team-a").get("versions").array().unwrap()[0]
        .text("sha256")
        .to_owned();
    let stored = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    assert!(call(&register, false).is_err());
    assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), stored);
    let shown = json::parse(&call(&["team-show", "--id", "team-a"], true).unwrap().stdout).unwrap();
    assert_eq!(shown.text("phase"), "planned");
    assert_eq!(shown.get("verified"), &Value::Null);
    assert_eq!(shown.get("spec").get("members").array().unwrap().len(), 2);
    host.lifecycle_reads
        .store(0, std::sync::atomic::Ordering::Relaxed);
    host.room_reads
        .store(0, std::sync::atomic::Ordering::Relaxed);
    let listed = call(&["team-list", "--json"], true).unwrap();
    assert_eq!(
        json::parse(&listed.stdout).unwrap().array().unwrap().len(),
        3
    );
    assert_eq!(
        host.lifecycle_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(
        host.room_reads.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), stored);
    let mut admission_context =
        Context::from_environment(&Request::new(Action::TeamShow), &r).unwrap();
    admission_context.panel_main_attempt = "main-a".into();
    admission_context.room_id = "room-aaaaaaaaaaaa".into();
    admission_context.participant = "main-a".into();
    // An old Context timestamp is not the final admission clock.
    admission_context.ts = "2201-01-01T00:00:00Z".into();
    let admission_plan = Plan::open(admission_context.clone(), &host, &stop).unwrap();
    let admission = oms_command_plan::team::AdmissionRequest {
        team_id: "team-a".into(),
        spec_sha256: digest.clone(),
        member_id: "lead-a".into(),
        member_generation: 1,
        provider: "codex".into(),
        harness: "codex".into(),
        host: "local".into(),
        transport: "direct".into(),
        account: Some("work".into()),
        selected_model: Some("fixture".into()),
        selected_effort: Some("high".into()),
        write: false,
    };
    let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(5);
    let guard = admission_plan
        .team_admission_before(admission.clone(), deadline())
        .unwrap();
    assert_eq!(guard.binding().text("root_owner_generation"), "evt-owner");
    assert_eq!(guard.binding().get("member").text("attempt_id"), "");
    let before = std::time::Instant::now();
    assert_eq!(
        admission_plan
            .team_admission_before(
                admission.clone(),
                before + std::time::Duration::from_millis(30)
            )
            .err()
            .unwrap()
            .code,
        75
    );
    assert!(before.elapsed() < std::time::Duration::from_secs(1));
    fs::write(r.join("team-policy.json"), b"{\"changed\":true}\n").unwrap();
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("policy changed")
    );
    fs::write(r.join("team-policy.json"), b"{}\n").unwrap();
    guard.check().unwrap();
    host.change_group_at.store(
        host.group_reads.load(std::sync::atomic::Ordering::Acquire) + 2,
        std::sync::atomic::Ordering::Release,
    );
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("group ownership changed")
    );
    guard.check().unwrap();
    host.change_policy_at.store(
        host.group_reads.load(std::sync::atomic::Ordering::Acquire) + 2,
        std::sync::atomic::Ordering::Release,
    );
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("policy changed")
    );
    fs::write(r.join("team-policy.json"), b"{}\n").unwrap();
    guard.check().unwrap();
    host.changed_guard
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(guard.check().unwrap_err().message.contains("guard changed"));
    host.changed_guard
        .store(false, std::sync::atomic::Ordering::Release);
    let mut withdrawn = json::parse(&stored).unwrap();
    let mut teams = withdrawn.get("teams").clone();
    let mut team = teams.get("team-a").clone();
    team.set("phase", string("settled")).unwrap();
    teams.set("team-a", team).unwrap();
    withdrawn.set("teams", teams).unwrap();
    fs::write(
        r.join(".oms/plan/tasks.json"),
        json::canonical(&withdrawn).unwrap(),
    )
    .unwrap();
    assert!(guard.check().unwrap_err().message.contains("withdrawn"));
    fs::write(r.join(".oms/plan/tasks.json"), &stored).unwrap();
    fs::write(r.join(".oms/lifecycle/events.jsonl"), format!("{history}{{\"schema\":1,\"event_id\":\"evt-cancel\",\"attempt_id\":\"main-a\",\"seq\":3,\"ts\":\"2026-01-01T00:00:02Z\",\"event_type\":\"attempt.state_changed\",\"state\":\"cancelled\"}}\n")).unwrap();
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("owner is stale")
    );
    fs::write(r.join(".oms/lifecycle/events.jsonl"), history).unwrap();
    guard.check().unwrap();
    drop(guard);
    let git_bin = PathBuf::from(std::env::var_os("OMS_TEAM_GIT_FIXTURE_BIN").unwrap());
    fs::write(git_bin.join("gate"), b"wait").unwrap();
    let started = std::time::Instant::now();
    let refused = admission_plan
        .team_admission_before(
            admission.clone(),
            started + std::time::Duration::from_millis(300),
        )
        .err()
        .unwrap();
    assert_eq!(refused.code, 75, "{refused}");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(git_bin.join("entered").is_file());
    fs::remove_file(git_bin.join("entered")).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !git_bin.join("entered").is_file() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            stop.store(true, std::sync::atomic::Ordering::Release);
        });
        let started = std::time::Instant::now();
        assert_eq!(
            admission_plan
                .team_admission_before(admission.clone(), deadline())
                .err()
                .unwrap()
                .code,
            130
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    });
    assert!(git_bin.join("entered").is_file());
    stop.store(false, std::sync::atomic::Ordering::Release);
    fs::remove_file(git_bin.join("gate")).unwrap();
    admission_plan
        .team_admission_before(admission.clone(), deadline())
        .unwrap()
        .check()
        .unwrap();
    for field in [
        "spec",
        "member",
        "provider",
        "harness",
        "host",
        "transport",
        "account",
        "model",
        "effort",
        "unknown-effort",
        "access",
    ] {
        let mut wrong = admission.clone();
        match field {
            "spec" => wrong.spec_sha256 = "0".repeat(64),
            "member" => wrong.member_generation += 1,
            "provider" => wrong.provider = "claude".into(),
            "harness" => wrong.harness = "claude".into(),
            "host" => wrong.host = "foreign".into(),
            "transport" => wrong.transport = "native".into(),
            "account" => wrong.account = Some("other-account".into()),
            "model" => wrong.selected_model = None,
            "effort" => wrong.selected_effort = Some("medium".into()),
            "unknown-effort" => wrong.selected_effort = None,
            "access" => wrong.write = true,
            _ => unreachable!(),
        }
        assert!(
            admission_plan
                .team_admission_before(wrong, deadline())
                .is_err(),
            "{field}"
        );
    }
    let mut child_context = admission_context.clone();
    child_context.child = true;
    assert!(
        Plan::open(child_context, &host, &stop)
            .unwrap()
            .team_admission_before(admission.clone(), deadline())
            .is_err()
    );
    assert!(!r.join(".oms/usage/receipts.jsonl").exists());
    assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), stored);
    let mut expiring = spec.clone();
    expiring.set("id", string("team-expiring")).unwrap();
    let expiry = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3;
    expiring
        .set(
            "expires_at",
            string(
                chrono::DateTime::from_timestamp(expiry as i64, 0)
                    .unwrap()
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string(),
            ),
        )
        .unwrap();
    write(&expiring);
    call(&register, false).unwrap();
    let mut expiry_request = admission.clone();
    expiry_request.team_id = "team-expiring".into();
    expiry_request.spec_sha256 = json::hash(&expiring).unwrap();
    let guard = admission_plan
        .team_admission_before(expiry_request, deadline())
        .unwrap();
    host.expire_during_projection
        .store(expiry, std::sync::atomic::Ordering::Release);
    assert!(guard.check().unwrap_err().message.contains("expired"));
    drop(guard);

    spec.set("generation", number(2)).unwrap();
    let mut updated_members = spec.get("members").array().unwrap().to_vec();
    let large_generation = i128::from(u64::MAX) + 7;
    updated_members[0]
        .set("generation", number(large_generation))
        .unwrap();
    spec.set("members", Value::Array(updated_members)).unwrap();
    spec.set("previous_sha256", string(&digest)).unwrap();
    write(&spec);
    assert!(call(&register, false).is_err());
    call(
        &[
            "team-register",
            "--team-spec",
            "team.json",
            "--expected-team-sha256",
            &digest,
        ],
        false,
    )
    .unwrap();
    let shown = json::parse(
        &call(&["team-show", "--id", "team-a"], false)
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(shown.text("phase"), "held");
    assert_eq!(shown.get("history").array().unwrap().len(), 2);
    let mut next_request = admission.clone();
    next_request.spec_sha256 = json::hash(&spec).unwrap();
    next_request.member_generation = large_generation;
    admission_plan
        .team_admission_before(next_request, deadline())
        .unwrap()
        .check()
        .unwrap();
    fs::write(r.join("team-policy.json"), b"{\"changed\":true}\n").unwrap();
    let stale = json::parse(
        &call(&["team-show", "--id", "team-a"], false)
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(stale.text("reason").contains("policy changed"));
    let current_digest = shown
        .get("history")
        .array()
        .unwrap()
        .last()
        .unwrap()
        .text("sha256");
    assert!(
        call(
            &[
                "team-cancel",
                "--id",
                "team-a",
                "--expected-team-sha256",
                &digest,
                "--reason",
                "withdraw"
            ],
            false
        )
        .is_err()
    );
    call(
        &[
            "team-cancel",
            "--id",
            "team-a",
            "--expected-team-sha256",
            current_digest,
            "--reason",
            "withdraw unlaunched team",
        ],
        false,
    )
    .unwrap();
    let cancelled = json::parse(
        &call(&["team-show", "--id", "team-a"], false)
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(cancelled.text("phase"), "settled");
    assert_eq!(
        fs::read_to_string(r.join(".oms/lifecycle/events.jsonl")).unwrap(),
        history
    );

    // The native publisher and room/lifecycle adapters are fixture inputs here;
    // Git objects, Plan transitions, verifier execution and typed completion are real.
    fs::write(r.join("team-policy.json"), b"{}\n").unwrap();
    fs::write(r.join(".git/info/exclude"), b".oms/\n").unwrap();
    git(&r, &["add", "."]);
    git(&r, &["commit", "-qm", "freeze Team fixture source"]);
    let git_text = |values: &[&str]| {
        let output = fixture_git(&r).args(values).output().unwrap();
        assert!(
            output.status.success(),
            "git {values:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let base_commit = git_text(&["rev-parse", "HEAD"]);
    for id in ["commit-obligation", "commit-product"] {
        call(
            &[
                "add",
                "--id",
                id,
                "--title",
                id,
                "--allowed",
                "src/product.txt",
                "--verify",
                "bash verify-product.sh",
            ],
            false,
        )
        .unwrap();
        call(&["claim", "--id", id, "--provider", "codex"], false).unwrap();
    }
    call(
        &[
            "add",
            "--id",
            "commit-dependent",
            "--title",
            "use the verified product",
            "--depends",
            "commit-obligation",
            "--allowed",
            "src",
            "--verify",
            "true",
        ],
        false,
    )
    .unwrap();
    fs::create_dir_all(r.join("src")).unwrap();
    fs::write(r.join("src/product.txt"), b"product\n").unwrap();
    fs::write(
        r.join("verify-product.sh"),
        b"#!/bin/sh\nset -eu\ntest \"$(cat src/product.txt)\" = product\n",
    )
    .unwrap();
    git(&r, &["add", "src/product.txt", "verify-product.sh"]);
    git(&r, &["commit", "-qm", "actual completion product"]);
    let product_commit = git_text(&["rev-parse", "HEAD"]);
    let product_tree = git_text(&["rev-parse", "HEAD^{tree}"]);
    git(
        &r,
        &["update-ref", "refs/remotes/fixture/main", &product_commit],
    );
    let mut cksum = Command::new("cksum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        cksum.stdin.take().as_mut().unwrap(),
        std::os::unix::ffi::OsStrExt::as_bytes(r.as_os_str()),
    )
    .unwrap();
    let checksum = cksum.wait_with_output().unwrap();
    assert!(checksum.status.success());
    let checksum = String::from_utf8(checksum.stdout).unwrap();
    let mut checksum = checksum.split_whitespace();
    let receipt_directory = PathBuf::from(std::env::var_os("XDG_STATE_HOME").unwrap())
        .join("oh-my-setting/land")
        .join(format!(
            "repo-{}-{}",
            checksum.next().unwrap(),
            checksum.next().unwrap()
        ));
    fs::create_dir_all(&receipt_directory).unwrap();
    fs::set_permissions(&receipt_directory, fs::Permissions::from_mode(0o770)).unwrap();
    let receipt_path = receipt_directory.join(format!("{product_commit}-fixture.json"));
    let land_receipt = json::canonical(&object([
        ("schema", number(1)),
        ("sha", string(&product_commit)),
        ("state", string("passed")),
        ("remote", string("fixture")),
        ("target", string("main")),
        ("gate", object([("rc", number(0))])),
        (
            "push",
            object([
                ("rc", number(0)),
                ("moved", number(1)),
                ("base", string(&base_commit)),
            ]),
        ),
        ("ci", object([("conclusion", string("success"))])),
    ]))
    .unwrap();
    fs::write(&receipt_path, &land_receipt).unwrap();
    fs::set_permissions(&receipt_path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir_all(r.join(".oms/artifacts")).unwrap();
    let artifact_path = ".oms/artifacts/commit-product.md";
    let artifact = b"The fixture product implements commit-obligation.\n";
    fs::write(r.join(artifact_path), artifact).unwrap();
    call(
        &[
            "review",
            "--id",
            "commit-product",
            "--artifact",
            artifact_path,
        ],
        false,
    )
    .unwrap();
    let finish = [
        "finish",
        "--id",
        "commit-product",
        "--landed-commit",
        &product_commit,
    ];
    let awaiting =
        String::from_utf8_lossy(&land_receipt).replace("\"passed\"", "\"awaiting-merge\"");
    fs::write(&receipt_path, awaiting).unwrap();
    let refused = call(&finish, false).unwrap_err();
    assert!(
        refused
            .message
            .contains("pull request is open and unmerged"),
        "{}",
        refused.message
    );
    // A squash merge reaches the target as a new commit with the head's tree;
    // only a receipt that names that merged commit proves the landing.
    let squash = git_text(&[
        "commit-tree",
        &product_tree,
        "-p",
        &base_commit,
        "-m",
        "actual completion product (squash)",
    ]);
    git(&r, &["update-ref", "refs/remotes/fixture/main", &squash]);
    fs::write(&receipt_path, &land_receipt).unwrap();
    assert!(
        call(&finish, false)
            .unwrap_err()
            .message
            .contains("not reachable")
    );
    let merged = |commit: &str| {
        String::from_utf8_lossy(&land_receipt).replace(
            "\"push\":",
            &format!("\"merge\":{{\"commit\":\"{commit}\",\"state\":\"MERGED\"}},\"push\":"),
        )
    };
    let other_tree = git_text(&[
        "commit-tree",
        "HEAD^^{tree}",
        "-p",
        &base_commit,
        "-m",
        "other",
    ]);
    git(
        &r,
        &["update-ref", "refs/remotes/fixture/main", &other_tree],
    );
    fs::write(&receipt_path, merged(&other_tree)).unwrap();
    assert!(call(&finish, false).is_err());
    git(&r, &["update-ref", "refs/remotes/fixture/main", &squash]);
    let land_receipt = merged(&squash).into_bytes();
    fs::write(&receipt_path, &land_receipt).unwrap();
    call(&finish, false).unwrap();

    let parent = json::parse(&fs::read(r.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    let dependent = parent.get("tasks").get("commit-dependent");
    let mut dependent_spec = spec.clone();
    for (key, value) in [
        ("id", string("team-commit-dependent")),
        ("generation", number(1)),
        ("previous_sha256", string("")),
        ("task_id", string("commit-dependent")),
        ("task_sha256", string(json::hash(dependent).unwrap())),
        (
            "criterion_sha256",
            string(oms_command_plan::team::criterion_digest(&parent, dependent).unwrap()),
        ),
        ("base_sha", string(&product_commit)),
    ] {
        dependent_spec.set(key, value).unwrap();
    }
    let dependent_spec_path = ".oms/plan/commit-dependent-team.json";
    fs::write(
        r.join(dependent_spec_path),
        json::canonical(&dependent_spec).unwrap(),
    )
    .unwrap();
    call(
        &["team-register", "--team-spec", dependent_spec_path],
        false,
    )
    .unwrap();
    let dependent_request = oms_command_plan::team::AdmissionRequest {
        team_id: "team-commit-dependent".into(),
        spec_sha256: json::hash(&dependent_spec).unwrap(),
        member_generation: large_generation,
        ..admission.clone()
    };
    assert!(
        admission_plan
            .team_admission_before(dependent_request.clone(), deadline())
            .err()
            .unwrap()
            .message
            .contains("dependencies")
    );

    let plan_raw = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let parent = json::parse(&plan_raw).unwrap();
    let original = parent.get("tasks").get("commit-obligation");
    let product = parent.get("tasks").get("commit-product");
    assert_eq!(product.text("state"), "done");
    assert_eq!(product.get("landing").text("sha"), product_commit);
    let manifest = Value::Array(
        ["src/product.txt", "verify-product.sh"]
            .into_iter()
            .map(|path| {
                object([
                    ("path", string(path)),
                    ("state", string("file")),
                    ("mode", string("100644")),
                    ("sha256", string(json::sha(fs::read(r.join(path)).unwrap()))),
                ])
            })
            .collect(),
    );
    let bundle = object([
        ("schema", number(1)),
        ("kind", string("satisfied-by")),
        ("plan_id", parent.get("plan_id").clone()),
        ("task_id", original.get("id").clone()),
        (
            "task_sha256",
            string(json::receipt_digest(original).unwrap()),
        ),
        ("plan_sha256", string(json::sha(&plan_raw))),
        ("state", original.get("state").clone()),
        ("lease_id", original.get("lease_id").clone()),
        (
            "owner",
            object([
                ("provider", string("codex")),
                ("participant", string("main-a")),
                ("parent_provider", string("codex")),
            ]),
        ),
        ("original_contract", json::projection(original).unwrap()),
        (
            "review_summary",
            string("The exact committed product fulfills the original obligation."),
        ),
        (
            "reviewed_obligations",
            Value::Array(vec![string("product")]),
        ),
        (
            "requirements",
            Value::Array(vec![object([
                ("id", string("product")),
                ("obligation", original.get("title").clone()),
                (
                    "deliverable",
                    string("src/product.txt in the exact committed tree"),
                ),
                (
                    "source_paths",
                    Value::Array(vec![string("src/product.txt")]),
                ),
                (
                    "evidence",
                    Value::Array(vec![object([
                        ("path", string(artifact_path)),
                        ("sha256", string(json::sha(artifact))),
                    ])]),
                ),
            ])]),
        ),
        ("base_commit", string(&base_commit)),
        (
            "source_envelope",
            Value::Array(vec![string("src/product.txt"), string("verify-product.sh")]),
        ),
        ("source_manifest", manifest.clone()),
        (
            "verifier",
            object([
                ("command", string("bash verify-product.sh")),
                ("adopted", Value::Bool(false)),
                (
                    "review_summary",
                    string("Frozen script checks the actual product bytes."),
                ),
                (
                    "files",
                    Value::Array(vec![manifest.array().unwrap()[1].clone()]),
                ),
            ]),
        ),
        (
            "successor",
            object([
                ("plan_id", parent.get("plan_id").clone()),
                ("task_id", product.get("id").clone()),
                (
                    "task_sha256",
                    string(json::receipt_digest(product).unwrap()),
                ),
                ("original_contract", json::projection(product).unwrap()),
                ("product_kind", string("commit")),
                ("commit_sha", string(&product_commit)),
                ("land_receipt_sha256", string(json::sha(&land_receipt))),
            ]),
        ),
    ]);
    let bundle_path = ".oms/plan/commit-product-bundle.json";
    let bundle_raw = json::canonical(&bundle).unwrap();
    fs::write(r.join(bundle_path), &bundle_raw).unwrap();
    let satisfy = [
        "satisfy",
        "--id",
        "commit-obligation",
        "--completion-bundle",
        bundle_path,
        "--expected-completion-bundle-sha256",
        &json::sha(&bundle_raw),
        "--expected-plan-sha256",
        bundle.text("plan_sha256"),
        "--expected-task-sha256",
        bundle.text("task_sha256"),
        "--expected-state",
        bundle.text("state"),
        "--lease-id",
        bundle.text("lease_id"),
    ];
    fs::write(r.join("src/product.txt"), b"stale product\n").unwrap();
    assert!(
        call(&satisfy, false)
            .unwrap_err()
            .message
            .contains("source content/mode changed")
    );
    assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), plan_raw);
    fs::write(r.join("src/product.txt"), b"product\n").unwrap();
    call(&satisfy, false).unwrap();
    let completed_raw = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let completed = json::parse(&completed_raw).unwrap();
    let completed = completed.get("tasks").get("commit-obligation");
    assert_eq!(completed.text("completion_kind"), "satisfied-by");
    assert_eq!(completed.text("state"), "done");
    let completion_path = r.join(completed.get("completion_receipt").text("path"));
    let completion_raw = fs::read(&completion_path).unwrap();
    let completion = json::parse(&completion_raw).unwrap();
    assert_eq!(
        completion
            .get("snapshot")
            .get("evidence")
            .text("product_kind"),
        "commit"
    );
    assert_eq!(completion.get("snapshot").text("head"), product_commit);
    assert_eq!(git_text(&["rev-parse", "HEAD^{tree}"]), product_tree);

    REFUSE_ORDINARY_GROUP.store(true, Ordering::Release);
    // One guard spans all of the Git and receipt mutations below.
    let proof_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let guard = admission_plan
        .team_admission_before(dependent_request.clone(), proof_deadline)
        .unwrap();
    guard.check().unwrap();
    fs::write(&receipt_path, b"{}\n").unwrap();
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("no passed oms land receipt")
    );
    fs::write(&receipt_path, &land_receipt).unwrap();
    guard.check().unwrap();
    fs::write(&completion_path, b"{}\n").unwrap();
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("evidence digest changed")
    );
    fs::write(&completion_path, &completion_raw).unwrap();
    guard.check().unwrap();
    git(&r, &["update-ref", "-d", "refs/remotes/fixture/main"]);
    assert!(guard.check().unwrap_err().message.contains("not reachable"));
    git(&r, &["update-ref", "refs/remotes/fixture/main", &squash]);
    guard.check().unwrap();
    drop(guard);
    fs::write(r.join(artifact_path), b"tampered product report\n").unwrap();
    assert!(
        admission_plan
            .team_admission_before(dependent_request.clone(), deadline())
            .err()
            .unwrap()
            .message
            .contains("evidence digest changed")
    );
    fs::write(r.join(artifact_path), artifact).unwrap();
    git(&r, &["commit", "--allow-empty", "-qm", "a new Team base"]);
    assert!(
        admission_plan
            .team_admission_before(dependent_request.clone(), deadline())
            .err()
            .unwrap()
            .message
            .contains("team base changed")
    );
    git(&r, &["update-ref", "HEAD", &product_commit]);
    admission_plan
        .team_admission_before(dependent_request, deadline())
        .unwrap()
        .check()
        .unwrap();
    REFUSE_ORDINARY_GROUP.store(false, Ordering::Release);
    assert_eq!(
        fs::read(r.join(".oms/plan/tasks.json")).unwrap(),
        completed_raw
    );
    let req = Request::new(Action::TeamList);
    let mut ctx = Context::from_environment(&req, &r).unwrap();
    ctx.child = true;
    assert!(Plan::open(ctx, &host, &stop).unwrap().execute(&req).is_ok());
}

#[test]
#[ignore = "driven by versioned_contracts_gate_scheduling"]
fn contract_child_accept() {
    let repo = PathBuf::from(std::env::var_os("CONTRACT_TEST_REPO").unwrap());
    let (code, _, err) = go(&repo, &["contract-accept", "--id", "api", "--version", "2"]);
    assert_eq!(code, 2);
    assert!(err.contains("parent-only"), "{err}");
    let (code, _, err) = go(&repo, &["refresh-contracts", "--task", "downstream"]);
    assert_eq!(code, 2);
    assert!(err.contains("parent-only"), "{err}");
}

#[test]
fn contract_registry_refuses_unsafe_inputs_and_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    if isolated("contract_registry_refuses_unsafe_inputs_and_permissions") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let run = |rest: &[&str]| go(&r, rest);
    assert_eq!(run(&["init", "--goal", "g"]).0, 0);
    assert_eq!(
        run(&["contract-propose", "--id", "api", "--summary", " "]).0,
        2
    );
    assert_eq!(
        run(&[
            "contract-propose",
            "--id",
            "api",
            "--summary",
            "x",
            "--scope-paths",
            "../bad"
        ])
        .0,
        2
    );
    assert_eq!(
        run(&[
            "contract-propose",
            "--id",
            "api",
            "--summary",
            "x",
            "--decision-artifact",
            "../bad"
        ])
        .0,
        2
    );
    symlink("a.txt", r.join("link")).unwrap();
    assert_eq!(
        run(&[
            "contract-propose",
            "--id",
            "api",
            "--summary",
            "x",
            "--decision-artifact",
            "link"
        ])
        .0,
        2
    );
    assert_eq!(
        run(&[
            "contract-propose",
            "--id",
            "api",
            "--summary",
            "x",
            "--decision-artifact",
            "a.txt"
        ])
        .0,
        0
    );
    let path = r.join(".oms/plan/contracts.json");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(run(&["contract-list"]).0, 2);
}

#[test]
fn add_batch_is_atomic_replay_safe_and_cas_bound() {
    if isolated("add_batch_is_atomic_replay_safe_and_cas_bound") {
        return;
    }
    use oms_command_plan::{Context, Plan, Request, json};
    let tmp = Tmp::new();
    let file = tmp.0.join("batch.json");
    let apply_in = |repo_path: &Path, body: &str, plan: &str| {
        fs::write(&file, body).unwrap();
        let sha = json::sha(body.as_bytes());
        go(
            &tmp.0,
            &[
                "--repo",
                repo_path.to_str().unwrap(),
                "add-batch",
                "--tasks",
                file.to_str().unwrap(),
                "--expected-tasks-sha256",
                &sha,
                "--expected-plan-sha256",
                plan,
            ],
        )
    };
    // Fresh absent plan: refuses 64-hex SHA, accepts `absent`, mints lineage, and replays cleanly.
    let fresh = repo_at(&tmp, "fresh");
    let fresh_plan = fresh.join(".oms/plan/tasks.json");
    let one = r#"{"schema":1,"kind":"agent-plan-batch","tasks":[{"id":"f0","title":"fresh 0","allowed":["src"]}]}"#;
    let stale = "0".repeat(64);
    assert_eq!(apply_in(&fresh, one, &stale).0, 2);
    assert!(!fresh_plan.exists());
    let (c, o, e) = apply_in(&fresh, one, "absent");
    assert_eq!(c, 0, "{e}");
    assert!(o.contains("batch added"), "{o}");
    let fresh_bytes = fs::read(&fresh_plan).unwrap();
    let (_, fresh_list, _) = go(
        &tmp.0,
        &["--repo", fresh.to_str().unwrap(), "list", "--json"],
    );
    let fresh_doc = json::parse(fresh_list.as_bytes()).unwrap();
    assert!(fresh_doc.text("plan_id").starts_with("plan_"));
    assert_eq!(fresh_doc.get("tasks").array().unwrap().len(), 1);
    let (c, o, e) = apply_in(&fresh, one, "absent");
    assert_eq!(c, 0, "{e}");
    assert!(o.contains("already applied"), "{o}");
    assert_eq!(fresh_bytes, fs::read(&fresh_plan).unwrap());
    let two = r#"{"schema":1,"kind":"agent-plan-batch","tasks":[{"id":"f1","title":"fresh 1"}]}"#;
    assert_eq!(apply_in(&fresh, two, "absent").0, 2);
    assert_eq!(fresh_bytes, fs::read(&fresh_plan).unwrap());

    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    let run = |rest: &[&str]| {
        let mut command = vec!["--repo", rs];
        command.extend_from_slice(rest);
        go(&tmp.0, &command)
    };
    assert_eq!(run(&["init", "--goal", "g"]).0, 0);
    assert_eq!(run(&["add", "--id", "pre0", "--title", "existing"]).0, 0);
    let rows = |n: usize, last: &str| {
        let mut v = (0..n)
            .map(|i| {
                format!(
                    r#"{{"id":"t{i}","title":"task {i}","allowed":["d{i}"],"verify":"true","role":"builder","priority":{i}}}"#
                )
            })
            .collect::<Vec<_>>();
        v.push(last.to_owned());
        format!(
            r#"{{"schema":1,"kind":"agent-plan-batch","tasks":[{}]}}"#,
            v.join(",")
        )
    };
    let apply = |body: &str, plan: &str| apply_in(&r, body, plan);
    let plan_path = r.join(".oms/plan/tasks.json");
    let plan_sha = || json::sha(fs::read(&plan_path).unwrap());
    let before = fs::read(&plan_path).unwrap();
    let pre_batch_sha = plan_sha();
    for bad in [
        r#"{"id":"bad","title":"x","depends":["nope"]}"#,
        r#"{"id":"bad","title":"x","allowed":[""]}"#,
        r#"{"id":"bad","title":"x","allowed":["   "]}"#,
        r#"{"id":"bad","title":"x","allowed":["/"]}"#,
        r#"{"id":"bad","title":"x","forbidden":["/etc/passwd"]}"#,
        r#"{"id":"bad","title":"x","allowed":["[z-a]"]}"#,
        r#"{"id":"bad","title":"a","title":"b"}"#,
    ] {
        let (c, _, e) = apply(&rows(9, bad), &pre_batch_sha);
        assert_eq!(c, 2, "{bad}: {e}");
        assert_eq!(before, fs::read(&plan_path).unwrap(), "{bad}");
    }
    let good = rows(
        9,
        r#"{"id":"t9","title":"task 9","allowed":["d9"],"verify":"true","depends":["t0","t1"]}"#,
    );
    let (c, _, e) = apply(&good, &stale);
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("plan changed"), "{e}");
    assert_eq!(before, fs::read(&plan_path).unwrap());
    assert_eq!(apply(&good, "absent").0, 2);
    assert_eq!(before, fs::read(&plan_path).unwrap());
    // Parent-only authority check: child context is refused before mutation.
    fs::write(&file, &good).unwrap();
    let good_sha = json::sha(good.as_bytes());
    let child_req = Request::parse(&args(&[
        "--repo",
        rs,
        "add-batch",
        "--tasks",
        file.to_str().unwrap(),
        "--expected-tasks-sha256",
        &good_sha,
        "--expected-plan-sha256",
        &pre_batch_sha,
    ]))
    .unwrap();
    let mut child_ctx = Context::from_environment(&child_req, &r).unwrap();
    child_ctx.child = true;
    let stop = AtomicBool::new(false);
    let child_err = Plan::open(child_ctx, &Host, &stop)
        .unwrap()
        .execute(&child_req)
        .unwrap_err();
    assert_eq!(child_err.code, 2);
    assert!(child_err.message.contains("parent-only"), "{child_err}");
    assert_eq!(before, fs::read(&plan_path).unwrap());

    let (c, o, e) = apply(&good, &pre_batch_sha);
    assert_eq!(c, 0, "{e}");
    assert!(o.contains("batch added"), "{o}");
    let (c, listed, e) = run(&["list", "--json"]);
    assert_eq!(c, 0, "{e}");
    let doc = json::parse(listed.as_bytes()).unwrap();
    let persisted = doc.get("tasks").array().unwrap();
    assert_eq!(persisted.len(), 11, "pre0 + 10 batch tasks: {listed}");
    for id in [
        "pre0", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9",
    ] {
        assert!(persisted.iter().any(|t| t.text("id") == id), "missing {id}");
    }
    let t9 = persisted.iter().find(|t| t.text("id") == "t9").unwrap();
    assert_eq!(t9.get("depends").strings().unwrap(), vec!["t0", "t1"]);
    assert_eq!(t9.get("allowed_paths").strings().unwrap(), vec!["d9"]);
    assert_eq!(t9.text("verify"), "true");
    let t3 = persisted.iter().find(|t| t.text("id") == "t3").unwrap();
    assert_eq!(t3.text("title"), "task 3");
    assert_eq!(t3.text("role"), "builder");
    assert!(t3.get("priority").is_int(3));
    let (_, ready, _) = run(&["ready"]);
    assert_eq!(ready.lines().count(), 10, "pre0 + t0..t8 ready: {ready}");
    assert!(!ready.contains("t9"), "{ready}");

    assert_eq!(run(&["claim", "--id", "t0", "--provider", "codex"]).0, 0);
    let after = fs::read(&plan_path).unwrap();
    assert_ne!(plan_sha(), pre_batch_sha);
    // Exact replay with the ORIGINAL pre-batch token succeeds and preserves the claim.
    let (c, o, e) = apply(&good, &pre_batch_sha);
    assert_eq!(
        c, 0,
        "replay after claim with the original pre-batch sha: {e}"
    );
    assert!(o.contains("already applied"), "{o}");
    assert_eq!(after, fs::read(&plan_path).unwrap());
    // Syntactically invalid or missing plan CAS token is rejected BEFORE replay.
    assert_eq!(apply(&good, "not-a-sha").0, 2);
    assert_eq!(after, fs::read(&plan_path).unwrap());
    let (c, _, _) = run(&[
        "add-batch",
        "--tasks",
        file.to_str().unwrap(),
        "--expected-tasks-sha256",
        &good_sha,
    ]);
    assert_eq!(c, 2);
    assert_eq!(after, fs::read(&plan_path).unwrap());
    let changed = good.replace("task 3", "other");
    let (c, _, e) = apply(&changed, &pre_batch_sha);
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("does not match"), "{e}");
    assert_eq!(after, fs::read(&plan_path).unwrap());
    let partial = r#"{"schema":1,"kind":"agent-plan-batch","tasks":[{"id":"t0","title":"task 0","allowed":["d0"],"verify":"true","role":"builder","priority":0},{"id":"t10","title":"task 10"}]}"#;
    let (c, _, e) = apply(partial, &plan_sha());
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("partially present"), "{e}");
    assert_eq!(after, fs::read(&plan_path).unwrap());
}

#[test]
fn github_issue_links_stay_local_same_repo_and_mode_gated() {
    if isolated("github_issue_links_stay_local_same_repo_and_mode_gated") {
        return;
    }
    use oms_command_plan::json;
    const REPO: &str = "example-org/example-repo";
    let tmp = Tmp::new();
    let at = |r: &Path, rest: &[&str]| {
        let mut command = vec!["--repo", r.to_str().unwrap()];
        command.extend_from_slice(rest);
        go(&tmp.0, &command)
    };
    let plan = |r: &Path| fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let task = |r: &Path, id: &str| {
        let (c, o, e) = at(r, &["show", "--id", id]);
        assert_eq!(c, 0, "{e}");
        json::parse(o.as_bytes()).unwrap()
    };
    // No origin and a non-GitHub origin refuse --issue without writing.
    for (name, origin) in [
        ("bare", None),
        (
            "gitlab",
            Some("https://gitlab.com/example-org/example-repo.git"),
        ),
    ] {
        let other = repo_at(&tmp, name);
        if let Some(url) = origin {
            git(&other, &["remote", "add", "origin", url]);
        }
        assert_eq!(at(&other, &["init", "--goal", "g"]).0, 0);
        let before = plan(&other);
        let (c, _, e) = at(
            &other,
            &["add", "--id", "x", "--title", "x", "--issue", "5"],
        );
        assert_eq!(c, 2, "{name}: {e}");
        assert!(e.contains("origin"), "{name}: {e}");
        assert_eq!(before, plan(&other), "{name}");
    }

    let r = repo(&tmp);
    git(
        &r,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:example-org/example-repo.git",
        ],
    );
    assert_eq!(at(&r, &["init", "--goal", "g"]).0, 0);
    // Mode off: an unbound task keeps its exact pre-issue shape and digest.
    assert_eq!(at(&r, &["add", "--id", "plain", "--title", "p"]).0, 0);
    let stored = json::parse(&plan(&r))
        .unwrap()
        .get("tasks")
        .get("plain")
        .clone();
    assert!(stored.opt("issue").is_none() && stored.opt("pr").is_none());
    let shown = task(&r, "plain");
    assert!(shown.opt("issue").is_none() && shown.opt("issue_cache").is_none());
    assert_eq!(shown.text("task_sha256"), json::hash(&stored).unwrap());
    for (id, input, number) in [
        ("i1", "7", 7),
        ("i2", "#8", 8),
        ("i3", "example-org/example-repo#9", 9),
        ("i4", "Example-Org/EXAMPLE-REPO#10", 10),
    ] {
        let (c, _, e) = at(&r, &["add", "--id", id, "--title", id, "--issue", input]);
        assert_eq!(c, 0, "{input}: {e}");
        assert_eq!(task(&r, id).text("issue"), format!("{REPO}#{number}"));
    }
    let before = plan(&r);
    for bad in [
        "other-org/example-repo#5",
        "0",
        "#",
        "five",
        "https://github.com/example-org/example-repo/issues/5",
    ] {
        let (c, _, e) = at(&r, &["add", "--id", "bad", "--title", "x", "--issue", bad]);
        assert_eq!(c, 2, "{bad}: {e}");
        assert_eq!(before, plan(&r), "{bad}");
    }

    // Cached status comes only from the local snapshot cache.
    let unavailable = task(&r, "i1");
    let cache = unavailable.get("issue_cache");
    assert_eq!(cache.text("freshness"), "unavailable");
    assert_eq!(cache.text("reason"), "cache missing");
    assert!(
        cache.get("state") == &json::Value::Null && cache.get("captured_at") == &json::Value::Null
    );
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let row = |n: u32, state: &str, age: i64, ttl: u64, title: &str| {
        format!(
            r#"{{"reference":"{REPO}#{n}","kind":"issue","state":"{state}","title":"{title}","labels":[],"body_sha256":"{}","captured_at":{},"ttl_s":{ttl}}}"#,
            "b".repeat(64),
            now - age
        )
    };
    let cache_file = r.join(".oms/github/cache.jsonl");
    fs::create_dir_all(&cache_file).unwrap();
    assert_eq!(
        task(&r, "i1").get("issue_cache").text("reason"),
        "cache unreadable"
    );
    fs::remove_dir(&cache_file).unwrap();
    let pull_row = |n, state, age, ttl| {
        row(n, state, age, ttl, "A pull").replace(r#""kind":"issue""#, r#""kind":"pull""#)
    };
    fs::write(
        cache_file,
        [
            row(7, "open", 10, 3600, "Remote title"),
            row(8, "closed", 7200, 60, "Old"),
            pull_row(9, "merged", 10, 3600),
            pull_row(10, "open", 10, 3600),
            pull_row(12, "closed", 7200, 60),
        ]
        .join("\n"),
    )
    .unwrap();
    for (id, freshness, state) in [
        ("i1", "fresh", "open"),
        ("i2", "stale", "closed"),
        ("i3", "unavailable", ""),
    ] {
        let cache = task(&r, id).get("issue_cache").clone();
        assert_eq!(
            (cache.text("freshness"), cache.text("state")),
            (freshness, state),
            "{id}"
        );
    }
    assert_eq!(
        task(&r, "i1").get("issue_cache").text("title"),
        "Remote title"
    );
    // #9 has only a kind=pull row: never shown as issue state.
    let pull = task(&r, "i3").get("issue_cache").clone();
    assert_eq!(pull.text("reason"), "no matching cache entry");
    assert!(
        ["state", "captured_at", "title"]
            .iter()
            .all(|k| pull.get(k) == &json::Value::Null)
    );
    let (_, listed, _) = at(&r, &["list"]);
    assert!(
        listed.contains(&format!("issue {REPO}#7 (open, fresh)")),
        "{listed}"
    );
    assert!(
        listed.contains(&format!(
            "issue {REPO}#9 (unknown, unavailable: no matching cache entry)"
        )),
        "{listed}"
    );
    assert!(!listed.contains("Remote title"), "{listed}");
    let plain_line = listed.lines().find(|l| l.starts_with("plain")).unwrap();
    assert!(!plain_line.contains("issue"), "{plain_line}");
    let (_, brief, _) = at(&r, &["brief", "--id", "i2"]);
    assert!(
        brief.contains(&format!("issue: {REPO}#8 (closed, stale)")),
        "{brief}"
    );
    let (_, listed, _) = at(&r, &["list", "--json"]);
    let rows = json::parse(listed.as_bytes()).unwrap();
    let rows = rows.get("tasks").array().unwrap();
    assert!(
        rows.iter()
            .all(|t| t.opt("issue_cache").is_some() == t.opt("issue").is_some())
    );

    // Review candidates are observations. The underlying task state and
    // evidence digests stay on the original review.
    fs::create_dir_all(r.join(".oms/artifacts")).unwrap();
    let artifact = ".oms/artifacts/review.txt";
    fs::write(r.join(artifact), "review fixture").unwrap();
    for id in ["i1", "i2", "i3", "i4"] {
        assert_eq!(at(&r, &["claim", "--id", id, "--provider", "codex"]).0, 0);
        assert_eq!(at(&r, &["review", "--id", id, "--artifact", artifact]).0, 0);
    }
    let mut stored = json::parse(&plan(&r)).unwrap();
    let mut tasks = stored.get("tasks").clone();
    let mut pinned = tasks.get("i1").clone();
    pinned
        .set("pr", json::string(format!("{REPO}#12")))
        .unwrap();
    tasks.set("i1", pinned).unwrap();
    stored.set("tasks", tasks).unwrap();
    let original = json::canonical(&stored).unwrap();
    fs::write(r.join(".oms/plan/tasks.json"), &original).unwrap();
    for (id, n, state, disposition, freshness) in [
        ("i1", 12, "closed", "superseded", "stale"),
        ("i3", 9, "merged", "completed-external", "fresh"),
    ] {
        let shown = task(&r, id);
        assert_eq!(shown.text("state"), "review");
        let candidate = shown.get("pr_review_candidate");
        assert_eq!(candidate.text("reference"), format!("{REPO}#{n}"));
        assert_eq!(candidate.text("state"), state);
        assert_eq!(candidate.text("disposition"), disposition);
        assert_eq!(candidate.text("freshness"), freshness);
        let (_, review, _) = at(&r, &["evidence-snapshot", "--id", id]);
        let review = json::parse(review.as_bytes()).unwrap();
        assert!(review.opt("pr_review_candidate").is_none());
        assert_eq!(review.text("task_sha256"), shown.text("task_sha256"));
    }
    for id in ["i2", "i4"] {
        assert!(task(&r, id).opt("pr_review_candidate").is_none());
    }
    let (_, listed, _) = at(&r, &["list"]);
    assert!(listed.contains(&format!(
        "PR {REPO}#9 merged (fresh); completed-external candidate"
    )));
    let (_, status, _) = at(&r, &["status", "--json"]);
    let status = json::parse(status.as_bytes()).unwrap();
    assert_eq!(status.get("pr_review_candidates").array().unwrap().len(), 2);
    assert_eq!(status.get("by_state").get("review"), &json::number(4));
    assert_eq!(status.get("closed_count"), &json::number(0));
    assert_eq!(plan(&r), original);

    // link: idempotent, conflicting refs need --reason, active tasks refuse.
    let (c, _, e) = at(&r, &["link", "--task", "plain", "--issue", "11"]);
    assert_eq!(c, 0, "{e}");
    let linked = plan(&r);
    let (c, o, e) = at(
        &r,
        &["link", "--task", "plain", "--issue", &format!("{REPO}#11")],
    );
    assert_eq!(c, 0, "{e}");
    assert!(o.contains("already linked"), "{o}");
    assert_eq!(linked, plan(&r));
    let (c, _, e) = at(&r, &["link", "--task", "plain", "--issue", "12"]);
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("--reason"), "{e}");
    assert_eq!(linked, plan(&r));
    let (c, _, e) = at(
        &r,
        &[
            "link", "--task", "plain", "--issue", "12", "--reason", "split",
        ],
    );
    assert_eq!(c, 0, "{e}");
    let relinked = task(&r, "plain");
    assert_eq!(relinked.text("issue"), format!("{REPO}#12"));
    let event = relinked
        .get("history")
        .array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(
        (
            event.text("kind"),
            event.text("before"),
            event.text("after"),
            event.text("reason")
        ),
        (
            "issue-relinked",
            format!("{REPO}#11").as_str(),
            format!("{REPO}#12").as_str(),
            "split"
        )
    );
    assert_eq!(at(&r, &["add", "--id", "c1", "--title", "c"]).0, 0);
    assert_eq!(at(&r, &["claim", "--id", "c1", "--provider", "codex"]).0, 0);
    let claimed = plan(&r);
    let (c, _, e) = at(&r, &["link", "--task", "c1", "--issue", "13"]);
    assert_eq!(c, 2, "{e}");
    assert_eq!(claimed, plan(&r));
    for bad in [
        &["link", "--task", "c1"][..],
        &["show", "--id", "c1", "--issue", "1"],
    ] {
        assert_eq!(at(&r, bad).0, 2, "{bad:?}");
    }

    // Project mode on: local-only backlog is refused; decomposition inherits.
    let project = |decl: &str| {
        fs::write(
            r.join("PROJECT.md"),
            format!("# P\n\n## Collaboration\n\n{decl}\n"),
        )
        .unwrap()
    };
    project(&format!("- GitHub project: {REPO}"));
    let before = plan(&r);
    let (c, _, e) = at(&r, &["add", "--id", "m1", "--title", "m"]);
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("GitHub project mode"), "{e}");
    assert_eq!(
        at(
            &r,
            &[
                "add",
                "--id",
                "m1",
                "--title",
                "m",
                "--discovered-from",
                "c1"
            ]
        )
        .0,
        2
    );
    assert_eq!(before, plan(&r));
    let (c, _, e) = at(
        &r,
        &[
            "add",
            "--id",
            "m2",
            "--title",
            "m",
            "--discovered-from",
            "i1",
        ],
    );
    assert_eq!(c, 0, "{e}");
    assert_eq!(task(&r, "m2").text("issue"), format!("{REPO}#7"));
    // An inherited value is revalidated: a hand-edited foreign parent issue fails closed.
    let edited = String::from_utf8(plan(&r)).unwrap().replacen(
        &format!("{REPO}#10\""),
        "other-org/other-repo#5\"",
        1,
    );
    assert!(edited.contains("other-org/other-repo#5"));
    fs::write(r.join(".oms/plan/tasks.json"), &edited).unwrap();
    let (c, _, e) = at(
        &r,
        &[
            "add",
            "--id",
            "m4",
            "--title",
            "m",
            "--discovered-from",
            "i4",
        ],
    );
    assert_eq!(c, 2, "{e}");
    assert_eq!(edited.as_bytes(), plan(&r));
    assert_eq!(
        at(&r, &["add", "--id", "m3", "--title", "m", "--issue", "14"]).0,
        0
    );
    let file = tmp.0.join("batch.json");
    let batch = |body: &str| {
        fs::write(&file, body).unwrap();
        let sha = json::sha(body.as_bytes());
        let current = json::sha(plan(&r));
        at(
            &r,
            &[
                "add-batch",
                "--tasks",
                file.to_str().unwrap(),
                "--expected-tasks-sha256",
                &sha,
                "--expected-plan-sha256",
                &current,
            ],
        )
    };
    let before = plan(&r);
    let (c, _, e) = batch(
        r#"{"schema":1,"kind":"agent-plan-batch","tasks":[{"id":"b1","title":"b","issue":"15"},{"id":"b2","title":"b"}]}"#,
    );
    assert_eq!(c, 2, "{e}");
    assert_eq!(before, plan(&r));
    assert_eq!(batch(r#"{"schema":1,"kind":"agent-plan-batch","tasks":[{"id":"b1","title":"b","issue":"nope"}]}"#).0, 2);
    let (c, _, e) = batch(
        r#"{"schema":1,"kind":"agent-plan-batch","tasks":[{"id":"b1","title":"b","issue":"15"}]}"#,
    );
    assert_eq!(c, 0, "{e}");
    assert_eq!(task(&r, "b1").text("issue"), format!("{REPO}#15"));

    // A mismatched or malformed declaration fails closed; removing it restores mode off.
    for decl in [
        "- GitHub project: example-org/other-repo",
        "- GitHub project: not a slug",
    ] {
        project(decl);
        let before = plan(&r);
        for command in [
            &["add", "--id", "z", "--title", "z", "--issue", "16"][..],
            &[
                "add",
                "--id",
                "z",
                "--title",
                "z",
                "--discovered-from",
                "i1",
            ],
            &["link", "--task", "c1", "--issue", "16"],
            &["add", "--id", "z", "--title", "z"],
        ] {
            let (c, _, e) = at(&r, command);
            assert_eq!(c, 2, "{decl} {command:?}: {e}");
            assert_eq!(before, plan(&r), "{decl} {command:?}");
        }
    }
    // A non-UTF-8 PROJECT.md that still names the declaration fails closed.
    let mut broken = format!("# P\n\n## Collaboration\n\n- GitHub project: {REPO}\n").into_bytes();
    broken.push(0xff);
    fs::write(r.join("PROJECT.md"), &broken).unwrap();
    let before = plan(&r);
    let (c, _, e) = at(&r, &["add", "--id", "z", "--title", "z"]);
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("not UTF-8"), "{e}");
    assert_eq!(before, plan(&r));
    project("- Collaboration mode: careful");
    assert_eq!(at(&r, &["add", "--id", "z", "--title", "z"]).0, 0);
    assert!(task(&r, "z").opt("issue").is_none());

    // A committed .github/oms-project.md alone turns the mode on.
    let tracked = repo_at(&tmp, "tracked");
    git(
        &tracked,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:example-org/example-repo.git",
        ],
    );
    fs::create_dir_all(tracked.join(".github")).unwrap();
    fs::write(
        tracked.join(".github/oms-project.md"),
        format!("# P\n\n## Collaboration\n\n- GitHub project: {REPO}\n"),
    )
    .unwrap();
    git(&tracked, &["add", ".github"]);
    git(&tracked, &["commit", "-qm", "declare"]);
    assert_eq!(at(&tracked, &["init", "--goal", "g"]).0, 0);
    let before = plan(&tracked);
    let (c, _, e) = at(&tracked, &["add", "--id", "t", "--title", "t"]);
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("GitHub project mode"), "{e}");
    assert_eq!(before, plan(&tracked));
    assert_eq!(
        at(
            &tracked,
            &["add", "--id", "t", "--title", "t", "--issue", "3"]
        )
        .0,
        0
    );
    assert_eq!(task(&tracked, "t").text("issue"), format!("{REPO}#3"));
    // A fork: the tracked upstream declaration does not name origin, so the mode is off.
    git(
        &tracked,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:fork-org/example-repo.git",
        ],
    );
    let (c, _, e) = at(&tracked, &["add", "--id", "f", "--title", "f"]);
    assert_eq!(c, 0, "{e}");
    assert!(task(&tracked, "f").opt("issue").is_none());

    // Reviewed proposals carry issues through the same validation.
    let p = repo_at(&tmp, "proposed");
    git(
        &p,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example-org/example-repo",
        ],
    );
    let spec =
        format!("# P\n\n- State: confirmed\n\n## Collaboration\n\n- GitHub project: {REPO}\n");
    fs::write(p.join("PROJECT.md"), &spec).unwrap();
    let head = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&p)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let proposal = |top: &str, extra: &str| {
        let body = format!(
            r#"{{"schema":1,"kind":"agent-plan-proposal","spec_sha256":"{}","plan_sha256":"absent","base_sha":"{}","id_prefix":"","allowed_envelope":["src"],"acceptance_files":[],{top}"tasks":[{{"id":"p1","title":"p","allowed":["src"],"verify":"true","depends":[]{extra}}}]}}"#,
            json::sha(spec.as_bytes()),
            head.trim()
        );
        let path = tmp.0.join("proposal.json");
        fs::write(&path, &body).unwrap();
        at(
            &p,
            &[
                "apply-proposal",
                "--proposal",
                path.to_str().unwrap(),
                "--expected-proposal-sha256",
                &json::sha(body.as_bytes()),
                "--expected-plan-sha256",
                "absent",
                "--goal",
                "g",
                "--accept",
                "true",
                "--allowed-envelope",
                "src",
            ],
        )
    };
    let (c, _, e) = proposal("", "");
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("requires an issue"), "{e}");
    assert!(!p.join(".oms/plan/tasks.json").exists());
    assert_eq!(proposal("", r#","issue":"other-org/x#1""#).0, 2);
    // A top-level issue must equal every task's issue.
    let (c, _, e) = proposal(r##""issue":"#22","##, r##","issue":"#21""##);
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("must match every task issue"), "{e}");
    assert!(!p.join(".oms/plan/tasks.json").exists());
    let (c, _, e) = proposal(
        r##""issue":"#21","##,
        r##","issue":"#21","brief":"reviewed work""##,
    );
    assert_eq!(c, 0, "{e}");
    assert_eq!(task(&p, "p1").text("issue"), format!("{REPO}#21"));
    assert_eq!(task(&p, "p1").text("brief"), "reviewed work");
    let (c, _, e) = proposal("", r##","issue":"#21""##);
    assert_ne!(
        c, 0,
        "a replay without the reviewed brief must not count as applied"
    );
    assert!(e.contains("does not match reviewed proposal"), "{e}");
    let (c, o, e) = proposal("", r##","issue":"#21","brief":"reviewed work""##);
    assert_eq!(c, 0, "{e}");
    assert!(o.contains("already applied"), "{o}");
    let raw = fs::read(p.join(".oms/plan/tasks.json")).unwrap();
    let doc = json::parse(&raw).unwrap();
    let plan_hash = json::sha(&raw);
    let task_hash = json::hash(doc.get("tasks").get("p1")).unwrap();
    let brief_path = tmp.0.join("proposal-brief");
    fs::write(&brief_path, "revised work").unwrap();
    let path = brief_path.to_str().unwrap();
    for extra in ["--allowed", "--verify"] {
        let result = at(
            &p,
            &[
                "amend",
                "--id",
                "p1",
                "--expected-plan-sha256",
                &plan_hash,
                "--expected-task-sha256",
                &task_hash,
                "--reason",
                "update",
                "--brief-file",
                path,
                extra,
                "true",
            ],
        );
        assert_ne!(result.0, 0, "{result:?}");
    }
    let result = at(
        &p,
        &[
            "amend",
            "--id",
            "p1",
            "--expected-plan-sha256",
            &plan_hash,
            "--expected-task-sha256",
            &task_hash,
            "--reason",
            "update",
            "--brief-file",
            path,
        ],
    );
    assert_eq!(result.0, 0, "{result:?}");
    assert_eq!(task(&p, "p1").text("brief"), "revised work");
    let (c, _, e) = at(
        &p,
        &["link", "--task", "p1", "--issue", "22", "--reason", "x"],
    );
    assert_eq!(c, 2, "{e}");
    assert!(e.contains("proposal-bound"), "{e}");
}

#[test]
fn github_pr_recording_requires_accepted_same_repo_pr_url() {
    use oms_command_plan::json::{self, Value, number, object, string};
    use std::os::unix::fs::PermissionsExt;

    if std::env::var_os("OMS_PR_RECEIPT_FIXTURE_STATE").is_none() {
        let fixture = Tmp::new();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "github_pr_recording_requires_accepted_same_repo_pr_url",
            "--test-threads=1",
            "--nocapture",
        ]);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("OMS_") {
                command.env_remove(key);
            }
        }
        command.env("XDG_STATE_HOME", fixture.0.join("state"));
        command.env("OMS_PR_RECEIPT_FIXTURE_STATE", "1");
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        print!("{}", String::from_utf8_lossy(&output.stdout));
        return;
    }

    const REPO: &str = "example-org/example-repo";
    let tmp = Tmp::new();
    let r = repo(&tmp);
    git(
        &r,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:example-org/example-repo.git",
        ],
    );
    fs::write(r.join(".git/info/exclude"), b".oms/\n").unwrap();
    let at = |rest: &[&str]| {
        let mut command = vec!["--repo", r.to_str().unwrap()];
        command.extend_from_slice(rest);
        go(&tmp.0, &command)
    };
    let task = |id: &str| {
        let (c, o, e) = at(&["show", "--id", id]);
        assert_eq!(c, 0, "{e}");
        json::parse(o.as_bytes()).unwrap()
    };
    let git_head = || {
        let out = Command::new("git")
            .arg("-C")
            .arg(&r)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let mut cksum = Command::new("cksum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        cksum.stdin.take().as_mut().unwrap(),
        std::os::unix::ffi::OsStrExt::as_bytes(r.as_os_str()),
    )
    .unwrap();
    let checksum = cksum.wait_with_output().unwrap();
    assert!(checksum.status.success());
    let checksum = String::from_utf8(checksum.stdout).unwrap();
    let mut checksum = checksum.split_whitespace();
    let receipt_dir = PathBuf::from(std::env::var_os("XDG_STATE_HOME").unwrap())
        .join("oh-my-setting/land")
        .join(format!(
            "repo-{}-{}",
            checksum.next().unwrap(),
            checksum.next().unwrap()
        ));
    fs::create_dir_all(&receipt_dir).unwrap();
    fs::set_permissions(&receipt_dir, fs::Permissions::from_mode(0o770)).unwrap();
    fs::create_dir_all(r.join(".oms/artifacts")).unwrap();

    assert_eq!(at(&["init", "--goal", "g"]).0, 0);

    let land_case =
        |id: &str, issue: &str, receipt_fn: &dyn Fn(&str, &str) -> Value, expect_finish_rc: i32| {
            let base = git_head();
            let file = format!("src/{id}.txt");
            let artifact = format!(".oms/artifacts/{id}.md");
            assert_eq!(
                at(&[
                    "add",
                    "--id",
                    id,
                    "--title",
                    id,
                    "--issue",
                    issue,
                    "--allowed",
                    &file,
                ])
                .0,
                0
            );
            assert_eq!(at(&["claim", "--id", id, "--provider", "codex"]).0, 0);
            fs::create_dir_all(r.join("src")).unwrap();
            fs::write(r.join(&file), format!("{id}\n")).unwrap();
            git(&r, &["add", &file]);
            git(&r, &["commit", "-qm", id]);
            let commit = git_head();
            let built = receipt_fn(&base, &commit);
            let remote = built.text("remote");
            git(
                &r,
                &[
                    "update-ref",
                    &format!("refs/remotes/{remote}/main"),
                    &commit,
                ],
            );
            let receipt = json::canonical(&built).unwrap();
            let path = receipt_dir.join(format!("{commit}-{id}.json"));
            fs::write(&path, &receipt).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(r.join(&artifact), format!("artifact for {id}\n")).unwrap();
            assert_eq!(at(&["review", "--id", id, "--artifact", &artifact]).0, 0);
            let (rc, _, err) = at(&["finish", "--id", id, "--landed-commit", &commit]);
            assert_eq!(rc, expect_finish_rc, "{id}: {err}");
        };

    let base_receipt = |base: &str, commit: &str| {
        object([
            ("schema", number(1)),
            ("sha", string(commit)),
            ("state", string("passed")),
            ("remote", string("origin")),
            ("target", string("main")),
            ("gate", object([("rc", number(0))])),
            (
                "push",
                object([
                    ("rc", number(0)),
                    ("moved", number(1)),
                    ("base", string(base)),
                ]),
            ),
            ("ci", object([("conclusion", string("success"))])),
            (
                "merge",
                object([("method", string("squash")), ("state", string("MERGED"))]),
            ),
        ])
    };

    // 1. Valid accepted receipt with matching target repo and actual PR URL records PR identity.
    land_case(
        "pr-ok",
        "7",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("pr_number", number(19)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/Example-Org/Example-Repo/pull/19"),
            )
            .unwrap();
            r
        },
        0,
    );
    assert_eq!(task("pr-ok").text("pr"), format!("{REPO}#19"));

    // A passed receipt for the same path cannot complete a different exact review.
    let published = task("pr-ok");
    let commit = published.get("landing").text("sha");
    let id = "exact-external-review";
    let artifact = ".oms/artifacts/exact-external-review.md";
    let patch = ".oms/artifacts/exact-external-review.patch";
    let report = b"Reviewed alternative product\n";
    let reviewed = b"diff --git a/src/pr-ok.txt b/src/pr-ok.txt\nnew file mode 100644\n--- /dev/null\n+++ b/src/pr-ok.txt\n@@ -0,0 +1 @@\n+reviewed product\n";
    fs::write(r.join(artifact), report).unwrap();
    fs::write(r.join(patch), reviewed).unwrap();
    assert_eq!(
        at(&[
            "add",
            "--id",
            id,
            "--title",
            id,
            "--allowed",
            "src/pr-ok.txt",
        ])
        .0,
        0
    );
    assert_eq!(at(&["claim", "--id", id, "--provider", "codex"]).0, 0);
    let claimed = task(id);
    let lease = claimed.text("lease_id");
    let (rc, _, err) = at(&[
        "review",
        "--id",
        id,
        "--lease-id",
        lease,
        "--artifact",
        artifact,
        "--patch",
        patch,
        "--expected-review-artifact-sha256",
        &json::sha(report),
        "--expected-review-patch-sha256",
        &json::sha(reviewed),
    ]);
    assert_eq!(rc, 0, "{err}");
    let plan_file = r.join(".oms/plan/tasks.json");
    let before = fs::read(&plan_file).unwrap();
    let (rc, _, err) = at(&[
        "finish",
        "--id",
        id,
        "--lease-id",
        lease,
        "--landed-commit",
        commit,
    ]);
    assert_eq!(rc, 2, "{err}");
    assert!(
        err.contains("exact reviewed evidence requires patch-land"),
        "{err}"
    );
    assert_eq!(fs::read(&plan_file).unwrap(), before);

    // 2. Cross-repo PR URL is refused (PR identity omitted).
    land_case(
        "pr-cross-repo",
        "8",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("pr_number", number(20)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/other-org/example-repo/pull/20"),
            )
            .unwrap();
            r
        },
        0,
    );
    assert!(task("pr-cross-repo").opt("pr").is_none());

    // 3. Non-PR URL (/issues/N) is refused.
    land_case(
        "pr-issue-url",
        "9",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("pr_number", number(21)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/example-org/example-repo/issues/21"),
            )
            .unwrap();
            r
        },
        0,
    );
    assert!(task("pr-issue-url").opt("pr").is_none());

    // 4. Mismatched PR number vs PR URL is refused.
    land_case(
        "pr-mismatch",
        "10",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("pr_number", number(22)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/example-org/example-repo/pull/99"),
            )
            .unwrap();
            r
        },
        0,
    );
    assert!(task("pr-mismatch").opt("pr").is_none());

    // 5. Dry-run / unmerged merge state refuses to record PR identity.
    land_case(
        "pr-dry-run",
        "11",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("dry_run", Value::Bool(true)).unwrap();
            r.set("pr_number", number(23)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/example-org/example-repo/pull/23"),
            )
            .unwrap();
            r
        },
        0,
    );
    assert!(task("pr-dry-run").opt("pr").is_none());

    land_case(
        "pr-unmerged",
        "12",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("merge", object([("state", string("SKIPPED"))]))
                .unwrap();
            r.set("pr_number", number(24)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/example-org/example-repo/pull/24"),
            )
            .unwrap();
            r
        },
        0,
    );
    assert!(task("pr-unmerged").opt("pr").is_none());

    // 6. Unaccepted receipt state (pending) fails finish closed and records no PR identity.
    land_case(
        "pr-pending",
        "13",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("state", string("pending")).unwrap();
            r.set("pr_number", number(25)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/example-org/example-repo/pull/25"),
            )
            .unwrap();
            r
        },
        2,
    );
    assert!(task("pr-pending").opt("pr").is_none());

    // 7. Distinct fetch and push URLs: push destination governs compatibility,
    // matching landing/delivery's `--push` resolution.
    // 7a. Named remote whose fetch URL matches origin, but push URL points to a foreign repo -> refused.
    git(
        &r,
        &[
            "remote",
            "add",
            "split-bad",
            "git@github.com:example-org/example-repo.git",
        ],
    );
    git(
        &r,
        &[
            "remote",
            "set-url",
            "--push",
            "split-bad",
            "git@github.com:other-org/example-repo.git",
        ],
    );
    land_case(
        "pr-split-bad",
        "14",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("remote", string("split-bad")).unwrap();
            r.set("pr_number", number(26)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/example-org/example-repo/pull/26"),
            )
            .unwrap();
            r
        },
        0,
    );
    assert!(task("pr-split-bad").opt("pr").is_none());

    // 7b. Named remote whose fetch URL is foreign, but push URL matches origin -> accepted.
    git(
        &r,
        &[
            "remote",
            "add",
            "split-ok",
            "git@github.com:other-org/fork-repo.git",
        ],
    );
    git(
        &r,
        &[
            "remote",
            "set-url",
            "--push",
            "split-ok",
            "git@github.com:Example-Org/Example-Repo.git",
        ],
    );
    land_case(
        "pr-split-ok",
        "15",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("remote", string("split-ok")).unwrap();
            r.set("pr_number", number(27)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/example-org/example-repo/pull/27"),
            )
            .unwrap();
            r
        },
        0,
    );
    assert_eq!(task("pr-split-ok").text("pr"), format!("{REPO}#27"));

    // 7c. Even `origin` itself refuses PR recording when its push URL is rewritten to a foreign repo.
    git(
        &r,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:other-org/example-repo.git",
        ],
    );
    land_case(
        "pr-origin-push-foreign",
        "16",
        &|base, commit| {
            let mut r = base_receipt(base, commit);
            r.set("pr_number", number(28)).unwrap();
            r.set(
                "pr_url",
                string("https://github.com/example-org/example-repo/pull/28"),
            )
            .unwrap();
            r
        },
        0,
    );
    assert!(task("pr-origin-push-foreign").opt("pr").is_none());
}
