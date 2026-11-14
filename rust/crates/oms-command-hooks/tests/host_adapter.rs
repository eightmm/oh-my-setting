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

#[test]
fn skill_router_detaches_context_pressure_handoff_capture_via_host_services() {
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
