#![forbid(unsafe_code)]

use oms_command_state::{GcHookResult, HostServices, NoHostServices, json, run, run_with_host};
use std::ffi::OsString;
use std::fs::{self, FileTimes, OpenOptions, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

struct TempDir(PathBuf);
impl TempDir {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, Permissions::from_mode(0o750)).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn os_args(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn call_recipe(repo: &Path, args: &[&str]) -> (i32, json::Value, String) {
    let flag = AtomicBool::new(false);
    let mut full = Vec::with_capacity(args.len() + 2);
    full.extend_from_slice(args);
    full.push("--repo");
    let repo_str = repo.to_string_lossy().into_owned();
    full.push(&repo_str);
    let (code, stdout, stderr) = run("recipe", &os_args(&full), repo, &flag);
    let err = String::from_utf8_lossy(&stderr).into_owned();
    let val = if stdout.is_empty() {
        json::Value::Null
    } else {
        json::parse(&stdout).unwrap_or_else(|e| {
            panic!(
                "invalid recipe stdout json ({e}): {}\nstderr: {err}",
                String::from_utf8_lossy(&stdout)
            )
        })
    };
    (code, val, err)
}

fn write_json(repo: &Path, rel: &str, text: &str) {
    let p = repo.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, text).unwrap();
}

fn write_py_worker(repo: &Path) {
    let script = r#"import json, os, pathlib, socket, sys, time
mode, *args = sys.argv[1:]
if mode == "write":
    pathlib.Path(args[0]).write_text(json.dumps(args[1:]))
elif mode == "first":
    with open("calls", "a") as f:
        f.write("first\n")
    pathlib.Path("first.out").write_text("first")
elif mode == "flaky":
    with open("calls", "a") as f:
        f.write("flaky\n")
    gate = pathlib.Path("fail-once")
    if gate.exists():
        gate.unlink()
        raise SystemExit(7)
    pathlib.Path("second.out").write_text("second")
elif mode == "noise":
    print("x" * 100000)
elif mode == "wait":
    time.sleep(30)
    pathlib.Path("wait.out").write_text("done")
elif mode == "sensitive":
    print("pass" + "word=" + "test-fixture-value")
elif mode == "serve":
    with socket.socket() as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("127.0.0.1", int(args[0])))
        s.listen()
        while True:
            c, _ = s.accept()
            with c:
                c.sendall(b"ready")
elif mode == "probe":
    with socket.create_connection(("127.0.0.1", int(args[0])), timeout=2) as s:
        pathlib.Path("probe.out").write_text(s.recv(20).decode() + ":" + os.environ["MESSAGE"])
"#;
    fs::write(repo.join("worker.py"), script).unwrap();
}

fn find_free_port() -> u16 {
    let sock = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    sock.local_addr().unwrap().port()
}

#[test]
fn recipe_execute_resume_limits_companions_and_trigger_run() {
    let tmp = TempDir::new("oms-recipe-exec");
    let repo = tmp.path();
    write_py_worker(repo);

    // 1. Read-only plan vs run without --execute vs real execution + sensitive scan.
    let rec_basic = r#"{
        "schema": 1,
        "kind": "recipe",
        "id": "oms-basic",
        "commands": {
            "work": {"argv": ["python3", "worker.py", "write", "result.json", "ok"], "depends": ["worker.py"], "timeout_seconds": 5},
            "sens": {"argv": ["python3", "worker.py", "sensitive"], "depends": ["worker.py"], "timeout_seconds": 5},
            "noise": {"argv": ["python3", "worker.py", "noise"], "depends": ["worker.py"], "timeout_seconds": 5, "max_output_bytes": 1024},
            "wait": {"argv": ["python3", "worker.py", "wait"], "depends": ["worker.py"], "timeout_seconds": 1}
        },
        "steps": [{"id": "work", "command": "work", "outputs": [{"path": "result.json", "min_bytes": 1}]}]
    }"#;
    write_json(repo, "basic.json", rec_basic);

    let (code, plan_val, _) = call_recipe(repo, &["plan", "basic.json"]);
    assert_eq!(code, 0);
    assert_eq!(plan_val.text("id"), "oms-basic");
    assert!(!repo.join(".oms/recipes").exists());

    let (code, _, err) = call_recipe(repo, &["run", "basic.json"]);
    assert_eq!(code, 2, "{err}");
    assert!(!repo.join(".oms/recipes").exists());

    let (code, run_val, err) = call_recipe(repo, &["run", "basic.json", "--execute"]);
    assert_eq!(code, 0, "stdout={run_val:?} stderr={err}");
    assert_eq!(run_val.text("status"), "succeeded");
    assert_eq!(
        fs::read_to_string(repo.join("result.json")).unwrap(),
        r#"["ok"]"#
    );

    // 2. Sensitive console scan never stores raw value in receipt.
    let rec_sens = r#"{
        "schema": 1,
        "kind": "recipe",
        "id": "oms-sens",
        "commands": {
            "sens": {"argv": ["python3", "worker.py", "sensitive"], "depends": ["worker.py"], "timeout_seconds": 5}
        },
        "steps": [{"id": "sens", "command": "sens", "outputs": []}]
    }"#;
    write_json(repo, "sens.json", rec_sens);
    let (code, sens_run, err) = call_recipe(repo, &["run", "sens.json", "--execute"]);
    assert_eq!(code, 0, "stdout={sens_run:?} stderr={err}");
    let attempt = sens_run
        .get("steps")
        .get("sens")
        .get("attempts")
        .array()
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(
        attempt.get("sensitive_console_detected"),
        &json::Value::Bool(true)
    );
    assert_eq!(attempt.get("console_retained"), &json::Value::Bool(false));
    let receipt_raw =
        fs::read_to_string(repo.join(format!(".oms/recipes/runs/{}.json", sens_run.text("id"))))
            .unwrap();
    assert!(!receipt_raw.contains("test-fixture-value"));

    // 3. Output limit -> exit 125 and reason == "output-limit".
    let rec_noise = r#"{
        "schema": 1,
        "kind": "recipe",
        "id": "oms-noise",
        "commands": {
            "noise": {"argv": ["python3", "worker.py", "noise"], "depends": ["worker.py"], "timeout_seconds": 5, "max_output_bytes": 1024}
        },
        "steps": [{"id": "noise", "command": "noise", "outputs": []}]
    }"#;
    write_json(repo, "noise.json", rec_noise);
    let (code, noise_run, err) = call_recipe(repo, &["run", "noise.json", "--execute"]);
    assert_eq!(code, 125, "{err}");
    assert_eq!(
        noise_run
            .get("steps")
            .get("noise")
            .get("attempts")
            .array()
            .unwrap()
            .last()
            .unwrap()
            .text("reason"),
        "output-limit"
    );

    // 4. Timeout -> exit 124 and reason == "timeout".
    let rec_wait = r#"{
        "schema": 1,
        "kind": "recipe",
        "id": "oms-wait",
        "commands": {
            "wait": {"argv": ["python3", "worker.py", "wait"], "depends": ["worker.py"], "timeout_seconds": 1}
        },
        "steps": [{"id": "wait", "command": "wait", "outputs": [{"path": "wait.out"}]}]
    }"#;
    write_json(repo, "wait.json", rec_wait);
    let (code, wait_run, err) = call_recipe(repo, &["run", "wait.json", "--execute"]);
    assert_eq!(code, 124, "{err}");
    assert_eq!(
        wait_run
            .get("steps")
            .get("wait")
            .get("attempts")
            .array()
            .unwrap()
            .last()
            .unwrap()
            .text("reason"),
        "timeout"
    );

    // 5. Flaky two-step resume skips already-succeeded step and rejects tampered output.
    let rec_flaky = r#"{
        "schema": 1,
        "kind": "recipe",
        "id": "oms-flaky",
        "commands": {
            "first": {"argv": ["python3", "worker.py", "first"], "depends": ["worker.py"], "timeout_seconds": 5},
            "flaky": {"argv": ["python3", "worker.py", "flaky"], "depends": ["worker.py"], "timeout_seconds": 5}
        },
        "steps": [
            {"id": "first", "command": "first", "outputs": [{"path": "first.out"}]},
            {"id": "flaky", "command": "flaky", "outputs": [{"path": "second.out"}]}
        ]
    }"#;
    write_json(repo, "flaky.json", rec_flaky);
    fs::write(repo.join("fail-once"), b"").unwrap();
    let (code, failed, err) = call_recipe(repo, &["run", "flaky.json", "--execute"]);
    assert_eq!(code, 7, "{err}");
    assert_eq!(failed.text("status"), "failed");
    let run_id = failed.text("id").to_string();

    let (code, _, _) = call_recipe(
        repo,
        &["resume", &run_id, "--file", "flaky.json", "--execute"],
    );
    assert_eq!(code, 2);

    let (code, resumed, err) = call_recipe(
        repo,
        &[
            "resume",
            &run_id,
            "--file",
            "flaky.json",
            "--execute",
            "--retry-failed",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(resumed.text("status"), "succeeded");
    assert_eq!(
        fs::read_to_string(repo.join("calls")).unwrap(),
        "first\nflaky\nflaky\n"
    );

    fs::write(repo.join("first.out"), b"tampered").unwrap();
    let (code, _, err) = call_recipe(
        repo,
        &["resume", &run_id, "--file", "flaky.json", "--execute"],
    );
    assert_eq!(code, 2);
    assert!(err.contains("completed output changed"), "{err}");

    // 6. Workspace ports + companions.
    let port = find_free_port();
    let port_str = port.to_string();
    let ws = r#"{
        "schema": 1,
        "kind": "workspace",
        "id": "oms-ws",
        "inputs": {"msg": {"type": "string", "default": "hi"}},
        "ports": {"svc": 18080},
        "commands": {
            "serve": {"argv": ["python3", "worker.py", "serve", "${port.svc}"], "depends": ["worker.py"], "timeout_seconds": 10},
            "probe": {
                "argv": ["python3", "worker.py", "probe", "${port.svc}"],
                "depends": ["worker.py"],
                "timeout_seconds": 5,
                "environment": {"MESSAGE": {"value": "${input.msg}"}}
            }
        },
        "companions": {"srv": {"command": "serve", "ready_port": "svc", "startup_seconds": 5}},
        "actions": {"verify": {"steps": [{"id": "probe", "command": "probe", "outputs": [{"path": "probe.out"}]}]}}
    }"#;
    write_json(repo, "ws.json", ws);
    let port_arg = format!("svc={port_str}");
    let (code, ws_run, err) = call_recipe(
        repo,
        &[
            "workspace",
            "run",
            "ws.json",
            "verify",
            "--port",
            &port_arg,
            "--with",
            "srv",
            "--input",
            "msg=hello",
            "--execute",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(ws_run.text("status"), "succeeded");
    assert_eq!(
        fs::read_to_string(repo.join("probe.out")).unwrap(),
        "ready:hello"
    );
    assert_eq!(
        ws_run
            .get("companion_attempts")
            .array()
            .unwrap()
            .first()
            .unwrap()
            .text("status"),
        "stopped"
    );

    // 7. Scheduled trigger tick + trigger run --execute.
    let trig = r#"{
        "schema": 1,
        "kind": "recipe-trigger",
        "id": "oms-daily",
        "recipe": "basic.json",
        "schedule": {"at": "2026-01-01T00:00:00Z", "every_seconds": 60}
    }"#;
    write_json(repo, "trigger.json", trig);
    assert_eq!(call_recipe(repo, &["trigger", "add", "trigger.json"]).0, 0);
    let (code, tick, err) =
        call_recipe(repo, &["trigger", "tick", "--now", "2026-01-01T00:02:00Z"]);
    assert_eq!(code, 0, "{err}");
    let req_id = tick
        .get("requests")
        .array()
        .unwrap()
        .first()
        .unwrap()
        .text("id")
        .to_string();

    assert_eq!(call_recipe(repo, &["trigger", "run", &req_id]).0, 2);
    let (code, trig_done, err) = call_recipe(repo, &["trigger", "run", &req_id, "--execute"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(trig_done.get("run").text("status"), "succeeded");
    assert_eq!(trig_done.get("request").text("status"), "succeeded");
}

struct StubHost;
impl HostServices for StubHost {
    fn gc_landing_captures(
        &self,
        _repo: &Path,
        _days: u64,
        _apply: bool,
    ) -> oms_command_state::Result<GcHookResult> {
        Ok(GcHookResult::Reclaimed(vec![(
            "landing-capture".into(),
            ".oms/landing-patches/land-1.patch".into(),
        )]))
    }
    fn gc_dispatch_logs(
        &self,
        _repo: &Path,
        _days: u64,
        _apply: bool,
    ) -> oms_command_state::Result<GcHookResult> {
        Ok(GcHookResult::Reclaimed(vec![(
            "dispatch-logs".into(),
            ".oms/panel-state/dispatch-logs/old.log".into(),
        )]))
    }
}

fn chmod_tree_private(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let _ = fs::set_permissions(&dir, Permissions::from_mode(0o750));
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let _ = fs::set_permissions(&p, Permissions::from_mode(0o640));
                }
            }
        }
    }
}

fn age_file(path: &Path, mtime_secs: u64) {
    let f = OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(
        FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs)),
    )
    .unwrap();
}

#[test]
fn gc_completion_captures_host_hooks_telemetry_prune_and_orphans() {
    let tmp = TempDir::new("oms-gc-state");
    let repo = tmp.path();

    // Seed aged completion capture, empty completions dir, landing/dispatch dirs, >1000 artifact rows, and aged orphan.
    let cap_bytes =
        br#"{"schema":1,"repository_sha256":"abc","room_id":"r1","room":{},"lifecycle":{}}"#;
    let cap_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(cap_bytes))
    };
    let cap_rel = format!(".oms/plan/completion-captures/{cap_sha}.json");
    fs::create_dir_all(repo.join(".oms/plan/completion-captures")).unwrap();
    fs::create_dir_all(repo.join(".oms/plan/completions")).unwrap();
    fs::write(repo.join(&cap_rel), cap_bytes).unwrap();

    fs::create_dir_all(repo.join(".oms/landing-patches")).unwrap();
    fs::write(repo.join(".oms/landings.jsonl"), b"").unwrap();
    fs::create_dir_all(repo.join(".oms/panel-state/dispatch-logs")).unwrap();

    fs::create_dir_all(repo.join(".oms/artifacts/kept")).unwrap();
    let kept_payload = repo.join(".oms/artifacts/kept/report.txt");
    let orphan_payload = repo.join(".oms/artifacts/loose-orphan.bin");
    fs::write(&kept_payload, b"kept").unwrap();
    fs::write(&orphan_payload, b"orphan").unwrap();

    // 1005 valid artifact rows so keep=1000 prunes 5 rows while preserving kept_payload ref on the last row.
    let mut index_bytes = Vec::new();
    for i in 0..1005u32 {
        let line = if i == 1004 {
            format!(
                r#"{{"schema":1,"event_id":"ev-{i}","operation_id":"op-{i}","artifact_id":"art-{i}","ts":"2026-01-01T00:00:00Z","kind":"artifact-created","provider":"codex","exit":0,"artifact":".oms/artifacts/kept/report.txt"}}"#
            )
        } else {
            format!(
                r#"{{"schema":1,"event_id":"ev-{i}","operation_id":"op-{i}","artifact_id":"art-{i}","ts":"2026-01-01T00:00:00Z","kind":"artifact-created","provider":"codex","exit":0}}"#
            )
        };
        index_bytes.extend_from_slice(line.as_bytes());
        index_bytes.push(b'\n');
    }
    fs::write(repo.join(".oms/artifacts/index.jsonl"), &index_bytes).unwrap();
    chmod_tree_private(repo);
    age_file(&repo.join(&cap_rel), 1000);
    age_file(&kept_payload, 1000);
    age_file(&orphan_payload, 1000);

    let flag = AtomicBool::new(false);
    let repo_s = repo.to_string_lossy().into_owned();

    // Dry run with NoHostServices defers host hooks while listing completion-provenance, artifact-index, and orphan-artifact.
    let dry_args = os_args(&[
        "--repo",
        &repo_s,
        "--days",
        "1",
        "--dry-run",
        "--delete-orphan-files",
        "--json",
    ]);
    let (code, stdout, stderr) = run("gc", &dry_args, repo, &flag);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    let dry_report = json::parse(&stdout).unwrap();
    assert!(dry_report.get("deferred").array().unwrap().len() >= 2);
    assert!(repo.join(&cap_rel).exists());
    assert!(orphan_payload.exists());

    // Apply via run_with_host + StubHost reclaims everything safely and preserves kept_payload + 1000 rows.
    let mut no_svc = oms_command_state_test_services();
    let apply_args = os_args(&[
        "--repo",
        &repo_s,
        "--days",
        "1",
        "--apply",
        "--delete-orphan-files",
        "--json",
    ]);
    let (code, stdout, stderr) =
        run_with_host("gc", &apply_args, repo, &flag, &mut no_svc, &StubHost);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    let apply_report = json::parse(&stdout).unwrap();
    assert_eq!(apply_report.get("deferred").array().unwrap().len(), 0);
    assert!(!repo.join(&cap_rel).exists());
    assert!(!orphan_payload.exists());
    assert!(kept_payload.exists());
    let final_lines = fs::read_to_string(repo.join(".oms/artifacts/index.jsonl"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(final_lines, 1000);
    let _ = NoHostServices;
}

struct DummyRunServices;
impl oms_command_runs::Services for DummyRunServices {}
fn oms_command_state_test_services() -> DummyRunServices {
    DummyRunServices
}

#[test]
fn recipe_child_process_guard_blocks_mutations_allows_plan() {
    if let Some(repo_raw) = std::env::var_os("OMS_RECIPE_CHILD_TEST_REPO") {
        let repo = Path::new(&repo_raw);
        let (code, plan_val, err) = call_recipe(repo, &["plan", "basic.json"]);
        assert_eq!(code, 0, "stderr={err}");
        assert_eq!(plan_val.text("id"), "oms-basic");
        let (code, _, _) = call_recipe(repo, &["run", "basic.json", "--execute"]);
        assert_eq!(code, 2);
        let (code, _, _) = call_recipe(repo, &["trigger", "add", "trig.json"]);
        assert_eq!(code, 2);
        let (code, _, _) = call_recipe(repo, &["workset", "put", "ws.json"]);
        assert_eq!(code, 2);
        assert!(!repo.join(".oms/recipes").exists());
        return;
    }

    let tmp = TempDir::new("oms-recipe-child-guard");
    let repo = tmp.path();
    write_py_worker(repo);
    write_json(
        repo,
        "basic.json",
        r#"{
            "schema": 1,
            "kind": "recipe",
            "id": "oms-basic",
            "commands": {
                "work": {"argv": ["python3", "worker.py", "write", "result.json", "ok"], "depends": ["worker.py"], "timeout_seconds": 5}
            },
            "steps": [{"id": "work", "command": "work", "outputs": [{"path": "result.json", "min_bytes": 1}]}]
        }"#,
    );
    write_json(
        repo,
        "trig.json",
        r#"{
            "schema": 1,
            "kind": "trigger",
            "id": "trig-guard",
            "recipe": "basic.json",
            "schedule": {"once": true}
        }"#,
    );
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(exe)
        .args([
            "--exact",
            "recipe_child_process_guard_blocks_mutations_allows_plan",
            "--nocapture",
        ])
        .env("OMS_RECIPE_CHILD_TEST_REPO", repo)
        .env("OMS_RECIPE_PROCESS", "1")
        .status()
        .unwrap();
    assert!(status.success());
}
