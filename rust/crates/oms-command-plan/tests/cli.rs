#![forbid(unsafe_code)]

use oms_command_plan::services::{GroupMembership, GroupResolver};
use oms_command_plan::{Services, UnavailableServices, owns, run, run_with_io, run_with_services};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let n = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("oms-plan-cli-{}-{n}", std::process::id()));
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
