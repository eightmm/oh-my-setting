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
    events_list_unavailable: std::cell::Cell<bool>,
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
    catalog_override: RefCell<Option<Value>>,
    capability_evidence: RefCell<Option<Value>>,
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
                "display-message" if args.last().is_some_and(|v| v == "#{session_id}") => {
                    b"$1\n".to_vec()
                }
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
                        rows.push(json!({"provider": provider, "present": true, "usable": null, "models": [], "catalog_models": models, "catalog_probe":"failed", "catalog_age_seconds":86401, "snapshot_age_seconds":172801, "effort_values":["low","medium","high"]}));
                    } else {
                        rows.push(
                            json!({"provider": provider, "installed": true, "models": models}),
                        );
                    }
                }
                if let Some(catalog) = self.catalog_override.borrow().as_ref() {
                    rows = catalog.as_array().unwrap().clone();
                }
                if let Some(evidence) = self.capability_evidence.borrow().as_ref() {
                    for row in &mut rows {
                        row["capability_evidence"] = evidence.clone();
                    }
                }
                Ok(json_out(json!({"providers": rows})))
            }
            NativeCommand::Events => {
                if args == ["list", "--json"] && self.events_list_unavailable.get() {
                    return Err(Error::new("fixture Events unavailable"));
                }
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

fn chats_native_identity_binding() {
    let repo = TempDir::new("oms-panel-chats");
    init_git_repo(repo.path());
    let services = Fake::default();
    setup_fake_room(&services, "room-chat");
    services.room.borrow_mut()["participants"][0]["participant"] = json!("att-main");
    services.room.borrow_mut()["participants"][0]
        .as_object_mut()
        .unwrap()
        .remove("attempt");
    services.room.borrow_mut()["participants"]
        .as_array_mut()
        .unwrap()
        .push(json!({"participant":"worker-1","role":"worker","provider":"claude","joined":true}));
    services.attempts.borrow_mut()[0]["refs"]["panel_native_pane"] = json!("%1");
    services.attempts.borrow_mut().push(json!({"attempt_id":"worker-attempt","tool":"peer-delegate","refs":{"panel_room_participant":"worker-1","panel_native_pane":"%2"}}));
    services.room.borrow_mut()["participants"].as_array_mut().unwrap().push(json!({"participant":"worker-missing","role":"worker","provider":"claude","joined":true}));
    services.attempts.borrow_mut().push(json!({"attempt_id":"worker-missing","tool":"peer-delegate","refs":{"panel_native_pane":"%4"}}));
    services.attempts.borrow_mut().push(json!({"attempt_id":"worker-latest","tool":"peer-delegate","refs":{"panel_room_participant":"worker-1","panel_native_pane":"%3","panel_native_session":"worker-latest-session"}}));
    *services.tmux_panes.borrow_mut() = "%1\t@1\t$1\n%2\t@2\t$2\n%3\t@3\t$1\n%4\t@4\t$1\n".into();
    for reference in [Value::Null, json!(""), json!("foreign")] {
        if reference.is_null() {
            services.attempts.borrow_mut()[0]["refs"]
                .as_object_mut()
                .unwrap()
                .remove("panel_room_participant");
        } else {
            services.attempts.borrow_mut()[0]["refs"]["panel_room_participant"] = reference.clone();
        }
        services.attempts.borrow_mut()[0]["refs"]["panel_native_session"] = json!("main-session");
        let (code, out, err) = run_with_services(
            "panel",
            &os(&[
                "--repo",
                repo.path().to_str().unwrap(),
                "--room",
                "room-chat",
                "--chats",
                "--json",
            ]),
            repo.path(),
            repo.path(),
            &STOP,
            &services,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let result: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(result["rows"][0]["terminal"], json!(reference != "foreign"));
        assert_eq!(result["rows"][1]["terminal"], json!(true));
        assert_eq!(
            result["rows"][1]["session_id"],
            json!("worker-latest-session")
        );
        assert_eq!(result["rows"][2]["terminal"], json!(false));
        services.attempts.borrow_mut()[2]["refs"]["panel_room_participant"] =
            json!("foreign-worker");
        let (code, out, _) = run_with_services(
            "panel",
            &os(&[
                "--repo",
                repo.path().to_str().unwrap(),
                "--room",
                "room-chat",
                "--chats",
                "--json",
            ]),
            repo.path(),
            repo.path(),
            &STOP,
            &services,
        );
        assert_eq!(code, 0);
        let foreign: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(foreign["rows"][2]["terminal"], json!(false));
        services.attempts.borrow_mut()[2]["refs"]
            .as_object_mut()
            .unwrap()
            .remove("panel_room_participant");
        assert_eq!(
            result["rows"][0]["session_id"],
            if reference == "foreign" {
                Value::Null
            } else {
                json!("main-session")
            }
        );
        if reference != "foreign" {
            let (code, out, err) = run_with_services(
                "panel",
                &os(&[
                    "--repo",
                    repo.path().to_str().unwrap(),
                    "--room",
                    "room-chat",
                    "--open-chat",
                    "att-main",
                    "--chat-surface",
                    "terminal",
                ]),
                repo.path(),
                repo.path(),
                &STOP,
                &services,
            );
            assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
            let _ = out;
            let seen = services.commands_seen.borrow();
            assert!(seen.iter().any(|(_, args)| {
                args.windows(2).any(|pair| pair == ["select-window", "-t"])
                    && args.iter().any(|v| v == "@1")
            }));
            assert!(seen.iter().any(|(_, args)| {
                args.windows(2).any(|pair| pair == ["select-pane", "-t"])
                    && args.iter().any(|v| v == "%1")
            }));
        }
    }
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

    // A recorded `provider:id` account binds only its own provider: an explicit
    // `--to` for another provider drops it instead of pinning or failing.
    services.attempts.borrow_mut()[0]["refs"]["panel_account"] = json!("codex:snu");
    let brief = brief_path.to_string_lossy().into_owned();
    let repo_arg = repo.path().to_string_lossy().into_owned();
    let cont = |extra: &[&str]| {
        let mut args = vec![
            "--repo",
            &repo_arg,
            "--room",
            "room-cont",
            "--owner",
            "claude",
            "--continue",
            "task-empty-scope",
            "--brief-file",
            &brief,
            "--dry-run",
        ];
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
    let mut builder = services.attempts.borrow().last().unwrap().clone();
    builder["attempt_id"] = json!("att-prev-builder");
    builder["refs"]["panel_role"] = json!("builder");
    builder["panel"]["role"] = json!("builder");
    services.attempts.borrow_mut().push(builder);
    let (code, stdout, stderr) = cont(&[]);
    assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
    let plan: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(plan["route"]["role"], "builder");
    assert_eq!(plan["route"]["purpose"], "builder");
    assert_ne!(cont(&["--purpose", "implement"]).0, 0);
    assert_eq!(plan["route"]["workload"], "routine");
    assert!(!plan["argv"].to_string().contains("--scope"), "{plan}");
    assert!(plan["argv"].to_string().contains("cargo test"), "{plan}");
    services.attempts.borrow_mut().pop();
    let (code, stdout, stderr) = cont(&["--to", "claude"]);
    assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
    let plan: Value = serde_json::from_slice(&stdout).unwrap();
    assert!(!plan["argv"].to_string().contains("--account"), "{plan}");
    assert!(plan["route"].get("account_pinned").is_none(), "{plan}");
    // An explicit account that no registry holds is refused, never ignored.
    let (code, _, _) = cont(&["--to", "codex", "--account", "zz-none"]);
    assert_ne!(code, 0);

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
    let prompt = lplan["argv"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .as_str()
        .unwrap();
    assert!(prompt.contains(&format!("Repository: {}", repo.path().display())));
    assert!(prompt.contains("As Overall Main"));
    assert!(prompt.contains("oms list"));
    assert!(prompt.contains("oms panel --routes --room ROOM --json"));
    assert!(prompt.contains("oms panel --results --room ROOM --task-id ID --json"));
    assert!(prompt.contains("oms panel --orchestrator"));
    assert!(prompt.contains("verify the integrated result"));
    assert!(prompt.contains("record each distinct user request and unresolved prior item"));
    assert!(prompt.contains(
        "`oms agent-plan add-batch --tasks FILE --expected-tasks-sha256 SHA --expected-plan-sha256 PLAN_SHA` (use `absent` if no Plan exists)"
    ));
    assert!(prompt.contains("oms agent-plan --help"));
    assert!(prompt.contains("save a pending Room summary"));
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
        "attempt_id": "att-main",
        "tool": "panel-main",
        "provider": "claude",
        "state": "working",
        "terminal": false
    }));
    services.attempts.borrow_mut().push(json!({
        "attempt_id": "att-rev",
        "task_id": "task-rev",
        "parent_attempt_id": "att-main",
        "provider": "codex",
        "state": "review",
        "terminal": false,
        "exit": 1,
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

    // Malformed panel_review_reason refs must fail consistently before dry-run
    // and apply without invoking native Events.
    let seen_before_bad = services.native_seen.borrow().len();
    for bad_reason in [
        "/abs/path",
        "\\unc\\path",
        "C:/drive/path",
        "../escape",
        "a/../b",
        "..",
    ] {
        for apply in [false, true] {
            let mut argv = vec![
                "--repo".to_owned(),
                repo.path().to_string_lossy().into_owned(),
                "--owner".to_owned(),
                "claude".to_owned(),
                "--settle-review".to_owned(),
                "--attempt".to_owned(),
                "att-rev".to_owned(),
                "--reason".to_owned(),
                bad_reason.to_owned(),
            ];
            if apply {
                argv.push("--apply".to_owned());
            }
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            let (bad_code, _, bad_err) = run_with_services(
                "panel",
                &os(&refs),
                repo.path(),
                repo.path(),
                &STOP,
                &services,
            );
            assert_ne!(
                bad_code, 0,
                "malformed reason {bad_reason:?} (apply={apply}) should fail"
            );
            assert!(
                String::from_utf8_lossy(&bad_err).contains("panel_review_reason"),
                "unexpected stderr for {bad_reason:?} (apply={apply}): {}",
                String::from_utf8_lossy(&bad_err)
            );
        }
    }
    for apply in [false, true] {
        let mut argv = vec![
            "--repo".to_owned(),
            repo.path().to_string_lossy().into_owned(),
            "--settle-review".to_owned(),
            "--standalone".to_owned(),
            "--older-than".to_owned(),
            "1e-250".to_owned(),
        ];
        if apply {
            argv.push("--apply".to_owned());
        }
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (bad_code, _, bad_err) = run_with_services(
            "panel",
            &os(&refs),
            repo.path(),
            repo.path(),
            &STOP,
            &services,
        );
        assert_ne!(
            bad_code, 0,
            "oversized formatted --older-than reason (apply={apply}) should fail"
        );
        assert!(
            String::from_utf8_lossy(&bad_err).contains("panel_review_reason"),
            "unexpected stderr for oversized --older-than (apply={apply}): {}",
            String::from_utf8_lossy(&bad_err)
        );
    }
    assert_eq!(services.native_seen.borrow().len(), seen_before_bad);

    // Prior valid relative-path / punctuation reasons remain accepted on dry-run.
    let (rel_code, rel_out, rel_err) = run_with_services(
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
            "replaced src/review.rs (score=0)",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(rel_code, 0, "stderr={}", String::from_utf8_lossy(&rel_err));
    let rel_dry: Value = serde_json::from_slice(&rel_out).unwrap();
    assert_eq!(rel_dry["executes"], json!(false));
    assert_eq!(
        rel_dry["candidates"][0]["reason"],
        "replaced src/review.rs (score=0)"
    );

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
    assert_eq!(dry["candidates"][0]["attempt_id"], "att-rev");

    // Apply explicit settlement using the event CLI's supported reason ref.
    let (explicit_code, explicit_out, explicit_err) = run_with_services(
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
            "--apply",
        ]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(
        explicit_code,
        0,
        "stderr={}",
        String::from_utf8_lossy(&explicit_err)
    );
    let explicit: Value = serde_json::from_slice(&explicit_out).unwrap();
    assert_eq!(explicit["applied"], json!(["att-rev"]));
    assert!(services.native_seen.borrow().iter().any(|(command, args)| {
        *command == NativeCommand::Events
            && args.first().is_some_and(|arg| arg == "transition")
            && args.windows(2).any(|pair| {
                pair[0] == "--ref" && pair[1] == "panel_review_reason=superseded attempt"
            })
    }));

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

    // Configured categories fill defaults only; explicit risk never loses to them.
    let cat = |args: &[&str]| {
        let mut argv = vec!["--recommend"];
        argv.extend_from_slice(args);
        argv.push("--json");
        let (code, out, _) = run_with_services(
            "panel",
            &os(&argv),
            repo.path(),
            repo.path(),
            &STOP,
            &services,
        );
        (
            code,
            serde_json::from_slice::<Value>(&out).unwrap_or(Value::Null),
        )
    };
    let (code, typo) = cat(&["typo"]);
    assert_eq!(code, 0);
    assert_eq!(typo["task_kind"], "mechanical-edit");
    assert_eq!(
        (typo["risk"].as_str(), typo["size"].as_str()),
        (Some("low"), Some("small"))
    );
    assert_eq!(typo["category"]["defaulted"], json!(["risk", "size"]));
    assert_eq!(typo["tier"], "light");
    let (_, high) = cat(&["typo", "--risk", "high"]);
    assert_eq!(high["risk"], "high");
    assert_eq!(high["category"]["defaulted"], json!(["size"]));
    assert_eq!(high["tier"], "deep");
    assert_ne!(cat(&["typo", "--risk", "low", "--diff", "HEAD"]).0, 0);
    assert_ne!(cat(&["nosuchword"]).0, 0);
    let mut bad = table_val.clone();
    bad["categories"]["debug"] = json!({"kind": "debug", "risk": "low", "size": "small"});
    bad["categories"]["Two"] = json!({"kind": "debug"});
    assert_eq!(recommend::lint(&bad).len(), 3);
    let mut legacy = table_val.clone();
    legacy.as_object_mut().unwrap().remove("categories");
    assert!(recommend::lint(&legacy).is_empty());

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

fn catalog_full_models_are_used_for_routing() {
    if std::env::var_os("OMS_CATALOG_CHILD").is_none() {
        let home = TempDir::new("oms-panel-catalog-home");
        let mut child = Command::new(std::env::current_exe().unwrap());
        clean_env_child(&mut child);
        for key in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "PI_CODING_AGENT_DIR",
            "OMS_MODEL",
            "OMS_ANTIGRAVITY_SETTINGS",
        ] {
            child.env_remove(key);
        }
        let status = child
            .arg("catalog_full_models_are_used_for_routing")
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join(".config"))
            .env("XDG_STATE_HOME", home.path().join(".state"))
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .env("OMS_PANEL_PARITY_ROOT", home.path())
            .env("OMS_CATALOG_CHILD", "1")
            .status()
            .unwrap();
        assert!(status.success(), "catalog routing child failed");
        return;
    }
    let repo = TempDir::new("panel-catalog-full");
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
    // The display list is capped at 20; a newer model can appear later.
    let display: Vec<String> = (0..20).map(|i| format!("unrelated-{i}")).collect();
    let mut full = display.clone();
    full.push("gpt-6.2-terra".into());
    let catalog_routes = |row: Value| {
        *services.catalog_override.borrow_mut() = Some(json!([row]));
        let (code, out, err) = run(&["--routes", "--json"]);
        assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&err));
        serde_json::from_slice::<Value>(&out).unwrap()
    };
    let row = |models: Value, full: Option<Value>| {
        let mut row = json!({"provider":"codex", "present":true, "usable":null, "models":models});
        if let Some(full) = full {
            row["catalog_models"] = full;
        }
        row
    };
    let later = catalog_routes(row(json!(display), Some(json!(full))));
    assert_eq!(
        later["chosen"]["builder"]["model"], "gpt-6.2-terra",
        "{}",
        later["chosen"]["builder"]
    );
    assert_eq!(
        later["chosen"]["builder"]["snapshot"]["catalog"]["count"],
        21
    );
    let legacy = catalog_routes(row(json!(["gpt-6.2-terra"]), None));
    assert_eq!(legacy["chosen"]["builder"]["model"], "gpt-6.2-terra");
    assert_eq!(
        legacy["chosen"]["builder"]["snapshot"]["catalog"]["count"],
        1
    );
    let empty = catalog_routes(row(json!(["gpt-6.2-terra"]), Some(json!([]))));
    assert_eq!(empty["chosen"]["builder"]["model"], "gpt-6.1-terra");
    assert_eq!(
        empty["chosen"]["builder"]["snapshot"]["catalog"]["count"],
        0
    );
    assert!(
        !repo.path().join(".oms").exists(),
        "route preview wrote state"
    );
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
    let proof = std::env::var("OMS_ROUTE_READER_PROOF").ok();
    let (code, out, err) = run(if proof.as_deref() == Some("json") {
        &["--routes", "--json"]
    } else {
        &["--routes"]
    });
    assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&err));
    if proof.as_deref() == Some("tty") {
        assert!(out.is_empty() && err.is_empty());
        assert!(!repo.path().join(".oms").exists());
        assert!(services.native_seen.borrow().iter().all(|(command, args)| {
            (*command == NativeCommand::ProviderCatalog && args == &os(&["--json"]))
                || (*command == NativeCommand::Events && args == &os(&["list", "--json"]))
        }));
        println!("route-reader-proof: read-only TTY completed");
        return;
    }
    let rt: Value = serde_json::from_slice(&out).unwrap();
    let mut expected = serde_json::to_vec_pretty(&rt).unwrap();
    expected.push(b'\n');
    assert_eq!(out, expected, "legacy JSON byte formatting changed");
    assert_eq!(
        rt.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "schema",
            "main",
            "tiers",
            "roles",
            "tie_band",
            "auto_review",
            "council",
            "writers",
            "kinds",
            "owner",
            "chosen"
        ]
        .into()
    );
    assert!(
        !repo.path().join(".oms").exists(),
        "route preview wrote state"
    );
    if proof.as_deref() == Some("json") {
        assert!(err.is_empty());
        println!("route-reader-proof: JSON bypassed TTY reader");
        return;
    }
    assert!(rt["chosen"]["builder"]["provider"].is_string());
    let snapshot = &rt["chosen"]["builder"]["snapshot"];
    assert_eq!(snapshot["schema"], 1);
    assert_eq!(snapshot["policy_schema"], 2);
    assert!(
        snapshot["policy_sha256"]
            .as_str()
            .is_some_and(|v| v.len() == 64)
    );
    assert_eq!(snapshot["served_model"], Value::Null);
    assert_eq!(snapshot["execution_profile"], Value::Null);
    assert_eq!(snapshot["catalog"]["reported_probe"], "failed");
    assert_eq!(snapshot["catalog"]["catalog_age_seconds"], 86401);
    assert_eq!(snapshot["capabilities"]["probe_age_seconds"], 172801);
    assert_eq!(snapshot["capabilities"]["effort_support"], "unknown");
    assert_eq!(
        services
            .native_seen
            .borrow()
            .iter()
            .filter(|(command, _)| *command == NativeCommand::ProviderCatalog)
            .count(),
        1
    );
    let rendered = render::routes(&rt, 120);
    assert!(rendered.contains("Proposed routes (read only)"));
    assert!(rendered.contains("served unknown") && rendered.contains("applied/served unknown"));
    assert!(rendered.contains("reported age 86401 seconds"));
    assert!(rendered.contains("reported probe age 172801 seconds"));
    assert!(rendered.contains("Exact effort support: unknown"));
    assert!(rendered.contains("Entitlement: unknown · billing: unknown · quota: unknown"));
    assert!(rendered.contains("endpoint unknown"));
    assert!(rendered.contains("Reported scope freshness at read: unknown"));
    let envelope = json!({"kind":"oms-panel-routes","routes":rt});
    let missing = repo.path().join("absent-reader-repository");
    for width in 19..=200 {
        let rendered = render::routes(&rt, width);
        assert!(rendered.lines().all(|line| render::width(line) <= width));
    }
    for view in [
        reader::View::Summary,
        reader::View::Info,
        reader::View::Raw,
        reader::View::Events,
        reader::View::Diff,
        reader::View::Files,
        reader::View::Transcript,
    ] {
        let mut reader = reader::Reader::default();
        reader.view = view;
        let lines = reader.render(&missing, &envelope, 40).unwrap();
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|line| render::width(line) <= 40));
    }
    let mut page = Vec::new();
    let mut keys = [Key::Char('i'), Key::Char('r'), Key::Char('q')].into_iter();
    reader::page_with(
        &missing,
        &envelope,
        &mut |_| Ok(vec![keys.next().unwrap()]),
        &mut page,
        &|| (80, 20),
        false,
        &STOP,
    )
    .unwrap();
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains("OMS Reader") && page.contains("Proposed routes"));
    assert!(
        !page.contains(char::from(27)),
        "plain reader emitted terminal escapes"
    );
    assert!(!missing.exists() && !repo.path().join(".oms").exists());

    let mut evidence = rt.clone();
    evidence["chosen"] = json!({"builder": rt["chosen"]["builder"]});
    let snapshot = &mut evidence["chosen"]["builder"]["snapshot"];
    snapshot["configured"]["model"] = "configured-model".into();
    snapshot["requested_model"] = "requested-model".into();
    snapshot["resolved"]["model"] = "resolved-model".into();
    snapshot["requested_effort"] = "low".into();
    snapshot["resolved"]["effort"] = "medium".into();
    snapshot["requested_account"] = "requested-seat".into();
    snapshot["selected_account"] = "selected-seat".into();
    snapshot["account_pinned"] = true.into();
    snapshot["capabilities"]["account_alias"] = "observed-seat".into();
    snapshot["capabilities"]["endpoint"] = "https://models.example.invalid/v1".into();
    snapshot["capabilities"]["evidence"] = json!({"schema":1,"source":"fixture-model-list",
        "account_alias":"observed-seat","endpoint":"https://models.example.invalid/v1",
        "observed_at":900,"expires_at":1100});
    snapshot["capabilities"]["freshness"] = "reported_fresh".into();
    snapshot["capabilities"]["selected_account_matches"] = false.into();
    snapshot["capabilities"]["effort_support"] = "reported_supported".into();
    snapshot["capabilities"]["exact_model_efforts"] = json!(["low", "medium"]);
    let rendered = render::routes(&evidence, 200);
    for expected in [
        "configured configured-model",
        "requested requested-model",
        "resolved resolved-model",
        "requested low",
        "selected medium",
        "applied/served unknown",
        "requested requested-seat",
        "proposed selected-seat",
        "pinned true",
        "Exact effort support: reported_supported",
        "exact model values: low, medium",
        "Capability account: observed-seat",
        "Reported scope freshness at read: reported_fresh",
        "selected account matches false",
        "source fixture-model-list",
        "observed 900",
        "expires 1100",
        "Selected endpoint binding: unknown",
    ] {
        assert!(rendered.contains(expected), "{expected}: {rendered}");
    }
    evidence["chosen"]["builder"]["snapshot"]["capabilities"]["freshness"] =
        "reported_stale".into();
    assert!(render::routes(&evidence, 200).contains("freshness at read: reported_stale"));
    for width in 19..=200 {
        assert!(
            render::routes(&evidence, width)
                .lines()
                .all(|line| render::width(line) <= width)
        );
    }
    evidence["chosen"]["builder"]["snapshot"]["capabilities"]["effort_support"] =
        "reported_unsupported".into();
    assert!(render::routes(&evidence, 200).contains("Exact effort support: reported_unsupported"));
    evidence["chosen"]["builder"]["snapshot"] = Value::Null;
    assert!(
        render::routes(&evidence, 200)
            .contains("Catalog models: unknown · resolved present unknown")
    );
    let untrusted = format!(
        "검증{}[2J{}결합 {}",
        char::from(27),
        char::from_u32(0x202e).unwrap(),
        concat!("ghp", "_1234567890abcdef1234567890abcdef1234")
    );
    evidence["chosen"]["builder"] = json!({"error":untrusted});
    let envelope = json!({"kind":"oms-panel-routes","routes":evidence});
    for view in [reader::View::Summary, reader::View::Raw, reader::View::Info] {
        let mut reader = reader::Reader::default();
        reader.view = view;
        let rendered = reader.render(&missing, &envelope, 40).unwrap().join("\n");
        assert!(
            !rendered.contains(char::from(27))
                && !rendered.contains(char::from_u32(0x202e).unwrap())
        );
        assert!(!rendered.contains(concat!("ghp", "_")));
    }
    assert_eq!(
        services
            .native_seen
            .borrow()
            .iter()
            .filter(|(command, _)| *command == NativeCommand::ProviderCatalog)
            .count(),
        1,
        "rendering queried the provider"
    );
    assert!(!repo.path().join(".oms").exists());

    let (code, out, err) = run(&[
        "--dispatch",
        "worker",
        "--owner",
        "codex",
        "--prompt",
        "inspect source",
        "--to",
        "claude",
        "--model",
        "claude-sonnet-5-5",
        "--reasoning-effort",
        "low",
        "--dry-run",
    ]);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let preview: Value = serde_json::from_slice(&out).unwrap();
    let selected = &preview["route"]["snapshot"];
    assert_eq!(selected["requested_model"], "claude-sonnet-5-5");
    assert_eq!(selected["requested_effort"], "low");
    assert_eq!(selected["configured"]["effort"], "medium");
    assert_eq!(selected["resolved"]["effort"], "low");
    assert_eq!(selected["served_effort"], Value::Null);
    assert_eq!(preview["executes"], false);
    assert!(preview["route_snapshot_sha256"].is_string());
    assert_eq!(
        services
            .native_seen
            .borrow()
            .iter()
            .filter(|(command, _)| *command == NativeCommand::ProviderCatalog)
            .count(),
        2
    );
    assert!(
        !services
            .native_seen
            .borrow()
            .iter()
            .any(|(command, _)| matches!(
                command,
                NativeCommand::Delegate | NativeCommand::Review | NativeCommand::Advise
            ))
    );
    assert!(
        services
            .native_seen
            .borrow()
            .iter()
            .filter(|(command, _)| *command == NativeCommand::ProviderCatalog)
            .all(|(_, args)| args == &os(&["--json"]))
    );

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
    // Injected catalog metadata covers the real consumer path, not a native
    // scoped producer. No account selection or provider call is attested.
    for (expiry, expected) in [(2, "reported_stale"), (u64::MAX, "reported_fresh")] {
        *services.capability_evidence.borrow_mut() = Some(json!({"schema":1,
            "source":"fixture-model-list","account_alias":"observed-seat",
            "endpoint":"https://models.example.invalid/v1","observed_at":1,
            "expires_at":expiry}));
        let (code, out, err) = run(&["--routes", "--json"]);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let report: Value = serde_json::from_slice(&out).unwrap();
        for route in report["chosen"].as_object().unwrap().values() {
            let cap = &route["snapshot"]["capabilities"];
            assert_eq!(cap["freshness"], expected);
            assert_eq!(cap["evidence"]["expires_at"], expiry);
            assert_eq!(cap["account_alias"], "observed-seat");
            assert_eq!(cap["selected_account_matches"], Value::Null);
        }
        assert!(render::routes(&report, 200).contains(expected));
        assert!(!repo.path().join(".oms").exists());
    }
}

fn teams_are_read_only_and_keep_unknown_evidence_and_stable_ids() {
    if std::env::var_os("OMS_TEAM_VIEW_CHILD").is_none() {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("teams_are_read_only_and_keep_unknown_evidence_and_stable_ids")
            .env("OMS_TEAM_VIEW_CHILD", "1")
            .env("OMS_HARNESS_CHILD", "1")
            .env("OMS_HARNESS_DELEGATE_DEPTH", "1")
            .env_remove("OMS_PANEL_READER_BOARD")
            .status()
            .unwrap();
        assert!(
            status.success(),
            "Team read must remain available to a guarded child"
        );
        return;
    }

    let repo = TempDir::new("panel-team-view");
    init_git_repo(repo.path());
    let services = Fake::default();
    let mut list = Vec::new();
    for id in ["team-b", "team-a"] {
        let mut spec: Value = serde_json::from_str(include_str!(
            "../../oms-command-plan/tests/fixtures/team-spec.json"
        ))
        .unwrap();
        spec["id"] = id.into();
        for key in [
            "task_sha256",
            "objective_sha256",
            "criterion_sha256",
            "policy_sha256",
        ] {
            spec[key] = "a".repeat(64).into();
        }
        spec["base_sha"] = "b".repeat(40).into();
        for member in spec["members"].as_array_mut().unwrap() {
            member["route"]["policy_sha256"] = "a".repeat(64).into();
        }
        list.push(json!({"id":id,"spec":spec,"history":[{"sha256":"d".repeat(64)}],"phase":"held","reason":"base changed"}));
    }
    *services.plan.borrow_mut() = json!(list);
    let args = ["--repo", repo.path().to_str().unwrap(), "--teams", "--json"];
    let (code, out, err) = run_with_services(
        "panel",
        &os(&args),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let report: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(report["kind"], "oms-panel-teams");
    assert_eq!(report["roots"][0]["teams"][0]["id"], "team-a");
    assert_eq!(report["roots"][0]["teams"][1]["id"], "team-b");
    assert_eq!(report["roots"][0]["teams"][0]["contract_phase"], "held");
    assert_eq!(report["resources"], Value::Null);
    assert_eq!(report["physical_intents"]["running_count"], Value::Null);
    assert_eq!(report["executes"], false);
    assert!(
        !repo.path().join(".oms").exists(),
        "read view must create no state"
    );
    let seen = services.native_seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0], (NativeCommand::Plan, os(&["team-list", "--json"])));
    drop(seen);
    let (plain_code, plain_out, plain_err) = run_with_services(
        "panel",
        &os(&["--repo", repo.path().to_str().unwrap(), "--teams"]),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(plain_code, 0, "{}", String::from_utf8_lossy(&plain_err));
    let plain_out = String::from_utf8(plain_out).unwrap();
    assert!(plain_out.contains("OMS · Teams") && plain_out.contains("team-a"));
    assert!(!repo.path().join(".oms").exists());

    for width in [19, 40, 80, 120, 200] {
        let rendered = render::teams(&report, width);
        assert!(rendered.lines().all(|line| render::width(line) <= width));
        assert!(rendered.contains("unknown"));
    }
    let rendered = render::teams(&report, 120);
    assert!(rendered.contains("team-a") && rendered.contains("team-b"));
    assert!(rendered.contains("lead-a") && rendered.contains("worker-a"));
    assert!(rendered.contains("base changed"));
    let mut unsafe_display = report.clone();
    unsafe_display["roots"][0]["teams"][0]["members"][0]["display_name"] = format!(
        "검증{}[2J{}결합",
        char::from(27),
        char::from_u32(0x202e).unwrap()
    )
    .into();
    let rendered = render::teams(&unsafe_display, 40);
    assert!(
        !rendered.contains(char::from(27)) && !rendered.contains(char::from_u32(0x202e).unwrap())
    );
    let reader = reader::Reader::default();
    assert!(
        !reader
            .render(Path::new("missing-team-reader-repo"), &report, 80)
            .unwrap()
            .is_empty()
    );
    *services.plan.borrow_mut() = Value::Null;
    let (code, out, _) = run_with_services(
        "panel",
        &os(&args),
        repo.path(),
        repo.path(),
        &STOP,
        &services,
    );
    assert_eq!(code, 0);
    let unavailable: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(unavailable["roots"], Value::Null);
    assert_eq!(unavailable["sources"]["teams"]["status"], "unavailable");
    assert_eq!(
        unavailable["physical_intents"]["running_count"],
        Value::Null
    );
    assert!(!repo.path().join(".oms").exists());
    assert!(Options::parse(&os(&["--teams", "--dispatch", "worker"])).is_err());
    assert!(Options::parse(&os(&["--teams", "--apply"])).is_err());
    assert!(Options::parse(&os(&["--teams", "--dry-run"])).is_err());
}

fn account_options_and_target_conflicts() {
    if std::env::var_os("OMS_ACCOUNT_MODEL_CHILD").is_some() {
        let repo = TempDir::new("panel-account-model");
        init_git_repo(repo.path());
        let services = Fake::default();
        services.real_catalog.set(true);
        let run = |extra: &[&str]| {
            let mut args = vec![
                "--repo",
                repo.path().to_str().unwrap(),
                "--dispatch",
                "worker",
            ];
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
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let cfg = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap()).join("oh-my-setting");
        let work = home.join("acc/codex-work");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&work)
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&cfg)
            .unwrap();
        std::fs::write(
            cfg.join("provider-accounts.json"),
            serde_json::to_vec(&json!({
                "schema": 1,
                "providers": {
                    "codex": {"accounts": [{"id": "work", "home": work}]}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            work.join("models_cache.json"),
            br#"{"models":[{"slug":"gpt-6.1-sol"}]}"#,
        )
        .unwrap();
        let (code, out, err) = run(&[
            "--prompt",
            "look",
            "--dry-run",
            "--to",
            "codex",
            "--account",
            "work",
            "--model",
            "gpt-6.1-sol",
        ]);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let body: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(body["route"]["account"], "work");
        assert_eq!(body["route"]["account_pinned"], true);
        assert_eq!(body["route"]["model"], "gpt-6.1-sol");

        let (code, _, _) = run(&[
            "--prompt",
            "look",
            "--dry-run",
            "--to",
            "codex",
            "--account",
            "work",
        ]);
        assert_ne!(code, 0);

        std::fs::write(
            work.join("models_cache.json"),
            br#"{"models":[{"slug":"gpt-6.1-terra"}]}"#,
        )
        .unwrap();
        let (code, _, _) = run(&[
            "--prompt",
            "look",
            "--dry-run",
            "--to",
            "codex",
            "--account",
            "work",
            "--model",
            "gpt-6.1-sol",
        ]);
        assert_ne!(code, 0);
        return;
    }

    let parse = |args: &[&str]| Options::parse(&os(args));
    assert!(
        parse(&[
            "--dispatch",
            "worker",
            "--prompt",
            "x",
            "--to",
            "codex:account=work"
        ])
        .is_ok()
    );
    assert!(
        parse(&[
            "--dispatch",
            "worker",
            "--prompt",
            "x",
            "--to",
            "pi",
            "--account",
            "w"
        ])
        .is_ok()
    );
    assert!(parse(&["--continue", "t", "--brief-file", "b", "--account", "w"]).is_ok());
    assert!(parse(&["--usage", "--account", "w"]).is_err());
    assert!(parse(&["--account"]).is_err());

    let repo = TempDir::new("panel-account-flags");
    init_git_repo(repo.path());
    let services = Fake::default();
    services.real_catalog.set(true);
    let run = |extra: &[&str]| {
        let mut args = vec![
            "--repo",
            repo.path().to_str().unwrap(),
            "--dispatch",
            "worker",
        ];
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
    let base = ["--prompt", "look", "--dry-run"];
    for bad in [
        &["--to", ":account=w"][..],
        &["--to", "codex:account=-w"][..],
        &["--to", "codex:account=_w"][..],
        &["--to", "codex", "--account", "-w"][..],
        &["--to", "codex:account="][..],
    ] {
        let args: Vec<_> = base.iter().chain(bad).copied().collect();
        let (code, _, err) = run(&args);
        assert_ne!(code, 0, "{bad:?}");
        let err = String::from_utf8_lossy(&err);
        assert!(
            err.contains("account") || err.contains("requires"),
            "{bad:?}: {err}"
        );
    }
    for extra in [
        &["--to", "codex:account=a", "--account", "b"][..],
        &["--to", "codex", "--account", "Bad Id"][..],
        &["--account", "w"][..],
        // No registry in this environment: a requested account cannot pin.
        &["--to", "codex", "--account", "w"][..],
        &["--to", "antigravity:account=w"][..],
    ] {
        let args: Vec<_> = base.iter().chain(extra).copied().collect();
        let (code, _, err) = run(&args);
        assert_ne!(code, 0, "{extra:?}: {}", String::from_utf8_lossy(&err));
    }

    let home = TempDir::new("oms-panel-account-model-home");
    let mut child = Command::new(std::env::current_exe().unwrap());
    clean_env_child(&mut child);
    for key in [
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "PI_CODING_AGENT_DIR",
        "OMS_MODEL",
        "OMS_ANTIGRAVITY_SETTINGS",
    ] {
        child.env_remove(key);
    }
    let status = child
        .arg("account_options_and_target_conflicts")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_STATE_HOME", home.path().join(".state"))
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .env("OMS_PANEL_PARITY_ROOT", home.path())
        .env("OMS_ACCOUNT_MODEL_CHILD", "1")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "account_options_and_target_conflicts model child failed"
    );
}

fn spread_routes_by_panel_room_id() {
    if std::env::var_os("OMS_SPREAD_CHILD").is_none() {
        let home = TempDir::new("oms-panel-spread-home");
        let mut child = Command::new(std::env::current_exe().unwrap());
        clean_env_child(&mut child);
        for key in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "PI_CODING_AGENT_DIR",
            "OMS_MODEL",
            "OMS_ANTIGRAVITY_SETTINGS",
        ] {
            child.env_remove(key);
        }
        let status = child
            .arg("spread_routes_by_panel_room_id")
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join(".config"))
            .env("XDG_STATE_HOME", home.path().join(".state"))
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .env("OMS_PANEL_PARITY_ROOT", home.path())
            .env("OMS_SPREAD_CHILD", "1")
            .status()
            .unwrap();
        assert!(
            status.success(),
            "spread_routes_by_panel_room_id child failed"
        );
        return;
    }

    let repo = TempDir::new("oms-panel-spread");
    init_git_repo(repo.path());
    let brief_dir = TempDir::new("oms-panel-spread-brief");
    let brief = brief_dir.path().join("brief.md");
    std::fs::write(&brief, "Goal: check spread\n").unwrap();
    let services = Fake::default();
    services.real_catalog.set(true);
    setup_fake_room(&services, "room-spread");
    let dispatch = |extra: &[&str]| {
        let mut args = os(&[
            "--repo",
            repo.path().to_str().unwrap(),
            "--room",
            "room-spread",
            "--owner",
            "antigravity",
            "--dispatch",
            "builder",
            "--access",
            "write",
            "--brief-file",
            brief.to_str().unwrap(),
            "--scope",
            "src/lib.rs",
            "--verify",
            "cargo test --locked",
            "--dry-run",
        ]);
        args.extend(os(extra));
        let (code, stdout, stderr) =
            run_with_services("panel", &args, repo.path(), repo.path(), &STOP, &services);
        assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
        serde_json::from_slice::<Value>(&stdout).unwrap()
    };

    // Room has only a `main` attempt (`panel_role: main`): excluded from `recent`, so `claude` starts first.
    assert_eq!(dispatch(&[])["route"]["provider"], "claude");

    // Recording a non-main worker with ONLY `panel_room_id` spreads next dispatch onto `codex`.
    services.attempts.borrow_mut().push(json!({
        "attempt_id": "att-worker-1",
        "created_at": oms_panel_core::timestamp().saturating_sub(60),
        "tool": "peer-delegate",
        "provider": "claude",
        "state": "completed",
        "terminal": true,
        "refs": {
            "panel_room_id": "room-spread",
            "panel_role": "worker"
        }
    }));
    assert_eq!(dispatch(&[])["route"]["provider"], "codex");

    services.attempts.borrow_mut().push(json!({
        "attempt_id": "att-worker-2",
        "created_at": oms_panel_core::timestamp().saturating_sub(60),
        "tool": "peer-delegate",
        "provider": "codex",
        "state": "completed",
        "terminal": true,
        "refs": {
            "panel_room_id": "room-spread",
            "panel_role": "worker"
        }
    }));
    assert_eq!(dispatch(&[])["route"]["provider"], "claude");

    let at = oms_panel_core::timestamp();
    services.attempts.borrow_mut().truncate(2);
    for (id, at, room) in [
        ("att-expired", at - 6 * 3600 - 60, "room-spread"),
        ("att-future", at + 60, "room-spread"),
        ("att-other-room", at - 60, "room-other"),
    ] {
        services.attempts.borrow_mut().push(json!({
            "attempt_id":id,"created_at":at,"provider":"codex",
            "state":if id == "att-expired" {"done"} else {"working"},
            "terminal":id == "att-expired",
            "refs":{"panel_room_id":room,"panel_role":"worker"}
        }));
    }
    assert_eq!(dispatch(&[])["route"]["provider"], "codex");
    {
        let mut attempts = services.attempts.borrow_mut();
        attempts[2]["state"] = json!("verifying");
        attempts[2]["terminal"] = json!(false);
    }
    // A still-running attempt matters even after it leaves the recent window.
    assert_eq!(dispatch(&[])["route"]["provider"], "claude");

    // The route inspector uses the same Events read as routing.
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let seat_a = home.join("private-seat-a");
    let seat_b = home.join("private-seat-b");
    ensure_private_dir(&seat_a);
    ensure_private_dir(&seat_b);
    write_private_file(
        &home.join(".config/oh-my-setting/provider-accounts.json"),
        &serde_json::to_vec(&json!({
            "schema":1, "providers": {"claude":{"policy":"spread","accounts":[
                {"id":"seat-a","home":seat_a}, {"id":"seat-b","home":seat_b}
            ]}}
        }))
        .unwrap(),
    );
    let mut catalog = oms_panel_core::routing::Policy::bundled()
        .unwrap()
        .tiers
        .into_values()
        .flatten()
        .filter(|candidate| candidate.provider == "claude")
        .map(|candidate| candidate.model)
        .collect::<Vec<_>>();
    catalog.sort();
    catalog.dedup();
    services.catalog_override.replace(Some(json!([
        {"provider":"claude","installed":true,"models":catalog}
    ])));
    services.attempts.borrow_mut().truncate(1);
    for alias in ["seat-b", "unconfigured-seat"] {
        for (flag, value, requires) in [
            (
                "--to",
                format!("claude:account={alias}"),
                "requires --dispatch or --continue",
            ),
            (
                "--account",
                alias.to_owned(),
                "--account requires --dispatch or --continue or --orchestrator or --launch",
            ),
        ] {
            let rejected = Fake::default();
            let (code, stdout, stderr) = run_with_services(
                "panel",
                &os(&[
                    "--repo",
                    repo.path().to_str().unwrap(),
                    "--room",
                    "room-spread",
                    "--routes",
                    "--json",
                    flag,
                    &value,
                ]),
                repo.path(),
                repo.path(),
                &STOP,
                &rejected,
            );
            assert_eq!(code, 2);
            assert!(stdout.is_empty());
            let error = String::from_utf8(stderr).unwrap();
            assert!(error.contains(flag) && error.contains(requires), "{error}");
            assert!(rejected.native_seen.borrow().is_empty());
            assert!(rejected.commands_seen.borrow().is_empty());
        }
    }
    let preview = |from_repo: bool| {
        services.native_seen.borrow_mut().clear();
        let (cwd, args) = if from_repo {
            (
                repo.path(),
                os(&[
                    "--repo",
                    repo.path().to_str().unwrap(),
                    "--room",
                    "room-spread",
                    "--routes",
                    "--json",
                ]),
            )
        } else {
            (home.as_path(), os(&["--routes", "--json"]))
        };
        let (code, stdout, stderr) =
            run_with_services("panel", &args, cwd, repo.path(), &STOP, &services);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
        let events_reads = services
            .native_seen
            .borrow()
            .iter()
            .filter(|(command, args)| {
                *command == NativeCommand::Events && args == &["list", "--json"]
            })
            .count();
        assert!(events_reads <= 1);
        if from_repo {
            assert_eq!(events_reads, 1);
        }
        let text = String::from_utf8(stdout).unwrap();
        assert!(!text.contains("private-seat-"));
        serde_json::from_str::<Value>(&text).unwrap()
    };
    let zero = preview(true);
    let evidence = &zero["chosen"]["builder"]["evidence"];
    assert_eq!(evidence["events_source"], "repository_events");
    assert_eq!(
        evidence["recent"],
        json!({"scope":"room","non_main_attempts":0,"window_seconds":21600,"in_flight":0})
    );
    assert_eq!(evidence["account_rank"]["repo_working_attempts"], 0);
    let allocation = &evidence["allocation"];
    let candidates = allocation["candidates"].as_array().unwrap();
    let claude = candidates
        .iter()
        .find(|row| row["provider"] == "claude")
        .unwrap();
    let codex = candidates
        .iter()
        .find(|row| row["provider"] == "codex")
        .unwrap();
    assert_eq!(claude["rank"], 1);
    assert_eq!(claude["resolved_model"], zero["chosen"]["builder"]["model"]);
    assert_eq!(claude["registered_quota"]["unavailable"], 2);
    assert_eq!(claude["account_quota"], "unavailable");
    assert_eq!(claude["non_main_attempts"], 0);
    assert_eq!(claude["in_flight"], 0);
    assert_eq!(codex["excluded_by"], "provider_unavailable");
    assert!(codex["rank"].is_null());
    assert!(codex["catalog_count"].is_null());
    assert_eq!(allocation["native_order"], "policy_order");
    assert!(!allocation.to_string().contains("seat-a"));
    assert!(!allocation.to_string().contains("seat-b"));

    services.attempts.borrow_mut().push(json!({
        "attempt_id":"att-queued-seat", "provider":"claude", "tool":"peer-delegate",
        "created_at": oms_panel_core::timestamp().saturating_sub(60),
        "state":"queued", "terminal":false,
        "refs":{"panel_room_id":"room-spread","panel_role":"worker","panel_account":"claude:seat-a"}
    }));
    let queued = preview(true);
    let chosen = &queued["chosen"]["builder"];
    let snapshot = &chosen["snapshot"];
    assert_eq!(chosen["provider"], "claude");
    assert_eq!(snapshot["schema"], 1);
    assert_eq!(snapshot["selected_account"], "seat-a");
    assert_eq!(snapshot["account_pinned"], false);
    assert!(snapshot["served_account"].is_null());
    assert!(snapshot.get("evidence").is_none());
    let evidence = &chosen["evidence"];
    assert_eq!(evidence["provider_disposition"], "provisional");
    assert_eq!(evidence["role"], "builder");
    assert_eq!(evidence["tier"], "routine");
    assert_eq!(evidence["account_reason"], "spread");
    assert_eq!(evidence["launch_account"], "headless_may_reselect");
    assert_eq!(
        evidence["recent"],
        json!({"scope":"room","non_main_attempts":1,"window_seconds":21600,"in_flight":0})
    );
    assert_eq!(
        evidence["account_rank"],
        json!({
            "policy":"spread","eligible_accounts":2,"decided_by":"registry_order",
            "weekly_used_bucket":null,"next_weekly_used_bucket":null,
            "repo_working_attempts":0,"next_repo_working_attempts":0
        })
    );
    let page = reader::Reader::default()
        .render(
            repo.path(),
            &json!({"kind":"oms-panel-routes","routes":queued}),
            150,
        )
        .unwrap()
        .join("\n");
    for phrase in [
        "Harness/provider proposal: claude",
        "Account: requested unknown · proposed seat-a · pinned false · served unknown",
        "Account ranking: spread · eligible 2 · decided by registry order",
        "claude · rank 1 · provider quota unavailable · account quota unavailable",
        "codex · provider unavailable",
        "Registered quota: 0 fresh · 0 stale · 2 unavailable · 0 unverified",
        "Repo working/running attempts: proposed 0 · next 0",
        "headless adapter may reselect",
    ] {
        assert!(page.contains(phrase), "{phrase}");
    }
    assert!(!page.contains("private-seat-"));

    let unpinned = dispatch(&[]);
    assert_eq!(unpinned["route"]["account"], "seat-a");
    assert!(
        !unpinned["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--account")
    );
    let pinned = dispatch(&[
        "--to",
        "claude:account=seat-b",
        "--model",
        "claude-sonnet-5-5",
    ]);
    assert_eq!(pinned["route"]["account_pinned"], true);
    assert_eq!(pinned["route"]["account"], "seat-b");
    assert!(
        pinned["argv"]
            .as_array()
            .unwrap()
            .windows(2)
            .any(|pair| pair == ["--account", "seat-b"])
    );
    // Under the headless loader's broader state filter the queued worker
    // counts as active. Use the real comparator with equal health and quota.
    let select_headless = |running_a| {
        let accounts =
            [("seat-a", &seat_a, running_a), ("seat-b", &seat_b, 0)].map(|(id, home, running)| {
                oms_command_peer::providers::Account {
                    provider: "claude".into(),
                    id: id.into(),
                    home: home.clone(),
                    used: None,
                    running,
                    usable: true,
                    models: None,
                }
            });
        oms_command_peer::providers::select_account_adaptive(
            &accounts,
            true,
            false,
            "provider-default",
            &Default::default(),
            1,
        )
        .unwrap()
        .id
    };
    assert_eq!(select_headless(0), "seat-a");
    assert_eq!(select_headless(1), "seat-b");

    services.attempts.borrow_mut()[1]["state"] = json!("working");
    let working = preview(true);
    let chosen = &working["chosen"]["builder"];
    assert_eq!(chosen["snapshot"]["selected_account"], "seat-b");
    let rank = &chosen["evidence"]["account_rank"];
    assert_eq!(rank["decided_by"], "working_attempts");
    assert_eq!(rank["repo_working_attempts"], 0);
    assert_eq!(rank["next_repo_working_attempts"], 1);
    assert_eq!(chosen["evidence"]["recent"]["in_flight"], 1);

    services.attempts.borrow_mut().push(json!({
        "attempt_id":"att-misbound-account","provider":"codex","tool":"peer-delegate",
        "state":"working","terminal":false,
        "refs":{"panel_room_id":"room-spread","panel_role":"worker","panel_account":"claude:seat-b"}
    }));
    let guarded = preview(true);
    let chosen = &guarded["chosen"]["builder"];
    assert_eq!(chosen["snapshot"]["selected_account"], "seat-b");
    assert_eq!(
        chosen["evidence"]["account_rank"]["repo_working_attempts"],
        0
    );

    let duplicate = services.attempts.borrow()[1].clone();
    services.attempts.borrow_mut().push(duplicate);
    let guarded = preview(true);
    let chosen = &guarded["chosen"]["builder"];
    assert_eq!(chosen["snapshot"]["selected_account"], "seat-a");
    assert_eq!(
        chosen["evidence"]["account_rank"]["repo_working_attempts"],
        0
    );
    services.attempts.borrow_mut().truncate(2);

    services.events_list_unavailable.set(true);
    let failed = preview(true);
    services.events_list_unavailable.set(false);
    services.attempts.borrow_mut().push(Value::Null);
    let malformed = preview(true);
    assert!(render::routes(&failed, 150).contains("registry order (workload unknown)"));
    assert!(render::routes(&malformed, 150).contains("incomplete working/running inputs"));
    for (report, source) in [
        (&failed, "unavailable"),
        (&malformed, "unavailable"),
        (&preview(false), "no_repository"),
    ] {
        let evidence = &report["chosen"]["builder"]["evidence"];
        assert_eq!(evidence["events_source"], source);
        assert!(evidence["recent"]["non_main_attempts"].is_null());
        assert!(evidence["account_rank"]["repo_working_attempts"].is_null());
        assert!(evidence["account_rank"]["next_repo_working_attempts"].is_null());
        for candidate in evidence["allocation"]["candidates"].as_array().unwrap() {
            assert!(candidate["non_main_attempts"].is_null());
            assert!(candidate["in_flight"].is_null());
        }
        let page = render::routes(report, 150);
        assert!(page.contains("Repo working/running attempts: proposed unknown · next unknown"));
    }

    // With two native candidates and no comparable weekly inputs, the preview
    // resolves the same room-local activity tie-break as dispatch.
    services.attempts.borrow_mut().pop();
    let mut catalog = services.catalog_override.borrow_mut();
    catalog
        .as_mut()
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(json!({
            "provider":"codex","installed":true,"models":["gpt-6.2-terra"]
        }));
    drop(catalog);
    let report = preview(true);
    let builder = &report["chosen"]["builder"];
    let allocation = &builder["evidence"]["allocation"];
    assert_eq!(
        allocation["native_order"],
        "activity_missing_weekly_evidence"
    );
    assert_eq!(builder["provider"], "codex");
    let candidates = allocation["candidates"].as_array().unwrap();
    let codex = candidates
        .iter()
        .find(|row| row["provider"] == "codex")
        .unwrap();
    let claude = candidates
        .iter()
        .find(|row| row["provider"] == "claude")
        .unwrap();
    assert_eq!(codex["rank"], 1);
    assert_eq!(codex["in_flight"], 0);
    assert_eq!(claude["rank"], 2);
    assert_eq!(claude["in_flight"], 1);
    assert_eq!(dispatch(&[])["route"]["provider"], "codex");
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
    assert!(snap["providers"].get("pi").is_none());

    let mut report_pi = report.clone();
    report_pi["provider_status"]["pi"] = json!({
        "account_label": "work",
        "accounts": {"work": {"available": true, "weekly_used": null}}
    });
    let snap_pi = usage::snapshot(&report_pi, now);
    assert_eq!(
        snap_pi["providers"]["pi"]["account"]["label"],
        json!("work")
    );
    assert_eq!(
        snap_pi["providers"]["pi"]["account"]["accounts"]["work"]["available"],
        json!(true)
    );
    assert_eq!(
        snap_pi["providers"]["pi"]["windows_unreported"]["5h"],
        json!("pi reports no quota window locally")
    );
    let txt_pi = usage::text(&snap_pi);
    assert!(
        txt_pi.contains("Pi · 0 live (0 main, 0 worker) · account work"),
        "{txt_pi}"
    );
    assert!(
        txt_pi.contains("pi reports no quota window locally"),
        "{txt_pi}"
    );

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

fn verify_reader_accepts_map(path: &Path) -> std::collections::BTreeMap<String, PathBuf> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    assert!(
        path.is_absolute(),
        "map path must be absolute: {}",
        path.display()
    );
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            rustix::fs::OFlags::NOFOLLOW.bits() as i32 | rustix::fs::OFlags::NONBLOCK.bits() as i32,
        )
        .open(path)
        .unwrap_or_else(|e| panic!("reader failed to open {}: {e}", path.display()));
    let meta = file.metadata().unwrap();
    assert!(meta.is_file());
    assert_eq!(meta.nlink(), 1);
    assert_eq!(meta.uid(), rustix::process::geteuid().as_raw());
    assert_eq!(
        meta.mode() & 0o7777,
        0o600,
        "expected mode 0600, got {:o}",
        meta.mode() & 0o7777
    );
    assert_eq!(meta.mode() & 0o022, 0);
    let mut raw = String::new();
    (&mut file).take(16385).read_to_string(&mut raw).unwrap();
    assert!(
        raw.len() <= 16384,
        "map exceeded 16 KiB: {} bytes",
        raw.len()
    );
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(parsed.is_object(), "expected JSON object");
    let map: std::collections::BTreeMap<String, PathBuf> = serde_json::from_str(&raw).unwrap();
    assert!(!map.is_empty() && map.len() <= 128);
    for (alias, target) in &map {
        assert!((1..=80).contains(&alias.len()));
        assert!(alias.as_bytes()[0].is_ascii_alphanumeric());
        assert!(
            alias
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        );
        assert!(target.is_absolute());
    }
    map
}

fn desktop_projects_registry_and_reader_parity() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = TempDir::new("oms-panel-projects");
    let base = root.path();
    let xdg = base.join("xdg-config");
    let home_dir = base.join("runtime-home");
    let locks = base.join("locks");
    ensure_private_dir(&locks);
    ensure_private_dir(&home_dir);

    let exe = std::env::current_exe().unwrap();
    let run_cli = |cwd: &Path, args: &[&str], envs: &[(&str, &Path)], extra: &[(&str, &str)]| {
        let mut cmd = Command::new(&exe);
        cmd.env_clear()
            .env("OMS_PANEL_PARITY_CLI", "1")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TMPDIR", base)
            .env("OMS_LOCK_DIR", &locks)
            .current_dir(cwd)
            .args(args);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let default_map = xdg.join("oh-my-setting/desktop-projects.json");
    let xdg_env = [
        ("XDG_CONFIG_HOME", xdg.as_path()),
        ("HOME", home_dir.as_path()),
    ];

    // Empty state before map file exists.
    let (code, stdout, stderr) = run_cli(base, &["--projects"], &xdg_env, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("no desktop projects"), "{stdout}");
    let (code, stdout, _) = run_cli(base, &["--projects", "--json"], &xdg_env, &[]);
    assert_eq!(code, 0);
    let initial_json: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(initial_json["projects"], json!([]));

    // Non-checkout directory is refused without creating map file.
    let plain_dir = base.join("plain-folder");
    std::fs::create_dir_all(&plain_dir).unwrap();
    let (code, _, stderr) = run_cli(&plain_dir, &["--register-project"], &xdg_env, &[]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("Git checkout") || stderr.contains("adopted OMS project"),
        "{stderr}"
    );
    assert!(!default_map.exists());

    // 1. Register default alias from current directory (creates directory & mode 0600 file).
    let alpha = base.join("checkouts/alpha-service");
    std::fs::create_dir_all(&alpha).unwrap();
    init_git_repo(&alpha);
    let alpha_canon = alpha.canonicalize().unwrap();
    let (code, stdout, stderr) = run_cli(&alpha, &["--register-project"], &xdg_env, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("alpha-service"), "{stdout}");

    // 7. Written file passes the exact reader rules (mode 0600, object of aliases to absolute paths).
    let parsed_map = verify_reader_accepts_map(&default_map);
    assert_eq!(parsed_map.len(), 1);
    assert_eq!(parsed_map.get("alpha-service"), Some(&alpha_canon));

    // 2. Idempotent re-register (via symlinked path too) succeeds and changes nothing.
    let alpha_link = base.join("alpha-link");
    std::os::unix::fs::symlink(&alpha, &alpha_link).unwrap();
    let meta_before = std::fs::symlink_metadata(&default_map).unwrap();
    let bytes_before = std::fs::read(&default_map).unwrap();
    let (code, _, stderr) = run_cli(
        base,
        &["--register-project", &alpha_link.to_string_lossy()],
        &xdg_env,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let meta_after = std::fs::symlink_metadata(&default_map).unwrap();
    assert_eq!(meta_before.ino(), meta_after.ino());
    assert_eq!(
        (meta_before.mtime(), meta_before.mtime_nsec()),
        (meta_after.mtime(), meta_after.mtime_nsec())
    );
    assert_eq!(bytes_before, std::fs::read(&default_map).unwrap());

    // 3. Alias conflict refusal: different path with colliding default or explicit alias refuses
    //    and names an alternative --alias to choose instead.
    let alpha_other = base.join("other/alpha-service");
    std::fs::create_dir_all(alpha_other.join(".oms")).unwrap();
    let alpha_other_canon = alpha_other.canonicalize().unwrap();
    let (code, _, stderr) = run_cli(&alpha_other, &["--register-project"], &xdg_env, &[]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("--alias alpha-service-2"),
        "expected suggested alias in stderr: {stderr}"
    );
    assert_eq!(verify_reader_accepts_map(&default_map).len(), 1);

    // Registering the adopted OMS project with the suggested alias succeeds and preserves alpha-service.
    let (code, _, stderr) = run_cli(
        base,
        &[
            "--register-project",
            &alpha_other.to_string_lossy(),
            "--alias",
            "alpha-service-2",
        ],
        &xdg_env,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let two_map = verify_reader_accepts_map(&default_map);
    assert_eq!(two_map.len(), 2);
    assert_eq!(two_map.get("alpha-service"), Some(&alpha_canon));
    assert_eq!(two_map.get("alpha-service-2"), Some(&alpha_other_canon));

    // Invalid aliases are refused.
    let overlong = "a".repeat(81);
    for bad_alias in ["-bad", ".bad", "bad/alias", "bad space", &overlong] {
        let (code, _, _) = run_cli(
            &alpha,
            &["--register-project", "--alias", bad_alias],
            &xdg_env,
            &[],
        );
        assert_ne!(code, 0, "accepted invalid alias {bad_alias}");
    }

    // --projects and --projects --json list aliases, paths, and report when a path no longer exists.
    std::fs::remove_dir_all(&alpha_other).unwrap();
    let (code, plain_list, stderr) = run_cli(base, &["--projects"], &xdg_env, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(plain_list.contains("alpha-service\t"), "{plain_list}");
    assert!(
        plain_list.contains("alpha-service-2") && plain_list.contains("no longer exists"),
        "{plain_list}"
    );
    let (code, json_list, stderr) = run_cli(base, &["--projects", "--json"], &xdg_env, &[]);
    assert_eq!(code, 0, "{stderr}");
    let listed: Value = serde_json::from_str(&json_list).unwrap();
    assert_eq!(
        listed["projects"],
        json!([
            {"alias": "alpha-service", "area": null, "path": alpha_canon, "exists": true},
            {"alias": "alpha-service-2", "area": null, "path": alpha_other_canon, "exists": false},
        ])
    );

    // Sidecar area lifecycle: set (trimming spaces + Hangul), keep without --area, update, clear,
    // and remove on unregister while keeping desktop-projects.json unchanged in format.
    let default_areas = xdg.join("oh-my-setting/desktop-project-areas.json");
    assert!(!default_areas.exists());
    let overlong_area = "a".repeat(25);
    let overlong_hangul = "가".repeat(9);
    for bad_area in [
        "   ",
        "\t",
        "bad/area",
        "bad:area",
        "area\n",
        "emoji🚀",
        &overlong_area,
        &overlong_hangul,
    ] {
        let (code, _, stderr) = run_cli(
            &alpha,
            &["--register-project", "--area", bad_area],
            &xdg_env,
            &[],
        );
        assert_ne!(code, 0, "accepted invalid area {bad_area:?}");
        assert!(stderr.contains("invalid project area"), "{stderr}");
        assert!(!default_areas.exists());
    }

    let map_before_area = std::fs::read(&default_map).unwrap();
    let map_ino_before = std::fs::symlink_metadata(&default_map).unwrap().ino();
    let (code, _, stderr) = run_cli(
        &alpha,
        &["--register-project", "--area", "  코어 infra_1.0-a  "],
        &xdg_env,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    // Updating area on an already-registered project leaves desktop-projects.json untouched.
    assert_eq!(
        std::fs::symlink_metadata(&default_map).unwrap().ino(),
        map_ino_before
    );
    assert_eq!(std::fs::read(&default_map).unwrap(), map_before_area);
    let areas_meta = std::fs::symlink_metadata(&default_areas).unwrap();
    assert!(areas_meta.is_file());
    assert_eq!(areas_meta.nlink(), 1);
    assert_eq!(areas_meta.mode() & 0o7777, 0o600);
    let parsed_areas: Value =
        serde_json::from_slice(&std::fs::read(&default_areas).unwrap()).unwrap();
    assert_eq!(parsed_areas, json!({"alpha-service": "코어 infra_1.0-a"}));

    // Re-registering without --area keeps the existing area without rewriting either file.
    let areas_ino_before = areas_meta.ino();
    let (code, _, stderr) = run_cli(&alpha, &["--register-project"], &xdg_env, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        std::fs::symlink_metadata(&default_areas).unwrap().ino(),
        areas_ino_before
    );

    // --projects and --projects --json display the area next to the alias.
    let (code, area_plain, stderr) = run_cli(base, &["--projects"], &xdg_env, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        area_plain.contains("alpha-service\t코어 infra_1.0-a\t"),
        "{area_plain}"
    );
    let (code, area_json, stderr) = run_cli(base, &["--projects", "--json"], &xdg_env, &[]);
    assert_eq!(code, 0, "{stderr}");
    let listed_with_area: Value = serde_json::from_str(&area_json).unwrap();
    assert_eq!(
        listed_with_area["projects"],
        json!([
            {"alias": "alpha-service", "area": "코어 infra_1.0-a", "path": alpha_canon, "exists": true},
            {"alias": "alpha-service-2", "area": null, "path": alpha_other_canon, "exists": false},
        ])
    );

    // Inline OMS_PANEL_PROJECTS has no sidecar even when XDG_CONFIG_HOME contains one.
    let inline_check = json!({"alpha-service": alpha_canon}).to_string();
    let (code, inline_out, stderr) = run_cli(
        base,
        &["--projects", "--json"],
        &xdg_env,
        &[("OMS_PANEL_PROJECTS", &inline_check)],
    );
    assert_eq!(code, 0, "{stderr}");
    let inline_listed: Value = serde_json::from_str(&inline_out).unwrap();
    assert_eq!(inline_listed["projects"][0]["area"], Value::Null);

    // Set area on second project, clear alpha-service with --area "", and verify unregister removes area.
    std::fs::create_dir_all(alpha_other.join(".oms")).unwrap();
    let (code, _, stderr) = run_cli(
        base,
        &[
            "--register-project",
            &alpha_other.to_string_lossy(),
            "--alias",
            "alpha-service-2",
            "--area",
            "platform",
        ],
        &xdg_env,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = run_cli(&alpha, &["--register-project", "--area", ""], &xdg_env, &[]);
    assert_eq!(code, 0, "{stderr}");
    let after_clear: Value =
        serde_json::from_slice(&std::fs::read(&default_areas).unwrap()).unwrap();
    assert_eq!(after_clear, json!({"alpha-service-2": "platform"}));
    std::fs::remove_dir_all(&alpha_other).unwrap();

    // 4. Unregister preserves remaining entries, refuses unknown aliases, and removes empty map cleanly.
    let (code, _, _) = run_cli(
        base,
        &["--unregister-project", "unknown-alias"],
        &xdg_env,
        &[],
    );
    assert_ne!(code, 0);
    let (code, _, stderr) = run_cli(
        base,
        &["--unregister-project", "alpha-service-2"],
        &xdg_env,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        !default_areas.exists(),
        "unregistering last alias with an area must unlink empty sidecar"
    );
    let remaining = verify_reader_accepts_map(&default_map);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining.get("alpha-service"), Some(&alpha_canon));

    let (code, _, stderr) = run_cli(
        base,
        &["--unregister-project", "alpha-service"],
        &xdg_env,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        !default_map.exists(),
        "empty map file should not remain as invalid {{}}"
    );
    let (code, after_empty, _) = run_cli(base, &["--projects", "--json"], &xdg_env, &[]);
    assert_eq!(code, 0);
    let empty_val: Value = serde_json::from_str(&after_empty).unwrap();
    assert_eq!(empty_val["projects"], json!([]));

    // HOME fallback when XDG_CONFIG_HOME is unset, plus OMS_PANEL_PROJECTS inline refusal.
    let (code, _, stderr) = run_cli(&alpha, &["--register-project"], &[("HOME", &home_dir)], &[]);
    assert_eq!(code, 0, "{stderr}");
    let fallback_map = home_dir.join(".config/oh-my-setting/desktop-projects.json");
    assert_eq!(
        verify_reader_accepts_map(&fallback_map).get("alpha-service"),
        Some(&alpha_canon)
    );
    let inline_json = json!({"inline": alpha_canon}).to_string();
    for args in [
        &["--register-project"][..],
        &["--unregister-project", "inline"],
    ] {
        let (code, _, stderr) = run_cli(
            &alpha,
            args,
            &[("HOME", &home_dir)],
            &[("OMS_PANEL_PROJECTS", &inline_json)],
        );
        assert_ne!(code, 0, "OMS_PANEL_PROJECTS must refuse writer {args:?}");
        assert!(stderr.contains("OMS_PANEL_PROJECTS"), "{stderr}");
    }

    // 8. Child guard refuses delegated workers on mutations.
    let guarded_file = base.join("child-guard-projects.json");
    for child_env in [
        ("OMS_HARNESS_CHILD", "1"),
        ("OMS_HARNESS_DELEGATE_DEPTH", "1"),
    ] {
        let (code, _, stderr) = run_cli(
            &alpha,
            &["--register-project"],
            &[("OMS_PANEL_PROJECTS_FILE", &guarded_file)],
            &[child_env],
        );
        assert_ne!(code, 0, "child guard failed for {child_env:?}");
        assert!(stderr.contains("owning parent"), "{stderr}");
        assert!(!guarded_file.exists());

        let (code, _, stderr) = run_cli(
            &alpha,
            &["--unregister-project", "alpha-service"],
            &[("OMS_PANEL_PROJECTS_FILE", &fallback_map)],
            &[child_env],
        );
        assert_ne!(
            code, 0,
            "child guard failed on unregister for {child_env:?}"
        );
        assert!(stderr.contains("owning parent"), "{stderr}");
        assert!(fallback_map.exists());

        let (code, stdout, stderr) = run_cli(
            &alpha,
            &["--projects", "--json"],
            &[("OMS_PANEL_PROJECTS_FILE", &fallback_map)],
            &[child_env],
        );
        assert_ne!(code, 0, "child guard failed on list for {child_env:?}");
        assert!(stderr.contains("delegated worker"), "{stderr}");
        assert!(stdout.is_empty(), "{stdout}");
    }

    // A pinned panel ignores the map, so the writer and lister refuse rather than drift from it.
    for args in [&["--register-project"][..], &["--projects"]] {
        let (code, _, stderr) = run_cli(
            &alpha,
            args,
            &[
                ("OMS_PANEL_PROJECTS_FILE", &fallback_map),
                ("OMS_PANEL_REPO", &alpha),
            ],
            &[],
        );
        assert_ne!(code, 0, "OMS_PANEL_REPO must refuse {args:?}");
        assert!(stderr.contains("OMS_PANEL_REPO"), "{stderr}");
    }

    // An explicitly named map keeps its last entry: the reader rejects a missing explicit file.
    let (code, _, stderr) = run_cli(
        &alpha,
        &["--unregister-project", "alpha-service"],
        &[("OMS_PANEL_PROJECTS_FILE", &fallback_map)],
        &[],
    );
    assert_ne!(code, 0, "last explicit entry must stay");
    assert!(stderr.contains("last alias"), "{stderr}");
    assert_eq!(
        verify_reader_accepts_map(&fallback_map).get("alpha-service"),
        Some(&alpha_canon)
    );

    // 5. 129th entry refusal via OMS_PANEL_PROJECTS_FILE (preserving all 128 entries).
    let full_file = base.join("full-projects.json");
    let mut seed_map = std::collections::BTreeMap::new();
    for i in 1..=127usize {
        let p = base.join(format!("p/{i}"));
        std::fs::create_dir_all(p.join(".oms")).unwrap();
        seed_map.insert(format!("p{i}"), p.canonicalize().unwrap());
    }
    write_private_file(&full_file, &serde_json::to_vec(&seed_map).unwrap());
    let p128 = base.join("p/128");
    std::fs::create_dir_all(p128.join(".oms")).unwrap();
    let (code, _, stderr) = run_cli(
        &p128,
        &["--register-project", "--alias", "p128"],
        &[("OMS_PANEL_PROJECTS_FILE", &full_file)],
        &[],
    );
    assert_eq!(code, 0, "128th entry should succeed: {stderr}");
    assert_eq!(verify_reader_accepts_map(&full_file).len(), 128);

    // Re-registering an existing entry at 128 still succeeds idempotently.
    let (code, _, stderr) = run_cli(
        &p128,
        &["--register-project", "--alias", "p128"],
        &[("OMS_PANEL_PROJECTS_FILE", &full_file)],
        &[],
    );
    assert_eq!(code, 0, "{stderr}");

    let p129 = base.join("p/129");
    std::fs::create_dir_all(p129.join(".oms")).unwrap();
    let (code, _, stderr) = run_cli(
        &p129,
        &["--register-project", "--alias", "p129"],
        &[("OMS_PANEL_PROJECTS_FILE", &full_file)],
        &[],
    );
    assert_ne!(code, 0, "129th entry must be refused");
    assert!(stderr.contains("128"), "{stderr}");
    assert_eq!(verify_reader_accepts_map(&full_file).len(), 128);

    // 6. Symlinked map refusal (live symlink, dangling symlink) and reader-rejected files
    //    (group/world writable, hard link) are refused without rewriting targets.
    let real_target = base.join("real-target.json");
    let target_body = serde_json::to_vec(&json!({"keep": alpha_canon})).unwrap();
    write_private_file(&real_target, &target_body);
    let sym_map = base.join("symlink-map.json");
    std::os::unix::fs::symlink(&real_target, &sym_map).unwrap();
    for args in [
        &["--register-project", "--alias", "new-entry"][..],
        &["--unregister-project", "keep"],
        &["--projects"],
    ] {
        let (code, _, stderr) =
            run_cli(&alpha, args, &[("OMS_PANEL_PROJECTS_FILE", &sym_map)], &[]);
        assert_ne!(code, 0, "symlinked map must be refused for {args:?}");
        assert!(stderr.contains("symlink"), "{stderr}");
    }
    assert!(
        std::fs::symlink_metadata(&sym_map)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&real_target).unwrap(), target_body);

    let dangling_map = base.join("dangling-map.json");
    std::os::unix::fs::symlink(base.join("nonexistent-target.json"), &dangling_map).unwrap();
    let (code, _, stderr) = run_cli(
        &alpha,
        &["--register-project"],
        &[("OMS_PANEL_PROJECTS_FILE", &dangling_map)],
        &[],
    );
    assert_ne!(code, 0, "dangling symlink map must be refused");
    assert!(stderr.contains("symlink"), "{stderr}");
    assert!(!base.join("nonexistent-target.json").exists());

    // Group-writable (0620) and hard-linked (nlink == 2) files are refused without rewrite.
    let writable_map = base.join("group-writable.json");
    write_private_file(&writable_map, &target_body);
    std::fs::set_permissions(&writable_map, std::fs::Permissions::from_mode(0o620)).unwrap();
    let (code, _, _) = run_cli(
        &alpha,
        &["--register-project", "--alias", "another"],
        &[("OMS_PANEL_PROJECTS_FILE", &writable_map)],
        &[],
    );
    assert_ne!(code, 0, "group-writable map must be refused");
    assert_eq!(std::fs::read(&writable_map).unwrap(), target_body);

    let hard_src = base.join("hard-src.json");
    let hard_dst = base.join("hard-dst.json");
    write_private_file(&hard_src, &target_body);
    std::fs::hard_link(&hard_src, &hard_dst).unwrap();
    let (code, _, _) = run_cli(
        &alpha,
        &["--register-project", "--alias", "another"],
        &[("OMS_PANEL_PROJECTS_FILE", &hard_dst)],
        &[],
    );
    assert_ne!(code, 0, "hard-linked map must be refused");
    assert_eq!(std::fs::read(&hard_src).unwrap(), target_body);

    // Missing explicit OMS_PANEL_PROJECTS_FILE fails on --projects (plain and --json) like the panel reader.
    for missing_explicit in [
        base.join("missing-explicit-map.json"),
        base.join("missing-parent-dir/explicit-map.json"),
    ] {
        for list_args in [&["--projects"][..], &["--projects", "--json"]] {
            let (code, stdout, stderr) = run_cli(
                base,
                list_args,
                &[("OMS_PANEL_PROJECTS_FILE", &missing_explicit)],
                &[],
            );
            assert_ne!(code, 0, "missing explicit map must fail for {list_args:?}");
            assert!(stdout.is_empty(), "{stdout}");
            assert!(stderr.contains("Invalid OMS project map"), "{stderr}");
        }
    }

    // Sidecar privacy (0600 required; 0644/0620, symlink, hard link refused) and strict parsing.
    let strict_dir = base.join("strict-sidecar");
    ensure_private_dir(&strict_dir);
    let strict_map = strict_dir.join("desktop-projects.json");
    let strict_areas = strict_dir.join("desktop-project-areas.json");
    write_private_file(
        &strict_map,
        &serde_json::to_vec(&json!({"keep": alpha_canon})).unwrap(),
    );

    // Non-private (0644) sidecar refused.
    write_private_file(&strict_areas, br#"{"keep":"rust"}"#);
    std::fs::set_permissions(&strict_areas, std::fs::Permissions::from_mode(0o644)).unwrap();
    for args in [
        &["--projects"][..],
        &["--register-project", "--area", "web"],
        &["--unregister-project", "keep"],
    ] {
        let (code, _, stderr) = run_cli(
            &alpha,
            args,
            &[("OMS_PANEL_PROJECTS_FILE", &strict_map)],
            &[],
        );
        assert_ne!(code, 0, "0644 sidecar must be refused for {args:?}");
        assert!(stderr.contains("area map"), "{stderr}");
    }
    std::fs::remove_file(&strict_areas).unwrap();

    // Symlinked sidecar refused.
    let outside_area = base.join("outside-areas.json");
    write_private_file(&outside_area, br#"{"keep":"rust"}"#);
    std::os::unix::fs::symlink(&outside_area, &strict_areas).unwrap();
    let (code, _, stderr) = run_cli(
        &alpha,
        &["--projects"],
        &[("OMS_PANEL_PROJECTS_FILE", &strict_map)],
        &[],
    );
    assert_ne!(code, 0, "symlinked sidecar must be refused");
    assert!(stderr.contains("symlink"), "{stderr}");
    std::fs::remove_file(&strict_areas).unwrap();

    // Strict JSON parse: duplicate keys, non-strings, invalid alias/area, and >16 KiB refused.
    let oversize_areas = format!("{{\"keep\":\"{}\"}}", "a".repeat(16400));
    for bad_sidecar in [
        r#"{"keep":"rust","keep":"web"}"#,
        r#"{"keep":123}"#,
        r#"{"keep":null}"#,
        r#"{"keep":""}"#,
        r#"{"keep":"bad/slash"}"#,
        r#"{"-bad":"rust"}"#,
        r#"["rust"]"#,
        oversize_areas.as_str(),
    ] {
        write_private_file(&strict_areas, bad_sidecar.as_bytes());
        let (code, _, stderr) = run_cli(
            &alpha,
            &["--projects"],
            &[("OMS_PANEL_PROJECTS_FILE", &strict_map)],
            &[],
        );
        assert_ne!(code, 0, "accepted malformed sidecar: {bad_sidecar}");
        assert!(stderr.contains("area map"), "{stderr}");
    }
    std::fs::remove_file(&strict_areas).unwrap();
}

fn snapshot_tree(root: &Path) -> std::collections::BTreeMap<PathBuf, (String, u32, Vec<u8>)> {
    use std::os::unix::fs::MetadataExt;
    fn walk(
        base: &Path,
        dir: &Path,
        out: &mut std::collections::BTreeMap<PathBuf, (String, u32, Vec<u8>)>,
    ) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let rel = path.strip_prefix(base).unwrap().to_path_buf();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let mode = meta.mode() & 0o7777;
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                out.insert(
                    rel,
                    (
                        "symlink".into(),
                        mode,
                        target.to_string_lossy().into_owned().into_bytes(),
                    ),
                );
            } else if meta.is_dir() {
                out.insert(rel, ("dir".into(), mode, Vec::new()));
                walk(base, &path, out);
            } else {
                out.insert(rel, ("file".into(), mode, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn stub_claude_main(state_dir: &Path) -> ! {
    use std::io::Write;
    let cwd = std::env::current_dir().unwrap();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let calls_path = state_dir.join("calls.jsonl");
    let mut log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&calls_path)
        .unwrap();
    writeln!(log_file, "{}", json!({"cwd": cwd, "argv": argv})).unwrap();

    let control: Value = std::fs::read(state_dir.join("control.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));
    let entries_path = state_dir.join("entries.json");
    let mut entries: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Value>> =
        std::fs::read(&entries_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
    let cwd_key = cwd.to_string_lossy().into_owned();

    let args_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    match args_ref.as_slice() {
        ["mcp", "get", name] => {
            if let Some(mode) = control["spawnGrandchildOnGet"].as_str() {
                use std::os::unix::process::CommandExt;
                let ready = state_dir.join("grandchild-ready.pid");
                let _ = std::fs::remove_file(&ready);
                let mut gc = Command::new(std::env::current_exe().unwrap());
                gc.env("OMS_PANEL_PARITY_SLEEP_CHILD", &ready)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit());
                if mode == "new-group" {
                    gc.process_group(0);
                }
                #[expect(
                    clippy::zombie_processes,
                    reason = "stub exits intentionally to leave a lingering grandchild holding stderr open"
                )]
                let _child = gc.spawn().unwrap();
                let wait_until = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while std::time::Instant::now() < wait_until {
                    if std::fs::read_to_string(&ready).is_ok_and(|s| !s.trim().is_empty()) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            if control["signalOnGet"] == true {
                let _ = rustix::process::kill_process(
                    rustix::process::getpid(),
                    rustix::process::Signal::TERM,
                );
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            if let Some(code) = control["getExitCode"].as_i64() {
                if let Some(out_text) = control["getStdout"].as_str() {
                    println!("{out_text}");
                }
                if let Some(err_text) = control["getStderr"].as_str() {
                    eprintln!("{err_text}");
                }
                std::process::exit(code as i32);
            }
            if control["failGet"] == true {
                eprintln!("simulated get failure");
                std::process::exit(2);
            }
            if let Some(cfg) = entries.get(&cwd_key).and_then(|m| m.get(*name)) {
                println!("{name}:");
                println!("  Scope: Local config (private to you in this project)");
                println!("  Status: Connected");
                if let Some(t) = cfg["type"].as_str() {
                    println!("  Type: {t}");
                }
                if let Some(cmd) = cfg["command"].as_str() {
                    println!("  Command: {cmd}");
                }
                let joined_args = cfg["args"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                println!("  Args: {joined_args}");
                println!("  Environment:");
                if let Some(env_obj) = cfg["env"].as_object() {
                    for (k, v) in env_obj {
                        if let Some(s) = v.as_str() {
                            println!("    {k}={s}");
                        }
                    }
                }
                println!("\nTo remove this server, run: claude mcp remove \"{name}\" -s local");
                if control["dropLocalAfterGet"] == true {
                    if let Some(m) = entries.get_mut(&cwd_key) {
                        m.remove(*name);
                    }
                    write_private_file(&entries_path, &serde_json::to_vec(&entries).unwrap());
                }
                std::process::exit(0);
            }
            if control["userEntry"] == true && *name == "oms-panel" {
                println!("oms-panel:");
                println!("  Scope: User config (available in all your projects)");
                println!("  Type: stdio");
                println!("  Command: /tmp/user-scope-oms/oms");
                println!("  Args: mcp-server");
                println!("  Environment:");
                println!("    OMS_MCP_TOOL_PROFILE=mcp-app");
                println!("\nTo remove this server, run: claude mcp remove \"oms-panel\" -s user");
                std::process::exit(0);
            }
            eprintln!("No MCP server found with name: {name}");
            std::process::exit(1);
        }
        ["mcp", "add-json", "--scope", "local", name, raw_json] => {
            assert_ne!(*name, "oh-my-setting", "must never touch lifecycle key");
            if control["failAdd"] == true {
                eprintln!(
                    "simulated vendor failure\n{{\"mcpServers\":{{\"leak\":\"forbidden\"}}}}\nCUSTOM_ENV_EXAMPLE=do-not-echo"
                );
                std::process::exit(1);
            }
            let parsed: Value = serde_json::from_str(raw_json).unwrap();
            entries
                .entry(cwd_key)
                .or_default()
                .insert((*name).to_owned(), parsed);
            write_private_file(&entries_path, &serde_json::to_vec(&entries).unwrap());
            if let Some(conflict) = control["touchOnAdd"].as_str() {
                write_private_file(Path::new(conflict), b"{}\n");
            }
            if let Some(marker) = control["wroteMarker"].as_str() {
                write_private_file(Path::new(marker), b"1\n");
            }
            if control["signalAfterWriteAdd"] == true {
                let _ = rustix::process::kill_process(
                    rustix::process::getpid(),
                    rustix::process::Signal::TERM,
                );
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            if control["writeThenHangAdd"] == true {
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
            std::process::exit(0);
        }
        ["mcp", "remove", "--scope", "local", name] => {
            if let Some(code) = control["removeExitCode"].as_i64() {
                if let Some(err_text) = control["removeStderr"].as_str() {
                    eprintln!("{err_text}");
                }
                std::process::exit(code as i32);
            }
            if control["hangRemove"] == true {
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
            if control["failRemove"] == true {
                eprintln!("simulated remove failure");
                std::process::exit(1);
            }
            let existed = entries
                .get_mut(&cwd_key)
                .and_then(|m| m.remove(*name))
                .is_some();
            write_private_file(&entries_path, &serde_json::to_vec(&entries).unwrap());
            if existed {
                std::process::exit(0);
            }
            eprintln!("No local MCP server found with name: {name}");
            std::process::exit(1);
        }
        _ => {
            eprintln!("unexpected claude stub invocation: {argv:?}");
            std::process::exit(2);
        }
    }
}

fn project_panels_claude_lifecycle_parity() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = TempDir::new("oms-panel-claude-install");
    let base = root.path();
    let xdg = base.join("temp-xdg");
    let home_dir = base.join("temp-home");
    let claude_cfg_dir = base.join("temp-claude-cfg");
    let codex_home = base.join("temp-codex-home");
    let locks = base.join("locks");
    let stub_bin_dir = base.join("stub-bin");
    let stub_state = base.join("stub-state");
    for d in [
        &xdg,
        &home_dir,
        &claude_cfg_dir,
        &codex_home,
        &locks,
        &stub_bin_dir,
        &stub_state,
    ] {
        ensure_private_dir(d);
    }

    // Sentinel vendor config files inside temp dirs: oms panel itself must never edit them directly;
    // only our stub binary runs.
    ensure_private_dir(&home_dir.join(".claude"));
    ensure_private_dir(&home_dir.join(".codex"));
    ensure_private_dir(&home_dir.join(".gemini/antigravity"));
    ensure_private_dir(&xdg.join("antigravity"));
    let sentinel_files = [
        (
            home_dir.join(".claude.json"),
            br#"{"mcpServers":{}}"#.as_slice(),
        ),
        (
            home_dir.join(".claude/settings.json"),
            br#"{"permissions":{}}"#.as_slice(),
        ),
        (
            claude_cfg_dir.join("settings.json"),
            br#"{"model":"sonnet"}"#.as_slice(),
        ),
        (
            codex_home.join("config.toml"),
            b"[mcp_servers]\n".as_slice(),
        ),
        (
            home_dir.join(".codex/config.toml"),
            b"[mcp_servers]\n".as_slice(),
        ),
        (
            home_dir.join(".gemini/antigravity/mcp_config.json"),
            br#"{"mcpServers":{}}"#.as_slice(),
        ),
        (
            xdg.join("antigravity/mcp_config.json"),
            br#"{"mcpServers":{}}"#.as_slice(),
        ),
    ];
    let mut sentinel_inodes = Vec::new();
    for (path, bytes) in &sentinel_files {
        write_private_file(path, bytes);
        sentinel_inodes.push(std::fs::symlink_metadata(path).unwrap().ino());
    }

    let exe = std::env::current_exe().unwrap();
    let path_claude = stub_bin_dir.join("claude");
    let explicit_claude = base.join("custom-claude-bin");
    std::os::unix::fs::symlink(&exe, &path_claude).unwrap();
    std::os::unix::fs::symlink(&exe, &explicit_claude).unwrap();

    let calls_log = stub_state.join("calls.jsonl");
    let entries_file = stub_state.join("entries.json");
    let control_file = stub_state.join("control.json");
    let set_control = |val: Value| {
        write_private_file(&control_file, &serde_json::to_vec(&val).unwrap());
    };
    let take_calls = || -> Vec<Value> {
        let Ok(raw) = std::fs::read_to_string(&calls_log) else {
            return Vec::new();
        };
        let _ = std::fs::remove_file(&calls_log);
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    set_control(json!({"userEntry": true}));

    // PATH points ONLY to stub_bin_dir so no host binary can ever execute.
    let run_panel = |cwd: &Path, args: &[&str], use_explicit_bin: bool, extra: &[(&str, &str)]| {
        let mut cmd = Command::new(&exe);
        cmd.env_clear()
            .env("OMS_PANEL_PARITY_CLI", "1")
            .env("OMS_PANEL_PARITY_CLAUDE_STUB", &stub_state)
            .env("PATH", &stub_bin_dir)
            .env("HOME", &home_dir)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("CLAUDE_CONFIG_DIR", &claude_cfg_dir)
            .env("CODEX_HOME", &codex_home)
            .env("TMPDIR", base)
            .env("OMS_LOCK_DIR", &locks)
            .current_dir(cwd)
            .args(args);
        if use_explicit_bin {
            cmd.env("OMS_CLAUDE_BIN", &explicit_claude);
        }
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let alpha = base.join("checkouts/alpha-service");
    std::fs::create_dir_all(&alpha).unwrap();
    init_git_repo(&alpha);
    std::fs::write(alpha.join("README.md"), "alpha\n").unwrap();
    let alpha_canon = alpha.canonicalize().unwrap();
    let alpha_tree_before = snapshot_tree(&alpha_canon);

    let beta = base.join("checkouts/beta-service");
    std::fs::create_dir_all(beta.join(".oms")).unwrap();
    let beta_canon = beta.canonicalize().unwrap();

    let default_map = xdg.join("oh-my-setting/desktop-projects.json");
    let panels_sidecar = xdg.join("oh-my-setting/desktop-project-panels.json");

    // 1. Unknown alias refused before map exists, and unsupported app refused; stub never runs.
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "missing"],
        true,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("--register-project"), "{stderr}");
    assert!(take_calls().is_empty());

    for bad_app in ["agy", "cursor"] {
        let (code, _, stderr) = run_panel(
            base,
            &["--install-panel", bad_app, "--project", "alpha-service"],
            true,
            &[],
        );
        assert_ne!(code, 0);
        assert!(
            stderr.contains("supported: claude, codex, antigravity"),
            "{stderr}"
        );
        assert!(take_calls().is_empty());
    }

    // 2. --register-project and --unregister-project alone never run the stub.
    let (code, _, stderr) = run_panel(&alpha, &["--register-project"], true, &[]);
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = run_panel(&beta, &["--register-project"], true, &[]);
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = run_panel(base, &["--unregister-project", "beta-service"], true, &[]);
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = run_panel(&beta, &["--register-project"], true, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        take_calls().is_empty(),
        "register/unregister must never invoke vendor CLI"
    );
    assert!(!panels_sidecar.exists());

    // Unknown alias when map exists is also refused on both install and uninstall.
    for mode in ["--install-panel", "--uninstall-panel"] {
        let (code, _, stderr) =
            run_panel(base, &[mode, "claude", "--project", "unknown"], true, &[]);
        assert_ne!(code, 0);
        assert!(stderr.contains("--register-project"), "{stderr}");
        assert!(take_calls().is_empty());
    }

    let expected_launcher = home_dir.join(".local/bin/oms");
    let expected_entry_json = serde_json::to_string(&json!({
        "type": "stdio",
        "command": expected_launcher,
        "args": ["mcp-server"],
        "env": {
            "OMS_MCP_TOOL_PROFILE": "chatgpt-panel",
            "OMS_PANEL_REPO": alpha_canon,
        }
    }))
    .unwrap();

    // 3. --dry-run (plain and --json) prints exact vendor command(s), runs nothing and writes nothing.
    let xdg_before_dry = snapshot_tree(&xdg);
    let locks_before_dry = snapshot_tree(&locks);
    let (code, dry_out, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "claude",
            "--project",
            "alpha-service",
            "--dry-run",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        dry_out.contains(&format!(
            "claude mcp add-json --scope local oms-panel '{expected_entry_json}'"
        )),
        "unexpected dry-run output: {dry_out}"
    );
    assert!(!dry_out.contains("oh-my-setting\n") && !dry_out.contains(" local oh-my-setting"));
    assert!(take_calls().is_empty(), "dry-run must not run stub");
    assert!(!panels_sidecar.exists());
    assert_eq!(snapshot_tree(&xdg), xdg_before_dry);
    assert_eq!(snapshot_tree(&locks), locks_before_dry);

    let (code, dry_json_out, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "claude",
            "--project",
            "alpha-service",
            "--dry-run",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let dry_json: Value = serde_json::from_str(&dry_json_out).unwrap();
    assert_eq!(
        dry_json,
        json!({
            "app": "claude",
            "alias": "alpha-service",
            "path": alpha_canon,
            "entry": "oms-panel",
            "action": "install",
            "changed": false,
        })
    );
    assert!(take_calls().is_empty());
    assert!(!panels_sidecar.exists());

    // 4. Install runs add-json once with expected cwd and exact args/json (shadowing user-scope
    //    oms-panel), writes private 0600 ownership record, and writes nothing in the checkout.
    let (code, install_out, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "claude",
            "--project",
            "alpha-service",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let install_json: Value = serde_json::from_str(&install_out).unwrap();
    assert_eq!(
        install_json,
        json!({
            "app": "claude",
            "alias": "alpha-service",
            "path": alpha_canon,
            "entry": "oms-panel",
            "action": "install",
            "changed": true,
        })
    );
    let calls = take_calls();
    assert_eq!(
        calls,
        vec![
            json!({"cwd": alpha_canon, "argv": ["mcp", "get", "oms-panel"]}),
            json!({
                "cwd": alpha_canon,
                "argv": ["mcp", "add-json", "--scope", "local", "oms-panel", expected_entry_json]
            }),
        ]
    );
    let sidecar_meta = std::fs::symlink_metadata(&panels_sidecar).unwrap();
    assert!(sidecar_meta.is_file());
    assert_eq!(sidecar_meta.nlink(), 1);
    assert_eq!(sidecar_meta.mode() & 0o7777, 0o600);
    let sidecar_val: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(
        sidecar_val,
        json!({
            "alpha-service": [{
                "app": "claude",
                "scope": "local",
                "name": "oms-panel",
                "path": alpha_canon,
            }]
        })
    );
    assert_eq!(
        snapshot_tree(&alpha_canon),
        alpha_tree_before,
        "install must write nothing inside the checkout"
    );

    // 5. Reinstalling an entry OMS recorded with the same content is a no-op (changed:false).
    let sidecar_ino = sidecar_meta.ino();
    let sidecar_bytes = std::fs::read(&panels_sidecar).unwrap();
    let (code, reinstall_out, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "claude",
            "--project",
            "alpha-service",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let reinstall_json: Value = serde_json::from_str(&reinstall_out).unwrap();
    assert_eq!(reinstall_json["changed"], false);
    assert_eq!(reinstall_json["action"], "install");
    let reinstall_calls = take_calls();
    assert_eq!(
        reinstall_calls,
        vec![json!({"cwd": alpha_canon, "argv": ["mcp", "get", "oms-panel"]})],
        "reinstall must not call add-json again"
    );
    assert_eq!(
        std::fs::symlink_metadata(&panels_sidecar).unwrap().ino(),
        sidecar_ino
    );
    assert_eq!(std::fs::read(&panels_sidecar).unwrap(), sidecar_bytes);

    // 6. --unregister-project with an installed panel is refused naming --uninstall-panel.
    let (code, _, stderr) = run_panel(base, &["--unregister-project", "alpha-service"], true, &[]);
    assert_ne!(code, 0);
    assert!(stderr.contains("--uninstall-panel"), "{stderr}");
    assert!(take_calls().is_empty());
    assert!(default_map.exists() && panels_sidecar.exists());

    // 7. Unmanaged existing local entry refused (also testing claude resolved on PATH).
    let mut pre_entries: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, Value>,
    > = serde_json::from_slice(&std::fs::read(&entries_file).unwrap()).unwrap();
    pre_entries.insert(
        beta_canon.to_string_lossy().into_owned(),
        std::collections::BTreeMap::from([(
            "oms-panel".into(),
            json!({"type": "stdio", "command": "custom-mcp", "args": [], "env": {}}),
        )]),
    );
    write_private_file(&entries_file, &serde_json::to_vec(&pre_entries).unwrap());
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        false,
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        stderr.contains("an unmanaged local oms-panel entry exists; remove it first"),
        "{stderr}"
    );
    assert_eq!(
        take_calls(),
        vec![json!({"cwd": beta_canon, "argv": ["mcp", "get", "oms-panel"]})]
    );
    assert_eq!(std::fs::read(&panels_sidecar).unwrap(), sidecar_bytes);
    pre_entries.remove(&beta_canon.to_string_lossy().into_owned());
    write_private_file(&entries_file, &serde_json::to_vec(&pre_entries).unwrap());

    // 8. Vendor failure leaves no record and never leaks config or env contents.
    set_control(json!({"userEntry": true, "failAdd": true}));
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        false,
        &[("CUSTOM_ENV_EXAMPLE", "do-not-echo")],
    );
    assert_ne!(code, 0);
    assert!(
        !stderr.contains("mcpServers") && !stderr.contains("forbidden"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("CUSTOM_ENV_EXAMPLE") && !stderr.contains("do-not-echo"),
        "{stderr}"
    );
    let _ = take_calls();
    assert_eq!(std::fs::read(&panels_sidecar).unwrap(), sidecar_bytes);
    set_control(json!({"userEntry": true}));

    // 9. Uninstall dry-run runs nothing; real uninstall runs remove in recorded path and drops row;
    //    uninstalling an unrecorded alias runs nothing.
    let (code, un_dry, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "claude",
            "--project",
            "alpha-service",
            "--dry-run",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        un_dry.contains("claude mcp remove --scope local oms-panel"),
        "{un_dry}"
    );
    assert!(take_calls().is_empty());
    assert!(panels_sidecar.exists());

    let (code, un_out, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "claude",
            "--project",
            "alpha-service",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let un_json: Value = serde_json::from_str(&un_out).unwrap();
    assert_eq!(
        un_json,
        json!({
            "app": "claude",
            "alias": "alpha-service",
            "path": alpha_canon,
            "entry": "oms-panel",
            "action": "uninstall",
            "changed": true,
        })
    );
    assert_eq!(
        take_calls(),
        vec![
            json!({"cwd": alpha_canon, "argv": ["mcp", "get", "oms-panel"]}),
            json!({
                "cwd": alpha_canon,
                "argv": ["mcp", "remove", "--scope", "local", "oms-panel"]
            }),
        ]
    );
    assert!(
        !panels_sidecar.exists(),
        "removing last panel row must unlink empty sidecar"
    );

    // Uninstall of an unrecorded alias runs nothing and returns changed:false.
    let (code, un_noop_out, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "claude",
            "--project",
            "alpha-service",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let un_noop_json: Value = serde_json::from_str(&un_noop_out).unwrap();
    assert_eq!(un_noop_json["changed"], false);
    assert_eq!(un_noop_json["action"], "uninstall");
    assert!(
        take_calls().is_empty(),
        "uninstall of unrecorded alias must run nothing"
    );

    // 10. Record-write failure triggers `claude mcp remove --scope local oms-panel` so no
    //     unrecorded entry remains.
    set_control(json!({
        "userEntry": true,
        "touchOnAdd": panels_sidecar,
    }));
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        false,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("rolled back"), "{stderr}");
    let rollback_calls = take_calls();
    assert_eq!(rollback_calls.len(), 3, "{rollback_calls:?}");
    assert_eq!(
        rollback_calls[0]["argv"],
        json!(["mcp", "get", "oms-panel"])
    );
    assert_eq!(rollback_calls[1]["argv"][1], "add-json");
    assert_eq!(
        rollback_calls[2],
        json!({
            "cwd": beta_canon,
            "argv": ["mcp", "remove", "--scope", "local", "oms-panel"]
        })
    );
    let after_rb_entries: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, Value>,
    > = serde_json::from_slice(&std::fs::read(&entries_file).unwrap()).unwrap();
    assert!(
        after_rb_entries
            .get(&beta_canon.to_string_lossy().into_owned())
            .is_none_or(|m| m.is_empty()),
        "rollback must remove unrecorded local entry"
    );
    std::fs::remove_file(&panels_sidecar).unwrap();
    set_control(json!({"userEntry": true}));

    // 11. If local entry is already gone (either at probe time or concurrently before remove),
    //     uninstall still drops row and says so.
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let _ = take_calls();
    write_private_file(&entries_file, b"{}");
    let (code, gone_out, stderr) = run_panel(
        base,
        &["--uninstall-panel", "claude", "--project", "beta-service"],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(gone_out.contains("already gone"), "{gone_out}");
    assert!(!panels_sidecar.exists());
    assert_eq!(
        take_calls(),
        vec![json!({
            "cwd": beta_canon,
            "argv": ["mcp", "get", "oms-panel"]
        })]
    );

    // Concurrent disappearance between `mcp get` and `mcp remove` also succeeds as already gone.
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let _ = take_calls();
    set_control(json!({"userEntry": false, "dropLocalAfterGet": true}));
    let (code, race_gone_out, stderr) = run_panel(
        base,
        &["--uninstall-panel", "claude", "--project", "beta-service"],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(race_gone_out.contains("already gone"), "{race_gone_out}");
    assert!(!panels_sidecar.exists());
    assert_eq!(
        take_calls(),
        vec![
            json!({"cwd": beta_canon, "argv": ["mcp", "get", "oms-panel"]}),
            json!({"cwd": beta_canon, "argv": ["mcp", "remove", "--scope", "local", "oms-panel"]}),
        ]
    );
    set_control(json!({"userEntry": true}));

    // 12. Deleted checkout (and non-directory replacement): uninstall forgets the row and runs
    //     nothing (text + JSON action:"forgotten"), --projects keeps reporting the missing path,
    //     then --unregister-project succeeds.
    let gamma = base.join("checkouts/gamma-service");
    std::fs::create_dir_all(gamma.join(".oms")).unwrap();
    let gamma_canon = gamma.canonicalize().unwrap();
    let (code, _, stderr) = run_panel(&gamma, &["--register-project"], true, &[]);
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "gamma-service"],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let _ = take_calls();
    std::fs::remove_dir_all(&gamma).unwrap();

    // While panel is still recorded, --unregister-project refuses.
    let (code, _, stderr) = run_panel(base, &["--unregister-project", "gamma-service"], true, &[]);
    assert_ne!(code, 0);
    assert!(stderr.contains("--uninstall-panel"), "{stderr}");

    // --projects keeps reporting the missing path.
    let (code, proj_txt, stderr) = run_panel(base, &["--projects"], true, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        proj_txt.contains("gamma-service") && proj_txt.contains("(path no longer exists)"),
        "{proj_txt}"
    );

    // Text uninstall forgets the row without running vendor CLI.
    let (code, forget_txt, stderr) = run_panel(
        base,
        &["--uninstall-panel", "claude", "--project", "gamma-service"],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        forget_txt.contains(
            "checkout is gone; dropped the record (the Claude entry stored for that path is inert)"
        ),
        "{forget_txt}"
    );
    assert!(
        take_calls().is_empty(),
        "deleted checkout uninstall must not invoke vendor CLI"
    );
    assert!(!panels_sidecar.exists());

    // --projects still reports the missing path until unregistered, and --unregister-project succeeds.
    let (code, proj_json_raw, stderr) = run_panel(base, &["--projects", "--json"], true, &[]);
    assert_eq!(code, 0, "{stderr}");
    let proj_json: Value = serde_json::from_str(&proj_json_raw).unwrap();
    let gamma_row = proj_json["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["alias"] == "gamma-service")
        .unwrap();
    assert_eq!(gamma_row["exists"], false);
    assert_eq!(gamma_row["path"], json!(gamma_canon));

    let (code, _, stderr) = run_panel(base, &["--unregister-project", "gamma-service"], true, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(take_calls().is_empty());

    // Also verify JSON action:"forgotten" when recorded checkout is replaced by a regular file.
    let delta = base.join("checkouts/delta-service");
    std::fs::create_dir_all(delta.join(".oms")).unwrap();
    let delta_canon = delta.canonicalize().unwrap();
    assert_eq!(run_panel(&delta, &["--register-project"], true, &[]).0, 0);
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "claude", "--project", "delta-service"],
            true,
            &[],
        )
        .0,
        0
    );
    let _ = take_calls();
    std::fs::remove_dir_all(&delta).unwrap();
    std::fs::write(&delta, b"not a directory\n").unwrap();
    let (code, forget_json_raw, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "claude",
            "--project",
            "delta-service",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let forget_json: Value = serde_json::from_str(&forget_json_raw).unwrap();
    assert_eq!(
        forget_json,
        json!({
            "app": "claude",
            "alias": "delta-service",
            "path": delta_canon,
            "entry": "oms-panel",
            "action": "forgotten",
            "changed": true,
        })
    );
    assert!(take_calls().is_empty());
    std::fs::remove_file(&delta).unwrap();
    assert_eq!(
        run_panel(base, &["--unregister-project", "delta-service"], true, &[]).0,
        0
    );

    // 13. add-json timeout (and signal/interrupt) with stub that writes then hangs -> rollback runs once.
    write_private_file(&entries_file, b"{}");
    set_control(json!({"userEntry": true, "writeThenHangAdd": true}));
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        true,
        &[("OMS_CLAUDE_TIMEOUT_MS", "200")],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("rolled back"), "{stderr}");
    assert!(!stderr.contains("uncertain"), "{stderr}");
    let to_calls = take_calls();
    assert_eq!(to_calls.len(), 3, "{to_calls:?}");
    assert_eq!(to_calls[0]["argv"], json!(["mcp", "get", "oms-panel"]));
    assert_eq!(to_calls[1]["argv"][1], "add-json");
    assert_eq!(
        to_calls[2],
        json!({
            "cwd": beta_canon,
            "argv": ["mcp", "remove", "--scope", "local", "oms-panel"]
        })
    );
    let post_to_entries: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, Value>,
    > = serde_json::from_slice(&std::fs::read(&entries_file).unwrap()).unwrap();
    assert!(
        post_to_entries
            .get(&beta_canon.to_string_lossy().into_owned())
            .is_none_or(|m| m.is_empty()),
        "timeout rollback must remove entry written before hang"
    );
    assert!(!panels_sidecar.exists());

    // Signal-interrupted add-json after write also rolls back once.
    set_control(json!({"userEntry": true, "signalAfterWriteAdd": true}));
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        true,
        &[("OMS_CLAUDE_TIMEOUT_MS", "200")],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("rolled back"), "{stderr}");
    let sig_calls = take_calls();
    assert_eq!(sig_calls.len(), 3, "{sig_calls:?}");
    assert_eq!(
        sig_calls[2]["argv"],
        json!(["mcp", "remove", "--scope", "local", "oms-panel"])
    );
    assert!(!panels_sidecar.exists());

    // Cancellation (stop flag) during add-json after write also rolls back once (exit 130).
    let wrote_marker = stub_state.join("add-wrote.marker");
    let _ = std::fs::remove_file(&wrote_marker);
    set_control(json!({
        "userEntry": true,
        "writeThenHangAdd": true,
        "wroteMarker": wrote_marker,
    }));
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        true,
        &[
            ("OMS_CLAUDE_TIMEOUT_MS", "5000"),
            (
                "OMS_PANEL_PARITY_INTERRUPT_ON_FILE",
                wrote_marker.to_str().unwrap(),
            ),
        ],
    );
    let _ = std::fs::remove_file(&wrote_marker);
    assert_eq!(code, 130, "{stderr}");
    assert!(stderr.contains("rolled back"), "{stderr}");
    let intr_calls = take_calls();
    assert_eq!(intr_calls.len(), 3, "{intr_calls:?}");
    assert_eq!(
        intr_calls[2]["argv"],
        json!(["mcp", "remove", "--scope", "local", "oms-panel"])
    );
    assert!(!panels_sidecar.exists());

    // 14. Rollback failure -> uncertain: true in JSON, clear line in text, exit nonzero.
    set_control(json!({
        "userEntry": true,
        "writeThenHangAdd": true,
        "failRemove": true,
    }));
    let (code, unc_json_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "claude",
            "--project",
            "beta-service",
            "--json",
        ],
        true,
        &[("OMS_CLAUDE_TIMEOUT_MS", "200")],
    );
    assert_ne!(code, 0);
    let unc_json: Value = serde_json::from_str(&unc_json_raw).unwrap();
    assert_eq!(unc_json["uncertain"], true, "{unc_json}");
    assert_eq!(unc_json["changed"], false, "{unc_json}");
    assert_eq!(unc_json["action"], "install", "{unc_json}");
    assert!(stderr.contains("uncertain"), "{stderr}");
    let unc_calls = take_calls();
    assert_eq!(unc_calls.len(), 3, "{unc_calls:?}");

    // Reset unrecorded entry left by failed rollback, then check text mode uncertain line.
    write_private_file(&entries_file, b"{}");
    let (code, unc_txt_out, unc_txt_err) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        true,
        &[("OMS_CLAUDE_TIMEOUT_MS", "200")],
    );
    assert_ne!(code, 0);
    assert!(unc_txt_out.is_empty(), "{unc_txt_out}");
    assert!(unc_txt_err.contains("uncertain"), "{unc_txt_err}");
    assert_eq!(unc_txt_err.lines().count(), 1, "{unc_txt_err}");
    let _ = take_calls();
    set_control(json!({"userEntry": true}));

    // 15. Exact-match unrecorded local entry (left behind after failed rollback!) is adopted;
    //     local entries differing in any field are refused as unmanaged.
    let beta_expected_entry = json!({
        "type": "stdio",
        "command": expected_launcher,
        "args": ["mcp-server"],
        "env": {
            "OMS_MCP_TOOL_PROFILE": "chatgpt-panel",
            "OMS_PANEL_REPO": beta_canon,
        }
    });
    for differing in [
        json!({
            "type": "stdio",
            "command": expected_launcher,
            "args": ["mcp-server", "--extra"],
            "env": {"OMS_MCP_TOOL_PROFILE": "chatgpt-panel", "OMS_PANEL_REPO": beta_canon}
        }),
        json!({
            "type": "stdio",
            "command": expected_launcher,
            "args": ["mcp-server"],
            "env": {"OMS_MCP_TOOL_PROFILE": "mcp-app", "OMS_PANEL_REPO": beta_canon}
        }),
        json!({
            "type": "stdio",
            "command": expected_launcher,
            "args": ["mcp-server"],
            "env": {"OMS_MCP_TOOL_PROFILE": "chatgpt-panel", "OMS_PANEL_REPO": alpha_canon}
        }),
        json!({
            "type": "stdio",
            "command": expected_launcher,
            "args": ["mcp-server"],
            "env": {
                "OMS_MCP_TOOL_PROFILE": "chatgpt-panel",
                "OMS_PANEL_REPO": beta_canon,
                "EXTRA_ENV": "1"
            }
        }),
    ] {
        write_private_file(
            &entries_file,
            &serde_json::to_vec(&json!({
                beta_canon.to_string_lossy().into_owned(): { "oms-panel": differing }
            }))
            .unwrap(),
        );
        let (code, _, stderr) = run_panel(
            base,
            &["--install-panel", "claude", "--project", "beta-service"],
            true,
            &[],
        );
        assert_ne!(code, 0, "differing local entry must be refused");
        assert!(stderr.contains("unmanaged local oms-panel"), "{stderr}");
        assert_eq!(
            take_calls(),
            vec![json!({"cwd": beta_canon, "argv": ["mcp", "get", "oms-panel"]})]
        );
        assert!(!panels_sidecar.exists());
    }

    // Exact match with no recorded row -> adopted (JSON and plain text).
    write_private_file(
        &entries_file,
        &serde_json::to_vec(&json!({
            beta_canon.to_string_lossy().into_owned(): { "oms-panel": beta_expected_entry }
        }))
        .unwrap(),
    );
    let (code, adopt_out, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "claude",
            "--project",
            "beta-service",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let adopt_json: Value = serde_json::from_str(&adopt_out).unwrap();
    assert_eq!(
        adopt_json,
        json!({
            "app": "claude",
            "alias": "beta-service",
            "path": beta_canon,
            "entry": "oms-panel",
            "action": "adopted",
            "changed": true,
        })
    );
    assert_eq!(
        take_calls(),
        vec![json!({"cwd": beta_canon, "argv": ["mcp", "get", "oms-panel"]})],
        "adopting exact-match entry must not call add-json"
    );
    let adopted_sidecar: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(
        adopted_sidecar["beta-service"][0]["path"],
        json!(beta_canon)
    );

    // 16. Exit 127/126/signal/non-matching exit 1 is never "absent", and unparseable exit-0
    //     `mcp get` output fails closed without mutating anything.
    let saved_sidecar = std::fs::read(&panels_sidecar).unwrap();
    for bad_ctl in [
        json!({"getExitCode": 127, "getStderr": "claude: oms-panel: not found"}),
        json!({"getExitCode": 126, "getStderr": "No local MCP server found with name: oms-panel"}),
        json!({"signalOnGet": true}),
        json!({"getExitCode": 1, "getStderr": "No MCP server found with name: other-server"}),
        json!({"getExitCode": 0, "getStdout": "oms-panel: unparseable output without scope"}),
        json!({"getExitCode": 0, "getStdout": "{malformed json"}),
        json!({"getExitCode": 0, "getStdout": "No MCP server found with name: oms-panel"}),
    ] {
        set_control(bad_ctl.clone());
        let (code, _, _) = run_panel(
            base,
            &["--install-panel", "claude", "--project", "alpha-service"],
            true,
            &[],
        );
        assert_ne!(code, 0, "install must fail closed for {bad_ctl}");
        let (code, _, _) = run_panel(
            base,
            &["--uninstall-panel", "claude", "--project", "beta-service"],
            true,
            &[],
        );
        assert_ne!(code, 0, "uninstall must fail closed for {bad_ctl}");
        assert_eq!(std::fs::read(&panels_sidecar).unwrap(), saved_sidecar);
    }
    set_control(json!({
        "userEntry": true,
        "removeExitCode": 127,
        "removeStderr": "claude: oms-panel: not found",
    }));
    let (code, _, _) = run_panel(
        base,
        &["--uninstall-panel", "claude", "--project", "beta-service"],
        true,
        &[],
    );
    assert_ne!(
        code, 0,
        "exit 127 on mcp remove must not be treated as absent"
    );
    assert_eq!(std::fs::read(&panels_sidecar).unwrap(), saved_sidecar);
    let _ = take_calls();
    set_control(json!({"userEntry": true}));

    // 17. Uninstall of a user-changed local entry leaves it in place and drops the ownership row.
    let user_modified = json!({
        "type": "stdio",
        "command": expected_launcher,
        "args": ["mcp-server", "--custom-user-flag"],
        "env": {
            "OMS_MCP_TOOL_PROFILE": "chatgpt-panel",
            "OMS_PANEL_REPO": beta_canon,
        }
    });
    write_private_file(
        &entries_file,
        &serde_json::to_vec(&json!({
            beta_canon.to_string_lossy().into_owned(): { "oms-panel": user_modified }
        }))
        .unwrap(),
    );
    let (code, un_mod_out, stderr) = run_panel(
        base,
        &["--uninstall-panel", "claude", "--project", "beta-service"],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        un_mod_out.contains("the local oms-panel entry was changed outside OMS; left it in place"),
        "{un_mod_out}"
    );
    assert_eq!(
        take_calls(),
        vec![json!({"cwd": beta_canon, "argv": ["mcp", "get", "oms-panel"]})],
        "uninstall of user-modified entry must not call mcp remove"
    );
    assert!(!panels_sidecar.exists());
    let preserved_entries: Value =
        serde_json::from_slice(&std::fs::read(&entries_file).unwrap()).unwrap();
    assert_eq!(
        preserved_entries[beta_canon.to_string_lossy().as_ref()]["oms-panel"],
        user_modified,
        "user-modified entry must remain untouched"
    );
    write_private_file(&entries_file, b"{}");

    // 18. A stub whose grandchild keeps stderr open does not hang past the timeout
    //     (both same-process-group killed immediately and escaped new-group bounded by deadline).
    let gc_pid_file = stub_state.join("grandchild-ready.pid");
    set_control(json!({"userEntry": true, "spawnGrandchildOnGet": "same-group"}));
    let t0 = std::time::Instant::now();
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "beta-service"],
        true,
        &[("OMS_CLAUDE_TIMEOUT_MS", "3000")],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(2000),
        "same-group grandchild keeping stderr open was not killed promptly: {:?}",
        t0.elapsed()
    );
    let _ = take_calls();

    set_control(json!({"userEntry": true, "spawnGrandchildOnGet": "new-group"}));
    let t1 = std::time::Instant::now();
    let (code, _, stderr) = run_panel(
        base,
        &["--uninstall-panel", "claude", "--project", "beta-service"],
        true,
        &[("OMS_CLAUDE_TIMEOUT_MS", "250")],
    );
    let elapsed_detached = t1.elapsed();
    if let Ok(raw_pid) = std::fs::read_to_string(&gc_pid_file)
        && let Ok(pid_num) = raw_pid.trim().parse::<i32>()
        && let Some(pid) = rustix::process::Pid::from_raw(pid_num)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
    let _ = std::fs::remove_file(&gc_pid_file);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        elapsed_detached < std::time::Duration::from_secs(5),
        "detached grandchild keeping stderr open hung past timeout: {elapsed_detached:?}"
    );
    assert!(!panels_sidecar.exists());
    let _ = take_calls();
    set_control(json!({"userEntry": true}));

    // 19. Sidecar privacy (0644, symlink) and strict parsing on desktop-project-panels.json.
    write_private_file(
        &panels_sidecar,
        serde_json::to_string(&json!({
            "alpha-service": [{"app":"claude","scope":"local","name":"oms-panel","path":alpha_canon}]
        }))
        .unwrap()
        .as_bytes(),
    );
    std::fs::set_permissions(&panels_sidecar, std::fs::Permissions::from_mode(0o644)).unwrap();
    for args in [
        &["--install-panel", "claude", "--project", "alpha-service"][..],
        &["--uninstall-panel", "claude", "--project", "alpha-service"],
        &["--unregister-project", "alpha-service"],
    ] {
        let (code, _, stderr) = run_panel(base, args, true, &[]);
        assert_ne!(code, 0, "0644 panel sidecar must fail for {args:?}");
        assert!(stderr.contains("panel map"), "{stderr}");
    }
    std::fs::remove_file(&panels_sidecar).unwrap();

    let outside_panels = base.join("outside-panels.json");
    write_private_file(&outside_panels, b"{}");
    std::os::unix::fs::symlink(&outside_panels, &panels_sidecar).unwrap();
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "claude", "--project", "alpha-service"],
        true,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("symlink"), "{stderr}");
    std::fs::remove_file(&panels_sidecar).unwrap();
    let _ = take_calls();

    // 20. Claude panels still accept aliases containing `.` and work with OMS_PANEL_PROJECTS_FILE.
    let dotted = base.join("checkouts/dotted-service");
    std::fs::create_dir_all(dotted.join(".oms")).unwrap();
    assert_eq!(
        run_panel(
            &dotted,
            &["--register-project", "--alias", "app.v1"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "claude", "--project", "app.v1"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "claude", "--project", "app.v1"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(base, &["--unregister-project", "app.v1"], true, &[]).0,
        0
    );

    let alt_map = base.join("alt-map/custom-projects.json");
    let alt_map_str = alt_map.to_str().unwrap();
    assert_eq!(
        run_panel(
            &alpha,
            &["--register-project"],
            true,
            &[("OMS_PANEL_PROJECTS_FILE", alt_map_str)],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "claude", "--project", "alpha-service"],
            true,
            &[("OMS_PANEL_PROJECTS_FILE", alt_map_str)],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "claude", "--project", "alpha-service"],
            true,
            &[("OMS_PANEL_PROJECTS_FILE", alt_map_str)],
        )
        .0,
        0
    );
    let _ = take_calls();
    write_private_file(&entries_file, b"{}");

    // 21. Checkout tree and all Claude/Codex/Antigravity config sentinels remain untouched.
    assert_eq!(snapshot_tree(&alpha_canon), alpha_tree_before);
    for ((path, expected_bytes), expected_ino) in sentinel_files.iter().zip(sentinel_inodes.iter())
    {
        let meta = std::fs::symlink_metadata(path).unwrap();
        assert_eq!(
            meta.ino(),
            *expected_ino,
            "sentinel replaced: {}",
            path.display()
        );
        assert_eq!(
            &std::fs::read(path).unwrap(),
            expected_bytes,
            "sentinel modified: {}",
            path.display()
        );
    }
}

fn project_panels_codex_lifecycle_parity() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = TempDir::new("oms-panel-codex-install");
    let base = root.path();
    let xdg = base.join("temp-xdg");
    let home_dir = base.join("temp-home");
    let codex_home = base.join("temp-codex-home");
    let locks = base.join("locks");
    let git_bin_dir = base.join("git-only-bin");
    for d in [&xdg, &home_dir, &codex_home, &locks, &git_bin_dir] {
        ensure_private_dir(d);
    }

    // Expose only `git` on PATH so no host `codex` or `claude` binary can ever run.
    let host_git = std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("git"))
                .find(|p| p.is_file())
        })
        .expect("git must exist on test runner PATH");
    std::os::unix::fs::symlink(&host_git, git_bin_dir.join("git")).unwrap();

    ensure_private_dir(&home_dir.join(".codex"));
    let home_claude_json = home_dir.join(".claude.json");
    let home_codex_cfg = home_dir.join(".codex/config.toml");
    let codex_home_cfg = codex_home.join("config.toml");
    write_private_file(&home_claude_json, br#"{"mcpServers":{}}"#);
    write_private_file(&home_codex_cfg, b"model = \"gpt-5\"\n");
    write_private_file(&codex_home_cfg, b"model = \"gpt-5\"\n");
    let home_claude_ino = std::fs::symlink_metadata(&home_claude_json).unwrap().ino();
    let home_codex_ino = std::fs::symlink_metadata(&home_codex_cfg).unwrap().ino();
    let mut codex_home_ino = std::fs::symlink_metadata(&codex_home_cfg).unwrap().ino();
    let mut expected_codex_home_bytes = std::fs::read(&codex_home_cfg).unwrap();

    let exe = std::env::current_exe().unwrap();
    let run_panel = |cwd: &Path, args: &[&str], with_codex_home: bool, extra: &[(&str, &str)]| {
        let mut cmd = Command::new(&exe);
        cmd.env_clear()
            .env("OMS_PANEL_PARITY_CLI", "1")
            .env("PATH", &git_bin_dir)
            .env("HOME", &home_dir)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("TMPDIR", base)
            .env("OMS_LOCK_DIR", &locks)
            .current_dir(cwd)
            .args(args);
        if with_codex_home {
            cmd.env("CODEX_HOME", &codex_home);
        }
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let panels_sidecar = xdg.join("oh-my-setting/desktop-project-panels.json");
    let expected_launcher = home_dir.join(".local/bin/oms");
    let expected_version = include_str!("../../../VERSION").trim();
    let repo_oms_mcp: Value =
        serde_json::from_str(include_str!("../../../plugins/oms/mcp.json")).unwrap();
    let expected_res_ver =
        repo_oms_mcp["mcpServers"]["oms-panel"]["env"]["OMS_MCP_PANEL_RESOURCE_VERSION"]
            .as_str()
            .unwrap();

    let render_expected_block = |repo_canon: &Path, pkg_ver: &str, res_ver: &str| -> String {
        format!(
            "# BEGIN oh-my-setting codex-panel\n# oh-my-setting {pkg_ver}\n[mcp_servers.oms-panel]\ncommand = \"{}\"\nargs = [\"mcp-server\"]\n\n[mcp_servers.oms-panel.env]\nOMS_MCP_TOOL_PROFILE = \"chatgpt-panel\"\nOMS_PANEL_REPO = \"{}\"\nOMS_MCP_PANEL_RESOURCE_VERSION = \"{res_ver}\"\n# END oh-my-setting codex-panel\n",
            expected_launcher.display(),
            repo_canon.display(),
        )
    };

    // 1. Uppercase alias (`PoseFixer`), dry-run, fresh install into a missing `.codex/config.toml`,
    //    exclude line added, untrusted vs trusted reporting, and idempotent rerun.
    let pose = base.join("checkouts/PoseFixer");
    std::fs::create_dir_all(&pose).unwrap();
    init_git_repo(&pose);
    let pose_canon = pose.canonicalize().unwrap();
    let pose_exclude = pose_canon.join(".git/info/exclude");
    let pose_exclude_before = std::fs::read(&pose_exclude).unwrap();
    let pose_codex_dir = pose_canon.join(".codex");
    let pose_codex_cfg = pose_codex_dir.join("config.toml");

    assert_eq!(
        run_panel(
            &pose,
            &["--register-project", "--alias", "PoseFixer"],
            true,
            &[]
        )
        .0,
        0
    );

    // Dry-run (plain and --json) writes nothing and creates no locks.
    let xdg_before_dry = snapshot_tree(&xdg);
    let locks_before_dry = snapshot_tree(&locks);
    let (code, dry_txt, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--dry-run",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        dry_txt.contains("would install codex panel oms-panel for PoseFixer")
            && dry_txt.contains("codex-project-untrusted")
            && dry_txt.contains("hint: Trust"),
        "{dry_txt}"
    );
    assert!(!pose_codex_dir.exists() && !panels_sidecar.exists());
    assert_eq!(snapshot_tree(&xdg), xdg_before_dry);
    assert_eq!(snapshot_tree(&locks), locks_before_dry);

    let (code, dry_json_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--dry-run",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let dry_json: Value = serde_json::from_str(&dry_json_raw).unwrap();
    assert_eq!(dry_json["app"], "codex");
    assert_eq!(dry_json["alias"], "PoseFixer");
    assert_eq!(dry_json["path"], json!(pose_canon));
    assert_eq!(dry_json["entry"], "oms-panel");
    assert_eq!(dry_json["action"], "install");
    assert_eq!(dry_json["changed"], false);
    assert_eq!(dry_json["active"], false);
    assert_eq!(dry_json["reason"], "codex-project-untrusted");
    assert!(
        dry_json["hint"]
            .as_str()
            .is_some_and(|h| h.contains("Codex") && !h.contains('\n')),
        "{dry_json}"
    );
    assert!(!pose_codex_dir.exists() && !panels_sidecar.exists());

    // Fresh install into missing `.codex/config.toml` while untrusted.
    let (code, install_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let install_json: Value = serde_json::from_str(&install_raw).unwrap();
    assert_eq!(install_json["app"], "codex");
    assert_eq!(install_json["alias"], "PoseFixer");
    assert_eq!(install_json["path"], json!(pose_canon));
    assert_eq!(install_json["entry"], "oms-panel");
    assert_eq!(install_json["action"], "install");
    assert_eq!(install_json["changed"], true);
    assert_eq!(install_json["active"], false);
    assert_eq!(install_json["reason"], "codex-project-untrusted");
    assert!(
        install_json["hint"]
            .as_str()
            .is_some_and(|h| h.contains(&pose_canon.display().to_string()) && !h.contains('\n')),
        "{install_json}"
    );

    let dir_meta = std::fs::symlink_metadata(&pose_codex_dir).unwrap();
    assert!(dir_meta.is_dir() && !dir_meta.file_type().is_symlink());
    assert_eq!(dir_meta.mode() & 0o7777, 0o700);
    let cfg_meta = std::fs::symlink_metadata(&pose_codex_cfg).unwrap();
    assert!(cfg_meta.is_file() && !cfg_meta.file_type().is_symlink());
    assert_eq!(cfg_meta.nlink(), 1);
    assert_eq!(cfg_meta.mode() & 0o7777, 0o600);

    let expected_pose_block =
        render_expected_block(&pose_canon, expected_version, expected_res_ver);
    let written_pose_cfg = std::fs::read_to_string(&pose_codex_cfg).unwrap();
    assert_eq!(written_pose_cfg, expected_pose_block);
    let expected_pose_sha = oms_panel_core::digest(expected_pose_block.as_bytes());

    let sidecar_meta = std::fs::symlink_metadata(&panels_sidecar).unwrap();
    assert_eq!(sidecar_meta.mode() & 0o7777, 0o600);
    let sidecar_val: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(
        sidecar_val,
        json!({
            "PoseFixer": [{
                "app": "codex",
                "scope": "project",
                "name": "oms-panel",
                "path": pose_canon,
                "block_sha256": expected_pose_sha,
                "created_config": true,
                "created_codex_dir": true,
                "created_exclude": true,
            }]
        })
    );

    // Exclude line added to `.git/info/exclude` preserving prior bytes, and git status is clean.
    let mut expected_exclude_after = pose_exclude_before.clone();
    expected_exclude_after.extend_from_slice(b"/.codex/config.toml\n");
    assert_eq!(
        std::fs::read(&pose_exclude).unwrap(),
        expected_exclude_after
    );
    assert_eq!(git_ok(&pose_canon, &["status", "--porcelain"]), "");

    // Idempotent rerun while untrusted is a no-op (`changed: false`, inodes untouched).
    let cfg_ino_before = cfg_meta.ino();
    let sidecar_ino_before = sidecar_meta.ino();
    let (code, noop_plain, stderr) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "PoseFixer"],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        noop_plain.contains("already installed codex panel oms-panel for PoseFixer")
            && noop_plain.contains("codex-project-untrusted")
            && noop_plain.contains("\nhint: Trust "),
        "{noop_plain}"
    );
    let (code, noop_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let noop_json: Value = serde_json::from_str(&noop_raw).unwrap();
    assert_eq!(noop_json["changed"], false);
    assert_eq!(noop_json["active"], false);
    assert_eq!(noop_json["reason"], "codex-project-untrusted");
    assert_eq!(
        std::fs::symlink_metadata(&pose_codex_cfg).unwrap().ino(),
        cfg_ino_before
    );
    assert_eq!(
        std::fs::symlink_metadata(&panels_sidecar).unwrap().ino(),
        sidecar_ino_before
    );

    // Untrusted install and rerun never touched `$CODEX_HOME/config.toml`.
    assert_eq!(
        std::fs::symlink_metadata(&codex_home_cfg).unwrap().ino(),
        codex_home_ino
    );
    assert_eq!(
        std::fs::read(&codex_home_cfg).unwrap(),
        expected_codex_home_bytes
    );

    // Mark `PoseFixer` trusted in `$CODEX_HOME/config.toml`: rerun reports `active: true` with no
    // untrusted reason/hint, and never writes `$CODEX_HOME/config.toml`.
    let trusted_codex_cfg = format!(
        "model = \"gpt-5\"\n\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
        pose_canon.display()
    );
    write_private_file(&codex_home_cfg, trusted_codex_cfg.as_bytes());
    codex_home_ino = std::fs::symlink_metadata(&codex_home_cfg).unwrap().ino();
    expected_codex_home_bytes = trusted_codex_cfg.into_bytes();

    let (code, trusted_noop_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let trusted_noop_json: Value = serde_json::from_str(&trusted_noop_raw).unwrap();
    assert_eq!(
        trusted_noop_json,
        json!({
            "app": "codex",
            "alias": "PoseFixer",
            "path": pose_canon,
            "entry": "oms-panel",
            "action": "install",
            "changed": false,
            "active": true,
        })
    );

    // Default `~/.codex/config.toml` fallback when CODEX_HOME is unset: untrusted in `~/.codex`.
    let (code, home_fallback_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let home_fallback_json: Value = serde_json::from_str(&home_fallback_raw).unwrap();
    assert_eq!(home_fallback_json["active"], false);
    assert_eq!(home_fallback_json["reason"], "codex-project-untrusted");

    // `--unregister-project` is refused while the Codex panel is installed.
    let (code, _, stderr) = run_panel(base, &["--unregister-project", "PoseFixer"], true, &[]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("--uninstall-panel codex --project PoseFixer"),
        "{stderr}"
    );

    // Uninstall cleans up OMS-created `.codex/config.toml`, `.codex` dir, exclude line, and sidecar.
    let (code, un_pose_raw, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let un_pose_json: Value = serde_json::from_str(&un_pose_raw).unwrap();
    assert_eq!(
        un_pose_json,
        json!({
            "app": "codex",
            "alias": "PoseFixer",
            "path": pose_canon,
            "entry": "oms-panel",
            "action": "uninstall",
            "changed": true,
        })
    );
    assert!(!pose_codex_cfg.exists());
    assert!(!pose_codex_dir.exists());
    assert!(!panels_sidecar.exists());
    assert_eq!(std::fs::read(&pose_exclude).unwrap(), pose_exclude_before);

    // Fresh install while trusted reports `active: true` (both plain and JSON).
    let (code, trusted_plain, stderr) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "PoseFixer"],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        trusted_plain,
        format!(
            "installed codex panel oms-panel for PoseFixer: {}\n",
            pose_canon.display()
        )
    );
    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "codex", "--project", "PoseFixer"],
            true,
            &[],
        )
        .0,
        0
    );

    // 2. Existing `.codex/config.toml` with other tables, comments, and mode 0640:
    //    every existing byte and permission bit is preserved on install and restored on uninstall.
    let alpha = base.join("checkouts/alpha-service");
    std::fs::create_dir_all(&alpha).unwrap();
    init_git_repo(&alpha);
    let alpha_canon = alpha.canonicalize().unwrap();
    assert_eq!(run_panel(&alpha, &["--register-project"], true, &[]).0, 0);

    let alpha_codex_dir = alpha_canon.join(".codex");
    let alpha_codex_cfg = alpha_codex_dir.join("config.toml");
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&alpha_codex_dir)
        .unwrap();
    std::fs::set_permissions(&alpha_codex_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let existing_user_bytes = b"# Top user comment\r\nmodel = \"gpt-5\"\r\n\r\n[sandbox_workspace_write]\nnetwork_access = false\n\n[mcp_servers.other-tool]\ncommand = \"/usr/local/bin/other\"\nargs = [\"--serve\"]\n\n# Trailing note\n";
    std::fs::write(&alpha_codex_cfg, existing_user_bytes).unwrap();
    std::fs::set_permissions(&alpha_codex_cfg, std::fs::Permissions::from_mode(0o640)).unwrap();
    let alpha_exclude = alpha_canon.join(".git/info/exclude");
    let alpha_exclude_before = std::fs::read(&alpha_exclude).unwrap();

    let (code, alpha_inst_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "alpha-service",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let alpha_inst_json: Value = serde_json::from_str(&alpha_inst_raw).unwrap();
    assert_eq!(alpha_inst_json["changed"], true);
    assert_eq!(alpha_inst_json["active"], false);

    let expected_alpha_block =
        render_expected_block(&alpha_canon, expected_version, expected_res_ver);
    let mut expected_combined = existing_user_bytes.to_vec();
    expected_combined.extend_from_slice(expected_alpha_block.as_bytes());
    assert_eq!(std::fs::read(&alpha_codex_cfg).unwrap(), expected_combined);
    assert_eq!(
        std::fs::symlink_metadata(&alpha_codex_cfg).unwrap().mode() & 0o7777,
        0o640
    );

    let alpha_sidecar: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(alpha_sidecar["alpha-service"][0]["created_config"], false);
    assert_eq!(
        alpha_sidecar["alpha-service"][0]["created_codex_dir"],
        false
    );
    assert_eq!(alpha_sidecar["alpha-service"][0]["created_exclude"], true);

    // Uninstall removes only the OMS block and exclude line, keeping `.codex/config.toml` and
    // `.codex` dir with exact prior user bytes and mode.
    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "codex", "--project", "alpha-service"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        std::fs::read(&alpha_codex_cfg).unwrap(),
        existing_user_bytes
    );
    assert_eq!(
        std::fs::symlink_metadata(&alpha_codex_cfg).unwrap().mode() & 0o7777,
        0o640
    );
    assert!(alpha_codex_dir.exists());
    assert_eq!(std::fs::read(&alpha_exclude).unwrap(), alpha_exclude_before);
    assert!(!panels_sidecar.exists());

    // 2b. Existing `.codex/config.toml` and `.git/info/exclude` WITHOUT trailing newlines:
    //     install records `separator_added` and `exclude_separator_added`, preserves them across
    //     block refresh, and uninstall restores the exact original unterminated bytes.
    let unterminated_user_cfg = b"# Unterminated user config\r\nmodel = \"gpt-5\"\n[mcp_servers.other-tool]\ncommand = \"/usr/local/bin/other\"";
    let unterminated_exclude = b"# Unterminated exclude file\n*.local-tmp";
    assert!(!unterminated_user_cfg.ends_with(b"\n"));
    assert!(!unterminated_exclude.ends_with(b"\n"));
    std::fs::write(&alpha_codex_cfg, unterminated_user_cfg).unwrap();
    std::fs::set_permissions(&alpha_codex_cfg, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::write(&alpha_exclude, unterminated_exclude).unwrap();
    std::fs::set_permissions(&alpha_exclude, std::fs::Permissions::from_mode(0o644)).unwrap();

    let (code, unterm_inst_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "alpha-service",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let unterm_inst_json: Value = serde_json::from_str(&unterm_inst_raw).unwrap();
    assert_eq!(unterm_inst_json["changed"], true);

    let mut expected_unterm_cfg = unterminated_user_cfg.to_vec();
    expected_unterm_cfg.push(b'\n');
    expected_unterm_cfg.extend_from_slice(expected_alpha_block.as_bytes());
    assert_eq!(
        std::fs::read(&alpha_codex_cfg).unwrap(),
        expected_unterm_cfg
    );
    let mut expected_unterm_excl = unterminated_exclude.to_vec();
    expected_unterm_excl.extend_from_slice(b"\n/.codex/config.toml\n");
    assert_eq!(std::fs::read(&alpha_exclude).unwrap(), expected_unterm_excl);

    let unterm_sidecar: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(unterm_sidecar["alpha-service"][0]["separator_added"], true);
    assert_eq!(
        unterm_sidecar["alpha-service"][0]["exclude_separator_added"],
        true
    );

    // Simulate an older OMS block version on disk and verify refresh keeps both separator flags.
    let older_alpha_block = render_expected_block(&alpha_canon, "0.7.0-older", expected_res_ver);
    let older_alpha_sha = oms_panel_core::digest(older_alpha_block.as_bytes());
    let mut older_unterm_cfg = unterminated_user_cfg.to_vec();
    older_unterm_cfg.push(b'\n');
    older_unterm_cfg.extend_from_slice(older_alpha_block.as_bytes());
    std::fs::write(&alpha_codex_cfg, &older_unterm_cfg).unwrap();
    std::fs::set_permissions(&alpha_codex_cfg, std::fs::Permissions::from_mode(0o640)).unwrap();
    write_private_file(
        &panels_sidecar,
        serde_json::to_string_pretty(&json!({
            "alpha-service": [{
                "app": "codex",
                "scope": "project",
                "name": "oms-panel",
                "path": alpha_canon,
                "block_sha256": older_alpha_sha,
                "created_config": false,
                "created_codex_dir": false,
                "created_exclude": true,
                "separator_added": true,
                "exclude_separator_added": true,
            }]
        }))
        .unwrap()
        .as_bytes(),
    );
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "alpha-service"],
            true,
            &[],
        )
        .0,
        0
    );
    let refreshed_unterm_sidecar: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(
        refreshed_unterm_sidecar["alpha-service"][0]["separator_added"],
        true
    );
    assert_eq!(
        refreshed_unterm_sidecar["alpha-service"][0]["exclude_separator_added"],
        true
    );

    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "codex", "--project", "alpha-service"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        std::fs::read(&alpha_codex_cfg).unwrap(),
        unterminated_user_cfg,
        "uninstall must remove separator_added byte to restore exact original config bytes"
    );
    assert_eq!(
        std::fs::symlink_metadata(&alpha_codex_cfg).unwrap().mode() & 0o7777,
        0o640
    );
    assert_eq!(
        std::fs::read(&alpha_exclude).unwrap(),
        unterminated_exclude,
        "uninstall must remove exclude_separator_added byte to restore exact original exclude bytes"
    );
    assert_eq!(
        std::fs::symlink_metadata(&alpha_exclude).unwrap().mode() & 0o7777,
        0o644
    );
    assert!(!panels_sidecar.exists());
    std::fs::write(&alpha_exclude, &alpha_exclude_before).unwrap();

    // 2c. OMS-created `.codex/config.toml` (`created_config: true`) where the user deletes the OMS
    //     block and leaves user text without a trailing newline, then reinstalls (`separator_added:
    //     true` alongside `created_config: true`):
    //     - subsequent panel commands (Codex, Claude, `--unregister-project`) still parse the sidecar;
    //     - uninstall removes the OMS block and separator while keeping exact user text;
    //     - when nothing else remains in `.codex/config.toml`, uninstall deletes the file and
    //       OMS-created `.codex` dir.
    let beta_unterm = base.join("checkouts/beta-unterm");
    std::fs::create_dir_all(&beta_unterm).unwrap();
    init_git_repo(&beta_unterm);
    let beta_unterm_canon = beta_unterm.canonicalize().unwrap();
    let beta_unterm_before_tree = snapshot_tree(&beta_unterm_canon);
    let beta_unterm_exclude = beta_unterm_canon.join(".git/info/exclude");
    let beta_unterm_exclude_before = std::fs::read(&beta_unterm_exclude).unwrap();
    let beta_unterm_codex_dir = beta_unterm_canon.join(".codex");
    let beta_unterm_codex_cfg = beta_unterm_codex_dir.join("config.toml");

    let temp_sibling = base.join("checkouts/temp-sibling");
    std::fs::create_dir_all(temp_sibling.join(".oms")).unwrap();
    assert_eq!(
        run_panel(
            &beta_unterm_canon,
            &["--register-project", "--alias", "beta-unterm"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            &temp_sibling,
            &["--register-project", "--alias", "temp-sibling"],
            true,
            &[],
        )
        .0,
        0
    );

    // Step 1: initial install creates `.codex/` and `.codex/config.toml` (`created_config: true`).
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "beta-unterm"],
            true,
            &[],
        )
        .0,
        0
    );
    let step1_sidecar: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(step1_sidecar["beta-unterm"][0]["created_config"], true);
    assert_eq!(step1_sidecar["beta-unterm"][0]["created_codex_dir"], true);

    // Step 2: user deletes the OMS block and leaves user text without a trailing newline.
    let user_unterm_text = b"model = \"gpt-5\"\n[features]\nfast_mode = true";
    assert!(!user_unterm_text.ends_with(b"\n"));
    std::fs::write(&beta_unterm_codex_cfg, user_unterm_text).unwrap();

    // Step 3: reinstall appends the block with a separator and keeps `created_config: true`.
    let (code, step3_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "beta-unterm",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let step3_json: Value = serde_json::from_str(&step3_raw).unwrap();
    assert_eq!(step3_json["changed"], true);

    let step3_sidecar: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(step3_sidecar["beta-unterm"][0]["created_config"], true);
    assert_eq!(step3_sidecar["beta-unterm"][0]["created_codex_dir"], true);
    assert_eq!(step3_sidecar["beta-unterm"][0]["separator_added"], true);

    let expected_beta_unterm_block =
        render_expected_block(&beta_unterm_canon, expected_version, expected_res_ver);
    let mut expected_step3_cfg = user_unterm_text.to_vec();
    expected_step3_cfg.push(b'\n');
    expected_step3_cfg.extend_from_slice(expected_beta_unterm_block.as_bytes());
    assert_eq!(
        std::fs::read(&beta_unterm_codex_cfg).unwrap(),
        expected_step3_cfg
    );

    // Step 4: subsequent panel commands parse and re-encode the sidecar cleanly (Codex rerun,
    // real Claude install/uninstall on the same and sibling aliases, and `--unregister-project`).
    let (code, noop_after_sep, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "beta-unterm",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&noop_after_sep).unwrap()["changed"],
        false
    );

    let claude_coexist_state = base.join("claude-coexist-stub");
    ensure_private_dir(&claude_coexist_state);
    write_private_file(
        &claude_coexist_state.join("control.json"),
        br#"{"userEntry":true}"#,
    );
    let claude_coexist_bin = claude_coexist_state.join("claude-stub");
    std::os::unix::fs::symlink(&exe, &claude_coexist_bin).unwrap();
    let claude_coexist_env = [
        (
            "OMS_PANEL_PARITY_CLAUDE_STUB",
            claude_coexist_state.to_str().unwrap(),
        ),
        ("OMS_CLAUDE_BIN", claude_coexist_bin.to_str().unwrap()),
    ];

    for alias in ["beta-unterm", "alpha-service"] {
        let (code, inst_raw, stderr) = run_panel(
            base,
            &["--install-panel", "claude", "--project", alias, "--json"],
            true,
            &claude_coexist_env,
        );
        assert_eq!(
            code, 0,
            "claude install ({alias}) must parse and write sidecar: {stderr}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&inst_raw).unwrap()["changed"],
            true
        );
    }
    for alias in ["alpha-service", "beta-unterm"] {
        let (code, uninst_raw, stderr) = run_panel(
            base,
            &["--uninstall-panel", "claude", "--project", alias, "--json"],
            true,
            &claude_coexist_env,
        );
        assert_eq!(
            code, 0,
            "claude uninstall ({alias}) must parse and write sidecar: {stderr}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&uninst_raw).unwrap()["changed"],
            true
        );
    }

    let (code, _, stderr) = run_panel(base, &["--unregister-project", "temp-sibling"], true, &[]);
    assert_eq!(code, 0, "unregister-project must parse sidecar: {stderr}");

    let (code, _, stderr) = run_panel(base, &["--unregister-project", "beta-unterm"], true, &[]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("--uninstall-panel codex --project beta-unterm")
            && !stderr.contains("Invalid OMS project panel map"),
        "{stderr}"
    );

    // Step 5: uninstall leaves exact unterminated user text (without deleting the file or `.codex`).
    let (code, _, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "codex",
            "--project",
            "beta-unterm",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        std::fs::read(&beta_unterm_codex_cfg).unwrap(),
        user_unterm_text,
        "uninstall must remove block and separator while keeping exact user text"
    );
    assert!(beta_unterm_codex_dir.exists());
    assert_eq!(
        std::fs::read(&beta_unterm_exclude).unwrap(),
        beta_unterm_exclude_before
    );
    assert!(!panels_sidecar.exists());

    // Step 6: when nothing else remains in `.codex/config.toml` for a `created_config: true,
    // separator_added: true` row, uninstall deletes both the file and the empty OMS-created `.codex`.
    std::fs::remove_file(&beta_unterm_codex_cfg).unwrap();
    std::fs::remove_dir(&beta_unterm_codex_dir).unwrap();
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "beta-unterm"],
            true,
            &[],
        )
        .0,
        0
    );
    std::fs::write(&beta_unterm_codex_cfg, user_unterm_text).unwrap();
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "beta-unterm"],
            true,
            &[],
        )
        .0,
        0
    );
    // User deletes their text, leaving only the separator newline and OMS block.
    let mut sep_plus_block = vec![b'\n'];
    sep_plus_block.extend_from_slice(expected_beta_unterm_block.as_bytes());
    std::fs::write(&beta_unterm_codex_cfg, &sep_plus_block).unwrap();

    let (code, _, stderr) = run_panel(
        base,
        &["--uninstall-panel", "codex", "--project", "beta-unterm"],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(!beta_unterm_codex_cfg.exists());
    assert!(!beta_unterm_codex_dir.exists());
    assert_eq!(snapshot_tree(&beta_unterm_canon), beta_unterm_before_tree);
    assert!(!panels_sidecar.exists());
    assert_eq!(
        run_panel(base, &["--unregister-project", "beta-unterm"], true, &[]).0,
        0
    );

    // 3. Refusal on unmanaged `mcp_servers.oms-panel`, invalid TOML, symlinks, and tracked file.
    for (label, bad_toml, expected_err) in [
        (
            "unmanaged header",
            "[other]\nx = 1\n\n[mcp_servers.oms-panel]\ncommand = \"custom\"\nargs = []\n",
            "unmanaged mcp_servers.oms-panel",
        ),
        (
            "unmanaged quoted header",
            "[mcp_servers.\"oms-panel\"]\ncommand = \"custom\"\n",
            "unmanaged mcp_servers.oms-panel",
        ),
        (
            "unmanaged inline table",
            "mcp_servers = { oms-panel = { command = \"custom\" } }\n",
            "unmanaged mcp_servers.oms-panel",
        ),
        (
            "invalid TOML",
            "[mcp_servers\nbroken = [\n",
            "Invalid .codex/config.toml",
        ),
    ] {
        std::fs::write(&alpha_codex_cfg, bad_toml.as_bytes()).unwrap();
        let (code, _, stderr) = run_panel(
            base,
            &["--install-panel", "codex", "--project", "alpha-service"],
            true,
            &[],
        );
        assert_ne!(code, 0, "expected refusal on {label}");
        assert!(
            stderr.contains(expected_err),
            "unexpected stderr on {label}: {stderr}"
        );
        assert_eq!(
            std::fs::read(&alpha_codex_cfg).unwrap(),
            bad_toml.as_bytes(),
            "file must remain untouched on {label}"
        );
        assert_eq!(std::fs::read(&alpha_exclude).unwrap(), alpha_exclude_before);
        assert!(!panels_sidecar.exists());
    }

    // Symlinked `.codex/config.toml` and symlinked `.codex` dir are refused untouched.
    let outside_toml = base.join("outside-target.toml");
    write_private_file(&outside_toml, b"model = \"gpt-5\"\n");
    std::fs::remove_file(&alpha_codex_cfg).unwrap();
    std::os::unix::fs::symlink(&outside_toml, &alpha_codex_cfg).unwrap();
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "alpha-service"],
        true,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("symlink"), "{stderr}");
    assert_eq!(
        std::fs::read(&outside_toml).unwrap(),
        b"model = \"gpt-5\"\n"
    );
    std::fs::remove_file(&alpha_codex_cfg).unwrap();
    std::fs::remove_dir(&alpha_codex_dir).unwrap();

    let outside_codex_dir = base.join("outside-codex-dir");
    ensure_private_dir(&outside_codex_dir);
    std::os::unix::fs::symlink(&outside_codex_dir, &alpha_codex_dir).unwrap();
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "alpha-service"],
        true,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("symlink"), "{stderr}");
    std::fs::remove_file(&alpha_codex_dir).unwrap();

    // Tracked `.codex/config.toml` is refused with a clear message and never modified.
    std::fs::create_dir_all(&alpha_codex_dir).unwrap();
    let tracked_bytes = b"# tracked team config\nmodel = \"gpt-5\"\n";
    std::fs::write(&alpha_codex_cfg, tracked_bytes).unwrap();
    git_ok(&alpha_canon, &["add", "-f", ".codex/config.toml"]);
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "alpha-service"],
        true,
        &[],
    );
    assert_ne!(code, 0, "staged/tracked .codex/config.toml must be refused");
    assert!(stderr.contains("tracked"), "{stderr}");
    assert_eq!(std::fs::read(&alpha_codex_cfg).unwrap(), tracked_bytes);
    assert!(!panels_sidecar.exists());

    git_ok(&alpha_canon, &["commit", "-m", "track codex config"]);
    let (code, _, stderr) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "alpha-service"],
        true,
        &[],
    );
    assert_ne!(code, 0, "committed .codex/config.toml must be refused");
    assert!(stderr.contains("tracked"), "{stderr}");
    assert_eq!(std::fs::read(&alpha_codex_cfg).unwrap(), tracked_bytes);
    assert!(!panels_sidecar.exists());

    git_ok(&alpha_canon, &["rm", "-q", ".codex/config.toml"]);
    git_ok(&alpha_canon, &["commit", "-m", "untrack codex config"]);
    let _ = std::fs::remove_dir(&alpha_codex_dir);

    // 4. Refresh of an out-of-date block (older OMS version AND older resource version) vs
    //    refusal of an edited block (reinstall refuses untouched; uninstall leaves edited block
    //    and reports `preservedFiles`).
    let prefix_bytes = b"# Prefix\nmodel = \"gpt-5\"\n";
    let suffix_bytes = b"\n[features]\nexperimental = true\n";
    for (old_pkg, old_res) in [
        ("0.7.0-older", expected_res_ver),
        (expected_version, "v0-older"),
    ] {
        ensure_private_dir(&alpha_codex_dir);
        let old_block = render_expected_block(&alpha_canon, old_pkg, old_res);
        let old_sha = oms_panel_core::digest(old_block.as_bytes());
        let mut seeded_cfg = prefix_bytes.to_vec();
        seeded_cfg.extend_from_slice(old_block.as_bytes());
        seeded_cfg.extend_from_slice(suffix_bytes);
        write_private_file(&alpha_codex_cfg, &seeded_cfg);

        let mut exclude_with_line = alpha_exclude_before.clone();
        exclude_with_line.extend_from_slice(b"/.codex/config.toml\n");
        std::fs::write(&alpha_exclude, &exclude_with_line).unwrap();

        write_private_file(
            &panels_sidecar,
            serde_json::to_string_pretty(&json!({
                "alpha-service": [{
                    "app": "codex",
                    "scope": "project",
                    "name": "oms-panel",
                    "path": alpha_canon,
                    "block_sha256": old_sha,
                    "created_config": false,
                    "created_codex_dir": false,
                    "created_exclude": true,
                }]
            }))
            .unwrap()
            .as_bytes(),
        );

        let (code, ref_raw, stderr) = run_panel(
            base,
            &[
                "--install-panel",
                "codex",
                "--project",
                "alpha-service",
                "--json",
            ],
            true,
            &[],
        );
        assert_eq!(code, 0, "{stderr}");
        let ref_json: Value = serde_json::from_str(&ref_raw).unwrap();
        assert_eq!(ref_json["changed"], true);

        let mut expected_refreshed = prefix_bytes.to_vec();
        expected_refreshed.extend_from_slice(expected_alpha_block.as_bytes());
        expected_refreshed.extend_from_slice(suffix_bytes);
        assert_eq!(
            std::fs::read(&alpha_codex_cfg).unwrap(),
            expected_refreshed,
            "refresh must rewrite only the OMS block while keeping surrounding bytes"
        );
        let refreshed_sidecar: Value =
            serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
        assert_eq!(
            refreshed_sidecar["alpha-service"][0]["block_sha256"],
            oms_panel_core::digest(expected_alpha_block.as_bytes())
        );
    }

    // Now edit the OMS block on disk: reinstall refuses untouched, and uninstall preserves and
    // reports the edited block while dropping the sidecar row.
    let edited_block = expected_alpha_block.replace(
        "OMS_MCP_TOOL_PROFILE = \"chatgpt-panel\"",
        "OMS_MCP_TOOL_PROFILE = \"chatgpt-panel\"\nCUSTOM_FLAG = \"1\"",
    );
    let mut edited_cfg = prefix_bytes.to_vec();
    edited_cfg.extend_from_slice(edited_block.as_bytes());
    edited_cfg.extend_from_slice(suffix_bytes);
    write_private_file(&alpha_codex_cfg, &edited_cfg);

    let (code, _, ed_err) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "alpha-service"],
        true,
        &[],
    );
    assert_ne!(code, 0, "reinstall must refuse an edited OMS block");
    assert!(ed_err.contains("modified outside OMS"), "{ed_err}");
    assert_eq!(std::fs::read(&alpha_codex_cfg).unwrap(), edited_cfg);

    let (code, un_ed_raw, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "codex",
            "--project",
            "alpha-service",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let un_ed_json: Value = serde_json::from_str(&un_ed_raw).unwrap();
    assert_eq!(un_ed_json["changed"], true);
    assert_eq!(un_ed_json["preservedFiles"], json!([alpha_codex_cfg]));
    assert_eq!(
        std::fs::read(&alpha_codex_cfg).unwrap(),
        edited_cfg,
        "edited block must stay untouched on uninstall"
    );
    assert!(!panels_sidecar.exists());

    // Subsequent install with no row still refuses to overwrite the preserved edited block.
    let (code, _, retry_ed_err) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "alpha-service"],
        true,
        &[],
    );
    assert_ne!(code, 0);
    assert!(retry_ed_err.contains("OMS block"), "{retry_ed_err}");
    assert_eq!(std::fs::read(&alpha_codex_cfg).unwrap(), edited_cfg);

    std::fs::remove_file(&alpha_codex_cfg).unwrap();
    std::fs::remove_dir(&alpha_codex_dir).unwrap();
    std::fs::write(&alpha_exclude, &alpha_exclude_before).unwrap();

    // 5. Linked worktree `.git/info/exclude` (uses common git dir), pre-ignored checkout via
    //    `.gitignore`, and selective uninstall cleanup of OMS-created artifacts only.
    let linked_wt = base.join("checkouts/alpha-linked-wt");
    git_ok(
        &alpha_canon,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt-branch",
            linked_wt.to_str().unwrap(),
        ],
    );
    let linked_canon = linked_wt.canonicalize().unwrap();
    assert_eq!(
        run_panel(
            &linked_canon,
            &["--register-project", "--alias", "alpha-wt"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "alpha-wt"],
            true,
            &[],
        )
        .0,
        0
    );
    assert!(
        std::fs::read_to_string(&alpha_exclude)
            .unwrap()
            .lines()
            .any(|l| l == "/.codex/config.toml"),
        "linked worktree must write exclude line into common git dir"
    );
    assert_eq!(git_ok(&linked_canon, &["status", "--porcelain"]), "");

    // Add a user file inside OMS-created `.codex/` and a user pattern in `info/exclude`:
    // uninstall removes `.codex/config.toml` and `/.codex/config.toml`, while keeping `.codex/`
    // (non-empty) and the user pattern in `info/exclude`.
    let wt_extra = linked_canon.join(".codex/user-note.txt");
    std::fs::write(&wt_extra, b"keep\n").unwrap();
    let mut exclude_with_user_rule = std::fs::read(&alpha_exclude).unwrap();
    exclude_with_user_rule.extend_from_slice(b"*.local-scratch\n");
    std::fs::write(&alpha_exclude, &exclude_with_user_rule).unwrap();

    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "codex", "--project", "alpha-wt"],
            true,
            &[],
        )
        .0,
        0
    );
    assert!(!linked_canon.join(".codex/config.toml").exists());
    assert!(
        wt_extra.exists(),
        "non-empty .codex dir with user file must remain"
    );
    let mut expected_excl_keep_user = alpha_exclude_before.clone();
    expected_excl_keep_user.extend_from_slice(b"*.local-scratch\n");
    assert_eq!(
        std::fs::read(&alpha_exclude).unwrap(),
        expected_excl_keep_user
    );
    std::fs::write(&alpha_exclude, &alpha_exclude_before).unwrap();

    // Pre-existing empty `.codex` dir + `.gitignore` ignoring `.codex/`:
    // `created_codex_dir: false`, `created_exclude: false`, `created_config: true`.
    let preignored = base.join("checkouts/preignored-service");
    std::fs::create_dir_all(&preignored).unwrap();
    init_git_repo(&preignored);
    std::fs::write(preignored.join(".gitignore"), ".codex/\n").unwrap();
    git_ok(&preignored, &["add", ".gitignore"]);
    git_ok(&preignored, &["commit", "-m", "ignore codex"]);
    std::fs::create_dir_all(preignored.join(".codex")).unwrap();
    let preignored_canon = preignored.canonicalize().unwrap();
    let preignored_exclude = preignored_canon.join(".git/info/exclude");
    let preignored_excl_before = std::fs::read(&preignored_exclude).unwrap();

    assert_eq!(
        run_panel(
            &preignored_canon,
            &["--register-project", "--alias", "app.v1"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "app.v1"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        std::fs::read(&preignored_exclude).unwrap(),
        preignored_excl_before,
        "already-ignored .codex/config.toml must not touch .git/info/exclude"
    );
    let pre_sidecar: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(pre_sidecar["app.v1"][0]["created_config"], true);
    assert_eq!(pre_sidecar["app.v1"][0]["created_codex_dir"], false);
    assert_eq!(pre_sidecar["app.v1"][0]["created_exclude"], false);

    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "codex", "--project", "app.v1"],
            true,
            &[],
        )
        .0,
        0
    );
    assert!(!preignored_canon.join(".codex/config.toml").exists());
    assert!(
        preignored_canon.join(".codex").exists(),
        "pre-existing .codex dir must not be deleted on uninstall"
    );
    assert_eq!(
        std::fs::read(&preignored_exclude).unwrap(),
        preignored_excl_before
    );

    // Deleted checkout forgets the row (`action: "forgotten"`) and unblocks `--unregister-project`;
    // explicit `OMS_PANEL_PROJECTS_FILE` also works for Codex panels.
    let alt_map = base.join("alt-projects/desktop-projects.json");
    let alt_map_str = alt_map.to_str().unwrap();
    let ephemeral = base.join("checkouts/ephemeral");
    std::fs::create_dir_all(ephemeral.join(".oms")).unwrap();
    let ephemeral_canon = ephemeral.canonicalize().unwrap();
    assert_eq!(
        run_panel(
            &ephemeral_canon,
            &["--register-project", "--alias", "EphemeralProj"],
            true,
            &[("OMS_PANEL_PROJECTS_FILE", alt_map_str)],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "EphemeralProj"],
            true,
            &[("OMS_PANEL_PROJECTS_FILE", alt_map_str)],
        )
        .0,
        0
    );
    std::fs::remove_dir_all(&ephemeral_canon).unwrap();
    let (code, forget_raw, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "codex",
            "--project",
            "EphemeralProj",
            "--json",
        ],
        true,
        &[("OMS_PANEL_PROJECTS_FILE", alt_map_str)],
    );
    assert_eq!(code, 0, "{stderr}");
    let forget_json: Value = serde_json::from_str(&forget_raw).unwrap();
    assert_eq!(forget_json["action"], "forgotten");
    assert_eq!(forget_json["changed"], true);

    // 6. Legacy `user` / `plugin_sha256` sidecar shape is rejected like other invalid rows.
    let fake_sha = "a".repeat(64);
    write_private_file(
        &panels_sidecar,
        serde_json::to_string(&json!({
            "PoseFixer": [{
                "app": "codex",
                "scope": "user",
                "name": "oms-posefixer@oms-host-panels",
                "path": pose_canon,
                "plugin_sha256": fake_sha,
                "mcp_sha256": fake_sha,
            }]
        }))
        .unwrap()
        .as_bytes(),
    );
    for mode in ["--install-panel", "--uninstall-panel"] {
        let (code, _, stderr) =
            run_panel(base, &[mode, "codex", "--project", "PoseFixer"], true, &[]);
        assert_ne!(code, 0, "legacy plugin row must be rejected on {mode}");
        assert!(stderr.contains("Invalid OMS project panel map"), "{stderr}");
    }
    std::fs::remove_file(&panels_sidecar).unwrap();

    // 7. Rollback restoring exact bytes when sidecar publication fails (both for an existing
    //    `.codex/config.toml` and for a missing `.codex/config.toml`).
    let mut near_full_panels = serde_json::Map::new();
    let filler_path = format!("/srv/{}", "x".repeat(95));
    for idx in 0..91 {
        near_full_panels.insert(
            format!("filler-{idx:03}"),
            json!([{
                "app": "claude",
                "scope": "local",
                "name": "oms-panel",
                "path": filler_path,
            }]),
        );
    }
    let probe_len = serde_json::to_vec(&Value::Object(near_full_panels.clone()))
        .unwrap()
        .len();
    let pad_len = 16350usize.saturating_sub(probe_len + 90).max(1);
    near_full_panels.insert(
        "filler-pad".into(),
        json!([{
            "app": "claude",
            "scope": "local",
            "name": "oms-panel",
            "path": format!("/srv/{}", "p".repeat(pad_len)),
        }]),
    );
    let mut near_full_bytes = serde_json::to_vec(&Value::Object(near_full_panels)).unwrap();
    near_full_bytes.push(b'\n');
    assert!(
        near_full_bytes.len() <= 16384 && near_full_bytes.len() > 16250,
        "unexpected filler size: {}",
        near_full_bytes.len()
    );
    write_private_file(&panels_sidecar, &near_full_bytes);

    // 7(a): Existing `.codex/config.toml` -> rollback restores exact prior bytes, mode, and exclude.
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&alpha_codex_dir)
        .unwrap();
    let rb_prior_bytes = b"# Exact bytes before failed sidecar write\r\n[model]\nname = \"o3\"\n";
    std::fs::write(&alpha_codex_cfg, rb_prior_bytes).unwrap();
    std::fs::set_permissions(&alpha_codex_cfg, std::fs::Permissions::from_mode(0o640)).unwrap();

    let (code, _, rb_err) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "alpha-service"],
        true,
        &[],
    );
    assert_ne!(code, 0);
    assert!(rb_err.contains("rolled back"), "{rb_err}");
    assert_eq!(std::fs::read(&alpha_codex_cfg).unwrap(), rb_prior_bytes);
    assert_eq!(
        std::fs::symlink_metadata(&alpha_codex_cfg).unwrap().mode() & 0o7777,
        0o640
    );
    assert_eq!(std::fs::read(&alpha_exclude).unwrap(), alpha_exclude_before);
    assert_eq!(std::fs::read(&panels_sidecar).unwrap(), near_full_bytes);

    // 7(b): Missing `.codex/` and `.codex/config.toml` -> rollback removes created file, dir, and exclude.
    std::fs::remove_file(&alpha_codex_cfg).unwrap();
    std::fs::remove_dir(&alpha_codex_dir).unwrap();
    let (code, _, rb_missing_err) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "alpha-service"],
        true,
        &[],
    );
    assert_ne!(code, 0);
    assert!(rb_missing_err.contains("rolled back"), "{rb_missing_err}");
    assert!(!alpha_codex_cfg.exists() && !alpha_codex_dir.exists());
    assert_eq!(std::fs::read(&alpha_exclude).unwrap(), alpha_exclude_before);
    std::fs::remove_file(&panels_sidecar).unwrap();

    // 8. One owner per block in one sidecar (second alias for same physical checkout is refused
    //    naming the existing alias before any change), while an unowned exact OMS block is adopted.
    assert_eq!(
        run_panel(
            &alpha_canon,
            &["--register-project", "--alias", "alpha-dup"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "alpha-service"],
            true,
            &[],
        )
        .0,
        0
    );
    let sidecar_before_dup = std::fs::read(&panels_sidecar).unwrap();
    let cfg_before_dup = std::fs::read(&alpha_codex_cfg).unwrap();
    let excl_before_dup = std::fs::read(&alpha_exclude).unwrap();

    for extra_flag in [Some("--dry-run"), None] {
        let mut args = vec!["--install-panel", "codex", "--project", "alpha-dup"];
        if let Some(flag) = extra_flag {
            args.push(flag);
        }
        let (code, _, dup_err) = run_panel(base, &args, true, &[]);
        assert_ne!(
            code, 0,
            "second Codex alias for same checkout must be refused ({extra_flag:?})"
        );
        assert!(
            dup_err.contains("alpha-service"),
            "error must name existing alias 'alpha-service': {dup_err}"
        );
        assert_eq!(std::fs::read(&panels_sidecar).unwrap(), sidecar_before_dup);
        assert_eq!(std::fs::read(&alpha_codex_cfg).unwrap(), cfg_before_dup);
        assert_eq!(std::fs::read(&alpha_exclude).unwrap(), excl_before_dup);
    }

    // Lost sidecar recovery: when no row owns the exact on-disk OMS block, install adopts it.
    std::fs::remove_file(&panels_sidecar).unwrap();
    let (code, adopt_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "alpha-dup",
            "--json",
        ],
        true,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let adopt_json: Value = serde_json::from_str(&adopt_raw).unwrap();
    assert_eq!(adopt_json["action"], "adopted");
    assert_eq!(adopt_json["changed"], true);
    assert_eq!(std::fs::read(&alpha_codex_cfg).unwrap(), cfg_before_dup);

    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "codex", "--project", "alpha-dup"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(base, &["--unregister-project", "alpha-dup"], true, &[]).0,
        0
    );
    let _ = std::fs::remove_file(&alpha_codex_cfg);
    let _ = std::fs::remove_dir(&alpha_codex_dir);
    std::fs::write(&alpha_exclude, &alpha_exclude_before).unwrap();

    // 9. Hand over exclude-line ownership across two linked worktrees of one repo:
    //    install A -> install B -> uninstall A -> uninstall B restores `info/exclude` byte-for-byte.
    let _ = std::fs::remove_file(&wt_extra);
    let _ = std::fs::remove_dir(linked_canon.join(".codex"));
    let linked_wt_b = base.join("checkouts/alpha-linked-wt-b");
    git_ok(
        &alpha_canon,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt-branch-b",
            linked_wt_b.to_str().unwrap(),
        ],
    );
    let linked_b_canon = linked_wt_b.canonicalize().unwrap();
    assert_eq!(
        run_panel(
            &linked_b_canon,
            &["--register-project", "--alias", "alpha-wt-b"],
            true,
            &[],
        )
        .0,
        0
    );

    let shared_excl_orig = b"# shared repo exclude without trailing newline\n*.tmp-build";
    std::fs::write(&alpha_exclude, shared_excl_orig).unwrap();

    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "alpha-wt"],
            true,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "codex", "--project", "alpha-wt-b"],
            true,
            &[],
        )
        .0,
        0
    );

    let both_wt_sidecar: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(both_wt_sidecar["alpha-wt"][0]["created_exclude"], true);
    assert_eq!(
        both_wt_sidecar["alpha-wt"][0]["exclude_separator_added"],
        true
    );
    assert_eq!(both_wt_sidecar["alpha-wt-b"][0]["created_exclude"], false);
    assert!(both_wt_sidecar["alpha-wt-b"][0]["exclude_separator_added"].is_null());

    // Uninstall A while B remains: exclude line stays active and ownership transfers to B.
    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "codex", "--project", "alpha-wt"],
            true,
            &[],
        )
        .0,
        0
    );
    assert!(!linked_canon.join(".codex").exists());
    assert_eq!(git_ok(&linked_b_canon, &["status", "--porcelain"]), "");
    let after_a_sidecar: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert!(after_a_sidecar["alpha-wt"].is_null());
    assert_eq!(after_a_sidecar["alpha-wt-b"][0]["created_exclude"], true);
    assert_eq!(
        after_a_sidecar["alpha-wt-b"][0]["exclude_separator_added"],
        true
    );

    // Uninstall B: last owner removes exclude line AND separator, restoring exact original bytes.
    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "codex", "--project", "alpha-wt-b"],
            true,
            &[],
        )
        .0,
        0
    );
    assert!(!linked_b_canon.join(".codex").exists());
    assert!(!panels_sidecar.exists());
    assert_eq!(
        std::fs::read(&alpha_exclude).unwrap(),
        shared_excl_orig,
        "two-worktree install A, install B, uninstall A, uninstall B must restore info/exclude byte-for-byte"
    );
    std::fs::write(&alpha_exclude, &alpha_exclude_before).unwrap();

    // 10. Trust lookup matches Codex resolution:
    //     - `[projects."<cwd>"].trust_level` checked first;
    //     - falls back to `[projects."<repo-root>"]` (parent of canonical `--git-common-dir`);
    //     - explicit `untrusted` on worktree overrides trusted main root;
    //     - non-repository parent never counts;
    //     - untrusted hint names checkout path when its own trust_level is set (or outside Git), else repo root.
    let nonrepo_parent = base.join("trust-matrix/nonrepo-parent");
    ensure_private_dir(&nonrepo_parent);
    let nonrepo_parent_canon = nonrepo_parent.canonicalize().unwrap();
    let tm_main = nonrepo_parent.join("main-repo");
    std::fs::create_dir_all(&tm_main).unwrap();
    init_git_repo(&tm_main);
    let tm_main_canon = tm_main.canonicalize().unwrap();

    let tm_sub = tm_main_canon.join("packages/sub-checkout");
    std::fs::create_dir_all(tm_sub.join(".oms")).unwrap();
    let tm_sub_canon = tm_sub.canonicalize().unwrap();

    let tm_wt = base.join("trust-matrix/linked-wt");
    git_ok(
        &tm_main_canon,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "tm-wt-branch",
            tm_wt.to_str().unwrap(),
        ],
    );
    let tm_wt_canon = tm_wt.canonicalize().unwrap();

    let tm_nongit = nonrepo_parent_canon.join("standalone-oms");
    std::fs::create_dir_all(tm_nongit.join(".oms")).unwrap();
    let tm_nongit_canon = tm_nongit.canonicalize().unwrap();

    for (dir, alias) in [
        (&tm_main_canon, "tm-main"),
        (&tm_sub_canon, "tm-sub"),
        (&tm_wt_canon, "tm-wt"),
        (&tm_nongit_canon, "tm-nongit"),
    ] {
        assert_eq!(
            run_panel(dir, &["--register-project", "--alias", alias], true, &[]).0,
            0
        );
    }

    let trust_codex_dir = base.join("trust-codex-home");
    ensure_private_dir(&trust_codex_dir);
    let trust_codex_cfg = trust_codex_dir.join("config.toml");
    let trust_codex_str = trust_codex_dir.to_str().unwrap();

    let probe_trust = |alias: &str| -> Value {
        let (code, out, err) = run_panel(
            base,
            &[
                "--install-panel",
                "codex",
                "--project",
                alias,
                "--dry-run",
                "--json",
            ],
            false,
            &[("CODEX_HOME", trust_codex_str)],
        );
        assert_eq!(code, 0, "dry-run trust probe failed for {alias}: {err}");
        serde_json::from_str(&out).unwrap()
    };

    // 10(a): Non-repo parent folder trusted -> none of main, main/sub, linked wt, or nongit are active;
    //        Git checkouts hint the main repo root while non-Git checkout hints its own path.
    write_private_file(
        &trust_codex_cfg,
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            nonrepo_parent_canon.display()
        )
        .as_bytes(),
    );
    let expected_repo_root_hint = format!(
        "Trust {} in Codex so it loads .codex/config.toml.",
        tm_main_canon.display()
    );
    for git_alias in ["tm-main", "tm-sub", "tm-wt"] {
        let res = probe_trust(git_alias);
        assert_eq!(res["active"], false, "{git_alias}: {res}");
        assert_eq!(res["reason"], "codex-project-untrusted");
        assert_eq!(res["hint"], expected_repo_root_hint, "{git_alias}: {res}");
    }
    let nongit_res = probe_trust("tm-nongit");
    assert_eq!(nongit_res["active"], false);
    assert_eq!(nongit_res["reason"], "codex-project-untrusted");
    assert_eq!(
        nongit_res["hint"],
        format!(
            "Trust {} in Codex so it loads .codex/config.toml.",
            tm_nongit_canon.display()
        )
    );

    // 10(b): Main repo root trusted -> main, main/sub, AND linked worktree are all active!
    write_private_file(
        &trust_codex_cfg,
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            tm_main_canon.display()
        )
        .as_bytes(),
    );
    for (alias, expect_active) in [
        ("tm-main", true),
        ("tm-sub", true),
        ("tm-wt", true),
        ("tm-nongit", false),
    ] {
        let (code, inst_raw, err) = run_panel(
            base,
            &["--install-panel", "codex", "--project", alias, "--json"],
            false,
            &[("CODEX_HOME", trust_codex_str)],
        );
        assert_eq!(code, 0, "{alias}: {err}");
        let inst: Value = serde_json::from_str(&inst_raw).unwrap();
        assert_eq!(inst["active"], expect_active, "{alias}: {inst}");
        if expect_active {
            assert!(inst.get("reason").is_none() && inst.get("hint").is_none());
        }
    }

    // 10(c): Explicit `untrusted` on linked worktree wins over trusted main repo root,
    //        and its hint names the worktree checkout path itself rather than the repo root.
    write_private_file(
        &trust_codex_cfg,
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n\n[projects.\"{}\"]\ntrust_level = \"untrusted\"\n",
            tm_main_canon.display(),
            tm_wt_canon.display()
        )
        .as_bytes(),
    );
    assert_eq!(probe_trust("tm-main")["active"], true);
    assert_eq!(probe_trust("tm-sub")["active"], true);
    let wt_override = probe_trust("tm-wt");
    assert_eq!(wt_override["active"], false);
    assert_eq!(wt_override["reason"], "codex-project-untrusted");
    assert_eq!(
        wt_override["hint"],
        format!(
            "Trust {} in Codex so it loads .codex/config.toml.",
            tm_wt_canon.display()
        )
    );

    // 10(c2): A `[projects."<worktree>"]` table with no `trust_level` key falls back to the
    //         trusted main repo root (`active: true`).
    write_private_file(
        &trust_codex_cfg,
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n\n[projects.\"{}\"]\n",
            tm_main_canon.display(),
            tm_wt_canon.display()
        )
        .as_bytes(),
    );
    let wt_fallback = probe_trust("tm-wt");
    assert_eq!(wt_fallback["active"], true, "{wt_fallback}");
    assert!(wt_fallback.get("reason").is_none() && wt_fallback.get("hint").is_none());
    let (code, wt_real_raw, err) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "tm-wt", "--json"],
        false,
        &[("CODEX_HOME", trust_codex_str)],
    );
    assert_eq!(code, 0, "{err}");
    let wt_real: Value = serde_json::from_str(&wt_real_raw).unwrap();
    assert_eq!(wt_real["active"], true, "{wt_real}");
    assert!(wt_real.get("reason").is_none() && wt_real.get("hint").is_none());

    // 10(d): Linked worktree path only trusted -> only linked worktree is active.
    write_private_file(
        &trust_codex_cfg,
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            tm_wt_canon.display()
        )
        .as_bytes(),
    );
    assert_eq!(probe_trust("tm-main")["active"], false);
    assert_eq!(probe_trust("tm-sub")["active"], false);
    assert_eq!(probe_trust("tm-wt")["active"], true);

    // 10(e): Subdirectory path only trusted -> only subdirectory checkout is active.
    write_private_file(
        &trust_codex_cfg,
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            tm_sub_canon.display()
        )
        .as_bytes(),
    );
    assert_eq!(probe_trust("tm-main")["active"], false);
    assert_eq!(probe_trust("tm-sub")["active"], true);
    assert_eq!(probe_trust("tm-wt")["active"], false);

    for alias in ["tm-main", "tm-sub", "tm-wt", "tm-nongit"] {
        assert_eq!(
            run_panel(
                base,
                &["--uninstall-panel", "codex", "--project", alias],
                false,
                &[("CODEX_HOME", trust_codex_str)],
            )
            .0,
            0
        );
    }

    // 11. Read-only trust inspection never fails the install when CODEX_HOME or config.toml is a
    //     symlink, non-regular file, unreadable, or invalid TOML (`reason: "codex-trust-unknown"`,
    //     without following symlinks).
    let symlink_codex_home = base.join("symlinked-codex-home");
    // Point symlinked CODEX_HOME at `codex_home` (where `PoseFixer` IS trusted!) to prove the
    // symlink is not followed.
    std::os::unix::fs::symlink(&codex_home, &symlink_codex_home).unwrap();
    let (code, sym_home_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--json",
        ],
        false,
        &[("CODEX_HOME", symlink_codex_home.to_str().unwrap())],
    );
    assert_eq!(
        code, 0,
        "symlinked CODEX_HOME must not fail install: {stderr}"
    );
    let sym_home_json: Value = serde_json::from_str(&sym_home_raw).unwrap();
    assert_eq!(sym_home_json["changed"], true);
    assert_eq!(sym_home_json["active"], false);
    assert_eq!(sym_home_json["reason"], "codex-trust-unknown");
    assert!(
        sym_home_json["hint"]
            .as_str()
            .is_some_and(|h| h.contains("symlink") && !h.contains('\n')),
        "{sym_home_json}"
    );
    assert!(pose_codex_cfg.is_file());

    // Plain output also reports `codex-trust-unknown` with a `hint:` line.
    let (code, sym_plain, stderr) = run_panel(
        base,
        &["--install-panel", "codex", "--project", "PoseFixer"],
        false,
        &[("CODEX_HOME", symlink_codex_home.to_str().unwrap())],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        sym_plain.contains("(inactive: codex-trust-unknown)\nhint: "),
        "{sym_plain}"
    );

    // Symlinked `CODEX_HOME/config.toml`, non-regular `config.toml` (directory), invalid TOML,
    // and unreadable `config.toml` (0000) all succeed with `codex-trust-unknown`.
    let broken_codex_home = base.join("broken-codex-home");
    ensure_private_dir(&broken_codex_home);
    let broken_cfg = broken_codex_home.join("config.toml");
    let broken_home_str = broken_codex_home.to_str().unwrap();

    // (a) symlinked config.toml pointing to trusted `codex_home_cfg` (must not follow)
    std::os::unix::fs::symlink(&codex_home_cfg, &broken_cfg).unwrap();
    let (code, out, err) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--json",
        ],
        false,
        &[("CODEX_HOME", broken_home_str)],
    );
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["active"], false);
    assert_eq!(v["reason"], "codex-trust-unknown");
    assert!(v["hint"].as_str().is_some_and(|h| h.contains("symlink")));
    std::fs::remove_file(&broken_cfg).unwrap();

    // (b) config.toml is a directory (not a regular file)
    ensure_private_dir(&broken_cfg);
    let (code, out, err) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--json",
        ],
        false,
        &[("CODEX_HOME", broken_home_str)],
    );
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["active"], false);
    assert_eq!(v["reason"], "codex-trust-unknown");
    assert!(
        v["hint"]
            .as_str()
            .is_some_and(|h| h.contains("regular file"))
    );
    std::fs::remove_dir(&broken_cfg).unwrap();

    // (c) invalid TOML in config.toml
    write_private_file(&broken_cfg, b"[projects\nbroken = [\n");
    let (code, out, err) = run_panel(
        base,
        &[
            "--install-panel",
            "codex",
            "--project",
            "PoseFixer",
            "--json",
        ],
        false,
        &[("CODEX_HOME", broken_home_str)],
    );
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["active"], false);
    assert_eq!(v["reason"], "codex-trust-unknown");
    assert!(
        v["hint"]
            .as_str()
            .is_some_and(|h| h.contains("valid TOML") && !h.contains('\n'))
    );

    // (d) unreadable config.toml (when running as non-root)
    if std::fs::metadata(base).unwrap().uid() != 0 {
        std::fs::set_permissions(&broken_cfg, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (code, out, err) = run_panel(
            base,
            &[
                "--install-panel",
                "codex",
                "--project",
                "PoseFixer",
                "--json",
            ],
            false,
            &[("CODEX_HOME", broken_home_str)],
        );
        std::fs::set_permissions(&broken_cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(code, 0, "{err}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["active"], false);
        assert_eq!(v["reason"], "codex-trust-unknown");
        assert!(
            v["hint"]
                .as_str()
                .is_some_and(|h| !h.is_empty() && !h.contains('\n'))
        );
    }

    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "codex", "--project", "PoseFixer"],
            true,
            &[],
        )
        .0,
        0
    );
    assert!(!pose_codex_cfg.exists() && !pose_codex_dir.exists() && !panels_sidecar.exists());
    assert_eq!(std::fs::read(&pose_exclude).unwrap(), pose_exclude_before);

    // 12. Antigravity project panels (`.agents/mcp_config.json`), reusing the temporary HOME,
    //     git-only PATH (no `agy` binary), and checkout fixtures without touching `~/.gemini`.
    let gemini_dir = home_dir.join(".gemini");
    let pose_agents_dir = pose_canon.join(".agents");
    let pose_agy_cfg = pose_agents_dir.join("mcp_config.json");

    let render_expected_agy_entry = |repo_canon: &Path, res_ver: &str| -> Value {
        json!({
            "command": expected_launcher,
            "args": ["mcp-server"],
            "env": {
                "OMS_MCP_TOOL_PROFILE": "chatgpt-panel",
                "OMS_PANEL_REPO": repo_canon,
                "OMS_MCP_PANEL_RESOURCE_VERSION": res_ver,
            }
        })
    };
    let canonical_entry_sha = |entry: &Value| -> String {
        let mut sorted_env = serde_json::Map::new();
        let env_bt: std::collections::BTreeMap<&str, &Value> = entry["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        for (k, v) in env_bt {
            sorted_env.insert(k.to_owned(), (*v).clone());
        }
        let mut sorted = serde_json::Map::new();
        sorted.insert("args".into(), entry["args"].clone());
        sorted.insert("command".into(), entry["command"].clone());
        sorted.insert("env".into(), Value::Object(sorted_env));
        oms_panel_core::digest(&serde_json::to_vec(&Value::Object(sorted)).unwrap())
    };
    let map_keys = |val: &Value| -> Vec<String> {
        val.as_object().unwrap().keys().cloned().collect::<Vec<_>>()
    };

    // 12(a): Dry-run and fresh install report like Claude (no trust fields, never touching `~/.gemini`).
    let (code, agy_dry_txt, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "PoseFixer",
            "--dry-run",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        agy_dry_txt,
        format!(
            "would install antigravity panel oms-panel for PoseFixer: {}\n",
            pose_canon.display()
        )
    );
    let (code, agy_dry_json_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "PoseFixer",
            "--dry-run",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&agy_dry_json_raw).unwrap(),
        json!({
            "app": "antigravity",
            "alias": "PoseFixer",
            "path": pose_canon,
            "entry": "oms-panel",
            "action": "install",
            "changed": false,
        })
    );
    assert!(!pose_agents_dir.exists() && !panels_sidecar.exists() && !gemini_dir.exists());

    let (code, agy_inst_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "PoseFixer",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(!gemini_dir.exists(), "install must never touch ~/.gemini");
    let agy_inst_json: Value = serde_json::from_str(&agy_inst_raw).unwrap();
    assert_eq!(
        agy_inst_json,
        json!({
            "app": "antigravity",
            "alias": "PoseFixer",
            "path": pose_canon,
            "entry": "oms-panel",
            "action": "install",
            "changed": true,
        })
    );

    let agy_dir_meta = std::fs::symlink_metadata(&pose_agents_dir).unwrap();
    assert!(agy_dir_meta.is_dir() && !agy_dir_meta.file_type().is_symlink());
    assert_eq!(agy_dir_meta.mode() & 0o7777, 0o700);
    let agy_cfg_meta = std::fs::symlink_metadata(&pose_agy_cfg).unwrap();
    assert!(agy_cfg_meta.is_file() && !agy_cfg_meta.file_type().is_symlink());
    assert_eq!(agy_cfg_meta.nlink(), 1);
    assert_eq!(agy_cfg_meta.mode() & 0o7777, 0o600);

    let expected_pose_entry = render_expected_agy_entry(&pose_canon, expected_res_ver);
    let expected_pose_cfg_str = format!(
        "{}\n",
        serde_json::to_string_pretty(&json!({"mcpServers": {"oms-panel": expected_pose_entry}}))
            .unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(&pose_agy_cfg).unwrap(),
        expected_pose_cfg_str
    );
    let expected_pose_entry_sha = canonical_entry_sha(&expected_pose_entry);

    let agy_sidecar_meta = std::fs::symlink_metadata(&panels_sidecar).unwrap();
    assert_eq!(agy_sidecar_meta.mode() & 0o7777, 0o600);
    let agy_sidecar_val: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert_eq!(
        agy_sidecar_val,
        json!({
            "PoseFixer": [{
                "app": "antigravity",
                "scope": "project",
                "name": "oms-panel",
                "path": pose_canon,
                "entry_sha256": expected_pose_entry_sha,
                "created_config": true,
                "created_agents_dir": true,
                "created_exclude": true,
            }]
        })
    );

    let mut expected_agy_exclude = pose_exclude_before.clone();
    expected_agy_exclude.extend_from_slice(b"/.agents/mcp_config.json\n");
    assert_eq!(std::fs::read(&pose_exclude).unwrap(), expected_agy_exclude);
    assert_eq!(git_ok(&pose_canon, &["status", "--porcelain"]), "");

    // 12(b): Rerun no-op leaves inodes untouched and reports no trust fields.
    let agy_cfg_ino_before = agy_cfg_meta.ino();
    let agy_sidecar_ino_before = agy_sidecar_meta.ino();

    let (code, agy_noop_plain, stderr) = run_panel(
        base,
        &["--install-panel", "antigravity", "--project", "PoseFixer"],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        agy_noop_plain,
        format!(
            "already installed antigravity panel oms-panel for PoseFixer: {}\n",
            pose_canon.display()
        )
    );
    let (code, agy_noop_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "PoseFixer",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&agy_noop_raw).unwrap(),
        json!({
            "app": "antigravity",
            "alias": "PoseFixer",
            "path": pose_canon,
            "entry": "oms-panel",
            "action": "install",
            "changed": false,
        })
    );
    assert_eq!(
        std::fs::symlink_metadata(&pose_agy_cfg).unwrap().ino(),
        agy_cfg_ino_before
    );
    assert_eq!(
        std::fs::symlink_metadata(&panels_sidecar).unwrap().ino(),
        agy_sidecar_ino_before
    );

    // `--unregister-project` is refused while the Antigravity panel remains installed.
    let (code, _, stderr) = run_panel(base, &["--unregister-project", "PoseFixer"], false, &[]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("--uninstall-panel antigravity --project PoseFixer"),
        "{stderr}"
    );

    // Uninstall cleans up OMS-created `.agents/mcp_config.json`, `.agents` dir, exclude line, and sidecar.
    let (code, un_agy_raw, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "antigravity",
            "--project",
            "PoseFixer",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&un_agy_raw).unwrap(),
        json!({
            "app": "antigravity",
            "alias": "PoseFixer",
            "path": pose_canon,
            "entry": "oms-panel",
            "action": "uninstall",
            "changed": true,
        })
    );
    assert!(!pose_agy_cfg.exists());
    assert!(!pose_agents_dir.exists());
    assert!(!panels_sidecar.exists());
    assert_eq!(std::fs::read(&pose_exclude).unwrap(), pose_exclude_before);

    // 12(c): Existing `.agents/mcp_config.json` with other servers/keys, custom formatting, mode 0640,
    //        and unterminated `.git/info/exclude`:
    //        - install rewrites `.agents/mcp_config.json` with 2-space pretty JSON preserving all
    //          other keys/servers, key order, and mode 0640;
    //        - uninstall removes only `oms-panel` and writes 2-space pretty JSON (byte-exact whenever
    //          the prior file was already 2-space pretty JSON) while restoring `info/exclude` bytes;
    //        - when OMS created `mcpServers` in a pre-existing file and `oms-panel` is already gone at
    //          uninstall, uninstall removes the empty `mcpServers` key instead of leaving `{}` behind.
    let alpha_agents_dir = alpha_canon.join(".agents");
    let alpha_agy_cfg = alpha_agents_dir.join("mcp_config.json");
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&alpha_agents_dir)
        .unwrap();
    std::fs::set_permissions(&alpha_agents_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    let custom_agy_user_bytes = b"{\r\n    \"zetaTop\": true,\r\n    \"mcpServers\": {\r\n        \"sqlite-helper\": { \"command\": \"/usr/local/bin/sqlite-mcp\", \"args\": [\"--serve\", \"/tmp/db\"] },\r\n        \"alpha-remote\": { \"url\": \"https://mcp.example.invalid/mcp\" }\r\n    },\r\n    \"alphaTail\": 7\r\n}";
    let unterm_agy_exclude = b"# Unterminated exclude\n*.local-agy";
    std::fs::write(&alpha_agy_cfg, custom_agy_user_bytes).unwrap();
    std::fs::set_permissions(&alpha_agy_cfg, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::write(&alpha_exclude, unterm_agy_exclude).unwrap();

    let (code, alpha_agy_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&alpha_agy_raw).unwrap(),
        json!({
            "app": "antigravity",
            "alias": "alpha-service",
            "path": alpha_canon,
            "entry": "oms-panel",
            "action": "install",
            "changed": true,
        })
    );

    let on_disk_after_install = std::fs::read_to_string(&alpha_agy_cfg).unwrap();
    let expected_alpha_entry = render_expected_agy_entry(&alpha_canon, expected_res_ver);
    let parsed_after_install: Value = serde_json::from_str(&on_disk_after_install).unwrap();
    assert_eq!(
        map_keys(&parsed_after_install),
        vec!["zetaTop", "mcpServers", "alphaTail"]
    );
    assert_eq!(
        map_keys(&parsed_after_install["mcpServers"]),
        vec!["sqlite-helper", "alpha-remote", "oms-panel"]
    );
    assert_eq!(parsed_after_install["zetaTop"], true);
    assert_eq!(parsed_after_install["alphaTail"], 7);
    assert_eq!(
        parsed_after_install["mcpServers"]["sqlite-helper"]["args"],
        json!(["--serve", "/tmp/db"])
    );
    assert_eq!(
        parsed_after_install["mcpServers"]["alpha-remote"]["url"],
        "https://mcp.example.invalid/mcp"
    );
    assert_eq!(
        parsed_after_install["mcpServers"]["oms-panel"],
        expected_alpha_entry
    );
    assert_eq!(
        on_disk_after_install,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&parsed_after_install).unwrap()
        ),
        "install must write 2-space pretty JSON when modifying .agents/mcp_config.json"
    );
    assert_eq!(
        std::fs::symlink_metadata(&alpha_agy_cfg).unwrap().mode() & 0o7777,
        0o640
    );
    let sidecar_after_custom: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    assert!(
        sidecar_after_custom["alpha-service"][0]
            .get("prior_ws")
            .is_none()
    );
    assert!(
        sidecar_after_custom["alpha-service"][0]
            .get("prior_sha256")
            .is_none()
    );

    assert_eq!(
        run_panel(
            base,
            &[
                "--uninstall-panel",
                "antigravity",
                "--project",
                "alpha-service",
            ],
            false,
            &[],
        )
        .0,
        0
    );
    let expected_pretty_after_uninstall = format!(
        "{}\n",
        serde_json::to_string_pretty(
            &serde_json::from_slice::<Value>(custom_agy_user_bytes).unwrap()
        )
        .unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(&alpha_agy_cfg).unwrap(),
        expected_pretty_after_uninstall
    );
    let parsed_after_uninstall: Value =
        serde_json::from_slice(&std::fs::read(&alpha_agy_cfg).unwrap()).unwrap();
    assert_eq!(
        map_keys(&parsed_after_uninstall),
        vec!["zetaTop", "mcpServers", "alphaTail"]
    );
    assert_eq!(
        map_keys(&parsed_after_uninstall["mcpServers"]),
        vec!["sqlite-helper", "alpha-remote"]
    );
    assert_eq!(
        std::fs::symlink_metadata(&alpha_agy_cfg).unwrap().mode() & 0o7777,
        0o640
    );
    assert!(alpha_agents_dir.exists());
    assert_eq!(
        std::fs::read(&alpha_exclude).unwrap(),
        unterm_agy_exclude,
        "uninstall must restore exact original info/exclude bytes"
    );
    assert!(!panels_sidecar.exists());
    std::fs::write(&alpha_exclude, &alpha_exclude_before).unwrap();

    // When the prior file is already 2-space pretty JSON (including without `mcpServers`),
    // uninstall removes `oms-panel` and the OMS-created `mcpServers` key and is byte-exact.
    let prior_pretty_no_mcp = b"{\n  \"zetaTop\": true,\n  \"alphaTail\": 2\n}\n";
    std::fs::write(&alpha_agy_cfg, prior_pretty_no_mcp).unwrap();
    assert_eq!(
        run_panel(
            base,
            &[
                "--install-panel",
                "antigravity",
                "--project",
                "alpha-service"
            ],
            false,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &[
                "--uninstall-panel",
                "antigravity",
                "--project",
                "alpha-service",
            ],
            false,
            &[],
        )
        .0,
        0
    );
    assert_eq!(std::fs::read(&alpha_agy_cfg).unwrap(), prior_pretty_no_mcp);

    // When OMS created `mcpServers` inside a pre-existing file and `oms-panel` is already gone at
    // uninstall, uninstall removes the empty `mcpServers` key (preserving top-level key order via
    // `shift_remove`) instead of returning AlreadyGone with `{}` left behind.
    assert_eq!(
        run_panel(
            base,
            &[
                "--install-panel",
                "antigravity",
                "--project",
                "alpha-service"
            ],
            false,
            &[],
        )
        .0,
        0
    );
    std::fs::write(
        &alpha_agy_cfg,
        b"{\n  \"zetaTop\": true,\n  \"mcpServers\": {},\n  \"betaMid\": 5,\n  \"alphaTail\": 2\n}\n",
    )
    .unwrap();
    let (code, un_empty_mcp_txt, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "antigravity",
            "--project",
            "alpha-service",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        un_empty_mcp_txt.contains("uninstalled antigravity panel oms-panel for alpha-service"),
        "{un_empty_mcp_txt}"
    );
    assert_eq!(
        std::fs::read_to_string(&alpha_agy_cfg).unwrap(),
        "{\n  \"zetaTop\": true,\n  \"betaMid\": 5,\n  \"alphaTail\": 2\n}\n"
    );
    assert!(!panels_sidecar.exists());

    // 12(d): Refusal on invalid JSON, non-object root, non-object `mcpServers`, unmanaged differing
    //        `oms-panel`, non-regular file, symlinks, and Git-tracked `.agents/mcp_config.json`.
    for (label, bad_json, expected_err) in [
        (
            "invalid JSON",
            "{broken\n",
            "Invalid .agents/mcp_config.json",
        ),
        ("array root", "[]\n", "root value must be a JSON object"),
        (
            "duplicate keys",
            "{\"mcpServers\":{},\"mcpServers\":{}}\n",
            "Invalid .agents/mcp_config.json",
        ),
        (
            "mcpServers array",
            "{\"mcpServers\": []}\n",
            "mcpServers must be a JSON object",
        ),
        (
            "mcpServers null",
            "{\"mcpServers\": null}\n",
            "mcpServers must be a JSON object",
        ),
        (
            "unmanaged oms-panel",
            "{\"mcpServers\": {\"oms-panel\": {\"command\": \"custom\"}}}\n",
            "unmanaged mcpServers.oms-panel",
        ),
    ] {
        std::fs::write(&alpha_agy_cfg, bad_json.as_bytes()).unwrap();
        let (code, _, stderr) = run_panel(
            base,
            &[
                "--install-panel",
                "antigravity",
                "--project",
                "alpha-service",
            ],
            false,
            &[],
        );
        assert_ne!(code, 0, "expected refusal on {label}");
        assert!(
            stderr.contains(expected_err),
            "unexpected stderr on {label}: {stderr}"
        );
        assert_eq!(
            std::fs::read(&alpha_agy_cfg).unwrap(),
            bad_json.as_bytes(),
            "file must stay untouched on {label}"
        );
        assert_eq!(std::fs::read(&alpha_exclude).unwrap(), alpha_exclude_before);
        assert!(!panels_sidecar.exists());
    }

    // Non-regular `.agents/mcp_config.json` (directory) is refused untouched.
    std::fs::remove_file(&alpha_agy_cfg).unwrap();
    ensure_private_dir(&alpha_agy_cfg);
    let (code, _, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
        ],
        false,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("regular file"), "{stderr}");
    std::fs::remove_dir(&alpha_agy_cfg).unwrap();

    // Symlinked `.agents/mcp_config.json` and symlinked `.agents` dir are refused untouched.
    let outside_agy_json = base.join("outside-mcp_config.json");
    write_private_file(&outside_agy_json, b"{}\n");
    std::os::unix::fs::symlink(&outside_agy_json, &alpha_agy_cfg).unwrap();
    let (code, _, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
        ],
        false,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("symlink"), "{stderr}");
    assert_eq!(std::fs::read(&outside_agy_json).unwrap(), b"{}\n");
    std::fs::remove_file(&alpha_agy_cfg).unwrap();
    std::fs::remove_dir(&alpha_agents_dir).unwrap();

    let outside_agents_dir = base.join("outside-agents-dir");
    ensure_private_dir(&outside_agents_dir);
    std::os::unix::fs::symlink(&outside_agents_dir, &alpha_agents_dir).unwrap();
    let (code, _, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
        ],
        false,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("symlink"), "{stderr}");
    std::fs::remove_file(&alpha_agents_dir).unwrap();

    // Tracked `.agents/mcp_config.json` (staged and committed) is refused untouched.
    std::fs::create_dir_all(&alpha_agents_dir).unwrap();
    let tracked_agy_bytes = b"{\n  \"mcpServers\": {}\n}\n";
    std::fs::write(&alpha_agy_cfg, tracked_agy_bytes).unwrap();
    git_ok(&alpha_canon, &["add", "-f", ".agents/mcp_config.json"]);
    let (code, _, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
        ],
        false,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("tracked"), "{stderr}");
    assert_eq!(std::fs::read(&alpha_agy_cfg).unwrap(), tracked_agy_bytes);

    git_ok(&alpha_canon, &["commit", "-m", "track agy mcp_config"]);
    let (code, _, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
        ],
        false,
        &[],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("tracked"), "{stderr}");
    assert_eq!(std::fs::read(&alpha_agy_cfg).unwrap(), tracked_agy_bytes);

    git_ok(&alpha_canon, &["rm", "-q", ".agents/mcp_config.json"]);
    git_ok(&alpha_canon, &["commit", "-m", "untrack agy mcp_config"]);
    let _ = std::fs::remove_dir(&alpha_agents_dir);

    // 12(e): Adopting an exact unmanaged `oms-panel` entry in the middle (`a`, `oms-panel`, `b`, `c`)
    //        leaves compact JSON untouched, keeps the order of `a`, `oms-panel`, `b`, `c` after refresh,
    //        and keeps the order of `a`, `b`, `c` after uninstall (`shift_remove`); plus edited entry
    //        refusal/preservation, near-full sidecar rollback, two-worktree handover, and forget.
    ensure_private_dir(&alpha_agents_dir);
    let mut middle_servers = serde_json::Map::new();
    middle_servers.insert("a".into(), json!({ "command": "/bin/a" }));
    middle_servers.insert("oms-panel".into(), expected_alpha_entry.clone());
    middle_servers.insert("b".into(), json!({ "command": "/bin/b" }));
    middle_servers.insert("c".into(), json!({ "command": "/bin/c" }));
    let mut middle_root = serde_json::Map::new();
    middle_root.insert("mcpServers".into(), Value::Object(middle_servers));
    let compact_exact_bytes = serde_json::to_vec(&Value::Object(middle_root)).unwrap();
    write_private_file(&alpha_agy_cfg, &compact_exact_bytes);
    let (code, adopt_agy_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&adopt_agy_raw).unwrap(),
        json!({
            "app": "antigravity",
            "alias": "alpha-service",
            "path": alpha_canon,
            "entry": "oms-panel",
            "action": "adopted",
            "changed": true,
        })
    );
    assert_eq!(
        std::fs::read(&alpha_agy_cfg).unwrap(),
        compact_exact_bytes,
        "adopting an exact match must not rewrite .agents/mcp_config.json"
    );

    // Simulate an out-of-date `OMS_MCP_PANEL_RESOURCE_VERSION` on the middle `oms-panel` entry:
    // reinstall refreshes `oms-panel` in place while keeping `a`, `oms-panel`, `b`, `c` order,
    // and uninstall removes `oms-panel` via `shift_remove` keeping `a`, `b`, `c` order.
    let older_alpha_entry = render_expected_agy_entry(&alpha_canon, "v0-older");
    let older_alpha_entry_sha = canonical_entry_sha(&older_alpha_entry);
    let mut older_middle_doc: Value = serde_json::from_slice(&compact_exact_bytes).unwrap();
    older_middle_doc["mcpServers"]["oms-panel"] = older_alpha_entry;
    assert_eq!(
        map_keys(&older_middle_doc["mcpServers"]),
        vec!["a", "oms-panel", "b", "c"]
    );
    write_private_file(
        &alpha_agy_cfg,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&older_middle_doc).unwrap()
        )
        .as_bytes(),
    );
    let mut sidecar_for_refresh: Value =
        serde_json::from_slice(&std::fs::read(&panels_sidecar).unwrap()).unwrap();
    sidecar_for_refresh["alpha-service"][0]["entry_sha256"] = json!(older_alpha_entry_sha);
    write_private_file(
        &panels_sidecar,
        serde_json::to_string_pretty(&sidecar_for_refresh)
            .unwrap()
            .as_bytes(),
    );

    let (code, refreshed_raw, stderr) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&refreshed_raw).unwrap()["changed"],
        true
    );
    let after_refresh_doc: Value =
        serde_json::from_slice(&std::fs::read(&alpha_agy_cfg).unwrap()).unwrap();
    assert_eq!(
        map_keys(&after_refresh_doc["mcpServers"]),
        vec!["a", "oms-panel", "b", "c"]
    );
    assert_eq!(
        after_refresh_doc["mcpServers"]["oms-panel"],
        expected_alpha_entry
    );

    assert_eq!(
        run_panel(
            base,
            &[
                "--uninstall-panel",
                "antigravity",
                "--project",
                "alpha-service",
            ],
            false,
            &[],
        )
        .0,
        0
    );
    let after_middle_uninstall: Value =
        serde_json::from_slice(&std::fs::read(&alpha_agy_cfg).unwrap()).unwrap();
    assert_eq!(
        map_keys(&after_middle_uninstall["mcpServers"]),
        vec!["a", "b", "c"]
    );
    assert!(!panels_sidecar.exists());
    std::fs::write(&alpha_exclude, &alpha_exclude_before).unwrap();

    // Edit `oms-panel` outside OMS: reinstall refuses untouched, and uninstall preserves + reports.
    write_private_file(&alpha_agy_cfg, &compact_exact_bytes);
    assert_eq!(
        run_panel(
            base,
            &[
                "--install-panel",
                "antigravity",
                "--project",
                "alpha-service",
            ],
            false,
            &[],
        )
        .0,
        0
    );
    let mut edited_agy_doc: Value = serde_json::from_slice(&compact_exact_bytes).unwrap();
    edited_agy_doc["mcpServers"]["oms-panel"]["env"]["CUSTOM_EDIT"] = json!("1");
    let edited_agy_bytes = serde_json::to_vec_pretty(&edited_agy_doc).unwrap();
    write_private_file(&alpha_agy_cfg, &edited_agy_bytes);

    let (code, _, ed_agy_err) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
        ],
        false,
        &[],
    );
    assert_ne!(code, 0);
    assert!(ed_agy_err.contains("modified outside OMS"), "{ed_agy_err}");
    assert_eq!(std::fs::read(&alpha_agy_cfg).unwrap(), edited_agy_bytes);

    let (code, un_ed_agy_raw, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "antigravity",
            "--project",
            "alpha-service",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let un_ed_agy_json: Value = serde_json::from_str(&un_ed_agy_raw).unwrap();
    assert_eq!(un_ed_agy_json["changed"], true);
    assert_eq!(un_ed_agy_json["preservedFiles"], json!([alpha_agy_cfg]));
    assert_eq!(std::fs::read(&alpha_agy_cfg).unwrap(), edited_agy_bytes);
    assert!(!panels_sidecar.exists());

    std::fs::remove_file(&alpha_agy_cfg).unwrap();
    std::fs::remove_dir(&alpha_agents_dir).unwrap();
    std::fs::write(&alpha_exclude, &alpha_exclude_before).unwrap();

    // Near-full sidecar rollback restores missing `.agents` dir, file, and exclude line.
    write_private_file(&panels_sidecar, &near_full_bytes);
    let (code, _, rb_agy_err) = run_panel(
        base,
        &[
            "--install-panel",
            "antigravity",
            "--project",
            "alpha-service",
        ],
        false,
        &[],
    );
    assert_ne!(code, 0);
    assert!(rb_agy_err.contains("rolled back"), "{rb_agy_err}");
    assert!(!alpha_agy_cfg.exists() && !alpha_agents_dir.exists());
    assert_eq!(std::fs::read(&alpha_exclude).unwrap(), alpha_exclude_before);
    std::fs::remove_file(&panels_sidecar).unwrap();

    // Two linked worktrees (`alpha-wt` and `alpha-wt-b`): handover and last-owner cleanup of
    // `.agents/mcp_config.json`, `.agents` dir, and shared `info/exclude` line + separator.
    std::fs::write(&alpha_exclude, shared_excl_orig).unwrap();
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "antigravity", "--project", "alpha-wt"],
            false,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--install-panel", "antigravity", "--project", "alpha-wt-b"],
            false,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &["--uninstall-panel", "antigravity", "--project", "alpha-wt"],
            false,
            &[],
        )
        .0,
        0
    );
    assert!(!linked_canon.join(".agents").exists());
    assert_eq!(git_ok(&linked_b_canon, &["status", "--porcelain"]), "");
    assert_eq!(
        run_panel(
            base,
            &[
                "--uninstall-panel",
                "antigravity",
                "--project",
                "alpha-wt-b"
            ],
            false,
            &[],
        )
        .0,
        0
    );
    assert!(!linked_b_canon.join(".agents").exists());
    assert!(!panels_sidecar.exists());
    assert_eq!(std::fs::read(&alpha_exclude).unwrap(), shared_excl_orig);
    std::fs::write(&alpha_exclude, &alpha_exclude_before).unwrap();

    // Deleted checkout is forgotten cleanly (`action: "forgotten"`).
    let ephemeral_agy = base.join("checkouts/ephemeral-agy");
    std::fs::create_dir_all(ephemeral_agy.join(".oms")).unwrap();
    let ephemeral_agy_canon = ephemeral_agy.canonicalize().unwrap();
    assert_eq!(
        run_panel(
            &ephemeral_agy_canon,
            &["--register-project", "--alias", "EphemeralAgy"],
            false,
            &[],
        )
        .0,
        0
    );
    assert_eq!(
        run_panel(
            base,
            &[
                "--install-panel",
                "antigravity",
                "--project",
                "EphemeralAgy"
            ],
            false,
            &[],
        )
        .0,
        0
    );
    std::fs::remove_dir_all(&ephemeral_agy_canon).unwrap();
    let (code, forget_agy_raw, stderr) = run_panel(
        base,
        &[
            "--uninstall-panel",
            "antigravity",
            "--project",
            "EphemeralAgy",
            "--json",
        ],
        false,
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    let forget_agy_json: Value = serde_json::from_str(&forget_agy_raw).unwrap();
    assert_eq!(forget_agy_json["action"], "forgotten");
    assert_eq!(forget_agy_json["changed"], true);
    assert_eq!(
        run_panel(base, &["--unregister-project", "EphemeralAgy"], false, &[]).0,
        0
    );
    assert!(!gemini_dir.exists(), "OMS must never touch ~/.gemini");

    // 13. Verify temp HOME/.claude.json, HOME/.codex/config.toml, and CODEX_HOME/config.toml
    //     remained completely untouched by OMS.
    assert_eq!(
        std::fs::symlink_metadata(&home_claude_json).unwrap().ino(),
        home_claude_ino
    );
    assert_eq!(
        std::fs::symlink_metadata(&home_codex_cfg).unwrap().ino(),
        home_codex_ino
    );
    assert_eq!(
        std::fs::symlink_metadata(&codex_home_cfg).unwrap().ino(),
        codex_home_ino
    );
    assert_eq!(
        std::fs::read(&codex_home_cfg).unwrap(),
        expected_codex_home_bytes
    );
}

fn main() {
    if let Some(ready_path) = std::env::var_os("OMS_PANEL_PARITY_SLEEP_CHILD") {
        use std::io::Write;
        let _ = std::fs::write(Path::new(&ready_path), std::process::id().to_string());
        let _ = std::io::stderr().write_all(b"grandchild holding stderr open\n");
        let _ = std::io::stderr().flush();
        std::thread::sleep(std::time::Duration::from_secs(30));
        std::process::exit(0);
    }
    if let Some(state_dir) = std::env::var_os("OMS_PANEL_PARITY_CLAUDE_STUB")
        && std::env::args().next().is_some_and(|arg0| {
            Path::new(&arg0)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.contains("claude"))
        })
    {
        stub_claude_main(Path::new(&state_dir));
    }
    if std::env::var_os("OMS_PANEL_PARITY_CLI").is_some() {
        use std::io::Write;
        if let Some(trigger) = std::env::var_os("OMS_PANEL_PARITY_INTERRUPT_ON_FILE") {
            let trigger_path = PathBuf::from(trigger);
            std::thread::spawn(move || {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while std::time::Instant::now() < until {
                    if trigger_path.exists() {
                        STOP.store(true, Ordering::Release);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            });
        }
        let cwd = std::env::current_dir().unwrap();
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        let services = Fake::default();
        let (code, out, err) = run_with_services("panel", &args, &cwd, &cwd, &STOP, &services);
        std::io::stdout().write_all(&out).unwrap();
        std::io::stderr().write_all(&err).unwrap();
        std::process::exit(code);
    }
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
        ensure_private_dir(&base.join("locks"));
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
            .env("TMUX_PANE", "%9")
            .env("OMS_PANEL_PARITY_ROOT", &base)
            .env("TMPDIR", &base)
            .env("XDG_STATE_HOME", &state)
            .env("OMS_LOCK_DIR", base.join("locks"))
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
    let cases: [(&str, fn()); 15] = [
        (
            "chats_native_identity_binding",
            chats_native_identity_binding,
        ),
        (
            "desktop_projects_registry_and_reader_parity",
            desktop_projects_registry_and_reader_parity,
        ),
        (
            "project_panels_claude_lifecycle_parity",
            project_panels_claude_lifecycle_parity,
        ),
        (
            "project_panels_codex_lifecycle_parity",
            project_panels_codex_lifecycle_parity,
        ),
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
            "catalog_full_models_are_used_for_routing",
            catalog_full_models_are_used_for_routing,
        ),
        (
            "routing_accepts_present_provider_catalog",
            routing_accepts_present_provider_catalog,
        ),
        (
            "teams_are_read_only_and_keep_unknown_evidence_and_stable_ids",
            teams_are_read_only_and_keep_unknown_evidence_and_stable_ids,
        ),
        (
            "account_options_and_target_conflicts",
            account_options_and_target_conflicts,
        ),
        (
            "spread_routes_by_panel_room_id",
            spread_routes_by_panel_room_id,
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
