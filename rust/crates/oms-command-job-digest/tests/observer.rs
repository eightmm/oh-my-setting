use oms_command_job_digest::run;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn bound() -> Duration {
    Duration::from_secs(
        std::env::var("OMS_TEST_CALL_TIMEOUT")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(30),
    )
}
fn observe(root: &Path, args: &[&str], interrupted: &AtomicBool) -> (i32, String, String) {
    let (code, out, err) = run(
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        root,
        interrupted,
    );
    if std::env::var("OMS_DIGEST_MATRIX_MODE").is_ok_and(|mode| mode == "account-raw") {
        assert!(
            out.windows(b"COMPLETED\xff".len())
                .any(|bytes| bytes == b"COMPLETED\xff")
        );
    }
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8(err).unwrap(),
    )
}
fn wait_for(path: &Path) {
    let until = Instant::now() + bound();
    while !path.exists() {
        assert!(
            Instant::now() < until,
            "condition not observed: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn assert_not_running(pid: i32) {
    let until = Instant::now() + bound();
    loop {
        let gone = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err();
        // A dead orphan can remain a zombie until the host init process reaps it.
        let zombie = fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
            s.rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z'))
        });
        if gone || zombie {
            break;
        }
        assert!(
            Instant::now() < until,
            "owned descendant {pid} survived cleanup"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn scheduler_matrix() {
    if let Some(root) = std::env::var_os("OMS_DIGEST_MATRIX_ROOT") {
        let root = PathBuf::from(root);
        let mode = std::env::var("OMS_DIGEST_MATRIX_MODE").unwrap();
        let interrupted = AtomicBool::new(false);
        fs::write(root.join("run.log"), "ERROR retained\n").unwrap();
        let timeout = if mode == "retry" { "10" } else { "1" };
        let mut args = vec!["--wait", "--wait-timeout", timeout, "123_4", "run.log"];
        if mode == "no-budget" {
            args = vec!["--wait", "123_4", "run.log"];
        }
        if mode == "remove-metrics" {
            fs::write(
                root.join("metrics"),
                "{\"step\":1,\"name\":\"loss\",\"value\":1}\n",
            )
            .unwrap();
            args.extend(["--metrics", "metrics"]);
        }
        let started = Instant::now();
        let result = std::thread::scope(|scope| {
            if mode.starts_with("interrupt") {
                let flag = &interrupted;
                let marker = root.join(if mode == "interrupt-sleep" {
                    "queue-called"
                } else {
                    "descendant"
                });
                scope.spawn(move || {
                    wait_for(&marker);
                    flag.store(true, Ordering::Relaxed);
                });
            }
            observe(&root, &args, &interrupted)
        });
        let (code, out, err) = result;
        assert!(started.elapsed() < bound());
        if mode == "remove-log" {
            assert_eq!(code, 1, "{out}");
            assert!(out.starts_with("# Job digest"));
            assert!(out.contains("- log: run.log ( lines)"));
            assert!(
                err.contains("run.log") && err.contains("No such file"),
                "{err}"
            );
        } else if mode == "remove-metrics" {
            assert_eq!(code, 0);
            assert!(out.contains("## Tail"));
            assert!(out.contains("Metrics section failed; read the file directly."));
            assert!(
                err.contains("FileNotFoundError") && err.contains("metrics"),
                "{err}"
            );
        } else if mode.starts_with("interrupt") {
            assert_eq!(code, 130, "{err}");
            assert!(out.is_empty());
        } else if ["queue-hang", "pipe-hang", "outage", "whitespace"].contains(&mode.as_str()) {
            assert_eq!(code, 124, "{err}");
            assert!(out.contains("wait: pending"));
            assert!(out.contains("## Tail"));
            assert!(err.contains("queue state may be unknown"));
            assert!(!err.contains("no longer queued"));
            if mode == "outage" {
                assert_eq!(err.matches("transiently").count(), 1);
                assert!(err.contains("next=1s"), "{err}");
            }
        } else {
            assert_eq!(code, 0, "{err}");
            assert!(out.contains("## Tail"));
            assert!(err.contains("no longer queued"));
            if ["account-hang", "account-fail", "account-empty"].contains(&mode.as_str()) {
                assert!(out.contains("outcome unknown, not verified"));
            } else if mode == "account-many" {
                assert!(out.contains("row17"));
                assert!(!out.contains("row18"));
            } else if mode == "account-mixed" {
                assert!(out.contains("header\nrule\nCOMPLETED\nwarning"), "{out}");
            } else {
                assert!(out.contains("COMPLETED"), "{out}");
            }
            if mode == "retry" {
                assert_eq!(err.matches("transiently").count(), 1, "{err}");
                assert!(err.contains("failed 2 times while waiting"));
                assert_eq!(fs::read_to_string(root.join("calls")).unwrap(), "4");
            }
        }
        if let Ok(pid) = fs::read_to_string(root.join("descendant")) {
            assert_not_running(pid.parse().unwrap());
        }
        assert!(!root.join("forbidden").exists());
        return;
    }
    let root = std::env::temp_dir().join(format!("oms-digest-observer-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let source = root.join("actor.rs");
    fs::write(&source, include_str!("support/observer.inc")).unwrap();
    let actor = root.join("actor");
    assert!(
        Command::new("rustc")
            .args(["--edition", "2024"])
            .arg(source)
            .arg("-o")
            .arg(&actor)
            .status()
            .unwrap()
            .success()
    );
    for name in ["squeue", "sacct", "scancel", "sbatch"] {
        fs::copy(&actor, root.join(name)).unwrap();
    }
    let mut failures = Vec::new();
    for mode in [
        "success",
        "invalid-id",
        "unknown-job",
        "retry",
        "outage",
        "whitespace",
        "queue-hang",
        "pipe-hang",
        "account-hang",
        "account-fail",
        "account-empty",
        "account-mixed",
        "account-raw",
        "account-many",
        "account-equal",
        "interrupt-queue",
        "interrupt-account",
        "interrupt-sleep",
        "no-budget",
        "relative-path",
        "remove-log",
        "remove-metrics",
    ] {
        for name in ["calls", "descendant", "queue-called"] {
            let _ = fs::remove_file(root.join(name));
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "scheduler_matrix", "--nocapture"])
            .env(
                "PATH",
                if mode == "relative-path" {
                    Path::new(".")
                } else {
                    &root
                },
            )
            .env("OMS_JOB_DIGEST_POLL", "0")
            .env("OMS_POLL_VERBOSE", "1")
            .env("OMS_DIGEST_MATRIX_ROOT", &root)
            .env("OMS_DIGEST_MATRIX_MODE", mode)
            .status()
            .unwrap();
        if !status.success() {
            failures.push(mode);
        }
    }
    fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "observer modes failed: {failures:?}");
}
