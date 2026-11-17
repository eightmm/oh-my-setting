//! End-to-end parity contracts for `oms-command-panel` CLI, routing, usage, comparison,
//! native dry-run launch, review settlement, salvage, reader and managed subcommands.
#![forbid(unsafe_code)]

use oms_command_panel::cli::Options;
use oms_command_panel::input::Key;
use oms_command_panel::{commands, reader, recommend, render, run_with_services, usage};
use oms_panel_core::services::ExecutionRequest;
use oms_panel_core::{
    CommandOutput, Error, NativeCommand, PeerExecutor, Result, Services, Store, Value,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static STOP: AtomicBool = AtomicBool::new(false);
static COUNTER: AtomicU64 = AtomicU64::new(1);

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&base)
            .unwrap();
        let inner = base.join("ws");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&inner)
            .unwrap();
        Self(inner.canonicalize().unwrap())
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

fn os(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn sha_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn ensure_private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        ensure_private_dir(parent);
    }
    if !path.exists() {
        std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn ensure_private_oms_dirs(repo: &Path) {
    ensure_private_dir(&repo.join(".oms/artifacts"));
    ensure_private_dir(&repo.join(".oms/locks"));
}

fn write_private_file(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent);
    }
    let _ = std::fs::remove_file(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .unwrap();
}

fn host_boot_id() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(raw) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    "unknown".to_owned()
}

fn write_private_dispatch(repo: &Path, room_id: &str, task_id: &str, member: &str, parent: &str) {
    use std::os::unix::fs::MetadataExt;
    ensure_private_oms_dirs(repo);
    let ctl = oms_panel_core::terminals::controller_store(repo, true).unwrap();
    ensure_private_dir(ctl.repo());
    let dir = ctl.repo().join(member);
    ensure_private_dir(&dir);

    let mut call_map = serde_json::Map::new();
    call_map.insert("backend".into(), json!("subreaper"));
    call_map.insert("control_root".into(), json!(ctl.repo().to_string_lossy()));
    call_map.insert("kind".into(), json!("oms-panel-call"));
    call_map.insert("nonce".into(), json!("0123456789abcdef0123456789abcdef"));
    call_map.insert("parent".into(), json!(parent));
    call_map.insert("participant".into(), json!(member));
    call_map.insert("room".into(), json!(room_id));
    call_map.insert("schema".into(), json!(2));
    call_map.insert("task_id".into(), json!(task_id));
    let call = Value::Object(call_map);
    let call_bytes = serde_json::to_vec(&call).unwrap();
    let anchor_path = repo.join(format!(".oms/artifacts/panel-{member}.json"));
    write_private_file(&anchor_path, &call_bytes);
    let meta = std::fs::symlink_metadata(&anchor_path).unwrap();
    let generation = json!([
        meta.dev(),
        meta.ino(),
        meta.size(),
        meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128,
        meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128
    ]);

    let canon_repo = std::fs::canonicalize(repo)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let boot = host_boot_id();
    let proc_info = json!({"pid": std::process::id(), "identity": "parity-controller"});
    let identity = json!({
        "schema": 1,
        "repo": canon_repo,
        "room": room_id,
        "participant": member,
        "parent": parent,
        "task_id": task_id,
        "nonce": "0123456789abcdef0123456789abcdef",
        "boot_id": boot,
        "controller": proc_info,
        "root": dir.to_string_lossy(),
    });
    write_private_file(
        &dir.join("identity.json"),
        &serde_json::to_vec(&identity).unwrap(),
    );
    let envelope = |record: Value| {
        serde_json::to_vec(&json!({
            "schema": 1,
            "repo": canon_repo,
            "participant": member,
            "launch": identity,
            "boot_id": boot,
            "writer": proc_info,
            "supervisor": Value::Null,
            "record": record,
        }))
        .unwrap()
    };
    write_private_file(
        &dir.join(format!("{member}-launch.json")),
        &envelope(json!({
            "call": call,
            "generation": generation,
            "verify": "cargo test",
            "execution_repo": canon_repo,
            "rework": Value::Null,
        })),
    );
    let dead_pid = 2_000_000_000u64;
    write_private_file(
        &dir.join(format!("{member}-process.json")),
        &envelope(json!({"pid": dead_pid})),
    );
    write_private_file(
        &dir.join(format!("{member}-supervisor.json")),
        &envelope(json!({
            "launch_sha256": sha_hex(&call_bytes),
            "pid": dead_pid,
            "all_reaped": true,
            "exit": 130,
        })),
    );
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn init_git_repo(dir: &Path) -> String {
    git_ok(dir, &["init", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "base line\n").unwrap();
    git_ok(dir, &["add", "README.md"]);
    git_ok(dir, &["commit", "-m", "init"]);
    git_ok(dir, &["rev-parse", "HEAD"])
}

fn json_out(value: Value) -> CommandOutput {
    CommandOutput {
        stdout: serde_json::to_vec(&value).unwrap(),
        cleanup_complete: true,
        output_complete: true,
        ..Default::default()
    }
}

#[derive(Default)]
struct Fake {
    attempts: RefCell<Vec<Value>>,
    telemetry: RefCell<Vec<Value>>,
    room: RefCell<Value>,
    plan: RefCell<Value>,
    tmux_panes: RefCell<String>,
    tmux_windows: RefCell<String>,
    tmux_owner: RefCell<String>,
    tmux_missing_session: std::cell::Cell<bool>,
    replace_artifacts_on_tmux: std::cell::Cell<bool>,
    commands_seen: RefCell<Vec<(String, Vec<String>)>>,
    native_seen: RefCell<Vec<(NativeCommand, Vec<OsString>)>>,
    real_catalog: std::cell::Cell<bool>,
}

impl PeerExecutor for Fake {
    fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
        let args: Vec<String> = request
            .argv
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        self.commands_seen
            .borrow_mut()
            .push((request.cwd.display().to_string(), args.clone()));
        if args.first().is_some_and(|v| v == "git") {
            let mut command = Command::new(&request.argv[0]);
            command
                .env("GIT_AUTHOR_NAME", "Fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
                .env("GIT_COMMITTER_NAME", "Fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
                .args(&request.argv[1..])
                .current_dir(&request.cwd)
                .envs(request.environment.clone())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for name in &request.remove_environment {
                if !request.environment.iter().any(|(key, _)| key == name) {
                    command.env_remove(name);
                }
            }
            let mut child = command.spawn().map_err(Error::new)?;
            drop(child.stdin.take());
            let done = child.wait_with_output().map_err(Error::new)?;
            return Ok(CommandOutput {
                code: done.status.code().unwrap_or(1),
                stdout: done.stdout,
                stderr: done.stderr,
                cleanup_complete: true,
                output_complete: true,
                limited: false,
            });
        }
        if args.first().is_some_and(|v| v == "tmux") {
            let verb = args.get(1).map(String::as_str).unwrap_or("");
            if matches!(verb, "has-session" | "show-options")
                && self.replace_artifacts_on_tmux.get()
            {
                let should_trigger = if self.tmux_missing_session.get() {
                    verb == "has-session"
                } else {
                    verb == "show-options"
                };
                if should_trigger {
                    self.replace_artifacts_on_tmux.set(false);
                    let artifacts = request.cwd.join(".oms/artifacts");
                    let prev = request.cwd.join(".oms/artifacts-prev");
                    let _ = std::fs::remove_dir_all(&prev);
                    std::fs::rename(&artifacts, &prev).unwrap();
                    ensure_private_dir(&artifacts);
                }
            }
            if verb == "has-session" && self.tmux_missing_session.get() {
                return Ok(CommandOutput {
                    code: 1,
                    stderr: b"can't find session\n".to_vec(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                });
            }
            let stdout = match verb {
                "list-panes" => self.tmux_panes.borrow().clone().into_bytes(),
                "list-windows" => self.tmux_windows.borrow().clone().into_bytes(),
                "show-options" => self.tmux_owner.borrow().clone().into_bytes(),
                "display-message" => format!("$1\t{}\tsocket\n", std::process::id()).into_bytes(),
                "has-session" | "select-pane" | "select-window" | "switch-client" => vec![],
                _ => vec![],
            };
            return Ok(CommandOutput {
                stdout,
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            });
        }
        Ok(json_out(Value::Null))
    }
}

impl Services for Fake {
    fn command(
        &self,
        command: NativeCommand,
        _repo: &Path,
        args: &[OsString],
        _: &AtomicBool,
    ) -> Result<CommandOutput> {
        self.native_seen.borrow_mut().push((command, args.to_vec()));
        match command {
            NativeCommand::ProviderCatalog => {
                let policy = oms_panel_core::routing::Policy::bundled()?;
                let mut rows = Vec::new();
                for provider in ["codex", "claude", "antigravity"] {
                    let models = policy
                        .tiers
                        .values()
                        .flatten()
                        .chain(policy.council.extra.iter())
                        .filter(|v| v.provider == provider)
                        .map(|v| v.model.clone())
                        .collect::<Vec<_>>();
                    if self.real_catalog.get() {
                        // Shape of `oms models --json`: no `installed`, `usable` unprobed, models only in `catalog_models`.
                        rows.push(json!({"provider": provider, "present": true, "usable": null, "models": [], "catalog_models": models}));
                    } else {
                        rows.push(
                            json!({"provider": provider, "installed": true, "models": models}),
                        );
                    }
                }
                Ok(json_out(json!({"providers": rows})))
            }
            NativeCommand::Events => {
                if args.first().is_some_and(|v| v == "transition") {
                    let arg = |flag: &str| {
                        args.iter()
                            .position(|v| v == flag)
                            .map(|i| args[i + 1].to_string_lossy().into_owned())
                            .unwrap_or_default()
                    };
                    let id = arg("--attempt");
                    let mut items = self.attempts.borrow_mut();
                    if let Some(row) = items.iter_mut().find(|v| v["attempt_id"] == id) {
                        row["state"] = json!(arg("--state"));
                        row["terminal"] = json!(true);
                    }
                }
                Ok(json_out(Value::Array(self.attempts.borrow().clone())))
            }
            NativeCommand::Room => Ok(json_out(self.room.borrow().clone())),
            NativeCommand::Telemetry => {
                if args.first().is_some_and(|v| v == "record") {
                    let mut entry = serde_json::Map::new();
                    entry.insert(
                        "event_id".into(),
                        json!(format!("ev-{}", self.telemetry.borrow().len() + 1)),
                    );
                    entry.insert("kind".into(), json!("review-outcome"));
                    let mut i = 0;
                    while i < args.len() {
                        let key = args[i].to_string_lossy();
                        if let Some(flag) = key.strip_prefix("--")
                            && i + 1 < args.len()
                        {
                            let val = args[i + 1].to_string_lossy().into_owned();
                            if flag == "review" {
                                if let Ok(v) = serde_json::from_str::<Value>(&val) {
                                    entry.insert("review".into(), v);
                                }
                            } else if flag == "exit" {
                                entry.insert("exit".into(), json!(val.parse::<i64>().unwrap_or(0)));
                            } else {
                                entry.insert(flag.replace('-', "_"), json!(val));
                            }
                            i += 2;
                            continue;
                        }
                        i += 1;
                    }
                    let obj = Value::Object(entry);
                    self.telemetry.borrow_mut().push(obj.clone());
                    return Ok(json_out(obj));
                }
                Ok(json_out(Value::Array(self.telemetry.borrow().clone())))
            }
            NativeCommand::Plan => Ok(json_out(self.plan.borrow().clone())),
            _ => Ok(json_out(Value::Null)),
        }
    }
}

fn setup_fake_room(services: &Fake, room_id: &str) {
    *services.room.borrow_mut() = json!({
        "schema": 1,
        "id": room_id,
        "title": "Parity room",
        "main": "att-main",
        "participants": [
            {
                "participant": "main-owner",
                "attempt": "att-main",
                "role": "main",
                "provider": "claude",
                "model": "claude-opus-5-5",
                "joined": true,
                "seq": 1
            }
        ],
        "messages": [],
        "repo_tasks": [],
        "scopes": []
    });
    services.attempts.borrow_mut().push(json!({
        "attempt_id": "att-main",
        "tool": "panel-main",
        "provider": "claude",
        "state": "working",
        "sequence": 1,
        "terminal": false,
        "refs": {
            "panel_role": "main",
            "panel_room": room_id,
            "panel_room_id": room_id,
            "panel_room_participant": "main-owner",
            "panel_owner": "claude",
            "panel_main": "att-main"
        }
    }));
}

fn cli_validation_and_repo_conflict_parity() {
    // `--continue --scope` without `--dispatch`/`--access` is permitted.
    let parsed: Options = Options::parse(&os(&[
        "--continue",
        "task-a",
        "--scope",
        "src/lib.rs",
        "--brief-file",
        "/tmp/brief.md",
        "--dry-run",
    ]))
    .unwrap();
    assert_eq!(parsed.get("continue"), Some("task-a"));
    assert_eq!(parsed.all("scope"), vec!["src/lib.rs"]);

    // `--next-attention` rejects conflicting view/action flags.
    assert!(Options::parse(&os(&["--next-attention", "--json"])).is_err());
    assert!(Options::parse(&os(&["--next-attention", "--watch"])).is_err());

    // `--dry-run` is rejected on arbitrary read-only report flags.
    assert!(Options::parse(&os(&["--usage", "--dry-run"])).is_err());
    assert!(Options::parse(&os(&["--compare", "cmp", "--dry-run"])).is_err());

    // `--compare` rejects path traversal / invalid task identifiers.
    assert!(Options::parse(&os(&["--compare", "../escape"])).is_err());

    // `--recommend` permits --model and --reasoning-effort without error.
    let rec_opts = Options::parse(&os(&[
        "--recommend",
        "implement",
        "--risk",
        "medium",
        "--size",
        "small",
        "--model",
        "gpt-6-luna",
        "--reasoning-effort",
        "medium",
    ]))
    .unwrap();
    assert_eq!(rec_opts.get("recommend"), Some("implement"));

    // Scratch worktree resolves execution separately while state maps back to shared repo,
    // and conflicting OMS_ROOM_REPO is refused.
    let base = TempDir::new("oms-panel-base");
    let other = TempDir::new("oms-panel-other");
    let wt_root = TempDir::new("oms-panel-wt");
    init_git_repo(base.path());
    init_git_repo(other.path());
    let wt_dir = wt_root.path().join("scratch");
    git_ok(
        base.path(),
        &[
            "worktree",
            "add",
            "-b",
            "oms/scratch-parity",
            &wt_dir.to_string_lossy(),
            "main",
        ],
    );

    let wt_opts = Options::parse(&os(&[
        "--repo",
        &wt_dir.to_string_lossy(),
        "--dispatch",
        "worker",
        "--prompt",
        "hi",
    ]))
    .unwrap();
    let (state_repo, exec_repo) =
        commands::resolve_repositories_with(&wt_opts, base.path(), Some(wt_dir.as_path())).unwrap();
    assert_eq!(state_repo.as_deref(), Some(wt_dir.as_path()));
    assert_eq!(exec_repo.as_deref(), Some(wt_dir.as_path()));

    // Different state repo than OMS_ROOM_REPO conflicts cleanly on dispatch.
    let err =
        commands::resolve_repositories_with(&wt_opts, base.path(), Some(other.path())).unwrap_err();
    assert!(
        err.message.contains("conflicts with") || err.message.contains("inherited room repository"),
        "{err}"
    );

    // Explicit non-git `--repo` returns exit code 3.
    let non_git = TempDir::new("oms-panel-nongit");
    let bad_opts = Options::parse(&os(&[
        "--repo",
        &non_git.path().to_string_lossy(),
        "--routes",
    ]))
    .unwrap();
    let ng_err = commands::resolve_repositories_with(&bad_opts, non_git.path(), None).unwrap_err();
    assert_eq!(ng_err.code, 3);

    let _ = Command::new("git")
        .args(["worktree", "remove", "--force", &wt_dir.to_string_lossy()])
        .current_dir(base.path())
        .status();
}

fn empty_scope_preserved_on_continue_and_dry_run_launch() {
    let repo = TempDir::new("oms-panel-cont");
    init_git_repo(repo.path());
    ensure_private_oms_dirs(repo.path());
    let _store = Store::open(repo.path()).unwrap();
    let services = Fake::default();
    setup_fake_room(&services, "room-cont");
    write_private_dispatch(
        repo.path(),
        "room-cont",
        "task-empty-scope",
        "call-prev",
        "main-owner",
    );
    services.attempts.borrow_mut().push(json!({
        "attempt_id": "att-prev",
        "tool": "panel-delegate",
        "task_id": "task-empty-scope",
        "parent_attempt_id": "att-main",
        "provider": "codex",
        "state": "done",
        "terminal": true,
        "sequence": 2,
        "panel": {"scopes": [], "role": "worker", "access": "write", "workload": "routine", "purpose": "implement", "verify": "cargo test"},
        "refs": {
            "panel_owner": "claude",
            "panel_main": "att-main",
            "panel_main_attempt": "att-main",
            "panel_room": "room-cont",
            "panel_room_id": "room-cont",
            "panel_room_participant": "call-prev",
            "panel_role": "worker",
            "panel_access": "write",
            "panel_workload": "routine",
            "panel_purpose": "implement",
            "panel_verify": "cargo test"
        }
    }));

    let prev_patch_rel = ".oms/artifacts/prev.patch";
    std::fs::write(repo.path().join(prev_patch_rel), b"").unwrap();
    let prev_row = json!({
        "schema": 1,
        "event_id": "ev-prev",
        "ts": "2026-11-10T01:00:00Z",
        "kind": "delegate",
        "task_id": "task-empty-scope",
        "attempt_id": "att-prev",
        "exit": 0,
        "patch": prev_patch_rel,
        "patch_sha256": sha_hex(b"")
    });
    std::fs::write(
        repo.path().join(".oms/artifacts/index.jsonl"),
        format!("{}\n", serde_json::to_string(&prev_row).unwrap()),
    )
    .unwrap();
    services.telemetry.borrow_mut().push(prev_row);

    let brief_path = repo.path().join(".oms/continue-brief.md");
    std::fs::write(&brief_path, "continue work\n").unwrap();
    let (code, stdout, stderr) = run_with_services(
        "panel",
        &os(&[
            "--repo",
            &repo.path().to_string_lossy(),
            "--room",
            "room-cont",
            "--owner",
            "claude",
            "--continue",
            "task-empty-scope",
            "--brief-file",
            &brief_path.to_string_lossy(),
            "--dry-run",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
    let plan: Value = serde_json::from_slice(&stdout).unwrap();
    let argv: Vec<&str> = plan["argv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!argv.contains(&"--scope"), "{plan}");
    assert_eq!(plan["route"]["role"], "worker");
    assert_eq!(plan["route"]["workload"], "routine");

    // Native launch dry-run works cleanly before room setup.
    let (lcode, lout, lerr) = run_with_services(
        "panel",
        &os(&[
            "--repo",
            &repo.path().to_string_lossy(),
            "--launch",
            "codex",
            "--dry-run",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(lcode, 0, "stderr={}", String::from_utf8_lossy(&lerr));
    let lplan: Value = serde_json::from_slice(&lout).unwrap();
    assert_eq!(lplan["executes"], json!(false));
    assert_eq!(lplan["harness"], json!("codex"));
}

fn settle_review_and_recommend_parity() {
    let repo = TempDir::new("oms-panel-settle");
    init_git_repo(repo.path());
    ensure_private_oms_dirs(repo.path());
    let _store = Store::open(repo.path()).unwrap();
    let services = Fake::default();
    setup_fake_room(&services, "room-settle");

    let diff = b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let diff_sha = sha_hex(diff);
    let art_rel = ".oms/artifacts/candidate.patch";
    std::fs::write(repo.path().join(art_rel), diff).unwrap();

    services.attempts.borrow_mut().push(json!({
        "attempt_id": "att-rev",
        "task_id": "task-rev",
        "provider": "codex",
        "state": "review",
        "terminal": false,
        "sequence": 2,
        "created_at": "2020-01-01T00:00:00Z",
        "updated_at": "2020-01-01T00:01:00Z",
        "refs": {
            "panel_owner": "claude",
            "panel_main": "att-main",
            "panel_room": "room-settle",
            "panel_role": "worker",
            "panel_access": "write"
        }
    }));
    services.telemetry.borrow_mut().push(json!({
        "event_id": "ev-fail-rev",
        "kind": "review-outcome",
        "task_id": "task-rev",
        "exit": 1,
        "ts": "2020-01-01T00:02:00Z",
        "review": {
            "candidate": {"patch_sha256": diff_sha},
            "gate_verify_exit": 1,
            "seats": [{"verdict": "fail"}]
        }
    }));
    *services.plan.borrow_mut() = json!({
        "tasks": [
            {"id": "task-rev", "state": "done", "outcome": "completed"}
        ]
    });

    // Dry settle-review with explicit --attempt.
    let (code, stdout, stderr) = run_with_services(
        "panel",
        &os(&[
            "--repo",
            &repo.path().to_string_lossy(),
            "--owner",
            "claude",
            "--settle-review",
            "--attempt",
            "att-rev",
            "--reason",
            "superseded attempt",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
    let dry: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(dry["executes"], json!(false));

    // Apply standalone settlement with fractional aged-days.
    let (acode, aout, aerr) = run_with_services(
        "panel",
        &os(&[
            "--repo",
            &repo.path().to_string_lossy(),
            "--settle-review",
            "--standalone",
            "--aged-days",
            "7.5",
            "--apply",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(acode, 0, "stderr={}", String::from_utf8_lossy(&aerr));
    let applied: Value = serde_json::from_slice(&aout).unwrap();
    assert_eq!(applied["executes"], json!(true));

    // Recommend: bundled rules lint passes, manual risk/size + failed escalation, and --diff works.
    let table_val: Value =
        serde_json::from_str(include_str!("../../../config/task-routing-rules.json")).unwrap();
    assert!(recommend::lint(&table_val).is_empty());

    let (rcode, rout, rerr) = run_with_services(
        "panel",
        &os(&[
            "--recommend",
            "debug",
            "--risk",
            "medium",
            "--size",
            "small",
            "--failed",
            "2",
            "--json",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(rcode, 0, "stderr={}", String::from_utf8_lossy(&rerr));
    let rec: Value = serde_json::from_slice(&rout).unwrap();
    assert_eq!(rec["schema"], json!(1));
    assert!(rec["escalation"].is_object());

    std::fs::write(repo.path().join("README.md"), "updated line\n").unwrap();
    git_ok(repo.path(), &["add", "README.md"]);
    git_ok(repo.path(), &["commit", "-m", "second"]);
    let (dcode, dout, derr) = run_with_services(
        "panel",
        &os(&[
            "--repo",
            &repo.path().to_string_lossy(),
            "--recommend",
            "implement",
            "--diff",
            "HEAD~1..HEAD",
            "--json",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(dcode, 0, "stderr={}", String::from_utf8_lossy(&derr));
    let drec: Value = serde_json::from_slice(&dout).unwrap();
    assert_eq!(drec["diff"]["files"], json!(1));

    // Native dispatcher changes receive the same audit routing as legacy entrypoints.
    let native = "crates/oms/src/main.rs";
    std::fs::create_dir_all(repo.path().join("crates/oms/src")).unwrap();
    std::fs::write(repo.path().join(native), "fn main() {}\n").unwrap();
    git_ok(repo.path(), &["add", native]);
    let (code, out, err) = run_with_services(
        "panel",
        &os(&["--recommend", "implement", "--diff", "HEAD", "--json"]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let native_rec: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(native_rec["risk"], "high");
    assert_eq!(native_rec["diff"]["files"], 1);
}

fn routing_accepts_present_provider_catalog() {
    let repo = TempDir::new("panel-real-catalog");
    init_git_repo(repo.path());
    let services = Fake::default();
    services.real_catalog.set(true);
    let run = |extra: &[&str]| {
        let mut args = vec!["--repo", repo.path().to_str().unwrap()];
        args.extend_from_slice(extra);
        run_with_services(
            "panel",
            &os(&args),
            repo.path(),
            repo.path(),
            &STOP,
            &services,
        )
    };
    let (code, out, err) = run(&["--routes"]);
    assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&err));
    let rt: Value = serde_json::from_slice(&out).unwrap();
    assert!(rt["chosen"]["builder"]["provider"].is_string());
    for dispatch in [
        &["--dispatch", "researcher", "--dry-run"][..],
        &["--dispatch", "builder", "--access", "write", "--dry-run"][..],
    ] {
        let (_, _, err) = run(dispatch);
        let err = String::from_utf8_lossy(&err);
        assert!(
            !err.contains("no eligible installed provider"),
            "{dispatch:?}: {err}"
        );
    }
}

fn usage_sessions_compare_and_routes_parity() {
    let repo = TempDir::new("oms-panel-metrics");
    init_git_repo(repo.path());
    let _store = Store::open(repo.path()).unwrap();
    let services = Fake::default();
    setup_fake_room(&services, "room-usage");

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let report = json!({
        "room": {
            "id": "room-usage",
            "participants": [
                {"participant": "p-main", "role": "main", "provider": "claude", "joined": true, "model": "claude-opus-5-5"},
                {"participant": "p-work", "role": "worker", "provider": "codex", "joined": true, "model": "gpt-6-luna"},
                {"participant": "p-adv", "role": "advisor", "provider": "codex", "joined": true, "model": "gpt-6-astra"}
            ]
        },
        "attempts": {"active_recent": [], "recent": []},
        "provider_status": {
            "claude": {
                "source": "HUD cache",
                "observed_at": now - 60.0,
                "context_left": 62,
                "cache_read_share": 80,
                "five_hour_used": 23,
                "five_hour_resets_at": now + 7230.0,
                "weekly_used": 41,
                "weekly_resets_at": now + 3.0 * 86400.0 + 4.0 * 3600.0 + 1800.0
            },
            "codex": {
                "source": "session log",
                "observed_at": now - 700.0,
                "weekly_used": 62,
                "weekly_resets_at": now + 3.0 * 86400.0 + 4.0 * 3600.0 + 1800.0
            },
            "antigravity": {
                "source": "unavailable"
            }
        }
    });
    let snap = usage::snapshot(&report, now);
    assert_eq!(snap["schema"], json!(1));
    assert_eq!(snap["fresh_seconds"], json!(600));
    assert_eq!(snap["providers"]["claude"]["tightest"], json!("weekly"));
    assert_eq!(
        snap["providers"]["claude"]["seats"],
        json!({"live": 1, "main": 1, "workers": 0})
    );
    assert_eq!(
        snap["providers"]["codex"]["seats"],
        json!({"live": 1, "main": 0, "workers": 1})
    );
    assert_eq!(
        snap["providers"]["codex"]["windows"][0]["stale"],
        json!(true)
    );
    assert_eq!(
        snap["providers"]["codex"]["windows"][0]["level"],
        Value::Null
    );
    assert_eq!(
        snap["providers"]["antigravity"]["windows_unreported"]["5h"],
        json!("agy reports no quota window locally")
    );
    let txt = usage::text(&snap);
    assert!(
        txt.contains("41% used; resets in 3d4h; age 1m; source HUD cache"),
        "{txt}"
    );
    assert!(txt.contains("exhaustion projection: unreported"), "{txt}");

    // `--sessions --count --json`
    let (scode, sout, serr) = run_with_services(
        "panel",
        &os(&[
            "--repo",
            &repo.path().to_string_lossy(),
            "--room",
            "room-usage",
            "--sessions",
            "--count",
            "--json",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(scode, 0, "stderr={}", String::from_utf8_lossy(&serr));
    let scount: Value = serde_json::from_slice(&sout).unwrap();
    assert_eq!(scount["kind"], json!("oms-sessions-count"));

    // `--compare` creation order, only_here / shared, review-outcome join, and null active stats.
    std::fs::create_dir_all(repo.path().join(".oms/artifacts")).unwrap();
    let p1 = b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-1\n+2\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-1\n+2\n";
    let p2 = b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-1\n+2\ndiff --git a/c.txt b/c.txt\n--- a/c.txt\n+++ b/c.txt\n@@ -1 +1 @@\n-1\n+2\n";
    std::fs::write(repo.path().join(".oms/artifacts/p1.patch"), p1).unwrap();
    std::fs::write(repo.path().join(".oms/artifacts/p2.patch"), p2).unwrap();
    let s1 = sha_hex(p1);
    let s2 = sha_hex(p2);

    services.attempts.borrow_mut().push(json!({
        "attempt_id": "cmp-1",
        "task_id": "cmp",
        "provider": "codex",
        "state": "done",
        "created_at": "2026-11-10T01:00:00Z",
        "updated_at": "2026-11-10T01:00:42Z",
        "refs": {"panel_access": "write", "panel_model": "gpt-x", "panel_effort": "high"},
        "usage": {"tokens": 12500, "duration_ms": 0},
        "usage_reports": {"tokens": 1, "duration_ms": 0}
    }));
    services.attempts.borrow_mut().push(json!({
        "attempt_id": "cmp-2",
        "task_id": "cmp",
        "provider": "claude",
        "state": "working",
        "created_at": "2026-11-10T01:01:00Z",
        "updated_at": "2026-11-10T01:01:30Z",
        "refs": {"panel_access": "write", "panel_model": "opus-x", "panel_effort": "high"}
    }));
    let tel_rows = vec![
        json!({"schema": 1, "event_id": "d1", "ts": "2026-11-10T01:00:42Z", "kind": "delegate", "task_id": "cmp", "attempt_id": "cmp-1", "exit": 0, "verify_exit": 0, "patch": ".oms/artifacts/p1.patch", "patch_sha256": s1}),
        json!({"schema": 1, "event_id": "d2", "ts": "2026-11-10T01:01:30Z", "kind": "delegate", "task_id": "cmp", "attempt_id": "cmp-2", "exit": 0, "patch": ".oms/artifacts/p2.patch", "patch_sha256": s2}),
        json!({"schema": 1, "event_id": "r1", "ts": "2026-11-10T01:02:00Z", "kind": "review-outcome", "task_id": "cmp", "exit": 0, "review": {"gate_verify_exit": 0, "candidate": {"patch_sha256": s1}, "seats": [{"verdict": "pass"}, {"verdict": "pass"}]}}),
    ];
    let tel_lines: String = tel_rows
        .iter()
        .map(|r| format!("{}\n", serde_json::to_string(r).unwrap()))
        .collect();
    std::fs::write(repo.path().join(".oms/artifacts/index.jsonl"), tel_lines).unwrap();
    services.telemetry.borrow_mut().extend(tel_rows);

    let (ccode, cout, cerr) = run_with_services(
        "panel",
        &os(&[
            "--repo",
            &repo.path().to_string_lossy(),
            "--compare",
            "cmp",
            "--json",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(ccode, 0, "stderr={}", String::from_utf8_lossy(&cerr));
    let cmp: Value = serde_json::from_slice(&cout).unwrap();
    assert_eq!(cmp["ranked"], json!(false));
    assert_eq!(cmp["attempts"][0]["only_here"], json!(["b.txt"]));
    assert_eq!(cmp["attempts"][0]["shared"], json!(["a.txt"]));
    assert_eq!(cmp["attempts"][0]["duration_s"], json!(42));
    assert_eq!(cmp["attempts"][0]["review"]["seats"], json!({"pass": 2}));
    assert_eq!(cmp["attempts"][1]["tokens"], Value::Null);
    assert_eq!(cmp["attempts"][1]["duration_s"], Value::Null);
    assert_eq!(cmp["attempts"][1]["verify"]["status"], json!("unrecorded"));

    // `--routes` includes writers, kinds, owner and chosen per role.
    let (rtcode, rtout, rterr) = run_with_services(
        "panel",
        &os(&[
            "--repo",
            &repo.path().to_string_lossy(),
            "--room",
            "room-usage",
            "--routes",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(rtcode, 0, "stderr={}", String::from_utf8_lossy(&rterr));
    let rt: Value = serde_json::from_slice(&rtout).unwrap();
    assert!(rt["writers"].is_array());
    assert!(rt["kinds"].is_array());
    assert!(rt["owner"].is_string());
    assert!(rt["chosen"]["builder"]["provider"].is_string());
}

fn salvage_via_recorded_kept_and_reader_render_and_redaction() {
    let repo = TempDir::new("oms-panel-salvage");
    let base = init_git_repo(repo.path());
    let _store = Store::open(repo.path()).unwrap();

    // Create a detached kept worktree under OMS_DELEGATE_WORKTREE_ROOT with an uncommitted change to salvage.
    let wt_root = PathBuf::from(std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT").unwrap());
    let kept_parent = wt_root.join("oh-my-setting-delegate.test1");
    ensure_private_dir(&kept_parent);
    let kept_wt = kept_parent.join("wt-test1");
    git_ok(
        repo.path(),
        &[
            "worktree",
            "add",
            "--detach",
            &kept_wt.to_string_lossy(),
            &base,
        ],
    );
    let kept_wt_canon = std::fs::canonicalize(&kept_wt).unwrap();
    let repo_canon = std::fs::canonicalize(repo.path()).unwrap();
    write_private_file(
        &kept_parent.join(".oh-my-setting-tmp"),
        format!(
            "kind=oh-my-setting-temp\ntemporary=1\nrepo={}\nworktree={}\n",
            repo_canon.display(),
            kept_wt_canon.display()
        )
        .as_bytes(),
    );
    std::fs::write(kept_wt_canon.join("salvaged.txt"), "recovered work\n").unwrap();

    let log_body = format!(
        "failed early\nworktree kept at {}\n",
        kept_wt_canon.display()
    );
    let log_sha = sha_hex(log_body.as_bytes());
    let log_rel = ".oms/artifacts/salvage-log.txt";
    ensure_private_oms_dirs(repo.path());
    std::fs::write(repo.path().join(log_rel), &log_body).unwrap();

    let services = Fake::default();
    setup_fake_room(&services, "room-salvage");
    write_private_dispatch(
        repo.path(),
        "room-salvage",
        "task-salvage",
        "call-salvage",
        "main-owner",
    );
    services.attempts.borrow_mut().push(json!({
        "attempt_id": "att-stop",
        "tool": "panel-delegate",
        "task_id": "task-salvage",
        "parent_attempt_id": "att-main",
        "provider": "codex",
        "state": "stopped",
        "terminal": true,
        "sequence": 2,
        "refs": {
            "panel_owner": "claude",
            "panel_main": "att-main",
            "panel_main_attempt": "att-main",
            "panel_room": "room-salvage",
            "panel_room_id": "room-salvage",
            "panel_room_participant": "call-salvage",
            "panel_role": "worker",
            "panel_access": "write",
            "panel_base_sha": base
        }
    }));
    let del_row = json!({
        "schema": 1,
        "event_id": "ev-salvage",
        "ts": "2026-11-10T01:00:00Z",
        "kind": "delegate",
        "task_id": "task-salvage",
        "attempt_id": "att-stop",
        "exit": 130,
        "artifact": log_rel,
        "artifact_sha256": log_sha,
        "base_sha": base
    });
    std::fs::write(
        repo.path().join(".oms/artifacts/index.jsonl"),
        format!("{}\n", serde_json::to_string(&del_row).unwrap()),
    )
    .unwrap();
    services.telemetry.borrow_mut().push(del_row);

    let (code, stdout, stderr) = run_with_services(
        "panel",
        &os(&[
            "--repo",
            &repo.path().to_string_lossy(),
            "--salvage",
            "task-salvage",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
    let salvaged: Value = serde_json::from_slice(&stdout).unwrap();
    assert!(salvaged["patch_sha256"].is_string(), "{salvaged}");

    // Reader SHA-256 verification and paging + redaction.
    let art_body = concat!(
        "Token ",
        "ghp",
        "_1234567890abcdef1234567890abcdef1234",
        "\nDetail line\n"
    );
    let art_sha = sha_hex(art_body.as_bytes());
    let art_path = ".oms/artifacts/reader-out.txt";
    std::fs::write(repo.path().join(art_path), art_body).unwrap();
    let red = oms_panel_core::redact(art_body.as_bytes()).unwrap();
    assert!(
        !red.contains(concat!("ghp", "_")),
        "token must be redacted: {red}"
    );

    let reader_report = json!({
        "results": {
            "rows": [
                {
                    "task_id": "t-read",
                    "outcome": "completed",
                    "title": "Reader test",
                    "issue": Value::Null,
                    "summary": "Dedup summary",
                    "answer_truncated": true,
                    "result": {"summary": "Dedup summary"},
                    "calls": [
                        {
                            "kind": "delegate",
                            "selected_model": "gpt-6-luna",
                            "reason_code": "ok",
                            "artifact": art_path,
                            "artifact_sha256": art_sha,
                            "answer_truncated": true
                        }
                    ]
                }
            ]
        }
    });
    let rendered = render::results(&reader_report["results"], 80);
    assert_eq!(rendered.matches("Dedup summary").count(), 1);
    assert!(rendered.contains("... (answer truncated)"));

    let mut out = Vec::new();
    let mut keys = vec![Key::Char('r'), Key::Char('q')].into_iter();
    reader::page_with(
        repo.path(),
        &reader_report["results"],
        &mut |_timeout| {
            Ok(keys
                .next()
                .map(|k| vec![k])
                .unwrap_or_else(|| vec![Key::Char('q')]))
        },
        &mut out,
        &|| (80, 20),
        false,
        &STOP,
    )
    .unwrap();
    let paged = String::from_utf8_lossy(&out);
    assert!(
        !paged.contains(concat!("ghp", "_")),
        "paged reader leaked secret"
    );
    assert!(paged.contains("Detail line"));

    // Idle polls draw nothing; scrolling reuses cached lines (the artifact is
    // removed mid-session, so a re-render would fail) and never blanks the TTY.
    let hint = "Tab View  r Raw";
    for tty in [false, true] {
        std::fs::write(repo.path().join(art_path), art_body).unwrap();
        let mut script = vec![
            vec![],
            vec![Key::Char('r')],
            vec![],
            vec![],
            vec![Key::Down],
            vec![],
            vec![Key::Char('q')],
        ]
        .into_iter();
        let mut polls = 0;
        let mut quiet = Vec::new();
        reader::page_with(
            repo.path(),
            &reader_report["results"],
            &mut |_timeout| {
                polls += 1;
                if polls == 5 {
                    let _ = std::fs::remove_file(repo.path().join(art_path));
                }
                Ok(script.next().unwrap_or_else(|| vec![Key::Char('q')]))
            },
            &mut quiet,
            &|| (80, 20),
            tty,
            &STOP,
        )
        .unwrap();
        let quiet = String::from_utf8_lossy(&quiet);
        // Initial frame, view change, and scroll: 3 draws over 7 polls.
        assert_eq!(quiet.matches(hint).count(), 3, "tty={tty}: {quiet:?}");
        assert!(!quiet.contains("\x1b[2J") && !quiet.contains("\x1b[H"));
        assert_eq!(quiet.contains("\x1b[?2026h"), tty);
    }

    // Managed job `_supervise` requires generation and validates against missing store cleanly.
    let (jcode, _jout, jerr) = run_with_services(
        "panel",
        &os(&[
            "job",
            "_supervise",
            "--repo",
            &repo.path().to_string_lossy(),
            "--id",
            "job-missing",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_ne!(jcode, 0);
    assert!(
        String::from_utf8_lossy(&jerr).contains("--generation"),
        "{}",
        String::from_utf8_lossy(&jerr)
    );

    let _ = Command::new("git")
        .args(["worktree", "remove", "--force", &kept_wt.to_string_lossy()])
        .current_dir(repo.path())
        .status();
}

fn clean_env_child(command: &mut Command) {
    command.env_clear();
    for name in ["PATH", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn missing_entry_refuses_before_mutation() {
    if let Some(repo) = std::env::var_os("PANEL_REFUSAL_REPO") {
        let repo = PathBuf::from(repo).canonicalize().unwrap();
        for args in [
            os(&["--reopen"]),
            os(&["--spawn-main", "codex"]),
            os(&["--launch", "codex", "--host", "tmux"]),
            os(&["--launch", "codex", "--host", "inline"]),
        ] {
            let services = Fake::default();
            *services.tmux_panes.borrow_mut() = format!(
                "oms-fixture\t@1\t%1\t0\t\toms panel --repo '{}' --launch codex\n",
                repo.display()
            );
            let (code, _, err) = run_with_services("panel", &args, &repo, &repo, &STOP, &services);
            assert_eq!(code, 2, "{args:?}: {}", String::from_utf8_lossy(&err));
            assert!(
                String::from_utf8_lossy(&err).contains("oms panel executable unavailable"),
                "{args:?}: {}",
                String::from_utf8_lossy(&err)
            );
            assert!(
                services
                    .native_seen
                    .borrow()
                    .iter()
                    .all(|(kind, args)| *kind != NativeCommand::Room
                        || args.first().is_none_or(|arg| arg != "new"))
            );
            assert!(
                services.commands_seen.borrow().iter().all(|(_, args)| {
                    args.first().is_none_or(|arg| arg != "tmux")
                        || args.get(1).is_some_and(|arg| {
                            matches!(
                                arg.as_str(),
                                "list-panes"
                                    | "show-option"
                                    | "show-options"
                                    | "show-environment"
                                    | "has-session"
                            )
                        })
                }),
                "unexpected mutation: {:?}",
                services.commands_seen.borrow()
            );
            assert!(!repo.join(".oms").exists(), "refusal created durable state");
        }
        for with_script in [false, true] {
            if with_script {
                let scripts = repo.join("scripts");
                std::fs::create_dir(&scripts).unwrap();
                write_private_file(&scripts.join("oms"), b"#!/bin/sh\nexit 0\n");
            }
            for args in [
                os(&[
                    "job",
                    "run",
                    "--repo",
                    &repo.to_string_lossy(),
                    "--session",
                    "oms-fixture",
                    "--",
                    "echo",
                    "fixture",
                ]),
                os(&[
                    "job",
                    "restart",
                    "job-fixture",
                    "--repo",
                    &repo.to_string_lossy(),
                    "--session",
                    "oms-fixture",
                    "--generation",
                    "1",
                ]),
            ] {
                let services = Fake::default();
                let (code, _, err) =
                    run_with_services("panel", &args, &repo, &repo, &STOP, &services);
                assert_eq!(code, 2, "{}", String::from_utf8_lossy(&err));
                assert!(
                    String::from_utf8_lossy(&err).contains("oms supervisor executable unavailable"),
                    "{}",
                    String::from_utf8_lossy(&err)
                );
                assert!(
                    !repo.join(".oms").exists(),
                    "job refusal created durable state"
                );
            }
        }
        // Post-acquisition replacement of .oms/artifacts must refuse before creating a room or mutating tmux.
        for (missing_session, args) in [
            (false, os(&["--spawn-main", "codex"])),
            (true, os(&["--launch", "codex", "--host", "tmux"])),
        ] {
            let lock_repo = repo.join(format!("lock-swap-{missing_session}"));
            ensure_private_dir(&lock_repo);
            let scripts = lock_repo.join("scripts");
            ensure_private_dir(&scripts);
            write_private_file(&scripts.join("oms"), b"#!/bin/sh\nexit 0\n");
            let services = Fake::default();
            services.tmux_missing_session.set(missing_session);
            *services.tmux_owner.borrow_mut() = format!("{}\n", lock_repo.display());
            services.replace_artifacts_on_tmux.set(true);
            let (code, _, err) =
                run_with_services("panel", &args, &lock_repo, &lock_repo, &STOP, &services);
            assert_ne!(code, 0, "{args:?}: expected refusal after lock replacement");
            assert!(
                !String::from_utf8_lossy(&err).is_empty(),
                "{args:?}: expected error detail"
            );
            assert!(
                services
                    .native_seen
                    .borrow()
                    .iter()
                    .all(|(kind, args)| *kind != NativeCommand::Room
                        || args.first().is_none_or(|arg| arg != "new")),
                "{args:?}: room created despite invalid start lock: {:?}",
                services.native_seen.borrow()
            );
            assert!(
                services.commands_seen.borrow().iter().all(|(_, args)| {
                    args.first().is_none_or(|arg| arg != "tmux")
                        || args.get(1).is_none_or(|arg| {
                            !matches!(
                                arg.as_str(),
                                "set-environment" | "new-window" | "new-session"
                            )
                        })
                }),
                "{args:?}: tmux mutated despite invalid start lock: {:?}",
                services.commands_seen.borrow()
            );
        }
        return;
    }
    let fixture = TempDir::new("oms-panel-refusal");
    let repo = fixture.path();
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let mut init = Command::new(&git);
    clean_env_child(&mut init);
    assert!(
        init.args(["init", "-q"])
            .current_dir(repo)
            .status()
            .unwrap()
            .success()
    );
    let bin = repo.join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
    for name in ["tmux", "codex"] {
        std::os::unix::fs::symlink(bin.join("git"), bin.join(name)).unwrap();
    }
    let executable = bin.join("panel-parity");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let state = repo.join("state");
    std::fs::create_dir(&state).unwrap();
    let mut child = Command::new(executable);
    clean_env_child(&mut child);
    let output = child
        .arg("missing_entry_refuses_before_mutation")
        .env("PATH", &bin)
        .env("PANEL_REFUSAL_REPO", repo)
        .env("OMS_PANEL_PARITY_ROOT", repo)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CACHE_HOME", &state)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

struct RootGuard(PathBuf);
impl Drop for RootGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn main() {
    if std::env::var_os("OMS_PANEL_PARITY_ROOT").is_none() {
        let base = std::env::temp_dir().join(format!("oms-ppr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&base)
            .unwrap();
        let base = std::fs::canonicalize(&base).unwrap();
        let _guard = RootGuard(base.clone());
        let state = base.join("state");
        let worktrees = base.join("wt");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&state)
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&worktrees)
            .unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(std::env::args_os().skip(1))
            .env("OMS_PANEL_PARITY_ROOT", &base)
            .env("TMPDIR", &base)
            .env("XDG_STATE_HOME", &state)
            .env("OMS_DELEGATE_WORKTREE_ROOT", &worktrees);
        for key in [
            "OMS_ROOM_ID",
            "OMS_ROOM_PARTICIPANT",
            "OMS_ROOM_REPO",
            "OMS_HARNESS_CHILD",
            "OMS_HARNESS_DELEGATE_DEPTH",
            "OMS_PANEL_CHILD",
            "OMS_PANEL_MAIN_ATTEMPT",
            "OMS_EXECUTION_REPO",
        ] {
            cmd.env_remove(key);
        }
        let status = cmd.status().unwrap();
        drop(_guard);
        std::process::exit(status.code().unwrap_or(1));
    }

    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-') && arg != "panel_parity")
        .collect();
    let cases: [(&str, fn()); 7] = [
        (
            "missing_entry_refuses_before_mutation",
            missing_entry_refuses_before_mutation,
        ),
        (
            "cli_validation_and_repo_conflict_parity",
            cli_validation_and_repo_conflict_parity,
        ),
        (
            "empty_scope_preserved_on_continue_and_dry_run_launch",
            empty_scope_preserved_on_continue_and_dry_run_launch,
        ),
        (
            "settle_review_and_recommend_parity",
            settle_review_and_recommend_parity,
        ),
        (
            "routing_accepts_present_provider_catalog",
            routing_accepts_present_provider_catalog,
        ),
        (
            "usage_sessions_compare_and_routes_parity",
            usage_sessions_compare_and_routes_parity,
        ),
        (
            "salvage_via_recorded_kept_and_reader_render_and_redaction",
            salvage_via_recorded_kept_and_reader_render_and_redaction,
        ),
    ];
    let mut ran = 0usize;
    for (name, func) in cases {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        ran += 1;
        func();
        println!("test {name} ... ok");
    }
    println!("panel_parity: {ran} passed; 0 failed");
}
