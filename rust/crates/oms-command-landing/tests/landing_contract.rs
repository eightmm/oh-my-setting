//! Contract tests for the landing command surface. Behavioural cases run in a
//! re-executed copy of this test binary with a scrubbed environment and a PATH
//! that holds only `git` and `bash`, so no `python3`, `OMS_*` or `GIT_*` state leaks in.
#![forbid(unsafe_code)]

use oms_command_landing::{TOOLS, owns, run, usage};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
const SEP: char = '\u{1f}';

struct TempDir(PathBuf);
impl TempDir {
    fn new(prefix: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("oms-landing-{prefix}-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("chmod temp dir");
        Self(path.canonicalize().expect("canonicalize temp dir"))
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

/// Strip group/world write recursively so results do not depend on the umask.
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

fn which(name: &str) -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} on PATH"))
}

/// Scrubbed sandbox: private home and TMPDIR, PATH linking only `git` and `bash`.
struct Isolated {
    home: TempDir,
    bin: PathBuf,
    tmp: PathBuf,
    /// Private copy of this test binary: the controller refuses an executing
    /// image that is group/world writable, which depends on the build umask.
    exe: PathBuf,
}
impl Isolated {
    fn new() -> Self {
        let home = TempDir::new("home");
        let bin = home.path().join("bin");
        fs::create_dir_all(&bin).expect("bin dir");
        for tool in ["git", "bash"] {
            std::os::unix::fs::symlink(which(tool), bin.join(tool)).expect("link tool");
        }
        let tmp = home.path().join("tmp");
        fs::create_dir_all(&tmp).expect("tmp dir");
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).expect("chmod tmp");
        strip_group_world_write(&bin);
        let exe = home.path().join("landing-contract-exe");
        fs::copy(std::env::current_exe().expect("current_exe"), &exe).expect("copy test binary");
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).expect("chmod exe");
        Self {
            home,
            bin,
            tmp,
            exe,
        }
    }
    fn repo(&self, name: &str) -> PathBuf {
        let dir = self.home.path().join(name);
        fs::create_dir_all(&dir).expect("repo dir");
        let dir = dir.canonicalize().expect("canonicalize repo");
        self.git(&dir, &["init", "-q", "-b", "main"]);
        self.git(&dir, &["config", "user.name", "T"]);
        self.git(&dir, &["config", "user.email", "t@example.invalid"]);
        fs::write(dir.join("a.txt"), "one\n").expect("write");
        self.git(&dir, &["add", "."]);
        self.git(&dir, &["commit", "-q", "-m", "init"]);
        strip_group_world_write(&dir);
        dir
    }
    fn git(&self, dir: &Path, args: &[&str]) {
        let out = self.command(dir).args(args).output().expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn command(&self, cwd: &Path) -> Command {
        let mut c = Command::new(self.bin.join("git"));
        c.env_clear()
            .env("PATH", &self.bin)
            .env("HOME", self.home.path())
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(cwd);
        c
    }
    /// Run `tool` in a clean child; returns (code, stdout, stderr).
    fn run(&self, tool: &str, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(&self.exe)
            .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
            .env_clear()
            .env("PATH", &self.bin)
            .env("HOME", self.home.path())
            .env("TMPDIR", &self.tmp)
            .env("LC_ALL", "C")
            .env("OMS_CT_TOOL", tool)
            .env("OMS_CT_CWD", cwd)
            .env("OMS_CT_ARGS", args.join(&SEP.to_string()))
            .current_dir(cwd)
            .output()
            .expect("spawn child");
        let text = String::from_utf8_lossy(&out.stdout);
        let field = |k: &str| {
            text.lines()
                .find_map(|l| l.find(k).map(|i| &l[i + k.len()..]))
                .unwrap_or_else(|| {
                    panic!(
                        "child missing {k}: {text}\n{}",
                        String::from_utf8_lossy(&out.stderr)
                    )
                })
                .to_owned()
        };
        let unescape = |s: String| {
            let mut out = String::new();
            let mut it = s.chars();
            while let Some(c) = it.next() {
                match (c, it.clone().next()) {
                    ('\\', Some('n')) => {
                        out.push('\n');
                        it.next();
                    }
                    ('\\', Some('\\')) => {
                        out.push('\\');
                        it.next();
                    }
                    _ => out.push(c),
                }
            }
            out
        };
        (
            field("CT_CODE=").parse().expect("code"),
            unescape(field("CT_OUT=")),
            unescape(field("CT_ERR=")),
        )
    }
}

/// Entry point for re-executed children; a no-op in the normal test run.
#[test]
fn child_entry() {
    let Some(tool) = std::env::var_os("OMS_CT_TOOL") else {
        return;
    };
    let cwd = PathBuf::from(std::env::var_os("OMS_CT_CWD").expect("cwd"));
    let raw = std::env::var("OMS_CT_ARGS").unwrap_or_default();
    let args: Vec<OsString> = if raw.is_empty() {
        vec![]
    } else {
        raw.split(SEP).map(OsString::from).collect()
    };
    let flag = AtomicBool::new(false);
    let (code, out, err) = run(tool.to_str().unwrap(), &args, &cwd, &flag);
    let esc = |b: Vec<u8>| {
        String::from_utf8_lossy(&b)
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
    };
    println!("CT_CODE={code}");
    println!("CT_OUT={}", esc(out));
    println!("CT_ERR={}", esc(err));
}

fn inproc(tool: &str, args: &[&str]) -> (i32, String, String) {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let flag = AtomicBool::new(false);
    let (c, o, e) = run(tool, &args, Path::new("."), &flag);
    (
        c,
        String::from_utf8_lossy(&o).into(),
        String::from_utf8_lossy(&e).into(),
    )
}

#[test]
fn owns_exactly_the_twelve_tools() {
    assert_eq!(TOOLS.len(), 12);
    for t in TOOLS {
        assert!(owns(t, &[]), "{t}");
        assert!(
            usage(t).is_some_and(|u| u.starts_with("Usage: oms ")),
            "{t}"
        );
    }
    for t in ["", "graph", "plan", "Land", "land "] {
        assert!(!owns(t, &[]), "{t:?}");
        assert!(usage(t).is_none());
    }
    let (code, out, err) = inproc("graph", &[]);
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(err.contains("unknown command: graph"), "{err}");
}

#[test]
fn help_prints_usage_for_every_tool() {
    for t in TOOLS {
        for flag in ["--help", "-h"] {
            let (code, out, err) = inproc(t, &[flag]);
            assert_eq!(code, 0, "{t} {flag}: {err}");
            assert_eq!(out, usage(t).unwrap());
        }
    }
    // help short-circuits before later (even invalid) arguments
    let (code, out, _) = inproc("land", &["--help", "--bogus"]);
    assert_eq!(code, 0);
    assert!(out.contains("oms land status"));
}

#[test]
fn parse_errors_exit_two() {
    for (tool, args, needle) in [
        ("land", vec!["--bogus"], "unknown argument: --bogus"),
        ("land", vec!["-x"], "unknown argument: -x"),
        ("land", vec!["--repo"], "--repo requires a value"),
        (
            "land",
            vec!["--repo", ""],
            "--repo requires a nonempty value",
        ),
        ("cleanup", vec!["--repo", "x"], "unknown argument: --repo"),
        (
            "report",
            vec!["--from-failure"],
            "--from-failure requires a value",
        ),
        ("ci-status", vec!["--limit"], "--limit requires a value"),
    ] {
        let (code, out, err) = inproc(tool, &args);
        assert_eq!(code, 2, "{tool} {args:?}: {err}");
        assert!(out.is_empty());
        assert!(
            err.starts_with("error: ") && err.contains(needle),
            "{tool} {args:?}: {err}"
        );
    }
}

#[test]
fn interrupted_flag_exits_130() {
    let flag = AtomicBool::new(true);
    let (code, out, err) = run("land", &[], Path::new("."), &flag);
    assert_eq!(code, 130);
    assert!(out.is_empty());
    assert!(String::from_utf8_lossy(&err).contains("interrupted"));
}

const NO_SERVICE: &str = "service unavailable";
const NO_PROOF: &str = "private-group account proof service unavailable";

fn assert_refused(r: &(i32, String, String), needle: &str) {
    assert_eq!(r.0, 2, "{r:?}");
    assert!(r.1.is_empty(), "{r:?}");
    assert!(r.2.starts_with("error: ") && r.2.contains(needle), "{r:?}");
}

#[test]
fn read_only_commands_run_without_python_or_services() {
    let iso = Isolated::new();
    let repo = iso.repo("ro");
    for (tool, args) in [
        ("scratch-worktree", vec!["list"]),
        ("checkpoint", vec!["list"]),
        ("candidate", vec!["table"]),
        ("change-guard", vec!["status"]),
    ] {
        let (code, _, err) = iso.run(tool, &repo, &args);
        assert_eq!((code, err.as_str()), (0, ""), "{tool}");
    }
    let (code, out, _) = iso.run("change-guard", &repo, &["status"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("change-guard: inactive"), "{out}");
    let (code, out, _) = iso.run("candidate", &repo, &["table"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("no candidates"), "{out}");
}

#[test]
fn land_status_reports_free_lock_with_exit_one() {
    let iso = Isolated::new();
    let repo = iso.repo("land");
    let (code, out, _) = iso.run("land", &repo, &["status"]);
    assert_eq!(code, 1);
    assert!(out.starts_with("land lock: free"), "{out}");
    let (code, out, _) = iso.run("land", &repo, &["status", "--json"]);
    assert_eq!(code, 1);
    assert!(out.contains("\"active\": false"), "{out}");
    assert_refused(&iso.run("land", &repo, &["bogus"]), "unknown land action");
}

/// A tracked change as a patch file inside the repo, so admission gets past
/// "missing" and "empty" validation and reaches the `Services` boundary.
fn real_patch(iso: &Isolated, repo: &Path) -> &'static str {
    fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    let out = iso
        .command(repo)
        .args(["diff", "--binary"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!out.stdout.is_empty());
    fs::write(repo.join("change.patch"), &out.stdout).unwrap();
    iso.git(repo, &["checkout", "-q", "--", "a.txt"]);
    strip_group_world_write(repo);
    "change.patch"
}

#[test]
fn services_gated_commands_fail_closed() {
    let iso = Isolated::new();
    let repo = iso.repo("auth");
    assert_refused(
        &iso.run("draft-pr", &repo, &["prepare"]),
        "publication fence",
    );
    for tool in ["cleanup", "patch-land", "sync-shared"] {
        assert_refused(&iso.run(tool, &repo, &[]), NO_SERVICE);
    }
    assert_refused(&iso.run("cleanup", &repo, &["--dry-run"]), NO_SERVICE);
    // patch-admit validates the patch before it needs an admission context
    assert_refused(
        &iso.run("patch-admit", &repo, &["--patch", "missing.patch"]),
        "No such file",
    );
    fs::write(repo.join("empty.patch"), "").unwrap();
    strip_group_world_write(&repo);
    assert_refused(
        &iso.run("patch-admit", &repo, &["--patch", "empty.patch"]),
        "patch is empty",
    );
    let patch = real_patch(&iso, &repo);
    // each Services boundary is reached only after the patch itself is admitted
    assert_refused(
        &iso.run(
            "patch-admit",
            &repo,
            &["--patch", patch, "--plan-task", "t1"],
        ),
        "native plan service unavailable",
    );
    assert_refused(
        &iso.run("patch-admit", &repo, &["--patch", patch, "--covers", "c1"]),
        "native runtime criterion coverage service unavailable",
    );
    // a verifier that cannot be supervised is a rejection, not an error
    let (code, out, _) = iso.run(
        "patch-admit",
        &repo,
        &["--patch", patch, "--verify", "true"],
    );
    assert_eq!(code, 1, "{out}");
    assert!(out.starts_with("REJECT"), "{out}");
    let rep = iso.repo("report-failure");
    fs::write(rep.join("VERSION"), "1.2.3\n").unwrap();
    strip_group_world_write(&rep);
    iso.git(
        &rep,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/name.git",
        ],
    );
    assert_refused(
        &iso.run(
            "report",
            &rep,
            &["bug", "--from-failure", "0123456789abcdef", "--dry-run"],
        ),
        "allowlisted failure report service unavailable",
    );
    // no origin remote: land stops at its first Git observation
    assert_refused(&iso.run("land", &repo, &[]), "No such remote 'origin'");
}

#[test]
fn patch_admit_runs_locally_without_services() {
    let iso = Isolated::new();
    let repo = iso.repo("admit");
    let patch = real_patch(&iso, &repo);
    // the private capture lives outside the repository and must be readable
    let (code, out, err) = iso.run("patch-admit", &repo, &["--patch", patch]);
    assert_eq!((code, out.as_str()), (0, "ADMIT\n"), "{err}");
}

#[test]
fn checkpoint_and_scratch_worktree_are_local() {
    let iso = Isolated::new();
    let repo = iso.repo("local");
    let (code, out, err) = iso.run("checkpoint", &repo, &["create"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("checkpoint: created cp-"), "{out}");
    let id = out.trim().rsplit(' ').next().unwrap().to_owned();
    let (code, out, _) = iso.run("checkpoint", &repo, &["list"]);
    assert_eq!(code, 0);
    assert!(out.contains(&id), "{out}");
    // a live owner pid avoids the `ps` session-owner probe
    let pid = std::process::id().to_string();
    let (code, out, err) = iso.run("scratch-worktree", &repo, &["add", "--owner-pid", &pid]);
    assert_eq!(code, 0, "{err}");
    let worktree = PathBuf::from(out.trim());
    assert!(worktree.join("a.txt").is_file(), "{out}");
    let (code, out, _) = iso.run("scratch-worktree", &repo, &["list"]);
    assert_eq!(code, 0);
    assert!(out.contains(worktree.to_str().unwrap()), "{out}");
    // without --owner-pid the owner probe needs `ps`, absent from the PATH
    let (code, _, err) = iso.run("scratch-worktree", &repo, &["add"]);
    assert_eq!(code, 2);
    assert!(err.contains("launch failed"), "{err}");
}

#[test]
fn group_writable_git_dir_requires_account_proof() {
    let iso = Isolated::new();
    let repo = iso.repo("proof");
    fs::write(repo.join("e.patch"), "").unwrap();
    strip_group_world_write(&repo);
    // explicit mode, independent of the ambient umask
    fs::set_permissions(repo.join(".git"), fs::Permissions::from_mode(0o775)).unwrap();
    for (tool, args) in [
        ("land", vec![]),
        ("checkpoint", vec!["create"]),
        ("draft-pr", vec!["prepare"]),
        ("patch-admit", vec!["--patch", "e.patch"]),
    ] {
        assert_refused(&iso.run(tool, &repo, &args), NO_PROOF);
    }
    fs::set_permissions(repo.join(".git"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(iso.run("checkpoint", &repo, &["create"]).0, 0);
}

#[test]
fn ci_status_fails_closed_without_gh() {
    let iso = Isolated::new();
    let repo = iso.repo("ci");
    for args in [vec!["status"], vec!["history"], vec![]] {
        let (code, out, err) = iso.run("ci-status", &repo, &args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(out.is_empty());
        assert!(err.contains("launch failed"), "{err}");
    }
}

#[test]
fn report_dry_run_degrades_without_python() {
    let iso = Isolated::new();
    let repo = iso.repo("report");
    fs::write(repo.join("VERSION"), "1.2.3\n").unwrap();
    strip_group_world_write(&repo);
    iso.git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/name.git",
        ],
    );
    let base = ["bug", "--title", "t", "--message", "m"];
    let (code, out, err) = iso.run("report", &repo, &[&base[..], &["--dry-run"]].concat());
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("Title: t\n"), "{out}");
    assert!(out.contains("Python: retired\n"), "{out}");
    assert!(
        !out.contains("issues/new"),
        "dry run must not build a URL: {out}"
    );
    // without --yes: nothing is published, URL is printed, exit 1
    let (code, out, _) = iso.run("report", &repo, &base);
    assert_eq!(code, 1);
    assert!(out.contains("Not published: explicit --yes"), "{out}");
    assert!(
        out.contains("https://github.com/owner/name/issues/new?"),
        "{out}"
    );
    // --yes without gh: degrades to the URL, publishes nothing
    let (code, out, _) = iso.run("report", &repo, &[&base[..], &["--yes"]].concat());
    assert_eq!(code, 0);
    assert!(out.contains("gh unavailable or unauthenticated"), "{out}");
}

#[test]
fn report_rejects_malformed_input() {
    let iso = Isolated::new();
    let repo = iso.repo("report-bad");
    assert_refused(
        &iso.run("report", &repo, &[]),
        "report requires bug or feature",
    );
    assert_refused(
        &iso.run("report", &repo, &["bogus"]),
        "report requires bug or feature",
    );
    assert_refused(
        &iso.run("report", &repo, &["feature"]),
        "feature requires --title",
    );
    assert_refused(
        &iso.run(
            "report",
            &repo,
            &["bug", "--from-failure", "zzz", "--dry-run"],
        ),
        "malformed failure fingerprint",
    );
    assert_refused(
        &iso.run(
            "report",
            &repo,
            &["bug", "--from-failure", "0123456789abcde", "--dry-run"],
        ),
        "malformed failure fingerprint",
    );
    assert_refused(
        &iso.run(
            "report",
            &repo,
            &[
                "feature",
                "--from-failure",
                "0123456789abcdef",
                "--title",
                "t",
            ],
        ),
        "--from-failure requires bug",
    );
    assert_refused(
        &iso.run("report", &repo, &["bug", "--title", "a\nb", "--dry-run"]),
        "title must be a nonempty single line",
    );
    assert_refused(
        &iso.run(
            "report",
            &repo,
            &["bug", "--message", "see /var/log/x", "--dry-run"],
        ),
        "absolute path in user text",
    );
    // no origin remote: refused, not guessed
    let (code, _, err) = iso.run("report", &repo, &["bug", "--dry-run", "--message", "m"]);
    assert_eq!(code, 2);
    assert!(err.contains("origin"), "{err}");
}

#[test]
fn change_guard_lifecycle() {
    let iso = Isolated::new();
    let repo = iso.repo("guard");
    let (code, out, err) = iso.run("change-guard", &repo, &["--allow", "a.txt", "begin"]);
    assert_eq!(code, 0, "{err}");
    let (code, out2, _) = iso.run("change-guard", &repo, &["status"]);
    assert_eq!(code, 0);
    assert!(
        !out2.starts_with("change-guard: inactive"),
        "{out} / {out2}"
    );
    let (code, _, err) = iso.run("change-guard", &repo, &["end"]);
    assert_eq!(code, 0, "{err}");
    let (_, out3, _) = iso.run("change-guard", &repo, &["status"]);
    assert!(out3.starts_with("change-guard: inactive"), "{out3}");
}
