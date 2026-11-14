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
    assert!(
        String::from_utf8_lossy(&stdout).contains("context"),
        "stdout: {}",
        String::from_utf8_lossy(&stdout)
    );

    let calls = host.detached();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "session-handoff");
    assert_eq!(calls[0].1[0], "capture");
    assert!(calls[0].1.contains(&"--agent".to_owned()));
    assert!(calls[0].1.contains(&"codex".to_owned()));
    assert!(calls[0].1.contains(&"sess-router-1".to_owned()));
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

fn harness(tmp: &TempDir, tool: &str, payload: &serde_json::Value) -> (i32, String) {
    let (code, out, _) = hook(tmp, "harness-hook", &[tool], payload.to_string().as_bytes());
    (code, out)
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
    let payload = serde_json::json!({"hook_event_name":"UserPromptSubmit","cwd":repo,"session_id":"pi-2","prompt":"hi"});
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
    fs::create_dir_all(native.join("rust")).unwrap();
    fs::write(native.join("rust/Cargo.toml"), "").unwrap();
    assert_eq!(row(&native).as_deref(), Some("resume-hook.sh"));
    let cargo_only = tmp.path().join("cargo-only");
    fs::create_dir_all(cargo_only.join("rust")).unwrap();
    fs::write(cargo_only.join("rust/Cargo.toml"), "").unwrap();
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
    assert!(prompt(&tmp).contains("You are a control-tower main"));
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
        assert!(first.contains("You are a control-tower main"), "{source}");
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
