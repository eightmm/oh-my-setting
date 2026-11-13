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
        for name in ["bash", "git", "dirname", "uname", "readlink", "awk", "wc"] {
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
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
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
    let bin = f.tool_path();
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
