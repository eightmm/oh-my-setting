use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oms-native-entry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
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
        let dir = root.join("rust/target/oms-native").join(revision);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_oms"), dir.join("oms")).unwrap();
        let digest = format!(
            "{:x}",
            Sha256::digest(std::fs::read(dir.join("oms")).unwrap())
        );
        std::fs::write(dir.join("manifest.json"), format!("{{\"schema\":1,\"source_commit\":\"{revision}\",\"artifacts\":{{\"oms\":\"{digest}\"}}}}" )).unwrap();
        std::fs::write(dir.join("oms.sha256"), format!("{digest}\n")).unwrap();
        std::fs::create_dir(root.join("scripts")).unwrap();
        let source = std::env::var_os("OMS_CONTRACT_REPO")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."));
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
            .join("rust/target/oms-native/1111111111111111111111111111111111111111/oms")
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
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        for name in [
            "agent-task.sh",
            "runtime.sh",
            "artifact-index.sh",
            "agent-role.sh",
            "input.sh",
            "agent-events.sh",
            "job-digest.sh",
            "lib/python-runtime.sh",
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
fn dispatch_leaves_other_runtime_commands_to_legacy() {
    let f = Fixture::new();
    let out = Command::new(f.binary())
        .arg("--oms-dispatch")
        .arg(&f.0)
        .args([
            "1111111111111111111111111111111111111111",
            "--",
            "runtime",
            "--repo",
            "context-inspect",
            "envelope",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(75));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
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
        vec!["artifact-index", "telemetry", "--waste"],
        vec!["artifact-index", "list"],
        vec!["agent-events", "show", "--attempt", "missing"],
        vec!["agent-events", "list"],
        vec!["input", "--help"],
        vec!["input", "attach", "terminal"],
        vec!["input", "list"],
        vec!["runtime", "experiment", "launch", "--help"],
        vec!["runtime", "experiment", "register", "{}"],
        vec![
            "runtime",
            "--repo",
            "template",
            "experiment",
            "show",
            "template",
        ],
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
        vec!["job-digest", "123", "missing.log"],
        vec!["artifact-index", "telemetry", "--repo", "absent"],
        vec!["artifact-index", "telemetry", "--by", "bad"],
        vec!["agent-role", "init", "--name", "never-created"],
        vec!["agent-role", "surface"],
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
        vec!["agent-events", "show", "--attempt", "missing", "--bundle"],
    ] {
        exact(
            f.private(&bin, "--oms-select").args(argv).output().unwrap(),
            0,
            b"",
            b"",
        );
    }
    for (child, depth, expected) in [
        ("1", "0", 75),
        ("0", "1", 75),
        ("0", "2", 75),
        ("0", "", 75),
        ("0", "bad", 75),
        ("0", "0", 0),
    ] {
        for mode in ["--oms-select", "--oms-dispatch"] {
            if expected == 0 && mode == "--oms-dispatch" {
                continue;
            }
            exact(
                f.private(&bin, mode)
                    .args(["input", "attachment", "missing", "--full"])
                    .env("OMS_HARNESS_CHILD", child)
                    .env("OMS_HARNESS_DELEGATE_DEPTH", depth)
                    .output()
                    .unwrap(),
                expected,
                b"",
                b"",
            );
        }
    }
    assert_eq!(files(&data), before);
    assert!(!f.0.join("preflight").exists());
    exact(
        f.public(&bin)
            .args(["input", "attachment", "missing", "--full"])
            .env("OMS_HARNESS_CHILD", "1")
            .output()
            .unwrap(),
        1,
        b"",
        b"error: OMS requires Python 3.11+; reinstall to provision the managed runtime\n",
    );
    assert!(f.0.join("preflight").is_file());
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

// Expected bytes captured through scripts/oms in the immutable reference checkout.
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
            "usage: oms runtime [-h] [--repo REPO] [--pretty] [--version]\n                   {envelope,evidence,next,brief,failure,context,context-inspect,profile,release,capsule,backend,experiment,benchmark,doctor}\n                   ...\noms runtime: error: unrecognized arguments: --first --last\nhint: run `oms runtime --help` for usage\n",
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
            "usage: oms runtime [-h] [--repo REPO] [--pretty] [--version]\n                   {envelope,evidence,next,brief,failure,context,context-inspect,profile,release,capsule,backend,experiment,benchmark,doctor}\n                   ...\noms runtime: error: argument --repo: expected one argument\nhint: run `oms runtime --help` for usage\n",
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
    use std::os::unix::ffi::OsStringExt;
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
    let raw = OsString::from_vec(b"raw-\xff.log".to_vec());
    std::fs::write(data.join(&raw), b"ERROR \xfe\n").unwrap();
    let before = files(&data);
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
