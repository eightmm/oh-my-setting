#![forbid(unsafe_code)]

use oms_command_autonomy::{CombinedOutcome, verify_combined};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
fn commit_file(repo: &Path, branch: &str, base: &str, name: &str, body: &str) -> String {
    git(repo, &["checkout", "-q", "-B", branch, base]);
    fs::write(repo.join(name), body).unwrap();
    git(repo, &["add", name]);
    git(repo, &["commit", "-qm", name]);
    git(repo, &["rev-parse", "HEAD"])
}
/// Primary on `main`; `target` is a ref nobody has checked out. `a` renames the
/// callee, `b` adds a new caller of the old name in another file: the merge is
/// textually clean but the combination is semantically broken.
struct Setup {
    fixture: Fixture,
    base: String,
    a: String,
    b: String,
}
fn setup() -> Setup {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "oms-combined-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&dir).unwrap();
    let repo = fs::canonicalize(&dir).unwrap();
    let fixture = Fixture(repo.clone());
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("callee.txt"), "name=old\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["branch", "target", &base]);
    let a = commit_file(&repo, "side-a", &base, "callee.txt", "name=new\n");
    let b = commit_file(&repo, "side-b", &base, "caller.txt", "use=old\n");
    git(&repo, &["checkout", "-q", "main"]);
    Setup {
        fixture,
        base,
        a,
        b,
    }
}
fn argv(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}
fn snapshot(repo: &Path) -> (String, String, String, String) {
    (
        git(repo, &["rev-parse", "HEAD"]),
        git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        git(repo, &["status", "--porcelain", "--untracked-files=all"]),
        git(repo, &["worktree", "list", "--porcelain"]),
    )
}
fn no_scratch(repo: &Path) {
    assert!(
        !repo.join(".git/worktrees").exists()
            || fs::read_dir(repo.join(".git/worktrees")).unwrap().count() == 0,
        "scratch worktree metadata remained"
    );
}

#[test]
fn textually_clean_merge_with_incompatible_combination_fails_verification() {
    let s = setup();
    let repo = &s.fixture.0;
    let before = snapshot(repo);
    let check = argv(&[
        "sh",
        "-c",
        "grep -q '^name=new' callee.txt && ! grep -q '^use=old' caller.txt",
    ]);
    let out = verify_combined(
        repo,
        "refs/heads/target",
        &s.base,
        &[s.a.clone(), s.b.clone()],
        Some(&check),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        matches!(&out, CombinedOutcome::VerificationFailed { code: 1, .. }),
        "{out:?}"
    );
    assert_eq!(snapshot(repo), before);
    no_scratch(repo);
    // Each candidate alone is accepted by the same verifier shape: only the
    // combination is wrong.
    let alone = verify_combined(
        repo,
        "refs/heads/target",
        &s.base,
        std::slice::from_ref(&s.a),
        Some(&check),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        matches!(alone, CombinedOutcome::Verified { .. }),
        "{alone:?}"
    );
}

#[test]
fn target_advancing_during_verification_cannot_verify() {
    let s = setup();
    let repo = &s.fixture.0;
    let advance = format!(
        "git -C '{}' update-ref refs/heads/target {}",
        repo.display(),
        s.b
    );
    let out = verify_combined(
        repo,
        "refs/heads/target",
        &s.base,
        std::slice::from_ref(&s.a),
        Some(&argv(&["sh", "-c", &advance])),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        out,
        CombinedOutcome::TargetDrifted {
            expected_base_sha: s.base.clone(),
            actual_base_sha: s.b.clone()
        }
    );
    no_scratch(repo);
    // An already-moved target is refused before any scratch worktree exists.
    let again = verify_combined(
        repo,
        "refs/heads/target",
        &s.base,
        std::slice::from_ref(&s.a),
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(matches!(again, CombinedOutcome::TargetDrifted { .. }));
}

#[test]
fn verified_combination_reports_identity_and_leaves_no_trace() {
    if let Ok(mode) = std::env::var("OMS_COMBINED_CARGO_ENV_MODE") {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::DirBuilderExt;
        let s = setup();
        let repo = &s.fixture.0;
        let cargo_base = commit_file(
            repo,
            "main",
            &s.base,
            "Cargo.toml",
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\n",
        );
        git(repo, &["branch", "-f", "target", &cargo_base]);
        let a = commit_file(repo, "side-a", &cargo_base, "callee.txt", "name=new\n");
        let b = commit_file(repo, "side-b", &cargo_base, "caller.txt", "use=old\n");
        git(repo, &["checkout", "-q", "main"]);
        let before = snapshot(repo);

        let mut dir_0700 = fs::DirBuilder::new();
        dir_0700.recursive(true).mode(0o700);
        let probe_root =
            std::env::temp_dir().join(format!("oms-combined-probe-{}-{mode}", std::process::id()));
        let _ = fs::remove_dir_all(&probe_root);
        dir_0700.create(&probe_root).unwrap();
        let probe = Fixture(probe_root);
        let target_out = probe.0.join("target.txt");
        let cwd_out = probe.0.join("cwd.txt");

        match mode.as_str() {
            "slot" => {
                let caller_target = PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").unwrap());
                let xdg_cache = PathBuf::from(std::env::var_os("XDG_CACHE_HOME").unwrap());
                let repo_hash = format!("{:x}", Sha256::digest(repo.to_string_lossy().as_bytes()));
                let slot_base = xdg_cache
                    .join("oh-my-setting/cargo-slots")
                    .join(&repo_hash[..16]);
                let slot0_dir = slot_base.join("slot-0");
                let expected_slot0 = slot0_dir.join("target");

                let ready = probe.0.join("ready");
                let go = probe.0.join("go");
                let script = format!(
                    "printf '%s' \"${{CARGO_TARGET_DIR-<unset>}}\" > '{}' && pwd -P > '{}' && : > '{}' && i=0; while [ ! -e '{}' ] && [ \"$i\" -lt 500 ]; do sleep 0.01; i=$((i+1)); done",
                    target_out.display(),
                    cwd_out.display(),
                    ready.display(),
                    go.display(),
                );
                let finished = AtomicBool::new(false);
                let mut slot_busy_during_verify = false;
                let out = std::thread::scope(|scope| {
                    let watcher = scope.spawn(|| {
                        while !ready.exists()
                            && !finished.load(std::sync::atomic::Ordering::Acquire)
                        {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        let mut busy = false;
                        if ready.exists() {
                            let mut settings = oms_lock::Settings::from_env().unwrap();
                            settings.cwd = repo.clone();
                            if let Ok(lock) =
                                oms_lock::Lock::for_file(&slot0_dir.join("slot.lock"), settings)
                            {
                                busy = matches!(lock.try_acquire(), Err(oms_lock::Error::Busy));
                            }
                        }
                        let _ = fs::write(&go, b"1");
                        busy
                    });
                    let res = verify_combined(
                        repo,
                        "refs/heads/target",
                        &cargo_base,
                        &[a.clone(), b.clone()],
                        Some(&argv(&["sh", "-c", &script])),
                        &AtomicBool::new(false),
                    );
                    finished.store(true, std::sync::atomic::Ordering::Release);
                    slot_busy_during_verify = watcher.join().unwrap();
                    res
                })
                .unwrap();
                assert!(matches!(out, CombinedOutcome::Verified { .. }), "{out:?}");
                let observed = fs::read_to_string(&target_out).unwrap();
                assert_ne!(
                    Path::new(&observed),
                    caller_target,
                    "verify_combined leaked caller CARGO_TARGET_DIR"
                );
                assert_eq!(Path::new(&observed), expected_slot0);
                assert!(
                    slot_busy_during_verify,
                    "cargo slot lock must be held while verify runs"
                );
                let mut settings = oms_lock::Settings::from_env().unwrap();
                settings.cwd = repo.clone();
                let slot0_lock =
                    oms_lock::Lock::for_file(&slot0_dir.join("slot.lock"), settings.clone())
                        .unwrap();
                let released = slot0_lock
                    .try_acquire()
                    .expect("cargo slot lock must be released after verify ends");
                drop(released);

                let mut held = Vec::new();
                for idx in 0..12 {
                    let dir = slot_base.join(format!("slot-{idx}"));
                    dir_0700.create(&dir).unwrap();
                    let lock =
                        oms_lock::Lock::for_file(&dir.join("slot.lock"), settings.clone()).unwrap();
                    held.push(lock.try_acquire().unwrap());
                }
                let busy_script = format!(
                    "printf '%s' \"${{CARGO_TARGET_DIR-<unset>}}\" > '{}' && pwd -P > '{}'",
                    target_out.display(),
                    cwd_out.display(),
                );
                let busy_out = verify_combined(
                    repo,
                    "refs/heads/target",
                    &cargo_base,
                    &[a, b],
                    Some(&argv(&["sh", "-c", &busy_script])),
                    &AtomicBool::new(false),
                )
                .unwrap();
                drop(held);
                assert!(
                    matches!(busy_out, CombinedOutcome::Verified { .. }),
                    "{busy_out:?}"
                );
                let busy_target = fs::read_to_string(&target_out).unwrap();
                let busy_cwd = fs::read_to_string(&cwd_out).unwrap();
                assert_eq!(
                    Path::new(&busy_target),
                    Path::new(busy_cwd.trim()).join("target")
                );
            }
            "optout-caller-target" => {
                let caller_target = PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").unwrap());
                let script = format!(
                    "printf '%s' \"${{CARGO_TARGET_DIR-<unset>}}\" > '{}' && pwd -P > '{}'",
                    target_out.display(),
                    cwd_out.display(),
                );
                let out = verify_combined(
                    repo,
                    "refs/heads/target",
                    &cargo_base,
                    &[a, b],
                    Some(&argv(&["sh", "-c", &script])),
                    &AtomicBool::new(false),
                )
                .unwrap();
                assert!(matches!(out, CombinedOutcome::Verified { .. }), "{out:?}");
                let observed = fs::read_to_string(&target_out).unwrap();
                let observed_cwd = fs::read_to_string(&cwd_out).unwrap();
                assert_ne!(Path::new(&observed), caller_target);
                assert_eq!(
                    Path::new(&observed),
                    Path::new(observed_cwd.trim()).join("target")
                );
            }
            "optout-unset" => {
                let script = format!(
                    "printf '%s' \"${{CARGO_TARGET_DIR-<unset>}}\" > '{}'",
                    target_out.display(),
                );
                let out = verify_combined(
                    repo,
                    "refs/heads/target",
                    &cargo_base,
                    &[a, b],
                    Some(&argv(&["sh", "-c", &script])),
                    &AtomicBool::new(false),
                )
                .unwrap();
                assert!(matches!(out, CombinedOutcome::Verified { .. }), "{out:?}");
                assert_eq!(fs::read_to_string(&target_out).unwrap(), "<unset>");
            }
            other => panic!("unexpected mode: {other}"),
        }
        assert_eq!(snapshot(repo), before);
        no_scratch(repo);
        return;
    }

    let s = setup();
    let repo = &s.fixture.0;
    let before = snapshot(repo);
    let out = verify_combined(
        repo,
        "refs/heads/target",
        &s.base,
        &[s.a.clone(), s.b.clone()],
        Some(&argv(&[
            "sh",
            "-c",
            "test -f callee.txt && test -f caller.txt",
        ])),
        &AtomicBool::new(false),
    )
    .unwrap();
    let CombinedOutcome::Verified {
        combined_sha,
        tree_sha,
    } = out
    else {
        panic!("{out:?}");
    };
    assert_eq!(
        git(repo, &["rev-parse", &format!("{combined_sha}^{{tree}}")]),
        tree_sha
    );
    assert_eq!(
        git(repo, &["rev-list", "--parents", "-n1", &combined_sha])
            .split(' ')
            .count(),
        3,
        "a --no-ff merge commit joins the last candidate"
    );
    assert_eq!(snapshot(repo), before);
    no_scratch(repo);

    use std::os::unix::fs::DirBuilderExt;
    let mut dir_0700 = fs::DirBuilder::new();
    dir_0700.recursive(true).mode(0o700);
    let env_root = std::env::temp_dir().join(format!(
        "oms-combined-env-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    dir_0700.create(&env_root).unwrap();
    let env_fixture = Fixture(fs::canonicalize(&env_root).unwrap());
    let home = env_fixture.0.join("home");
    let xdg_cache = env_fixture.0.join("cache");
    let lock_dir = env_fixture.0.join("locks");
    let caller_target = env_fixture.0.join("caller-target");
    dir_0700.create(&home).unwrap();
    dir_0700.create(&xdg_cache).unwrap();
    dir_0700.create(&lock_dir).unwrap();
    for mode in ["slot", "optout-caller-target", "optout-unset"] {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "verified_combination_reports_identity_and_leaves_no_trace",
            "--nocapture",
        ])
        .env("OMS_COMBINED_CARGO_ENV_MODE", mode)
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("OMS_LOCK_DIR", &lock_dir)
        .env_remove("OMS_DELEGATE_ISOLATED_BUILD")
        .env_remove("OMS_CARGO_SLOT_CACHE");
        match mode {
            "slot" => {
                cmd.env("CARGO_TARGET_DIR", &caller_target);
            }
            "optout-caller-target" => {
                cmd.env("CARGO_TARGET_DIR", &caller_target)
                    .env("OMS_CARGO_SLOT_CACHE", "0");
            }
            "optout-unset" => {
                cmd.env_remove("CARGO_TARGET_DIR")
                    .env("OMS_DELEGATE_ISOLATED_BUILD", "1");
            }
            _ => unreachable!(),
        }
        let child = cmd.output().unwrap();
        assert!(
            child.status.success(),
            "{mode}:\n{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(
            String::from_utf8_lossy(&child.stdout).contains("1 passed;"),
            "{mode}: child ran no test"
        );
    }
}

#[test]
fn conflicts_and_invalid_inputs_are_refused_without_side_effects() {
    let s = setup();
    let repo = &s.fixture.0;
    let c1 = commit_file(repo, "side-c", &s.base, "callee.txt", "name=other\n");
    git(repo, &["checkout", "-q", "main"]);
    let stop = AtomicBool::new(false);
    let out = verify_combined(
        repo,
        "refs/heads/target",
        &s.base,
        &[s.a.clone(), c1.clone()],
        None,
        &stop,
    )
    .unwrap();
    assert!(
        matches!(&out, CombinedOutcome::MergeConflict { commit, .. } if *commit == c1),
        "{out:?}"
    );
    no_scratch(repo);
    for (base_ref, base, commits) in [
        ("refs/heads/target", s.base.as_str(), vec![]),
        ("--detach", s.base.as_str(), vec![s.a.clone()]),
        ("refs/heads/target", "main", vec![s.a.clone()]),
        ("refs/heads/target", s.base.as_str(), vec!["-x".to_owned()]),
    ] {
        assert!(verify_combined(repo, base_ref, base, &commits, None, &stop).is_err());
    }
    let interrupted = AtomicBool::new(true);
    let err = verify_combined(
        repo,
        "refs/heads/target",
        &s.base,
        &[s.a],
        None,
        &interrupted,
    )
    .unwrap_err();
    assert_eq!(err.code, 130);
    no_scratch(repo);
}

#[test]
fn cli_verify_combined_returns_structured_outcomes() {
    let s = setup();
    let repo = &s.fixture.0;
    let stop = AtomicBool::new(false);
    let svc = oms_command_autonomy::LocalServices::new(&stop);
    let call = |cmd: &str| {
        let candidates = format!("{},{}", s.a, s.b);
        let args = argv(&[
            "--repo",
            repo.to_str().unwrap(),
            "--verify-combined",
            "--base-ref",
            "refs/heads/target",
            "--base-sha",
            &s.base,
            "--candidates",
            &candidates,
            "--verify-cmd",
            cmd,
        ]);
        oms_command_autonomy::run_with_services("plan-run", &args, repo, &stop, &svc)
    };
    let (code, out, err) = call("test -f callee.txt && test -f caller.txt");
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let ok: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(ok["status"], "verified");

    let (code, out, _) = call("grep -q '^name=new' callee.txt && ! grep -q '^use=old' caller.txt");
    assert_eq!(code, 1);
    let fail: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(fail["status"], "verification_failed");

    let drift = format!(
        "git -C '{}' update-ref refs/heads/target {}",
        repo.display(),
        s.b
    );
    let (code, out, _) = call(&drift);
    assert_eq!(code, 3);
    let drifted: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(drifted["status"], "target_drifted");
    no_scratch(repo);
}
