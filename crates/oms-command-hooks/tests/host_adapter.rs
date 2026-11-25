#![forbid(unsafe_code)]

use oms_command_hooks::{HostServices, run_hook, run_with_services};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("oms-hooks-{prefix}-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("mkdir temp");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("chmod temp");
        let path = fs::canonicalize(&path).expect("canonicalize temp");
        Self(path)
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

#[derive(Default)]
struct RecordingHost {
    detached: Mutex<Vec<(String, Vec<String>, PathBuf)>>,
    failures: Mutex<Vec<(i32, String)>>,
}

impl RecordingHost {
    fn detached(&self) -> Vec<(String, Vec<String>, PathBuf)> {
        self.detached.lock().expect("lock detached").clone()
    }
}

impl HostServices for RecordingHost {
    fn supports_detach(&self) -> bool {
        true
    }

    fn detach_command(&self, tool: &str, args: &[OsString], cwd: &Path) -> Result<(), String> {
        self.detached.lock().expect("lock detached").push((
            tool.to_owned(),
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            cwd.to_path_buf(),
        ));
        Ok(())
    }

    fn record_hook_failure(&self, _repo: &Path, _cmd: &str, exit: i32, summary: &str, _next: &str) {
        self.failures.lock().unwrap().push((exit, summary.into()));
    }
}

/// A git repository below the temp dir: root discovery stops here even when an
/// ancestor of the system temp dir (for example /tmp) is itself a git checkout.
fn git_repo(tmp: &TempDir) -> PathBuf {
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).expect("mkdir repo");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .expect("git init")
            .success()
    );
    repo
}

fn argv(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

#[test]
fn codex_usage_config_installs_checks_and_removes_native_notifier_without_python() {
    let tmp = TempDir::new("config");
    let cfg = tmp.path().join("config.toml");
    let native_bin = tmp.path().join("bin/oms");
    let cancel = AtomicBool::new(false);

    let (code, stdout, stderr) = run_hook(
        "codex-usage-config",
        &argv(&[
            "install",
            cfg.to_str().unwrap(),
            "--notify-script",
            native_bin.to_str().unwrap(),
        ]),
        &[],
        tmp.path(),
        tmp.path(),
        &cancel,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let out = String::from_utf8_lossy(&stdout);
    assert!(out.contains("installed"), "stdout: {out}");

    let written = fs::read_to_string(&cfg).expect("read config.toml");
    let expected_line = format!(
        "notify = [\"{}\",\"codex-turn-notify\"]",
        native_bin.display()
    );
    assert!(
        written.contains(&expected_line),
        "expected native notify entry in config.toml:\n{written}"
    );

    let (chk_code, _, chk_err) = run_hook(
        "codex-usage-config",
        &argv(&["check", cfg.to_str().unwrap()]),
        &[],
        tmp.path(),
        tmp.path(),
        &cancel,
    );
    assert_eq!(
        chk_code,
        0,
        "check stderr: {}",
        String::from_utf8_lossy(&chk_err)
    );

    let (rm_code, rm_out, rm_err) = run_hook(
        "codex-usage-config",
        &argv(&["remove", cfg.to_str().unwrap()]),
        &[],
        tmp.path(),
        tmp.path(),
        &cancel,
    );
    assert_eq!(
        rm_code,
        0,
        "remove stderr: {}",
        String::from_utf8_lossy(&rm_err)
    );
    assert!(
        !fs::read_to_string(&cfg)
            .expect("read removed")
            .contains("codex-turn-notify"),
        "expected notify block removed: {}",
        String::from_utf8_lossy(&rm_out)
    );
}

/// Runs the named test in a child with every inherited OMS_* variable removed
/// plus `extra`, so a live panel shell cannot change the outcome. Returns true
/// inside the child, where the caller runs its body.
fn clean_env_child(name: &str, extra: &[(&str, &str)]) -> bool {
    if std::env::var_os("OMS_TEST_CLEAN_CHILD").is_some() {
        return true;
    }
    let mut cmd = Command::new(std::env::current_exe().expect("test exe"));
    cmd.args(["--exact", name, "--test-threads=1", "--nocapture"]);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("OMS_") {
            cmd.env_remove(k);
        }
    }
    // Never let a test resolve or post to the developer's live Claude session.
    cmd.env_remove("CLAUDE_CODE_MESSAGING_SOCKET");
    cmd.env(
        "CLAUDE_CONFIG_DIR",
        std::env::temp_dir().join("oms-hooks-no-claude"),
    );
    cmd.env("OMS_TEST_CLEAN_CHILD", "1");
    cmd.envs(extra.iter().copied());
    let out = cmd.output().expect("spawn clean child");
    assert!(
        out.status.success(),
        "{name}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

#[test]
fn skill_router_detaches_context_pressure_handoff_capture_via_host_services() {
    if !clean_env_child(
        "skill_router_detaches_context_pressure_handoff_capture_via_host_services",
        &[],
    ) {
        return;
    }
    let tmp = TempDir::new("router");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join(".oms")).expect("mkdir .oms");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .expect("git init")
            .success()
    );
    let transcript = tmp.path().join("rollout.jsonl");
    fs::write(
        &transcript,
        "{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"total_tokens\":88000},\"model_context_window\":100000}}\n",
    )
    .expect("write rollout");
    strip_group_world_write(tmp.path());

    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": repo.to_str().unwrap(),
        "session_id": "sess-router-1",
        "prompt": "inspect src/lib.rs",
        "transcript_path": transcript.to_str().unwrap(),
    });
    let host = RecordingHost::default();
    let cancel = AtomicBool::new(false);
    for (used, severity, captures) in [
        (88000, Some("context low"), 1),
        (88000, None, 1),
        (94000, Some("context nearly exhausted"), 2),
        (94000, None, 2),
        (20000, None, 2),
        (88000, Some("context low"), 3),
    ] {
        let usage = serde_json::json!({
            "type": "token_count",
            "info": {
                "last_token_usage": {"total_tokens": used},
                "model_context_window": 100000,
            },
        });
        fs::write(&transcript, format!("{usage}\n")).expect("update rollout");
        let (code, stdout, stderr) = run_with_services(
            "skill-router",
            &[],
            payload.to_string().as_bytes(),
            &repo,
            tmp.path(),
            &cancel,
            &host,
        );
        assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
        let out = String::from_utf8_lossy(&stdout);
        if let Some(severity) = severity {
            assert!(out.contains(severity), "stdout: {out}");
            assert!(out.contains("native in-session compaction"), "{out}");
            assert!(out.contains("unavailable or fails"), "{out}");
            assert!(out.contains("oms session-handoff list"), "{out}");
            assert!(!out.contains("migrate to a fresh session"), "{out}");
            assert!(!out.contains("continue in a fresh session"), "{out}");
            assert!(!out.contains("wrap up now"), "{out}");
        } else {
            assert!(!out.contains("native in-session compaction"), "{out}");
        }
        assert_eq!(host.detached().len(), captures);
    }

    let calls = host.detached();
    for (tool, args, cwd) in calls {
        assert_eq!(tool, "session-handoff");
        assert_eq!(args[0], "capture");
        assert!(args.contains(&"--agent".to_owned()));
        assert!(args.contains(&"codex".to_owned()));
        assert!(args.contains(&"sess-router-1".to_owned()));
        assert_eq!(cwd, repo);
    }
}

fn hook(tmp: &TempDir, name: &str, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
    // umask 002 hosts create group-writable state dirs that the guards refuse.
    strip_group_world_write(tmp.path());
    let (code, o, e) = run_hook(
        name,
        &argv(args),
        stdin,
        tmp.path(),
        tmp.path(),
        &AtomicBool::new(false),
    );
    (
        code,
        String::from_utf8_lossy(&o).into_owned(),
        String::from_utf8_lossy(&e).into_owned(),
    )
}

#[test]
fn claude_app_notify_requires_explicit_wakeup_and_names_no_python_script() {
    if !clean_env_child(
        "claude_app_notify_requires_explicit_wakeup_and_names_no_python_script",
        &[],
    ) {
        return;
    }
    let tmp = TempDir::new("claude-notify");
    for args in [
        &[][..],
        &["send"],
        &["send", "repo"],
        &["send", "a", "b", "c"],
        &["send", "--consumer"],
        &["post", "repo", "text"],
    ] {
        let (code, out, err) = hook(&tmp, "claude-app-notify", args, &[]);
        assert_eq!(code, 2, "{args:?}");
        assert!(out.is_empty());
        assert!(
            err.starts_with("usage: claude-app-notify send") && !err.contains(".py"),
            "{err}"
        );
    }
    let (code, out, _) = hook(&tmp, "claude-app-notify", &["send", ".", "hello"], &[]);
    assert_eq!(code, 0);
    let row: serde_json::Value = serde_json::from_str(&out).expect("receipt json");
    assert_eq!(row["status"], "wakeup_required");
    assert_eq!(row["submitted"], false);
    assert_eq!(row["app"], "claude");
    let bad = "not-a-consumer";
    let args = ["send", "--wakeup", "--consumer", bad, ".", "hello"];
    let (code, out, _) = hook(&tmp, "claude-app-notify", &args, &[]);
    assert_eq!(code, 0);
    let row: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(row["status"], "unbound");
    assert_eq!(row["submitted"], false);
}

#[test]
fn codex_app_notify_usage_is_native() {
    let tmp = TempDir::new("codex-notify");
    let (code, _, err) = hook(&tmp, "codex-app-notify", &[], &[]);
    assert_eq!(code, 2);
    assert_eq!(err, "usage: codex-app-notify send REPO TEXT\n");
}

#[test]
fn surface_rows_are_native_and_legacy_rows_migrate_to_them() {
    use oms_command_hooks::surfaces;
    assert_eq!(
        surfaces::hook_command("syntax-guard-hook.sh"),
        "oms syntax-guard-hook"
    );
    let tmp = TempDir::new("surfaces");
    let root = tmp.path().join("oms-root");
    fs::create_dir_all(root.join("scripts/lib")).unwrap();
    let legacy = format!(
        "bash {0}/scripts/lib/python-entry.sh bash {0}/scripts/resume-hook.sh",
        root.display()
    );
    let roots = vec![root.clone()];
    let owned = |c: &str| {
        surfaces::owned_script(&serde_json::json!({"type":"command","command":c}), &roots)
    };
    assert_eq!(owned(&legacy).as_deref(), Some("resume-hook.sh"));
    assert_eq!(owned("oms resume-hook").as_deref(), Some("resume-hook.sh"));
    assert_eq!(owned("oms resume-hook --x"), None);
    assert_eq!(owned("oms unknown-hook"), None);
    let mut hooks =
        serde_json::json!({"SessionStart":[{"hooks":[{"type":"command","command":legacy}]}]});
    let surfaces_cfg = [
        serde_json::json!({"event":"SessionStart","script":"resume-hook.sh","matcher":null,"timeout":null}),
    ];
    surfaces::converge(&mut hooks, &surfaces_cfg, &roots).unwrap();
    assert_eq!(
        hooks["SessionStart"][0]["hooks"][0]["command"],
        "oms resume-hook"
    );
    assert!(!hooks.to_string().contains("python-entry"));
}

#[test]
fn syntax_guard_reports_json_errors_without_an_interpreter() {
    if !clean_env_child(
        "syntax_guard_reports_json_errors_without_an_interpreter",
        &[],
    ) {
        return;
    }
    let tmp = TempDir::new("syntax");
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms")).unwrap();
    let bad = repo.join("bad.json");
    fs::write(&bad, "{").unwrap();
    let good = repo.join("good.json");
    fs::write(&good, "{}").unwrap();
    let payload = |p: &Path| {
        serde_json::json!({"tool_name":"Write","cwd":repo,"tool_input":{"file_path":p}}).to_string()
    };
    let (code, out, _) = hook(&tmp, "syntax-guard-hook", &[], payload(&bad).as_bytes());
    assert_eq!(code, 0);
    assert!(
        out.contains("bad.json does not parse") && out.contains("json:"),
        "{out}"
    );
    let (code, out, _) = hook(&tmp, "syntax-guard-hook", &[], payload(&good).as_bytes());
    assert_eq!((code, out.as_str()), (0, ""));
}

#[test]
fn main_write_guard_refuses_managed_repository_edits_from_a_panel_main_only() {
    const NAME: &str = "main_write_guard_refuses_managed_repository_edits_from_a_panel_main_only";
    const MAIN: [(&str, &str); 2] = [
        ("OMS_PANEL_MAIN_ATTEMPT", "att-main-write"),
        ("OMS_PANEL_SESSION", "oms-main-write"),
    ];
    if std::env::var_os("OMS_TEST_CLEAN_CHILD").is_none() {
        clean_env_child(NAME, &MAIN);
        clean_env_child(NAME, &[MAIN[0], MAIN[1], ("OMS_ROOM_REPO", "repo")]);
        clean_env_child(NAME, &[MAIN[0], MAIN[1], ("OMS_MAIN_WRITE_GUARD", "0")]);
        clean_env_child(NAME, &[MAIN[0], MAIN[1], ("OMS_HARNESS_CHILD", "1")]);
        clean_env_child(
            NAME,
            &[MAIN[0], MAIN[1], ("OMS_HARNESS_DELEGATE_DEPTH", "1")],
        );
        clean_env_child(NAME, &[MAIN[0], MAIN[1], ("OMS_PANEL_DISPATCH", "1")]);
        clean_env_child(NAME, &[]);
        return;
    }
    let set = |name: &str| std::env::var(name).unwrap_or_default();
    let enforced = !set("OMS_PANEL_SESSION").is_empty()
        && set("OMS_PANEL_DISPATCH") != "1"
        && set("OMS_MAIN_WRITE_GUARD") != "0"
        && set("OMS_HARNESS_CHILD") != "1"
        && matches!(set("OMS_HARNESS_DELEGATE_DEPTH").as_str(), "" | "0");
    let tmp = TempDir::new("main-write");
    let repo = git_repo(&tmp);
    let git = |dir: &Path, args: &[&str]| {
        let identity = ["-c", "user.name=t", "-c", "user.email=t@example.invalid"];
        assert!(
            Command::new("git")
                .args(identity)
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(dir)
                .status()
                .expect("git")
                .success()
        );
    };
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(
        &repo,
        &["worktree", "add", "-q", "../linked", "-b", "linked"],
    );
    let submodule = repo.join("sub");
    fs::create_dir_all(&submodule).unwrap();
    let module_git = repo.join(".git/modules/sub");
    fs::create_dir_all(module_git.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            module_git.to_str().unwrap(),
            submodule.to_str().unwrap(),
        ],
    );
    let sub_dir = repo.join("sub_dir");
    fs::create_dir_all(&sub_dir).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let link_dir = outside.join("link_dir");
    std::os::unix::fs::symlink(&sub_dir, &link_dir).unwrap();
    let dangling = outside.join("dangling.rs");
    std::os::unix::fs::symlink(repo.join("does-not-exist.rs"), &dangling).unwrap();
    let other_repo = tmp.path().join("other");
    fs::create_dir_all(&other_repo).unwrap();
    git(&other_repo, &["init", "-q"]);

    let run_raw = |raw: &[u8]| hook(&tmp, "main-write-guard-hook", &[], raw);
    let run_in = |cwd: &Path, tool: &str, key: &str, path: &Path| {
        let payload =
            serde_json::json!({"tool_name":tool,"cwd":cwd,"tool_input":{key:path}}).to_string();
        run_raw(payload.as_bytes())
    };
    let assert_refused_if_enforced = |label: &str, (code, out, err): (i32, String, String)| {
        if enforced {
            assert_eq!((code, out.as_str()), (2, ""), "{label}");
            assert!(
                err.starts_with("main write guard: ") && err.contains("--dispatch"),
                "{label}: {err}"
            );
        } else {
            assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""), "{label}");
        }
    };

    for (label, tool, key, path) in [
        (
            "Edit absolute",
            "Edit",
            "file_path",
            repo.join("src/new.rs"),
        ),
        (
            "Write relative missing nested",
            "Write",
            "file_path",
            PathBuf::from("nested/missing/new.rs"),
        ),
        (
            "MultiEdit absolute",
            "MultiEdit",
            "file_path",
            repo.join("src/multi.rs"),
        ),
        (
            "NotebookEdit linked worktree",
            "NotebookEdit",
            "notebook_path",
            tmp.path().join("linked/a.ipynb"),
        ),
        (
            "Edit submodule",
            "Edit",
            "file_path",
            submodule.join("lib.rs"),
        ),
        (
            "Write external symlink",
            "Write",
            "file_path",
            link_dir.join("inside.rs"),
        ),
        (
            "Edit external symlink parent traversal",
            "Edit",
            "file_path",
            link_dir.join("../escaped.rs"),
        ),
        (
            "Write dangling external symlink",
            "Write",
            "file_path",
            dangling,
        ),
    ] {
        assert_refused_if_enforced(label, run_in(&repo, tool, key, &path));
    }

    if !set("OMS_ROOM_REPO").is_empty() {
        let outside_cwd = std::env::temp_dir();
        assert_refused_if_enforced(
            "OMS_ROOM_REPO with outside payload cwd",
            run_in(&outside_cwd, "Edit", "file_path", &repo.join("src/room.rs")),
        );
        assert_eq!(
            run_in(
                &outside_cwd,
                "Write",
                "file_path",
                &tmp.path().join("notes.md")
            ),
            (0, String::new(), String::new()),
        );
    }

    for payload in [
        serde_json::json!({"tool_name":"Edit","cwd":repo,"tool_input":{"file_path":""}}),
        serde_json::json!({"tool_name":"Edit","cwd":repo,"tool_input":{}}),
        serde_json::json!({"tool_name":"Edit","cwd":repo,"tool_input":{"file_path":123}}),
    ] {
        assert_eq!(
            run_raw(payload.to_string().as_bytes()),
            (0, String::new(), String::new()),
            "{payload}"
        );
    }

    let (code, out, err) = run_raw(b"{");
    assert_eq!((code, out.as_str()), (0, ""));
    if enforced {
        assert!(
            err.contains("main write guard: cannot evaluate this edit"),
            "{err}"
        );
    } else {
        assert_eq!(err, "");
    }

    for (label, path) in [
        ("outside file", tmp.path().join("notes.md")),
        ("unrelated git repo", other_repo.join("src/a.rs")),
    ] {
        assert_eq!(
            run_in(&repo, "Write", "file_path", &path),
            (0, String::new(), String::new()),
            "{label}"
        );
    }
}

fn harness(tmp: &TempDir, tool: &str, payload: &serde_json::Value) -> (i32, String) {
    let (code, out, _) = hook(tmp, "harness-hook", &[tool], payload.to_string().as_bytes());
    (code, out)
}

fn codex_handoff_rollout(path: &Path, repo: &Path, sid: &str) {
    use serde_json::json;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let rows = [
        json!({"type":"session_meta","payload":{"id":sid,"cwd":repo}}),
        json!({"type":"event_msg","payload":{"type":"user_message","message":"first exact user turn"}}),
        json!({"type":"event_msg","payload":{"type":"user_message","message":"second exact user turn"}}),
    ];
    let body = rows
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, format!("{body}\n")).unwrap();
}

#[test]
fn precompact_handoff_keeps_dissent_until_valid_resolution() {
    use serde_json::{Value, json};

    let tmp = TempDir::new("handoff-resolution");
    let home = tmp.path().join("home");
    let active = home.join("codex");
    if !clean_env_child(
        "precompact_handoff_keeps_dissent_until_valid_resolution",
        &[
            ("HOME", home.to_str().unwrap()),
            ("XDG_CONFIG_HOME", ""),
            ("CODEX_HOME", active.to_str().unwrap()),
            ("OMS_WORK_JOURNAL", "0"),
            ("OMS_PRECOMPACT_HANDOFF", "1"),
        ],
    ) {
        return;
    }
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms/artifacts")).unwrap();
    let active = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let sid = "resolution-session";
    let transcript = active.join("sessions/rollout-resolution.jsonl");
    codex_handoff_rollout(&transcript, &repo, sid);
    let input = json!({
        "hook_event_name": "PreCompact", "cwd": repo, "session_id": sid,
        "transcript_path": transcript
    });
    let target = json!({
        "schema":1, "event_id":"split", "operation_id":"op", "artifact_id":"art",
        "ts":"2026-01-01T00:00:00Z", "kind":"review-outcome", "provider":"codex", "exit":1,
        "review":{"overall":1,"seats":[
            {"provider":"codex","verdict":"pass"},
            {"provider":"claude","verdict":"fail"}
        ]}
    });
    let resolver = json!({
        "schema":1, "event_id":"settled", "operation_id":"op", "artifact_id":"art",
        "ts":"2026-01-01T00:00:01Z", "kind":"artifact-resolution", "provider":"codex",
        "resolves_event_id":"split",
        "parent_event_id":"split", "resolution":"resolved", "exit":0
    });
    let changed = |key: &str, value: Value| {
        let mut row = resolver.clone();
        row[key] = value;
        vec![target.clone(), row]
    };
    let missing = |key: &str| {
        let mut target = target.clone();
        let mut resolver = resolver.clone();
        target.as_object_mut().unwrap().remove(key);
        resolver.as_object_mut().unwrap().remove(key);
        vec![target, resolver]
    };
    let cases = [
        ("valid", vec![target.clone(), resolver.clone()], false),
        (
            "malformed",
            vec![
                target.clone(),
                json!({"kind":"artifact-resolution","resolves_event_id":"split"}),
            ],
            true,
        ),
        ("schema", changed("schema", json!(true)), true),
        ("parent", changed("parent_event_id", json!("other")), true),
        ("operation", changed("operation_id", json!("other")), true),
        ("artifact", changed("artifact_id", json!("other")), true),
        ("exit", changed("exit", json!(1)), true),
        ("status", changed("resolution", json!("pending")), true),
        ("missing-operation", missing("operation_id"), true),
        ("missing-artifact", missing("artifact_id"), true),
        ("missing-provider", missing("provider"), true),
        ("missing-timestamp", missing("ts"), true),
        ("out-of-order", vec![resolver.clone(), target.clone()], true),
        (
            "duplicate-target",
            vec![target.clone(), resolver.clone(), target.clone()],
            true,
        ),
        (
            "duplicate-resolver",
            vec![target.clone(), resolver.clone(), resolver.clone()],
            true,
        ),
        (
            "invalid-duplicate",
            vec![target, resolver, json!({"event_id":"settled"})],
            true,
        ),
    ];
    for (name, rows, open) in cases {
        let bytes = rows
            .iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>();
        let index = repo.join(".oms/artifacts/index.jsonl");
        fs::write(&index, &bytes).unwrap();
        let result = hook(
            &tmp,
            "harness-hook",
            &["precompact-handoff", "codex"],
            input.to_string().as_bytes(),
        );
        assert_eq!(result, (0, String::new(), String::new()), "{name}");
        let captures = fs::read_dir(repo.join(".oms/handoffs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(captures.len(), 1, "{name}");
        let capture = fs::read_to_string(&captures[0]).unwrap();
        assert_eq!(
            capture.contains("## Open dissents"),
            open,
            "{name}: {capture}"
        );
        if open {
            assert!(capture.contains("event: split"), "{name}: {capture}");
        }
        assert_eq!(fs::read(&index).unwrap(), bytes.as_bytes(), "{name}");
        // Remove the capture so the next case is not the 15-minute debounce.
        fs::remove_file(&captures[0]).unwrap();
    }
}

#[test]
fn codex_precompact_captures_exact_transcript_beyond_discovery_limit() {
    use serde_json::json;

    let tmp = TempDir::new("precompact");
    let home = tmp.path().join("home");
    let active = home.join("active-codex");
    if !clean_env_child(
        "codex_precompact_captures_exact_transcript_beyond_discovery_limit",
        &[
            ("HOME", home.to_str().unwrap()),
            ("XDG_CONFIG_HOME", ""),
            ("CODEX_HOME", active.to_str().unwrap()),
            ("OMS_WORK_JOURNAL", "0"),
        ],
    ) {
        return;
    }
    let repo = git_repo(&tmp);
    fs::create_dir(repo.join(".oms")).unwrap();
    let active = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let sessions = active.join("sessions");
    let sid = "precompact-exact-session";
    let transcript = sessions.join("2026/11/18/rollout-exact.jsonl");
    codex_handoff_rollout(&transcript, &repo, sid);
    for i in 0..8192 {
        fs::write(sessions.join(format!("sibling-{i}.txt")), b"").unwrap();
    }
    let standalone = oms_command_peer::run_with_source(
        "session-handoff",
        &argv(&["capture", "--agent", "codex", "--session", sid]),
        &repo,
        tmp.path(),
        &AtomicBool::new(false),
    );
    assert_eq!(
        standalone,
        (
            2,
            Vec::new(),
            b"error: session discovery incomplete\n".to_vec()
        )
    );
    assert!(!repo.join(".oms/handoffs").exists());

    let registry = active
        .parent()
        .unwrap()
        .join(".config/oh-my-setting/provider-accounts.json");
    fs::create_dir_all(registry.parent().unwrap()).unwrap();
    fs::write(&registry, b"invalid account registry").unwrap();
    let input = json!({
        "hook_event_name": "PreCompact", "cwd": repo, "session_id": sid,
        "transcript_path": transcript
    });
    let (code, stdout, stderr) = hook(
        &tmp,
        "harness-hook",
        &["precompact-handoff", "codex"],
        input.to_string().as_bytes(),
    );
    assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "", ""));
    let captures: Vec<_> = fs::read_dir(repo.join(".oms/handoffs"))
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        captures.len(),
        1,
        "exact callback should capture in a large account"
    );
    let capture = fs::read_to_string(&captures[0]).unwrap();
    assert!(capture.contains(&format!("- session: {sid}")));
    assert!(capture.contains("second exact user turn"));
}

#[test]
fn codex_precompact_rejects_explicit_unsafe_hints_without_fallback() {
    use serde_json::{Value, json};
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new("precompact-reject");
    let home = tmp.path().join("home");
    let active = home.join("active-codex");
    let discovery_only = home.join("discovery-only-codex");
    if !clean_env_child(
        "codex_precompact_rejects_explicit_unsafe_hints_without_fallback",
        &[
            ("HOME", home.to_str().unwrap()),
            ("XDG_CONFIG_HOME", ""),
            ("CODEX_HOME", active.to_str().unwrap()),
            ("OMS_CODEX_HOME", discovery_only.to_str().unwrap()),
            ("OMS_WORK_JOURNAL", "0"),
        ],
    ) {
        return;
    }
    let repo = git_repo(&tmp);
    fs::create_dir(repo.join(".oms")).unwrap();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let active = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let sessions = active.join("sessions");
    let sid = "precompact-exact-session";
    let fallback = sessions.join("day/rollout-exact.jsonl");
    codex_handoff_rollout(&fallback, &repo, sid);
    let wrong_repo = sessions.join("foreign-repo.jsonl");
    codex_handoff_rollout(&wrong_repo, tmp.path(), sid);
    let wrong_session = sessions.join("foreign-session.jsonl");
    codex_handoff_rollout(&wrong_session, &repo, "another-session");
    let other_account = home.join("discovery-only-codex/sessions/other-account.jsonl");
    codex_handoff_rollout(&other_account, &repo, sid);
    let default_account = home.join(".codex/sessions/default-account.jsonl");
    codex_handoff_rollout(&default_account, &repo, sid);
    let registry = home.join(".config/oh-my-setting/provider-accounts.json");
    fs::create_dir_all(registry.parent().unwrap()).unwrap();
    fs::write(
        &registry,
        json!({"schema":1,"providers":{"codex":{"accounts":[{
            "id":"other-account","home":home.join("discovery-only-codex")
        }]}}})
        .to_string(),
    )
    .unwrap();
    let archive = active.join("archived_sessions/old.jsonl");
    codex_handoff_rollout(&archive, &repo, sid);
    let symlink_leaf = sessions.join("symlink-leaf.jsonl");
    symlink(&fallback, &symlink_leaf).unwrap();
    let symlink_dir = sessions.join("symlink-dir");
    symlink(fallback.parent().unwrap(), &symlink_dir).unwrap();
    let hardlink = sessions.join("hardlink.jsonl");
    let hardlink_source = sessions.join("hardlink-source.jsonl");
    codex_handoff_rollout(&hardlink_source, &repo, sid);
    fs::hard_link(&hardlink_source, &hardlink).unwrap();
    let oversized = sessions.join("oversized.jsonl");
    codex_handoff_rollout(&oversized, &repo, sid);
    fs::OpenOptions::new()
        .write(true)
        .open(&oversized)
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    let conflicting_header = sessions.join("conflicting-header.jsonl");
    fs::write(
        &conflicting_header,
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"another-session\",\"id\":{},\"cwd\":{}}}}}\n",
            json!(sid), json!(repo)
        ),
    ).unwrap();
    strip_group_world_write(tmp.path());
    let host = RecordingHost::default();
    let cancel = AtomicBool::new(false);
    let invoke = |input: &str| {
        assert_eq!(
            run_with_services(
                "harness-hook",
                &argv(&["precompact-handoff", "codex"]),
                input.as_bytes(),
                &repo,
                tmp.path(),
                &cancel,
                &host,
            ),
            (0, Vec::new(), Vec::new())
        );
    };
    let mut base = json!({"hook_event_name":"PreCompact","cwd":repo,"session_id":sid});
    let reject = |input: &str| {
        invoke(input);
        let (exit, summary) = host
            .failures
            .lock()
            .unwrap()
            .pop()
            .expect("recorded rejection");
        assert_eq!(exit, 1);
        assert!(
            summary == "invalid Codex hook transcript hint"
                || summary == "error: unsafe or unavailable Codex hook transcript",
            "{summary}"
        );
        assert!(!repo.join(".oms/handoffs").exists());
    };
    for hint in [
        Value::Null,
        json!(false),
        json!(""),
        json!("relative.jsonl"),
        json!(sessions.join("day/../day/rollout-exact.jsonl")),
        json!(sessions.join("missing.jsonl")),
        json!(wrong_repo),
        json!(wrong_session),
        json!(other_account),
        json!(default_account),
        json!(archive),
        json!(symlink_leaf),
        json!(symlink_dir.join("rollout-exact.jsonl")),
        json!(hardlink),
        json!(oversized),
        json!(conflicting_header),
    ] {
        base["transcript_path"] = hint;
        reject(&base.to_string());
    }

    base["transcript_path"] = json!(fallback);
    let valid = base.to_string();
    reject(&format!(r#"{{"transcript_path":null,{}"#, &valid[1..]));
    reject(&format!(
        r#"{{"session_id":"another-session",{}"#,
        &valid[1..]
    ));
    base["transcriptPath"] = json!(wrong_session);
    reject(&base.to_string());
    base.as_object_mut().unwrap().remove("transcriptPath");
    base["sessionId"] = json!("another-session");
    reject(&base.to_string());
    base.as_object_mut().unwrap().remove("sessionId");
    base.as_object_mut().unwrap().remove("session_id");
    reject(&base.to_string());
    base["session_id"] = json!(sid);

    // The same account and session can be captured through compatibility
    // discovery when the field is truly absent.
    fs::remove_file(conflicting_header).unwrap();
    base.as_object_mut().unwrap().remove("transcript_path");
    invoke(&base.to_string());
    assert!(host.failures.lock().unwrap().is_empty());
    assert_eq!(fs::read_dir(repo.join(".oms/handoffs")).unwrap().count(), 1);
}

const CODEX_TRACE: &str = ".oms/hooks/codex-trace.jsonl";

#[test]
fn codex_hook_trace_marks_receipt_while_callback_stdin_is_pending() {
    const TEST: &str = "codex_hook_trace_marks_receipt_while_callback_stdin_is_pending";
    const CHILD: &str = "OMS_TEST_TRACE_STDIN_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            oms_command_hooks::run_with_source(
                "harness-hook",
                &argv(&["telemetry-hook"]),
                &cwd,
                &cwd,
                &AtomicBool::new(false),
            ),
            (0, vec![], vec![]),
        );
        return;
    }

    use std::io::Write;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let tmp = TempDir::new("trace-stdin");
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms")).unwrap();
    strip_group_world_write(tmp.path());
    let path = repo.join(CODEX_TRACE);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--test-threads=1"])
        .current_dir(&repo)
        .env_clear()
        .env(CHILD, "1")
        .env("HOME", tmp.path())
        .env("OMS_CODEX_HOOK_TRACE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let pending = loop {
        if let Ok(data) = fs::read_to_string(&path)
            && data.ends_with('\n')
        {
            break data;
        }
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            break String::new();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let sent = child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name":"SessionEnd","session_id":"private-stdin-marker"}"#);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    sent.unwrap();

    let receipt: serde_json::Value = serde_json::from_str(&pending)
        .expect("one receipt should be recorded before the stdin read completes");
    let data = fs::read_to_string(path).unwrap();
    assert!(!data.contains("private-stdin-marker"));
    let rows: Vec<serde_json::Value> = data
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let [received, entry, exit] = rows.as_slice() else {
        panic!("expected receipt, entry and exit");
    };
    assert_eq!(received, &receipt);
    assert_eq!(received["phase"], "received");
    assert_eq!(received["event"], "unknown");
    assert_eq!(entry["phase"], "entry");
    assert_eq!(entry["event"], "SessionEnd");
    assert_eq!(exit["phase"], "exit");
    assert_eq!(exit["outcome"], "returned");
    assert_eq!(received["nonce"], entry["nonce"]);
    assert_eq!(received["nonce"], exit["nonce"]);
}

#[test]
fn codex_hook_trace_correlates_only_metadata_and_preserves_masked_stop_output() {
    if !clean_env_child(
        "codex_hook_trace_correlates_only_metadata_and_preserves_masked_stop_output",
        &[
            ("OMS_CODEX_HOOK_TRACE", "1"),
            ("OMS_SESSION_BUDGET_TURNS", "2"),
            ("OMS_WORK_JOURNAL", "0"),
            ("OMS_TEST_TRACE_PRIVATE_FIXTURE", "private-env-marker"),
        ],
    ) {
        return;
    }
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    let tmp = TempDir::new("trace");
    let repo = git_repo(&tmp);
    let foreign = tmp.path().join("payload-repo");
    fs::create_dir_all(foreign.join(".oms")).unwrap();
    fs::create_dir_all(repo.join(".oms/hooks/sessions")).unwrap();
    let session = "private-session-marker";
    let session_hash = format!("{:x}", Sha256::digest(session.as_bytes()));
    fs::write(
        repo.join(format!(".oms/hooks/sessions/{}.json", &session_hash[..32])),
        b"{",
    )
    .unwrap();
    strip_group_world_write(tmp.path());
    let cancel = AtomicBool::new(false);
    let raw = json!({
        "hook_event_name": "Stop", "cwd": repo, "session_id": session,
        "turn_id": "private-turn-marker", "prompt": "private-prompt-marker",
        "tool_input": {"command": "private-command-marker"},
    })
    .to_string();
    let invoke = |args: &[&str], input: &[u8]| {
        run_hook(
            "harness-hook",
            &argv(args),
            input,
            &repo,
            tmp.path(),
            &cancel,
        )
    };
    let ordinary = run_hook(
        "turn-guard",
        &[],
        raw.as_bytes(),
        &repo,
        tmp.path(),
        &cancel,
    );
    assert_eq!(ordinary.0, 0);
    assert!(
        std::str::from_utf8(&ordinary.1)
            .unwrap()
            .contains("unavailable (helper error)")
    );
    assert!(!repo.join(CODEX_TRACE).exists());
    assert_eq!(invoke(&["turn-guard"], raw.as_bytes()), ordinary);

    for input in [
        json!({"hook_event_name":"SessionStart", "cwd":foreign, "session_id":session}),
        json!({"hookEventName":"SessionEnd", "cwd":foreign, "session_id":session}),
        json!({"hook_event_name":"SessionStart", "hookEventName":"private-conflict-marker"}),
        json!({"hook_event_name":"private-event-marker", "cwd":foreign}),
    ] {
        assert_eq!(
            invoke(&["telemetry-hook"], input.to_string().as_bytes()),
            (0, vec![], vec![]),
        );
    }
    assert_eq!(
        invoke(
            &["telemetry-hook", "private-argv-marker"],
            br#"{"hook_event_name":"SessionStart"}"#
        ),
        (0, vec![], vec![]),
    );
    assert_eq!(
        invoke(&["telemetry-hook"], &vec![b'x'; 8 * 1024 * 1024 + 1]),
        (0, vec![], vec![])
    );
    oms_command_hooks::trace_runtime_unavailable("harness-hook", &argv(&["resume-hook"]), &repo);
    oms_command_hooks::trace_runtime_unavailable("harness-hook", &argv(&["relay"]), &repo);
    oms_command_hooks::trace_runtime_unavailable("resume-hook", &[], &repo);
    assert!(!foreign.join(CODEX_TRACE).exists());

    let path = repo.join(CODEX_TRACE);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let trace = fs::read_to_string(&path).unwrap();
    for private in [
        session,
        "private-turn-marker",
        "private-prompt-marker",
        "private-command-marker",
        "private-event-marker",
        "private-conflict-marker",
        "private-argv-marker",
        "private-env-marker",
        // Callback output text, from the failing Stop callback above.
        "helper error",
        "systemMessage",
        repo.to_str().unwrap(),
        &session_hash[..32],
    ] {
        assert!(!trace.contains(private));
    }
    let rows: Vec<Value> = trace
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let expected = [
        ("turn-guard", "Stop", "internal_failure"),
        ("telemetry-hook", "SessionStart", "returned"),
        ("telemetry-hook", "SessionEnd", "returned"),
        ("telemetry-hook", "unknown", "returned"),
        ("telemetry-hook", "unknown", "returned"),
        ("telemetry-hook", "unknown", "returned"),
        ("telemetry-hook", "unknown", "input_oversize"),
        ("resume-hook", "unknown", "runtime_unavailable"),
    ];
    let mut nonces = std::collections::BTreeSet::new();
    let mut rows = rows.iter();
    for (handler, event, outcome) in expected {
        let received = rows.next().unwrap();
        let nonce = received["nonce"].as_str().unwrap();
        assert!(nonces.insert(nonce));
        assert!(nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let received_at = received["received_at"].as_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(received_at).is_ok());
        assert_eq!(
            received,
            &json!({"schema":1,"phase":"received","handler":handler,"event":"unknown",
                "nonce":nonce,"received_at":received_at})
        );
        let entered = matches!(outcome, "internal_failure" | "returned");
        if entered {
            assert_eq!(
                rows.next().unwrap(),
                &json!({"schema":1,"phase":"entry","handler":handler,"event":event,
                    "nonce":nonce,"received_at":received_at})
            );
        }
        let exit = rows.next().unwrap();
        let elapsed = exit["elapsed_us"].as_u64().unwrap();
        let mut expected_exit = json!({"schema":1,"phase":"exit","handler":handler,"event":event,
            "nonce":nonce,"received_at":received_at,"elapsed_us":elapsed,"outcome":outcome});
        if entered {
            expected_exit["inner_exit"] = json!(0);
        }
        assert_eq!(exit, &expected_exit);
    }
    assert!(rows.next().is_none());
}

#[test]
fn codex_hook_trace_is_opt_in() {
    if std::env::var_os("OMS_TEST_CLEAN_CHILD").is_none() {
        // Unset, off, and any value other than the exact opt-in.
        for flag in [None, Some("0"), Some("true")] {
            let env = flag.map(|value| ("OMS_CODEX_HOOK_TRACE", value));
            clean_env_child("codex_hook_trace_is_opt_in", env.as_slice());
        }
        return;
    }
    let tmp = TempDir::new("trace-disabled");
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms")).unwrap();
    strip_group_world_write(tmp.path());
    assert_eq!(
        run_hook(
            "harness-hook",
            &argv(&["telemetry-hook"]),
            b"{}",
            &repo,
            tmp.path(),
            &AtomicBool::new(false)
        ),
        (0, vec![], vec![]),
    );
    oms_command_hooks::trace_runtime_unavailable("harness-hook", &argv(&["telemetry-hook"]), &repo);
    assert!(!repo.join(".oms/hooks").exists());
}

#[test]
fn codex_hook_trace_fails_open_for_busy_broad_and_linked_ledgers() {
    if !clean_env_child(
        "codex_hook_trace_fails_open_for_busy_broad_and_linked_ledgers",
        &[("OMS_CODEX_HOOK_TRACE", "1")],
    ) {
        return;
    }
    let tmp = TempDir::new("trace-unsafe");
    let repo = git_repo(&tmp);
    let path = repo.join(CODEX_TRACE);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    strip_group_world_write(tmp.path());
    let bad = repo.join("broken.json");
    fs::write(&bad, b"{").unwrap();
    let input = serde_json::json!({"hook_event_name":"PostToolUse","tool_name":"Write","cwd":repo,
        "tool_input":{"file_path":bad}})
    .to_string();
    let stop = AtomicBool::new(false);
    let expected = run_hook(
        "syntax-guard-hook",
        &[],
        input.as_bytes(),
        &repo,
        tmp.path(),
        &stop,
    );
    assert!(!expected.1.is_empty());
    let verify = || {
        assert_eq!(
            run_hook(
                "harness-hook",
                &argv(&["syntax-guard-hook"]),
                input.as_bytes(),
                &repo,
                tmp.path(),
                &stop
            ),
            expected,
        );
        assert_eq!(fs::read(&path).unwrap(), b"untouched\n");
    };
    fs::write(&path, b"untouched\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    verify();
    drop(held);

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    verify();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let limit = 1024 * 1024;
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(limit)
        .unwrap();
    assert_eq!(
        run_hook(
            "harness-hook",
            &argv(&["syntax-guard-hook"]),
            input.as_bytes(),
            &repo,
            tmp.path(),
            &stop
        ),
        expected,
    );
    assert_eq!(fs::metadata(&path).unwrap().len(), limit);
    // A row that would cross the limit is refused whole, not truncated into it.
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(limit - 8)
        .unwrap();
    assert_eq!(
        run_hook(
            "harness-hook",
            &argv(&["syntax-guard-hook"]),
            input.as_bytes(),
            &repo,
            tmp.path(),
            &stop
        ),
        expected,
    );
    assert_eq!(fs::metadata(&path).unwrap().len(), limit - 8);
    // With room to spare the same callback appends and stays within the limit.
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(limit - 4096)
        .unwrap();
    assert_eq!(
        run_hook(
            "harness-hook",
            &argv(&["syntax-guard-hook"]),
            input.as_bytes(),
            &repo,
            tmp.path(),
            &stop
        ),
        expected,
    );
    let grown = fs::metadata(&path).unwrap().len();
    assert!(grown > limit - 4096 && grown <= limit, "{grown}");
    fs::write(&path, b"untouched\n").unwrap();
    let outside = tmp.path().join("trace-target");
    fs::rename(&path, &outside).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(outside, &path).unwrap();
    verify();
}

#[test]
fn harness_hook_pi_panel_hook_prompts_hint_and_tools_stay_silent() {
    let extra = [
        ("OMS_PANEL_MAIN_ATTEMPT", "att_test"),
        ("OMS_PANEL_SESSION", "oms-test"),
        ("OMS_ROOM_ID", "room-test"),
        ("OMS_ROOM_PARTICIPANT", "part-test"),
    ];
    if !clean_env_child(
        "harness_hook_pi_panel_hook_prompts_hint_and_tools_stay_silent",
        &extra,
    ) {
        return;
    }
    let tmp = TempDir::new("pi-panel");
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms")).unwrap();
    let base = |event: &str| serde_json::json!({"hook_event_name":event,"cwd":repo,"session_id":"pi-1","prompt":"build it"});
    let (code, out) = harness(&tmp, "panel-hook", &base("UserPromptSubmit"));
    assert_eq!(code, 0);
    assert!(out.contains("[oms panel]"), "{out}");
    assert!(out.contains("[oms intake]"), "{out}");
    assert!(!out.contains("build it"), "{out}");
    let (code, out) = harness(&tmp, "panel-hook", &base("PostToolUse"));
    assert_eq!((code, out.as_str()), (0, ""));
    // malformed and empty input are silent, not errors
    for raw in [&b""[..], b"{", b"[]"] {
        let (code, out, _) = hook(&tmp, "harness-hook", &["panel-hook"], raw);
        assert_eq!((code, out.as_str()), (0, ""));
    }
}

#[test]
fn harness_hook_pi_panel_hook_is_silent_outside_a_panel() {
    if !clean_env_child(
        "harness_hook_pi_panel_hook_is_silent_outside_a_panel",
        &[
            ("OMS_ROOM_ID", "room-test"),
            ("OMS_ROOM_PARTICIPANT", "part-test"),
        ],
    ) {
        return;
    }
    let tmp = TempDir::new("pi-nopanel");
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms")).unwrap();
    // A live room that collaboration() would read and advance if it ran.
    seed_room(&repo);
    let payload = serde_json::json!({"hook_event_name":"UserPromptSubmit","cwd":repo,"session_id":"pi-2","prompt":"implement the requested change"});
    assert_eq!(harness(&tmp, "panel-hook", &payload), (0, String::new()));
    assert_eq!(harness(&tmp, "relay", &payload), (0, String::new()));
    let hooks = repo.join(".oms/hooks");
    let touched = fs::read_dir(&hooks).map_or(vec![], |d| {
        d.flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    });
    assert!(
        touched.is_empty(),
        "no hook state outside a panel: {touched:?}"
    );
}

#[test]
fn harness_hook_pi_relay_hint_requires_an_adopted_repo() {
    if !clean_env_child("harness_hook_pi_relay_hint_requires_an_adopted_repo", &[]) {
        return;
    }
    let tmp = TempDir::new("pi-relay");
    let repo = git_repo(&tmp);
    let payload =
        serde_json::json!({"hook_event_name":"SessionStart","cwd":repo,"session_id":"pi-3"});
    assert_eq!(harness(&tmp, "relay-hint", &payload), (0, String::new()));
    fs::create_dir_all(repo.join(".oms")).unwrap();
    assert_eq!(harness(&tmp, "relay-hint", &payload), (0, String::new()));
    assert_eq!(harness(&tmp, "relay", &payload), (0, String::new()));
    let (code, out, _) = hook(&tmp, "harness-hook", &["relay-hint"], b"{");
    assert_eq!((code, out.as_str()), (0, ""));
}

#[test]
fn oms_root_needs_a_checkout_marker_beyond_the_marketplace_file() {
    use oms_command_hooks::surfaces;
    let tmp = TempDir::new("root");
    let row = |root: &Path| {
        // An existing entry file rules out the dead-wrapper path, leaving only is_oms_root.
        fs::create_dir_all(root.join("scripts/lib")).unwrap();
        fs::write(root.join("scripts/lib/python-entry.sh"), "").unwrap();
        let cmd = format!(
            "bash {0}/scripts/lib/python-entry.sh bash {0}/scripts/resume-hook.sh",
            root.display()
        );
        surfaces::owned_script(&serde_json::json!({"type":"command","command":cmd}), &[])
    };
    let native = tmp.path().join("native");
    fs::create_dir_all(native.join(".agents/plugins")).unwrap();
    fs::write(native.join(".agents/plugins/marketplace.json"), "{}").unwrap();
    assert_eq!(row(&native), None, "marketplace alone is not enough");
    fs::create_dir_all(native.join("crates/oms")).unwrap();
    fs::write(native.join("crates/oms/Cargo.toml"), "").unwrap();
    assert_eq!(row(&native).as_deref(), Some("resume-hook.sh"));
    let cargo_only = tmp.path().join("cargo-only");
    fs::create_dir_all(cargo_only.join("crates/oms")).unwrap();
    fs::write(cargo_only.join("crates/oms/Cargo.toml"), "").unwrap();
    assert_eq!(row(&cargo_only), None, "Cargo.toml alone is not enough");
}

const PANEL_ENV: [(&str, &str); 4] = [
    ("OMS_PANEL_MAIN_ATTEMPT", "att_test"),
    ("OMS_PANEL_SESSION", "oms-test"),
    ("OMS_ROOM_ID", "room-test"),
    ("OMS_ROOM_PARTICIPANT", "part-test"),
];

fn seed_room(repo: &Path) {
    strip_group_world_write(repo);
    let room = |args: &[&str]| {
        let mut a = argv(args);
        a.extend(argv(&["--repo", repo.to_str().unwrap()]));
        let (code, _, err) = oms_command_room::run(&a, repo, &AtomicBool::new(false));
        assert_eq!(code, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
    };
    room(&["new", "--id", "room-test"]);
    room(&[
        "join",
        "--id",
        "room-test",
        "--participant",
        "part-test",
        "--role",
        "main",
    ]);
}

fn room_run(repo: &Path, args: &[&str]) {
    let mut a = argv(args);
    a.extend(argv(&["--repo", repo.to_str().unwrap()]));
    let (code, _, err) = oms_command_room::run(&a, repo, &AtomicBool::new(false));
    assert_eq!(code, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
}

fn prompt(tmp: &TempDir) -> String {
    let p = serde_json::json!({"hook_event_name":"UserPromptSubmit","cwd":tmp.path().join("repo"),"session_id":"pi-9","prompt":"build it"});
    harness(tmp, "panel-hook", &p).1
}

#[test]
fn harness_hook_pi_relay_hint_rearms_the_panel_prompt_after_context_loss() {
    let name = "harness_hook_pi_relay_hint_rearms_the_panel_prompt_after_context_loss";
    if !clean_env_child(name, &PANEL_ENV) {
        return;
    }
    let tmp = TempDir::new("pi-rearm");
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms/plan")).unwrap();
    fs::write(
        repo.join(".oms/plan/tasks.json"),
        r#"{"tasks":{"t1":{"state":"running","claimed_by_participant":"part-test"}}}"#,
    )
    .unwrap();
    seed_room(&repo);
    strip_group_world_write(tmp.path());
    let assert_main_contract = |hint: &str| {
        assert!(hint.contains("You are a control-tower main"), "{hint}");
        for obsolete_permission in [
            "Direct edits are for local known-cause fixes",
            "recovery after two failures",
        ] {
            assert!(!hint.contains(obsolete_permission), "{hint}");
        }
        for required in [
            "Repository edits stay delegated, including small fixes and integration code",
            "oms panel --continue TASK_ID --brief-file PATH",
            "oms panel --dispatch integrator --access write --scope PATH",
            "oms panel --dispatch researcher --access read",
            "no worker can start, report the blocker and wait",
            "specific user instruction for Main to make that change itself",
            "Room as a --kind status message",
            "unless the user started the panel with OMS_MAIN_WRITE_GUARD=0",
        ] {
            assert!(hint.contains(required), "missing {required}: {hint}");
        }
    };
    assert_main_contract(&prompt(&tmp));
    assert!(
        !prompt(&tmp).contains("You are a control-tower main"),
        "delivered once"
    );
    let start = |source: &str| serde_json::json!({"hook_event_name":"SessionStart","source":source,"cwd":repo,"session_id":"pi-9"});
    let sessions = repo.join(".oms/hooks/sessions");
    for source in ["compact", "clear", "resume", "startup"] {
        let (code, _) = harness(&tmp, "relay-hint", &start(source));
        assert_eq!(code, 0);
        let resume = fs::read_dir(&sessions)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().ends_with(".resume.json"))
            .expect("resume marker");
        let row: serde_json::Value = serde_json::from_slice(&fs::read(&resume).unwrap()).unwrap();
        assert_eq!(row["source"], source);
        assert!(row.get("delivered").is_none(), "{row}");
        assert!(
            !fs::read_dir(&sessions).unwrap().any(|e| e
                .unwrap()
                .path()
                .to_string_lossy()
                .ends_with(".hint.json")),
            "dedup digest removed"
        );
        let first = prompt(&tmp);
        assert_main_contract(&first);
        let line = format!("resumed after {source}: ");
        if source == "startup" {
            assert!(!first.contains("resumed after"), "{first}");
        } else {
            assert!(first.contains(&line), "{source}: {first}");
            assert!(first.contains("your plan task: t1 (running);"), "{first}");
            assert!(
                first.find(&line) < first.find("You are a control-tower main"),
                "resume line leads"
            );
        }
        let second = prompt(&tmp);
        assert!(!second.contains("resumed after"), "once: {second}");
        assert!(
            second.contains("Main manages; repository edits stay delegated"),
            "short reminder preserves the role contract: {second}"
        );
    }
    // OMS_ROOM_ID still names the pre-rollover room: the line follows the successor
    room_run(&repo, &["rollover", "--id", "room-test"]);
    room_run(
        &repo,
        &[
            "send",
            "--to",
            "all",
            "--kind",
            "status",
            "--text",
            "after rollover",
        ],
    );
    harness(&tmp, "relay-hint", &start("compact"));
    let rolled = prompt(&tmp);
    assert_main_contract(&rolled);
    assert!(rolled.contains("resumed after compact: "), "{rolled}");
    assert!(rolled.contains("status: after rollover."), "{rolled}");
    // other sources and other events leave the delivered marker alone
    assert!(!prompt(&tmp).contains("You are a control-tower main"));
    let other = serde_json::json!({"hook_event_name":"SessionStart","source":"fork","cwd":repo,"session_id":"pi-9"});
    assert_eq!(harness(&tmp, "relay-hint", &other), (0, String::new()));
    assert!(!prompt(&tmp).contains("You are a control-tower main"));
    let events = fs::read_to_string(repo.join(".oms/hooks/events.jsonl")).unwrap();
    assert!(
        events.contains("\"injection\"") && events.contains("\"panel\""),
        "{events}"
    );
    assert!(
        !events.contains("You are a control-tower main"),
        "rows stay content-free"
    );
    fs::write(
        repo.join(".oms/plan/tasks.json"),
        r#"{"tasks":{"t2":{"id":"t2","state":"ready","depends":[]}}}"#,
    )
    .unwrap();
    let ready = prompt(&tmp);
    assert!(ready.contains("next ready: t2"), "{ready}");
    assert!(
        ready.contains("managed plan-run claims the ready task itself"),
        "{ready}"
    );
    assert!(ready.contains("do not pre-claim"), "{ready}");
    assert!(!ready.contains("claim it before starting work"), "{ready}");
}

#[test]
fn harness_hook_pi_relay_records_idle_and_relay_hint_shows_the_other_agents_card() {
    let name = "harness_hook_pi_relay_records_idle_and_relay_hint_shows_the_other_agents_card";
    if !clean_env_child(name, &PANEL_ENV) {
        return;
    }
    let tmp = TempDir::new("pi-idle");
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms")).unwrap();
    let stop = serde_json::json!({"hook_event_name":"Stop","cwd":repo,"session_id":"pi-10"});
    assert_eq!(harness(&tmp, "relay", &stop), (0, String::new()));
    let row: serde_json::Value = serde_json::from_slice(
        &fs::read(repo.join(".oms/hooks/panel-activity/part-test.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(row["state"], "idle");
}

#[test]
fn harness_hook_pi_relay_hint_outside_a_panel_shows_the_relay_card_once() {
    let name = "harness_hook_pi_relay_hint_outside_a_panel_shows_the_relay_card_once";
    if !clean_env_child(name, &[("OMS_RELAY", "1")]) {
        return;
    }
    let tmp = TempDir::new("pi-card");
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms/hooks/relay")).unwrap();
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ended = chrono::DateTime::from_timestamp(secs as i64 - 120, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    fs::write(
        repo.join(".oms/hooks/relay/claude.json"),
        serde_json::json!({"schema":1,"agent":"claude","ended_at":ended,"message":"done"})
            .to_string(),
    )
    .unwrap();
    let start = serde_json::json!({"hook_event_name":"SessionStart","source":"startup","cwd":repo,"session_id":"pi-11"});
    let (code, out) = harness(&tmp, "relay-hint", &start);
    assert_eq!(code, 0);
    assert!(out.contains("[oms relay] claude finished a turn"), "{out}");
    assert_eq!(
        harness(&tmp, "relay-hint", &start),
        (0, String::new()),
        "seen once"
    );
    let events = fs::read_to_string(repo.join(".oms/hooks/events.jsonl")).unwrap();
    assert!(events.contains("session-start"), "{events}");
}

#[test]
fn relay_shows_land_card_once_per_session() {
    if !clean_env_child(
        "relay_shows_land_card_once_per_session",
        &[("OMS_RELAY", "1")],
    ) {
        return;
    }
    let tmp = TempDir::new("land-relay");
    let repo = git_repo(&tmp);
    let dir = repo.join(".oms/hooks/relay");
    fs::create_dir_all(&dir).unwrap();
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ended = chrono::DateTime::from_timestamp(secs as i64, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    fs::write(
        dir.join("land.json"),
        serde_json::json!({"schema":1,"ended_at":ended,"message":"land passed"}).to_string(),
    )
    .unwrap();
    let start = serde_json::json!({"hook_event_name":"SessionStart","source":"startup","cwd":repo,"session_id":"land-session"});
    let (code, out) = harness(&tmp, "relay-hint", &start);
    assert_eq!(code, 0);
    assert!(out.contains("land passed"), "{out}");
    assert_eq!(harness(&tmp, "relay-hint", &start), (0, String::new()));
}

#[test]
fn claude_app_notify_parses_options_strictly_and_discovers_sessions_only_for_a_wakeup() {
    let name = "claude_app_notify_parses_options_strictly_and_discovers_sessions_only_for_a_wakeup";
    if !clean_env_child(name, &[]) {
        return;
    }
    let tmp = TempDir::new("claude-flags");
    let usage = |args: &[&str]| {
        let (code, out, err) = hook(&tmp, "claude-app-notify", args, &[]);
        assert_eq!((code, out.as_str()), (2, ""), "{args:?}");
        assert!(err.starts_with("usage: claude-app-notify"), "{err}");
    };
    usage(&["send", "--wakeup", "--dry-run", ".", "hi"]);
    usage(&["send", "-x", ".", "hi"]);
    usage(&["send", "--dry-run", "."]);
    {
        use std::os::unix::ffi::OsStringExt;
        let mut bad = b"--bad".to_vec();
        bad.push(0xff);
        let args = [OsString::from("send"), OsString::from_vec(bad), ".".into()];
        let (code, o, _) = run_hook(
            "claude-app-notify",
            &args,
            &[],
            tmp.path(),
            tmp.path(),
            &AtomicBool::new(false),
        );
        assert_eq!((code, o.len()), (2, 0), "non-UTF-8 flag");
    }
    usage(&["send", "--wakeup", "-x", "hi"]);
    usage(&["send", "--consumer", "--wakeup", ".", "hi"]);
    usage(&["send", ".", "hi", "--", "extra"]);
    let receipt = |args: &[&str]| -> serde_json::Value {
        let (code, out, _) = hook(&tmp, "claude-app-notify", args, &[]);
        assert_eq!(code, 0, "{args:?}");
        serde_json::from_str(&out).unwrap()
    };
    // text after `--` is literal and never enables a wakeup
    let row = receipt(&["send", ".", "--", "--wakeup"]);
    assert_eq!(row["status"], "wakeup_required");
    let row = receipt(&["send", "--", ".", "--wakeup"]);
    assert_eq!(row["status"], "wakeup_required");
    // an explicit consumer is used verbatim: a malformed one is "unbound", not discovered
    let row = receipt(&["send", "--wakeup", "--consumer=zz", ".", "hi"]);
    assert_eq!(row["status"], "unbound");
    let hex = "0".repeat(32);
    let arg = format!("--consumer={hex}");
    let row = receipt(&["send", "--wakeup", &arg, ".", "hi"]);
    assert!(
        ["absent", "unavailable"].contains(&row["status"].as_str().unwrap()),
        "{row}"
    );
    assert_eq!(row["submitted"], false);
    // no current session is exported in the test environment: a wakeup without
    // a consumer resolves to none and is unbound
    let row = receipt(&["send", "--wakeup", ".", "hi"]);
    assert_eq!(row["status"], "unbound");
}

fn claude_wakeup_status(tmp: &TempDir) -> String {
    let (code, out, _) = hook(
        tmp,
        "claude-app-notify",
        &["send", "--wakeup", ".", "hi"],
        &[],
    );
    assert_eq!(code, 0);
    let row: serde_json::Value = serde_json::from_str(&out).unwrap();
    row["status"].as_str().unwrap().to_owned()
}

#[test]
fn claude_app_notify_literal_zero_disables_delivery() {
    let name = "claude_app_notify_literal_zero_disables_delivery";
    if !clean_env_child(name, &[("OMS_CLAUDE_NOTIFY", "0")]) {
        return;
    }
    assert_eq!(
        claude_wakeup_status(&TempDir::new("claude-off")),
        "disabled"
    );
}

#[test]
fn claude_app_notify_other_notify_values_do_not_disable_delivery() {
    let name = "claude_app_notify_other_notify_values_do_not_disable_delivery";
    if !clean_env_child(name, &[("OMS_CLAUDE_NOTIFY", "false")]) {
        return;
    }
    assert_eq!(
        claude_wakeup_status(&TempDir::new("claude-false")),
        "unbound"
    );
}

#[test]
fn skill_router_debounces_session_state_rewrites_within_a_minute() {
    use std::os::unix::fs::MetadataExt;
    let name = "skill_router_debounces_session_state_rewrites_within_a_minute";
    if !clean_env_child(name, &[("OMS_SESSION_BUDGET_TURNS", "10")]) {
        return;
    }
    let tmp = TempDir::new("debounce");
    let repo = git_repo(&tmp);
    fs::create_dir_all(repo.join(".oms")).unwrap();
    let payload = |id: &str| {
        serde_json::json!({"hook_event_name":"UserPromptSubmit","cwd":repo,"session_id":id,"prompt":"inspect src/lib.rs"})
            .to_string()
    };
    let sessions = repo.join(".oms/hooks/sessions");
    let only = |dir: &Path| -> PathBuf {
        let mut files: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        files.remove(0)
    };
    assert_eq!(
        hook(&tmp, "skill-router", &[], payload("s1").as_bytes()).0,
        0
    );
    let file = only(&sessions);
    let first = fs::read(&file).unwrap();
    // `updated_at` has one-second granularity, so age the stamp to prove the
    // debounce rather than an identical-bytes no-op.
    let mut row: serde_json::Value = serde_json::from_slice(&first).unwrap();
    let recent = chrono::DateTime::<chrono::Utc>::from(
        std::time::SystemTime::now() - std::time::Duration::from_secs(30),
    );
    row["updated_at"] = recent.format("%Y-%m-%dT%H:%M:%SZ").to_string().into();
    let seeded = format!("{row}\n");
    fs::write(&file, &seeded).unwrap();
    let ino = fs::metadata(&file).unwrap().ino();
    assert_eq!(
        hook(&tmp, "skill-router", &[], payload("s1").as_bytes()).0,
        0
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        seeded,
        "30s-old stamp kept"
    );
    assert_eq!(fs::metadata(&file).unwrap().ino(), ino, "no rewrite");

    // A stale stamp refreshes updated_at and keeps the budget fields.
    let mut row: serde_json::Value = serde_json::from_slice(&first).unwrap();
    row["updated_at"] = "2000-01-01T00:00:00Z".into();
    row["budget_turns"] = 3.into();
    fs::write(&file, format!("{row}\n")).unwrap();
    assert_eq!(
        hook(&tmp, "skill-router", &[], payload("s1").as_bytes()).0,
        0
    );
    let after: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_ne!(after["updated_at"], "2000-01-01T00:00:00Z");
    assert_eq!(after["budget_turns"], 3);

    // A malformed or other-session file never short-circuits.
    fs::write(&file, b"not json").unwrap();
    assert_eq!(
        hook(&tmp, "skill-router", &[], payload("s1").as_bytes()).0,
        0
    );
    let healed: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(healed["schema"], 1);
}

#[test]
fn panel_safe_point_reads_activity_once_and_keeps_noop_hook_parity() {
    let name = "panel_safe_point_reads_activity_once_and_keeps_noop_hook_parity";
    let extra = [
        ("OMS_PANEL_MAIN_ATTEMPT", "att_test"),
        ("OMS_PANEL_SESSION", "oms-test"),
        ("OMS_ROOM_ID", "room-test"),
        ("OMS_ROOM_PARTICIPANT", "part-test"),
        ("OMS_LIVE_COLLAB", "0"),
    ];
    if !clean_env_child(name, &extra) {
        return;
    }
    let tmp = TempDir::new("activity-once");
    let repo = git_repo(&tmp);
    let transcript = "t".repeat(2800);
    let state = repo.join(".oms/hooks/panel-activity/part-test.json");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    let seed = serde_json::json!({"schema":1,"participant":"part-test","attempt":"att_test","room":"room-test","state":"busy","since":0,"updated_at":0,"origin":"user","transcript_path":transcript});
    let seeded = format!("{seed}\n");
    fs::write(&state, &seeded).unwrap();
    let reads = || -> Option<u64> {
        fs::read_to_string("/proc/self/io")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("rchar: ")?.trim().parse().ok())
    };
    let payload = serde_json::json!({"hook_event_name":"UserPromptSubmit","cwd":repo,"session_id":"s1","prompt":"","transcript_path":transcript});
    let before = reads();
    assert_eq!(harness(&tmp, "panel-hook", &payload), (0, String::new()));
    if let (Some(before), Some(after)) = (before, reads()) {
        assert!(
            after - before < seeded.len() as u64 + 1024,
            "activity read {} bytes for {} bytes of state",
            after - before,
            seeded.len()
        );
    }
    let row: serde_json::Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(row["state"], "busy");
    assert_eq!(row["origin"], "user");
    assert_eq!(row["since"], row["updated_at"]);
    // A fresh invocation must still record a different event.
    let stopped = serde_json::json!({"hook_event_name":"Stop","cwd":repo,"session_id":"s1"});
    assert_eq!(harness(&tmp, "panel-hook", &stopped), (0, String::new()));
    let row: serde_json::Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(row["state"], "idle");
    for raw in [
        b"".as_slice(),
        b"null",
        b"[]",
        b"{",
        b"{}",
        b"{\"duration_ms\":0}",
    ] {
        assert_eq!(hook(&tmp, "telemetry-hook", &[], raw).0, 0);
    }
    for _ in 0..2 {
        assert_eq!(
            oms_command_hooks::notify::safe_message("hello\nworld", &repo, 64),
            "hello\nworld\n"
        );
    }
}

struct SyntaxHost {
    verdict: Option<bool>,
    seen: Mutex<Vec<(PathBuf, Vec<u8>)>>,
}

impl HostServices for SyntaxHost {
    fn check_python_syntax(&self, path: &Path, source: &[u8]) -> Option<bool> {
        self.seen
            .lock()
            .expect("lock seen")
            .push((path.to_path_buf(), source.to_vec()));
        self.verdict
    }
}

#[test]
fn syntax_guard_checks_python_through_the_host_parser() {
    let tmp = TempDir::new("syntax");
    let repo = git_repo(&tmp);
    fs::create_dir(repo.join(".oms")).expect("mkdir .oms");
    let file = repo.join("bad.py");
    fs::write(&file, b"def broken(:\n").expect("write py");
    let payload = serde_json::json!({
        "tool_name": "Write",
        "tool_input": {"file_path": file},
        "cwd": repo,
    })
    .to_string();
    let cancel = AtomicBool::new(false);
    for (verdict, reported) in [(Some(false), true), (Some(true), false), (None, false)] {
        let host = SyntaxHost {
            verdict,
            seen: Mutex::default(),
        };
        let (code, stdout, stderr) = run_with_services(
            "syntax-guard-hook",
            &[],
            payload.as_bytes(),
            &repo,
            tmp.path(),
            &cancel,
            &host,
        );
        assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
        let out = String::from_utf8_lossy(&stdout);
        assert_eq!(
            out.contains("bad.py does not parse after this Write — python: syntax error"),
            reported,
            "{verdict:?}: {out}"
        );
        let seen = host.seen.lock().expect("lock seen").clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, b"def broken(:\n");
        assert!(seen[0].0.ends_with("bad.py"));
    }
}

#[test]
fn codex_usage_config_refuses_python_notifier_scripts() {
    let tmp = TempDir::new("config-py");
    let cfg = tmp.path().join("config.toml");
    fs::write(&cfg, b"model = \"m\"\n").expect("write config");
    let cancel = AtomicBool::new(false);
    for action in ["install", "check"] {
        let (code, stdout, stderr) = run_hook(
            "codex-usage-config",
            &argv(&[
                action,
                cfg.to_str().unwrap(),
                "--notify-script",
                "codex_turn_notify.py",
            ]),
            &[],
            tmp.path(),
            tmp.path(),
            &cancel,
        );
        assert_ne!(code, 0, "{action}: {}", String::from_utf8_lossy(&stdout));
        assert!(
            String::from_utf8_lossy(&stderr).contains("Python notifier scripts are retired"),
            "{action}: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert_eq!(fs::read(&cfg).expect("read config"), b"model = \"m\"\n");
    }
}

/// A repository stand-in: `.git/HEAD` plus an adopted `.oms/`, no git binary.
fn fake_repo(tmp: &TempDir, head: &str) -> PathBuf {
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
    fs::write(repo.join(".git/HEAD"), head).expect("write HEAD");
    for (name, body) in [
        (
            ".oms/plan/tasks.json",
            r##"{"tasks":{"t1":{"state":"running","title":"Fix #(x) 100%"},"t2":{"state":"done"}}}"##.to_string(),
        ),
        (
            ".oms/failures.jsonl",
            "{\"event\":\"fail\",\"fingerprint\":\"aa\",\"kind\":\"cmd\"}\n{\"event\":\"fail\",\"fingerprint\":\"bb\",\"kind\":\"cmd\"}\n{\"event\":\"resolved\",\"fingerprint\":\"bb\"}\n".to_string(),
        ),
        (".oms/delegations/a.json", format!("{{\"pid\":{}}}", std::process::id())),
        (".oms/delegations/b.json", "{\"pid\":2147483647}".to_string()),
        (".oms/delegations/c.json", "{\"state\":\"done\"}".to_string()),
    ] {
        let path = repo.join(name);
        fs::create_dir_all(path.parent().unwrap()).expect("mkdir state");
        fs::write(path, body).expect("write state");
    }
    repo
}

#[test]
fn agy_statusline_reads_head_and_live_state_without_spawning_git() {
    use chrono::{DateTime, FixedOffset, SecondsFormat, TimeDelta, Utc};
    use serde_json::{Value, json};

    let tmp = TempDir::new("agy-hud");
    let repo = fake_repo(&tmp, "ref: refs/heads/feat-x\n");
    let input = json!({"model":"Gemini 3","state":"idle","plan_tier":"pro",
        "context_window":{"used_percentage":42.4},"vim":{"mode":"NORMAL"},"cwd":repo});
    let render = |input: &Value| {
        let (code, out, err) = hook(&tmp, "agy-statusline", &[], input.to_string().as_bytes());
        assert_eq!(code, 0, "{err}");
        assert!(err.is_empty(), "{err}");
        out
    };
    let legacy =
        "Gemini 3 (pro) | ctx 42% | feat-x | NORMAL/idle | oms T1 Fix #(x) 100% | F1 | A1\n";
    assert_eq!(render(&input), legacy);
    let now = DateTime::<Utc>::from(std::time::SystemTime::now());
    let future = now + TimeDelta::hours(2);
    let rfc3339 = |time: DateTime<Utc>| time.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut official = input.clone();
    official["agent_state"] = json!("tool_use");
    official["email"] = json!("private-fixture@example.test");
    official["conversation_id"] = json!("private-conversation-fixture");
    official["transcript_path"] = json!("private-transcript-fixture.jsonl");
    official["quota"] = json!({
        "gemini-weekly": {
            "remaining_fraction": 0.9378,
            "reset_time": rfc3339(future),
            "reset_in_seconds": 7200
        },
        "private-other-bucket": {"remaining_fraction":0.25, "reset_time":rfc3339(future)}
    });
    let expected = "Gemini 3 (pro) | ctx 42% | Gemini weekly 94% left | feat-x | NORMAL/tool_use | oms T1 Fix #(x) 100% | F1 | A1\n";
    assert_eq!(render(&official), expected);
    official["quota"]["gemini-weekly"]["disabled"] = json!(false);
    official["quota"]["gemini-weekly"]["reset_in_seconds"] = json!(0);
    assert_eq!(render(&official), expected);
    assert_eq!(
        render(&json!({"modelName":"m","state":" old\nlabel\u{1b}[31m "})),
        "m | old label\n"
    );
    for state in [
        "idle",
        "working",
        "tool_use",
        "reviewing",
        "authenticating",
        "initializing",
        "error",
    ] {
        assert_eq!(
            render(&json!({"modelName":"m","agent_state":state,"state":"fallback"})),
            format!("m | {state}\n")
        );
    }
    for state in [
        Value::Null,
        json!(false),
        json!(17),
        json!({}),
        json!(""),
        json!("unknown"),
        json!(" idle"),
    ] {
        assert_eq!(
            render(&json!({"modelName":"m","agent_state":state,"state":"fallback"})),
            "m\n"
        );
    }
    let bucket = official["quota"]["gemini-weekly"].clone();
    let show_quota =
        |bucket: &Value| render(&json!({"modelName":"m","quota":{"gemini-weekly":bucket}}));
    let mut offset_reset = bucket.clone();
    offset_reset["reset_time"] = json!(
        (now + TimeDelta::days(7) - TimeDelta::seconds(30))
            .with_timezone(&FixedOffset::east_opt(2 * 3600).unwrap())
            .to_rfc3339_opts(SecondsFormat::Millis, false)
    );
    assert_eq!(show_quota(&offset_reset), "m | Gemini weekly 94% left\n");
    for (fraction, percentage) in [
        (0.0, "0%"),
        (0.0001, "<1%"),
        (0.0099, "<1%"),
        (0.01, "1%"),
        (0.99, "99%"),
        (0.9901, ">99%"),
        (0.9999, ">99%"),
        (1.0, "100%"),
    ] {
        let mut record = bucket.clone();
        record["remaining_fraction"] = json!(fraction);
        assert_eq!(
            show_quota(&record),
            format!("m | Gemini weekly {percentage} left\n")
        );
    }
    for (field, bad) in [
        ("disabled", json!(true)),
        ("disabled", json!("false")),
        ("disabled", Value::Null),
        ("remaining_fraction", json!("0.9378")),
        ("remaining_fraction", json!(false)),
        ("remaining_fraction", json!(-0.01)),
        ("remaining_fraction", json!(1.01)),
        ("reset_time", json!("in two hours")),
        ("reset_time", json!(future.timestamp())),
        (
            "reset_time",
            json!(future.format("%Y-%m-%dT%H:%M:%S").to_string()),
        ),
        ("reset_time", json!(rfc3339(future).replace('T', " "))),
        ("reset_time", json!(rfc3339(now - TimeDelta::seconds(10)))),
        ("reset_time", json!(rfc3339(now + TimeDelta::days(8)))),
    ] {
        let mut record = bucket.clone();
        record[field] = bad;
        assert_eq!(show_quota(&record), "m\n", "{field}");
    }
    let mut relative = bucket.clone();
    relative.as_object_mut().unwrap().remove("reset_time");
    assert_eq!(show_quota(&relative), "m\n");
    assert_eq!(
        render(&json!({"modelName":"m","quota":{"gemini_weekly":bucket,"claude-weekly":bucket}})),
        "m\n"
    );
    let ambiguous = format!(
        r#"{{"modelName":"m","state":"idle","quota":{{"gemini-weekly":{{"disabled":true,"disabled":false,"remaining_fraction":0.9378,"reset_time":"{}"}}}}}}"#,
        rfc3339(future)
    );
    assert_eq!(
        hook(&tmp, "agy-statusline", &[], ambiguous.as_bytes()).1,
        "m | idle\n"
    );
    let duplicate_state =
        br#"{"modelName":"m","state":"fallback","agent_state":"idle","agent_state":"working"}"#;
    assert_eq!(hook(&tmp, "agy-statusline", &[], duplicate_state).1, "m\n");
    fs::write(
        repo.join(".git/HEAD"),
        "0123456789abcdef0123456789abcdef01234567\n",
    )
    .unwrap();
    let (_, out, _) = hook(&tmp, "agy-statusline", &[], br#"{"modelName":"m"}"#);
    assert_eq!(out, "m\n");
    let moved = serde_json::json!({"modelName":"m","cwd":repo});
    let (_, out, _) = hook(&tmp, "agy-statusline", &[], moved.to_string().as_bytes());
    assert!(out.starts_with("m | 0123456 | "), "{out}");
    assert_eq!(hook(&tmp, "agy-statusline", &[], b"not json").1, "");
}

#[test]
fn oms_hud_prints_standalone_tmux_safe_json_and_compact_forms() {
    let tmp = TempDir::new("oms-hud");
    let repo = fake_repo(&tmp, "ref: refs/heads/main\n");
    let r = repo.to_str().unwrap();
    let (code, out, _) = hook(&tmp, "oms-hud", &["--repo", r], b"");
    assert_eq!(code, 0);
    assert_eq!(out, "repo | main | oms T1 Fix #(x) 100% | F1 | A1\n");
    let (_, out, _) = hook(&tmp, "oms-hud", &["--repo", r, "--compact", "--tmux"], b"");
    assert_eq!(out, "main | oms T1 Fix (x) 100 | F1 | A1\n");
    let (_, out, _) = hook(&tmp, "oms-hud", &["--repo", r, "--json"], b"");
    let row: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        (
            row["branch"].as_str(),
            row["tasks"].as_u64(),
            row["failures"].as_u64(),
            row["agents"].as_u64()
        ),
        (Some("main"), Some(1), Some(1), Some(1))
    );
    assert_ne!(hook(&tmp, "oms-hud", &["--bogus"], b"").0, 0);
    // Outside an OMS repository nothing is live and no state is required.
    let bare = TempDir::new("oms-hud-bare");
    assert_eq!(oms_hook_live(bare.path()), "");
}

fn oms_hook_live(path: &Path) -> String {
    oms_command_hooks::oms_live_strip(Some(path))
}

#[test]
fn claude_statusline_appends_the_live_strip_inside_an_oms_repo() {
    let tmp = TempDir::new("claude-hud");
    let repo = fake_repo(&tmp, "ref: refs/heads/feat\n");
    let input = serde_json::json!({"model":{"display_name":"M"},"workspace":{"current_dir":repo}});
    let (code, out, err) = hook(&tmp, "claude-statusline", &[], input.to_string().as_bytes());
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("oms T1 Fix #(x) 100% | F1 | A1"), "{out}");
}

fn codex_hud(tmp: &TempDir, action: &str, cfg: &Path) -> String {
    let (code, out, err) = hook(
        tmp,
        "codex-hud-config",
        &[action, cfg.to_str().unwrap()],
        b"",
    );
    assert_eq!(code, 0, "{err}");
    out
}

#[test]
fn codex_hud_displaces_the_factory_bare_model_and_restores_it() {
    let tmp = TempDir::new("codex-hud");
    let cfg = tmp.path().join("config.toml");
    let original = "[tui]\nstatus_line = [\"model\"]\ntheme = \"x\"\n";
    fs::write(&cfg, original).unwrap();
    assert!(codex_hud(&tmp, "install", &cfg).contains("displaced"));
    let installed = fs::read_to_string(&cfg).unwrap();
    assert!(
        installed.contains("# displaced: status_line = [\"model\"]\n"),
        "{installed}"
    );
    assert!(
        installed.contains("\"git-branch\", \"current-dir\"]"),
        "{installed}"
    );
    assert!(codex_hud(&tmp, "check", &cfg).contains("managed"));
    assert!(codex_hud(&tmp, "install", &cfg).contains("already current"));
    codex_hud(&tmp, "remove", &cfg);
    assert_eq!(fs::read_to_string(&cfg).unwrap(), original);
}

#[test]
fn codex_hud_upgrades_the_legacy_block_and_keeps_custom_status_lines() {
    let tmp = TempDir::new("codex-hud-legacy");
    let cfg = tmp.path().join("config.toml");
    let legacy = "status_line = [\"model-with-reasoning\", \"context-remaining\", \"five-hour-limit\", \"weekly-limit\", \"git-branch\"]";
    fs::write(
        &cfg,
        format!("[tui]\n# >>> oh-my-setting managed Codex HUD >>>\n{legacy}\n# <<< oh-my-setting managed Codex HUD <<<\n"),
    )
    .unwrap();
    assert!(codex_hud(&tmp, "install", &cfg).contains("upgraded"));
    let upgraded = fs::read_to_string(&cfg).unwrap();
    assert!(upgraded.contains("\"git-branch\", \"current-dir\"]") && !upgraded.contains(legacy));
    codex_hud(&tmp, "remove", &cfg);
    assert_eq!(fs::read_to_string(&cfg).unwrap(), "[tui]\n");
    let custom = "[tui]\nstatus_line = [\"model\", \"git-branch\"]\n";
    fs::write(&cfg, custom).unwrap();
    assert!(codex_hud(&tmp, "install", &cfg).contains("preserved user"));
    assert_eq!(fs::read_to_string(&cfg).unwrap(), custom);
}

#[test]
fn intake_reminder_is_stateless_and_limited_to_real_requests() {
    let mode = std::env::var("OMS_TEST_INTAKE_MODE").unwrap_or_default();
    if mode.is_empty() {
        let spawn = |case: &str, extra: &[(&str, &str)]| {
            let mut cmd = Command::new(std::env::current_exe().expect("test exe"));
            cmd.args([
                "--exact",
                "intake_reminder_is_stateless_and_limited_to_real_requests",
                "--test-threads=1",
                "--nocapture",
            ]);
            for (k, _) in std::env::vars_os() {
                if k.to_string_lossy().starts_with("OMS_") {
                    cmd.env_remove(k);
                }
            }
            cmd.env_remove("CLAUDE_CODE_MESSAGING_SOCKET");
            cmd.env(
                "CLAUDE_CONFIG_DIR",
                std::env::temp_dir().join("oms-hooks-no-claude"),
            );
            cmd.env("OMS_TEST_INTAKE_MODE", case);
            cmd.envs(extra.iter().copied());
            let out = cmd.output().expect("spawn intake child");
            assert!(
                out.status.success(),
                "{case}: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        spawn("normal", &[]);
        spawn("router_off", &[("OMS_SKILL_ROUTER_OFF", "1")]);
        spawn(
            "child",
            &[
                ("OMS_HARNESS_CHILD", "1"),
                ("OMS_PANEL_MAIN_ATTEMPT", "att_test"),
                ("OMS_PANEL_SESSION", "oms-test"),
                ("OMS_ROOM_ID", "room-test"),
                ("OMS_ROOM_PARTICIPANT", "part-test"),
            ],
        );
        spawn(
            "delegate",
            &[
                ("OMS_HARNESS_DELEGATE_DEPTH", "1"),
                ("OMS_PANEL_MAIN_ATTEMPT", "att_test"),
                ("OMS_PANEL_SESSION", "oms-test"),
                ("OMS_ROOM_ID", "room-test"),
                ("OMS_ROOM_PARTICIPANT", "part-test"),
            ],
        );
        return;
    }
    let tmp = TempDir::new(&format!("intake-{mode}"));
    let repo = git_repo(&tmp);
    if matches!(mode.as_str(), "child" | "delegate" | "router_off") {
        let prompt_payload = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "cwd": repo,
            "session_id": "s",
            "prompt": "continue work",
        });
        let pre_payload = serde_json::json!({
            "hook_event_name": "PreInvocation",
            "cwd": repo,
            "session_id": "s",
            "prompt": "",
            "intake_eligible": true,
        });
        let direct = hook(
            &tmp,
            "skill-router",
            &[],
            prompt_payload.to_string().as_bytes(),
        )
        .1;
        let codex = harness(&tmp, "skill-router", &prompt_payload);
        let pre = hook(
            &tmp,
            "skill-router",
            &[],
            pre_payload.to_string().as_bytes(),
        )
        .1;
        if mode == "delegate" {
            assert!(
                direct.contains("[oms panel]") && !direct.contains("[oms intake]"),
                "{direct}"
            );
            assert_eq!(codex.0, 0);
            assert!(
                codex.1.contains("[oms panel]") && !codex.1.contains("[oms intake]"),
                "{}",
                codex.1
            );
            assert!(!pre.contains("[oms intake]"), "{pre}");
            let panel = harness(&tmp, "panel-hook", &prompt_payload);
            assert_eq!(panel.0, 0);
            assert!(
                panel.1.contains("[oms panel]") && !panel.1.contains("[oms intake]"),
                "{}",
                panel.1
            );
        } else {
            assert_eq!(direct, "");
            assert_eq!(codex, (0, String::new()));
            assert_eq!(pre, "");
            if mode == "child" {
                assert_eq!(
                    harness(&tmp, "panel-hook", &prompt_payload),
                    (0, String::new())
                );
            }
            assert!(!repo.join(".oms").exists(), "unadopted repo gains no state");
        }
        return;
    }
    let run = |prompt: &str, id: &str| {
        let payload = serde_json::json!({"hook_event_name":"UserPromptSubmit","cwd":repo,"session_id":id,"prompt":prompt});
        hook(&tmp, "skill-router", &[], payload.to_string().as_bytes()).1
    };
    for prompt in [
        "이어서",
        "고쳐",
        "고쳐줘",
        "/goal ship it",
        "<doc>fix the parser</doc>",
    ] {
        let out = run(prompt, "s");
        assert!(
            out.contains("[oms intake]")
                && out.contains("role, scope, and checks")
                && out.contains("existing task list")
                && out.contains("initializes no state"),
            "{prompt}: {out}"
        );
        assert!(
            !out.contains(prompt),
            "raw prompt must never be echoed: {prompt}: {out}"
        );
        assert_eq!(run(prompt, "s"), out, "stateless: repeats identically");
        let payload = serde_json::json!({"hook_event_name":"UserPromptSubmit","cwd":repo,"session_id":"s","prompt":prompt});
        let (code, codex_out) = harness(&tmp, "skill-router", &payload);
        assert_eq!(code, 0);
        let doc: serde_json::Value = serde_json::from_str(&codex_out).expect("codex json");
        assert_eq!(
            doc["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        let ctx = doc["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or("");
        assert!(ctx.contains("[oms intake]"), "{prompt}: {ctx}");
        assert!(!ctx.contains(prompt), "{prompt}: {ctx}");
    }
    for prompt in [
        "",
        "   ",
        "?",
        "<task-notification>done</task-notification>",
        "[SYSTEM NOTIFICATION] wake",
        "Another Claude session sent a message: ping",
        "Your claude.ai usage limit has reset.",
        "Task from Overall Main. Work within this task and report the result here. x",
    ] {
        assert!(!run(prompt, "s").contains("[oms intake]"), "{prompt}");
    }
    let legacy_pre = serde_json::json!({"hook_event_name":"PreInvocation","cwd":repo,"session_id":"s","prompt":""});
    assert!(
        !hook(&tmp, "skill-router", &[], legacy_pre.to_string().as_bytes())
            .1
            .contains("[oms intake]")
    );
    let false_pre = serde_json::json!({"hook_event_name":"PreInvocation","cwd":repo,"session_id":"s","prompt":"","intake_eligible":false});
    assert!(
        !hook(&tmp, "skill-router", &[], false_pre.to_string().as_bytes())
            .1
            .contains("[oms intake]")
    );
    let eligible_pre = serde_json::json!({"hook_event_name":"PreInvocation","cwd":repo,"session_id":"s","prompt":"","intake_eligible":true});
    let pre_out = hook(
        &tmp,
        "skill-router",
        &[],
        eligible_pre.to_string().as_bytes(),
    )
    .1;
    assert!(pre_out.contains("[oms intake]"), "{pre_out}");
    assert!(!repo.join(".oms").exists(), "unadopted repo gains no state");
}

#[test]
fn command_guard_audit_reports_rules_timestamps_and_the_legacy_footer() {
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    struct OwnedDir(PathBuf);
    impl Drop for OwnedDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let name = "command_guard_audit_reports_rules_timestamps_and_the_legacy_footer";
    if std::env::var_os("OMS_TEST_CLEAN_CHILD").is_none() {
        let mut nonce = [0u8; 16];
        fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut nonce))
            .expect("read random suffix");
        let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let state = std::env::temp_dir().join(format!("oms-hooks-audit-{suffix}"));
        assert!(!clean_env_child(
            name,
            &[("XDG_STATE_HOME", state.to_str().expect("utf8 state path"))],
        ));
        return;
    }
    // The child exclusively allocates its private XDG_STATE_HOME, so the user's real audit log is never read.
    let state_path = PathBuf::from(std::env::var_os("XDG_STATE_HOME").expect("state home"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&state_path)
        .expect("create exclusive state root");
    let state = OwnedDir(state_path);
    let dir = state.0.join("oh-my-setting");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .expect("create state dir");
    let rows = concat!(
        r#"{"ts":"2026-11-23T11:00:00Z","verdict":"refused","rule":"git-clean","command":"git","sha256":"abcdef"}"#,
        "\n",
        r#"{"ts":"2026-11-23T10:00:00Z","verdict":"noted","rule":"transfer","command":"scp","sha256":"012345"}"#,
        "\n"
    );
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join("command-guard.jsonl"))
        .expect("create audit log")
        .write_all(rows.as_bytes())
        .expect("write audit log");
    let (code, stdout, _) = run_hook(
        "command-guard-hook",
        &argv(&["--audit"]),
        b"",
        &state.0,
        &state.0,
        &AtomicBool::new(false),
    );
    let out = String::from_utf8_lossy(&stdout);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("command guard: newest 2 row(s): 1 refused, 0 bypassed"));
    assert!(
        out.contains("refused   git-clean") && out.contains("transfer"),
        "{out}"
    );
    assert!(out.contains("2026-11-23T11:00:00Z"), "{out}");
    assert!(
        out.ends_with("rows carry no command text; any process of this user can edit the file, so read it as a record, not proof\n"),
        "{out}"
    );
}

#[test]
fn pre_push_gate_runs_in_the_pushed_worktree_not_the_install_source() {
    let tmp = TempDir::new("prepush");
    let mk = |name: &str, body: &str| {
        let repo = tmp.path().join(name);
        fs::create_dir_all(repo.join("scripts")).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(repo.join("scripts/check.sh"), body).unwrap();
        repo
    };
    let source = mk(
        "source",
        "touch \"$(dirname \"$0\")/../touched\"; echo SOURCE-GATE\n",
    );
    let work = mk("work", "echo WORK-GATE \"$@\"; exit 7\n");
    let nogate = mk("nogate", "");
    fs::remove_file(nogate.join("scripts/check.sh")).unwrap();
    let sub = work.join("scripts");
    let run = |cwd: &Path, args: &[&str], stdin: &[u8]| {
        let (c, o, e) = run_hook(
            "pre-push-check",
            &argv(args),
            stdin,
            cwd,
            &source,
            &AtomicBool::new(false),
        );
        (
            c,
            String::from_utf8_lossy(&o).into_owned(),
            String::from_utf8_lossy(&e).into_owned(),
        )
    };
    let (code, out, _) = run(&sub, &["--full"], b"");
    assert_eq!((code, out.trim()), (7, "WORK-GATE"));
    assert!(!source.join("touched").exists());
    let line = format!(
        "refs/heads/x {} refs/heads/x {}\n",
        "a".repeat(40),
        "b".repeat(40)
    );
    for args in [&["--full"][..], &["--full", "origin"][..]] {
        let (code, _, err) = run(&nogate, args, line.as_bytes());
        assert_ne!(code, 0, "{err}");
        assert!(err.contains("gate unproved"), "{err}");
    }
    let (code, _, err) = run(&nogate, &["--full"], b"");
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("gate unproved"));
    let fake = tmp.path().join("fake");
    fs::create_dir_all(fake.join("scripts")).unwrap();
    fs::write(fake.join(".git"), "not a gitfile\n").unwrap();
    fs::write(
        fake.join("scripts/check.sh"),
        "touch \"$(dirname \"$0\")/../ran\"\n",
    )
    .unwrap();
    for (args, stdin) in [
        (&["--full"][..], &b""[..]),
        (&["--full"][..], line.as_bytes()),
        (&["origin"][..], line.as_bytes()),
    ] {
        let (code, _, err) = run(&fake, args, stdin);
        assert_ne!(code, 0, "{err}");
        assert!(err.contains("gate unproved"), "{err}");
        assert!(!fake.join("ran").exists());
    }
    let tag = format!(
        "refs/tags/t {} refs/tags/t {}\n",
        "a".repeat(40),
        "0".repeat(40)
    );
    let (code, out, _) = run(&fake, &["origin"], tag.as_bytes());
    assert_ne!(code, 0, "{out}");
    assert!(!out.contains("gate skipped"));
    let (code, out, _) = run(
        &nogate,
        &["origin"],
        format!(
            "(delete) {} refs/heads/x {}\n",
            "0".repeat(40),
            "a".repeat(40)
        )
        .as_bytes(),
    );
    assert_eq!(code, 0);
    assert!(out.contains("deletion-only"));
}
