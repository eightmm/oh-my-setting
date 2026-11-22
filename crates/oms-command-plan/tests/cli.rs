#![forbid(unsafe_code)]

use oms_command_plan::services::{GroupMembership, GroupResolver};
use oms_command_plan::{Services, UnavailableServices, owns, run, run_with_io, run_with_services};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let n = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("oms-plan-cli-{}-{seq}-{n}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        Self(fs::canonicalize(d).unwrap())
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `Context::from_environment` reads OMS_* variables; rerun in a child with
/// them removed so a harness/worker environment cannot change the outcome.
fn isolated(name: &str) -> bool {
    if std::env::vars_os().all(|(k, _)| !k.to_string_lossy().starts_with("OMS_")) {
        return false;
    }
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", name, "--test-threads=1", "--nocapture"]);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("OMS_") {
            cmd.env_remove(k);
        }
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    print!("{}", String::from_utf8_lossy(&out.stdout));
    true
}

fn git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

fn repo(tmp: &Tmp) -> PathBuf {
    repo_at(tmp, "repo")
}

fn repo_at(tmp: &Tmp, name: &str) -> PathBuf {
    let r = tmp.0.join(name);
    fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q"]);
    fs::write(r.join("a.txt"), "x").unwrap();
    git(&r, &["add", "."]);
    git(&r, &["commit", "-qm", "init"]);
    r
}

fn args(a: &[&str]) -> Vec<OsString> {
    a.iter().map(OsString::from).collect()
}

/// Account proof for group-writable fixtures (umask 002 hosts): the invoking
/// user is the sole member of every group.
struct Host;
impl Services for Host {
    fn group_membership_resolver(&self) -> Option<GroupResolver> {
        fn resolve(gid: u32) -> oms_command_plan::Result<GroupMembership> {
            let uid = rustix::process::getuid().as_raw();
            Ok(GroupMembership {
                gid,
                explicit_member_uids: vec![uid],
                primary_member_uids: vec![uid],
            })
        }
        Some(resolve)
    }
}

fn go(cwd: &Path, a: &[&str]) -> (i32, String, String) {
    go_with(&Host, cwd, a)
}

fn go_with(services: &dyn Services, cwd: &Path, a: &[&str]) -> (i32, String, String) {
    let stop = AtomicBool::new(false);
    let (c, o, e) = run_with_services(&args(a), cwd, &stop, services);
    (
        c,
        String::from_utf8(o).unwrap(),
        String::from_utf8(e).unwrap(),
    )
}

#[test]
fn owns_only_agent_plan() {
    assert!(owns("agent-plan", &[]));
    assert!(!owns("agent-events", &args(&["init"])));
    assert!(!owns("", &[]));
}

#[test]
fn help_and_parse_errors() {
    if isolated("help_and_parse_errors") {
        return;
    }
    let tmp = Tmp::new();
    for a in [&["--help"][..], &["-h"], &["list", "--help"]] {
        let (c, o, e) = go(&tmp.0, a);
        assert_eq!((c, e.as_str()), (0, ""), "{a:?}");
        assert!(o.starts_with("Usage: agent-plan.sh"), "{a:?}");
    }
    // `--goal` consumes `-h` as its operand, exactly like the shell parser.
    assert_eq!(go(&tmp.0, &["--goal", "-h"]).0, 2);
    // Options only (or nothing at all): usage on stderr, exit 2, empty stdout.
    for a in [&[][..], &["--json"], &["--repo", "x", "--check"]] {
        let (c, o, e) = go(&tmp.0, a);
        assert_eq!((c, o.as_str()), (2, ""), "{a:?}");
        assert!(e.starts_with("Usage:"), "{a:?}: {e}");
    }
    for a in [
        &["--bogus", "-h"][..],
        &["nonsense"],
        &["nonsense", "--help"],
        &["-x", "--help"],
        &["list", "show", "--help"],
        &["--priority", "abc", "--help"],
        &["--ttl", "abc", "--help"],
        &["--ttl", "", "--help"],
        &["--ttl", "99999999999999999999999", "--help"],
        &["--limit", "0", "--help"],
        &["--limit", "-1", "--help"],
        &["--max-tasks", "0", "--help"],
        &["--max-tasks", "13", "--help"],
        &["--landed-commit", "", "--help"],
        &["--depends", "a", "--depends", "b", "--help"],
        &["--allowed", "a", "--allowed", "b", "--help"],
        &["--forbidden", "a", "--forbidden", "b", "--help"],
        &["list", "show"],
        &["--repo"],
    ] {
        let (c, o, e) = go(&tmp.0, a);
        assert_eq!((c, o.as_str()), (2, ""), "{a:?}");
        assert!(e.starts_with("error: "), "{a:?}: {e}");
    }
}

#[test]
fn interrupted_before_work_fails_closed() {
    if isolated("interrupted_before_work_fails_closed") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let stop = AtomicBool::new(true);
    let (c, o, e) = run(
        &args(&["--repo", r.to_str().unwrap(), "list"]),
        &tmp.0,
        &stop,
    );
    assert_eq!((c, o.is_empty()), (2, true));
    assert!(String::from_utf8(e).unwrap().contains("interrupted"));
    // Interruption wins over path/Git work: a missing --repo is not reached.
    let (c, o, e) = run(&args(&["--repo", "missing", "list"]), &tmp.0, &stop);
    assert_eq!((c, o.is_empty()), (2, true));
    assert_eq!(String::from_utf8(e).unwrap(), "error: interrupted\n");
}

#[test]
fn lifecycle_from_unrelated_cwd() {
    if isolated("lifecycle_from_unrelated_cwd") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let other = tmp.0.join("elsewhere");
    fs::create_dir_all(&other).unwrap();
    let rs = r.to_str().unwrap();
    let base = |rest: &[&str]| {
        let mut v = vec!["--repo", rs];
        v.extend_from_slice(rest);
        go(&other, &v)
    };
    let (c, _, e) = base(&["init", "--goal", "ship it"]);
    assert_eq!(c, 0, "{e}");
    assert!(r.join(".oms/plan").exists());
    assert!(
        !other.join(".oms").exists(),
        "state must follow --repo, not cwd"
    );
    let (c, _, e) = base(&["add", "--id", "t1", "--title", "first", "--verify", "true"]);
    assert_eq!(c, 0, "{e}");
    let (c, o, e) = base(&["show", "--id", "t1"]);
    assert_eq!(c, 0, "{e}");
    assert!(o.contains("\"t1\""), "{o}");
    let (c, o, _) = base(&["list"]);
    assert_eq!(c, 0);
    assert!(o.contains("t1"));
    let (c, o, _) = base(&["ready"]);
    assert_eq!(c, 0);
    assert!(o.contains("t1"));
    let (c, o, e) = base(&["show", "--id", "missing"]);
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.starts_with("error: "));
    let (c, o, e) = base(&["retire", "--check"]);
    assert_eq!(c, 0, "{e}");
    assert!(!o.is_empty());
    assert!(!other.join(".oms").exists());
}

#[test]
fn relative_proposal_and_file_resolve_against_cwd_arg() {
    if isolated("relative_proposal_and_file_resolve_against_cwd_arg") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    // Relative to the supplied cwd, not the process cwd: a missing proposal fails
    // with a plain refusal rather than reading anything from the test process dir.
    let (c, o, e) = go(
        &r,
        &["--repo", rs, "apply-proposal", "--proposal", "nope.json"],
    );
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.starts_with("error: "), "{e}");
    // --file is cwd-relative: it lands under the supplied cwd, never the process cwd.
    let work = tmp.0.join("work");
    fs::create_dir_all(work.join("rel")).unwrap();
    let (c, _, e) = go(
        &work,
        &[
            "--repo",
            rs,
            "--file",
            "rel/plan.json",
            "init",
            "--goal",
            "g",
        ],
    );
    assert_eq!(c, 0, "{e}");
    assert!(work.join("rel/plan.json").exists());
}

#[test]
fn unavailable_services_fail_closed_with_io_split() {
    if isolated("unavailable_services_fail_closed_with_io_split") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(
        go(
            &tmp.0,
            &["--repo", rs, "init", "--goal", "g", "--accept", "true"]
        )
        .0,
        0
    );
    let lint = |verify: &str| {
        let a = args(&[
            "--repo",
            rs,
            "lint-verify",
            "--verify",
            verify,
            "--allowed",
            "a.rs",
        ]);
        let stop = AtomicBool::new(false);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let c = run_with_io(&a, &tmp.0, &stop, &UnavailableServices, &mut out, &mut err);
        let (c2, o2, e2) = run_with_services(&a, &tmp.0, &stop, &UnavailableServices);
        assert_eq!((c, &out, &err), (c2, &o2, &e2));
        (
            c,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    // A floor hit is typed output on stdout with empty stderr.
    let (c, o, e) = lint("cat a.rs");
    assert_eq!((c, e.as_str()), (2, ""));
    assert!(o.starts_with("floor_incompatible_verifier: a.rs"), "{o}");
    let (c, o, e) = lint("true");
    assert_eq!(
        (c, o.starts_with("lint-verify: ok"), e.as_str()),
        (0, true, "")
    );
    // Service-backed actions refuse: exit 2, empty stdout, `error:` on stderr.
    let (c, o, e) = go_with(&UnavailableServices, &tmp.0, &["--repo", rs, "accept"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // Provider normalisation is a native service: unknown names are refused on claim.
    assert_eq!(
        go(&tmp.0, &["--repo", rs, "add", "--id", "t1", "--title", "x"]).0,
        0
    );
    let (c, o, e) = go_with(
        &UnavailableServices,
        &tmp.0,
        &["--repo", rs, "claim", "--id", "t1", "--provider", "bogus"],
    );
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // retro is read-only and works on an initialised plan; an empty repo refuses.
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "retro"]);
    assert_eq!(c, 0, "{e}");
    assert!(!o.is_empty());
    let empty = repo_at(&tmp, "empty");
    let (c, o, e) = go(&tmp.0, &["--repo", empty.to_str().unwrap(), "retro"]);
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.contains("no readable plan"), "{e}");
}

#[test]
fn claim_block_from_unrelated_cwd() {
    if isolated("claim_block_from_unrelated_cwd") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    let (c, _, e) = go(
        &tmp.0,
        &[
            "--repo", rs, "add", "--id", "t1", "--title", "x", "--verify", "true",
        ],
    );
    assert_eq!(c, 0, "{e}");
    for step in [
        &["claim", "--id", "t1", "--provider", "codex"][..],
        &["block", "--id", "t1", "--reason", "waiting"],
        &["show", "--id", "t1"],
    ] {
        let mut v = vec!["--repo", rs];
        v.extend_from_slice(step);
        let (c, _, e) = go(&tmp.0, &v);
        assert_eq!(c, 0, "{step:?}: {e}");
    }
    let (_, o, _) = go(&tmp.0, &["--repo", rs, "show", "--id", "t1"]);
    assert!(o.contains("blocked"), "{o}");
    // A task already blocked cannot be claimed again: refused, state unchanged.
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "claim", "--id", "t1"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
}

const OWNER: &str = "owner_0123456789abcdef0123456789abcdef";

#[test]
fn repo_subdirectory_resolves_to_worktree_root() {
    if isolated("repo_subdirectory_resolves_to_worktree_root") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let sub = r.join("src");
    fs::create_dir_all(&sub).unwrap();
    let subs = sub.to_str().unwrap();
    // Without --repo the cwd itself is the default and still normalises.
    assert_eq!(go(&sub, &["init", "--goal", "g"]).0, 0);
    assert!(r.join(".oms/plan").exists() && !sub.join(".oms").exists());
    let (c, _, e) = go(
        &tmp.0,
        &["--repo", subs, "add", "--id", "t1", "--title", "x"],
    );
    assert_eq!(c, 0, "{e}");
    for step in [&["list"][..], &["ready"], &["show", "--id", "t1"]] {
        let (c, o, e) = go(&sub, step);
        assert_eq!(c, 0, "{step:?}: {e}");
        assert!(o.contains("t1"), "{step:?}: {o}");
        let mut v = vec!["--repo", subs];
        v.extend_from_slice(step);
        let (c, o, e) = go(&tmp.0, &v);
        assert_eq!(c, 0, "{v:?}: {e}");
        assert!(o.contains("t1"), "{v:?}: {o}");
    }
    assert!(!sub.join(".oms").exists());
    // Non-git directories resolve to themselves; missing ones are refused.
    let plain = tmp.0.join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert_eq!(go(&plain, &["init", "--goal", "g"]).0, 0);
    assert!(plain.join(".oms/plan").exists());
    let (c, o, e) = go(&tmp.0, &["--repo", "missing", "list"]);
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.starts_with("error: "));
}

/// Child half of `state_repo_env_defaults_the_repository`; it needs
/// OMS_STATE_REPO in its environment, which the parent test supplies.
#[test]
#[ignore = "driven by state_repo_env_defaults_the_repository"]
fn state_env_child() {
    let tmp = PathBuf::from(std::env::var_os("CLI_TEST_TMP").unwrap());
    let work = tmp.join("work");
    let (c, _, e) = go(&work, &["init", "--goal", "g"]);
    assert_eq!(c, 0, "{e}");
    let (c, o, e) = go(&work, &["add", "--id", "t1", "--title", "x"]);
    assert_eq!(c, 0, "{e}: {o}");
    let (c, o, e) = go(&work, &["list"]);
    assert_eq!(c, 0, "{e}");
    assert!(o.contains("t1"));
    assert!(tmp.join("repo/.oms/plan").exists());
    assert!(!work.join(".oms").exists());
    assert!(!tmp.join("repo/src/.oms").exists());
}

#[test]
fn state_repo_env_defaults_the_repository() {
    if isolated("state_repo_env_defaults_the_repository") {
        return;
    }
    // Absolute, and relative to the supplied cwd (never the process cwd).
    for value in ["ABS", "../repo", "ABSSUB", "../repo/src"] {
        let tmp = Tmp::new();
        let r = repo(&tmp);
        fs::create_dir_all(tmp.0.join("work")).unwrap();
        fs::create_dir_all(r.join("src")).unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "state_env_child",
            "--ignored",
            "--test-threads=1",
        ]);
        cmd.env("CLI_TEST_TMP", &tmp.0);
        cmd.env(
            "OMS_STATE_REPO",
            match value {
                "ABS" => r.to_str().unwrap().to_owned(),
                "ABSSUB" => r.join("src").to_str().unwrap().to_owned(),
                v => v.to_owned(),
            },
        );
        let out = cmd.output().unwrap();
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "{value}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn services_state_root_hint_places_shared_plan() {
    if isolated("services_state_root_hint_places_shared_plan") {
        return;
    }
    struct Scratch(PathBuf);
    impl Services for Scratch {
        fn group_membership_resolver(&self) -> Option<GroupResolver> {
            Host.group_membership_resolver()
        }
        fn state_root(&self, _: &Path) -> oms_command_plan::Result<Option<PathBuf>> {
            Ok(Some(self.0.clone()))
        }
    }
    let tmp = Tmp::new();
    let main = repo_at(&tmp, "main");
    let scratch = repo_at(&tmp, "scratch");
    let svc = Scratch(main.clone());
    let sc = scratch.to_str().unwrap();
    let (c, _, e) = go_with(&svc, &tmp.0, &["--repo", sc, "init", "--goal", "g"]);
    assert_eq!(c, 0, "{e}");
    assert!(main.join(".oms/plan").exists() && !scratch.join(".oms").exists());
}

#[test]
fn symlinked_cwd_markers_dir_and_file() {
    if isolated("symlinked_cwd_markers_dir_and_file") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    let link = tmp.0.join("link");
    std::os::unix::fs::symlink(&r, &link).unwrap();
    for dir in [
        ".oms/delegations",
        "./.oms/delegations",
        "../repo/.oms/delegations",
    ] {
        let (c, o, e) = go(
            &link,
            &[
                "--repo",
                rs,
                "recover-owner",
                "--owner-id",
                OWNER,
                "--markers-dir",
                dir,
            ],
        );
        assert_eq!(
            (c, o.as_str()),
            (0, "plan: recovered (none)\n"),
            "{dir}: {e}"
        );
    }
    let (c, o, e) = go(
        &link,
        &[
            "--repo",
            rs,
            "recover-owner",
            "--owner-id",
            OWNER,
            "--markers-dir",
            "other",
        ],
    );
    assert_eq!((c, o.as_str()), (2, ""));
    assert!(e.contains("repo-local"), "{e}");
    // --file under a symlinked cwd whose directory does not exist yet.
    let file = r.join("newdir/plan.json");
    let (c, _, e) = go(&link, &["--repo", rs, "--file", "newdir/plan.json", "list"]);
    assert_eq!(c, 0, "{e}");
    let (c, o, e) = go(
        &link,
        &[
            "--repo",
            rs,
            "--file",
            "newdir/plan.json",
            "init",
            "--goal",
            "g",
        ],
    );
    assert_eq!(c, 0, "{e}");
    assert!(o.contains(file.to_str().unwrap()), "{o}");
    assert!(file.exists());
}

/// Child half of the ambient-GIT_* test: it runs with GIT_DIR/GIT_WORK_TREE
/// pointing at another repository.
#[test]
#[ignore = "driven by ambient_git_environment_cannot_hijack_a_plain_directory"]
fn ambient_git_child() {
    let tmp = PathBuf::from(std::env::var_os("CLI_TEST_TMP").unwrap());
    let plain = tmp.join("plain");
    let (c, _, e) = go(
        &tmp,
        &["--repo", plain.to_str().unwrap(), "init", "--goal", "g"],
    );
    assert_eq!(c, 0, "{e}");
    assert!(plain.join(".oms/plan").exists());
    assert!(!tmp.join("other/.oms").exists());
}

#[test]
fn ambient_git_environment_cannot_hijack_a_plain_directory() {
    if isolated("ambient_git_environment_cannot_hijack_a_plain_directory") {
        return;
    }
    let tmp = Tmp::new();
    let other = repo_at(&tmp, "other");
    fs::create_dir_all(tmp.0.join("plain")).unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ambient_git_child",
            "--ignored",
            "--test-threads=1",
        ])
        .env("CLI_TEST_TMP", &tmp.0)
        .env("GIT_DIR", other.join(".git"))
        .env("GIT_WORK_TREE", &other)
        .output()
        .unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn symlinked_leaves_and_plan_directory_fail_closed() {
    if isolated("symlinked_leaves_and_plan_directory_fail_closed") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    let real = tmp.0.join("real.json");
    fs::copy(r.join(".oms/plan/tasks.json"), &real).unwrap();
    // --file leaf is a symlink to a valid plan: never dereferenced.
    let leaf = tmp.0.join("leaf.json");
    std::os::unix::fs::symlink(&real, &leaf).unwrap();
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "--file", "leaf.json", "list"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // --proposal leaf is a symlink.
    let (c, o, e) = go(
        &tmp.0,
        &["--repo", rs, "apply-proposal", "--proposal", "leaf.json"],
    );
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    // The default plan path: tasks.json as a symlink.
    let tasks = r.join(".oms/plan/tasks.json");
    fs::remove_file(&tasks).unwrap();
    std::os::unix::fs::symlink(&real, &tasks).unwrap();
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "list"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    // `.oms/plan` itself is a symlink.
    fs::remove_file(&tasks).unwrap();
    let moved = tmp.0.join("moved-plan");
    fs::rename(r.join(".oms/plan"), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, r.join(".oms/plan")).unwrap();
    let (c, o, e) = go(&tmp.0, &["--repo", rs, "list"]);
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // A symlinked markers directory is refused rather than dereferenced.
    fs::remove_file(r.join(".oms/plan")).unwrap();
    fs::rename(&moved, r.join(".oms/plan")).unwrap();
    fs::create_dir_all(tmp.0.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(tmp.0.join("elsewhere"), r.join(".oms/delegations")).unwrap();
    let (c, o, e) = go(
        &tmp.0,
        &[
            "--repo",
            rs,
            "recover-owner",
            "--owner-id",
            OWNER,
            "--markers-dir",
            r.join(".oms/delegations").to_str().unwrap(),
        ],
    );
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
}

#[test]
fn parent_traversal_stops_at_the_root() {
    if isolated("parent_traversal_stops_at_the_root") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    // Far more `..` than the cwd has components; the process cwd differs.
    let up = "../".repeat(64);
    let rel = format!("{up}{}", r.strip_prefix("/").unwrap().display());
    let (c, _, e) = go(&tmp.0, &["--repo", &rel, "init", "--goal", "g"]);
    assert_eq!(c, 0, "{e}");
    assert!(r.join(".oms/plan").exists());
    let shallow = Path::new("/");
    let (c, _, e) = go(
        shallow,
        &[
            "--repo",
            r.strip_prefix("/").unwrap().to_str().unwrap(),
            "list",
        ],
    );
    assert_eq!(c, 0, "{e}");
}

#[test]
fn context_and_execute_agree_when_cwd_is_not_the_process_cwd() {
    if isolated("context_and_execute_agree_when_cwd_is_not_the_process_cwd") {
        return;
    }
    use oms_command_plan::{Context, Plan, Request};
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    assert_eq!(go(&tmp.0, &["--repo", rs, "init", "--goal", "g"]).0, 0);
    let target = tmp.0.join("target");
    fs::create_dir_all(r.join("src")).unwrap();
    fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, tmp.0.join("link")).unwrap();
    let stop = AtomicBool::new(false);
    let req = Request::parse(&args(&[
        "--repo",
        "repo/src/..",
        "--file",
        "link/a/b/plan.json",
        "list",
    ]))
    .unwrap();
    let ctx = Context::from_environment_at(&req, &tmp.0, &tmp.0, None).unwrap();
    assert_eq!(ctx.cwd, tmp.0);
    assert_eq!(ctx.execution_repo, r);
    assert_eq!(ctx.plan_file, target.join("a/b/plan.json"));
    let resp = Plan::open(ctx, &Host, &stop)
        .unwrap()
        .execute(&req)
        .unwrap();
    assert_eq!(resp.code, 0);
    // A different spelling of another repository is refused, not trusted.
    let other = repo_at(&tmp, "other");
    let mut bad = req.clone();
    bad.repo = Some(other);
    let ctx = Context::from_environment_at(&req, &tmp.0, &tmp.0, None).unwrap();
    let err = Plan::open(ctx, &Host, &stop)
        .unwrap()
        .execute(&bad)
        .unwrap_err();
    assert!(
        err.message.contains("differs from context"),
        "{}",
        err.message
    );
    // A symlink followed by `..` steps out of the physical target, not the
    // spelling: /A/link -> /B/sub, so /A/link/.. is /B.
    let a = tmp.0.join("A");
    let b = repo_at(&tmp, "B");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(b.join("sub")).unwrap();
    std::os::unix::fs::symlink(b.join("sub"), a.join("link")).unwrap();
    let a_s = a.to_str().unwrap();
    let via = format!("{a_s}/link/..");
    let req = Request::parse(&args(&[
        "--repo",
        &via,
        "--file",
        &format!("{a_s}/link/../plan.json"),
        "list",
    ]))
    .unwrap();
    let ctx = Context::from_environment_at(&req, &tmp.0, &tmp.0, None).unwrap();
    assert_eq!(ctx.execution_repo, b);
    assert_eq!(ctx.plan_file, b.join("plan.json"));
    assert_eq!(
        Plan::open(ctx, &Host, &stop)
            .unwrap()
            .execute(&req)
            .unwrap()
            .code,
        0
    );
    fs::create_dir_all(b.join("newdir")).unwrap();
    for file in ["A/link/../plan.json", "A/link/../newdir/../plan.json"] {
        let req = Request::parse(&args(&["--repo", "A/link/..", "--file", file, "list"])).unwrap();
        let ctx = Context::from_environment_at(&req, &tmp.0, &tmp.0, None).unwrap();
        assert_eq!(
            (ctx.execution_repo.as_path(), ctx.plan_file.as_path()),
            (b.as_path(), b.join("plan.json").as_path()),
            "{file}"
        );
        assert_eq!(
            Plan::open(ctx, &Host, &stop)
                .unwrap()
                .execute(&req)
                .unwrap()
                .code,
            0
        );
    }
    // End to end: state lands in B, never in A.
    let (c, _, e) = go(&tmp.0, &["--repo", &via, "init", "--goal", "g"]);
    assert_eq!(c, 0, "{e}");
    assert!(b.join(".oms/plan").exists() && !a.join(".oms").exists());
    let (c, o, e) = go(
        &tmp.0,
        &[
            "--repo",
            &via,
            "recover-owner",
            "--owner-id",
            OWNER,
            "--markers-dir",
            "A/link/../.oms/delegations",
        ],
    );
    assert_eq!((c, o.as_str()), (0, "plan: recovered (none)\n"), "{e}");
    // A symlinked leaf reached through `link/..` is still not followed.
    fs::copy(b.join(".oms/plan/tasks.json"), tmp.0.join("real.json")).unwrap();
    std::os::unix::fs::symlink(tmp.0.join("real.json"), b.join("leaf.json")).unwrap();
    let (c, o, e) = go(
        &tmp.0,
        &["--repo", &via, "--file", "A/link/../leaf.json", "list"],
    );
    assert_eq!((c, o.as_str()), (2, ""), "{e}");
    assert!(e.starts_with("error: "), "{e}");
    // `..` still clamps at the root: `/..` stays `/`.
    let (c, _, e) = go(
        Path::new("/"),
        &["--repo", &format!("../../..{}", r.display()), "list"],
    );
    assert_eq!(c, 0, "{e}");
}

#[test]
fn invalid_parent_traversal_and_relative_cwd() {
    if isolated("invalid_parent_traversal_and_relative_cwd") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    fs::write(r.join("file"), "x").unwrap();
    std::os::unix::fs::symlink(r.join("absent"), r.join("dangling")).unwrap();
    for part in ["file", "dangling", "missing"] {
        let path = format!("{part}/..");
        assert_ne!(go(&r, &["--repo", &path, "list"]).0, 0, "{part}");
    }
    let process = std::env::current_dir().unwrap();
    assert_eq!(
        go(
            Path::new("."),
            &["--repo", process.to_str().unwrap(), "list"]
        )
        .0,
        0
    );
    assert_ne!(go(Path::new("missing-cwd-for-plan"), &["list"]).0, 0);
}

#[test]
fn merged_dependencies_scheduling_and_late_results() {
    if isolated("merged_dependencies_scheduling_and_late_results") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    let run = |rest: &[&str]| {
        let mut command = vec!["--repo", rs];
        command.extend_from_slice(rest);
        go(&tmp.0, &command)
    };
    assert_eq!(run(&["init", "--goal", "g"]).0, 0);
    let (code, _, err) = run(&[
        "add",
        "--id",
        "bad",
        "--title",
        "bad",
        "--depends",
        "missing",
    ]);
    assert_eq!(code, 2);
    assert!(err.contains("missing"), "{err}");
    let (code, _, err) = run(&["add", "--id", "self", "--title", "bad", "--depends", "self"]);
    assert_eq!(code, 2);
    assert!(err.contains("cycle"), "{err}");
    for (id, priority) in [("a", "-1"), ("b", "+2")] {
        let (code, _, err) = run(&[
            "add",
            "--id",
            id,
            "--title",
            id,
            "--allowed",
            id,
            "--priority",
            priority,
            "--resources",
            "gpu,gpu",
        ]);
        assert_eq!(code, 0, "{err}");
    }
    let (code, out, err) = run(&["ready"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "b\na\n");
    let (code, out, err) = run(&["ready", "--json", "--max-writers", "0"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"capacity_full\": true"), "{out}");
    assert_eq!(run(&["next", "--max-review-backlog", "0"]).0, 3);
    let (code, _, err) = run(&["claim", "--id", "a", "--provider", "codex"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = run(&["ready"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.is_empty(), "shared resource should block b: {out}");
    let (code, _, err) = run(&["review", "--id", "a"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = run(&["ready", "--json", "--max-review-backlog", "1"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"backpressure\": true"), "{out}");
    let (code, _, err) = run(&["next", "--max-review-backlog", "1"]);
    assert_eq!(code, 3);
    assert!(err.contains("review backlog backpressure"), "{err}");
    let before = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let (code, _, err) = run(&[
        "review",
        "--id",
        "a",
        "--lease-id",
        "old",
        "--artifact",
        "result",
    ]);
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("stale lease"), "{err}");
    assert_eq!(before, fs::read(r.join(".oms/plan/tasks.json")).unwrap());
    let ledger = fs::read_to_string(r.join(".oms/plan/late-results.jsonl")).unwrap();
    assert!(ledger.contains("\"supplied_lease_id\":\"old\""), "{ledger}");
}

#[test]
fn panel_acceptance_checks_canonical_claimed_and_running_expiry() {
    if isolated("panel_acceptance_checks_canonical_claimed_and_running_expiry") {
        return;
    }
    use oms_command_plan::{Action, Context, Plan, Request, json};
    let tmp = Tmp::new();
    let repo = repo(&tmp);
    assert_eq!(go(&repo, &["init", "--goal", "g"]).0, 0);
    assert_eq!(go(&repo, &["add", "--id", "a", "--title", "a"]).0, 0);
    let stop = AtomicBool::new(false);
    let claim = Request::new(Action::Claim)
        .with("id", "a")
        .with("provider", "codex")
        .with("ttl", "1");
    let mut context = Context::from_environment_at(&claim, &repo, &repo, None).unwrap();
    context.panel_main_attempt = format!("att_{}", "1".repeat(32));
    context.participant = "main-fixture".into();
    context.ts = "2026-11-21T00:00:00Z".into();
    let active = Plan::open(context.clone(), &Host, &stop).unwrap();
    active.execute(&claim).unwrap();
    context.ts = "2026-11-21T00:00:01Z".into();
    let expired = Plan::open(context, &Host, &stop).unwrap();
    for state in ["claimed", "running"] {
        if state == "running" {
            active
                .execute(&Request::new(Action::Start).with("id", "a"))
                .unwrap();
        }
        let snapshot = active.load().unwrap();
        let mut expected = snapshot.task("a").unwrap().clone();
        assert_eq!(expected.text("state"), state);
        expected
            .set("plan_id", snapshot.plan.get("plan_id").clone())
            .unwrap();
        active.verify_panel_review_lease(&expected).unwrap();
        assert!(
            expired
                .verify_panel_review_lease(&expected)
                .unwrap_err()
                .message
                .contains("expired")
        );
        expected
            .set(
                "lease_id",
                json::string(format!("lease_{}", "2".repeat(32))),
            )
            .unwrap();
        assert!(
            active
                .verify_panel_review_lease(&expected)
                .unwrap_err()
                .message
                .contains("lease changed")
        );
    }
    assert!(!repo.join(".oms/plan/late-results.jsonl").exists());
}

#[test]
fn versioned_contracts_gate_scheduling() {
    if isolated("versioned_contracts_gate_scheduling") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let rs = r.to_str().unwrap();
    let run = |rest: &[&str]| {
        let mut command = vec!["--repo", rs];
        command.extend_from_slice(rest);
        go(&tmp.0, &command)
    };
    let ok = |rest: &[&str]| {
        let (code, out, err) = run(rest);
        assert_eq!(code, 0, "{rest:?}: {err}");
        out
    };
    ok(&["init", "--goal", "g"]);
    ok(&["contract-propose", "--id", "api", "--summary", "v1"]);
    ok(&["contract-accept", "--id", "api", "--version", "1"]);
    ok(&[
        "add",
        "--id",
        "downstream",
        "--title",
        "work",
        "--contracts",
        "api@v1",
    ]);
    assert!(ok(&["ready"]).contains("downstream"));
    ok(&["claim", "--id", "downstream", "--provider", "codex"]);
    ok(&["release", "--id", "downstream"]);
    ok(&["contract-propose", "--id", "api", "--summary", "v2"]);
    assert!(ok(&["ready"]).contains("downstream"));
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "contract_child_accept", "--ignored"])
        .env("OMS_HARNESS_CHILD", "1")
        .env("CONTRACT_TEST_REPO", &r)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    ok(&["contract-accept", "--id", "api", "--version", "2"]);
    let show = ok(&["show", "--id", "downstream", "--json"]);
    assert!(show.contains("\"contract_stale\": true"), "{show}");
    assert!(
        show.contains("\"stale_contracts\": [\n    \"api\""),
        "{show}"
    );
    assert!(!ok(&["ready"]).contains("downstream"));
    let status = ok(&["status", "--json"]);
    assert!(
        status.contains("\"contract_stale_tasks\": [\"downstream\"]")
            && status.contains("\"contract_count\": 1"),
        "{status}"
    );
    let (code, _, err) = run(&["claim", "--id", "downstream", "--provider", "codex"]);
    assert_eq!(code, 2);
    assert!(err.contains("contract_stale"), "{err}");
    ok(&["refresh-contracts", "--task", "downstream"]);
    assert!(ok(&["show", "--id", "downstream"]).contains("api@v2"));
    assert!(ok(&["ready"]).contains("downstream"));
    ok(&["claim", "--id", "downstream", "--provider", "codex"]);
    ok(&[
        "review",
        "--id",
        "downstream",
        "--artifact",
        "v2-review.md",
        "--patch",
        "v2.diff",
    ]);
    let noop = ok(&["refresh-contracts", "--task", "downstream"]);
    assert!(noop.contains("\"state\": \"review\""), "{noop}");
    assert!(noop.contains("\"artifact\": \"v2-review.md\""), "{noop}");
    assert!(noop.contains("\"patch\": \"v2.diff\""), "{noop}");
    ok(&["contract-propose", "--id", "api", "--summary", "v3"]);
    ok(&["contract-accept", "--id", "api", "--version", "3"]);
    let refreshed = ok(&["refresh-contracts", "--task", "downstream"]);
    assert!(refreshed.contains("api@v3"), "{refreshed}");
    assert!(refreshed.contains("\"state\": \"ready\""), "{refreshed}");
    assert!(refreshed.contains("\"artifact\": \"\""), "{refreshed}");
    assert!(refreshed.contains("\"patch\": \"\""), "{refreshed}");
    assert!(
        refreshed.contains("\"review_lease_id\": \"\""),
        "{refreshed}"
    );
    ok(&["claim", "--id", "downstream", "--provider", "codex"]);
    ok(&[
        "review",
        "--id",
        "downstream",
        "--artifact",
        "v3-review.md",
        "--patch",
        "v3.diff",
    ]);
    ok(&[
        "block",
        "--id",
        "downstream",
        "--reason",
        "waiting on upstream fixture",
    ]);
    ok(&["contract-propose", "--id", "api", "--summary", "v4"]);
    ok(&["contract-accept", "--id", "api", "--version", "4"]);
    let blk = ok(&["refresh-contracts", "--task", "downstream"]);
    assert!(blk.contains("api@v4"), "{blk}");
    assert!(blk.contains("\"state\": \"blocked\""), "{blk}");
    assert!(
        blk.contains("\"reason\": \"waiting on upstream fixture\""),
        "{blk}"
    );
    assert!(blk.contains("\"artifact\": \"\""), "{blk}");
    assert!(blk.contains("\"patch\": \"\""), "{blk}");
    assert!(!ok(&["ready"]).contains("downstream"));
    assert_eq!(
        run(&["add", "--id", "bad", "--title", "bad", "--priority", "abc"]).0,
        2
    );
}

#[test]
fn team_contracts_preserve_parent_lineage_and_generation() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if std::env::var_os("OMS_TEAM_GIT_FIXTURE_BIN").is_none() {
        let fixture = Tmp::new();
        let bin = fixture.0.join("bin");
        fs::create_dir(&bin).unwrap();
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join("git"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in *'rev-parse HEAD'*) if [ -f \"${{0%/*}}/gate\" ]; then printf entered > \"${{0%/*}}/entered\"; while [ -f \"${{0%/*}}/gate\" ]; do /bin/sleep 0.02; done; fi;; esac\nexec '{}' \"$@\"\n",
            real.to_str().unwrap().replace('\'', "'\\''")
        );
        fs::write(bin.join("git"), script).unwrap();
        fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "team_contracts_preserve_parent_lineage_and_generation",
            "--nocapture",
        ]);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("OMS_") {
                command.env_remove(key);
            }
        }
        command.env("XDG_STATE_HOME", fixture.0.join("state"));
        command.env("OMS_TEAM_GIT_FIXTURE_BIN", &bin).env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        print!("{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    use oms_command_plan::json::{Value, number, object, string};
    use oms_command_plan::services::{LifecycleGuard, ProjectedLifecycle, ProjectedRoom};
    use oms_command_plan::{Action, Context, Plan, Request, json};
    static REFUSE_ORDINARY_GROUP: AtomicBool = AtomicBool::new(false);
    struct Guard<'a>(&'a AtomicBool);
    impl LifecycleGuard for Guard<'_> {
        fn check(&self) -> oms_command_plan::Result<()> {
            if self.0.load(std::sync::atomic::Ordering::Acquire) {
                return Err(oms_command_plan::Error {
                    code: 2,
                    message: "fixture lifecycle guard changed".into(),
                });
            }
            Ok(())
        }
    }
    #[derive(Default)]
    struct TeamHost {
        lifecycle_reads: std::sync::atomic::AtomicUsize,
        room_reads: std::sync::atomic::AtomicUsize,
        changed_guard: AtomicBool,
        expire_during_projection: AtomicU64,
        proof_gid: AtomicU64,
        group_reads: std::sync::atomic::AtomicUsize,
        change_group_at: std::sync::atomic::AtomicUsize,
        change_policy_at: std::sync::atomic::AtomicUsize,
        policy_path: std::sync::Mutex<Option<PathBuf>>,
    }
    impl Services for TeamHost {
        fn group_membership_resolver(&self) -> Option<GroupResolver> {
            fn ordinary(gid: u32) -> oms_command_plan::Result<GroupMembership> {
                if REFUSE_ORDINARY_GROUP.load(Ordering::Acquire) {
                    return Err(oms_command_plan::Error {
                        code: 2,
                        message: "ordinary group lookup in bounded Team admission".into(),
                    });
                }
                Host.group_membership_resolver().unwrap()(gid)
            }
            Some(ordinary)
        }
        fn group_membership_before(
            &self,
            gid: u32,
            _: std::time::Instant,
            _: &AtomicBool,
        ) -> oms_command_plan::Result<GroupMembership> {
            let mut proof = Host.group_membership_resolver().unwrap()(gid)?;
            if u64::from(gid) + 1 == self.proof_gid.load(std::sync::atomic::Ordering::Acquire) {
                let ordinal = self
                    .group_reads
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                    + 1;
                if self
                    .change_group_at
                    .load(std::sync::atomic::Ordering::Acquire)
                    == ordinal
                {
                    proof
                        .primary_member_uids
                        .push(proof.primary_member_uids[0].wrapping_add(1));
                }
                if self
                    .change_policy_at
                    .load(std::sync::atomic::Ordering::Acquire)
                    == ordinal
                {
                    fs::write(
                        self.policy_path.lock().unwrap().as_ref().unwrap(),
                        b"{\"changed\":true}\n",
                    )
                    .unwrap();
                }
            }
            Ok(proof)
        }
        fn lifecycle_writer(
            &self,
            _: &Path,
        ) -> oms_command_plan::Result<Box<dyn LifecycleGuard + '_>> {
            Ok(Box::new(Guard(&self.changed_guard)))
        }
        fn lifecycle_writer_before(
            &self,
            _: &Path,
            _: std::time::Instant,
        ) -> oms_command_plan::Result<Box<dyn LifecycleGuard + '_>> {
            Ok(Box::new(Guard(&self.changed_guard)))
        }
        fn project_lifecycle(&self, raw: &[u8]) -> oms_command_plan::Result<ProjectedLifecycle> {
            self.lifecycle_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let until = self
                .expire_during_projection
                .swap(0, std::sync::atomic::Ordering::AcqRel);
            while SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                < until
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let terminal = std::str::from_utf8(raw).unwrap().contains("cancelled");
            Ok(ProjectedLifecycle {
                source_sha256: json::sha(raw),
                attempts: object([(
                    "main-a",
                    object([
                        ("attempt_id", string("main-a")),
                        ("tool", string("panel-main")),
                        ("parent_attempt_id", string("")),
                        ("terminal", Value::Bool(terminal)),
                        (
                            "state",
                            string(if terminal { "cancelled" } else { "working" }),
                        ),
                        (
                            "refs",
                            object([("panel_room_id", string("room-aaaaaaaaaaaa"))]),
                        ),
                    ]),
                )]),
            })
        }
        fn project_room(&self, room: &str, raw: &[u8]) -> oms_command_plan::Result<ProjectedRoom> {
            self.room_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(ProjectedRoom {
                source_sha256: json::sha(raw),
                state: object([
                    ("schema", number(1)),
                    ("id", string(room)),
                    ("closed", Value::Bool(false)),
                    (
                        "participants",
                        Value::Array(vec![object([
                            ("participant", string("main-a")),
                            ("role", string("main")),
                            ("joined", Value::Bool(true)),
                        ])]),
                    ),
                ]),
            })
        }
        fn supervise(
            &self,
            command: &oms_command_plan::services::SupervisedCommand,
        ) -> oms_command_plan::Result<oms_command_plan::services::Execution> {
            use oms_supervisor::process::{Progress, Reactor, Request, Scope, StopReason};
            let mut reactor = Reactor::new();
            let mut request = Request::new(command.argv.clone());
            request.cwd = Some(command.cwd.clone());
            request.scope = Scope::ProcessGroup;
            request.deadline = Some(std::time::Instant::now() + command.wall);
            request.environment = command.environment.clone();
            request.remove_environment = command
                .remove_environment
                .iter()
                .map(OsString::from)
                .collect();
            request.capture.max_bytes = command.output_limit;
            let job = reactor
                .start(request)
                .map_err(|e| oms_command_plan::Error {
                    code: 2,
                    message: e.to_string(),
                })?;
            loop {
                match reactor
                    .poll(job, std::time::Duration::from_millis(10))
                    .map_err(|e| oms_command_plan::Error {
                        code: 2,
                        message: e.to_string(),
                    })? {
                    Progress::Pending(_) => {}
                    Progress::Complete(done) => {
                        return Ok(oms_command_plan::services::Execution {
                            exit: done
                                .exit
                                .map(|exit| exit.shell_code().try_into().unwrap_or(125))
                                .unwrap_or(125),
                            runner_exit: 0,
                            timed_out: done.stop == Some(StopReason::Deadline),
                            output_limited: done.stdout.limited,
                            launch_error: false,
                            supervision_error: !done.cleanup_complete() || !done.output_complete(),
                            output: done.stdout.bytes,
                            duration_s: done.duration.as_secs(),
                        });
                    }
                }
            }
        }
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let stop = AtomicBool::new(false);
    let host = TeamHost::default();
    let call = |values: &[&str], child: bool| {
        let req = Request::parse(&args(values)).unwrap();
        let mut ctx = Context::from_environment(&req, &r).unwrap();
        ctx.panel_main_attempt = "main-a".into();
        ctx.room_id = "room-aaaaaaaaaaaa".into();
        ctx.participant = "main-a".into();
        ctx.child = child;
        Plan::open(ctx, &host, &stop).unwrap().execute(&req)
    };
    call(&["init", "--goal", "bounded fixture"], false).unwrap();
    call(
        &[
            "add",
            "--id",
            "task-a",
            "--title",
            "team fixture",
            "--allowed",
            "src/",
            "--verify",
            "true",
        ],
        false,
    )
    .unwrap();
    assert_eq!(
        call(&["team-list", "--json"], true).unwrap().stdout,
        b"[]\n"
    );
    fs::create_dir_all(r.join(".oms/lifecycle")).unwrap();
    fs::create_dir_all(r.join(".oms/threads")).unwrap();
    let history = concat!(
        "{\"schema\":1,\"event_id\":\"evt-owner\",\"attempt_id\":\"main-a\",\"seq\":1,\"ts\":\"2026-01-01T00:00:00Z\",\"event_type\":\"attempt.created\",\"provider\":\"codex\",\"tool\":\"panel-main\",\"refs\":{\"panel_role\":\"main\",\"panel_room_id\":\"room-aaaaaaaaaaaa\",\"panel_room_participant\":\"main-a\",\"panel_owner\":\"codex\"}}\n",
        "{\"schema\":1,\"event_id\":\"evt-start\",\"attempt_id\":\"main-a\",\"seq\":2,\"ts\":\"2026-01-01T00:00:01Z\",\"event_type\":\"attempt.state_changed\",\"to_state\":\"working\"}\n"
    );
    fs::write(r.join(".oms/lifecycle/events.jsonl"), history).unwrap();
    fs::write(
        r.join(".oms/threads/room-aaaaaaaaaaaa.jsonl"),
        b"{\"schema\":1,\"thread\":\"room-aaaaaaaaaaaa\",\"seq\":1,\"ts\":\"2026-01-01T00:00:00Z\",\"room_event\":{\"kind\":\"join\",\"participant\":\"main-a\",\"provider\":\"codex\",\"role\":\"main\"}}\n",
    )
    .unwrap();
    fs::write(r.join("team-policy.json"), b"{}\n").unwrap();
    fs::set_permissions(
        r.join("team-policy.json"),
        fs::Permissions::from_mode(0o664),
    )
    .unwrap();
    host.proof_gid.store(
        u64::from(fs::metadata(r.join("team-policy.json")).unwrap().gid()) + 1,
        std::sync::atomic::Ordering::Release,
    );
    *host.policy_path.lock().unwrap() = Some(r.join("team-policy.json"));
    let plan = json::parse(&fs::read(r.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    let task = plan.get("tasks").get("task-a");
    let head = Command::new("git")
        .arg("-C")
        .arg(&r)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    let policy = json::sha(b"{}\n");
    let mut spec = json::parse(include_bytes!("fixtures/team-spec.json")).unwrap();
    for (key, value) in [
        ("room_id", string("room-aaaaaaaaaaaa")),
        ("plan_id", plan.get("plan_id").clone()),
        ("task_sha256", string(json::hash(task).unwrap())),
        ("objective_sha256", string(json::sha(b"bounded fixture"))),
        (
            "criterion_sha256",
            string(oms_command_plan::team::criterion_digest(&plan, task).unwrap()),
        ),
        (
            "base_sha",
            string(std::str::from_utf8(&head).unwrap().trim()),
        ),
        ("policy_sha256", string(&policy)),
    ] {
        spec.set(key, value).unwrap();
    }
    let mut members = spec.get("members").array().unwrap().to_vec();
    for member in &mut members {
        let mut route = member.get("route").clone();
        route.set("policy_sha256", string(&policy)).unwrap();
        member.set("route", route).unwrap();
    }
    spec.set("members", Value::Array(members)).unwrap();
    let write =
        |spec: &Value| fs::write(r.join("team.json"), json::canonical(spec).unwrap()).unwrap();
    let register = ["team-register", "--team-spec", "team.json", "--json"];
    for (id, allowed, forbidden, team_allow, team_deny) in [
        ("task-glob", "src/?", "", vec!["src/a"], vec![]),
        (
            "task-deny",
            "src",
            "src/private",
            vec!["src"],
            vec!["src/???????"],
        ),
        ("task-empty", "src", "", vec![], vec![]),
    ] {
        call(
            &[
                "add",
                "--id",
                id,
                "--title",
                "scope containment",
                "--allowed",
                allowed,
                "--forbidden",
                forbidden,
                "--verify",
                "true",
            ],
            false,
        )
        .unwrap();
        let parent = json::parse(&fs::read(r.join(".oms/plan/tasks.json")).unwrap()).unwrap();
        let task = parent.get("tasks").get(id);
        let mut invalid = spec.clone();
        invalid.set("task_id", string(id)).unwrap();
        invalid
            .set("task_sha256", string(json::hash(task).unwrap()))
            .unwrap();
        invalid
            .set(
                "allowed_paths",
                Value::Array(team_allow.into_iter().map(string).collect()),
            )
            .unwrap();
        invalid
            .set(
                "forbidden_paths",
                Value::Array(team_deny.into_iter().map(string).collect()),
            )
            .unwrap();
        write(&invalid);
        let error = call(&register, false).unwrap_err();
        assert!(error.message.contains("scope"), "{id}: {error}");
    }
    for (key, bad) in [
        ("schema", number(2)),
        ("root_owner_generation", string("evt-recycled")),
        ("expires_at", string("2000-01-01T00:00:00Z")),
        ("policy_sha256", string("0".repeat(64))),
        ("root_owner", string("foreign-main")),
    ] {
        let mut invalid = spec.clone();
        invalid.set(key, bad).unwrap();
        write(&invalid);
        assert!(call(&register, false).is_err(), "{key}");
    }
    call(
        &[
            "add",
            "--id",
            "unrestricted",
            "--title",
            "parent without an allow restriction",
            "--verify",
            "true",
        ],
        false,
    )
    .unwrap();
    let parent = json::parse(&fs::read(r.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    let unrestricted = parent.get("tasks").get("unrestricted");
    for (id, allowed) in [
        ("team-no-restriction", vec![]),
        ("team-narrow-restriction", vec!["src"]),
    ] {
        let mut allowed_spec = spec.clone();
        allowed_spec.set("id", string(id)).unwrap();
        allowed_spec.set("task_id", string("unrestricted")).unwrap();
        allowed_spec
            .set("task_sha256", string(json::hash(unrestricted).unwrap()))
            .unwrap();
        allowed_spec
            .set(
                "allowed_paths",
                Value::Array(allowed.into_iter().map(string).collect()),
            )
            .unwrap();
        write(&allowed_spec);
        call(&register, false).unwrap();
    }
    for (key, bad) in [
        ("logical_parent", string("worker-a")),
        ("root_owner", string("foreign-main")),
        ("id", string("main-a")),
    ] {
        let mut invalid = spec.clone();
        let mut members = invalid.get("members").array().unwrap().to_vec();
        members[0].set(key, bad).unwrap();
        invalid.set("members", Value::Array(members)).unwrap();
        assert!(
            oms_command_plan::team::TeamSpec::parse(&json::canonical(&invalid).unwrap()).is_err()
        );
    }
    write(&spec);
    assert!(call(&register, true).is_err());
    let enrolled = call(&register, false).unwrap();
    let record = json::parse(&enrolled.stdout).unwrap();
    let digest = record.get("team-a").get("versions").array().unwrap()[0]
        .text("sha256")
        .to_owned();
    let stored = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    assert!(call(&register, false).is_err());
    assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), stored);
    let shown = json::parse(&call(&["team-show", "--id", "team-a"], true).unwrap().stdout).unwrap();
    assert_eq!(shown.text("phase"), "planned");
    assert_eq!(shown.get("verified"), &Value::Null);
    assert_eq!(shown.get("spec").get("members").array().unwrap().len(), 2);
    host.lifecycle_reads
        .store(0, std::sync::atomic::Ordering::Relaxed);
    host.room_reads
        .store(0, std::sync::atomic::Ordering::Relaxed);
    let listed = call(&["team-list", "--json"], true).unwrap();
    assert_eq!(
        json::parse(&listed.stdout).unwrap().array().unwrap().len(),
        3
    );
    assert_eq!(
        host.lifecycle_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(
        host.room_reads.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), stored);
    let mut admission_context =
        Context::from_environment(&Request::new(Action::TeamShow), &r).unwrap();
    admission_context.panel_main_attempt = "main-a".into();
    admission_context.room_id = "room-aaaaaaaaaaaa".into();
    admission_context.participant = "main-a".into();
    // An old Context timestamp is not the final admission clock.
    admission_context.ts = "2201-01-01T00:00:00Z".into();
    let admission_plan = Plan::open(admission_context.clone(), &host, &stop).unwrap();
    let admission = oms_command_plan::team::AdmissionRequest {
        team_id: "team-a".into(),
        spec_sha256: digest.clone(),
        member_id: "lead-a".into(),
        member_generation: 1,
        provider: "codex".into(),
        harness: "codex".into(),
        host: "local".into(),
        transport: "direct".into(),
        account: Some("work".into()),
        selected_model: Some("fixture".into()),
        selected_effort: Some("high".into()),
        write: false,
    };
    let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(5);
    let guard = admission_plan
        .team_admission_before(admission.clone(), deadline())
        .unwrap();
    assert_eq!(guard.binding().text("root_owner_generation"), "evt-owner");
    assert_eq!(guard.binding().get("member").text("attempt_id"), "");
    let before = std::time::Instant::now();
    assert_eq!(
        admission_plan
            .team_admission_before(
                admission.clone(),
                before + std::time::Duration::from_millis(30)
            )
            .err()
            .unwrap()
            .code,
        75
    );
    assert!(before.elapsed() < std::time::Duration::from_secs(1));
    fs::write(r.join("team-policy.json"), b"{\"changed\":true}\n").unwrap();
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("policy changed")
    );
    fs::write(r.join("team-policy.json"), b"{}\n").unwrap();
    guard.check().unwrap();
    host.change_group_at.store(
        host.group_reads.load(std::sync::atomic::Ordering::Acquire) + 2,
        std::sync::atomic::Ordering::Release,
    );
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("group ownership changed")
    );
    guard.check().unwrap();
    host.change_policy_at.store(
        host.group_reads.load(std::sync::atomic::Ordering::Acquire) + 2,
        std::sync::atomic::Ordering::Release,
    );
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("policy changed")
    );
    fs::write(r.join("team-policy.json"), b"{}\n").unwrap();
    guard.check().unwrap();
    host.changed_guard
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(guard.check().unwrap_err().message.contains("guard changed"));
    host.changed_guard
        .store(false, std::sync::atomic::Ordering::Release);
    let mut withdrawn = json::parse(&stored).unwrap();
    let mut teams = withdrawn.get("teams").clone();
    let mut team = teams.get("team-a").clone();
    team.set("phase", string("settled")).unwrap();
    teams.set("team-a", team).unwrap();
    withdrawn.set("teams", teams).unwrap();
    fs::write(
        r.join(".oms/plan/tasks.json"),
        json::canonical(&withdrawn).unwrap(),
    )
    .unwrap();
    assert!(guard.check().unwrap_err().message.contains("withdrawn"));
    fs::write(r.join(".oms/plan/tasks.json"), &stored).unwrap();
    fs::write(r.join(".oms/lifecycle/events.jsonl"), format!("{history}{{\"schema\":1,\"event_id\":\"evt-cancel\",\"attempt_id\":\"main-a\",\"seq\":3,\"ts\":\"2026-01-01T00:00:02Z\",\"event_type\":\"attempt.state_changed\",\"state\":\"cancelled\"}}\n")).unwrap();
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("owner is stale")
    );
    fs::write(r.join(".oms/lifecycle/events.jsonl"), history).unwrap();
    guard.check().unwrap();
    drop(guard);
    let git_bin = PathBuf::from(std::env::var_os("OMS_TEAM_GIT_FIXTURE_BIN").unwrap());
    fs::write(git_bin.join("gate"), b"wait").unwrap();
    let started = std::time::Instant::now();
    let refused = admission_plan
        .team_admission_before(
            admission.clone(),
            started + std::time::Duration::from_millis(300),
        )
        .err()
        .unwrap();
    assert_eq!(refused.code, 75, "{refused}");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(git_bin.join("entered").is_file());
    fs::remove_file(git_bin.join("entered")).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !git_bin.join("entered").is_file() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            stop.store(true, std::sync::atomic::Ordering::Release);
        });
        let started = std::time::Instant::now();
        assert_eq!(
            admission_plan
                .team_admission_before(admission.clone(), deadline())
                .err()
                .unwrap()
                .code,
            130
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    });
    assert!(git_bin.join("entered").is_file());
    stop.store(false, std::sync::atomic::Ordering::Release);
    fs::remove_file(git_bin.join("gate")).unwrap();
    admission_plan
        .team_admission_before(admission.clone(), deadline())
        .unwrap()
        .check()
        .unwrap();
    for field in [
        "spec",
        "member",
        "provider",
        "harness",
        "host",
        "transport",
        "account",
        "model",
        "effort",
        "unknown-effort",
        "access",
    ] {
        let mut wrong = admission.clone();
        match field {
            "spec" => wrong.spec_sha256 = "0".repeat(64),
            "member" => wrong.member_generation += 1,
            "provider" => wrong.provider = "claude".into(),
            "harness" => wrong.harness = "claude".into(),
            "host" => wrong.host = "foreign".into(),
            "transport" => wrong.transport = "native".into(),
            "account" => wrong.account = Some("other-account".into()),
            "model" => wrong.selected_model = None,
            "effort" => wrong.selected_effort = Some("medium".into()),
            "unknown-effort" => wrong.selected_effort = None,
            "access" => wrong.write = true,
            _ => unreachable!(),
        }
        assert!(
            admission_plan
                .team_admission_before(wrong, deadline())
                .is_err(),
            "{field}"
        );
    }
    let mut child_context = admission_context.clone();
    child_context.child = true;
    assert!(
        Plan::open(child_context, &host, &stop)
            .unwrap()
            .team_admission_before(admission.clone(), deadline())
            .is_err()
    );
    assert!(!r.join(".oms/usage/receipts.jsonl").exists());
    assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), stored);
    let mut expiring = spec.clone();
    expiring.set("id", string("team-expiring")).unwrap();
    let expiry = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3;
    expiring
        .set(
            "expires_at",
            string(
                chrono::DateTime::from_timestamp(expiry as i64, 0)
                    .unwrap()
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string(),
            ),
        )
        .unwrap();
    write(&expiring);
    call(&register, false).unwrap();
    let mut expiry_request = admission.clone();
    expiry_request.team_id = "team-expiring".into();
    expiry_request.spec_sha256 = json::hash(&expiring).unwrap();
    let guard = admission_plan
        .team_admission_before(expiry_request, deadline())
        .unwrap();
    host.expire_during_projection
        .store(expiry, std::sync::atomic::Ordering::Release);
    assert!(guard.check().unwrap_err().message.contains("expired"));
    drop(guard);

    spec.set("generation", number(2)).unwrap();
    let mut updated_members = spec.get("members").array().unwrap().to_vec();
    let large_generation = i128::from(u64::MAX) + 7;
    updated_members[0]
        .set("generation", number(large_generation))
        .unwrap();
    spec.set("members", Value::Array(updated_members)).unwrap();
    spec.set("previous_sha256", string(&digest)).unwrap();
    write(&spec);
    assert!(call(&register, false).is_err());
    call(
        &[
            "team-register",
            "--team-spec",
            "team.json",
            "--expected-team-sha256",
            &digest,
        ],
        false,
    )
    .unwrap();
    let shown = json::parse(
        &call(&["team-show", "--id", "team-a"], false)
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(shown.text("phase"), "held");
    assert_eq!(shown.get("history").array().unwrap().len(), 2);
    let mut next_request = admission.clone();
    next_request.spec_sha256 = json::hash(&spec).unwrap();
    next_request.member_generation = large_generation;
    admission_plan
        .team_admission_before(next_request, deadline())
        .unwrap()
        .check()
        .unwrap();
    fs::write(r.join("team-policy.json"), b"{\"changed\":true}\n").unwrap();
    let stale = json::parse(
        &call(&["team-show", "--id", "team-a"], false)
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(stale.text("reason").contains("policy changed"));
    let current_digest = shown
        .get("history")
        .array()
        .unwrap()
        .last()
        .unwrap()
        .text("sha256");
    assert!(
        call(
            &[
                "team-cancel",
                "--id",
                "team-a",
                "--expected-team-sha256",
                &digest,
                "--reason",
                "withdraw"
            ],
            false
        )
        .is_err()
    );
    call(
        &[
            "team-cancel",
            "--id",
            "team-a",
            "--expected-team-sha256",
            current_digest,
            "--reason",
            "withdraw unlaunched team",
        ],
        false,
    )
    .unwrap();
    let cancelled = json::parse(
        &call(&["team-show", "--id", "team-a"], false)
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(cancelled.text("phase"), "settled");
    assert_eq!(
        fs::read_to_string(r.join(".oms/lifecycle/events.jsonl")).unwrap(),
        history
    );

    // The native publisher and room/lifecycle adapters are fixture inputs here;
    // Git objects, Plan transitions, verifier execution and typed completion are real.
    fs::write(r.join("team-policy.json"), b"{}\n").unwrap();
    fs::write(r.join(".git/info/exclude"), b".oms/\n").unwrap();
    git(&r, &["add", "."]);
    git(&r, &["commit", "-qm", "freeze Team fixture source"]);
    let git_text = |values: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&r)
            .args(values)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {values:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let base_commit = git_text(&["rev-parse", "HEAD"]);
    for id in ["commit-obligation", "commit-product"] {
        call(
            &[
                "add",
                "--id",
                id,
                "--title",
                id,
                "--allowed",
                "src/product.txt",
                "--verify",
                "bash verify-product.sh",
            ],
            false,
        )
        .unwrap();
        call(&["claim", "--id", id, "--provider", "codex"], false).unwrap();
    }
    call(
        &[
            "add",
            "--id",
            "commit-dependent",
            "--title",
            "use the verified product",
            "--depends",
            "commit-obligation",
            "--allowed",
            "src",
            "--verify",
            "true",
        ],
        false,
    )
    .unwrap();
    fs::create_dir_all(r.join("src")).unwrap();
    fs::write(r.join("src/product.txt"), b"product\n").unwrap();
    fs::write(
        r.join("verify-product.sh"),
        b"#!/bin/sh\nset -eu\ntest \"$(cat src/product.txt)\" = product\n",
    )
    .unwrap();
    git(&r, &["add", "src/product.txt", "verify-product.sh"]);
    git(&r, &["commit", "-qm", "actual completion product"]);
    let product_commit = git_text(&["rev-parse", "HEAD"]);
    let product_tree = git_text(&["rev-parse", "HEAD^{tree}"]);
    git(
        &r,
        &["update-ref", "refs/remotes/fixture/main", &product_commit],
    );
    let mut cksum = Command::new("cksum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        cksum.stdin.take().as_mut().unwrap(),
        std::os::unix::ffi::OsStrExt::as_bytes(r.as_os_str()),
    )
    .unwrap();
    let checksum = cksum.wait_with_output().unwrap();
    assert!(checksum.status.success());
    let checksum = String::from_utf8(checksum.stdout).unwrap();
    let mut checksum = checksum.split_whitespace();
    let receipt_directory = PathBuf::from(std::env::var_os("XDG_STATE_HOME").unwrap())
        .join("oh-my-setting/land")
        .join(format!(
            "repo-{}-{}",
            checksum.next().unwrap(),
            checksum.next().unwrap()
        ));
    fs::create_dir_all(&receipt_directory).unwrap();
    fs::set_permissions(&receipt_directory, fs::Permissions::from_mode(0o770)).unwrap();
    let receipt_path = receipt_directory.join(format!("{product_commit}-fixture.json"));
    let land_receipt = json::canonical(&object([
        ("schema", number(1)),
        ("sha", string(&product_commit)),
        ("state", string("passed")),
        ("remote", string("fixture")),
        ("target", string("main")),
        ("gate", object([("rc", number(0))])),
        (
            "push",
            object([
                ("rc", number(0)),
                ("moved", number(1)),
                ("base", string(&base_commit)),
            ]),
        ),
        ("ci", object([("conclusion", string("success"))])),
    ]))
    .unwrap();
    fs::write(&receipt_path, &land_receipt).unwrap();
    fs::set_permissions(&receipt_path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir_all(r.join(".oms/artifacts")).unwrap();
    let artifact_path = ".oms/artifacts/commit-product.md";
    let artifact = b"The fixture product implements commit-obligation.\n";
    fs::write(r.join(artifact_path), artifact).unwrap();
    call(
        &[
            "review",
            "--id",
            "commit-product",
            "--artifact",
            artifact_path,
        ],
        false,
    )
    .unwrap();
    let finish = [
        "finish",
        "--id",
        "commit-product",
        "--landed-commit",
        &product_commit,
    ];
    let awaiting =
        String::from_utf8_lossy(&land_receipt).replace("\"passed\"", "\"awaiting-merge\"");
    fs::write(&receipt_path, awaiting).unwrap();
    let refused = call(&finish, false).unwrap_err();
    assert!(
        refused
            .message
            .contains("pull request is open and unmerged"),
        "{}",
        refused.message
    );
    // A squash merge reaches the target as a new commit with the head's tree;
    // only a receipt that names that merged commit proves the landing.
    let squash = git_text(&[
        "commit-tree",
        &product_tree,
        "-p",
        &base_commit,
        "-m",
        "actual completion product (squash)",
    ]);
    git(&r, &["update-ref", "refs/remotes/fixture/main", &squash]);
    fs::write(&receipt_path, &land_receipt).unwrap();
    assert!(
        call(&finish, false)
            .unwrap_err()
            .message
            .contains("not reachable")
    );
    let merged = |commit: &str| {
        String::from_utf8_lossy(&land_receipt).replace(
            "\"push\":",
            &format!("\"merge\":{{\"commit\":\"{commit}\",\"state\":\"MERGED\"}},\"push\":"),
        )
    };
    let other_tree = git_text(&[
        "commit-tree",
        "HEAD^^{tree}",
        "-p",
        &base_commit,
        "-m",
        "other",
    ]);
    git(
        &r,
        &["update-ref", "refs/remotes/fixture/main", &other_tree],
    );
    fs::write(&receipt_path, merged(&other_tree)).unwrap();
    assert!(call(&finish, false).is_err());
    git(&r, &["update-ref", "refs/remotes/fixture/main", &squash]);
    let land_receipt = merged(&squash).into_bytes();
    fs::write(&receipt_path, &land_receipt).unwrap();
    call(&finish, false).unwrap();

    let parent = json::parse(&fs::read(r.join(".oms/plan/tasks.json")).unwrap()).unwrap();
    let dependent = parent.get("tasks").get("commit-dependent");
    let mut dependent_spec = spec.clone();
    for (key, value) in [
        ("id", string("team-commit-dependent")),
        ("generation", number(1)),
        ("previous_sha256", string("")),
        ("task_id", string("commit-dependent")),
        ("task_sha256", string(json::hash(dependent).unwrap())),
        (
            "criterion_sha256",
            string(oms_command_plan::team::criterion_digest(&parent, dependent).unwrap()),
        ),
        ("base_sha", string(&product_commit)),
    ] {
        dependent_spec.set(key, value).unwrap();
    }
    let dependent_spec_path = ".oms/plan/commit-dependent-team.json";
    fs::write(
        r.join(dependent_spec_path),
        json::canonical(&dependent_spec).unwrap(),
    )
    .unwrap();
    call(
        &["team-register", "--team-spec", dependent_spec_path],
        false,
    )
    .unwrap();
    let dependent_request = oms_command_plan::team::AdmissionRequest {
        team_id: "team-commit-dependent".into(),
        spec_sha256: json::hash(&dependent_spec).unwrap(),
        member_generation: large_generation,
        ..admission.clone()
    };
    assert!(
        admission_plan
            .team_admission_before(dependent_request.clone(), deadline())
            .err()
            .unwrap()
            .message
            .contains("dependencies")
    );

    let plan_raw = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let parent = json::parse(&plan_raw).unwrap();
    let original = parent.get("tasks").get("commit-obligation");
    let product = parent.get("tasks").get("commit-product");
    assert_eq!(product.text("state"), "done");
    assert_eq!(product.get("landing").text("sha"), product_commit);
    let manifest = Value::Array(
        ["src/product.txt", "verify-product.sh"]
            .into_iter()
            .map(|path| {
                object([
                    ("path", string(path)),
                    ("state", string("file")),
                    ("mode", string("100644")),
                    ("sha256", string(json::sha(fs::read(r.join(path)).unwrap()))),
                ])
            })
            .collect(),
    );
    let bundle = object([
        ("schema", number(1)),
        ("kind", string("satisfied-by")),
        ("plan_id", parent.get("plan_id").clone()),
        ("task_id", original.get("id").clone()),
        (
            "task_sha256",
            string(json::receipt_digest(original).unwrap()),
        ),
        ("plan_sha256", string(json::sha(&plan_raw))),
        ("state", original.get("state").clone()),
        ("lease_id", original.get("lease_id").clone()),
        (
            "owner",
            object([
                ("provider", string("codex")),
                ("participant", string("main-a")),
                ("parent_provider", string("codex")),
            ]),
        ),
        ("original_contract", json::projection(original).unwrap()),
        (
            "review_summary",
            string("The exact committed product fulfills the original obligation."),
        ),
        (
            "reviewed_obligations",
            Value::Array(vec![string("product")]),
        ),
        (
            "requirements",
            Value::Array(vec![object([
                ("id", string("product")),
                ("obligation", original.get("title").clone()),
                (
                    "deliverable",
                    string("src/product.txt in the exact committed tree"),
                ),
                (
                    "source_paths",
                    Value::Array(vec![string("src/product.txt")]),
                ),
                (
                    "evidence",
                    Value::Array(vec![object([
                        ("path", string(artifact_path)),
                        ("sha256", string(json::sha(artifact))),
                    ])]),
                ),
            ])]),
        ),
        ("base_commit", string(&base_commit)),
        (
            "source_envelope",
            Value::Array(vec![string("src/product.txt"), string("verify-product.sh")]),
        ),
        ("source_manifest", manifest.clone()),
        (
            "verifier",
            object([
                ("command", string("bash verify-product.sh")),
                ("adopted", Value::Bool(false)),
                (
                    "review_summary",
                    string("Frozen script checks the actual product bytes."),
                ),
                (
                    "files",
                    Value::Array(vec![manifest.array().unwrap()[1].clone()]),
                ),
            ]),
        ),
        (
            "successor",
            object([
                ("plan_id", parent.get("plan_id").clone()),
                ("task_id", product.get("id").clone()),
                (
                    "task_sha256",
                    string(json::receipt_digest(product).unwrap()),
                ),
                ("original_contract", json::projection(product).unwrap()),
                ("product_kind", string("commit")),
                ("commit_sha", string(&product_commit)),
                ("land_receipt_sha256", string(json::sha(&land_receipt))),
            ]),
        ),
    ]);
    let bundle_path = ".oms/plan/commit-product-bundle.json";
    let bundle_raw = json::canonical(&bundle).unwrap();
    fs::write(r.join(bundle_path), &bundle_raw).unwrap();
    let satisfy = [
        "satisfy",
        "--id",
        "commit-obligation",
        "--completion-bundle",
        bundle_path,
        "--expected-completion-bundle-sha256",
        &json::sha(&bundle_raw),
        "--expected-plan-sha256",
        bundle.text("plan_sha256"),
        "--expected-task-sha256",
        bundle.text("task_sha256"),
        "--expected-state",
        bundle.text("state"),
        "--lease-id",
        bundle.text("lease_id"),
    ];
    fs::write(r.join("src/product.txt"), b"stale product\n").unwrap();
    assert!(
        call(&satisfy, false)
            .unwrap_err()
            .message
            .contains("source content/mode changed")
    );
    assert_eq!(fs::read(r.join(".oms/plan/tasks.json")).unwrap(), plan_raw);
    fs::write(r.join("src/product.txt"), b"product\n").unwrap();
    call(&satisfy, false).unwrap();
    let completed_raw = fs::read(r.join(".oms/plan/tasks.json")).unwrap();
    let completed = json::parse(&completed_raw).unwrap();
    let completed = completed.get("tasks").get("commit-obligation");
    assert_eq!(completed.text("completion_kind"), "satisfied-by");
    assert_eq!(completed.text("state"), "done");
    let completion_path = r.join(completed.get("completion_receipt").text("path"));
    let completion_raw = fs::read(&completion_path).unwrap();
    let completion = json::parse(&completion_raw).unwrap();
    assert_eq!(
        completion
            .get("snapshot")
            .get("evidence")
            .text("product_kind"),
        "commit"
    );
    assert_eq!(completion.get("snapshot").text("head"), product_commit);
    assert_eq!(git_text(&["rev-parse", "HEAD^{tree}"]), product_tree);

    REFUSE_ORDINARY_GROUP.store(true, Ordering::Release);
    // One guard spans all of the Git and receipt mutations below.
    let proof_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let guard = admission_plan
        .team_admission_before(dependent_request.clone(), proof_deadline)
        .unwrap();
    guard.check().unwrap();
    fs::write(&receipt_path, b"{}\n").unwrap();
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("no passed oms land receipt")
    );
    fs::write(&receipt_path, &land_receipt).unwrap();
    guard.check().unwrap();
    fs::write(&completion_path, b"{}\n").unwrap();
    assert!(
        guard
            .check()
            .unwrap_err()
            .message
            .contains("evidence digest changed")
    );
    fs::write(&completion_path, &completion_raw).unwrap();
    guard.check().unwrap();
    git(&r, &["update-ref", "-d", "refs/remotes/fixture/main"]);
    assert!(guard.check().unwrap_err().message.contains("not reachable"));
    git(&r, &["update-ref", "refs/remotes/fixture/main", &squash]);
    guard.check().unwrap();
    drop(guard);
    fs::write(r.join(artifact_path), b"tampered product report\n").unwrap();
    assert!(
        admission_plan
            .team_admission_before(dependent_request.clone(), deadline())
            .err()
            .unwrap()
            .message
            .contains("evidence digest changed")
    );
    fs::write(r.join(artifact_path), artifact).unwrap();
    git(&r, &["commit", "--allow-empty", "-qm", "a new Team base"]);
    assert!(
        admission_plan
            .team_admission_before(dependent_request.clone(), deadline())
            .err()
            .unwrap()
            .message
            .contains("team base changed")
    );
    git(&r, &["update-ref", "HEAD", &product_commit]);
    admission_plan
        .team_admission_before(dependent_request, deadline())
        .unwrap()
        .check()
        .unwrap();
    REFUSE_ORDINARY_GROUP.store(false, Ordering::Release);
    assert_eq!(
        fs::read(r.join(".oms/plan/tasks.json")).unwrap(),
        completed_raw
    );
    let req = Request::new(Action::TeamList);
    let mut ctx = Context::from_environment(&req, &r).unwrap();
    ctx.child = true;
    assert!(Plan::open(ctx, &host, &stop).unwrap().execute(&req).is_ok());
}

#[test]
#[ignore = "driven by versioned_contracts_gate_scheduling"]
fn contract_child_accept() {
    let repo = PathBuf::from(std::env::var_os("CONTRACT_TEST_REPO").unwrap());
    let (code, _, err) = go(&repo, &["contract-accept", "--id", "api", "--version", "2"]);
    assert_eq!(code, 2);
    assert!(err.contains("parent-only"), "{err}");
    let (code, _, err) = go(&repo, &["refresh-contracts", "--task", "downstream"]);
    assert_eq!(code, 2);
    assert!(err.contains("parent-only"), "{err}");
}

#[test]
fn contract_registry_refuses_unsafe_inputs_and_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    if isolated("contract_registry_refuses_unsafe_inputs_and_permissions") {
        return;
    }
    let tmp = Tmp::new();
    let r = repo(&tmp);
    let run = |rest: &[&str]| go(&r, rest);
    assert_eq!(run(&["init", "--goal", "g"]).0, 0);
    assert_eq!(
        run(&["contract-propose", "--id", "api", "--summary", " "]).0,
        2
    );
    assert_eq!(
        run(&[
            "contract-propose",
            "--id",
            "api",
            "--summary",
            "x",
            "--scope-paths",
            "../bad"
        ])
        .0,
        2
    );
    assert_eq!(
        run(&[
            "contract-propose",
            "--id",
            "api",
            "--summary",
            "x",
            "--decision-artifact",
            "../bad"
        ])
        .0,
        2
    );
    symlink("a.txt", r.join("link")).unwrap();
    assert_eq!(
        run(&[
            "contract-propose",
            "--id",
            "api",
            "--summary",
            "x",
            "--decision-artifact",
            "link"
        ])
        .0,
        2
    );
    assert_eq!(
        run(&[
            "contract-propose",
            "--id",
            "api",
            "--summary",
            "x",
            "--decision-artifact",
            "a.txt"
        ])
        .0,
        0
    );
    let path = r.join(".oms/plan/contracts.json");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(run(&["contract-list"]).0, 2);
}
