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
    let dir = std::env::temp_dir().join(format!(
        "oms-combined-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
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
