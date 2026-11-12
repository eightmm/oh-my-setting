use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "oms-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn call(&self, args: &[&str], child: bool) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_oms"));
        c.args(["runtime", "--repo"])
            .arg(&self.0)
            .arg("context-inspect")
            .args(args);
        if child {
            c.env("OMS_HARNESS_CHILD", "1");
        } else {
            c.env_remove("OMS_HARNESS_CHILD");
        }
        c.output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn vectors() -> Value {
    let root = std::env::var_os("OMS_CONTRACT_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .find(|p| {
                    p.join("migration/contracts/context-inspect-v1.json")
                        .exists()
                })
                .unwrap()
                .to_path_buf()
        });
    serde_json::from_slice(
        &std::fs::read(root.join("migration/contracts/context-inspect-v1.json")).unwrap(),
    )
    .unwrap()
}
#[test]
fn frozen_vectors_through_binary() {
    let f = Fixture::new();
    for case in vectors()["cases"].as_array().unwrap() {
        std::fs::write(
            f.0.join("pack.json"),
            serde_json::to_vec(&case["input"]).unwrap(),
        )
        .unwrap();
        for child in [false, true] {
            let out = f.call(&["pack.json"], child);
            if let Some(expected) = case.get("expected") {
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                assert_eq!(
                    serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                    *expected
                );
            } else {
                assert_eq!(out.status.code(), Some(2));
                assert!(out.stdout.is_empty());
                assert!(
                    String::from_utf8_lossy(&out.stderr)
                        .contains(case["expected_error"].as_str().unwrap())
                );
            }
        }
    }
}
#[test]
fn compare_current_text_and_denials() {
    let f = Fixture::new();
    let data = vectors()["cases"][0]["input"].clone();
    std::fs::write(f.0.join("pack.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    let pretty = f.call(&["pack.json", "--pretty"], false);
    assert!(pretty.status.success());
    assert!(!pretty.stdout.ends_with(b"\n\n"));
    let value: Value = serde_json::from_slice(&pretty.stdout).unwrap();
    assert_eq!(value, vectors()["cases"][0]["expected"]);
    let out = f.call(&["pack.json", "--verify-current"], false);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["missing"], 1);
    std::fs::write(f.0.join("src.py"), vec![b'x'; 100]).unwrap();
    let out = f.call(&["pack.json", "--verify-current"], false);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["unchanged"], 1);
    std::fs::write(f.0.join("src.py"), b"changed").unwrap();
    let out = f.call(&["pack.json", "--verify-current"], false);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["changed"], 1);
    std::fs::remove_file(f.0.join("src.py")).unwrap();
    std::os::unix::fs::symlink("pack.json", f.0.join("src.py")).unwrap();
    let out = f.call(&["pack.json", "--verify-current"], false);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["unreadable"], 1);

    let out = f.call(
        &["pack.json", "--compare", "pack.json", "--format", "text"],
        true,
    );
    assert!(out.status.success());
    assert!(out.stdout.ends_with(b"\n"));
    let out = f.call(
        &["pack.json", "--compare", "pack.json", "--verify-current"],
        false,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let out = f.call(&["--session", "missing"], true);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("a harness child may only use read-only runtime actions")
    );
}

#[test]
fn sources_beyond_display_limit_and_path_safety() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let mut data = vectors()["cases"][0]["input"].clone();
    let mut other = data["selected"][0].clone();
    other["path"] = serde_json::json!("second.py");
    data["selected"].as_array_mut().unwrap().push(other);
    data["selected_bytes"] = serde_json::json!(240);
    data["remaining_bytes"] = serde_json::json!(760);
    std::fs::write(f.0.join("pack.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    let out = f.call(&["pack.json", "--verify-current", "--limit", "1"], false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sources_current"]["missing"], 2);
    assert_eq!(v["sources_current"]["missing_paths"]["omitted"], 1);
    symlink("pack.json", f.0.join("link")).unwrap();
    let out = f.call(&["link"], false);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("cannot read a valid repo-local context manifest")
    );
    std::fs::hard_link(f.0.join("pack.json"), f.0.join("hard")).unwrap();
    let out = f.call(&["hard"], false);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("context manifest must be an owned, bounded regular file without links")
    );
    std::fs::remove_file(f.0.join("hard")).unwrap();
    for path in ["link", "../pack.json", "missing"] {
        let out = f.call(&[path], false);
        assert_eq!(out.status.code(), Some(2), "{path}");
        assert!(out.stdout.is_empty());
    }
    for limit in ["0", "101", "bad", ""] {
        assert_eq!(
            f.call(&["pack.json", "--limit", limit], false)
                .status
                .code(),
            Some(2)
        );
    }
    assert!(
        f.call(&["pack.json", "--limit", "100"], false)
            .status
            .success()
    );
}

#[test]
fn help_and_parse_failures_precede_child_policy() {
    let f = Fixture::new();
    let root_help = Command::new(env!("CARGO_BIN_EXE_oms"))
        .args(["runtime", "--help"])
        .output()
        .unwrap();
    assert!(root_help.status.success());
    assert!(root_help.stdout.starts_with(b"usage: oms runtime [-h]"));
    let empty_repo = Command::new(env!("CARGO_BIN_EXE_oms"))
        .current_dir(&f.0)
        .args(["runtime", "--repo", "", "context-inspect", "missing"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&empty_repo.stderr)
            .contains("cannot read a valid repo-local context manifest")
    );
    std::fs::create_dir(f.0.join("nested")).unwrap();
    let tilde = Command::new(env!("CARGO_BIN_EXE_oms"))
        .env("HOME", &f.0)
        .args([
            "runtime",
            "--repo",
            "~//nested",
            "context-inspect",
            "missing",
        ])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&tilde.stderr)
            .contains("cannot read a valid repo-local context manifest")
    );
    let ambiguous = f.call(&["--=foo"], true);
    assert_eq!(ambiguous.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr)
            .contains("ambiguous option: --=foo could match --help, --repo, --pretty")
    );
    let help = f.call(&["--help"], false);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert!(
        help.stdout
            .starts_with(b"usage: oms runtime context-inspect")
    );
    for args in [
        &["--session", "missing", "-x"][..],
        &["--session", "missing", "--limit", "++5"][..],
        &["--session", "missing", "--limit", "+-5"][..],
        &["--session", "missing", "--=foo"][..],
        &["--session", "missing", "--limit", "bad"][..],
        &["--session", "--pretty"][..],
        &["--format", "invalid"][..],
    ] {
        let out = f.call(args, true);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("error:"));
    }
}

#[test]
fn interrupt_reaps_blocked_git() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let fifo = f.0.join("git-config");
    nix::unistd::mkfifo(
        &fifo,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_oms"))
        .args(["runtime", "--repo"])
        .arg(&f.0)
        .args(["context-inspect", "missing"])
        .env("GIT_CONFIG_GLOBAL", &fifo)
        .env_remove("OMS_HARNESS_CHILD")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let timeout = std::env::var("OMS_TEST_CALL_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut writer = loop {
        if let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(&fifo)
        {
            break file;
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
            panic!("interrupted process did not exit");
        }
        std::thread::yield_now();
    }
    use std::io::Write;
    assert_eq!(
        writer.write_all(b"x").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    drop(writer);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(130));
    assert!(out.stdout.is_empty());
    assert_eq!(out.stderr, b"runtime-core: interrupted\n");
}

#[test]
fn argv_boundaries_and_parse_precedence() {
    let f = Fixture::new();
    for name in ["--help", "-h", "--version", "--repo"] {
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(["runtime", "--", name])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("invalid choice"));
    }
    std::fs::write(
        f.0.join("-2026.json"),
        serde_json::to_vec(&vectors()["cases"][0]["input"]).unwrap(),
    )
    .unwrap();
    let rejected = f.call(&["-2026.json"], false);
    assert!(rejected.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("unrecognized arguments: -2026.json")
    );
    let out = f.call(&["--session", "-1abc"], true);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("argument --session: expected one argument")
    );
    let out = f.call(&["--limit", "-.5", "pack.json"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid int value: '-.5'"));
    let out = f.call(&["-.5"], false);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("cannot read a valid repo-local context manifest")
    );
    for args in [
        &["runtime", "--unknown", "--help"][..],
        &["runtime", "--unknown", "--version"][..],
        &["runtime", "context-inspect", "--unknown", "--help"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(out.stderr.is_empty());
    }
    for args in [
        &["runtime", "--repo", "context-inspect"][..],
        &["runtime", "--repo", "--pretty", "context-inspect"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stderr.starts_with(b"usage: oms runtime [-h]"));
    }
}

#[test]
fn unreadable_repository_preserves_error_order() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let dir = f.0.join("sealed");
    std::fs::create_dir(&dir).unwrap();
    for mode in [0o000, 0o111] {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        let output = |args: &[&str]| {
            Command::new(env!("CARGO_BIN_EXE_oms"))
                .args(["runtime", "--repo"])
                .arg(&dir)
                .arg("context-inspect")
                .args(args)
                .output()
                .unwrap()
        };
        let unreadable = std::fs::read_dir(&dir).is_err();
        let pretty = output(&["missing", "--format", "text", "--pretty"]);
        let missing = output(&[]);
        let read = output(&["missing"]);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            String::from_utf8_lossy(&pretty.stderr)
                .contains("--pretty applies only to JSON output")
        );
        assert!(String::from_utf8_lossy(&missing.stderr).contains("context-inspect takes one of:"));
        if unreadable {
            assert!(
                String::from_utf8_lossy(&read.stderr)
                    .contains("cannot read a valid repo-local context manifest")
            );
        }
    }
}

#[test]
fn git_output_larger_than_pipe_is_drained() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let source = f.0.join("fixture.rs");
    let fake = f.0.join("git");
    std::fs::write(
        &source,
        r#"use std::io::Write;
fn main() {
    let mut out = std::io::stdout().lock();
    out.write_all(&vec![b' '; 131072]).unwrap();
    out.write_all(std::env::var_os("OMS_FAKE_GIT_ROOT").unwrap().as_encoded_bytes()).unwrap();
    out.write_all(b"\r\n").unwrap();
}"#,
    )
    .unwrap();
    assert!(
        Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&fake)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        f.0.join("pack.json"),
        serde_json::to_vec(&vectors()["cases"][0]["input"]).unwrap(),
    )
    .unwrap();
    let nested = f.0.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_oms"))
        .args(["runtime", "--repo"])
        .arg(&nested)
        .args(["context-inspect", "pack.json"])
        .env("PATH", &f.0)
        .env("OMS_FAKE_GIT_ROOT", &f.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let timeout = std::env::var("OMS_TEST_CALL_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("CLI did not drain Git output");
        }
        std::thread::yield_now();
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        vectors()["cases"][0]["expected"]
    );
}

#[test]
fn help_on_closed_stdout_exits_without_panic() {
    // Other parallel tests spawn processes; isolate the pipe so none can inherit its reader.
    if std::env::var_os("OMS_TEST_CLOSED_PIPE_ISOLATED").is_none() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "help_on_closed_stdout_exits_without_panic"])
            .env("OMS_TEST_CLOSED_PIPE_ISOLATED", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    for args in [
        &["runtime", "--help"][..],
        &["runtime", "--version"][..],
        &["runtime", "context-inspect", "--help"][..],
    ] {
        let (reader, writer) = nix::unistd::pipe().unwrap();
        drop(reader);
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(args)
            .stdout(std::process::Stdio::from(writer))
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
    }
}

#[test]
fn unicode_limits_and_help_forms() {
    let f = Fixture::new();
    std::fs::write(
        f.0.join("pack.json"),
        serde_json::to_vec(&vectors()["cases"][0]["input"]).unwrap(),
    )
    .unwrap();
    for limit in ["١٠", "０１", "+१२", "1_٠"] {
        let out = f.call(&["pack.json", "--limit", limit], false);
        assert!(
            out.status.success(),
            "{limit}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = f.call(&["--limit", limit, "--help"], false);
        assert!(out.status.success(), "{limit}");
    }
    let mut data = vectors()["cases"][0]["input"].clone();
    let mut second = data["selected"][0].clone();
    second["path"] = serde_json::json!("second.py");
    data["selected"].as_array_mut().unwrap().push(second);
    data["selected_bytes"] = serde_json::json!(240);
    data["remaining_bytes"] = serde_json::json!(760);
    std::fs::write(f.0.join("pack.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    let out = f.call(&["pack.json", "--limit", "０１"], false);
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["sources"]["omitted"], 1);
    for malformed in ["", "١__٢", "\u{1c}1"] {
        let out = f.call(&["--limit", malformed, "--help"], false);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
    for token in ["-١", "-.٥"] {
        let out = f.call(&["--session", token], true);
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("a harness child may only use read-only runtime actions")
        );
    }
    let out = f.call(&["--session", "-١x"], true);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("argument --session: expected one argument")
    );
    for token in ["-hh", "-hhh", "-hfoo"] {
        assert!(f.call(&[token], false).status.success(), "{token}");
    }
    for token in ["-h=foo", "-h--"] {
        let out = f.call(&[token], false);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("argument -h/--help: ignored explicit argument")
        );
    }
    for args in [
        &["runtime", "--pretty=foo"][..],
        &["runtime", "--version=foo"][..],
        &["runtime", "context-inspect", "--help=foo"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_oms"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("ignored explicit argument 'foo'"));
    }
}
