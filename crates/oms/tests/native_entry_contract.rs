use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
static OMS_DIGEST: OnceLock<String> = OnceLock::new();

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oms-native-entry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let revision = "1111111111111111111111111111111111111111";
        std::fs::write(root.join(".git/HEAD"), format!("{revision}\n")).unwrap();
        let dir = root.join("target/oms-native").join(revision);
        std::fs::create_dir_all(&dir).unwrap();
        // Copy out-of-process so concurrent test threads doing fork()+exec()
        // never inherit a writable FD to the copied executable (ETXTBSY).
        assert!(
            Command::new("cp")
                .arg(env!("CARGO_BIN_EXE_oms"))
                .arg(dir.join("oms"))
                .status()
                .unwrap()
                .success()
        );
        let digest = OMS_DIGEST.get_or_init(|| {
            format!(
                "{:x}",
                Sha256::digest(std::fs::read(env!("CARGO_BIN_EXE_oms")).unwrap())
            )
        });
        std::fs::write(dir.join("manifest.json"), format!("{{\"schema\":1,\"source_commit\":\"{revision}\",\"artifacts\":{{\"oms\":\"{digest}\"}}}}" )).unwrap();
        std::fs::write(dir.join("oms.sha256"), format!("{digest}\n")).unwrap();
        std::fs::create_dir(root.join("scripts")).unwrap();
        let source = std::env::var_os("OMS_CONTRACT_REPO")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        std::fs::copy(source.join("scripts/oms"), root.join("scripts/oms")).unwrap();
        std::fs::create_dir(root.join("scripts/lib")).unwrap();
        std::fs::copy(
            source.join("scripts/lib/platform.sh"),
            root.join("scripts/lib/platform.sh"),
        )
        .unwrap();
        Self(root)
    }
    fn binary(&self) -> PathBuf {
        self.0
            .join("target/oms-native/1111111111111111111111111111111111111111/oms")
    }
    fn tool_path(&self) -> PathBuf {
        use std::os::unix::fs::symlink;
        let bin = self.0.join("tools");
        std::fs::create_dir(&bin).unwrap();
        for name in [
            "bash", "git", "dirname", "uname", "readlink", "awk", "wc", "sort", "env",
        ] {
            let path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
                .map(|p| p.join(name))
                .find(|p| p.is_file())
                .unwrap();
            symlink(std::fs::canonicalize(path).unwrap(), bin.join(name)).unwrap();
        }
        let paths = std::env::var_os("PATH").unwrap();
        let (name, executable) = ["sha256sum", "shasum", "openssl"]
            .into_iter()
            .find_map(|name| {
                std::env::split_paths(&paths)
                    .map(|p| p.join(name))
                    .find(|p| p.is_file())
                    .map(|p| (name, p))
            })
            .expect("portable SHA-256 tool required by the public bootstrap");
        symlink(std::fs::canonicalize(executable).unwrap(), bin.join(name)).unwrap();
        bin
    }
    fn guarded_tools(&self) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = self.tool_path();
        let source = std::env::var_os("OMS_CONTRACT_REPO")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        for name in [
            "agent-task.sh",
            "runtime.sh",
            "artifact-index.sh",
            "agent-role.sh",
            "input.sh",
            "agent-events.sh",
            "job-digest.sh",
            "lib/platform.sh",
        ] {
            std::fs::copy(
                source.join("scripts").join(name),
                self.0.join("scripts").join(name),
            )
            .unwrap();
        }
        std::fs::write(
            bin.join("python3"),
            "#!/bin/sh\n: > \"$OMS_TEST_PREFLIGHT\"\nexit 99\n",
        )
        .unwrap();
        std::fs::set_permissions(bin.join("python3"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        for name in ["data", "home", "global"] {
            std::fs::create_dir(self.0.join(name)).unwrap();
        }
        assert!(
            self.command(&bin, &bin.join("git"))
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        bin
    }
    fn command(&self, bin: &std::path::Path, program: &std::path::Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.0.join("data"))
            .env("PATH", bin)
            .env("HOME", self.0.join("home"))
            .env("OH_MY_SETTING_ROLES_DIR", self.0.join("global"))
            .env("OMS_PYTHON_RUNTIME_ROOT", self.0.join("missing-python"))
            .env("OMS_TEST_PREFLIGHT", self.0.join("preflight"))
            .env("OMS_HARNESS_CHILD", "0")
            .env("OMS_HARNESS_DELEGATE_DEPTH", "0");
        for key in [
            "OMS_ROOM_REPO",
            "OMS_ROOM_ID",
            "OMS_ROOM_PARTICIPANT",
            "OMS_FAIL_LEDGER",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
        ] {
            command.env_remove(key);
        }
        command
    }
    fn public(&self, bin: &std::path::Path) -> Command {
        let mut command = self.command(bin, &bin.join("bash"));
        command.arg(self.0.join("scripts/oms"));
        command
    }
    fn private(&self, bin: &std::path::Path, mode: &str) -> Command {
        let mut command = self.command(bin, &self.binary());
        command
            .arg(mode)
            .arg(&self.0)
            .args(["1111111111111111111111111111111111111111", "--"]);
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn exact(output: std::process::Output, code: i32, stdout: &[u8], stderr: &[u8]) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(stderr)
    );
    assert_eq!(output.stdout, stdout);
    assert_eq!(output.stderr, stderr);
}

fn files(root: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        found: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            found.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                if path.is_dir() {
                    vec![]
                } else {
                    std::fs::read(&path).unwrap()
                },
            );
            if path.is_dir() {
                visit(root, &path, found);
            }
        }
    }
    let mut found = std::collections::BTreeMap::new();
    visit(root, root, &mut found);
    found
}

#[test]
fn dispatch_availability_does_not_replace_incoming_source_proof() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let bin = f.tool_path();
    for name in ["data", "home", "global", "config", "state", "cache"] {
        std::fs::create_dir(f.0.join(name)).unwrap();
        std::fs::set_permissions(f.0.join(name), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::set_permissions(&f.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let run = |args: &[&str]| {
        f.private(&bin, "--oms-dispatch")
            .env("XDG_CONFIG_HOME", f.0.join("config"))
            .env("XDG_DATA_HOME", f.0.join("data"))
            .env("XDG_STATE_HOME", f.0.join("state"))
            .env("XDG_CACHE_HOME", f.0.join("cache"))
            .env("OMS_INSTALL_RECEIPT", f.0.join("config/install.json"))
            .env("TMPDIR", &f.0)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(run(&["list"]).status.success());
    let before = files(&f.0.join("home"));
    let out = run(&["link", "--plan"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("native payload/source proof failed"));
    assert_eq!(files(&f.0.join("home")), before);
    assert!(!f.0.join("config/install.json").exists());
    assert!(files(&f.0.join("data")).is_empty());
}

#[test]
fn native_state_views_use_fresh_canonical_runtime_evidence() {
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let repo = f.0.join("data");
    let project = repo.join("PROJECT.md");
    std::fs::write(
        &project,
        "# Project\n\n## Acceptance\n- A canonical criterion is visible.\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let output = f
            .public(&bin)
            .current_dir(&f.0)
            .env("OMS_AGENT_TASK_TTL", "604800")
            .args(args)
            .output()
            .unwrap();
        assert!(
            !output.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let path = repo.to_str().unwrap();
    let canonical = run(&["runtime", "envelope", "show", "--repo", path]);
    assert_eq!(canonical["criteria"].as_array().unwrap().len(), 1);
    assert!(!canonical["next_actions"].as_array().unwrap().is_empty());
    let state = run(&["state", "--repo", path, "--json"]);
    assert_eq!(state["runtime"]["healthy"], true);
    assert_eq!(state["runtime"]["criteria"], canonical["criteria"]);
    assert_eq!(state["runtime"]["next_actions"], canonical["next_actions"]);
    let dashboard = run(&["dashboard", "--repo", path, "--json"]);
    assert_eq!(dashboard["acceptance"]["criteria"], canonical["criteria"]);
    // Each command reads current producers, not the preceding rendered envelope.
    std::fs::write(&project, "# Project\n\n## Acceptance\n- A revised canonical criterion is visible.\n- Another criterion is visible.\n").unwrap();
    let refreshed = run(&["state", "--repo", path, "--json"]);
    assert_eq!(
        refreshed["runtime"]["criteria"].as_array().unwrap().len(),
        2
    );
    assert_ne!(refreshed["runtime"]["criteria"], canonical["criteria"]);

    std::fs::write(&project, "# Project\n\n## 목표\nProject objective\n").unwrap();
    let plan = repo.join(".oms/plan/tasks.json");
    let packet = repo.join(".oms/task/current.md");
    std::fs::create_dir_all(plan.parent().unwrap()).unwrap();
    std::fs::create_dir_all(packet.parent().unwrap()).unwrap();
    let active_plan = r#"{"schema":3,"goal":"Plan objective","tasks":{"one":{"id":"one","title":"Task one","state":"ready","depends":[]}}}"#;
    std::fs::write(&plan, active_plan).unwrap();
    let recent = oms_command_room::now();
    for (activity, expected, source) in [
        (recent.as_str(), "Packet objective", "task"),
        ("2000-01-01T00:00:00Z", "Plan objective", "plan"),
        ("", "Plan objective", "plan"),
        ("bad-date", "Plan objective", "plan"),
        ("2999-01-01T00:00:00Z", "Plan objective", "plan"),
    ] {
        let raw = format!(
            "# Active Task\n- task_id: handoff\n- status: active\n- updated: {activity}\n\n## Goal\nPacket objective\n"
        );
        std::fs::write(&packet, &raw).unwrap();
        let objective = serde_json::json!({"text":expected,"source":source});
        let envelope = run(&["runtime", "envelope", "show", "--repo", path]);
        assert_eq!(envelope["objective"], objective, "{envelope}");
        assert_eq!(envelope["task"]["goal"], "Packet objective");
        let state = run(&["state", "--repo", path, "--json"]);
        assert_eq!(state["runtime"]["objective"], objective);
        assert_eq!(state["task"]["goal"], "Packet objective");
        assert_eq!(
            run(&["dashboard", "--repo", path, "--json"])["goal"],
            objective
        );
        let panel = run(&["dashboard", "--repo", path, "--json", "--panel-snapshot"]);
        assert_eq!(panel["dashboard"]["goal"], expected);
        assert_eq!(std::fs::read_to_string(&packet).unwrap(), raw);
        assert_eq!(std::fs::read_to_string(&plan).unwrap(), active_plan);
    }
    let empty_plan = r#"{"schema":3,"goal":"Original plan objective","tasks":{}}"#;
    std::fs::write(&plan, empty_plan).unwrap();
    let envelope = run(&["runtime", "envelope", "show", "--repo", path]);
    assert_eq!(
        envelope["objective"],
        serde_json::json!({"text":"Project objective","source":"project"})
    );
    assert_eq!(std::fs::read_to_string(&plan).unwrap(), empty_plan);
}

#[test]
fn public_entry_dispatches_without_python() {
    let f = Fixture::new();
    use std::os::unix::fs::PermissionsExt;
    let bin = f.tool_path();
    let python = bin.join("python3");
    std::fs::write(&python, "#!/bin/sh\n: > \"$OMS_TEST_PREFLIGHT\"\nexit 99\n").unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(bin.join("bash"))
        .arg(f.0.join("scripts/oms"))
        .args(["runtime", "context-inspect", "--help"])
        .env("PATH", &bin)
        .env("OMS_TEST_PREFLIGHT", f.0.join("preflight"))
        .env("OMS_PYTHON_RUNTIME_ROOT", f.0.join("missing"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("context-inspect"));
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn dispatch_owns_runtime_subcommands_and_leaves_unowned_tools_to_legacy() {
    let f = Fixture::new();
    let owned = Command::new(f.binary())
        .arg("--oms-dispatch")
        .arg(&f.0)
        .args([
            "1111111111111111111111111111111111111111",
            "--",
            "runtime",
            "envelope",
            "--help",
        ])
        .output()
        .unwrap();
    assert_eq!(owned.status.code(), Some(0));
    assert!(!owned.stdout.is_empty());

    for sub in ["ssh-adapter", "exec-snapshot", "resources"] {
        let out = Command::new(f.binary())
            .arg("--oms-dispatch")
            .arg(&f.0)
            .args([
                "1111111111111111111111111111111111111111",
                "--",
                "runtime",
                sub,
                "--help",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{sub}");
        assert!(!out.stdout.is_empty(), "{sub}");
    }

    let unowned = Command::new(f.binary())
        .arg("--oms-dispatch")
        .arg(&f.0)
        .args([
            "1111111111111111111111111111111111111111",
            "--",
            "unowned-legacy-probe",
            "--help",
        ])
        .output()
        .unwrap();
    assert_eq!(unowned.status.code(), Some(75));
    assert!(unowned.stdout.is_empty());
    assert!(unowned.stderr.is_empty());
}

#[test]
fn public_source_symlink_and_copy_use_native_without_interpreter() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let linked = f.0.join("linked");
    symlink(f.0.join("scripts/oms"), &linked).unwrap();
    let copied = f.0.join("copy");
    let quoted = f.0.join("source ' with spaces");
    std::fs::create_dir(&quoted).unwrap();
    symlink(f.0.join("scripts/oms"), quoted.join("oms")).unwrap();
    let rendered = Command::new("bash")
        .args([
            "-c",
            "printf '#!/usr/bin/env bash\\nexec \"${BASH:-bash}\" %q \"$@\"\\n' \"$1\"",
            "renderer",
        ])
        .arg(quoted.join("oms"))
        .output()
        .unwrap();
    assert!(rendered.status.success());
    std::fs::write(&copied, rendered.stdout).unwrap();
    for entry in [&f.0.join("scripts/oms"), &linked, &copied] {
        let out = Command::new("bash")
            .arg(entry)
            .args([
                "runtime",
                "--repo=context-inspect",
                "context-inspect",
                "--help",
            ])
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("context-inspect"));
        for args in [
            vec!["runtime", "experiment", "template", "--help"],
            vec!["artifact-index", "telemetry", "--help"],
            vec!["agent-role", "--help"],
            vec!["input", "attach", "file", "--help"],
            vec!["agent-events", "show", "--bundle", "--help"],
        ] {
            let output = Command::new("bash")
                .arg(entry)
                .args(args)
                .env("PATH", &bin)
                .env("OMS_TEST_PREFLIGHT", f.0.join("preflight"))
                .env("OMS_PYTHON_RUNTIME_ROOT", f.0.join("missing-python"))
                .env("OMS_HARNESS_CHILD", "0")
                .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!output.stdout.is_empty());
            assert!(output.stderr.is_empty());
        }
    }
    let out = Command::new("bash")
        .arg(&copied)
        .args(["runtime", "context-inspect", "--session", "missing"])
        .env("PATH", &bin)
        .env("OMS_HARNESS_CHILD", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("a harness child"));
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn corrupt_manifest_fails_closed() {
    let f = Fixture::new();
    let bin = f.tool_path();
    let path = f.binary().parent().unwrap().join("manifest.json");
    let valid = std::fs::read(&path).unwrap();
    for invalid in ["", "{}", "{", "{\"schema\":\"1\"}"] {
        std::fs::write(&path, invalid).unwrap();
        let out = Command::new("bash")
            .arg(f.0.join("scripts/oms"))
            .args(["runtime", "context-inspect", "--help"])
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("native manifest"));
    }
    let mut padded = valid;
    padded.resize(4 * 1024 * 1024, b' ');
    std::fs::write(&path, &padded).unwrap();
    let call = || {
        Command::new("bash")
            .arg(f.0.join("scripts/oms"))
            .args(["runtime", "context-inspect", "--help"])
            .env("PATH", &bin)
            .output()
            .unwrap()
    };
    assert!(call().status.success());
    padded.push(b' ');
    std::fs::write(&path, padded).unwrap();
    let out = call();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn nonfinite_manifest_values_are_rejected() {
    let f = Fixture::new();
    let path = f.binary().parent().unwrap().join("manifest.json");
    let valid = std::fs::read_to_string(&path).unwrap();
    for constant in ["NaN", "Infinity", "-Infinity"] {
        std::fs::write(
            &path,
            format!("{},\"extra\":{constant}}}", &valid[..valid.len() - 1]),
        )
        .unwrap();
        let out = Command::new(f.binary())
            .arg("--oms-dispatch")
            .arg(&f.0)
            .args([
                "1111111111111111111111111111111111111111",
                "--",
                "runtime",
                "context-inspect",
                "--help",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("invalid native manifest"));
    }
}

#[test]
fn manifest_extra_large_integer_is_valid() {
    let f = Fixture::new();
    let path = f.binary().parent().unwrap().join("manifest.json");
    let valid = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!(
            "{},\"extra\":{}}}",
            &valid[..valid.len() - 1],
            "9".repeat(5000)
        ),
    )
    .unwrap();
    let out = Command::new(f.binary())
        .arg("--oms-dispatch")
        .arg(&f.0)
        .args([
            "1111111111111111111111111111111111111111",
            "--",
            "runtime",
            "context-inspect",
            "--help",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn root_option_errors_and_raw_paths_stay_native() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let bin = f.tool_path();
    for args in [
        vec![
            "runtime".into(),
            "--repo".into(),
            "--pretty".into(),
            "context-inspect".into(),
        ],
        vec![
            "runtime".into(),
            "context-inspect".into(),
            "--limit".into(),
            "0".into(),
        ],
        vec![
            "runtime".into(),
            "context-inspect".into(),
            "--limit".into(),
            "malformed".into(),
        ],
        vec![
            "runtime".into(),
            "--repo".into(),
            std::ffi::OsString::from_vec(b"bad\xff".to_vec()),
            "context-inspect".into(),
            "--session".into(),
            "missing".into(),
        ],
    ] {
        let out = Command::new("bash")
            .arg(f.0.join("scripts/oms"))
            .args(args)
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains("python"));
    }
}

#[test]
fn root_errors_match_standalone_without_legacy_fallback() {
    let f = Fixture::new();
    let bin = f.tool_path();
    for args in [
        vec!["runtime", "--repo", "-h", "context-inspect"],
        vec!["runtime", "--repo", "--help"],
        vec!["runtime", "--repo", "--version"],
        vec!["runtime", "-h=foo", "context-inspect"],
        vec!["runtime", "--help=foo", "context-inspect"],
        vec!["runtime", "--version=foo", "context-inspect"],
        vec!["runtime", "--", "-h", "context-inspect"],
        vec!["runtime", "--", "--unknown", "context-inspect"],
        vec!["runtime", "-", "context-inspect"],
    ] {
        let expected = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(&args)
            .output()
            .unwrap();
        let actual = Command::new("bash")
            .arg(f.0.join("scripts/oms"))
            .args(&args)
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert_eq!(
            actual.status.code(),
            expected.status.code(),
            "{args:?}: {}",
            String::from_utf8_lossy(&actual.stderr)
        );
        assert_eq!(actual.stdout, expected.stdout, "{args:?}");
        assert_eq!(actual.stderr, expected.stderr, "{args:?}");
    }
}

#[test]
fn foreign_git_environment_does_not_choose_payload() {
    let f = Fixture::new();
    let bin = f.tool_path();
    let foreign = f.0.join("other");
    std::fs::create_dir(&foreign).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&foreign)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(foreign.join(".git/HEAD"), format!("{}\n", "2".repeat(40))).unwrap();
    let out = Command::new("bash")
        .arg(f.0.join("scripts/oms"))
        .args(["runtime", "context-inspect", "--help"])
        .env("PATH", bin)
        .env("GIT_DIR", foreign.join(".git"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn missing_revision_uses_legacy_but_corrupt_present_entry_fails() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let bin = f.tool_path();
    std::fs::write(f.0.join("scripts/agent-task.sh"), "").unwrap();
    std::fs::write(f.0.join("scripts/runtime.sh"), "printf 'legacy\\n'\n").unwrap();
    let current = f.binary().parent().unwrap().to_path_buf();
    let old = current.with_file_name("2222222222222222222222222222222222222222");
    std::fs::rename(&current, &old).unwrap();
    let call = || {
        Command::new("bash")
            .arg(f.0.join("scripts/oms"))
            .args(["runtime", "--help"])
            .env("PATH", &bin)
            .output()
            .unwrap()
    };
    let out = call();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"legacy\n");
    symlink(&old, &current).unwrap();
    let out = call();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn private_dispatch_accepts_symlinked_checkout_parent() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let alias = f.0.join("alias");
    symlink(&f.0, &alias).unwrap();
    let out = Command::new(f.binary())
        .arg("--oms-dispatch")
        .arg(alias)
        .args([
            "1111111111111111111111111111111111111111",
            "--",
            "runtime",
            "context-inspect",
            "--help",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn public_entry_executes_native_context_modes() {
    let f = Fixture::new();
    let bin = f.tool_path();
    std::fs::write(f.0.join("session.jsonl"), b"{}\n").unwrap();
    for args in [
        vec!["--session", "session.jsonl"],
        vec!["--session", "session.jsonl", "--timeline"],
        vec!["--injections"],
        vec!["--injections", "--limit", "100"],
    ] {
        let out = Command::new("bash")
            .arg(f.0.join("scripts/oms"))
            .current_dir(&f.0)
            .args(["runtime", "--repo"])
            .arg(&f.0)
            .arg("context-inspect")
            .args(args)
            .env("PATH", &bin)
            .env_remove("OMS_HARNESS_CHILD")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(serde_json::from_slice::<serde_json::Value>(&out.stdout).is_ok());
    }
}

#[test]
fn public_entry_forwards_interrupt_to_native_command() {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let bin = f.tool_path();
    let git = bin.join("git");
    let real_git = std::fs::canonicalize(&git).unwrap();
    std::fs::remove_file(&git).unwrap();
    std::fs::write(&git, format!("#!/bin/sh\ncase \"$*\" in *'rev-parse --verify HEAD'*) printf '%s\\n' 1111111111111111111111111111111111111111;; *'rev-parse --show-toplevel'*) read -r unused < \"$OMS_TEST_FIFO\";; *) exec '{}' \"$@\";; esac\n", real_git.display())).unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fifo = f.0.join("git-config");
    nix::unistd::mkfifo(
        &fifo,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let mut child = Command::new("bash")
        .arg(f.0.join("scripts/oms"))
        .args(["runtime", "--repo"])
        .arg(&f.0)
        .args(["context-inspect", "missing"])
        .env("PATH", &bin)
        .env("OMS_TEST_FIFO", &fifo)
        .env_remove("OMS_HARNESS_CHILD")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let timeout = std::env::var("OMS_TEST_CALL_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let writer = loop {
        if let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(&fifo)
        {
            break file;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("native command exited before opening fixture: {status}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("git did not open fixture");
        }
        std::thread::yield_now();
    };
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native command did not stop");
        }
        std::thread::yield_now();
    }
    drop(writer);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(130));
    assert!(out.stdout.is_empty());
    assert_eq!(out.stderr, b"runtime-core: interrupted\n");
}

#[test]
fn tampered_executable_is_rejected_before_execution() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let bin = f.tool_path();
    let marker = f.0.join("unrelated-executed");
    std::fs::write(
        f.binary(),
        b"#!/bin/sh\nprintf executed > \"$OMS_UNRELATED_MARKER\"\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(f.binary(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new("bash")
        .arg(f.0.join("scripts/oms"))
        .args(["runtime", "context-inspect", "--help"])
        .env("PATH", &bin)
        .env("OMS_UNRELATED_MARKER", &marker)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("checksum mismatch"));
    assert!(
        !marker.exists(),
        "unrelated candidate executed before validation"
    );
}

#[test]
fn missing_or_malformed_preexecution_checksum_fails_closed() {
    let f = Fixture::new();
    let bin = f.tool_path();
    let checksum = f.binary().parent().unwrap().join("oms.sha256");
    let call = || {
        Command::new("bash")
            .arg(f.0.join("scripts/oms"))
            .args(["runtime", "context-inspect", "--help"])
            .env("PATH", &bin)
            .output()
            .unwrap()
    };
    for invalid in [
        Vec::new(),
        vec![b'a'; 64],
        vec![b'a'; 66],
        [vec![b'z'; 64], vec![b'\n']].concat(),
        [vec![b'0'; 64], vec![b'\n']].concat(),
    ] {
        std::fs::write(&checksum, invalid).unwrap();
        let out = call();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
    std::fs::remove_file(&checksum).unwrap();
    let out = call();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn public_failure_namespace_runs_without_python_and_preserves_caller_directory() {
    let cases: &[(&[&str], i32, &str)] = &[
        (
            &["runtime", "--foo bar", "failure", "catalog"],
            2,
            "argument command: invalid choice: '--foo bar'",
        ),
        (&["runtime", "failure", "catalog"], 0, "provider_capacity"),
        (
            &["runtime", "failure", "classify", "-fail now"],
            0,
            "unknown_failure",
        ),
        (
            &[
                "runtime", "--repo", "chosen", "--pretty", "failure", "catalog",
            ],
            0,
            "\n  ",
        ),
        (
            &[
                "runtime", "--repo", "missing", "failure", "--repo", "chosen", "catalog",
            ],
            0,
            "provider_capacity",
        ),
        (
            &["runtime", "--repo", "-a b", "failure", "catalog"],
            2,
            "repository path is not a directory: -a b",
        ),
        (
            &["runtime", "--bogus", "failure", "classify", "text"],
            2,
            "unrecognized arguments: --bogus",
        ),
        (&["runtime", "failure", "unknown"], 2, "invalid choice"),
    ];
    for (argv, status, fragment) in cases {
        let f = Fixture::new();
        let bin = f.tool_path();
        let data = f.0.join("data");
        std::fs::create_dir_all(data.join("chosen")).unwrap();
        let output = Command::new(bin.join("bash"))
            .arg(f.0.join("scripts/oms"))
            .args(*argv)
            .current_dir(&data)
            .env("PATH", &bin)
            .env("OMS_PYTHON_RUNTIME_ROOT", f.0.join("missing-python"))
            .env("OMS_HARNESS_CHILD", "1")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(*status),
            "{argv:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let bytes = if *status == 0 {
            &output.stdout
        } else {
            &output.stderr
        };
        assert!(
            String::from_utf8_lossy(bytes).contains(fragment),
            "{argv:?}: {}",
            String::from_utf8_lossy(bytes)
        );
        if *status == 0 {
            assert!(output.stderr.is_empty());
        } else {
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
fn public_telemetry_normal_and_grouped_match_canonical_goldens() {
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let data = f.0.join("data");
    std::fs::create_dir_all(data.join(".oms/artifacts")).unwrap();
    let fixtures: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../oms-command-telemetry/tests/fixtures.json"
    ))
    .unwrap();
    for fixture in fixtures
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| matches!(v["name"].as_str(), Some("empty" | "minimal")))
    {
        std::fs::write(
            data.join(".oms/artifacts/index.jsonl"),
            fixture["index"].as_str().unwrap(),
        )
        .unwrap();
        let before = files(&data);
        for case in fixture["cases"].as_array().unwrap() {
            for (json, expected) in [(false, "human"), (true, "json")] {
                let mut command = f.public(&bin);
                command.args(["artifact-index", "--repo", ".", "telemetry"]);
                command.args(
                    case["args"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap()),
                );
                if json {
                    command.arg("--json");
                }
                exact(
                    command.output().unwrap(),
                    0,
                    case[expected].as_str().unwrap().as_bytes(),
                    b"",
                );
            }
        }
        assert_eq!(files(&data), before);
    }
    exact(
        f.public(&bin)
            .args(["artifact-index", "telemetry", "--help"])
            .output()
            .unwrap(),
        0,
        include_bytes!("../../oms-command-telemetry/src/usage.txt"),
        b"",
    );
    exact(f.public(&bin).args(["artifact-index", "telemetry", "--by", "bad"]).output().unwrap(), 2, b"", b"error: --by must be day, model, provider, task, role or effort\nhint: run `oms artifact-index --help` for usage\n");
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn public_agent_role_uses_validated_source_for_all_seven_modes() {
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let data = f.0.join("data");
    for dir in [
        "roles",
        "config",
        "data/roles",
        "data/config",
        "data/.oms/roles",
    ] {
        std::fs::create_dir_all(f.0.join(dir)).unwrap();
    }
    for (name, bytes) in [
        (
            "panel-routing.json",
            include_bytes!("../../oms-command-agent-role/tests/data/panel-routing.json").as_slice(),
        ),
        (
            "role-contracts.json",
            include_bytes!("../../oms-command-agent-role/tests/data/role-contracts.json")
                .as_slice(),
        ),
    ] {
        std::fs::write(f.0.join("config").join(name), bytes).unwrap();
        std::fs::write(
            data.join("config").join(name),
            b"invalid data-repository policy",
        )
        .unwrap();
    }
    let renders: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../oms-command-agent-role/tests/data/render.json"
    ))
    .unwrap();
    let role = &renders[0];
    std::fs::write(f.0.join("roles/builder.md"), role["text"].as_str().unwrap()).unwrap();
    std::fs::write(data.join("roles/builder.md"), b"wrong source root\n").unwrap();
    std::fs::write(f.0.join("global/global.md"), b"global\n").unwrap();
    std::fs::write(data.join(".oms/roles/project.md"), b"project\n").unwrap();
    let before = files(&data);
    exact(
        f.public(&bin)
            .args(["agent-role", "list"])
            .output()
            .unwrap(),
        0,
        b"builder\nglobal\nproject\n",
        b"",
    );
    exact(
        f.public(&bin)
            .args(["agent-role", "show", "--name", "builder"])
            .output()
            .unwrap(),
        0,
        role["text"].as_str().unwrap().as_bytes(),
        b"",
    );
    for (mode, path) in [
        ("path", data.join(".oms/roles/builder.md")),
        ("resolve", f.0.join("roles/builder.md")),
    ] {
        exact(
            f.public(&bin)
                .args(["agent-role", mode, "--name", "builder"])
                .output()
                .unwrap(),
            0,
            format!("{}\n", path.display()).as_bytes(),
            b"",
        );
    }
    for provider in ["claude", "codex"] {
        exact(
            f.public(&bin)
                .args([
                    "agent-role",
                    "render",
                    "--name",
                    "builder",
                    "--provider",
                    provider,
                ])
                .output()
                .unwrap(),
            0,
            role[format!("{provider}-bundled")]
                .as_str()
                .unwrap()
                .as_bytes(),
            b"",
        );
    }
    for (extra, expected) in [
        (
            &[][..],
            include_bytes!("../../oms-command-agent-role/tests/data/surface.txt").as_slice(),
        ),
        (
            &["--json"][..],
            include_bytes!("../../oms-command-agent-role/tests/data/surface.json").as_slice(),
        ),
    ] {
        exact(
            f.public(&bin)
                .args(["agent-role", "surface"])
                .args(extra)
                .output()
                .unwrap(),
            0,
            expected,
            b"",
        );
    }
    assert_eq!(files(&data), before);
    for store in [data.join(".oms/roles"), f.0.join("global")] {
        let target = store.join("builder.md");
        std::fs::write(&target, role["text"].as_str().unwrap()).unwrap();
        for provider in ["claude", "codex"] {
            exact(
                f.public(&bin)
                    .args([
                        "agent-role",
                        "render",
                        "--name",
                        "builder",
                        "--provider",
                        provider,
                    ])
                    .output()
                    .unwrap(),
                0,
                role[format!("{provider}-override")]
                    .as_str()
                    .unwrap()
                    .as_bytes(),
                b"",
            );
        }
        std::fs::remove_file(target).unwrap();
    }
    let target = data.join(".oms/roles/new.md");
    exact(
        f.public(&bin)
            .args(["agent-role", "init", "--name", "new"])
            .output()
            .unwrap(),
        0,
        format!("role: created {}\n", target.display()).as_bytes(),
        b"",
    );
    let mut expected = before;
    expected.insert(PathBuf::from(".oms/.gitignore"), b"*\n".to_vec());
    expected.insert(PathBuf::from(".oms/roles/new.md"), b"# Role: new\n\nYou are acting as the \"new\" role for this task.\n\n## Mandate\n\n- Describe what this role does and does not do.\n\n## Rules\n\n- One actionable rule per line.\n\n## Output\n\n- State what the role must produce.\n".to_vec());
    assert_eq!(files(&data), expected);
    exact(
        f.public(&bin)
            .args(["agent-role", "init", "--name", "new"])
            .output()
            .unwrap(),
        2,
        b"",
        format!(
            "error: role already exists: {}\nhint: run `oms agent-role --help` for usage\n",
            target.display()
        )
        .as_bytes(),
    );
    exact(f.public(&bin).args(["agent-role", "init", "--global", "--name", "denied"]).env("OMS_HARNESS_CHILD", "1").output().unwrap(), 2, b"", b"error: a harness child cannot mutate parent-owned host or global state; return the request to the parent agent\nhint: run `oms agent-role --help` for usage\n");
    assert!(!f.0.join("global/denied.md").exists());
    exact(f.command(&bin, &PathBuf::from(env!("CARGO_BIN_EXE_oms"))).args(["agent-role", "--help"]).env("OMS_SOURCE_ROOT", &f.0).output().unwrap(), 2, b"", b"error: agent-role requires validated native source metadata\nhint: run `oms agent-role --help` for usage\n");
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn public_input_snapshots_and_cli_goldens_stay_native_for_parents() {
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let data = f.0.join("data");
    assert!(
        f.command(&bin, &bin.join("git"))
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    let cases: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../oms-command-input/tests/fixtures/cli.json"
    ))
    .unwrap();
    let before = files(&data);
    for case in cases.as_array().unwrap() {
        let args: Vec<_> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if !matches!(
            args.as_slice(),
            ["attach", "file" | "selection", "--help" | "--repo"]
                | ["attachment" | "attachments", "--help" | "--repo"]
        ) {
            continue;
        }
        let code = case["exit"].as_i64().unwrap() as i32;
        let error = format!(
            "{}{}",
            case["stderr"].as_str().unwrap(),
            if code == 2 {
                "hint: run `oms input --help` for usage\n"
            } else {
                ""
            }
        );
        exact(
            f.public(&bin).arg("input").args(args).output().unwrap(),
            code,
            case["stdout"].as_str().unwrap().as_bytes(),
            error.as_bytes(),
        );
    }
    assert_eq!(files(&data), before);
    std::fs::write(data.join("source.txt"), b"first\nlast\n").unwrap();
    let mut expected = files(&data);
    exact(
        f.public(&bin)
            .args(["input", "attachments"])
            .output()
            .unwrap(),
        0,
        b"{\n  \"attachments\": [],\n  \"schema\": 1\n}\n",
        b"",
    );
    for args in [
        vec!["attach", "file", "source.txt"],
        vec![
            "attach",
            "selection",
            "--source",
            "text",
            "--origin",
            "fixture",
            "--text",
            "selected",
        ],
    ] {
        let output = f.public(&bin).arg("input").args(args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let id = record["id"].as_str().unwrap();
        let folder = data.join(".oms/inputs/attachments").join(id);
        assert_eq!(
            std::fs::read(folder.join("meta.json")).unwrap(),
            output.stdout
        );
        for path in [".oms", ".oms/inputs", ".oms/inputs/attachments"] {
            expected.insert(PathBuf::from(path), vec![]);
        }
        expected.insert(PathBuf::from(".oms/inputs/.gitignore"), b"*\n".to_vec());
        expected.insert(folder.strip_prefix(&data).unwrap().to_path_buf(), vec![]);
        for name in ["meta.json", "content.txt"] {
            expected.insert(
                folder.join(name).strip_prefix(&data).unwrap().to_path_buf(),
                std::fs::read(folder.join(name)).unwrap(),
            );
        }
        assert_eq!(files(&data), expected);
        let before = files(&data);
        let retrieved = f
            .public(&bin)
            .args(["input", "attachment", id, "--full"])
            .output()
            .unwrap();
        assert_eq!(
            retrieved.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&retrieved.stderr)
        );
        assert!(retrieved.stderr.is_empty());
        let result: serde_json::Value = serde_json::from_slice(&retrieved.stdout).unwrap();
        assert_eq!(
            result["content"].as_str().unwrap().as_bytes(),
            std::fs::read(folder.join("content.txt")).unwrap()
        );
        let listing = f
            .public(&bin)
            .args(["input", "attachments"])
            .output()
            .unwrap();
        assert_eq!(listing.status.code(), Some(0));
        assert!(listing.stderr.is_empty());
        let result: serde_json::Value = serde_json::from_slice(&listing.stdout).unwrap();
        assert!(
            result["attachments"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == id)
        );
        assert_eq!(files(&data), before);
    }
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn public_attempt_bundle_matches_canonical_json_and_text() {
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let data = f.0.join("data");
    for (name, bytes) in [
        (
            ".oms/lifecycle/events.jsonl",
            include_bytes!("../../oms-command-attempt-bundle/tests/fixtures/events.jsonl")
                .as_slice(),
        ),
        (
            ".oms/artifacts/index.jsonl",
            include_bytes!("../../oms-command-attempt-bundle/tests/fixtures/artifacts.jsonl")
                .as_slice(),
        ),
        (
            ".oms/failures.jsonl",
            include_bytes!("../../oms-command-attempt-bundle/tests/fixtures/failures.jsonl")
                .as_slice(),
        ),
        (
            "transcript.jsonl",
            include_bytes!("../../oms-command-attempt-bundle/tests/fixtures/transcript.jsonl")
                .as_slice(),
        ),
    ] {
        let path = data.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let before = files(&data);
    for (extra, expected) in [
        (
            &[][..],
            include_bytes!("../../oms-command-attempt-bundle/tests/fixtures/bundle.txt").as_slice(),
        ),
        (
            &["--json"][..],
            include_bytes!("../../oms-command-attempt-bundle/tests/fixtures/bundle.json")
                .as_slice(),
        ),
    ] {
        exact(
            f.public(&bin)
                .args([
                    "agent-events",
                    "--repo",
                    ".",
                    "show",
                    "--attempt",
                    "attempt1",
                    "--bundle",
                    "--transcript",
                    "transcript.jsonl",
                ])
                .args(extra)
                .output()
                .unwrap(),
            0,
            expected,
            b"",
        );
    }
    exact(
        f.public(&bin)
            .args(["agent-events", "show", "--bundle", "--help"])
            .output()
            .unwrap(),
        0,
        include_bytes!("../../oms-command-attempt-bundle/src/help.txt"),
        b"",
    );
    exact(f.public(&bin).args(["agent-events", "show", "--bundle"]).output().unwrap(), 2, b"", b"usage: agent-events.sh show [-h] --attempt ATTEMPT [--json] [--bundle]\n                            [--transcript PATH]\nagent-events.sh show: error: the following arguments are required: --attempt\nhint: run `oms agent-events --help` for usage\n");
    assert_eq!(files(&data), before);
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn selection_is_inert_and_unowned_dispatch_returns_empty_75() {
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let data = f.0.join("data");
    std::fs::create_dir(data.join(".oms")).unwrap();
    std::fs::write(data.join(".oms/inputs"), b"must never be read as state").unwrap();
    let before = files(&data);
    for argv in [
        vec![""],
        vec!["artifact-index.sh", "telemetry"],
        vec!["unowned-legacy-probe", "--help"],
    ] {
        for mode in ["--oms-select", "--oms-dispatch"] {
            exact(
                f.private(&bin, mode).args(&argv).output().unwrap(),
                75,
                b"",
                b"",
            );
        }
    }
    exact(
        f.private(&bin, "--oms-select")
            .args(["input", "attachment", "missing"])
            .env_remove("OMS_HARNESS_CHILD")
            .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
            .output()
            .unwrap(),
        0,
        b"",
        b"",
    );
    for argv in [
        vec!["runtime", "experiment", "launch", "--help"],
        vec![
            "runtime",
            "--repo",
            "template",
            "experiment",
            "show",
            "template",
        ],
        vec!["job-digest", "123", "missing.log"],
        vec!["slurm-lint"],
        vec!["slurm-lint", "missing.sh"],
        vec!["slurm-lint", "--template"],
        vec!["slurm-lint", "--snapshot"],
        vec!["slurm-lint.sh", "--unknown"],
        vec!["artifact-index", "telemetry", "--repo", "absent"],
        vec!["artifact-index", "telemetry", "--by", "bad"],
        vec!["artifact-index", "telemetry", "--waste"],
        vec!["artifact-index", "list"],
        vec!["agent-role", "init", "--name", "never-created"],
        vec!["agent-role", "surface"],
        vec!["input", "--help"],
        vec!["input", "attach", "terminal"],
        vec!["input", "list"],
        vec!["input", "attach", "file", "absent"],
        vec!["input", "attachment", "missing"],
        vec![
            "runtime",
            "experiment",
            "template",
            "--output",
            "never-created.json",
        ],
        vec!["runtime", "experiment", "validate", "absent"],
        vec![
            "runtime",
            "--first",
            "experiment",
            "compare",
            "absent",
            "absent",
            "--last",
        ],
        vec!["agent-events", "show", "--attempt", "missing"],
        vec!["agent-events", "list"],
        vec!["agent-events", "show", "--attempt", "missing", "--bundle"],
    ] {
        exact(
            f.private(&bin, "--oms-select").args(argv).output().unwrap(),
            0,
            b"",
            b"",
        );
    }
    // Child restrictions are native: selection owns every child invocation and
    // dispatch reaches the native room-membership refusal, never Python.
    let refusal =
        "error: worker attachment reads require an admitted room repository and participant\n";
    for (child, depth) in [
        ("1", "0"),
        ("0", "1"),
        ("0", "2"),
        ("0", ""),
        ("0", "bad"),
        ("0", "0"),
    ] {
        let run = |mode: &str| {
            f.private(&bin, mode)
                .args(["input", "attachment", "missing", "--full"])
                .env("OMS_HARNESS_CHILD", child)
                .env("OMS_HARNESS_DELEGATE_DEPTH", depth)
                .output()
                .unwrap()
        };
        exact(run("--oms-select"), 0, b"", b"");
        if (child, depth) != ("0", "0") {
            let out = run("--oms-dispatch");
            assert_eq!(out.status.code(), Some(2), "{child}/{depth}");
            assert!(out.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&out.stderr).starts_with(refusal),
                "{child}/{depth}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    assert_eq!(files(&data), before);
    exact(
        f.public(&bin)
            .args(["input", "attachment", "missing", "--full"])
            .env("OMS_HARNESS_CHILD", "1")
            .output()
            .unwrap(),
        2,
        b"",
        format!("{refusal}hint: run `oms input --help` for usage\n").as_bytes(),
    );
    assert!(!f.0.join("preflight").exists());
    assert_eq!(files(&data), before);
}

#[test]
fn public_owned_status_75_and_stream_failures_never_retry() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let bin = f.guarded_tools();
    std::fs::remove_file(bin.join("sort")).unwrap();
    std::fs::write(bin.join("sort"), "#!/bin/sh\nexit 75\n").unwrap();
    std::fs::set_permissions(bin.join("sort"), std::fs::Permissions::from_mode(0o755)).unwrap();
    exact(
        f.public(&bin)
            .args(["agent-role", "list"])
            .output()
            .unwrap(),
        75,
        b"",
        b"error: agent-role list sort failed (exit 75)\n",
    );
    exact(
        f.public(&bin)
            .args(["agent-role", "show"])
            .output()
            .unwrap(),
        2,
        b"",
        b"error: show requires --name\nhint: run `oms agent-role --help` for usage\n",
    );
    exact(f.public(&bin).args(["runtime", "failure", "classify"]).output().unwrap(), 2, b"", b"usage: oms runtime failure classify [-h] [--repo REPO] [--pretty]\n                                    [--exit EXIT] [--code CODE]\n                                    text\noms runtime failure classify: error: the following arguments are required: text\nhint: run `oms runtime --help` for usage\n");
    if std::path::Path::new("/dev/full").exists() {
        for (args, stderr) in [
            (&["agent-role", "--help"][..], false),
            (&["agent-role", "show"][..], true),
        ] {
            let mut command = f.public(&bin);
            command.args(args);
            let full = std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap();
            if stderr {
                command.stderr(full);
            } else {
                command.stdout(full);
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty() && output.stderr.is_empty());
        }
    }
    assert!(!f.0.join("preflight").exists());
}

// Leaf bytes come from the immutable reference checkout; root choices also
// include explicitly added native commands.
#[test]
fn public_experiment_modes_preserve_root_forwarding_and_precedence() {
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let data = f.0.join("data");
    std::fs::create_dir(data.join("chosen")).unwrap();
    const CONTRACT: &str = "{\"schema\":2,\"question\":\"q\",\"hypothesis\":\"h\",\"prediction\":\"p\",\"baseline\":{\"x\":1},\"treatment\":{\"independent_change\":\"x\"},\"controlled_variables\":{},\"metrics\":{\"primary\":{\"name\":\"score\",\"direction\":\"maximize\"}},\"seeds\":[0],\"success\":{\"min_improvement\":0}}";
    std::fs::write(data.join("contract.json"), CONTRACT).unwrap();
    let before = files(&data);
    let cases: &[(&[&str], i32, &str, &str)] = &[
        (
            &["runtime", "experiment", "template"],
            0,
            "{\"baseline\":{\"config_digest\":\"\",\"run_id\":\"\"},\"controlled_variables\":{\"code_base\":\"\",\"dataset_split\":\"\",\"parameter_count_tolerance\":0.01,\"training_steps\":null},\"hypothesis\":\"\",\"invariant_pack\":[],\"metrics\":{\"primary\":{\"direction\":\"minimize\",\"name\":\"\"},\"secondary\":[]},\"prediction\":\"\",\"question\":\"\",\"schema\":2,\"seeds\":[0,1,2],\"success\":{\"min_improvement\":0.0,\"no_regression\":{}},\"treatment\":{\"config_digest\":\"\",\"independent_change\":\"\"}}\n",
            "",
        ),
        (
            &["runtime", "experiment", "validate", "contract.json"],
            0,
            "{\"baseline\":{\"x\":1},\"contract_digest\":\"d171c9b0d2a6ef262a46c825361d9444f0e8bc8ea7a078ccaa64c6d49948b6fb\",\"controlled_variables\":{},\"hypothesis\":\"h\",\"invariant_pack\":[],\"metric_specs\":{\"score\":{\"direction\":\"maximize\",\"name\":\"score\"}},\"metrics\":{\"primary\":{\"direction\":\"maximize\",\"name\":\"score\"},\"secondary\":[]},\"prediction\":\"p\",\"primary_metric\":\"score\",\"question\":\"q\",\"schema\":2,\"seeds\":[0],\"success\":{\"min_improvement\":0.0,\"no_regression\":{}},\"treatment\":{\"independent_change\":\"x\"}}\n",
            "",
        ),
        (
            &[
                "runtime",
                "experiment",
                "compare",
                "contract.json",
                CONTRACT,
            ],
            0,
            "{\"comparable\":true,\"issues\":[],\"left_digest\":\"d171c9b0d2a6ef262a46c825361d9444f0e8bc8ea7a078ccaa64c6d49948b6fb\",\"right_digest\":\"d171c9b0d2a6ef262a46c825361d9444f0e8bc8ea7a078ccaa64c6d49948b6fb\",\"schema\":1}\n",
            "",
        ),
        (
            &[
                "runtime",
                "--repo",
                "chosen",
                "--pretty",
                "experiment",
                "template",
            ],
            0,
            "{\n  \"baseline\": {\n    \"config_digest\": \"\",\n    \"run_id\": \"\"\n  },\n  \"controlled_variables\": {\n    \"code_base\": \"\",\n    \"dataset_split\": \"\",\n    \"parameter_count_tolerance\": 0.01,\n    \"training_steps\": null\n  },\n  \"hypothesis\": \"\",\n  \"invariant_pack\": [],\n  \"metrics\": {\n    \"primary\": {\n      \"direction\": \"minimize\",\n      \"name\": \"\"\n    },\n    \"secondary\": []\n  },\n  \"prediction\": \"\",\n  \"question\": \"\",\n  \"schema\": 2,\n  \"seeds\": [\n    0,\n    1,\n    2\n  ],\n  \"success\": {\n    \"min_improvement\": 0.0,\n    \"no_regression\": {}\n  },\n  \"treatment\": {\n    \"config_digest\": \"\",\n    \"independent_change\": \"\"\n  }\n}\n",
            "",
        ),
        (
            &[
                "runtime",
                "--repo",
                "missing",
                "experiment",
                "template",
                "--repo",
                ".",
            ],
            0,
            "{\"baseline\":{\"config_digest\":\"\",\"run_id\":\"\"},\"controlled_variables\":{\"code_base\":\"\",\"dataset_split\":\"\",\"parameter_count_tolerance\":0.01,\"training_steps\":null},\"hypothesis\":\"\",\"invariant_pack\":[],\"metrics\":{\"primary\":{\"direction\":\"minimize\",\"name\":\"\"},\"secondary\":[]},\"prediction\":\"\",\"question\":\"\",\"schema\":2,\"seeds\":[0,1,2],\"success\":{\"min_improvement\":0.0,\"no_regression\":{}},\"treatment\":{\"config_digest\":\"\",\"independent_change\":\"\"}}\n",
            "",
        ),
        (
            &["runtime", "--first", "experiment", "template", "--last"],
            2,
            "",
            "usage: oms runtime [-h] [--repo REPO] [--pretty] [--version]\n                   {envelope,evidence,next,brief,failure,context,context-inspect,profile,release,capsule,backend,experiment,benchmark,resources,doctor}\n                   ...\noms runtime: error: unrecognized arguments: --first --last\nhint: run `oms runtime --help` for usage\n",
        ),
        (
            &["runtime", "--first", "experiment", "validate"],
            2,
            "",
            "usage: oms runtime experiment validate [-h] [--repo REPO] [--pretty] contract\noms runtime experiment validate: error: the following arguments are required: contract\nhint: run `oms runtime --help` for usage\n",
        ),
        (
            &[
                "runtime",
                "--first",
                "experiment",
                "validate",
                "{}",
                "--help",
            ],
            0,
            "usage: oms runtime experiment validate [-h] [--repo REPO] [--pretty] contract\n\npositional arguments:\n  contract\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO  project repository or directory\n  --pretty     pretty-print JSON\n",
            "",
        ),
        (
            &["runtime", "--repo", "--pretty", "experiment", "template"],
            2,
            "",
            "usage: oms runtime [-h] [--repo REPO] [--pretty] [--version]\n                   {envelope,evidence,next,brief,failure,context,context-inspect,profile,release,capsule,backend,experiment,benchmark,resources,doctor}\n                   ...\noms runtime: error: argument --repo: expected one argument\nhint: run `oms runtime --help` for usage\n",
        ),
        (
            &["runtime", "experiment", "compare", "{}"],
            2,
            "",
            "usage: oms runtime experiment compare [-h] [--repo REPO] [--pretty] left right\noms runtime experiment compare: error: the following arguments are required: right\nhint: run `oms runtime --help` for usage\n",
        ),
        (
            &["runtime", "experiment", "template", "--help"],
            0,
            "usage: oms runtime experiment template [-h] [--repo REPO] [--pretty]\n                                       [--output OUTPUT]\n\noptions:\n  -h, --help       show this help message and exit\n  --repo REPO      project repository or directory\n  --pretty         pretty-print JSON\n  --output OUTPUT\n",
            "",
        ),
    ];
    for (args, code, output, error) in cases {
        exact(
            f.public(&bin).args(*args).output().unwrap(),
            *code,
            output.as_bytes(),
            error.as_bytes(),
        );
    }
    assert_eq!(files(&data), before);
    exact(f.public(&bin).args(["runtime", "experiment", "template", "--output", "blocked/template.json"]).env("OMS_HARNESS_CHILD", "1").output().unwrap(), 2, b"", b"runtime-core: a harness child may only use read-only runtime actions\nhint: run `oms runtime --help` for usage\n");
    assert_eq!(files(&data), before);
    let output = f
        .public(&bin)
        .args([
            "runtime",
            "experiment",
            "template",
            "--output",
            "template.json",
        ])
        .output()
        .unwrap();
    exact(output, 0, cases[0].2.as_bytes(), b"");
    let bytes = std::fs::read(data.join("template.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        serde_json::from_str::<serde_json::Value>(cases[0].2).unwrap()
    );
    let mut expected = before;
    expected.insert(PathBuf::from("template.json"), bytes);
    assert_eq!(files(&data), expected);
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn public_job_digest_routes_all_leaf_arguments_without_python_or_writes() {
    use std::ffi::OsString;
    fn stable_output(bytes: &[u8]) -> Vec<u8> {
        let prefix = b"# Job digest\n\n- generated: ";
        let Some(rest) = bytes.strip_prefix(prefix) else {
            return bytes.to_vec();
        };
        let end = rest.iter().position(|b| *b == b'\n').unwrap();
        let stamp = &rest[..end];
        assert_eq!(stamp.len(), 20);
        for (i, byte) in stamp.iter().enumerate() {
            match i {
                4 | 7 => assert_eq!(*byte, b'-'),
                10 => assert_eq!(*byte, b'T'),
                13 | 16 => assert_eq!(*byte, b':'),
                19 => assert_eq!(*byte, b'Z'),
                _ => assert!(byte.is_ascii_digit()),
            }
        }
        let mut output = prefix.to_vec();
        output.extend_from_slice(b"<generated>");
        output.extend_from_slice(&rest[end..]);
        output
    }
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let data = f.0.join("data");
    std::fs::write(data.join("run.log"), b"ok\nERROR fixture\nlast\n").unwrap();
    std::fs::write(
        data.join("metrics.jsonl"),
        b"{\"step\":1,\"name\":\"loss\",\"value\":2}\n{\"step\":2,\"name\":\"loss\",\"value\":1}\n",
    )
    .unwrap();
    let before = files(&data);
    for args in [
        vec!["--help"],
        vec![],
        vec!["--tail"],
        vec!["--metrics"],
        vec!["--max-bytes", "511", "run.log"],
        vec!["--wait-timeout", "0", "123"],
        vec!["missing.log"],
        vec!["--tail", "1", "--patterns", "1", "run.log"],
        vec!["--metrics", "metrics.jsonl"],
        vec!["--metrics", "metrics.jsonl", "--tail", "1", "run.log"],
    ] {
        let argv: Vec<_> = args.iter().map(OsString::from).collect();
        let (code, expected, mut error) =
            oms_command_job_digest::run(&argv, &data, &std::sync::atomic::AtomicBool::new(false));
        if code == 2 {
            error.extend_from_slice(b"hint: run `oms job-digest --help` for usage\n");
        }
        for child in ["0", "1"] {
            let mut output = f
                .public(&bin)
                .arg("job-digest")
                .args(&args)
                .env("OMS_HARNESS_CHILD", child)
                .output()
                .unwrap();
            output.stdout = stable_output(&output.stdout);
            exact(output, code, &stable_output(&expected), &error);
        }
        assert_eq!(files(&data), before);
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(b"raw-\xff.log".to_vec());
        std::fs::write(data.join(&raw), b"ERROR \xfe\n").unwrap();
        let before_raw = files(&data);
        let (code, expected, error) = oms_command_job_digest::run(
            std::slice::from_ref(&raw),
            &data,
            &std::sync::atomic::AtomicBool::new(false),
        );
        let mut output = f
            .public(&bin)
            .args([OsString::from("job-digest"), raw])
            .output()
            .unwrap();
        assert!(
            output
                .stdout
                .windows(b"- log: raw-\xff.log".len())
                .any(|part| part == b"- log: raw-\xff.log")
        );
        output.stdout = stable_output(&output.stdout);
        exact(output, code, &stable_output(&expected), &error);
        assert_eq!(files(&data), before_raw);
    }
    let before = files(&data);
    exact(
        f.private(&bin, "--oms-select")
            .args(["job-digest", "123", "missing.log"])
            .output()
            .unwrap(),
        0,
        b"",
        b"",
    );
    assert_eq!(files(&data), before);
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn public_slurm_lint_preserves_static_modes_source_roots_and_raw_arguments() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::copy(
        source.join("scripts/slurm-lint.sh"),
        f.0.join("scripts/slurm-lint.sh"),
    )
    .unwrap();
    let data = f.0.join("data");
    for root in [&f.0, &data] {
        std::fs::create_dir(root.join("templates")).unwrap();
        std::fs::create_dir(root.join("local")).unwrap();
    }
    let template = b"#!/bin/bash\n# source template\n: > executed\n";
    std::fs::write(f.0.join("templates/slurm-job.sh"), template).unwrap();
    std::fs::write(
        data.join("templates/slurm-job.sh"),
        b"wrong caller template\n",
    )
    .unwrap();
    let snapshot = b"- QOS default: high\n## QOS\nName|PreemptMode|MaxTRESPJ|MaxWall\nhigh|CANCEL|gres/gpu=2|01:00:00\n";
    std::fs::write(f.0.join("local/slurm.md"), snapshot).unwrap();
    std::fs::write(data.join("local/slurm.md"), b"wrong caller snapshot\n").unwrap();
    std::fs::write(data.join("snapshot.md"), snapshot).unwrap();
    std::fs::write(data.join("empty.md"), b"").unwrap();
    std::fs::write(data.join("job.sh"), b"#!/bin/bash\n#SBATCH --time=01:00:00\n#SBATCH --gpus=2\n#SBATCH --output=run-%j.log\n: > executed\n").unwrap();
    std::fs::write(
        data.join("zero.sh"),
        b"#SBATCH --time=0\n#SBATCH --gpus=0\n#SBATCH --output=run-%j.log\n",
    )
    .unwrap();
    std::fs::write(data.join("empty.sh"), b"").unwrap();
    let raw = if cfg!(target_os = "macos") {
        OsString::from("job space.sh")
    } else {
        OsString::from_vec(b"job-\xff.sh".to_vec())
    };
    std::fs::copy(data.join("job.sh"), data.join(&raw)).unwrap();
    let before = files(&data);
    let source_before = [files(&f.0.join("templates")), files(&f.0.join("local"))];
    let reference = |args: &[OsString]| {
        let (code, output, mut error) = oms_command_slurm_lint::run_with_source(
            args,
            &data,
            &f.0,
            &std::sync::atomic::AtomicBool::new(false),
        );
        if code == 2 {
            error.extend_from_slice(b"hint: run `oms slurm-lint --help` for usage\n");
        }
        (code, output, error)
    };
    for (args, expected_code) in [
        (vec![], 2),
        (vec![""], 2),
        (vec!["--help"], 0),
        (vec!["-h", "--unknown"], 0),
        (vec!["--unknown", "--help"], 2),
        (vec!["--snapshot"], 2),
        (vec!["missing.sh"], 2),
        (vec!["job.sh", "other.sh"], 2),
        (vec!["job.sh", "--template"], 2),
        (vec!["--template", "extra"], 2),
        (vec!["--template"], 0),
        (vec!["--preview", "--template"], 0),
        (vec!["job.sh", "--snapshot", "snapshot.md"], 0),
        (vec!["empty.sh", "--snapshot", "snapshot.md"], 1),
        (vec!["zero.sh", "--snapshot", "snapshot.md"], 1),
        (vec!["--preview", "job.sh", "--snapshot", "snapshot.md"], 0),
        (vec!["--preview", "empty.sh", "--snapshot", "empty.md"], 0),
        (vec!["--preview", "zero.sh", "--snapshot", "absent.md"], 0),
    ] {
        let argv: Vec<_> = args.iter().map(OsString::from).collect();
        let (code, expected, error) = reference(&argv);
        assert_eq!(code, expected_code, "{args:?}");
        if args == ["--template"] {
            assert_eq!(expected, template);
        }
        exact(
            f.public(&bin)
                .arg("slurm-lint")
                .args(&argv)
                .env_remove("OH_MY_SETTING_SLURM_REF")
                .output()
                .unwrap(),
            code,
            &expected,
            &error,
        );
    }
    for (verb, args) in [
        ("slurm-lint.sh", vec!["--template"]),
        ("slurm-lint", vec!["--template"]),
        ("slurm-lint.sh", vec!["--unknown"]),
    ] {
        let argv: Vec<_> = args.iter().map(OsString::from).collect();
        let (code, expected, error) = reference(&argv);
        exact(
            f.public(&bin)
                .arg(verb)
                .args(argv)
                .env("OMS_HARNESS_CHILD", "1")
                .output()
                .unwrap(),
            code,
            &expected,
            &error,
        );
    }
    for preview in [false, true] {
        for (setting, snapshot) in [
            (None, f.0.join("local/slurm.md").into_os_string()),
            (Some(""), f.0.join("local/slurm.md").into_os_string()),
            (Some("snapshot.md"), OsString::from("snapshot.md")),
        ] {
            let mut args = vec![raw.clone()];
            if preview {
                args.push(OsString::from("--preview"));
            }
            let mut explicit = args.clone();
            explicit.extend([OsString::from("--snapshot"), snapshot]);
            let (code, expected, error) = reference(&explicit);
            assert_eq!(code, 0);
            assert!(
                expected
                    .windows(raw.as_encoded_bytes().len())
                    .any(|v| v == raw.as_encoded_bytes())
            );
            if preview {
                assert!(String::from_utf8_lossy(&expected).contains("per job asks 2    within"));
            }
            let mut command = f.public(&bin);
            command
                .arg("slurm-lint")
                .args(args)
                .env_remove("OH_MY_SETTING_SLURM_REF");
            if let Some(value) = setting {
                command.env("OH_MY_SETTING_SLURM_REF", value);
            }
            exact(command.output().unwrap(), code, &expected, &error);
        }
    }
    exact(
        f.command(&bin, &PathBuf::from(env!("CARGO_BIN_EXE_oms")))
            .args(["slurm-lint", "--template"]).env("OMS_SOURCE_ROOT", &f.0)
            .output().unwrap(),
        2, b"", b"error: slurm-lint requires validated native source metadata\nhint: run `oms slurm-lint --help` for usage\n",
    );
    std::fs::remove_file(f.0.join("templates/slurm-job.sh")).unwrap();
    exact(
        f.public(&bin)
            .args(["slurm-lint", "--template"])
            .output()
            .unwrap(),
        2,
        b"",
        format!(
            "error: template missing: {}\nhint: run `oms slurm-lint --help` for usage\n",
            f.0.join("templates/slurm-job.sh").display()
        )
        .as_bytes(),
    );
    std::fs::write(f.0.join("templates/slurm-job.sh"), template).unwrap();
    assert_eq!(files(&data), before);
    assert_eq!(
        [files(&f.0.join("templates")), files(&f.0.join("local"))],
        source_before
    );
    assert!(!f.0.join("preflight").exists());
    assert!(!f.0.join("executed").exists());
}

#[test]
fn private_dispatch_accepts_verified_companions() {
    let fixture = Fixture::new();
    add_companions(
        &fixture,
        &[
            "oms-supervisor",
            "oms-state-queue-worker",
            "oms-mcp-peer-owner",
            "oms-run-bounded-helper",
            "oms-graph-python-helper",
        ],
    );
    let dir = fixture.binary().parent().unwrap().to_owned();
    let run = || {
        Command::new(fixture.binary())
            .arg("--oms-select")
            .arg(&fixture.0)
            .args(["1111111111111111111111111111111111111111", "--", "list"])
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    std::fs::write(dir.join("oms-supervisor"), "changed").unwrap();
    assert_eq!(run().status.code(), Some(2));
}

#[test]
fn newly_wired_wave3_families_select_and_dispatch_without_python() {
    let f = Fixture::new();
    let bin = f.guarded_tools();
    for argv in [
        vec!["doctor", "--help"],
        vec!["intent", "--help"],
        vec!["agent-plan", "--help"],
        vec!["agent-task", "--help"],
        vec!["room", "--help"],
        vec!["journal", "--help"],
        vec!["graph", "--help"],
        vec!["land", "--help"],
        vec!["state", "--help"],
        vec!["run-ledger", "--help"],
        vec!["peer-ask", "--help"],
        vec!["models", "--help"],
        vec!["panel", "--help"],
        vec!["agent-supervisor", "--help"],
        vec!["slurm-submit", "--help"],
        vec!["runtime", "envelope", "--help"],
        vec!["runtime", "--help"],
        vec!["runtime", "--version"],
    ] {
        exact(
            f.private(&bin, "--oms-select")
                .args(&argv)
                .output()
                .unwrap(),
            0,
            b"",
            b"",
        );
        let out = f
            .private(&bin, "--oms-dispatch")
            .args(&argv)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{argv:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "{argv:?} stdout empty");
    }
    for hook in [
        "command-guard-hook",
        "command-guard-hook.sh",
        "harness-hook.sh",
    ] {
        exact(
            f.private(&bin, "--oms-select").arg(hook).output().unwrap(),
            0,
            b"",
            b"",
        );
    }
    exact(
        f.private(&bin, "--oms-select")
            .args(["mcp-server"])
            .output()
            .unwrap(),
        0,
        b"",
        b"",
    );
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn validate_payload_rejects_foreign_entry_in_staged_dir() {
    let f = Fixture::new();
    let dir = f.binary().parent().unwrap().to_path_buf();
    std::fs::write(dir.join("stray.txt"), b"unexpected").unwrap();
    for mode in ["--oms-select", "--oms-dispatch"] {
        let out = Command::new(f.binary())
            .arg(mode)
            .arg(&f.0)
            .args(["1111111111111111111111111111111111111111", "--", "list"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{mode}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("unexpected file in native payload directory"),
            "{mode}: {err}"
        );
    }
}

#[test]
fn frontdoor_routes_navigation_and_native_commands_without_python() {
    fn copy_regular_tree(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let from = entry.path();
            let to = dst.join(entry.file_name());
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                copy_regular_tree(&from, &to);
            } else if kind.is_file() {
                std::fs::copy(&from, &to).unwrap();
            }
        }
    }
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let source = std::env::var_os("OMS_CONTRACT_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    for rel in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        let src_file = source.join(rel);
        if src_file.is_file() {
            std::fs::copy(&src_file, f.0.join(rel)).unwrap();
        }
    }
    for entry_dir in std::fs::read_dir(source.join("crates")).unwrap() {
        let entry_dir = entry_dir.unwrap();
        if !entry_dir.file_type().unwrap().is_dir() {
            continue;
        }
        let crate_src = entry_dir.path();
        let crate_dst = f.0.join("crates").join(entry_dir.file_name());
        std::fs::create_dir_all(&crate_dst).unwrap();
        for file in ["Cargo.toml", "build.rs"] {
            let src_file = crate_src.join(file);
            if src_file.is_file() {
                std::fs::copy(&src_file, crate_dst.join(file)).unwrap();
            }
        }
        let src_tree = crate_src.join("src");
        if src_tree.is_dir() {
            copy_regular_tree(&src_tree, &crate_dst.join("src"));
        }
    }
    for data_root in ["config", "roles", "prompts"] {
        let dir = source.join(data_root);
        if dir.is_dir() {
            copy_regular_tree(&dir, &f.0.join(data_root));
        }
    }
    let entry = f.0.join("scripts/oms");

    let nav = f
        .command(&bin, &f.binary())
        .arg("--oms-frontdoor")
        .arg(&entry)
        .args(["--", "list", "--all"])
        .output()
        .unwrap();
    assert_eq!(
        nav.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&nav.stderr)
    );
    let nav_out = String::from_utf8_lossy(&nav.stdout);
    assert!(nav_out.contains("install-profile"));
    assert!(nav_out.contains("panel"));

    for argv in [
        vec!["room", "--help"],
        vec!["agent-plan", "--help"],
        vec!["runtime", "envelope", "--help"],
        vec!["runtime", "ssh-adapter", "--help"],
        vec!["runtime", "exec-snapshot", "--help"],
    ] {
        let out = f
            .command(&bin, &f.binary())
            .arg("--oms-frontdoor")
            .arg(&entry)
            .arg("--")
            .args(&argv)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{argv:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "{argv:?} stdout empty");
    }
    assert!(!f.0.join("preflight").exists());
}

#[test]
fn child_input_runs_natively_with_child_restrictions() {
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let data = f.0.join("data");
    std::fs::write(data.join("source.txt"), b"child view\n").unwrap();
    let parent = f
        .public(&bin)
        .args(["input", "attach", "file", "source.txt"])
        .output()
        .unwrap();
    assert_eq!(
        parent.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&parent.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(&parent.stdout).unwrap();
    let id = record["id"].as_str().unwrap();
    for (key, value) in [
        ("OMS_HARNESS_CHILD", "1"),
        ("OMS_HARNESS_DELEGATE_DEPTH", "1"),
    ] {
        exact(
            f.public(&bin)
                .args(["input", "attachments"])
                .env(key, value)
                .output()
                .unwrap(),
            2,
            b"",
            b"error: workers may retrieve only explicitly addressed attachment IDs; queue inspection is parent-only\nhint: run `oms input --help` for usage\n",
        );
        // Addressed retrieval reaches the native room-membership check.
        exact(
            f.public(&bin)
                .args(["input", "attachment", id, "--full"])
                .env(key, value)
                .output()
                .unwrap(),
            2,
            b"",
            b"error: worker attachment reads require an admitted room repository and participant\nhint: run `oms input --help` for usage\n",
        );
    }
    assert!(!f.0.join("preflight").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn sigterm_on_native_peer_command_reaps_setsid_descendants() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    let f = Fixture::new();
    let bin = f.guarded_tools();
    for tool in ["sh", "setsid", "sleep"] {
        let link = bin.join(tool);
        if !link.exists() {
            let found = std::env::split_paths(&std::env::var_os("PATH").unwrap())
                .map(|p| p.join(tool))
                .find(|p| p.is_file())
                .unwrap_or_else(|| panic!("{tool} on PATH"));
            symlink(std::fs::canonicalize(found).unwrap(), link).unwrap();
        }
    }
    let source = std::env::var_os("OMS_CONTRACT_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let ft = entry.file_type().unwrap();
            let to = dst.join(entry.file_name());
            if ft.is_dir() {
                copy_tree(&entry.path(), &to);
            } else if ft.is_file() {
                std::fs::copy(entry.path(), to).unwrap();
            }
        }
    }
    for data_root in ["config", "roles", "prompts"] {
        let dir = source.join(data_root);
        if dir.is_dir() {
            copy_tree(&dir, &f.0.join(data_root));
        }
    }
    let grand_pid_file = f.0.join("grand.pid");
    let grand_ready_file = f.0.join("grand.ready");
    std::fs::write(
        bin.join("codex"),
        "#!/bin/sh\nsetsid /bin/sh -c 'trap \"\" TERM; exec sleep 30' </dev/null >/dev/null 2>&1 & printf '%s\\n' \"$!\" > \"$OMS_GRAND_PID\"; : > \"$OMS_GRAND_READY\"; exec sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(bin.join("codex"), std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut cmd = f.private(&bin, "--oms-dispatch");
    cmd.args([
        "peer-ask",
        "--providers",
        "codex",
        "--model",
        "gpt-5.4",
        "--prompt",
        "ping",
    ])
    .env("OMS_GRAND_PID", &grand_pid_file)
    .env("OMS_GRAND_READY", &grand_ready_file)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .process_group(0);

    let mut child = cmd.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !grand_ready_file.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            let out = child.wait_with_output().unwrap();
            panic!(
                "oms peer-ask exited early ({status}): stdout={} stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
        }
        assert!(Instant::now() < deadline, "codex fixture did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let grand_pid: i32 = std::fs::read_to_string(&grand_pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();

    let reap_deadline = Instant::now() + Duration::from_secs(6);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= reap_deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native oms did not exit cleanly after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(status.code(), Some(130));
    std::thread::sleep(Duration::from_millis(50));
    let alive = Command::new("kill")
        .args(["-0", &grand_pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(
        !alive,
        "setsid TERM-ignoring grandchild {grand_pid} survived SIGTERM on native oms"
    );

    // Also verify nested `Scope::ProcessGroup` supervision (`5s` grace, matching
    // `ControlServices::invoke_with_scope`) cleanly cancels and reaps the child `oms`
    // and its `WholeTree` detached grandchild via `Reactor::new()`.
    let grand2_pid_file = f.0.join("grand2.pid");
    let grand2_ready_file = f.0.join("grand2.ready");
    let mut request = oms_supervisor::process::Request::new(vec![
        f.binary().into_os_string(),
        "--oms-dispatch".into(),
        f.0.clone().into_os_string(),
        "1111111111111111111111111111111111111111".into(),
        "--".into(),
        "peer-ask".into(),
        "--providers".into(),
        "codex".into(),
        "--model".into(),
        "gpt-5.4".into(),
        "--prompt".into(),
        "ping".into(),
    ]);
    request.cwd = Some(f.0.join("data"));
    request.stdin = Some(Vec::new());
    request.environment = vec![
        ("PATH".into(), bin.into_os_string()),
        ("HOME".into(), f.0.join("home").into_os_string()),
        (
            "OH_MY_SETTING_ROLES_DIR".into(),
            f.0.join("global").into_os_string(),
        ),
        (
            "OMS_PYTHON_RUNTIME_ROOT".into(),
            f.0.join("missing-python").into_os_string(),
        ),
        (
            "OMS_TEST_PREFLIGHT".into(),
            f.0.join("preflight").into_os_string(),
        ),
        ("OMS_HARNESS_CHILD".into(), "0".into()),
        ("OMS_HARNESS_DELEGATE_DEPTH".into(), "0".into()),
        (
            "OMS_GRAND_PID".into(),
            grand2_pid_file.clone().into_os_string(),
        ),
        (
            "OMS_GRAND_READY".into(),
            grand2_ready_file.clone().into_os_string(),
        ),
    ];
    request.remove_environment = [
        "OMS_ROOM_REPO",
        "OMS_ROOM_ID",
        "OMS_ROOM_PARTICIPANT",
        "OMS_FAIL_LEDGER",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    request.scope = oms_supervisor::process::Scope::ProcessGroup;
    request.terminate_grace = Duration::from_secs(5);
    request.deadline = Some(Instant::now() + Duration::from_secs(30));

    let mut reactor = oms_supervisor::process::Reactor::new();
    let job = reactor.start(request).unwrap();
    let ready2_deadline = Instant::now() + Duration::from_secs(10);
    while !grand2_ready_file.exists() {
        match reactor.poll(job, Duration::from_millis(20)).unwrap() {
            oms_supervisor::process::Progress::Pending(_) => {}
            oms_supervisor::process::Progress::Complete(done) => {
                panic!(
                    "nested oms exited early: exit={:?} stop={:?} stderr={}",
                    done.exit,
                    done.stop,
                    String::from_utf8_lossy(done.stderr.as_ref().map_or(&[][..], |s| &s.bytes))
                );
            }
        }
        assert!(
            Instant::now() < ready2_deadline,
            "nested codex fixture did not start"
        );
    }
    let grand2_pid: i32 = std::fs::read_to_string(&grand2_pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    reactor
        .stop(job, oms_supervisor::process::StopReason::Cancelled)
        .unwrap();
    let cancel2_deadline = Instant::now() + Duration::from_secs(7);
    let done = loop {
        match reactor.poll(job, Duration::from_millis(25)).unwrap() {
            oms_supervisor::process::Progress::Pending(_) => {
                assert!(
                    Instant::now() < cancel2_deadline,
                    "nested oms did not finish cancelling within 5s grace"
                );
            }
            oms_supervisor::process::Progress::Complete(done) => break done,
        }
    };
    assert_eq!(
        done.stop,
        Some(oms_supervisor::process::StopReason::Cancelled)
    );
    assert!(done.cleanup_complete());
    std::thread::sleep(Duration::from_millis(50));
    let alive2 = Command::new("kill")
        .args(["-0", &grand2_pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(
        !alive2,
        "setsid TERM-ignoring grandchild {grand2_pid} survived nested ProcessGroup cancellation"
    );
}

// ---- native landing services through the real binary -----------------------

const UNAVAILABLE: &str = "service unavailable; parent CLI integration is required";

#[test]
fn slurm_template_requires_native_checkpoint_provenance_before_training() {
    use serde_json::json;
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Landing::new();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let template = source.join("templates/slurm-job.sh");
    let mkdir = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("mkdir"))
        .find(|p| p.is_file())
        .unwrap();
    symlink(mkdir, f.tools.join("mkdir")).unwrap();
    std::fs::create_dir(f.repo.join("tests")).unwrap();
    std::fs::write(f.repo.join("tests/check.sh"), "true\n").unwrap();
    std::fs::write(f.repo.join("PROJECT.md"),"# Project\n## Status\n- State: confirmed\n## Project\n- Goal: verify checkpoint provenance\n## Verification\n- Required checks: `bash tests/check.sh`\n- Required check files: tests/check.sh\n## Scope\n- Scope defaults: a.txt\n## Acceptance\n- Native checks pass\n").unwrap();
    std::fs::write(
        f.repo.join(".git/info/exclude"),
        ".oms/\ncheckpoints/\nlogs/\n",
    )
    .unwrap();
    f.git(&["add", "PROJECT.md", "tests/check.sh"]);
    f.git(&["commit", "-qm", "checkpoint contract"]);
    let head = f.git(&["rev-parse", "HEAD"]);
    std::fs::create_dir(f.repo.join(".oms")).unwrap();
    let raw = json!({"schema":2,"question":"q","hypothesis":"h","prediction":"p","baseline":{"config_digest":"b".repeat(64)},"treatment":{"independent_change":"x","config_digest":"c".repeat(64)},"controlled_variables":{"code_base":head,"dataset_split":"frozen-data"},"metrics":{"primary":{"name":"score","direction":"maximize"}},"seeds":[0],"success":{"min_improvement":1},"budget":{"runs":2}});
    std::fs::write(f.repo.join(".oms/experiment-input.json"), raw.to_string()).unwrap();
    f.ok(&[
        "runtime",
        "experiment",
        "register",
        ".oms/experiment-input.json",
        "--id",
        "resume",
    ]);
    let (code, out, err) = f.oms(&[
        "runtime",
        "experiment",
        "run",
        "--id",
        "resume",
        "--arm",
        "baseline",
        "--seed",
        "0",
        "--metrics",
        ".oms/metrics.json",
        "--",
        "bash",
        "-c",
        "printf '{\"score\":10}\\n' > .oms/metrics.json",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    let row: serde_json::Value = serde_json::from_str(&out).unwrap();
    let c: serde_json::Value = serde_json::from_slice(
        &std::fs::read(f.repo.join(".oms/experiments/contracts/resume.json")).unwrap(),
    )
    .unwrap();
    std::fs::create_dir(f.repo.join("checkpoints")).unwrap();
    std::fs::write(f.repo.join("checkpoints/latest.pt"), b"model-state").unwrap();
    let eval =
        json!({"metrics":c["metrics"],"success":c["success"],"invariant_pack":c["invariant_pack"]});
    let manifest = json!({"schema":1,"experiment_id":"resume","run_id":row["run_id"],"contract_digest":c["contract_digest"],"arm":"baseline","seed":0,"snapshot":null,"code_base":head,"dataset_split":"frozen-data","config_digest":"b".repeat(64),"evaluation_sha256":oms_runtime::value::sha(oms_runtime::value::canonical(&oms_runtime::value::from_serde(&eval).unwrap()).unwrap()),"step":7,"rng_state_sha256":"e".repeat(64),"checkpoint":{"path":"checkpoints/latest.pt","size":11,"sha256":format!("{:x}",Sha256::digest(b"model-state"))}});
    std::fs::write(f.repo.join(".oms/checkpoint.json"), manifest.to_string()).unwrap();
    let oms = f.tools.join("oms");
    std::fs::write(&oms,format!("#!/bin/sh\nexec '{}' --oms-dispatch '{}' 1111111111111111111111111111111111111111 -- \"$@\"\n",f.f.binary().display(),f.f.0.display())).unwrap();
    std::fs::set_permissions(&oms, std::fs::Permissions::from_mode(0o755)).unwrap();
    let srun = f.tools.join("srun");
    std::fs::write(
        &srun,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$TRAIN_LOG\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&srun, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = f.f.0.join("training-called");
    let run = || {
        let mut command = Command::new(f.tools.join("bash"));
        f.env(&mut command);
        command
            .arg(&template)
            .env("SLURM_SUBMIT_DIR", &f.repo)
            .env("SLURM_RESTART_COUNT", "1")
            .env("CKPT_DIR", "checkpoints")
            .env("OMS_EXPERIMENT_ID", "resume")
            .env("OMS_EXPERIMENT_RUN_ID", row["run_id"].as_str().unwrap())
            .env("OMS_CHECKPOINT_MANIFEST", ".oms/checkpoint.json")
            .env("TRAIN_LOG", &log)
            .output()
            .unwrap()
    };
    let out = run();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("checkpoints/latest.pt")
    );
    std::fs::remove_file(&log).unwrap();
    let mut wrong = manifest.clone();
    wrong["seed"] = 1.into();
    std::fs::write(f.repo.join(".oms/checkpoint.json"), wrong.to_string()).unwrap();
    assert!(!run().status.success());
    assert!(!log.exists(), "mismatched provenance cannot reach training");
    std::fs::write(f.repo.join(".oms/checkpoint.json"), manifest.to_string()).unwrap();
    std::fs::rename(
        f.repo.join(".oms/experiments/runs.jsonl"),
        f.repo.join(".oms/experiments/runs.retained"),
    )
    .unwrap();
    assert!(!run().status.success());
    assert!(
        !log.exists(),
        "a Git-only remote checkout has no canonical run authority"
    );
}

#[test]
fn native_autopilot_review_repair_calls_the_worker_after_acceptance_passes() {
    use std::os::unix::fs::PermissionsExt;
    let f = Landing::new();
    f.configure_commit_identity();
    f.provider_config();
    std::fs::create_dir(f.repo.join("tests")).unwrap();
    std::fs::write(
        f.repo.join("tests/check.sh"),
        "set -eu\ntest \"$(cat a.txt)\" != a\n",
    )
    .unwrap();
    std::fs::write(f.repo.join("PROJECT.md"),"# Project\n## Status\n- State: confirmed\n## Project\n- Goal: repair the source-backed semantic defect\n## Verification\n- Required checks: `bash tests/check.sh`\n- Required check files: tests/check.sh\n## Scope\n- Scope defaults: a.txt\n## Acceptance\n- Native checks pass\n").unwrap();
    f.git(&["add", "PROJECT.md", "tests/check.sh"]);
    f.git(&["commit", "-qm", "contract"]);
    let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("python3"))
        .find(|p| p.is_file())
        .unwrap();
    let script = format!(
        "#!{}\n{}",
        python.display(),
        r#"import json, os, pathlib, re, sys
prompt=sys.stdin.read()
provider=pathlib.Path(sys.argv[0]).name
log=pathlib.Path(os.environ['ADAPTIVE_LOG'])
if provider == 'codex':
    role='review'
    bad=pathlib.Path('a.txt').read_text().strip()=='bad'
    finding={'file':'a.txt','line':1,'summary':'replace the demonstrated bad result','severity':'blocking','quote':'bad'}
    answer=('```findings\n'+json.dumps(finding)+'\n```\nGATE: fail') if bad else 'GATE: pass'
    print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':answer}}))
    print(json.dumps({'type':'turn.completed','usage':{'input_tokens':1,'output_tokens':1}}))
elif 'Decompose remaining work into at most' in prompt:
    role='planner'
    prefix=json.loads(re.search(r'Ids start with ("[^"]*")\.',prompt).group(1))
    answer=json.dumps({'tasks':[{'id':prefix+'t1','title':'repair the demonstrated source defect','allowed':['a.txt'],'verify':'bash tests/check.sh','depends':[]}]})
    print(json.dumps({'type':'result','subtype':'success','is_error':False,'result':answer}))
else:
    role='worker'
    p=pathlib.Path('a.txt');p.write_text('bad\n' if p.read_text().strip()=='a' else 'good\n')
    print(json.dumps({'type':'result','subtype':'success','is_error':False,'result':'updated the assigned file'}))
with log.open('a') as out: out.write(json.dumps({'role':role,'cwd':os.getcwd(),'prompt':prompt})+'\n')
"#
    );
    for name in ["claude", "codex"] {
        let p = f.tools.join(name);
        std::fs::write(&p, &script).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = f.f.0.join("adaptive-provider-calls.jsonl");
    let ci_run = f.f.0.join("ci-run.json");
    let ci_jobs = f.f.0.join("ci-jobs.json");
    let invoke = |args: &[&str]| {
        f.oms_env(
            args,
            &[
                ("ADAPTIVE_LOG", log.to_str().unwrap()),
                ("EXACT_CI_RUN", ci_run.to_str().unwrap()),
                ("EXACT_CI_JOBS", ci_jobs.to_str().unwrap()),
            ],
        )
    };
    let receipt = || {
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(f.repo.join(".oms/plan/autopilot-run.json")).unwrap(),
        )
        .unwrap()
    };
    let status = || {
        let (code, out, err) = invoke(&["autopilot", "status"]);
        assert_eq!(code, 0, "{out}{err}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()
    };
    let count = |role: &str| {
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["role"] == role)
            .count()
    };
    let (code, out, err) = invoke(&[
        "autopilot",
        "propose",
        "--planner",
        "claude",
        "--planner-model",
        "fixture",
        "--worker",
        "claude",
        "--worker-model",
        "fixture",
        "--reviewer",
        "codex",
        "--reviewer-model",
        "fixture",
        "--review-mode",
        "gate",
        "--allowed",
        "a.txt",
        "--base",
        "main",
        "--max-cycles",
        "4",
        "--initial-tasks",
        "1",
        "--replan-tasks",
        "1",
        "--provider-timeout",
        "30s",
    ]);
    assert_eq!(code, 4, "{out}{err}");
    let first = receipt();
    let run = |row: &serde_json::Value| {
        invoke(&[
            "autopilot",
            "run",
            "--proposal",
            row["proposal"]["path"].as_str().unwrap(),
            "--expected-proposal-sha256",
            row["proposal"]["sha256"].as_str().unwrap(),
        ])
    };
    let (code, out, err) = run(&first);
    assert_eq!(code, 3, "{out}{err}");
    assert_eq!(count("worker"), 1, "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(f.repo.join("a.txt")).unwrap(),
        "bad\n"
    );
    let stopped = receipt();
    assert_eq!(
        stopped["diagnostic"]["reason_code"], "semantic-review-failed",
        "{out}{err}"
    );
    let ready = status();
    assert_eq!(
        ready["next_action"]["action"], "propose-revision",
        "{ready}"
    );
    // Actual native host observation, pending refusal, stale decision CAS,
    // terminal consumption and duplicate rejection all run through the CLI.
    f.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/example/project.git",
    ]);
    let head = f.git(&["rev-parse", "HEAD"]);
    let binding = serde_json::json!({"schema":1,"source":"ci","identity":{"repository":"example/project","head_sha":head,"workflow_id":7,"run_id":9,"run_attempt":1}});
    let raw = binding.to_string();
    let binding_path = f.repo.join(".oms/wait.json");
    std::fs::write(&binding_path, &raw).unwrap();
    let wait_sha = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let mut ci = serde_json::json!({"id":9,"workflow_id":7,"run_attempt":1,"head_sha":head,"repository":{"full_name":"example/project"},"status":"in_progress","conclusion":null,"updated_at":"2026-11-23T10:00:00Z"});
    let mut jobs = serde_json::json!({"total_count":1,"jobs":[{"id":11,"run_id":9,"run_attempt":1,"head_sha":head,"run_url":"https://api.github.com/repos/example/project/actions/runs/9","name":"check","status":"in_progress","conclusion":null}]});
    std::fs::write(&ci_run, ci.to_string()).unwrap();
    std::fs::write(&ci_jobs, jobs.to_string()).unwrap();
    let gh = f.tools.join("gh");
    std::fs::write(
        &gh,
        r#"#!/bin/bash
[[ "$1 $2 $3" == 'api --method GET' ]] || exit 93
case "${@: -1}" in
  repos/example/project/actions/runs/9) cat "$EXACT_CI_RUN";;
  'repos/example/project/actions/runs/9/attempts/1/jobs?per_page=100') cat "$EXACT_CI_JOBS";;
  *) exit 94;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ready = status();
    let (code, out, err) = invoke(&[
        "autopilot",
        "decide",
        "--expected-decision-sha256",
        ready["next_action"]["decision_sha256"].as_str().unwrap(),
        "--action",
        "wait",
        "--wait-binding",
        binding_path.to_str().unwrap(),
        "--expected-wait-sha256",
        &wait_sha,
        "--reason",
        "await this exact CI run",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    let waiting = status();
    assert_eq!(waiting["next_action"]["wait"]["status"], "running");
    let (code, _, _) = invoke(&[
        "autopilot",
        "decide",
        "--expected-decision-sha256",
        waiting["next_action"]["decision_sha256"].as_str().unwrap(),
        "--action",
        "resume",
    ]);
    assert_eq!(code, 2, "pending external work cannot resume");
    assert_eq!(count("worker"), 1);
    assert_eq!(count("review"), 1);
    ci["status"] = "completed".into();
    ci["conclusion"] = "failure".into();
    jobs["jobs"][0]["status"] = "completed".into();
    jobs["jobs"][0]["conclusion"] = "failure".into();
    std::fs::write(&ci_run, ci.to_string()).unwrap();
    std::fs::write(&ci_jobs, jobs.to_string()).unwrap();
    let (code, _, _) = invoke(&[
        "autopilot",
        "decide",
        "--expected-decision-sha256",
        waiting["next_action"]["decision_sha256"].as_str().unwrap(),
        "--action",
        "resume",
    ]);
    assert_eq!(
        code, 2,
        "refresh exposes a new exact decision before it can be consumed"
    );
    let terminal = status();
    assert_eq!(terminal["next_action"]["action"], "resume");
    let (code, out, err) = invoke(&[
        "autopilot",
        "decide",
        "--expected-decision-sha256",
        terminal["next_action"]["decision_sha256"].as_str().unwrap(),
        "--action",
        "resume",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    let consumed = receipt();
    assert_eq!(
        consumed["adaptive"]["wait"]["consumed_result_sha256"],
        terminal["next_action"]["wait"]["result_sha256"]
    );
    let result_sha = terminal["next_action"]["wait"]["result_sha256"]
        .as_str()
        .unwrap();
    let (code, handoff, err) = invoke(&["session-handoff", "prompt", "--max-bytes", "8192"]);
    assert_eq!(code, 0, "{handoff}{err}");
    assert!(
        handoff.contains(result_sha) && handoff.contains("source-checked journal experience"),
        "{handoff}"
    );
    assert_eq!(count("worker"), 1, "handoff never dispatches work");
    assert_eq!(count("review"), 1, "handoff never calls a model");
    std::fs::write(f.repo.join("a.txt"), "dirty unrelated state\n").unwrap();
    let (code, stale, err) = invoke(&["session-handoff", "prompt", "--max-bytes", "8192"]);
    assert_eq!(code, 0, "{stale}{err}");
    assert!(
        !stale.contains("Advisory prior outcome:"),
        "experience with stale worktree applicability must be withheld: {stale}"
    );
    std::fs::write(f.repo.join("a.txt"), "bad\n").unwrap();
    let (code, _, _) = invoke(&[
        "autopilot",
        "decide",
        "--expected-decision-sha256",
        terminal["next_action"]["decision_sha256"].as_str().unwrap(),
        "--action",
        "resume",
    ]);
    assert_eq!(code, 2, "duplicate result cannot be consumed twice");
    let (code, out, err) = run(&consumed);
    assert_eq!(
        code, 3,
        "a failed CI result never grants acceptance: {out}{err}"
    );
    assert_eq!(
        count("worker"),
        1,
        "observation and fresh review do not resend completed tasks"
    );
    // Read the decision after this fresh acceptance receipt, preserving CAS
    // over all producer evidence rather than reusing the earlier projection.
    assert_eq!(f.oms(&["agent-plan", "accept"]).0, 0);
    let ready = status();
    assert_eq!(
        ready["next_action"]["action"], "propose-revision",
        "{ready}"
    );
    // The verifier already succeeds; the semantic finding, source bytes,
    // native scope/verdicts and exact review receipt authorize the correction.
    let (code, out, err) = invoke(&[
        "autopilot",
        "decide",
        "--expected-decision-sha256",
        ready["next_action"]["decision_sha256"].as_str().unwrap(),
        "--action",
        "propose-revision",
        "--reason",
        "repair the retained source-backed finding",
    ]);
    assert_eq!(code, 4, "{out}{err}");
    let correction = receipt();
    assert_eq!(correction["stage"], "proposal-review");
    let planner_calls = std::fs::read_to_string(&log).unwrap();
    let planner = planner_calls
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .rfind(|row| row["role"] == "planner")
        .unwrap();
    let prompt = planner["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("advisory experience") && prompt.contains(result_sha),
        "the next real planner must receive the exact source-checked prior result: {prompt}"
    );
    assert_eq!(
        count("worker"),
        1,
        "proposing a correction cannot execute it"
    );
    let proposal: serde_json::Value = serde_json::from_slice(
        &std::fs::read(correction["proposal"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert!(
        proposal["tasks"][0]["id"]
            .as_str()
            .unwrap()
            .starts_with("r1-")
    );
    let (code, out, err) = run(&correction);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        count("worker"),
        2,
        "reviewed corrective work must execute although the prior acceptance command already passed"
    );
    assert_eq!(
        count("review"),
        2,
        "each exact resulting snapshot gets its native review"
    );
    assert_eq!(receipt()["stage"], "done", "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(f.repo.join("a.txt")).unwrap(),
        "good\n"
    );
    let tasks: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.repo.join(".oms/plan/tasks.json")).unwrap())
            .unwrap();
    assert_eq!(tasks["tasks"]["r1-t1"]["state"], "done");
    let (code, _, _) = run(&correction);
    assert_eq!(code, 0);
    assert_eq!(
        count("worker"),
        2,
        "replay never dispatches an already completed correction"
    );
}

#[test]
fn native_autopilot_advances_a_verified_milestone_before_global_acceptance() {
    use std::os::unix::fs::PermissionsExt;
    let f = Landing::new();
    f.configure_commit_identity();
    f.provider_config();
    std::fs::create_dir(f.repo.join("tests")).unwrap();
    std::fs::write(f.repo.join("b.txt"), "b\n").unwrap();
    for (name, body) in [
        ("first.sh", "test \"$(cat a.txt)\" = good\n"),
        ("second.sh", "test \"$(cat b.txt)\" = good\n"),
        (
            "check.sh",
            "set -eu\nbash tests/first.sh\nbash tests/second.sh\n",
        ),
    ] {
        std::fs::write(f.repo.join("tests").join(name), body).unwrap();
    }
    std::fs::write(f.repo.join("PROJECT.md"),"# Project\n## Status\n- State: confirmed\n## Project\n- Goal: complete two independently verified milestones\n## Verification\n- Required checks: `bash tests/check.sh`\n- Required check files: tests/check.sh, tests/first.sh, tests/second.sh\n## Scope\n- Scope defaults: a.txt, b.txt\n## Acceptance\n- [id:C1] The first output is good\n- [id:C2] The second output is good\n## Roadmap\n- [ ] first: first output (C1)\n- [ ] second: second output (C2)\n").unwrap();
    f.git(&["add", "PROJECT.md", "tests", "b.txt"]);
    f.git(&["commit", "-qm", "two milestone contract"]);
    let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("python3"))
        .find(|p| p.is_file())
        .unwrap();
    let script = format!(
        "#!{}\n{}",
        python.display(),
        r#"import json, os, pathlib, re, sys
prompt=sys.stdin.read()
if pathlib.Path(sys.argv[0]).name=='codex':
    role='review'
    print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'GATE: pass'}}))
    print(json.dumps({'type':'turn.completed','usage':{'input_tokens':1,'output_tokens':1}}))
elif 'Decompose remaining work into at most' in prompt:
    role='planner'
    prefix=json.loads(re.search(r'Ids start with ("[^"]*")\.',prompt).group(1))
    first=prefix=='m1-'
    assert first or prefix=='m2-', prefix
    answer=json.dumps({'tasks':[{'id':prefix+'t1','title':'implement the frozen milestone','allowed':['a.txt' if first else 'b.txt'],'verify':'bash tests/'+('first.sh' if first else 'second.sh'),'depends':[],'covers':['C1' if first else 'C2']}]})
    print(json.dumps({'type':'result','subtype':'success','is_error':False,'result':answer}))
else:
    role='worker'
    path=pathlib.Path('a.txt' if pathlib.Path('a.txt').read_text().strip()!='good' else 'b.txt')
    path.write_text('good\n')
    print(json.dumps({'type':'result','subtype':'success','is_error':False,'result':'implemented the assigned milestone'}))
with pathlib.Path(os.environ['ADAPTIVE_LOG']).open('a') as out: out.write(json.dumps({'role':role,'prompt':prompt})+'\n')
"#
    );
    for name in ["claude", "codex"] {
        let path = f.tools.join(name);
        std::fs::write(&path, &script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = f.f.0.join("milestone-provider-calls.jsonl");
    let invoke = |args: &[&str]| f.oms_env(args, &[("ADAPTIVE_LOG", log.to_str().unwrap())]);
    let receipt = || {
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(f.repo.join(".oms/plan/autopilot-run.json")).unwrap(),
        )
        .unwrap()
    };
    let calls = |role: &str| {
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["role"] == role)
            .count()
    };
    let run = |row: &serde_json::Value| {
        invoke(&[
            "autopilot",
            "run",
            "--proposal",
            row["proposal"]["path"].as_str().unwrap(),
            "--expected-proposal-sha256",
            row["proposal"]["sha256"].as_str().unwrap(),
        ])
    };
    let (code, out, err) = invoke(&[
        "autopilot",
        "propose",
        "--planner",
        "claude",
        "--planner-model",
        "fixture",
        "--worker",
        "claude",
        "--worker-model",
        "fixture",
        "--reviewer",
        "codex",
        "--reviewer-model",
        "fixture",
        "--review-mode",
        "gate",
        "--allowed",
        "a.txt,b.txt",
        "--base",
        "main",
        "--max-cycles",
        "4",
        "--initial-tasks",
        "1",
        "--replan-tasks",
        "1",
        "--provider-timeout",
        "30s",
        "--milestones",
        "first,second",
    ]);
    assert_eq!(code, 4, "{out}{err}");
    let first = receipt();
    let (code, out, err) = run(&first);
    assert_eq!(
        code, 4,
        "the verified first milestone should produce the next reviewed proposal: {out}{err}"
    );
    assert_eq!(calls("worker"), 1);
    assert_eq!(
        calls("review"),
        1,
        "the first subset requires fresh native review evidence"
    );
    assert_eq!(
        f.oms(&["agent-plan", "accept"]).0,
        3,
        "global acceptance must still fail after only the first milestone"
    );
    let second = receipt();
    assert_eq!(second["owner_id"], first["owner_id"]);
    assert_eq!(
        second["contract"], first["contract"],
        "milestone continuation preserves the lifetime budget"
    );
    assert_eq!(second["stage"], "proposal-review");
    assert_eq!(second["adaptive"]["milestone_index"], 1);
    assert_eq!(second["adaptive"]["completed_milestones"][0]["id"], "first");
    let proposal: serde_json::Value = serde_json::from_slice(
        &std::fs::read(second["proposal"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(proposal["tasks"][0]["id"], "m2-t1");
    let (code, _, _) = run(&first);
    assert_eq!(
        code, 2,
        "the previous Parent approval cannot approve the next proposal"
    );
    assert_eq!(calls("worker"), 1);
    let (code, out, err) = run(&second);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(calls("worker"), 2);
    assert_eq!(
        calls("review"),
        2,
        "final whole-goal review is fresh after the second task"
    );
    assert_eq!(receipt()["stage"], "done");
    assert_eq!(
        receipt()["adaptive"]["completed_milestones"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(f.oms(&["agent-plan", "accept"]).0, 0);
    let (code, _, _) = run(&second);
    assert_eq!(code, 0);
    assert_eq!(
        calls("worker"),
        2,
        "replay cannot dispatch a completed milestone"
    );
}

#[test]
fn native_delegate_context_manifest_reaches_the_physical_provider_boundary() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Landing::new();
    fixture.provider_config();
    std::fs::write(
        fixture.repo.join("PROJECT.md"),
        "# Project\nnative snapshot contract\n",
    )
    .unwrap();
    std::fs::write(fixture.repo.join("target.rs"), "fn target() {}\n").unwrap();
    fixture.git(&["add", "PROJECT.md", "target.rs"]);
    fixture.git(&["commit", "-qm", "context snapshot"]);
    std::fs::write(fixture.repo.join("PROJECT.md"), "dirty parent contract\n").unwrap();
    std::fs::write(
        fixture.repo.join("pack.json"),
        format!(
            "{{\"pack_digest\":\"{}\",\"files\":[\"target.rs\"]}}",
            "a".repeat(64)
        ),
    )
    .unwrap();
    let provider = fixture.tools.join("claude");
    std::fs::write(&provider, "#!/bin/sh\ncat > \"$CONTEXT_CAPTURE\"\nprintf 'changed\\n' >> a.txt\nprintf '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}\\n'\n").unwrap();
    std::fs::set_permissions(provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    let capture = fixture.f.0.join("captured-prompt.txt");
    let (code, out, err) = fixture.oms_env(
        &[
            "peer-delegate",
            "--repo",
            fixture.repo.to_str().unwrap(),
            "--to",
            "claude",
            "--model",
            "fixture",
            "--no-task",
            "--no-memory",
            "--no-ml-context",
            "--context-manifest",
            "--context-pack",
            "pack.json",
            "--scope",
            "a.txt",
            "--verify",
            "true",
            "Change a.txt",
        ],
        &[("CONTEXT_CAPTURE", capture.to_str().unwrap())],
    );
    assert_eq!(code, 0, "{out}{err}");
    let prompt = std::fs::read(capture).unwrap();
    let directory = fixture.repo.join(".oms/runtime/context");
    let manifest_path = std::fs::read_dir(&directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|s| s == "json"))
        .unwrap();
    let raw = std::fs::read(&manifest_path).unwrap();
    let bundle = std::fs::read(manifest_path.with_extension("txt")).unwrap();
    assert!(prompt.windows(bundle.len()).any(|bytes| bytes == bundle));
    assert!(
        std::str::from_utf8(&bundle)
            .unwrap()
            .contains("native snapshot contract")
    );
    assert!(
        !std::str::from_utf8(&bundle)
            .unwrap()
            .contains("dirty parent contract")
    );
    let recorded = std::fs::read_dir(directory.join("calls"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let row: serde_json::Value = serde_json::from_slice(&std::fs::read(recorded).unwrap()).unwrap();
    assert_eq!(
        row["attribution"]["request_sha256"],
        format!("{:x}", Sha256::digest(&prompt))
    );
    assert_eq!(
        row["attribution"]["context_manifest_sha256"],
        format!("{:x}", Sha256::digest(&raw))
    );
    assert_eq!(row["dispatch"], "executor-returned");
    assert!(row["provider_delivery"].is_null());
}

// Observe the live process boundary in the synthetic provider. Production
// never uses this fixture's process observations as settlement authority.
#[cfg(target_os = "linux")]
#[test]
fn native_panel_dispatch_requires_exact_staged_provider_cleanup() {
    for case in ["native", "script", "replaced-reservation", "interrupted"] {
        staged_panel_dispatch(case);
    }
}

#[cfg(target_os = "linux")]
fn staged_panel_dispatch(case: &str) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Landing::new();
    fixture.provider_config();
    let roles = fixture.f.0.join("roles");
    std::fs::create_dir(&roles).unwrap();
    std::fs::write(
        roles.join("implementation-worker.md"),
        b"# Implementation worker\nFollow the declared scope.\n",
    )
    .unwrap();
    let provider = fixture.tools.join("claude");
    if case == "script" {
        std::fs::write(
            &provider,
            r#"#!/bin/sh
set -eCu
sample() {
        IFS= read -r stat < "/proc/$1/stat"
        set -- ${stat##*) }
        printf '%s %s' "$2" "${20}"
}
set -- $(sample "$$")
stage=$1
birth=$2
set -- $(sample "$stage")
controller=$1
test "$stage" != "$$" && test "$controller" != "$$" && test "$controller" != "$stage"
test "/proc/$stage/exe" -ef "$CUSTODY_STAGE"
test "/proc/$controller/exe" -ef "$CUSTODY_STAGE"
printf '%s %s %s %s %s\n' "$$" "$birth" "$stage" "$2" "$controller" > "$CUSTODY_PROBE"
cat >/dev/null
printf 'changed\n' >> a.txt
printf '{"type":"result","subtype":"success","is_error":false,"result":"done"}\n'
"#,
        )
        .unwrap();
        std::fs::set_permissions(provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    } else {
        let source = fixture.f.0.join("fixture-provider.rs");
        std::fs::write(&source, r##"
use std::{env, fs, io::{self, Read, Write}, os::unix::fs::OpenOptionsExt, path::PathBuf};
fn sample(pid: u32) -> (u32, String) {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<_> = stat.rsplit_once(") ").unwrap().1.split_whitespace().collect();
    (fields[1].parse().unwrap(), fields[19].into())
}
fn main() -> io::Result<()> {
    assert!(!env::args_os().any(|v| v == "--panel-custody"));
    assert!(!fs::read_dir("/proc/self/fd")?.flatten().any(|fd| {
        fs::read_link(fd.path()).ok().is_some_and(|path| path.to_string_lossy().starts_with("socket:"))
    }));
    let pid = std::process::id();
    let (stage, birth) = sample(pid);
    let (controller, stage_birth) = sample(stage);
    let mut marker = fs::OpenOptions::new().write(true).create_new(true).open(env::var_os("CUSTODY_PROBE").unwrap())?;
    writeln!(marker, "{pid} {birth} {stage} {stage_birth} {controller}")?;
    io::stdin().read_to_end(&mut Vec::new())?;
    let case = env::var("CUSTODY_CASE").unwrap();
    if case == "interrupted" {
        assert!(std::process::Command::new("/bin/sh")
            .args(["-c", r#"kill -INT "$1""#, "sh", &stage.to_string()])
            .status()?.success());
        loop { std::thread::park(); }
    }
    if case == "replaced-reservation" {
        let dir = PathBuf::from(env::var_os("OMS_PANEL_REPO").unwrap()).join(".oms/artifacts/panel-dispatch");
        let member = env::var("OMS_ROOM_PARTICIPANT").unwrap();
        let launch = dir.join(format!("{member}-launch.json"));
        let replacement = launch.with_extension("replacement");
        let mut new = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&replacement)?;
        new.write_all(&fs::read(&launch)?)?;
        fs::rename(replacement, launch)?;
    }
    writeln!(fs::OpenOptions::new().append(true).open("a.txt")?, "changed")?;
    println!(r#"{{"type":"result","subtype":"success","is_error":false,"result":"done"}}"#);
    Ok(())
}
"##).unwrap();
        let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let build = Command::new(compiler)
            .args(["--edition", "2024"])
            .arg(source)
            .arg("-o")
            .arg(&provider)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
    }
    let room_report: serde_json::Value =
        serde_json::from_str(&fixture.ok(&["room", "new", "--topic", "Custody fixture", "--json"]))
            .unwrap();
    let room = room_report["id"].as_str().unwrap();
    let main = fixture.ok(&[
        "agent-events",
        "start",
        "--provider",
        "codex",
        "--tool",
        "panel-main",
        "--ref",
        "panel_role=main",
        "--ref",
        &format!("panel_room_id={room}"),
        "--then",
        "starting",
        "--then",
        "working",
    ]);
    let main = main.trim();
    fixture.ok(&[
        "room",
        "join",
        "--id",
        room,
        "--participant",
        main,
        "--provider",
        "codex",
        "--role",
        "main",
        "--json",
    ]);
    std::fs::write(fixture.repo.join("brief.md"), b"Append a line to a.txt.\n").unwrap();
    let marker = fixture.f.0.join("provider-custody");
    let binary = fixture.f.binary();
    let (code, out, err) = fixture.oms_env(
        &[
            "panel",
            "--dispatch",
            "worker",
            "--access",
            "write",
            "--purpose",
            "implement",
            "--owner",
            "codex",
            "--room",
            room,
            "--main",
            main,
            "--to",
            "claude",
            "--task-id",
            "custody-fixture",
            "--brief-file",
            "brief.md",
            "--scope",
            "a.txt",
            "--verify",
            "true",
            "--json",
        ],
        &[
            ("CUSTODY_PROBE", marker.to_str().unwrap()),
            ("CUSTODY_STAGE", binary.to_str().unwrap()),
            ("CUSTODY_CASE", case),
        ],
    );
    assert_eq!(code, 0, "{out}{err}");
    let ack: serde_json::Value = serde_json::from_str(&out).unwrap();
    let member = ack["member"].as_str().unwrap();
    let public = fixture
        .repo
        .join(format!(".oms/artifacts/panel-{member}.json"));
    let private = fixture
        .f
        .0
        .join("home/.local/state/oh-my-setting/panel-control")
        .join(oms_panel_core::digest(
            fixture.repo.as_os_str().as_encoded_bytes(),
        ))
        .join("dispatch")
        .join(member);
    if case == "interrupted" {
        assert_ne!(ack["exit"], 0, "{ack}");
        assert!(marker.is_file());
        assert!(!public.exists());
        assert!(!private.join(format!("{member}-supervisor.json")).exists());
        return;
    }
    assert_eq!(ack["exit"], 0, "{ack}");
    assert_eq!(ack["cleanup_complete"], true);
    assert_eq!(ack["output_complete"], true);
    assert_eq!(ack["state"], "awaiting");
    let observed = std::fs::read_to_string(marker).unwrap();
    let [
        provider_pid,
        provider_birth,
        stage_pid,
        stage_birth,
        controller_pid,
    ]: [u64; 5] = observed
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    assert!(provider_birth > 0 && stage_birth > 0);
    assert_ne!(provider_pid, stage_pid);
    assert_ne!(provider_pid, controller_pid);
    assert_ne!(stage_pid, controller_pid);

    std::fs::write(
        fixture.repo.join("summary.md"),
        b"Completed the custody fixture.\n",
    )
    .unwrap();
    let event_path = oms_command_operations::event_path(&fixture.repo);
    let before = std::fs::read(&event_path).unwrap();
    let (code, out, err) = fixture.oms(&[
        "panel",
        "--finalize",
        "--owner",
        "codex",
        "--room",
        room,
        "--task-id",
        "custody-fixture",
        "--summary-file",
        "summary.md",
        "--outcome",
        "completed",
    ]);
    if case == "native" {
        assert_eq!(code, 0, "{out}{err}");
        let result: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(result["settlement"]["applied"].as_array().unwrap().len(), 1);
        assert_ne!(std::fs::read(event_path).unwrap(), before);
        let read = |suffix| -> serde_json::Value {
            serde_json::from_slice(
                &std::fs::read(private.join(format!("{member}-{suffix}.json"))).unwrap(),
            )
            .unwrap()
        };
        let process = read("process");
        let receipt = read("supervisor");
        assert_eq!(process["record"]["pid"], provider_pid);
        assert_eq!(process["record"]["group"], provider_pid);
        assert_eq!(process["supervisor"]["pid"], stage_pid);
        assert_eq!(process["writer"]["pid"], controller_pid);
        assert!(
            process["record"]["identity"]
                .as_str()
                .unwrap()
                .ends_with(&format!(":{provider_birth}"))
        );
        assert!(
            process["supervisor"]["identity"]
                .as_str()
                .unwrap()
                .ends_with(&format!(":{stage_birth}"))
        );
        assert_eq!(receipt["record"]["pid"], provider_pid);
        assert_eq!(receipt["record"]["all_reaped"], true);
        assert_eq!(receipt["record"]["exit"], 0);
        assert!(public.is_file());
    } else {
        assert_eq!(code, 2, "{out}{err}");
        assert!(out.is_empty());
        assert!(err.contains("missing verified whole-tree cleanup"), "{err}");
        assert_eq!(std::fs::read(event_path).unwrap(), before);
        assert!(!fixture.repo.join(".oms/artifacts/panel-results").exists());
        assert!(!public.exists());
        assert!(!private.join(format!("{member}-supervisor.json")).exists());
    }
}

/// A dispatch root plus an isolated host: private HOME, TMPDIR, delegate
/// worktree root and Git identity, a PATH without `gh` or Python, and a
/// committed work repository whose `.oms/` is ignored.
struct Landing {
    f: Fixture,
    tools: PathBuf,
    repo: PathBuf,
}
impl Landing {
    fn new() -> Self {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        for dir in ["home", "tmp", "wtroot", "tools", "repo"] {
            std::fs::create_dir(f.0.join(dir)).unwrap();
        }
        let tools = f.0.join("tools");
        let paths = std::env::var_os("PATH").unwrap();
        for name in [
            "bash", "sh", "git", "env", "cat", "true", "uname", "dirname", "readlink",
        ] {
            let found = std::env::split_paths(&paths)
                .map(|p| p.join(name))
                .find(|p| p.is_file())
                .unwrap();
            symlink(std::fs::canonicalize(found).unwrap(), tools.join(name)).unwrap();
        }
        std::fs::write(
            f.0.join("gitconfig"),
            "[user]\n\tname = t\n\temail = t@example.invalid\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        std::fs::write(f.0.join("VERSION"), "1.2.3\n").unwrap();
        let landing = Self {
            repo: f.0.join("repo"),
            tools,
            f,
        };
        landing.git(&["init", "-q"]);
        // Native Plan proofs deliberately ignore global Git configuration.
        landing.git(&["config", "--local", "user.name", "t"]);
        landing.git(&["config", "--local", "user.email", "t@example.invalid"]);
        std::fs::write(landing.repo.join("a.txt"), "a\n").unwrap();
        std::fs::write(landing.repo.join(".git/info/exclude"), ".oms/\n").unwrap();
        landing.git(&["add", "a.txt"]);
        landing.git(&["commit", "-qm", "init"]);
        landing
    }
    fn configure_commit_identity(&self) {
        // The native publication command deliberately clears global Git config.
        self.git(&["config", "--local", "user.name", "t"]);
        self.git(&["config", "--local", "user.email", "t@example.invalid"]);
    }
    fn provider_config(&self) {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::create_dir(self.f.0.join("config")).unwrap();
        for entry in std::fs::read_dir(source.join("config")).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                std::fs::copy(
                    &path,
                    self.f.0.join("config").join(path.file_name().unwrap()),
                )
                .unwrap();
            }
        }
    }
    fn env(&self, command: &mut Command) {
        command
            .env_clear()
            .env("PATH", &self.tools)
            .env("HOME", self.f.0.join("home"))
            .env("TMPDIR", self.f.0.join("tmp"))
            .env("OMS_DELEGATE_WORKTREE_ROOT", self.f.0.join("wtroot"))
            .env("GIT_CONFIG_GLOBAL", self.f.0.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("OMS_HARNESS_CHILD", "0")
            .env("OMS_HARNESS_DELEGATE_DEPTH", "0");
    }
    fn git_in(&self, dir: &std::path::Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        self.env(&mut command);
        command.env("PATH", std::env::var_os("PATH").unwrap());
        let out = command.arg("-C").arg(dir).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.repo, args)
    }
    /// One bounded `oms` call; never answers with an unavailable service.
    fn oms_env(&self, args: &[&str], extra: &[(&str, &str)]) -> (i32, String, String) {
        use std::io::Read;
        let mut command = Command::new(self.f.binary());
        self.env(&mut command);
        command
            .arg("--oms-dispatch")
            .arg(&self.f.0)
            .args(["1111111111111111111111111111111111111111", "--"])
            .args(args)
            .envs(extra.iter().copied())
            .current_dir(&self.repo)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().unwrap();
        let mut pipes = [
            child.stdout.take().map(|mut p| {
                std::thread::spawn(move || {
                    let mut raw = Vec::new();
                    p.read_to_end(&mut raw).map(|_| raw)
                })
            }),
            child.stderr.take().map(|mut p| {
                std::thread::spawn(move || {
                    let mut raw = Vec::new();
                    p.read_to_end(&mut raw).map(|_| raw)
                })
            }),
        ];
        let bound = std::env::var("OMS_TEST_CALL_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(120);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(bound);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("oms {args:?} exceeded OMS_TEST_CALL_TIMEOUT");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let mut text = pipes.iter_mut().map(|p| {
            String::from_utf8_lossy(&p.take().unwrap().join().unwrap().unwrap()).into_owned()
        });
        let (out, err) = (text.next().unwrap(), text.next().unwrap());
        assert!(
            !out.contains(UNAVAILABLE) && !err.contains(UNAVAILABLE),
            "oms {args:?}: {out}{err}"
        );
        (status.code().unwrap_or(-1), out, err)
    }
    fn oms(&self, args: &[&str]) -> (i32, String, String) {
        self.oms_env(args, &[])
    }
    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.oms(args);
        assert_eq!(code, 0, "oms {args:?}: {out}{err}");
        out
    }
    /// `.oms/p/p.patch` appends `line` to a.txt; the tree is left clean.
    fn patch(&self, line: &str) {
        std::fs::create_dir_all(self.repo.join(".oms/p")).unwrap();
        let old = std::fs::read_to_string(self.repo.join("a.txt")).unwrap();
        std::fs::write(self.repo.join("a.txt"), format!("{old}{line}\n")).unwrap();
        let diff = self.git(&["diff"]);
        std::fs::write(self.repo.join(".oms/p/p.patch"), diff + "\n").unwrap();
        self.git(&["checkout", "-q", "a.txt"]);
    }
    fn events(&self) -> Vec<String> {
        std::fs::read_to_string(self.repo.join(".oms/landings.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["event"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .filter(|e| ["intent", "complete", "abandoned"].contains(&e.as_str()))
            .collect()
    }
    fn approval(&self, id: &str) -> serde_json::Value {
        serde_json::from_str(&self.ok(&["approval-inbox", "show", "--approval", id, "--json"]))
            .unwrap()
    }
    /// A fresh approved patch-land grant: (id, grant).
    fn approved(&self) -> (String, String) {
        let requested: serde_json::Value = serde_json::from_str(&self.ok(&[
            "patch-land",
            "--patch",
            ".oms/p/p.patch",
            "--request-approval",
        ]))
        .unwrap();
        let id = requested["approval_id"].as_str().unwrap().to_owned();
        assert_eq!(requested["patch_sha"], self.approval(&id)["patch_sha"]);
        let grant = self.ok(&[
            "approval-inbox",
            "decide",
            "--approval",
            &id,
            "--decision",
            "approve",
            "--expected-version",
            "1",
            "--actor",
            "tester",
        ]);
        (id, grant.trim().to_owned())
    }
}

#[test]
fn public_inline_main_uses_native_resource_admission_and_keeps_unknown_custody() {
    use oms_command_telemetry::store::{dispatch, physical, resources, usage};
    use std::io::Read;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};
    const NAME: &str =
        "public_inline_main_uses_native_resource_admission_and_keeps_unknown_custody";
    if let Some(probe) = std::env::var_os("OMS_NATIVE_MAIN_PROBE") {
        let repo = PathBuf::from(std::env::var_os("OMS_STATE_REPO").unwrap());
        let call = std::env::var("OMS_PROVIDER_CALL_ID").unwrap();
        let owner = std::env::var("OMS_PANEL_MAIN_ATTEMPT").unwrap();
        let room = std::env::var("OMS_ROOM_ID").unwrap();
        let operation = std::env::var("OMS_OPERATION_ID").unwrap();
        assert_eq!(std::env::current_dir().unwrap(), repo);
        let stop = AtomicBool::new(false);
        let intents = physical::load(&repo, &stop).unwrap();
        assert_eq!(intents.len(), 1);
        let intent = &intents[0];
        assert_eq!(intent.phase, physical::Phase::Ready);
        assert_eq!(intent.identity.call_id, call);
        assert_eq!(intent.identity.operation_id, operation);
        assert_eq!(intent.identity.provider, "claude");
        assert_eq!(
            intent.identity.selected_model.as_deref(),
            Some("fixture-model")
        );
        let binding = intent.resource_binding.as_ref().unwrap();
        assert_eq!(binding.root_owner, owner);
        assert!(binding.team.is_none());
        let (events, _) = oms_command_operations::load_projection(&repo).unwrap();
        let created = events
            .iter()
            .find(|row| {
                row.get("attempt_id").text() == Some(owner.as_str())
                    && row.get("event_type").text() == Some("attempt.created")
            })
            .unwrap();
        assert_eq!(
            Some(binding.root_generation.as_str()),
            created.get("event_id").text()
        );
        assert_eq!(
            created.get("refs").get("panel_room_id").text().unwrap(),
            room
        );
        let receipts = usage::load(&repo).unwrap();
        assert_eq!(receipts.calls.len(), 1);
        assert_eq!(receipts.calls[0].receipt.phase, usage::Phase::Pending);
        assert!(!repo.join(".oms/plan/tasks.json").exists());
        let lock = oms_lock::Lock::for_file(
            &repo.join(dispatch::REGISTRY),
            oms_lock::Settings::from_env().unwrap(),
        )
        .unwrap();
        lock.try_acquire().unwrap().check().unwrap();
        oms_command_operations::lock::FileLock::acquire_before(
            &oms_command_operations::event_path(&repo),
            &stop,
            Instant::now() + Duration::from_millis(100),
        )
        .unwrap()
        .check()
        .unwrap();
        assert!(
            oms_panel_core::Store::open(&repo)
                .unwrap()
                .transaction("panel-main-session", Duration::from_millis(20))
                .is_err(),
            "Main session lock must span its native child"
        );
        let marker = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(probe)
            .unwrap();
        serde_json::to_writer(
            marker,
            &serde_json::json!({"call":call,"owner":owner,"room":room}),
        )
        .unwrap();
        println!("native Main fixture");
        println!("{{\"usage\":{{\"total_tokens\":0}},\"total_cost_usd\":0}}");
        return;
    }
    let l = Landing::new();
    let paths = std::env::var_os("PATH").unwrap();
    for name in ["awk", "wc", "sort"] {
        let found = std::env::split_paths(&paths)
            .map(|p| p.join(name))
            .find(|p| p.is_file())
            .unwrap();
        symlink(std::fs::canonicalize(found).unwrap(), l.tools.join(name)).unwrap();
    }
    let (name, found) = ["sha256sum", "shasum", "openssl"]
        .into_iter()
        .find_map(|name| {
            std::env::split_paths(&paths)
                .map(|p| p.join(name))
                .find(|p| p.is_file())
                .map(|p| (name, p))
        })
        .unwrap();
    symlink(std::fs::canonicalize(found).unwrap(), l.tools.join(name)).unwrap();
    let executable = std::env::current_exe().unwrap();
    let executable = executable.to_str().unwrap().replace('\'', "'\\''");
    let provider = l.tools.join("claude");
    std::fs::write(
        &provider,
        format!("#!/bin/sh\nexec '{executable}' --exact {NAME} --nocapture\n"),
    )
    .unwrap();
    std::fs::set_permissions(provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    let public = |words: &[&str]| {
        let mut command = Command::new(l.tools.join("bash"));
        l.env(&mut command);
        command
            .arg(l.f.0.join("scripts/oms"))
            .args(words)
            .current_dir(&l.repo);
        command
    };
    let host = public(&["runtime", "resources", "host"]).output().unwrap();
    assert!(
        host.status.success(),
        "{}",
        String::from_utf8_lossy(&host.stderr)
    );
    let host: serde_json::Value = serde_json::from_slice(&host.stdout).unwrap();
    let policy = resources::Policy {
        domain: "native-main-fixture".into(),
        epoch: "one".into(),
        host_generation_sha256: host["host_generation_sha256"].as_str().unwrap().into(),
        max_teams: 1,
        max_invocations: 1,
        max_open_processes: 1,
        max_write_wip: 1,
        reviewer_headroom: 0,
    };
    std::fs::write(l.repo.join("policy.json"), policy.to_json().unwrap()).unwrap();
    let activate = public(&[
        "runtime",
        "resources",
        "activate",
        "--policy",
        "policy.json",
    ])
    .output()
    .unwrap();
    assert!(
        activate.status.success(),
        "{}",
        String::from_utf8_lossy(&activate.stderr)
    );
    assert!(!l.repo.join(".oms/plan/tasks.json").exists());
    let probe = l.f.0.join("main-proof.json");
    let launch = || {
        let flags = rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY;
        let master = rustix::pty::openpt(flags).unwrap();
        rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC).unwrap();
        rustix::pty::grantpt(&master).unwrap();
        rustix::pty::unlockpt(&master).unwrap();
        let name = rustix::pty::ptsname(&master, Vec::new()).unwrap();
        let slave = rustix::fs::open(
            name.as_c_str(),
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let slave = std::fs::File::from(slave);
        rustix::fs::fcntl_setfl(
            &master,
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NONBLOCK,
        )
        .unwrap();
        let mut master = std::fs::File::from(master);
        let mut command = public(&[
            "panel",
            "--launch",
            "claude",
            "--host",
            "inline",
            "--layout",
            "inline",
            "--model",
            "fixture-model",
        ]);
        command
            .env("OMS_NATIVE_MAIN_PROBE", &probe)
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        let mut child = command.spawn().unwrap();
        drop(command);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut output = Vec::new();
        loop {
            let mut bytes = [0; 4096];
            match master.read(&mut bytes) {
                Ok(n) => output.extend_from_slice(&bytes[..n]),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("PTY read: {error}");
                }
            }
            if Instant::now() >= deadline || output.len() > 64 * 1024 {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "bounded Main fixture failed: {}",
                    String::from_utf8_lossy(&output)
                );
            }
            if let Some(status) = child.try_wait().unwrap() {
                // The child is reaped before any assertions can unwind.
                return (status, output);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let (status, output) = launch();
    assert!(status.success(), "{}", String::from_utf8_lossy(&output));
    let proof = std::fs::read(&probe).unwrap();
    let proof: serde_json::Value = serde_json::from_slice(&proof).unwrap();
    let stop = AtomicBool::new(false);
    let receipts = usage::load(&l.repo).unwrap();
    assert_eq!(receipts.calls.len(), 1);
    let receipt = &receipts.calls[0].receipt;
    assert_eq!(receipt.identity.call_id, proof["call"]);
    assert_eq!(receipt.phase, usage::Phase::Unknown);
    assert_eq!(receipt.cleanup_complete, None);
    assert_eq!(receipt.source_sha256, None);
    assert_eq!(receipt.cost, usage::Cost::Unknown);
    assert!(receipts.calls[0].tokens.is_none());
    let occupancy = resources::snapshot(&l.repo, &stop)
        .unwrap()
        .unwrap()
        .occupancy;
    assert_eq!(
        (
            occupancy.teams,
            occupancy.invocations,
            occupancy.open_processes,
            occupancy.write_wip
        ),
        (0, 1, 1, 1)
    );
    assert!(!l.repo.join(".oms/plan/tasks.json").exists());
    let report = public(&["runtime", "resources", "status"])
        .output()
        .unwrap();
    assert!(report.status.success());
    let report: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(report["active"], true);
    assert_eq!(report["host_matches_policy"], true);
    assert_eq!(
        report["occupancy"],
        serde_json::json!({"teams":0,"invocations":1,"open_processes":1,"write_wip":1})
    );
    let room = public(&[
        "room",
        "show",
        "--id",
        proof["room"].as_str().unwrap(),
        "--json",
    ])
    .output()
    .unwrap();
    assert!(
        room.status.success(),
        "{}",
        String::from_utf8_lossy(&room.stderr)
    );
    let room: serde_json::Value = serde_json::from_slice(&room.stdout).unwrap();
    assert!(
        room["participants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|member| member["participant"] == proof["owner"]
                && member["joined"] == true
                && member["role"] == "main")
    );
    let room = std::fs::read_to_string(l.repo.join(".oms/lifecycle/events.jsonl")).unwrap();
    assert!(room.contains(proof["owner"].as_str().unwrap()));
    let (status, output) = launch();
    assert!(!status.success(), "{}", String::from_utf8_lossy(&output));
    assert_eq!(usage::load(&l.repo).unwrap().calls.len(), 1);
    assert_eq!(physical::load(&l.repo, &stop).unwrap().len(), 1);
    assert_eq!(
        resources::snapshot(&l.repo, &stop)
            .unwrap()
            .unwrap()
            .occupancy,
        occupancy
    );
    assert_eq!(status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output).contains("ResourceCapacity(\"invocations\")"),
        "{}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn landing_plain_approval_and_recover_converge_through_native_services() {
    let l = Landing::new();
    l.patch("b");
    let (code, out, _) = l.oms(&["patch-admit", "--patch", ".oms/p/p.patch"]);
    assert_eq!((code, out.lines().next()), (0, Some("ADMIT")));
    let approval_required = "requires an approved exact landing request";
    for (name, value, message) in [
        ("OMS_EXECUTION_PROFILE", "remote", approval_required),
        ("OMS_EXECUTION_PROFILE", "isolated", approval_required),
        (
            "OMS_EXECUTION_PROFILE",
            "bogus",
            "invalid OMS_EXECUTION_PROFILE",
        ),
        ("OMS_REQUIRE_LANDING_APPROVAL", "1", approval_required),
    ] {
        let (code, _, err) = l.oms_env(
            &["patch-land", "--patch", ".oms/p/p.patch"],
            &[(name, value)],
        );
        assert!(code == 2 && err.contains(message), "{name}={value}: {err}");
    }
    assert!(l.events().is_empty());
    l.ok(&["patch-land", "--patch", ".oms/p/p.patch"]);
    assert_eq!(l.events(), ["intent", "complete"]);
    l.git(&["commit", "-qam", "b"]);

    // Approval-bound: approved v2 -> consuming v3 -> consumed v4.
    l.patch("c");
    let (id, grant) = l.approved();
    l.ok(&[
        "patch-land",
        "--patch",
        ".oms/p/p.patch",
        "--approval",
        &id,
        "--approval-token",
        &grant,
        "--approval-version",
        "2",
    ]);
    let row = l.approval(&id);
    assert_eq!(
        (row["state"].as_str(), row["version"].as_u64()),
        (Some("consumed"), Some(4))
    );
    // A consumed grant never lands again.
    l.git(&["commit", "-qam", "c"]);
    l.patch("d");
    let (code, _, err) = l.oms(&[
        "patch-land",
        "--patch",
        ".oms/p/p.patch",
        "--approval",
        &id,
        "--approval-token",
        &grant,
        "--approval-version",
        "2",
    ]);
    assert_ne!(code, 0, "{err}");

    // A rejected reservation leaves a durable unapplied intent; recovery
    // abandons it without touching the still-approved grant.
    let (id, grant) = l.approved();
    let forged = format!("{}.AAAA", grant.split_once('.').unwrap().0);
    let land = |grant: &str| {
        l.oms(&[
            "patch-land",
            "--patch",
            ".oms/p/p.patch",
            "--approval",
            &id,
            "--approval-token",
            grant,
            "--approval-version",
            "2",
        ])
    };
    assert_ne!(land(&forged).0, 0);
    assert_eq!(l.events()[4..], ["intent"]);
    let (code, out, _) = l.oms(&["patch-land", "--recover"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "patch-land: abandoned unapplied landing\n")
    );
    assert_eq!(l.events()[4..], ["intent", "abandoned"]);
    assert_eq!(l.approval(&id)["state"], "approved");
    assert_eq!(l.git(&["status", "--porcelain"]), "");

    // Applied but unreceipted (a kill after apply): recovery consumes the
    // reservation exactly once and a second recovery is a no-op.
    assert_ne!(land(&forged).0, 0);
    l.ok(&[
        "approval-inbox",
        "begin-consume",
        "--approval",
        &id,
        "--token",
        &grant,
        "--expected-version",
        "2",
        "--consumer",
        "patch-land",
    ]);
    l.git(&["apply", ".oms/p/p.patch"]);
    let (code, out, _) = l.oms(&["patch-land", "--recover"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "patch-land: recovered applied landing\n")
    );
    let row = l.approval(&id);
    assert_eq!(
        (row["state"].as_str(), row["version"].as_u64()),
        (Some("consumed"), Some(4))
    );
    assert_eq!(l.events()[6..], ["intent", "complete"]);
    l.ok(&["patch-land", "--recover"]);
    assert_eq!(l.events().len(), 8);

    // Reserved but never applied: recovery fails the reservation closed.
    l.git(&["commit", "-qam", "d"]);
    l.patch("e");
    let (id, grant) = l.approved();
    let forged = format!("{}.AAAA", grant.split_once('.').unwrap().0);
    assert_ne!(
        l.oms(&[
            "patch-land",
            "--patch",
            ".oms/p/p.patch",
            "--approval",
            &id,
            "--approval-token",
            &forged,
            "--approval-version",
            "2",
        ])
        .0,
        0
    );
    l.ok(&[
        "approval-inbox",
        "begin-consume",
        "--approval",
        &id,
        "--token",
        &grant,
        "--expected-version",
        "2",
        "--consumer",
        "patch-land",
    ]);
    l.ok(&["patch-land", "--recover"]);
    assert_eq!(l.approval(&id)["state"], "failed");
    assert_eq!(l.events().last().map(String::as_str), Some("abandoned"));
}

#[test]
fn canonical_views_show_only_proven_scratch_records_outside_canonical_attempts() {
    use oms_panel_core::native::operations::{append_lifecycle, new_event, value::obj};
    use std::{fs, path::Path, sync::atomic::AtomicBool};

    let host = Landing::new();
    let add = || {
        host.ok(&[
            "scratch-worktree",
            "add",
            "--owner-pid",
            &std::process::id().to_string(),
        ])
    };
    let scratch = PathBuf::from(add().trim());
    let other = PathBuf::from(add().trim());
    let record = |repo: &Path, tool: &str| {
        let event = new_event(
            "same-local-id",
            1usize.into(),
            "attempt.created",
            obj([
                ("to_state", "queued".into()),
                ("provider", "codex".into()),
                ("tool", tool.into()),
            ]),
        )
        .unwrap();
        append_lifecycle(repo, event, &AtomicBool::new(false)).unwrap();
    };
    record(&host.repo, "peer-ask");
    record(&scratch, "peer-review");
    record(&other, "peer-delegate");
    let read = |args: &[&str]| {
        let (code, out, err) = host.oms(args);
        assert!(matches!(code, 0 | 1), "{err}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap_or_else(|_| panic!("{err}: {out}"))
    };
    let dashboard = || read(&["dashboard", "--json", "--panel-snapshot"])["dashboard"].clone();
    let current = dashboard();
    let scratch_only = &current["managedScratch"];
    let attempts = scratch_only["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "{scratch_only}");
    assert_ne!(attempts[0]["id"], attempts[1]["id"]);
    assert_ne!(attempts[0]["sourceId"], attempts[1]["sourceId"]);
    assert_eq!(current["commandStatus"]["total"], 1);
    assert_eq!(current["active"], 1);
    assert!(current["conversationMain"].is_null());
    assert!(!scratch_only.to_string().contains("same-local-id"));
    assert!(
        !scratch_only
            .to_string()
            .contains(&*host.f.0.to_string_lossy())
    );
    assert_eq!(&read(&["state", "--json"])["managedScratch"], scratch_only);
    assert_eq!(
        &read(&["dashboard", "--json"])["managedScratch"],
        scratch_only
    );
    let text = host.oms(&["state"]).1;
    assert!(text.contains("recorded queued at") && text.contains("peer-review"));
    let board = host.oms(&["panel", "--watch", "--count", "1"]);
    assert!(
        board.1.contains("Managed scratch records") && board.1.contains("peer-review"),
        "board: {board:?}"
    );
    add_companions(&host.f, &["oms-mcp-peer-owner"]);
    for profile in ["mcp-app", "chatgpt-panel"] {
        let bin = host.f.binary();
        let mut command = Command::new(&bin);
        host.env(&mut command);
        command
            .arg("--oms-dispatch")
            .arg(&host.f.0)
            .arg(bin.parent().unwrap().file_name().unwrap())
            .arg("--")
            .current_dir(&host.repo);
        let (data, bytes) = fixture_panel_tool(command, profile, &host.repo, "oms_panel_snapshot");
        assert_eq!(
            &data["dashboard"]["managedScratch"], scratch_only,
            "{profile}"
        );
        assert_eq!(data["dashboard"]["active"], 1);
        assert!(bytes < 60_000, "{profile} {bytes}");
        println!("{profile} scratch snapshot: {bytes} wire bytes");
    }
    let marker = scratch.parent().unwrap().join(".oh-my-setting-tmp");
    fs::write(marker, "forged\n").unwrap();
    let changed = dashboard();
    let after = &changed["managedScratch"];
    assert_eq!(after["status"], "partial", "{after}");
    assert_eq!(after["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(after["attempts"][0]["tool"], "peer-delegate");
    assert_eq!(changed["commandStatus"]["total"], 1);
    host.git(&["worktree", "remove", "--force", other.to_str().unwrap()]);
    assert!(
        dashboard()["managedScratch"]["attempts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn plan_queries_from_registered_scratch_use_only_proven_shared_state() {
    use std::{fs, os::unix::fs::symlink, path::Path};
    let host = Landing::new();
    host.ok(&["agent-plan", "init", "--goal", "shared goal"]);
    host.ok(&[
        "agent-plan",
        "add",
        "--id",
        "task-a",
        "--title",
        "shared task",
        "--verify",
        "true",
        "--allowed",
        "a.txt",
    ]);
    let canonical = host.ok(&["agent-plan", "status", "--json"]);
    let stored = host.repo.join(".oms/plan/tasks.json");
    let unchanged = fs::read(&stored).unwrap();
    let scratch = host.ok(&[
        "scratch-worktree",
        "add",
        "--owner-pid",
        &std::process::id().to_string(),
    ]);
    let scratch = Path::new(scratch.trim());
    let read = |path: &Path| {
        host.oms(&[
            "agent-plan",
            "--repo",
            path.to_str().unwrap(),
            "status",
            "--json",
        ])
    };
    let nested = scratch.join("nested");
    fs::create_dir(&nested).unwrap();
    // Every path, kind and content, so any state written by a query shows.
    let tree = |root: &Path| {
        let mut pending = vec![root.to_path_buf()];
        let mut entries = std::collections::BTreeMap::new();
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let kind = fs::symlink_metadata(&path).unwrap().file_type();
                let content = if kind.is_dir() {
                    pending.push(path.clone());
                    None
                } else if kind.is_symlink() {
                    Some(
                        fs::read_link(&path)
                            .unwrap()
                            .into_os_string()
                            .into_encoded_bytes(),
                    )
                } else {
                    Some(fs::read(&path).unwrap())
                };
                entries.insert(path, content);
            }
        }
        entries
    };
    let untouched = tree(scratch);
    assert_eq!(read(scratch), (0, canonical.clone(), String::new()));
    assert_eq!(read(&nested), (0, canonical.clone(), String::new()));
    let name = scratch.to_str().unwrap();
    assert_eq!(
        host.ok(&["agent-plan", "--repo", name, "show", "--id", "task-a"]),
        host.ok(&["agent-plan", "show", "--id", "task-a"])
    );
    for query in ["list", "ready"] {
        let shared = host.oms(&["agent-plan", query]);
        assert!(shared.1.contains("task-a"), "{query}: {shared:?}");
        assert_eq!(host.oms(&["agent-plan", "--repo", name, query]), shared);
    }
    assert_eq!(tree(scratch), untouched);
    assert_ne!(
        host.oms(&[
            "agent-plan",
            "--repo",
            name,
            "next",
            "--claim",
            "--provider",
            "codex"
        ])
        .0,
        0
    );
    assert_ne!(
        host.oms(&["agent-plan", "--repo", name, "init", "--goal", "shadow"])
            .0,
        0
    );
    let child = host.oms_env(
        &["agent-plan", "--repo", name, "status", "--json"],
        &[("OMS_HARNESS_CHILD", "1")],
    );
    assert_eq!(child.0, 0, "{}", child.2);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&child.1).unwrap()["present"],
        false
    );
    let scoped = host.oms_env(
        &["agent-plan", "--repo", name, "status", "--json"],
        &[
            ("OMS_HARNESS_CHILD", "1"),
            ("OMS_STATE_REPO", host.repo.to_str().unwrap()),
        ],
    );
    assert_eq!(scoped, (0, canonical.clone(), String::new()));
    let marker = scratch.parent().unwrap().join(".oh-my-setting-tmp");
    let original = fs::read(&marker).unwrap();
    let saved = marker.with_extension("original");
    fs::rename(&marker, &saved).unwrap();
    assert_ne!(read(scratch).0, 0);
    symlink(&saved, &marker).unwrap();
    assert_ne!(read(scratch).0, 0);
    fs::remove_file(&marker).unwrap();
    fs::rename(&saved, &marker).unwrap();

    let unrelated = host.f.0.join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    host.git_in(&unrelated, &["init", "-q"]);
    let name_unrelated = unrelated.to_str().unwrap();
    host.ok(&[
        "agent-plan",
        "--repo",
        name_unrelated,
        "init",
        "--goal",
        "foreign goal",
    ]);
    let foreign = tree(&unrelated);
    // A genuine scratch whose marker names another repository owns neither Plan.
    let owner = format!("repo={}\n", host.repo.display());
    let rebound = String::from_utf8(original.clone()).unwrap();
    assert!(rebound.contains(&owner), "{rebound}");
    fs::write(
        &marker,
        rebound.replace(&owner, &format!("repo={name_unrelated}\n")),
    )
    .unwrap();
    let refused = read(scratch);
    assert_eq!((refused.0, refused.1.as_str()), (2, ""), "{}", refused.2);
    fs::write(&marker, &original).unwrap();
    let redirect = host.oms_env(
        &["agent-plan", "--repo", name, "status", "--json"],
        &[
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "core.worktree"),
            ("GIT_CONFIG_VALUE_0", unrelated.to_str().unwrap()),
        ],
    );
    // Git versions can ignore core.worktree on registered linked checkouts.
    // A supported query can only return the verified original, never the other Plan.
    if redirect.0 == 0 {
        assert_eq!(redirect.1, canonical);
    } else {
        assert_eq!(redirect.0, 2);
    }
    let forged = host.f.0.join("wtroot/oh-my-setting-scratch.copy");
    let copied = forged.join("wt-copy");
    fs::create_dir(&forged).unwrap();
    fs::set_permissions(&forged, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    fs::write(forged.join(".oh-my-setting-tmp"), &original).unwrap();
    fs::create_dir(&copied).unwrap();
    host.git_in(&copied, &["init", "-q"]);
    assert_ne!(read(&copied).0, 0);
    fs::write(
        forged.join(".oh-my-setting-tmp"),
        format!(
            "kind=oh-my-setting-temp\ntemporary=1\nrepo={}\nworktree={}\n",
            host.repo.display(),
            copied.display()
        ),
    )
    .unwrap();
    assert_ne!(read(&copied).0, 0);
    fs::rename(copied.join(".git"), copied.join("old.git")).unwrap();
    fs::copy(scratch.join(".git"), copied.join(".git")).unwrap();
    assert_ne!(read(&copied).0, 0);
    let swapped = forged.join("old");
    fs::rename(&copied, &swapped).unwrap();
    symlink(&unrelated, &copied).unwrap();
    assert_ne!(read(&copied).0, 0);
    assert_eq!(fs::read(&stored).unwrap(), unchanged);
    assert_eq!(tree(scratch), untouched);
    assert_eq!(tree(&unrelated), foreign);
    for repo in [scratch, &swapped] {
        assert!(!repo.join(".oms/plan").exists());
    }
}

#[test]
fn landing_admission_covers_and_plan_scope_use_native_context() {
    let l = Landing::new();
    std::fs::write(
        l.repo.join("PROJECT.md"),
        "# P\n\n## Goal\n\nShip it.\n\n## Done when\n\n- [id:crit-a] the file says b\n",
    )
    .unwrap();
    l.git(&["add", "PROJECT.md"]);
    l.git(&["commit", "-qm", "project"]);
    l.patch("b");
    let admit = |covers: &[&str]| {
        let mut args = vec!["patch-admit", "--patch", ".oms/p/p.patch"];
        for id in covers {
            args.extend(["--covers", id]);
        }
        l.oms(&args)
    };
    let (code, out, err) = admit(&["crit-a"]);
    assert_eq!(code, 0, "{out}{err}");
    let index = std::fs::read_to_string(l.repo.join(".oms/artifacts/index.jsonl")).unwrap();
    let row: serde_json::Value = serde_json::from_str(index.lines().last().unwrap()).unwrap();
    assert_eq!(row["covers"], serde_json::json!(["crit-a"]));
    assert_eq!(row["coverage"]["covers"], row["covers"]);
    assert_eq!(row["status"], row["coverage"]["status"]);
    let projected: serde_json::Value =
        serde_json::from_str(&l.ok(&["runtime", "evidence", "show", "--repo", "."])).unwrap();
    assert_eq!(
        projected["criteria"][0]["evidence"][0]["evidence_ref"],
        row["event_id"]
    );

    let at_limit: Vec<String> = (0..15).map(|i| format!("c{i}")).collect();
    let at_limit: Vec<&str> = at_limit.iter().map(String::as_str).collect();
    let longest = "x".repeat(160);
    let too_long = "x".repeat(161);
    for (covers, message) in [
        (vec!["nosuch"], "unknown criterion id(s): nosuch"),
        (at_limit.clone(), "unknown criterion id(s): c0"),
        ([at_limit.as_slice(), &["c15"]].concat(), "accepts 1 to 15"),
        (vec![""], "--covers requires a nonempty value"),
        (vec!["bad id!"], "invalid shape"),
        (vec![longest.as_str()], "unknown criterion id(s)"),
        (vec![too_long.as_str()], "invalid shape"),
    ] {
        let (code, out, err) = admit(&covers);
        assert_eq!(code, 2, "{covers:?}: {out}{err}");
        assert!(err.contains(message), "{covers:?}: {err}");
    }

    // --plan-task resolves the task's allowed paths natively: a patch
    // outside them fails the scope gate before any landing authority.
    l.ok(&["agent-plan", "init"]);
    l.ok(&[
        "agent-plan",
        "add",
        "--id",
        "t1",
        "--title",
        "T",
        "--allowed",
        "b.txt",
        "--verify",
        "true",
    ]);
    l.ok(&["agent-plan", "claim", "--id", "t1", "--provider", "codex"]);
    let task: serde_json::Value =
        serde_json::from_str(&l.ok(&["agent-plan", "show", "--id", "t1"])).unwrap();
    std::fs::write(l.repo.join(".oms/p/r.md"), "report\n").unwrap();
    l.ok(&[
        "agent-plan",
        "review",
        "--id",
        "t1",
        "--lease-id",
        task["lease_id"].as_str().unwrap(),
        "--artifact",
        ".oms/p/r.md",
        "--patch",
        ".oms/p/p.patch",
    ]);
    let (code, out, err) = l.oms(&[
        "patch-admit",
        "--patch",
        ".oms/p/p.patch",
        "--plan-task",
        "t1",
        "--covers",
        "crit-a",
    ]);
    assert_eq!(code, 1, "{out}{err}");
    let report =
        std::fs::read_to_string(err.trim().rsplit_once('(').unwrap().1.trim_end_matches(')'))
            .unwrap();
    assert!(report.contains("- scope: FAIL"), "{report}");
    // In scope, native patch-admit and patch-land --plan-task complete end to end
    // (including Linux WholeTree verify via the single-threaded trampoline).
    l.ok(&[
        "agent-plan",
        "add",
        "--id",
        "t2",
        "--title",
        "T",
        "--allowed",
        "a.txt,tests/review-binding.txt",
        "--verify",
        "true",
    ]);
    l.ok(&["agent-plan", "claim", "--id", "t2", "--provider", "codex"]);
    let task: serde_json::Value =
        serde_json::from_str(&l.ok(&["agent-plan", "show", "--id", "t2"])).unwrap();
    let reviewed_patch = ".oms/p/exact.patch";
    let mut reviewed = std::fs::read(l.repo.join(".oms/p/p.patch")).unwrap();
    // Red checks execute only for test-file hunks. Keep one in the exact
    // candidate so the later advisory command really mutates the report.
    reviewed.extend_from_slice(b"diff --git a/tests/review-binding.txt b/tests/review-binding.txt\nnew file mode 100644\n--- /dev/null\n+++ b/tests/review-binding.txt\n@@ -0,0 +1 @@\n+bound\n");
    let report = std::fs::read(l.repo.join(".oms/p/r.md")).unwrap();
    std::fs::write(l.repo.join(reviewed_patch), &reviewed).unwrap();
    l.ok(&[
        "agent-plan",
        "review",
        "--id",
        "t2",
        "--lease-id",
        task["lease_id"].as_str().unwrap(),
        "--artifact",
        ".oms/p/r.md",
        "--patch",
        reviewed_patch,
        "--expected-review-artifact-sha256",
        &oms_command_plan::json::sha(&report),
        "--expected-review-patch-sha256",
        &oms_command_plan::json::sha(&reviewed),
    ]);
    let before = std::fs::read(l.repo.join(".oms/plan/tasks.json")).unwrap();
    let alternative = ".oms/p/alternative.patch";
    std::fs::write(
        l.repo.join(alternative),
        String::from_utf8(reviewed.clone())
            .unwrap()
            .replace("+b", "+other"),
    )
    .unwrap();
    let (code, out, err) = l.oms(&["patch-admit", "--patch", alternative, "--plan-task", "t2"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(
        err.contains("patch differs from exact reviewed evidence"),
        "{err}"
    );
    for (path, original, changed, message) in [
        (
            reviewed_patch,
            reviewed.as_slice(),
            String::from_utf8(reviewed.clone())
                .unwrap()
                .replace("+b", "+changed")
                .into_bytes(),
            "reviewed patch bytes changed",
        ),
        (
            ".oms/p/r.md",
            report.as_slice(),
            b"changed report\n".to_vec(),
            "reviewed artifact bytes changed",
        ),
    ] {
        std::fs::write(l.repo.join(path), changed).unwrap();
        for verb in ["patch-admit", "patch-land"] {
            let (code, out, err) = l.oms(&[verb, "--patch", reviewed_patch, "--plan-task", "t2"]);
            assert_eq!(code, 2, "{out}{err}");
            assert!(err.contains(message), "{err}");
            assert_eq!(
                std::fs::read(l.repo.join(".oms/plan/tasks.json")).unwrap(),
                before
            );
            assert_eq!(
                std::fs::read_to_string(l.repo.join("a.txt")).unwrap(),
                "a\n"
            );
        }
        std::fs::write(l.repo.join(path), original).unwrap();
    }
    let mutate_report = format!(
        "printf 'changed during advisory verification\\n' > '{}'",
        l.repo.join(".oms/p/r.md").display(),
    );
    let (code, out, err) = l.oms(&[
        "patch-admit",
        "--patch",
        reviewed_patch,
        "--plan-task",
        "t2",
        "--red-check",
        &mutate_report,
    ]);
    assert_eq!(
        std::fs::read_to_string(l.repo.join(".oms/p/r.md")).unwrap(),
        "changed during advisory verification\n",
        "advisory command did not run: {out}{err}"
    );
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("reviewed artifact bytes changed"), "{err}");
    assert_eq!(
        std::fs::read(l.repo.join(".oms/plan/tasks.json")).unwrap(),
        before
    );
    std::fs::write(l.repo.join(".oms/p/r.md"), &report).unwrap();
    let selected: serde_json::Value =
        serde_json::from_str(&l.ok(&["agent-plan", "show", "--id", "t2"])).unwrap();
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(l.repo.join(".oms/plan/tasks.json")).unwrap())
            .unwrap();
    let expected_review = serde_json::json!({
        "binding":{"schema":1,"plan_id":stored["plan_id"],"task_id":"t2",
            "task_sha256":selected["task_sha256"],"base_sha":l.git(&["rev-parse", "HEAD"]),
            "lease_id":selected["lease_id"],"lease_epoch":selected["lease_epoch"]},
        "report_sha256":oms_command_plan::json::sha(&report),
        "patch_sha256":oms_command_plan::json::sha(&reviewed)
    })
    .to_string();
    let old_lease = selected["lease_id"].as_str().unwrap();
    l.ok(&[
        "agent-plan",
        "release",
        "--id",
        "t2",
        "--lease-id",
        old_lease,
    ]);
    let issued: serde_json::Value = serde_json::from_str(&l.ok(&[
        "agent-plan",
        "claim",
        "--id",
        "t2",
        "--provider",
        "codex",
        "--json",
    ]))
    .unwrap();
    let replacement = issued["lease_id"].as_str().unwrap();
    assert_ne!(replacement, old_lease);
    l.ok(&[
        "agent-plan",
        "review",
        "--id",
        "t2",
        "--lease-id",
        replacement,
        "--artifact",
        ".oms/p/r.md",
        "--patch",
        reviewed_patch,
        "--expected-review-artifact-sha256",
        &oms_command_plan::json::sha(&report),
        "--expected-review-patch-sha256",
        &oms_command_plan::json::sha(&reviewed),
    ]);
    let (code, out, err) = l.oms(&[
        "patch-land",
        "--plan-task",
        "t2",
        "--expected-plan-review",
        &expected_review,
    ]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("reviewed lease changed"), "{err}");
    assert!(l.events().is_empty());
    assert_eq!(
        std::fs::read_to_string(l.repo.join("a.txt")).unwrap(),
        "a\n"
    );
    assert!(!l.repo.join("tests/review-binding.txt").exists());
    let (code, out, err) = l.oms(&[
        "patch-admit",
        "--patch",
        reviewed_patch,
        "--plan-task",
        "t2",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("ADMIT"), "{out}");
    let (code, out, err) = l.oms(&["patch-land", "--patch", reviewed_patch, "--plan-task", "t2"]);
    assert_eq!(code, 0, "{out}{err}");
    let done: serde_json::Value =
        serde_json::from_str(&l.ok(&["agent-plan", "show", "--id", "t2"])).unwrap();
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        done["review_evidence"]["patch"]["sha256"],
        oms_command_plan::json::sha(&reviewed)
    );
    assert_ne!(done["patch"], reviewed_patch);
    assert_eq!(
        std::fs::read_to_string(l.repo.join("tests/review-binding.txt")).unwrap(),
        "bound\n"
    );
    assert_eq!(
        std::fs::read_to_string(l.repo.join("a.txt")).unwrap(),
        "a\nb\n"
    );
    l.git(&["commit", "-qam", "landed t2"]);
    let (code, out, err) = l.oms(&[
        "patch-admit",
        "--patch",
        ".oms/p/p.patch",
        "--plan-task",
        "nosuch",
    ]);
    assert_eq!(code, 2, "{out}{err}");
}

#[test]
fn landing_report_from_failure_drafts_only_allowlisted_fields() {
    let l = Landing::new();
    let extra = [("OMS_REPORT_REPO", "example/oms")];
    let draft = |fp: &str| {
        l.oms_env(
            &[
                "report",
                "bug",
                "--from-failure",
                fp,
                "--title",
                "t",
                "--message",
                "m",
                "--dry-run",
            ],
            &extra,
        )
    };
    let detail = "--patch private-argument";
    l.ok(&[
        "fail-ledger",
        "record",
        "--cmd",
        &format!("oms patch-land {detail}"),
        "--exit",
        "1",
        "--summary",
        "free text",
    ]);
    l.ok(&[
        "fail-ledger",
        "record",
        "--cmd",
        "bash -c thing",
        "--exit",
        "7",
    ]);
    let rows: serde_json::Value =
        serde_json::from_str(&l.ok(&["fail-ledger", "list", "--json"])).unwrap();
    let rows = rows
        .as_array()
        .or_else(|| rows["failures"].as_array())
        .unwrap()
        .clone();
    let fp = |prefix: &str| {
        rows.iter()
            .find(|r| r["cmd"].as_str().unwrap().starts_with(prefix))
            .unwrap()["fingerprint"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (code, out, err) = draft(&fp("oms "));
    assert_eq!(code, 0, "{err}");
    for line in [
        "Verb: oms patch-land",
        "Exit: 1",
        "Failure code: unknown_failure",
        "Repeat count: 1",
    ] {
        assert!(out.contains(line), "{line}: {out}");
    }
    assert!(
        !out.contains("private-argument") && !out.contains("free text"),
        "{out}"
    );
    let (code, out, _) = draft(&fp("bash "));
    assert_eq!(code, 0);
    assert!(
        out.contains("Verb: unavailable\n") && out.contains("Exit: 7\n"),
        "{out}"
    );
    for (fp, message) in [
        ("0123456789abcdef", "failure record unavailable"),
        ("0123456789abcde", "malformed failure fingerprint"),
        ("0123456789abcdef0", "malformed failure fingerprint"),
        ("0123456789ABCDEF", "malformed failure fingerprint"),
        ("", "--from-failure requires a nonempty value"),
    ] {
        let (code, out, err) = draft(fp);
        assert_eq!(code, 2, "{fp:?}: {out}{err}");
        assert!(err.contains(message), "{fp:?}: {err}");
    }
}

#[test]
fn landing_cleanup_removes_only_dead_owned_clean_residue() {
    use std::os::unix::fs::symlink;
    let l = Landing::new();
    let dead = {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    };
    let tmp = l.f.0.join("tmp");
    let residue = |base: &std::path::Path,
                   kind: &str,
                   suffix: &str,
                   pid: &str,
                   repo: &std::path::Path| {
        let dir = base.join(format!("oh-my-setting-{kind}.{suffix}"));
        std::fs::create_dir(&dir).unwrap();
        let worktree = dir.join(format!("wt-{suffix}"));
        l.git_in(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::write(
            dir.join(".oh-my-setting-tmp"),
            format!(
                "kind=oh-my-setting-temp\ntemporary=1\npid={pid}\nrepo={}\nworktree={}\nscratch=0\n",
                repo.display(),
                worktree.display()
            ),
        )
        .unwrap();
        dir
    };
    let empty = l.ok(&["cleanup", "--dry-run"]);
    assert!(
        empty.ends_with("\ncleanup: 0 removable item(s) found, 0 kept with work\n"),
        "{empty}"
    );

    let dead_s = dead.to_string();
    let live_s = std::process::id().to_string();
    let removable = residue(&tmp, "delegate", "dead", &dead_s, &l.repo);
    let delegate_root = residue(&l.f.0.join("wtroot"), "admit", "root", &dead_s, &l.repo);
    let capture_dir = ".oms/artifacts/output-digest";
    let managed_root = l.f.0.join("wtroot");
    let captured = residue(&managed_root, "scratch", "captured", &dead_s, &l.repo);
    let captures = captured.join("wt-captured").join(capture_dir);
    std::fs::create_dir_all(&captures).unwrap();
    let capture = captures.join("20261124T000000Z-100000000000000000000.log");
    let proof = b"ERROR: focused verifier failed\n";
    std::fs::write(&capture, proof).unwrap();
    let unsafe_store = residue(&managed_root, "scratch", "unsafe", &dead_s, &l.repo);
    let unsafe_ancestor = unsafe_store.join("wt-unsafe/.oms/artifacts");
    std::fs::create_dir(unsafe_ancestor.parent().unwrap()).unwrap();
    symlink(l.f.0.join("missing-artifacts"), &unsafe_ancestor).unwrap();
    // An empty capture store and ignored build cache are not evidence.
    let clean_worktree = delegate_root.join("wt-root");
    std::fs::create_dir_all(clean_worktree.join(capture_dir)).unwrap();
    std::fs::write(l.repo.join(".git/info/exclude"), ".oms/\n/target/\n").unwrap();
    std::fs::create_dir(clean_worktree.join("target")).unwrap();
    std::fs::write(clean_worktree.join("target/.rustc_info.json"), "{}\n").unwrap();
    let live = residue(&tmp, "delegate", "live", &live_s, &l.repo);
    let unproven = residue(&tmp, "land", "pid", "notapid", &l.repo);
    let dirty = residue(&tmp, "scratch", "dirty", &dead_s, &l.repo);
    std::fs::write(dirty.join("wt-dirty/a.txt"), "changed\n").unwrap();
    let other = l.f.0.join("other");
    std::fs::create_dir(&other).unwrap();
    l.git_in(&other, &["init", "-q"]);
    l.git_in(&other, &["commit", "-q", "--allow-empty", "-m", "o"]);
    let foreign = residue(&tmp, "delegate", "foreign", &dead_s, &other);
    // A symlinked worktree child and a symlinked residue directory.
    let swapped = residue(&tmp, "admit", "swap", &dead_s, &l.repo);
    std::fs::rename(swapped.join("wt-swap"), l.f.0.join("moved")).unwrap();
    symlink(l.f.0.join("moved"), swapped.join("wt-swap")).unwrap();
    let target = residue(&l.f.0, "delegate", "target", &dead_s, &l.repo);
    let linked = tmp.join("oh-my-setting-delegate.linked");
    symlink(&target, &linked).unwrap();
    // Install links: only one pointing exactly at this install is ours.
    std::fs::create_dir_all(l.f.0.join("home/.claude/skills")).unwrap();
    std::fs::create_dir_all(l.f.0.join("home/.agents/skills")).unwrap();
    let ours = l.f.0.join("home/.claude/skills/oh-my-setting");
    symlink(l.f.0.join("custom-skills"), &ours).unwrap();
    let user = l.f.0.join("home/.agents/skills/oms-trace");
    symlink(l.f.0.join("elsewhere"), &user).unwrap();

    let report = l.ok(&["cleanup", "--dry-run"]);
    let line = |verb: &str, path: &std::path::Path| {
        report
            .lines()
            .any(|l| l.starts_with(&format!("{verb}: {} (", path.display())))
    };
    for path in [&removable, &delegate_root, &ours] {
        assert!(line("would remove", path), "{}: {report}", path.display());
    }
    for path in [
        &unproven,
        &dirty,
        &foreign,
        &swapped,
        &captured,
        &unsafe_store,
    ] {
        assert!(line("kept", path), "{}: {report}", path.display());
    }
    assert!(
        report.contains("worktree has uncommitted or untracked files"),
        "{report}"
    );
    assert!(report.contains("owned by another repository"), "{report}");
    assert!(report.contains("owner pid is unproven"), "{report}");
    assert!(report.contains("unsafe residue worktree path"), "{report}");
    assert!(report.contains(&format!("holds {capture_dir}")), "{report}");
    assert!(
        report.contains(&format!("{capture_dir} is unsafe or unreadable")),
        "{report}"
    );
    assert!(
        !report.contains(&live.display().to_string()) && !report.contains("linked"),
        "{report}"
    );
    assert!(
        report.ends_with("\ncleanup: 3 removable item(s) found, 6 kept with work\n"),
        "{report}"
    );
    assert!(removable.exists() && ours.symlink_metadata().is_ok());
    assert_eq!(std::fs::read(&capture).unwrap(), proof);

    let (code, _, err) = l.oms_env(&["cleanup", "--apply"], &[("OMS_HARNESS_CHILD", "1")]);
    assert_eq!(code, 2, "{err}");
    assert!(removable.exists());

    let applied = l.ok(&["cleanup", "--apply"]);
    assert!(
        applied.ends_with("\ncleanup: removed 3 item(s), 6 kept with work\n"),
        "{applied}"
    );
    assert!(!removable.exists() && !delegate_root.exists() && ours.symlink_metadata().is_err());
    for path in [
        &live, &unproven, &dirty, &foreign, &swapped, &target, &linked, &user,
    ] {
        assert!(path.symlink_metadata().is_ok(), "{}", path.display());
    }
    assert_eq!(
        std::fs::read_to_string(dirty.join("wt-dirty/a.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(std::fs::read(&capture).unwrap(), proof);
    assert_eq!(
        std::fs::read_link(&unsafe_ancestor).unwrap(),
        l.f.0.join("missing-artifacts")
    );
    let registered = l.git(&["worktree", "list", "--porcelain"]);
    assert!(
        !registered.contains("wt-dead")
            && !registered.contains("wt-root")
            && registered.contains("wt-dirty")
            && registered.contains("wt-captured")
            && registered.contains("wt-unsafe"),
        "{registered}"
    );
}

#[test]
fn landing_front_doors_never_report_an_unavailable_service() {
    let l = Landing::new();
    l.patch("b");
    let (code, _, err) = l.oms(&["patch-admit", "--patch", ".oms/p/p.patch"]);
    assert_eq!(code, 0, "{err}");
    l.ok(&["patch-land", "--patch", ".oms/p/p.patch"]);
    l.ok(&["patch-land", "--recover"]);
    l.ok(&["cleanup", "--dry-run"]);
    // Every remaining front door either succeeds or fails on its own domain
    // check (no remote, no gh), never on a missing native service.
    for args in [
        &["land"][..],
        &["change-guard", "status"],
        &["checkpoint", "list"],
        &["candidate", "table"],
        &["scratch-worktree", "list"],
        &["sync-shared"],
        &["draft-pr", "comments"],
        &["ci-status"],
        &[
            "report",
            "bug",
            "--from-failure",
            "0123456789abcdef",
            "--message",
            "m",
            "--dry-run",
        ],
    ] {
        l.oms_env(args, &[("OMS_REPORT_REPO", "example/oms")]);
    }
}

#[test]
fn github_project_mode_activates_from_the_checked_in_declaration() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let bin = f.guarded_tools();
    let (data, fork, log) = (f.0.join("data"), f.0.join("fork"), f.0.join("gh.log"));
    // On PATH and as OMS_GH_BIN: any GitHub call, configured or raw, is logged.
    let gh = bin.join("gh");
    let stub = format!(
        "#!{}\nprintf '%s\\n' \"$*\" >> \"$OMS_TEST_GH_LOG\"\nexit 1\n",
        bin.join("bash").display()
    );
    std::fs::write(&gh, stub).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A git repository standing in for the operator's state repo: a leaked
    // locator would resolve and write here.
    let ambient = f.0.join("ambient");
    std::fs::create_dir(&ambient).unwrap();
    assert!(
        f.command(&bin, &bin.join("git"))
            .current_dir(&ambient)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    let ambient_before = files(&ambient);
    for dir in ["tmp", "xdg/config", "xdg/data", "xdg/state", "xdg/cache"] {
        std::fs::create_dir_all(f.0.join(dir)).unwrap();
    }
    // Every child gets exactly this allowlist; ambient state, room, panel,
    // lease and worktree locators are cleared whether inherited or set below.
    const KEEP: &[&str] = &[
        "PATH",
        "HOME",
        "TMPDIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "OH_MY_SETTING_ROLES_DIR",
        "OMS_PYTHON_RUNTIME_ROOT",
        "OMS_TEST_PREFLIGHT",
        "OMS_HARNESS_CHILD",
        "OMS_HARNESS_DELEGATE_DEPTH",
        "OMS_GH_BIN",
        "OMS_TEST_GH_LOG",
    ];
    let seal = |mut c: Command| {
        // Hostile inheritance a live panel pane exports.
        for key in [
            "OMS_STATE_REPO",
            "OMS_ROOM_REPO",
            "OMS_DELEGATE_WORKTREE_ROOT",
            "OMS_FAIL_LEDGER",
        ] {
            c.env(key, &ambient);
        }
        for key in [
            "OMS_ROOM_ID",
            "OMS_ROOM_PARTICIPANT",
            "OMS_PANEL_MAIN_ATTEMPT",
            "OMS_ATTEMPT_ID",
            "OMS_AUTOPILOT_OWNER_ID",
            "OMS_PLAN_LEASE_ID",
        ] {
            c.env(key, "ambient");
        }
        c.env("TMPDIR", f.0.join("tmp"))
            .env("XDG_CONFIG_HOME", f.0.join("xdg/config"))
            .env("XDG_DATA_HOME", f.0.join("xdg/data"))
            .env("XDG_STATE_HOME", f.0.join("xdg/state"))
            .env("XDG_CACHE_HOME", f.0.join("xdg/cache"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("OMS_GH_BIN", &gh)
            .env("OMS_TEST_GH_LOG", &log);
        let kept: Vec<_> = c
            .get_envs()
            .filter(|(k, _)| KEEP.iter().any(|w| *k == *w))
            .filter_map(|(k, v)| Some((k.to_owned(), v?.to_owned())))
            .collect();
        c.env_clear().envs(kept);
        c
    };
    let git = |dir: &std::path::Path, args: &[&str]| {
        let mut c = seal(f.command(&bin, &bin.join("git")));
        let out = c
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let oms = |dir: &std::path::Path, args: &[&str]| {
        seal(f.private(&bin, "--oms-dispatch"))
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap()
    };
    let ok = |dir: &std::path::Path, args: &[&str]| {
        let out = oms(dir, args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };
    let refused = |dir: &std::path::Path, args: &[&str], message: &str| {
        let out = oms(dir, args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(err.contains(message), "{args:?}: {err}");
    };

    // This repository's own declaration, committed; there is no PROJECT.md.
    let declaration =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.github/oms-project.md");
    std::fs::create_dir(data.join(".github")).unwrap();
    std::fs::copy(declaration, data.join(".github/oms-project.md")).unwrap();
    git(&data, &["add", ".github/oms-project.md"]);
    git(&data, &["commit", "-qm", "declare"]);
    git(
        &data,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:eightmm/oh-my-setting.git",
        ],
    );
    let status: serde_json::Value =
        serde_json::from_slice(&ok(&data, &["github", "status", "--json"])).unwrap();
    assert_eq!(
        json!([
            status["project"],
            status["project_source"],
            status["project_active"],
            status["origin"]
        ]),
        json!([
            "eightmm/oh-my-setting",
            "tracked",
            true,
            "eightmm/oh-my-setting"
        ])
    );
    let init = String::from_utf8(ok(&data, &["agent-plan", "init", "--goal", "g"])).unwrap();
    let tasks = data.join(
        init.strip_prefix("plan: initialized (")
            .unwrap()
            .strip_suffix(")\n")
            .unwrap(),
    );
    let before = std::fs::read(&tasks).unwrap();
    refused(
        &data,
        &["agent-plan", "add", "--id", "a", "--title", "t"],
        "GitHub project mode",
    );
    assert_eq!(std::fs::read(&tasks).unwrap(), before);
    ok(
        &data,
        &[
            "agent-plan",
            "add",
            "--id",
            "b",
            "--title",
            "t",
            "--issue",
            "12",
        ],
    );

    // Worker intake carries the canonical issue reference.
    let claimed: serde_json::Value = serde_json::from_slice(&ok(
        &data,
        &[
            "agent-plan",
            "next",
            "--claim",
            "--provider",
            "codex",
            "--json",
        ],
    ))
    .unwrap();
    assert_eq!(
        json!([claimed["id"], claimed["state"], claimed["issue"]]),
        json!(["b", "claimed", "eightmm/oh-my-setting#12"])
    );

    // A fork inherits the tracked declaration but not the local plan: inactive.
    git(&data, &["clone", "-q", ".", fork.to_str().unwrap()]);
    git(
        &fork,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:fork-org/oh-my-setting.git",
        ],
    );
    exact(
        oms(&fork, &["github", "status"]),
        0,
        b"project mode: inactive (.github/oms-project.md names eightmm/oh-my-setting; origin is fork-org/oh-my-setting)\norigin: fork-org/oh-my-setting\n",
        b"",
    );
    ok(&fork, &["agent-plan", "init", "--goal", "g"]);
    ok(&fork, &["agent-plan", "add", "--id", "c", "--title", "t"]);
    refused(
        &fork,
        &[
            "github",
            "post-evidence",
            "--issue",
            "12",
            "--pull",
            "3",
            "--yes",
        ],
        "post-evidence: project mode is not active",
    );
    assert!(std::fs::read(&log).unwrap_or_default().is_empty());
    assert!(!f.0.join("preflight").exists());
    assert_eq!(files(&ambient), ambient_before);
}

#[test]
fn panel_project_graph_mcp_end_to_end_unindexed_and_indexed() {
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;

    let f = Fixture::new();
    let bin = f.binary();
    let dir = bin.parent().unwrap().to_path_buf();
    let manifest_path = dir.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let owner_path = dir.join("oms-mcp-peer-owner");
    std::fs::write(&owner_path, b"#!/usr/bin/env bash\nexit 0\n").unwrap();
    std::fs::set_permissions(&owner_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    manifest["artifacts"]["oms-mcp-peer-owner"] =
        format!("{:x}", Sha256::digest(std::fs::read(&owner_path).unwrap())).into();
    std::fs::write(&manifest_path, manifest.to_string()).unwrap();
    let demo = f.0.join("demo-repo");
    std::fs::create_dir_all(demo.join("src")).unwrap();
    std::fs::create_dir_all(demo.join("tests")).unwrap();
    for dir in [&demo, &demo.join("src"), &demo.join("tests")] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(
        demo.join("src/lib.sh"),
        "#!/usr/bin/env bash\nhelper() { echo ok; }\n",
    )
    .unwrap();
    std::fs::write(
        demo.join("src/main.sh"),
        "#!/usr/bin/env bash\nsource src/lib.sh\nrun_app() { helper; }\n",
    )
    .unwrap();
    std::fs::write(
        demo.join("tests/test-app.sh"),
        "#!/usr/bin/env bash\nsource src/main.sh\nrun_app\n",
    )
    .unwrap();
    std::fs::write(demo.join(".env"), "LOCAL_SECRET_DEMO=do_not_expose\n").unwrap();
    for rel in ["src/lib.sh", "src/main.sh", "tests/test-app.sh", ".env"] {
        std::fs::set_permissions(demo.join(rel), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    for args in [
        &["init", "-q"][..],
        &["add", "src/lib.sh", "src/main.sh", "tests/test-app.sh"][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&demo)
                .status()
                .unwrap()
                .success()
        );
    }
    let config_dir = f.0.join("xdg/oh-my-setting");
    std::fs::create_dir_all(&config_dir).unwrap();
    let catalog = config_dir.join("desktop-projects.json");
    std::fs::write(&catalog, json!({"demo": demo}).to_string()).unwrap();
    std::fs::set_permissions(&catalog, std::fs::Permissions::from_mode(0o600)).unwrap();

    let mut child = Command::new(&bin)
        .args([
            "--oms-dispatch",
            f.0.to_str().unwrap(),
            "1111111111111111111111111111111111111111",
            "--",
            "mcp-server",
        ])
        .current_dir(&f.0)
        .env("OMS_MCP_TOOL_PROFILE", "mcp-app")
        .env("XDG_CONFIG_HOME", f.0.join("xdg"))
        .env("OMS_PROJECT_GRAPH_STATE", f.0.join("unrelated-index"))
        .env_remove("OMS_PANEL_REPO")
        .env_remove("OMS_PANEL_PROJECTS")
        .env_remove("OMS_PANEL_PROJECTS_FILE")
        .env_remove("OMS_HARNESS_CHILD")
        .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
        .env_remove("OMS_ROOM_PARTICIPANT")
        .env_remove("OMS_STATE_REPO")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut rpc = |id: u64, method: &str, params: Value| -> Value {
        let req = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        writeln!(stdin, "{req}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        assert!(stdout.read_line(&mut line).unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    };

    // 1. Registered in generic read-only mcp-app profile.
    let listed = rpc(1, "tools/list", json!({}));
    assert!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "oms_panel_project_graph"
                && t["annotations"]["readOnlyHint"] == true)
    );

    // 2. Unindexed repo first-load & drilldown via real `oms graph project panel`, zero writes.
    let unindexed = rpc(
        2,
        "tools/call",
        json!({"name":"oms_panel_project_graph","arguments":{"project":"demo"}}),
    );
    assert_eq!(unindexed["result"]["isError"], false, "{unindexed}");
    let sc = &unindexed["result"]["structuredContent"];
    assert_eq!(sc["schema"], 1);
    assert_eq!(sc["kind"], "oms-panel-project-graph");
    assert_eq!(sc["status"], "ready");
    assert_eq!(sc["provenance"]["source"], "git-ls-files");
    assert_eq!(sc["provenance"]["mode"], "fallback");
    assert_eq!(sc["provenance"]["verifiedRelationships"], false);
    assert_eq!(sc["provenance"]["wroteIndex"], false);
    assert!(!sc.to_string().contains(".env"));
    assert!(!sc.to_string().contains("do_not_expose"));
    assert!(!demo.join(".oms").exists());

    let drill = rpc(
        3,
        "tools/call",
        json!({"name":"oms_panel_project_graph","arguments":{"project":"demo","node":"dir:src"}}),
    );
    assert_eq!(drill["result"]["isError"], false, "{drill}");
    assert_eq!(
        drill["result"]["structuredContent"]["selection"]["status"],
        "found"
    );

    // 3. Rejects arbitrary repo parameter and unapproved alias.
    let bad_param = rpc(
        4,
        "tools/call",
        json!({"name":"oms_panel_project_graph","arguments":{"project":"demo","repo":"/tmp"}}),
    );
    assert_eq!(bad_param["error"]["code"], -32602);
    let bad_alias = rpc(
        5,
        "tools/call",
        json!({"name":"oms_panel_project_graph","arguments":{"project":"unapproved"}}),
    );
    assert_eq!(bad_alias["result"]["isError"], true);

    // 4. Build real index explicitly, then verify indexed read distinguishes EXTRACTED edges and writes nothing.
    let build = Command::new(&bin)
        .args([
            "--oms-dispatch",
            f.0.to_str().unwrap(),
            "1111111111111111111111111111111111111111",
            "--",
            "graph",
            "project",
            "build",
            "--repo",
            demo.to_str().unwrap(),
            "--json",
        ])
        .current_dir(&demo)
        .env_remove("OMS_HARNESS_CHILD")
        .env_remove("OMS_STATE_REPO")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let graph_mtime_before = std::fs::metadata(demo.join(".oms/project-graph/graph.json"))
        .unwrap()
        .modified()
        .unwrap();

    let indexed = rpc(
        6,
        "tools/call",
        json!({"name":"oms_panel_project_graph","arguments":{"project":"demo"}}),
    );
    assert_eq!(indexed["result"]["isError"], false, "{indexed}");
    let isc = &indexed["result"]["structuredContent"];
    assert_eq!(isc["provenance"]["source"], "indexed-graph");
    assert_eq!(isc["provenance"]["mode"], "indexed");
    assert_eq!(isc["provenance"]["verifiedRelationships"], true);
    assert_eq!(isc["provenance"]["wroteIndex"], false);
    assert!(
        isc["counts"]["confidence"]["EXTRACTED"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "expected EXTRACTED edges: {isc}"
    );
    let graph_mtime_after = std::fs::metadata(demo.join(".oms/project-graph/graph.json"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(graph_mtime_before, graph_mtime_after);

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}

fn add_companions(fixture: &Fixture, names: &[&str]) {
    use std::os::unix::fs::PermissionsExt;
    let binary = fixture.binary();
    let dir = binary.parent().unwrap();
    let manifest = dir.join("manifest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    for &name in names {
        let path = dir.join(name);
        std::fs::write(&path, name).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        value["artifacts"][name] = format!("{:x}", Sha256::digest(name.as_bytes())).into();
    }
    std::fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn fixture_panel_tool(
    mut command: Command,
    profile: &str,
    repo: &std::path::Path,
    tool: &str,
) -> (serde_json::Value, usize) {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = command
        .arg("mcp-server")
        .env("OMS_MCP_TOOL_PROFILE", profile)
        .env(
            "OMS_PANEL_PROJECTS",
            serde_json::json!({"fixture":repo}).to_string(),
        )
        .env_remove("OMS_PANEL_REPO")
        .env_remove("OMS_PANEL_PROJECTS_FILE")
        .env_remove("OMS_ROOM_PARTICIPANT")
        .env_remove("OMS_HARNESS_CHILD")
        .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":{"project":"fixture"}}});
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let reply: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    (
        reply["result"]["structuredContent"].clone(),
        out.stdout.len(),
    )
}

#[test]
fn panel_capabilities_matches_native_oms_list_all_without_side_effects() {
    let fixture = Fixture::new();
    add_companions(&fixture, &["oms-mcp-peer-owner"]);
    let bin = fixture.binary();
    let revision = bin.parent().unwrap().file_name().unwrap();
    let invoke = || {
        let mut cmd = Command::new(&bin);
        cmd.arg("--oms-dispatch")
            .arg(&fixture.0)
            .arg(revision)
            .arg("--")
            .current_dir(&fixture.0);
        cmd
    };
    let listed = invoke().args(["list", "--all"]).output().unwrap();
    assert!(listed.status.success());
    let expected = std::str::from_utf8(&listed.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            let (name, description) = l.split_once(' ').unwrap();
            serde_json::json!({"name":name,"description":description.trim_start()})
        })
        .collect::<Vec<_>>();
    assert!(!expected.is_empty());
    let (data, _) = fixture_panel_tool(invoke(), "mcp-app", &fixture.0, "oms_panel_capabilities");
    assert_eq!(data["commands"], serde_json::json!(expected), "{data}");
    assert_eq!(data["total"], expected.len());
    assert_eq!(data["omitted"], 0);
    assert!(!fixture.0.join(".oms").exists());
}
