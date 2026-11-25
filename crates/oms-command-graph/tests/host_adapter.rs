#![forbid(unsafe_code)]

use oms_command_graph::{HostServices, execution, run_with_host, run_with_services};
use oms_command_plan::{Action, Context, Plan, Request, UnavailableServices};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("oms-graph-{prefix}-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("chmod temp dir");
        let path = fs::canonicalize(&path).expect("canonicalize temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

fn strip_group_world_write(root: &Path) {
    let Ok(meta) = fs::symlink_metadata(root) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o022 != 0 {
        let _ = fs::set_permissions(root, fs::Permissions::from_mode(mode & !0o022));
    }
    if meta.is_dir()
        && let Ok(entries) = fs::read_dir(root)
    {
        for entry in entries.flatten() {
            strip_group_world_write(&entry.path());
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Clone, Debug)]
struct RecordedCall {
    tool: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

struct RecordingHost {
    install_root: Option<PathBuf>,
    calls: Mutex<Vec<RecordedCall>>,
    exit_code: i32,
}

impl RecordingHost {
    fn new(install_root: Option<PathBuf>) -> Self {
        Self {
            install_root,
            calls: Mutex::new(Vec::new()),
            exit_code: 0,
        }
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("lock calls").clone()
    }
}

impl HostServices for RecordingHost {
    fn install_root(&self) -> Option<&Path> {
        self.install_root.as_deref()
    }

    fn invoke_command(
        &self,
        tool: &str,
        args: &[OsString],
        _cwd: &Path,
        env: &[(String, String)],
        _timeout: Option<Duration>,
        _cancel: &AtomicBool,
    ) -> oms_command_graph::Result<(i32, Vec<u8>, Vec<u8>, bool)> {
        self.calls.lock().expect("lock calls").push(RecordedCall {
            tool: tool.to_owned(),
            args: args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            env: env.to_vec(),
        });
        Ok((
            self.exit_code,
            b"{\"ok\":true}\n".to_vec(),
            Vec::new(),
            false,
        ))
    }
}

fn harness_env(key: &std::ffi::OsStr) -> bool {
    key.to_string_lossy().starts_with("OMS_")
}

/// `Context::from_environment` reads OMS_* variables (state repository, harness
/// child marker) that cannot be cleared in-process under `forbid(unsafe_code)`:
/// rerun the test in a child without any of them.
fn scrubbed_child(name: &str) -> bool {
    if std::env::vars_os().all(|(k, _)| !harness_env(&k)) {
        return false;
    }
    let mut command = Command::new(std::env::current_exe().expect("current exe"));
    for (key, _) in std::env::vars_os().filter(|(k, _)| harness_env(k)) {
        command.env_remove(key);
    }
    let out = command
        .args(["--exact", name, "--test-threads=1", "--nocapture"])
        .output()
        .expect("run scrubbed child");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("1 passed;"), "child ran no test: {text}");
    true
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_git_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.invalid"]);
    fs::write(dir.join("README.md"), "hello\n").expect("write README");
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "-q", "-m", "init"]);
    strip_group_world_write(dir);
}

fn run_plan(repo: &Path, req: Request) {
    let cancel = AtomicBool::new(false);
    let context = Context::from_environment(&req, repo).expect("plan context");
    let plan = Plan::open(context, &UnavailableServices, &cancel).expect("open plan");
    let response = plan.execute(&req).expect("execute plan");
    assert_eq!(
        response.code,
        0,
        "plan {} failed: {}",
        req.action.name(),
        String::from_utf8_lossy(&response.stderr)
    );
    strip_group_world_write(repo);
}

fn init_sample_plan(repo: &Path) {
    let mut init = Request::new(Action::Init).with("goal", "Test execution graph");
    init.repo = Some(repo.to_path_buf());
    run_plan(repo, init);

    let mut add = Request::new(Action::Add)
        .with("id", "task-1")
        .with("title", "Sample task")
        .with("allowed", "README.md");
    add.repo = Some(repo.to_path_buf());
    run_plan(repo, add);
}

fn argv(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn write_private(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write spec");
    strip_group_world_write(path);
}

#[test]
fn dry_run_emits_native_plan_run_invocation_without_scripts() {
    if scrubbed_child("dry_run_emits_native_plan_run_invocation_without_scripts") {
        return;
    }
    let tmp = TempDir::new("dry");
    init_git_repo(tmp.path());
    init_sample_plan(tmp.path());

    let spec_path = tmp.path().join("agent-spec.json");
    write_private(
        &spec_path,
        r#"{
  "schema": 1,
  "id": "agent-only",
  "entry": "work",
  "nodes": {
    "work": {
      "kind": "agent",
      "effect": "write",
      "title": "Run task-1",
      "plan_task": "task-1",
      "provider": "codex"
    },
    "done": {"kind": "terminal", "title": "Done"}
  },
  "edges": [
    {"from": "work", "to": "done", "outcomes": ["completed"]}
  ]
}"#,
    );

    let host = RecordingHost::new(None);
    let cancel = AtomicBool::new(false);
    let (code, stdout, stderr) = run_with_host(
        &argv(&[
            "exec",
            "run",
            spec_path.to_str().unwrap(),
            "--worker",
            "codex",
            "--dry-run",
            "--json",
            "--repo",
            tmp.path().to_str().unwrap(),
        ]),
        tmp.path(),
        &cancel,
        None,
        None,
        &UnavailableServices,
        &host,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let parsed: serde_json::Value = serde_json::from_slice(&stdout).expect("parse dry-run json");
    assert_eq!(parsed["dry_run"], true);
    let commands = parsed["commands"].as_array().expect("commands array");
    assert_eq!(commands.len(), 1);
    let first: Vec<String> = serde_json::from_value(commands[0].clone()).expect("cmd array");
    assert_eq!(first[0], "oms");
    assert_eq!(first[1], "plan-run");
    assert!(first.contains(&"--id".to_owned()));
    assert!(first.contains(&"task-1".to_owned()));
    assert!(first.contains(&"--dry-run".to_owned()));
    assert!(
        !first.iter().any(|part| part.contains("scripts/")),
        "dry-run must not reference shell scripts: {first:?}"
    );
    assert!(host.calls().is_empty());
}

#[test]
fn agent_and_acceptance_nodes_dispatch_through_host_services() {
    if scrubbed_child("agent_and_acceptance_nodes_dispatch_through_host_services") {
        return;
    }
    let tmp = TempDir::new("exec");
    init_git_repo(tmp.path());
    init_sample_plan(tmp.path());

    let spec_path = tmp.path().join("exec-spec.json");
    write_private(
        &spec_path,
        r#"{
  "schema": 1,
  "id": "native-dispatch",
  "entry": "work",
  "nodes": {
    "work": {
      "kind": "agent",
      "effect": "write",
      "title": "Run task-1",
      "plan_task": "task-1",
      "provider": "codex"
    },
    "accept": {
      "kind": "tool",
      "tool": "plan_acceptance",
      "title": "Accept plan"
    },
    "done": {"kind": "terminal", "title": "Done"}
  },
  "edges": [
    {"from": "work", "to": "accept", "outcomes": ["completed", "failed", "unverified"]},
    {"from": "accept", "to": "done", "outcomes": ["completed"]}
  ]
}"#,
    );

    let host = RecordingHost::new(None);
    let cancel = AtomicBool::new(false);
    let (code, stdout, stderr) = run_with_host(
        &argv(&[
            "exec",
            "run",
            spec_path.to_str().unwrap(),
            "--worker",
            "codex",
            "--goal",
            "Verify host dispatch",
            "--json",
            "--repo",
            tmp.path().to_str().unwrap(),
        ]),
        tmp.path(),
        &cancel,
        None,
        None,
        &UnavailableServices,
        &host,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let parsed: serde_json::Value = serde_json::from_slice(&stdout).expect("parse exec json");
    assert_eq!(parsed["status"], "terminal");
    assert_eq!(parsed["primary"], "done");

    let calls = host.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].tool, "plan-run");
    assert!(calls[0].args.contains(&"task-1".to_owned()));
    assert!(
        calls[0]
            .env
            .iter()
            .any(|(k, v)| k == "OMS_GRAPH_NODE" && v == "work")
    );
    assert_eq!(calls[1].tool, "agent-plan");
    assert!(calls[1].args.contains(&"accept".to_owned()));
    assert!(
        calls[1]
            .env
            .iter()
            .any(|(k, v)| k == "OMS_GRAPH_NODE" && v == "accept")
    );
}

#[test]
fn expired_claims_stay_unverified_after_plan_run_and_on_resume() {
    if scrubbed_child("expired_claims_stay_unverified_after_plan_run_and_on_resume") {
        return;
    }
    struct ExpiringHost {
        state: &'static str,
        interrupt: bool,
    }
    impl HostServices for ExpiringHost {
        fn invoke_command(
            &self,
            tool: &str,
            _: &[OsString],
            repo: &Path,
            _: &[(String, String)],
            _: Option<Duration>,
            _: &AtomicBool,
        ) -> oms_command_graph::Result<(i32, Vec<u8>, Vec<u8>, bool)> {
            assert_eq!(tool, "plan-run");
            run_plan(
                repo,
                Request::new(Action::Claim)
                    .with("id", "task-1")
                    .with("provider", "codex"),
            );
            let path = repo.join(".oms/plan/tasks.json");
            let mut plan: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            let task = &mut plan["tasks"]["task-1"];
            task["state"] = self.state.into();
            task["claimed_at"] = "2000-01-01T00:00:00Z".into();
            task["ttl"] = "1".into();
            fs::write(path, plan.to_string()).unwrap();
            if self.interrupt {
                return Err(oms_command_graph::Error {
                    code: 130,
                    message: "interrupted after claim".into(),
                });
            }
            Ok((1, vec![], vec![], false))
        }
    }

    for state in ["claimed", "running"] {
        for interrupt in [false, true] {
            let repo = TempDir::new("expired-claim");
            init_git_repo(repo.path());
            init_sample_plan(repo.path());
            let spec_path = repo.path().join("exec-spec.json");
            write_private(&spec_path, &serde_json::json!({
                "schema":1, "id":"expired-claim", "entry":"work",
                "nodes":{
                    "work":{"kind":"agent","effect":"write","title":"Run task","plan_task":"task-1",
                        "provider":"codex","proof":["plan.task.task-1.state", "plan.task.task-1.claim_expired"]},
                    "done":{"kind":"terminal","title":"Done"},
                },
                "edges":[{"from":"work","to":"done","outcomes":["unverified"],
                    "when":[format!("plan.task.task-1.state={state}"),"plan.task.task-1.claim_expired"]}],
            }).to_string());
            let host = ExpiringHost { state, interrupt };
            let cancel = AtomicBool::new(false);
            let invoke = |args: &[&str]| {
                run_with_host(
                    &argv(args),
                    repo.path(),
                    &cancel,
                    None,
                    None,
                    &UnavailableServices,
                    &host,
                )
            };
            let mut out = invoke(&[
                "exec",
                "run",
                spec_path.to_str().unwrap(),
                "--worker",
                "codex",
                "--json",
            ]);
            let run = execution::events::latest(repo.path()).unwrap();
            if interrupt {
                assert_eq!(out.0, 130, "{out:?}");
                out = invoke(&[
                    "exec", "resume", "--run", &run, "--worker", "codex", "--json",
                ]);
            }
            assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
            let result: serde_json::Value = serde_json::from_slice(&out.1).unwrap();
            assert_eq!(result["primary"], "done", "{result}");
            let events = execution::events::read(repo.path(), &run).unwrap();
            let outcome = events
                .iter()
                .find(|row| {
                    execution::st(row, "event") == "node_outcome"
                        && execution::st(row, "node") == "work"
                })
                .unwrap();
            assert_eq!(execution::st(outcome, "outcome"), "unverified");
            assert_eq!(execution::st(outcome, "claimed_outcome"), "unverified");
            let facts = outcome.get("facts");
            assert_eq!(execution::st(facts, "plan.task.task-1.state"), state);
            assert_eq!(
                facts.get("plan.task.task-1.claim_expired"),
                &execution::Value::Bool(true)
            );
        }
    }
}

#[test]
fn acceptance_node_fails_closed_without_host_services() {
    if scrubbed_child("acceptance_node_fails_closed_without_host_services") {
        return;
    }
    let tmp = TempDir::new("nohost");
    init_git_repo(tmp.path());

    let spec_path = tmp.path().join("accept-spec.json");
    write_private(
        &spec_path,
        r#"{
  "schema": 1,
  "id": "accept-only",
  "entry": "accept",
  "nodes": {
    "accept": {
      "kind": "tool",
      "tool": "plan_acceptance",
      "title": "Accept plan"
    },
    "done": {"kind": "terminal", "title": "Done"}
  },
  "edges": [
    {"from": "accept", "to": "done", "outcomes": ["completed"]}
  ]
}"#,
    );

    let cancel = AtomicBool::new(false);
    let (code, stdout, stderr) = run_with_services(
        &argv(&[
            "exec",
            "run",
            spec_path.to_str().unwrap(),
            "--worker",
            "codex",
            "--json",
            "--repo",
            tmp.path().to_str().unwrap(),
        ]),
        tmp.path(),
        &cancel,
        None,
        None,
        &UnavailableServices,
    );
    assert_ne!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let parsed: serde_json::Value = serde_json::from_slice(&stdout).expect("parse exec json");
    let run_id = parsed["run_id"].as_str().expect("run_id");
    assert_eq!(parsed["status"], "blocked");

    let (ev_code, ev_stdout, ev_stderr) = run_with_services(
        &argv(&[
            "exec",
            "events",
            "--run",
            run_id,
            "--json",
            "--repo",
            tmp.path().to_str().unwrap(),
        ]),
        tmp.path(),
        &cancel,
        None,
        None,
        &UnavailableServices,
    );
    assert_eq!(
        ev_code,
        0,
        "events stderr: {}",
        String::from_utf8_lossy(&ev_stderr)
    );
    let events: serde_json::Value = serde_json::from_slice(&ev_stdout).expect("parse events json");
    let details: Vec<&str> = events["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter_map(|e| e["detail"].as_str())
        .collect();
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("canonical host adapter")),
        "expected fail-closed host adapter detail in events: {events}"
    );
}

#[test]
fn bundled_graph_loads_from_host_install_root_without_scripts_dir() {
    if scrubbed_child("bundled_graph_loads_from_host_install_root_without_scripts_dir") {
        return;
    }
    let tmp = TempDir::new("install-root");
    init_git_repo(tmp.path());
    let install_root = tmp.path().join("packaged");
    fs::create_dir_all(install_root.join("config/graphs")).expect("mkdir config/graphs");
    fs::write(
        install_root.join("config/graphs/bundled.json"),
        r#"{
  "schema": 1,
  "id": "bundled",
  "entry": "start",
  "nodes": {
    "start": {"kind": "terminal", "title": "Done"}
  },
  "edges": []
}"#,
    )
    .expect("write bundled spec");
    strip_group_world_write(&install_root);

    let spec = execution::load_with_root(
        &execution::Value::from("bundled"),
        tmp.path(),
        Some(&install_root),
    )
    .expect("load bundled graph without scripts dir");
    assert_eq!(execution::st(&spec, "id"), "bundled");

    let host = RecordingHost::new(Some(install_root));
    let cancel = AtomicBool::new(false);
    let (code, _stdout, stderr) = run_with_host(
        &argv(&[
            "exec",
            "validate",
            "bundled",
            "--repo",
            tmp.path().to_str().unwrap(),
        ]),
        tmp.path(),
        &cancel,
        None,
        None,
        &UnavailableServices,
        &host,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
}

#[test]
fn commit_bound_dispatches_goal_drive_through_host_services() {
    if scrubbed_child("commit_bound_dispatches_goal_drive_through_host_services") {
        return;
    }
    let tmp = TempDir::new("commit-bound");
    init_git_repo(tmp.path());
    init_sample_plan(tmp.path());

    let patch_rel = ".oms/artifacts/task-1.patch";
    fs::create_dir_all(tmp.path().join(".oms/artifacts")).expect("mkdir artifacts");
    fs::write(
        tmp.path().join(patch_rel),
        "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-hello\n+world\n",
    )
    .expect("write patch");
    fs::create_dir_all(tmp.path().join(".oms/plan")).expect("mkdir plan");
    fs::write(
        tmp.path().join(".oms/plan/progress.jsonl"),
        "{\"kind\":\"patch-land\",\"task_id\":\"task-1\",\"exit\":0}\n",
    )
    .expect("write progress");

    let mut status_req = Request::new(Action::Status);
    status_req.repo = Some(tmp.path().to_path_buf());
    let plan_path = Context::from_environment(&status_req, tmp.path())
        .expect("plan context")
        .plan_file;
    let mut plan: serde_json::Value =
        serde_json::from_slice(&fs::read(&plan_path).expect("read plan")).expect("parse plan");
    if let Some(tasks) = plan["tasks"].as_array_mut() {
        let task = tasks
            .iter_mut()
            .find(|t| t["id"] == "task-1")
            .expect("task-1 row");
        task["state"] = serde_json::json!("done");
        task["patch"] = serde_json::json!(patch_rel);
    } else if let Some(tasks) = plan["tasks"].as_object_mut() {
        let task = tasks.get_mut("task-1").expect("task-1 key");
        task["state"] = serde_json::json!("done");
        task["patch"] = serde_json::json!(patch_rel);
    } else {
        panic!("unexpected plan.tasks shape: {plan}");
    }
    fs::write(&plan_path, serde_json::to_vec(&plan).expect("encode plan")).expect("write plan");
    strip_group_world_write(tmp.path());

    let spec_path = tmp.path().join("commit-spec.json");
    write_private(
        &spec_path,
        r#"{
  "schema": 1,
  "id": "commit-only",
  "entry": "bind",
  "nodes": {
    "bind": {
      "kind": "agent",
      "effect": "write",
      "title": "Bind done task",
      "plan_task": "task-1",
      "bind_task": "work_item",
      "provider": "codex"
    },
    "commit": {
      "kind": "tool",
      "tool": "commit_bound",
      "binding": "work_item",
      "title": "Commit bound task"
    },
    "done": {"kind": "terminal", "title": "Done"}
  },
  "edges": [
    {"from": "bind", "to": "commit", "outcomes": ["completed"]},
    {"from": "commit", "to": "done", "outcomes": ["completed"]}
  ]
}"#,
    );

    let host = RecordingHost::new(None);
    let cancel = AtomicBool::new(false);
    let (code, stdout, stderr) = run_with_host(
        &argv(&[
            "exec",
            "run",
            spec_path.to_str().unwrap(),
            "--worker",
            "codex",
            "--json",
            "--repo",
            tmp.path().to_str().unwrap(),
        ]),
        tmp.path(),
        &cancel,
        None,
        None,
        &UnavailableServices,
        &host,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let parsed: serde_json::Value = serde_json::from_slice(&stdout).expect("parse exec json");
    assert_eq!(parsed["status"], "terminal");
    assert_eq!(parsed["primary"], "done");

    let calls = host.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].tool, "plan-run");
    assert_eq!(calls[1].tool, "goal-drive");
    assert!(calls[1].args.contains(&"--commit-task".to_owned()));
    assert!(calls[1].args.contains(&"task-1".to_owned()));
    assert!(calls[1].args.contains(&"--commit-message".to_owned()));
}

#[test]
fn group_writable_repo_uses_host_group_resolver_and_fails_closed_without_one() {
    if scrubbed_child("group_writable_repo_uses_host_group_resolver_and_fails_closed_without_one") {
        return;
    }

    struct GroupHost;
    impl oms_command_plan::Services for GroupHost {
        fn group_membership_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
            fn resolve(
                gid: u32,
            ) -> oms_command_plan::Result<oms_command_plan::services::GroupMembership> {
                let uid = rustix::process::getuid().as_raw();
                Ok(oms_command_plan::services::GroupMembership {
                    gid,
                    explicit_member_uids: vec![uid],
                    primary_member_uids: vec![uid],
                })
            }
            Some(resolve)
        }
    }

    let tmp = TempDir::new("group-repo");
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo).expect("create repo");
    init_git_repo(&repo);
    fs::write(repo.join("lib.sh"), "sample() { return 0; }\n").expect("write source");
    git(&repo, &["add", "lib.sh"]);
    let state = repo.join(".oms/project-graph");
    fs::create_dir_all(&state).expect("create state dir");
    fs::set_permissions(&repo, fs::Permissions::from_mode(0o775)).expect("chmod repo 0775");
    fs::set_permissions(repo.join(".oms"), fs::Permissions::from_mode(0o775))
        .expect("chmod .oms 0775");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o775)).expect("chmod state 0775");

    let host = RecordingHost::new(None);
    let cancel = AtomicBool::new(false);
    let build_cmd = argv(&[
        "project",
        "build",
        "--repo",
        repo.to_str().unwrap(),
        "--json",
    ]);

    let (fail_code, _, fail_err) = run_with_host(
        &build_cmd,
        &repo,
        &cancel,
        None,
        None,
        &UnavailableServices,
        &host,
    );
    assert_eq!(fail_code, 2);
    assert!(
        String::from_utf8_lossy(&fail_err).contains("account proof service unavailable"),
        "unexpected build stderr: {}",
        String::from_utf8_lossy(&fail_err)
    );

    let (ok_code, ok_out, ok_err) =
        run_with_host(&build_cmd, &repo, &cancel, None, None, &GroupHost, &host);
    assert_eq!(ok_code, 0, "stderr: {}", String::from_utf8_lossy(&ok_err));
    let built: serde_json::Value = serde_json::from_slice(&ok_out).expect("parse build json");
    assert_eq!(built["stats"]["files"].as_i64(), Some(2));

    let check_cmd = argv(&[
        "project",
        "check",
        "--repo",
        repo.to_str().unwrap(),
        "--json",
    ]);
    let (check_fail_code, _, check_fail_err) = run_with_host(
        &check_cmd,
        &repo,
        &cancel,
        None,
        None,
        &UnavailableServices,
        &host,
    );
    assert_eq!(check_fail_code, 2);
    assert!(
        String::from_utf8_lossy(&check_fail_err).contains("account proof service unavailable"),
        "unexpected check stderr: {}",
        String::from_utf8_lossy(&check_fail_err)
    );

    let (check_ok_code, check_ok_out, check_ok_err) =
        run_with_host(&check_cmd, &repo, &cancel, None, None, &GroupHost, &host);
    assert_eq!(
        check_ok_code,
        0,
        "check stderr: {}",
        String::from_utf8_lossy(&check_ok_err)
    );
    let checked: serde_json::Value =
        serde_json::from_slice(&check_ok_out).expect("parse check json");
    assert_eq!(checked["fresh"], true);
}
