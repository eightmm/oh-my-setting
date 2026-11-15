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
    let f = OpenOptions::new()
        .read(true)
        .write(!path.is_dir())
        .open(path)
        .unwrap();
    f.set_times(
        FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs)),
    )
    .unwrap();
}

#[test]
fn gc_completion_captures_host_hooks_telemetry_prune_and_orphans() {
    if let Some(repo_raw) = std::env::var_os("OMS_GC_HOOK_REL_LOCK_REPO") {
        let repo = Path::new(&repo_raw);
        let cwd = std::env::current_dir().unwrap();
        let flag = AtomicBool::new(false);
        let mut no_svc = oms_command_state_test_services();
        let repo_s = repo.to_string_lossy().into_owned();
        let hook_args = os_args(&["--repo", &repo_s, "--days", "1", "--apply", "--json"]);
        let (code, stdout, stderr) =
            run_with_host("gc", &hook_args, &cwd, &flag, &mut no_svc, &StubHost);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
        let report = json::parse(&stdout).unwrap();
        assert!(
            !report
                .get("items")
                .array()
                .unwrap()
                .iter()
                .any(|i| matches!(
                    i.text("kind"),
                    "hook-panel-activity" | "hook-state-hint" | "hook-session"
                )),
            "{report:?}"
        );
        assert!(
            !report.get("deferred").array().unwrap().is_empty(),
            "{report:?}"
        );
        return;
    }

    if let Some(repo_raw) = std::env::var_os("OMS_GC_REL_LOCK_TEST_REPO") {
        let repo = Path::new(&repo_raw);
        let cwd = std::env::current_dir().unwrap();
        let flag = AtomicBool::new(false);
        let mut no_svc = oms_command_state_test_services();
        let repo_s = repo.to_string_lossy().into_owned();
        let zero_args = os_args(&["--repo", &repo_s, "--days", "0", "--apply", "--json"]);
        let (code, stdout, stderr) =
            run_with_host("gc", &zero_args, &cwd, &flag, &mut no_svc, &StubHost);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
        let zero_report = json::parse(&stdout).unwrap();
        assert!(
            !zero_report
                .get("items")
                .array()
                .unwrap()
                .iter()
                .any(|i| i.text("kind") == "orphan-peer-tmp"),
            "{zero_report:?}"
        );
        assert!(
            !zero_report.get("deferred").array().unwrap().is_empty(),
            "{zero_report:?}"
        );
        return;
    }

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

    // Aged small-file residue: panel-activity, state-hint, orphan peer wrapper
    // (no tree), a peer wrapper whose tree still exists, and a fresh hint.
    let activity = repo.join(".oms/hooks/panel-activity/who.json");
    let hint = repo.join(".oms/hooks/state-hint.2026-01-01");
    let fresh_hint = repo.join(".oms/hooks/state-hint.2999-01-01");
    let orphan_peer = repo.join(".oms/tmp/peer-orphan");
    let live_peer = repo.join(".oms/tmp/peer-live");
    fs::create_dir_all(activity.parent().unwrap()).unwrap();
    fs::create_dir_all(live_peer.join("tree")).unwrap();
    fs::create_dir_all(&orphan_peer).unwrap();
    fs::write(&activity, b"{}").unwrap();
    fs::write(&hint, b"1\n").unwrap();
    fs::write(&fresh_hint, b"1\n").unwrap();
    fs::write(orphan_peer.join("owner.json"), b"{}").unwrap();
    fs::write(live_peer.join("owner.json"), b"{}").unwrap();
    let extra_peer = repo.join(".oms/tmp/peer-extra");
    let registered_peer = repo.join(".oms/tmp/peer-registered");
    fs::create_dir_all(&extra_peer).unwrap();
    fs::create_dir_all(&registered_peer).unwrap();
    fs::write(extra_peer.join("owner.json"), b"{}").unwrap();
    fs::write(extra_peer.join("note.txt"), b"keep").unwrap();
    fs::write(registered_peer.join("owner.json"), b"{}").unwrap();
    let registry = repo.join(".git/worktrees/peer-registered");
    fs::create_dir_all(&registry).unwrap();
    fs::write(
        registry.join("gitdir"),
        format!("{}/.git\n", registered_peer.join("tree").display()),
    )
    .unwrap();
    chmod_tree_private(repo);
    for p in [
        &activity,
        &hint,
        &orphan_peer.join("owner.json"),
        &live_peer.join("owner.json"),
    ] {
        age_file(p, 1000);
    }
    age_file(&orphan_peer, 1000);
    age_file(&live_peer, 1000);
    let future_mtime = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    age_file(&fresh_hint, future_mtime);
    for p in [&extra_peer, &registered_peer] {
        age_file(&p.join("owner.json"), 1000);
        age_file(p, 1000);
    }

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
    let kinds: Vec<String> = dry_report
        .get("items")
        .array()
        .unwrap()
        .iter()
        .map(|i| i.text("kind").to_owned())
        .collect();
    for kind in ["hook-panel-activity", "hook-state-hint", "orphan-peer-tmp"] {
        assert_eq!(kinds.iter().filter(|k| *k == kind).count(), 1, "{kinds:?}");
    }
    assert!(activity.exists() && hint.exists() && orphan_peer.exists());

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
    assert!(!activity.exists() && !hint.exists() && !orphan_peer.exists());
    assert!(fresh_hint.exists() && live_peer.join("owner.json").exists());
    assert!(extra_peer.join("note.txt").exists() && extra_peer.join("owner.json").exists());
    assert!(
        registered_peer.join("owner.json").exists(),
        "registered kept"
    );
    let final_lines = fs::read_to_string(repo.join(".oms/artifacts/index.jsonl"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(final_lines, 1000);
    let _ = NoHostServices;

    // A locked peer wrapper must not be reclaimed even under --days 0.
    let locked_peer = repo.join(".oms/tmp/peer-locked");
    fs::create_dir_all(&locked_peer).unwrap();
    let locked_owner = locked_peer.join("owner.json");
    fs::write(&locked_owner, b"{}").unwrap();
    chmod_tree_private(repo);
    age_file(&locked_owner, 1000);
    age_file(&locked_peer, 1000);
    let canon_owner = locked_owner.canonicalize().unwrap();
    let mut settings = oms_lock::Settings::from_env().unwrap();
    settings.cwd = repo.canonicalize().unwrap();
    let _held = oms_lock::Lock::for_file(&canon_owner, settings)
        .unwrap()
        .try_acquire()
        .unwrap();
    let zero_args = os_args(&["--repo", &repo_s, "--days", "0", "--apply", "--json"]);
    let (code, stdout, stderr) =
        run_with_host("gc", &zero_args, repo, &flag, &mut no_svc, &StubHost);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    let zero_report = json::parse(&stdout).unwrap();
    assert!(
        !zero_report
            .get("items")
            .array()
            .unwrap()
            .iter()
            .any(|i| i.text("kind") == "orphan-peer-tmp"),
        "{zero_report:?}"
    );
    assert!(
        !zero_report.get("deferred").array().unwrap().is_empty(),
        "{zero_report:?}"
    );
    assert!(locked_owner.is_file(), "locked peer wrapper preserved");
    drop(_held);

    // Relative OMS_LOCK_DIR must resolve against the target repository, not the child's cwd.
    let rel_locks = repo.join("locks");
    let other_cwd = repo.join("elsewhere");
    fs::create_dir_all(&rel_locks).unwrap();
    fs::create_dir_all(&other_cwd).unwrap();
    fs::set_permissions(&rel_locks, Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&other_cwd, Permissions::from_mode(0o750)).unwrap();
    let mut rel_settings = oms_lock::Settings::from_env().unwrap();
    rel_settings.cwd = repo.canonicalize().unwrap();
    rel_settings.lock_dir = Some(PathBuf::from("locks"));
    let _rel_held = oms_lock::Lock::for_file(&canon_owner, rel_settings)
        .unwrap()
        .try_acquire()
        .unwrap();
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(exe)
        .args([
            "--exact",
            "gc_completion_captures_host_hooks_telemetry_prune_and_orphans",
            "--nocapture",
        ])
        .current_dir(&other_cwd)
        .env("OMS_LOCK_DIR", "locks")
        .env("OMS_GC_REL_LOCK_TEST_REPO", repo)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        locked_owner.is_file(),
        "locked peer wrapper preserved across relative OMS_LOCK_DIR"
    );
    drop(_rel_held);

    // Hook writers resolve relative OMS_LOCK_DIR against "/"; GC must match across cwds.
    let hook_lock_root = TempDir::new("oms-gc-hook-locks");
    let hook_lock_canon = hook_lock_root.path().canonicalize().unwrap();
    fs::set_permissions(&hook_lock_canon, Permissions::from_mode(0o700)).unwrap();
    let rel_hook_locks = hook_lock_canon.strip_prefix("/").unwrap().to_path_buf();
    let locked_activity = repo.join(".oms/hooks/panel-activity/locked.json");
    let locked_hint = repo.join(".oms/hooks/state-hint.2020-01-01");
    fs::write(&locked_activity, b"{}").unwrap();
    fs::write(&locked_hint, b"1\n").unwrap();
    chmod_tree_private(repo);
    age_file(&locked_activity, 1000);
    age_file(&locked_hint, 1000);
    let mut hook_settings = oms_lock::Settings::from_env().unwrap();
    hook_settings.cwd = PathBuf::from("/");
    hook_settings.lock_dir = Some(rel_hook_locks.clone());
    let _held_act = oms_lock::Lock::for_file(
        &locked_activity.canonicalize().unwrap(),
        hook_settings.clone(),
    )
    .unwrap()
    .try_acquire()
    .unwrap();
    let _held_hint = oms_lock::Lock::for_file(&locked_hint.canonicalize().unwrap(), hook_settings)
        .unwrap()
        .try_acquire()
        .unwrap();
    let hook_status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "gc_completion_captures_host_hooks_telemetry_prune_and_orphans",
            "--nocapture",
        ])
        .current_dir(&other_cwd)
        .env("OMS_LOCK_DIR", &rel_hook_locks)
        .env("OMS_GC_HOOK_REL_LOCK_REPO", repo)
        .status()
        .unwrap();
    assert!(hook_status.success());
    assert!(
        locked_activity.is_file(),
        "locked hook activity preserved across relative OMS_LOCK_DIR"
    );
    assert!(
        locked_hint.is_file(),
        "locked hook state hint preserved across relative OMS_LOCK_DIR"
    );
}

#[test]
fn gc_keeps_peer_wrapper_when_git_registry_is_unknown() {
    use std::os::unix::fs::symlink;
    // (name, how to break the registry for a wrapper that has no tree)
    type Setup = fn(&Path, &Path);
    let cases: [(&str, Setup); 7] = [
        ("symlinked-worktrees", |repo, _| {
            let real = repo.join("elsewhere");
            fs::create_dir_all(&real).unwrap();
            fs::create_dir_all(repo.join(".git")).unwrap();
            symlink(&real, repo.join(".git/worktrees")).unwrap();
        }),
        ("symlinked-gitdir", |repo, _| {
            let entry = repo.join(".git/worktrees/other");
            fs::create_dir_all(&entry).unwrap();
            fs::write(repo.join("target"), b"/nowhere/.git\n").unwrap();
            symlink(repo.join("target"), entry.join("gitdir")).unwrap();
        }),
        ("non-utf8-gitdir", |repo, _| {
            let entry = repo.join(".git/worktrees/other");
            fs::create_dir_all(&entry).unwrap();
            fs::write(entry.join("gitdir"), [b'/', 0xff, 0xfe, b'\n']).unwrap();
        }),
        ("relative-gitdir", |repo, wrapper| {
            let entry = repo.join(".git/worktrees/other");
            fs::create_dir_all(&entry).unwrap();
            let relative = format!(
                "../../../.oms/tmp/{}/tree/.git\n",
                wrapper.file_name().unwrap().to_string_lossy()
            );
            fs::write(entry.join("gitdir"), relative).unwrap();
        }),
        ("missing-gitdir", |repo, _| {
            fs::create_dir_all(repo.join(".git/worktrees/other")).unwrap();
        }),
        ("dotdot-alias-gitdir", |repo, wrapper| {
            let entry = repo.join(".git/worktrees/other");
            fs::create_dir_all(&entry).unwrap();
            fs::create_dir_all(repo.join(".oms/tmp/other")).unwrap();
            let target = format!(
                "{}/tree/.git\n",
                repo.join(".oms/tmp/other/..")
                    .join(wrapper.file_name().unwrap())
                    .display()
            );
            fs::write(entry.join("gitdir"), target).unwrap();
        }),
        ("symlink-alias-gitdir", |repo, wrapper| {
            let entry = repo.join(".git/worktrees/other");
            fs::create_dir_all(&entry).unwrap();
            let alias = repo.join("alias-wrapper");
            symlink(wrapper, &alias).unwrap();
            fs::write(
                entry.join("gitdir"),
                format!("{}/tree/.git\n", alias.display()),
            )
            .unwrap();
        }),
    ];
    for (name, setup) in cases {
        let tmp = TempDir::new("oms-gc-peer-registry");
        let repo = tmp.path();
        let wrapper = repo.join(".oms/tmp/peer-keep");
        fs::create_dir_all(&wrapper).unwrap();
        fs::create_dir_all(repo.join(".oms/hooks")).unwrap();
        fs::write(wrapper.join("owner.json"), b"{}").unwrap();
        setup(repo, &wrapper);
        chmod_tree_private(repo);
        age_file(&wrapper.join("owner.json"), 1000);
        age_file(&wrapper, 1000);
        let flag = AtomicBool::new(false);
        let args = os_args(&[
            "--repo",
            &repo.to_string_lossy(),
            "--days",
            "1",
            "--apply",
            "--json",
        ]);
        let mut services = oms_command_state_test_services();
        let (code, stdout, stderr) =
            run_with_host("gc", &args, repo, &flag, &mut services, &StubHost);
        assert_eq!(code, 0, "{name}: {}", String::from_utf8_lossy(&stderr));
        let report = json::parse(&stdout).unwrap();
        assert!(
            !report
                .get("items")
                .array()
                .unwrap()
                .iter()
                .any(|i| i.text("kind") == "orphan-peer-tmp"),
            "{name}: {report:?}"
        );
        assert!(
            wrapper.join("owner.json").is_file(),
            "{name}: owner.json kept"
        );
    }
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
