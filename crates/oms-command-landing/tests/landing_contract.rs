//! Contract tests for the landing command surface. Behavioural cases run in a
//! re-executed copy of this test binary with a scrubbed environment and a PATH
//! that holds only `git` and `bash`, so no `python3`, `OMS_*` or `GIT_*` state leaks in.
#![forbid(unsafe_code)]

use oms_command_landing::services::{Execution, PublicationGuard, Services};
use oms_command_landing::{TOOLS, V, obj, owns, run, run_with_services, usage};
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
        self.run_env(tool, cwd, args, &[])
    }
    /// Like `run`, with `extra` environment and local-process services (`TestServices`).
    fn run_with_services(
        &self,
        tool: &str,
        cwd: &Path,
        args: &[&str],
        extra: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut env = vec![("OMS_CT_SERVICES", "1")];
        env.extend_from_slice(extra);
        self.run_env(tool, cwd, args, &env)
    }
    fn run_env(
        &self,
        tool: &str,
        cwd: &Path,
        args: &[&str],
        extra: &[(&str, &str)],
    ) -> (i32, String, String) {
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
            .envs(extra.iter().copied())
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
    let (code, out, err) = if tool == "feedback-candidate" {
        let result = oms_command_landing::feedback::repair_candidate(
            &cwd,
            &cwd,
            args[0].to_str().unwrap(),
            args[1].to_str().unwrap(),
            &flag,
        )
        .and_then(|row| {
            oms_command_landing::feedback::revalidate_repair(&cwd, &row, &flag)?;
            row.json(false).map_err(oms_command_landing::Error::from)
        });
        match result {
            Ok(bytes) => (0, bytes, vec![]),
            Err(e) => (e.code, vec![], e.message.into_bytes()),
        }
    } else if let Some(log) = std::env::var_os("OMS_CT_LOG") {
        run_with_services(
            tool.to_str().unwrap(),
            &args,
            &cwd,
            &cwd,
            &flag,
            &RecordingServices(PathBuf::from(log)),
        )
    } else if std::env::var_os("OMS_CT_SERVICES").is_some() {
        run_with_services(
            tool.to_str().unwrap(),
            &args,
            &cwd,
            &cwd,
            &flag,
            &TestServices,
        )
    } else {
        run(tool.to_str().unwrap(), &args, &cwd, &flag)
    };
    let esc = |b: Vec<u8>| {
        String::from_utf8_lossy(&b)
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
    };
    println!("CT_CODE={code}");
    println!("CT_OUT={}", esc(out));
    println!("CT_ERR={}", esc(err));
}

/// Local stand-ins for the parent CLI's authorities: the gate really runs.
struct TestServices;
struct TestPlanServices;
struct NoGuard;
impl PublicationGuard for NoGuard {}
fn test_group_resolver(
    gid: u32,
) -> oms_command_plan::Result<oms_command_plan::services::GroupMembership> {
    let uid = rustix::process::getuid().as_raw();
    let explicit_member_uids = if std::env::var_os("OMS_CT_SHARED_GROUP").is_some() {
        vec![uid.wrapping_add(1)]
    } else {
        vec![]
    };
    Ok(oms_command_plan::services::GroupMembership {
        gid,
        explicit_member_uids,
        primary_member_uids: vec![uid],
    })
}
impl oms_command_plan::Services for TestPlanServices {
    fn group_membership_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        Some(test_group_resolver)
    }
}
impl Services for TestServices {
    fn plan(&self) -> Option<&dyn oms_command_plan::Services> {
        Some(&TestPlanServices)
    }
    fn supervise(
        &self,
        request: &oms_command_landing::services::VerifyRequest,
        _: &AtomicBool,
    ) -> oms_command_landing::Result<Execution> {
        let started = std::time::Instant::now();
        let out = Command::new(&request.argv[0])
            .args(&request.argv[1..])
            .current_dir(&request.cwd)
            .output()
            .expect("gate");
        Ok(Execution {
            code: out.status.code().unwrap_or(2),
            stdout: out.stdout,
            stderr: out.stderr,
            elapsed: started.elapsed(),
        })
    }
    fn publication_guard(
        &self,
        _: &Path,
        _: &AtomicBool,
    ) -> oms_command_landing::Result<Box<dyn PublicationGuard + '_>> {
        Ok(Box::new(NoGuard))
    }
    fn wait_siblings(
        &self,
        _: &Path,
        _: std::time::Duration,
        _: &AtomicBool,
    ) -> oms_command_landing::Result<()> {
        Ok(())
    }
    fn after_land(&self, _: &Path, _: &V, _: &AtomicBool) -> oms_command_landing::Result<V> {
        Ok(obj([("rc", 0usize.into())]))
    }
}

/// `TestServices` plus landing authorities that accept and append each call
/// (with the request fields under test) to a log, one line per call.
struct RecordingServices(PathBuf);
impl RecordingServices {
    fn log(&self, line: String) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }
}
impl Services for RecordingServices {
    fn plan(&self) -> Option<&dyn oms_command_plan::Services> {
        Some(&TestPlanServices)
    }
    fn supervise(
        &self,
        request: &oms_command_landing::services::VerifyRequest,
        interrupted: &AtomicBool,
    ) -> oms_command_landing::Result<Execution> {
        TestServices.supervise(request, interrupted)
    }
    fn publication_guard(
        &self,
        _: &Path,
        _: &AtomicBool,
    ) -> oms_command_landing::Result<Box<dyn PublicationGuard + '_>> {
        Ok(Box::new(NoGuard))
    }
    fn coverage(
        &self,
        _: &Path,
        ids: &[String],
        evidence: Option<&V>,
    ) -> oms_command_landing::Result<V> {
        let record = evidence.map_or(String::new(), |row| {
            format!(
                " covers={} status={}",
                row.get("covers").py(),
                row.get("status").py()
            )
        });
        self.log(format!("coverage {}{record}", ids.join(",")));
        Ok(obj([]))
    }
    fn landing_review(
        &self,
        request: &oms_command_landing::services::LandingRequest<'_>,
    ) -> oms_command_landing::Result<V> {
        self.log(format!("review frozen={}", request.frozen_patch.display()));
        Ok(obj([]))
    }
    fn panel_accept(
        &self,
        request: &oms_command_landing::services::LandingRequest<'_>,
        fence: &V,
    ) -> oms_command_landing::Result<()> {
        if fence.get("schema").py() != "1"
            || fence.get("patch_sha256").text() != Some(request.patch_sha256)
            || fence.get("base").text() != Some(request.base)
        {
            return Err(oms_command_landing::Error::new(2, "changed panel fence"));
        }
        self.log(format!("panel frozen={}", request.frozen_patch.display()));
        Ok(())
    }
    fn fence_landing(
        &self,
        request: &oms_command_landing::services::LandingRequest<'_>,
        intent: &V,
    ) -> oms_command_landing::Result<()> {
        self.log(format!(
            "fence frozen={} patch={} consent={}",
            request.frozen_patch.display(),
            intent.get("patch").py(),
            intent.get("verifier_change_consent").truth()
        ));
        Ok(())
    }
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
        (
            "land",
            vec!["--local-merge"],
            "--local-merge requires --from REF",
        ),
        (
            "land",
            vec!["--pr", "--local-merge", "--from", "x"],
            "--pr and --local-merge are mutually exclusive",
        ),
        (
            "land",
            vec!["--pr", "--merge-method", "octopus"],
            "--merge-method must be squash, merge or rebase",
        ),
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
fn landing_authorities_see_the_frozen_patch_and_recorded_coverage() {
    let iso = Isolated::new();
    let repo = iso.repo("frozen");
    let patch = real_patch(&iso, &repo);
    fs::write(repo.join(".git/info/exclude"), "change.patch\n.oms/\n").unwrap();
    let log = iso.tmp.join("services.log");
    let log_env = [("OMS_CT_LOG", log.to_str().unwrap())];
    let (code, _, err) = iso.run_env(
        "patch-admit",
        &repo,
        &["--patch", patch, "--covers", "c1"],
        &log_env,
    );
    assert_eq!(code, 0, "{err}");
    let base = iso
        .command(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let base = String::from_utf8(base.stdout).unwrap();
    let panel = obj([
        ("schema", 1usize.into()),
        ("base", base.trim().into()),
        (
            "patch_sha256",
            oms_command_plan::json::sha(fs::read(repo.join(patch)).unwrap()).into(),
        ),
    ]);
    let mut stale = panel.clone();
    stale.set("base", "changed");
    let stale = String::from_utf8(stale.json(false).unwrap()).unwrap();
    let (code, _, err) = iso.run_env(
        "patch-land",
        &repo,
        &["--patch", patch, "--panel-accept", &stale],
        &log_env,
    );
    assert_ne!(code, 0);
    assert!(err.contains("changed panel fence"), "{err}");
    assert_eq!(fs::read_to_string(repo.join("a.txt")).unwrap(), "one\n");
    let panel = String::from_utf8(panel.json(false).unwrap()).unwrap();
    let (code, _, err) = iso.run_env(
        "patch-land",
        &repo,
        &[
            "--patch",
            patch,
            "--allow-verifier-change",
            "--panel-accept",
            &panel,
        ],
        &log_env,
    );
    assert_eq!(code, 0, "{err}");
    let calls = fs::read_to_string(&log).unwrap();
    let calls: Vec<&str> = calls.lines().collect();
    assert_eq!(calls[0], "coverage c1");
    assert!(
        calls[1].starts_with("coverage c1 covers=['c1'] status="),
        "{calls:?}"
    );
    let landings = fs::read_to_string(repo.join(".oms/landings.jsonl")).unwrap();
    let intent = landings
        .lines()
        .map(|l| V::parse(l.as_bytes()).unwrap())
        .find(|r| r.get("event").text() == Some("intent"))
        .unwrap();
    let frozen = intent.get("patch").py();
    assert!(
        frozen.starts_with(&format!("{}/.oms/landing-patches/land-", repo.display())),
        "{frozen}"
    );
    // Review sees the path the intent records; the fence gets the consent.
    assert_eq!(calls[2], "panel frozen=");
    assert_eq!(calls[3], format!("review frozen={frozen}"));
    assert_eq!(
        calls[4],
        format!("fence frozen={frozen} patch={frozen} consent=true")
    );
    assert_eq!(calls[5], format!("panel frozen={frozen}"));
    assert_eq!(calls.len(), 6, "{calls:?}");
    assert!(Path::new(&frozen).is_file());
    assert!(intent.get("verifier_change_consent").truth());
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
    fs::set_permissions(&repo, fs::Permissions::from_mode(0o775)).unwrap();
    assert_refused(
        &iso.run_with_services(
            "checkpoint",
            &repo,
            &["create"],
            &[("OMS_CT_SHARED_GROUP", "1")],
        ),
        "unsafe",
    );
    let proven = iso.run_with_services("checkpoint", &repo, &["create"], &[]);
    assert_eq!(proven.0, 0, "{proven:?}");
    fs::set_permissions(&repo, fs::Permissions::from_mode(0o755)).unwrap();
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

struct FeedbackFixture {
    isolated: Isolated,
    repo: PathBuf,
    gh: PathBuf,
    head: String,
}
impl FeedbackFixture {
    fn new() -> Self {
        let isolated = Isolated::new();
        let repo = isolated.repo("feedback");
        isolated.git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/project.git",
            ],
        );
        let head = String::from_utf8(
            isolated
                .command(&repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let gh = isolated.bin.join("gh");
        fs::write(
            &gh,
            r#"#!/bin/bash
case "$1 $2" in
  'run list') printf '%s\n' "$OMS_CT_FEEDBACK_RUNS";;
  'pr list')
    [[ "$OMS_CT_FEEDBACK_PULLS" != null ]] || { printf '%s\n' 'private feedback transport detail' >&2; exit 2; }
    printf '%s\n' "$OMS_CT_FEEDBACK_PULLS";;
  'api --hostname')
    [[ "${@: -1}" == *'/comments?sort=updated&direction=desc&per_page=50' ]] || exit 2
    [[ "$OMS_CT_FEEDBACK_COMMENTS" != null ]] || { printf '%s\n' 'private feedback transport detail' >&2; exit 2; }
    printf '%s\n' "$OMS_CT_FEEDBACK_COMMENTS";;
  'api --method')
    case "${@: -1}" in
      'repos/example/project/pulls/19') printf '{"head":{"sha":"%s"}}\n' "$OMS_CT_FEEDBACK_HEAD";;
      'repos/example/project/pulls/19/comments?sort=updated&direction=desc&per_page=100') printf '%s\n' "$OMS_CT_FEEDBACK_COMMENTS";;
      *) exit 2;;
    esac;;
  *) exit 2;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            isolated,
            repo,
            gh,
            head,
        }
    }
    fn run(&self, action: &str, attempt: usize, head: &str, comments: &V, conclusion: &str) {
        let result = self.observe(action, attempt, head, comments, conclusion, "CI");
        assert!(result.0 <= 1 && result.2.is_empty(), "{result:?}");
    }
    fn runs(&self, attempt: usize, conclusion: &str, workflow: &str) -> V {
        V::Arr(vec![obj([
            ("databaseId", 73usize.into()),
            ("attempt", attempt.into()),
            ("headSha", self.head.clone().into()),
            ("status", "completed".into()),
            ("conclusion", conclusion.into()),
            ("workflowName", workflow.into()),
        ])])
    }
    fn observe(
        &self,
        action: &str,
        attempt: usize,
        head: &str,
        comments: &V,
        conclusion: &str,
        workflow: &str,
    ) -> (i32, String, String) {
        let runs = self.runs(attempt, conclusion, workflow);
        let pulls = V::Arr(vec![obj([
            ("number", 19usize.into()),
            ("headRefOid", head.into()),
        ])]);
        self.poll(action, &runs, &pulls, comments)
    }
    fn poll(&self, action: &str, runs: &V, pulls: &V, comments: &V) -> (i32, String, String) {
        let encode = |v: &V| String::from_utf8(v.json(false).unwrap()).unwrap();
        self.isolated.run_env(
            "ci-status",
            &self.repo,
            &[action],
            &[
                ("OMS_GH_BIN", self.gh.to_str().unwrap()),
                ("OMS_CI_TICK_INTERVAL", "0"),
                ("OMS_CT_FEEDBACK_RUNS", &encode(runs)),
                ("OMS_CT_FEEDBACK_PULLS", &encode(pulls)),
                ("OMS_CT_FEEDBACK_COMMENTS", &encode(comments)),
            ],
        )
    }
    fn comments(&self, body: &str, updated: &str) -> V {
        V::Arr(vec![obj([
            ("id", 42usize.into()),
            ("commit_id", self.head.clone().into()),
            ("updated_at", updated.into()),
            ("body", body.into()),
            ("path", "src/lib.rs".into()),
            ("line", 5usize.into()),
            ("user", obj([("login", "reviewer".into())])),
        ])])
    }
    fn candidate(&self, event: &str, payloads: &[(&str, V)]) -> (i32, String, String) {
        fs::write(&self.gh, r#"#!/bin/bash
case "${@: -1}" in
  'repos/example/project/pulls/19') printf '%s\n' "$OMS_CT_PULL";;
  'repos/example/project/pulls/comments/42') printf '%s\n' "$OMS_CT_COMMENT";;
  'repos/example/project/actions/runs/73') printf '%s\n' "$OMS_CT_RUN";;
  'repos/example/project/actions/runs/73/attempts/1/jobs?per_page=100') printf '%s\n' "$OMS_CT_JOBS";;
  'repos/example/project/actions/workflows/7/runs?'*) printf '%s\n' "$OMS_CT_BASE_RUNS";;
  'repos/example/project/actions/runs/74/attempts/1/jobs?per_page=100') printf '%s\n' "$OMS_CT_BASE_JOBS";;
  *) exit 2;;
esac
"#).unwrap();
        let mut env = payloads
            .iter()
            .map(|(name, row)| {
                (
                    (*name).to_owned(),
                    String::from_utf8(row.json(false).unwrap()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        env.push(("OMS_GH_BIN".into(), self.gh.to_str().unwrap().into()));
        self.isolated.run_env(
            "feedback-candidate",
            &self.repo,
            &[event, &self.head],
            &env.iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect::<Vec<_>>(),
        )
    }
    fn draft_comments(&self, comments: &V, json: bool) -> (i32, String, String) {
        let path = self.repo.join(".oms/publish/feedback.json");
        if !path.exists() {
            let tree = String::from_utf8(
                self.isolated
                    .command(&self.repo)
                    .args(["rev-parse", "HEAD^{tree}"])
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap();
            let mut intent = obj([
                ("schema", 1usize.into()),
                ("kind", "draft-pr-intent".into()),
                ("phase", "complete".into()),
                ("created_at", "2026-01-01T00:00:00Z".into()),
                ("updated_at", "2026-01-01T00:00:00Z".into()),
                (
                    "repo_id",
                    oms_reference::digest(self.repo.as_os_str().as_encoded_bytes()).into(),
                ),
                ("head_sha", self.head.clone().into()),
                ("tree_sha", tree.trim().into()),
                ("base_sha", self.head.clone().into()),
                ("branch", "main".into()),
                ("base", "main".into()),
                ("remote", "origin".into()),
                ("repo_slug", "example/project".into()),
                ("fetch_url", "https://github.com/example/project.git".into()),
                ("push_url", "https://github.com/example/project.git".into()),
                ("viewer_login", "reviewer".into()),
                ("spec_sha256", "".into()),
                ("draft", true.into()),
                ("create_only", true.into()),
                ("merge", false.into()),
                ("release", false.into()),
                (
                    "pr_url",
                    "https://github.com/example/project/pull/19".into(),
                ),
                ("pr_number", 19usize.into()),
                ("push_attempted", true.into()),
            ]);
            for key in ["verify", "title", "body"] {
                intent.set(key, "fixture");
                intent.set(&format!("{key}_sha256"), oms_reference::digest(b"fixture"));
            }
            oms_command_landing::review::validate_intent(&intent, &self.repo).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, intent.json(false).unwrap()).unwrap();
            strip_group_world_write(path.parent().unwrap());
        }
        let mut args = vec!["comments", "--intent", path.to_str().unwrap()];
        if json {
            args.push("--json");
        }
        self.isolated.run_with_services(
            "draft-pr",
            &self.repo,
            &args,
            &[
                ("OMS_GH_BIN", self.gh.to_str().unwrap()),
                ("OMS_CT_FEEDBACK_HEAD", &self.head),
                (
                    "OMS_CT_FEEDBACK_COMMENTS",
                    &String::from_utf8(comments.json(false).unwrap()).unwrap(),
                ),
            ],
        )
    }
}

#[test]
fn ci_feedback_deduplicates_polls_and_observes_same_head_reruns() {
    let f = FeedbackFixture::new();
    let empty = V::Arr(vec![]);
    f.run("record", 1, &f.head, &empty, "failure");
    f.run("tick", 1, &f.head, &empty, "failure");
    let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("classification").text(), Some("unknown"));
    let first = rows[0].get("event_id").clone();
    f.run("tick", 2, &f.head, &empty, "failure");
    let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().filter(|r| r.get("event_id") == &first).count(),
        1
    );
    let history = fs::read_to_string(f.repo.join(".oms/ci.jsonl")).unwrap();
    assert_eq!(history.lines().count(), 2);
    let (code, out, err) = f
        .isolated
        .run("ci-status", &f.repo, &["feedback", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(V::parse(out.as_bytes()).unwrap().array().len(), 2);
    let (code, out, err) = f.isolated.run(
        "ci-status",
        &f.repo,
        &["feedback", "--json", "--limit", "1"],
    );
    assert_eq!(code, 0, "{err}");
    let recent = V::parse(out.as_bytes()).unwrap();
    assert_eq!(recent.array().len(), 1);
    assert_eq!(recent.array()[0].get("source_revision"), &2usize.into());
}

#[test]
fn pr_feedback_tracks_edits_beyond_the_excerpt_and_stale_heads() {
    let f = FeedbackFixture::new();
    let original = format!("{} original", "x".repeat(700));
    let edited = format!("{} revised", "x".repeat(700));
    f.run(
        "record",
        1,
        &f.head,
        &f.comments(&original, "2026-01-01T00:00:00Z"),
        "success",
    );
    f.run(
        "tick",
        1,
        &f.head,
        &f.comments(&original, "2026-01-01T00:00:00Z"),
        "success",
    );
    let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
    assert_eq!(rows.len(), 2);
    f.run(
        "tick",
        1,
        &f.head,
        &f.comments(&edited, "2026-01-01T00:01:00Z"),
        "success",
    );
    let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
    let comments: Vec<_> = rows
        .iter()
        .filter(|r| r.get("source").text() == Some("pr-review-comment"))
        .collect();
    assert_eq!(comments.len(), 2);
    assert_ne!(comments[0].get("event_id"), comments[1].get("event_id"));
    assert_eq!(
        comments[0].get("evidence").get("body"),
        comments[1].get("evidence").get("body")
    );
    let moved = "a".repeat(40);
    f.run(
        "tick",
        1,
        &moved,
        &f.comments(&edited, "2026-01-01T00:01:00Z"),
        "success",
    );
    let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
    assert!(
        rows.iter()
            .any(|r| r.get("source").text() == Some("pr-review-comment")
                && r.get("classification").text() == Some("stale_head"))
    );
    assert!(
        rows.iter()
            .all(|r| r.get("trust").text() == Some("untrusted"))
    );
}

#[test]
fn ci_feedback_preserves_nonrepairable_conclusions_and_unknown_identity() {
    let f = FeedbackFixture::new();
    for (attempt, conclusion, classification) in [
        (1, "cancelled", "cancelled"),
        (2, "startup_failure", "environment_failure"),
        (3, "timed_out", "unknown"),
        (0, "failure", "unknown"),
    ] {
        f.run("record", attempt, &f.head, &V::Arr(vec![]), conclusion);
        let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
        assert!(
            rows.iter().any(
                |r| r.get("evidence").get("conclusion").text() == Some(conclusion)
                    && r.get("classification").text() == Some(classification)
            ),
            "{rows:?}"
        );
    }
}

#[test]
fn feedback_refusal_preserves_ci_and_comment_views() {
    let f = FeedbackFixture::new();
    f.run("record", 1, &f.head, &V::Arr(vec![]), "failure");
    let path = f.repo.join(oms_command_landing::feedback::EVENTS);
    let original = fs::read_to_string(&path).unwrap();
    let original_row = V::parse(original.trim().as_bytes()).unwrap();
    let mut ledgers = vec![];
    for (field, changed) in [
        ("schema", V::from(99usize)),
        ("event_id", "a".repeat(64).into()),
        ("classification", "success".into()),
    ] {
        let mut row = original_row.clone();
        row.set(field, changed);
        let mut corrupt = row.json(false).unwrap();
        corrupt.push(b'\n');
        ledgers.push((corrupt, true));
    }
    // Whitespace fills the byte bound without making the existing record invalid.
    let mut full = original.as_bytes().to_vec();
    full.resize(16 * 1024 * 1024 - 1, b' ');
    full.push(b'\n');
    ledgers.push((full, false));
    let comments = f.comments("check this source", "2026-01-01T00:00:00Z");
    let warning = "warning: feedback observation unavailable (code 2); durable feedback may be incomplete or stale\n";
    let mut attempt = 1;
    let mut packet_path = None;
    for (ledger, corrupt) in ledgers {
        fs::write(&path, &ledger).unwrap();
        assert_eq!(
            oms_command_landing::feedback::observations(&f.repo).is_err(),
            corrupt
        );
        let read = f
            .isolated
            .run("ci-status", &f.repo, &["feedback", "--json"]);
        assert_eq!(read.0, if corrupt { 2 } else { 0 }, "{read:?}");
        if corrupt {
            assert!(read.1.is_empty(), "{read:?}");
        }
        for (action, conclusion, code) in [("record", "success", 0), ("tick", "failure", 1)] {
            attempt += 1;
            let result = f.observe(action, attempt, &f.head, &V::Arr(vec![]), conclusion, "CI");
            assert_eq!(result.0, code, "{result:?}");
            assert_eq!(result.2, warning);
            if action == "tick" {
                assert!(result.1.is_empty(), "{result:?}");
            } else {
                assert!(result.1.contains("completed success"), "{result:?}");
            }
            let history = fs::read_to_string(f.repo.join(".oms/ci.jsonl")).unwrap();
            let current = V::parse(history.lines().last().unwrap().as_bytes()).unwrap();
            assert_eq!(current.get("attempt"), &attempt.into());
            assert_eq!(current.get("conclusion").text(), Some(conclusion));
            assert_eq!(fs::read(&path).unwrap(), ledger);
        }
        let result = f.draft_comments(&comments, true);
        assert_eq!(result.0, 0, "{result:?}");
        assert_eq!(result.2, warning);
        let view = V::parse(result.1.as_bytes()).unwrap();
        assert_eq!(
            view.get("feedback_observation").get("status").text(),
            Some("unavailable")
        );
        assert_eq!(
            view.get("feedback_observation").get("error_code"),
            &2usize.into()
        );
        assert_eq!(
            view.get("comments").array()[0].get("body").text(),
            Some("check this source")
        );
        let packet = view.get("packet").text().unwrap();
        let saved = V::parse(&fs::read(f.repo.join(packet)).unwrap()).unwrap();
        assert_eq!(saved.get("comments"), view.get("comments"));
        assert!(!saved.has("feedback_observation"));
        if let Some(previous) = &packet_path {
            assert_eq!(packet, previous);
        }
        packet_path = Some(packet.to_owned());
        let text = f.draft_comments(&comments, false);
        assert_eq!(text.0, 0, "{text:?}");
        assert!(text.1.contains("1 untrusted review comment(s)"), "{text:?}");
        assert_eq!(text.2, warning);
        assert_eq!(fs::read(&path).unwrap(), ledger);
    }
    fs::write(&path, original).unwrap();
    for added in [1usize, 0] {
        let result = f.draft_comments(&comments, true);
        assert_eq!(result.0, 0, "{result:?}");
        assert!(result.2.is_empty(), "{result:?}");
        let view = V::parse(result.1.as_bytes()).unwrap();
        assert_eq!(
            view.get("feedback_observation").get("status").text(),
            Some("recorded")
        );
        assert_eq!(view.get("feedback_observation").get("added"), &added.into());
        assert_eq!(view.get("packet").text(), packet_path.as_deref());
    }
}

#[test]
fn feedback_poll_errors_are_explicit_and_preserve_available_ci_evidence() {
    let f = FeedbackFixture::new();
    let pull = V::Arr(vec![obj([
        ("number", 19usize.into()),
        ("headRefOid", f.head.clone().into()),
    ])]);
    for (index, (pulls, comments)) in [
        (V::Null, V::Arr(vec![])),
        (obj([]), V::Arr(vec![])),
        (pull, V::Null),
    ]
    .into_iter()
    .enumerate()
    {
        let result = f.poll(
            "record",
            &f.runs(index + 1, "success", "CI"),
            &pulls,
            &comments,
        );
        assert_eq!(result.0, 0, "{result:?}");
        assert!(result.1.contains("completed success"), "{result:?}");
        assert_eq!(
            result.2,
            "warning: feedback observation unavailable (code 2); durable feedback may be incomplete or stale\n"
        );
        let history = fs::read_to_string(f.repo.join(".oms/ci.jsonl")).unwrap();
        assert_eq!(history.lines().count(), index + 1);
        let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
        assert_eq!(rows.len(), index + 1);
        assert!(
            rows.iter()
                .all(|row| row.get("source").text() == Some("ci-run"))
        );
    }
}

#[test]
fn feedback_withholds_sensitive_workflow_metadata() {
    let f = FeedbackFixture::new();
    let sensitive = ["gh", "p_", &"a".repeat(36)].concat();
    let result = f.observe("record", 1, &f.head, &V::Arr(vec![]), "failure", &sensitive);
    assert_eq!(result.0, 1, "{result:?}");
    let raw = fs::read_to_string(f.repo.join(oms_command_landing::feedback::EVENTS)).unwrap();
    assert!(!raw.contains(&sensitive));
    let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
    assert_eq!(rows[0].get("classification").text(), Some("unknown"));
    assert_eq!(rows[0].get("evidence").get("withheld"), &true.into());
    f.isolated.git(&f.repo, &["branch", "-m", &sensitive]);
    f.run("record", 2, &f.head, &V::Arr(vec![]), "failure");
    let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
    assert_eq!(rows[0].get("branch").text(), Some(""));
    let raw = fs::read_to_string(f.repo.join(oms_command_landing::feedback::EVENTS)).unwrap();
    assert!(!raw.contains(&sensitive));
}

#[test]
fn feedback_candidate_revalidates_exact_pr_run_attempt_and_base_jobs() {
    let f = FeedbackFixture::new();
    f.run("record", 1, &f.head, &V::Arr(vec![]), "failure");
    let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
    let event = rows[0].get("event_id").text().unwrap();
    let base = "b".repeat(40);
    let remote_run = |id: usize, sha: &str, conclusion: &str, attempt: usize| {
        obj([
            ("id", id.into()),
            ("run_attempt", attempt.into()),
            ("head_sha", sha.into()),
            ("head_branch", "main".into()),
            ("repository", obj([("full_name", "example/project".into())])),
            ("workflow_id", 7usize.into()),
            ("status", "completed".into()),
            ("conclusion", conclusion.into()),
        ])
    };
    let jobs = |id: usize, sha: &str, result: &str, name: &str| {
        obj([
            ("total_count", 1usize.into()),
            (
                "jobs",
                V::Arr(vec![obj([
                    ("run_id", id.into()),
                    ("head_sha", sha.into()),
                    ("status", "completed".into()),
                    ("conclusion", result.into()),
                    ("name", name.into()),
                    (
                        "steps",
                        V::Arr(vec![obj([
                            ("name", "verify".into()),
                            ("conclusion", result.into()),
                        ])]),
                    ),
                ])]),
            ),
        ])
    };
    for (conclusion, baseline, name, moved, attempt, expected) in [
        ("failure", "success", "test", false, 1, "actionable_failure"),
        ("failure", "failure", "test", false, 1, "base_failure"),
        ("failure", "success", "other", false, 1, "unknown"),
        ("cancelled", "success", "test", false, 1, "cancelled"),
        (
            "startup_failure",
            "success",
            "test",
            false,
            1,
            "environment_failure",
        ),
        ("failure", "success", "test", true, 1, "head moved"),
        (
            "failure",
            "success",
            "test",
            false,
            2,
            "identity is unknown",
        ),
    ] {
        let pull = obj([
            ("number", 19usize.into()),
            ("state", "open".into()),
            (
                "head",
                obj([
                    (
                        "sha",
                        if moved { base.clone() } else { f.head.clone() }.into(),
                    ),
                    ("ref", "main".into()),
                ]),
            ),
            (
                "base",
                obj([
                    ("sha", base.clone().into()),
                    ("repo", obj([("full_name", "example/project".into())])),
                ]),
            ),
        ]);
        let result = f.candidate(
            event,
            &[
                ("OMS_CT_PULL", pull),
                ("OMS_CT_RUN", remote_run(73, &f.head, conclusion, attempt)),
                ("OMS_CT_JOBS", jobs(73, &f.head, "failure", "test")),
                (
                    "OMS_CT_BASE_RUNS",
                    obj([(
                        "workflow_runs",
                        V::Arr(vec![remote_run(74, &base, baseline, 1)]),
                    )]),
                ),
                ("OMS_CT_BASE_JOBS", jobs(74, &base, baseline, name)),
            ],
        );
        if expected == "actionable_failure" {
            assert_eq!(result.0, 0, "{result:?}");
            assert_eq!(
                V::parse(result.1.as_bytes())
                    .unwrap()
                    .get("classification")
                    .text(),
                Some(expected)
            );
        } else {
            assert_eq!(result.0, 2, "{expected}: {result:?}");
            assert!(result.2.contains(expected), "{expected}: {result:?}");
        }
    }
}

#[test]
fn feedback_candidate_revalidates_comment_revision() {
    let f = FeedbackFixture::new();
    let comments = f.comments("Review this condition", "2026-01-01T00:00:00Z");
    f.run("record", 1, &f.head, &comments, "success");
    let rows = oms_command_landing::feedback::observations(&f.repo).unwrap();
    let event = rows
        .iter()
        .find(|r| r.get("source").text() == Some("pr-review-comment"))
        .unwrap()
        .get("event_id")
        .text()
        .unwrap();
    let pull = obj([
        ("number", 19usize.into()),
        ("state", "open".into()),
        ("head", obj([("sha", f.head.clone().into())])),
        (
            "base",
            obj([("repo", obj([("full_name", "example/project".into())]))]),
        ),
    ]);
    let mut comment = comments.array()[0].clone();
    comment.set(
        "pull_request_url",
        "https://api.github.com/repos/example/project/pulls/19",
    );
    let result = f.candidate(
        event,
        &[
            ("OMS_CT_PULL", pull.clone()),
            ("OMS_CT_COMMENT", comment.clone()),
        ],
    );
    assert_eq!(result.0, 0, "{result:?}");
    comment.set("body", "Edited after the task was reviewed");
    let result = f.candidate(event, &[("OMS_CT_PULL", pull), ("OMS_CT_COMMENT", comment)]);
    assert_eq!(result.0, 2, "{result:?}");
    assert!(result.2.contains("edited or moved"), "{result:?}");
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

impl Isolated {
    fn git_out(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.command(dir).args(args).output().expect("git");
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
    /// Work repo whose `origin` is a local bare repository holding `main`.
    fn with_remote(&self, name: &str) -> (PathBuf, PathBuf) {
        let repo = self.repo(name);
        let bare = self.home.path().join(format!("{name}-remote.git"));
        self.git(
            self.home.path(),
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        self.git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        self.git(&repo, &["push", "-q", "origin", "main"]);
        (repo, bare)
    }
    fn commit_file(&self, repo: &Path, file: &str) -> String {
        fs::write(repo.join(file), format!("{file}\n")).expect("write");
        self.git(repo, &["add", file]);
        self.git(repo, &["commit", "-q", "-m", &format!("add {file}")]);
        strip_group_world_write(repo);
        self.git_out(repo, &["rev-parse", "HEAD"])
    }
    /// Fake `gh`: logs argv, answers JSON queries from the bare remote, and
    /// reports CI as `ci` for the exact `--commit` it is asked about.
    fn fake_gh(&self, bare: &Path, ci: &str) -> (PathBuf, PathBuf) {
        let log = self.home.path().join("gh.log");
        let state = self.home.path().join("gh.pr");
        let script = self.home.path().join("fake-gh");
        let body = format!(
            r#"#!{bash}
printf '%s\n' "$*" >> '{log}'
head=; commit=; event=
args=("$@")
for ((i = 0; i < ${{#args[@]}}; i++)); do
  case "${{args[i]}}" in
    --head) head=${{args[i+1]}} ;;
    --commit) commit=${{args[i+1]}} ;;
    --event) event=${{args[i+1]}} ;;
  esac
done
case "$1 $2" in
  "pr list")
    if [ -f '{state}' ]; then
      tip=$(git --git-dir='{bare}' rev-parse "refs/heads/$head")
      printf '[{{"number":7,"url":"https://example.invalid/pull/7","headRefOid":"%s"}}]\n' "$tip"
    else echo '[]'; fi ;;
  "pr create") : > '{state}'; echo https://example.invalid/pull/7 ;;
  "pr merge") ;;
  "pr view") printf '{{"state":"MERGED","mergeCommit":{{"oid":"%s"}}}}\n' "$(git --git-dir='{bare}' rev-parse refs/heads/main)" ;;
  "run list")
    if [ "$event" = pull_request ]; then
      printf '[{{"databaseId":99,"status":"completed","conclusion":"{ci}","headSha":"%s"}}]\n' "$commit"
    else echo '[]'; fi ;;
  *) exit 1 ;;
esac
"#,
            bash = which("bash").display(),
            log = log.display(),
            state = state.display(),
            bare = bare.display(),
        );
        fs::write(&script, body).expect("fake gh");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("chmod");
        (script, log)
    }
}

#[test]
fn land_pr_pushes_feature_branch_and_merges_after_exact_sha_ci() {
    let iso = Isolated::new();
    let (repo, bare) = iso.with_remote("pr");
    iso.git(&repo, &["checkout", "-q", "-b", "feat"]);
    let head = iso.commit_file(&repo, "feat.txt");
    let main_before = iso.git_out(&bare, &["rev-parse", "refs/heads/main"]);
    let (gh, log) = iso.fake_gh(&bare, "success");
    let r = iso.run_with_services(
        "land",
        &repo,
        &["--pr", "--gate", "true", "--ignore-siblings", "--no-update"],
        &[("OMS_GH_BIN", gh.to_str().unwrap())],
    );
    assert_eq!(r.0, 0, "{r:?}");
    assert!(r.1.contains("merged PR #7 (squash)"), "{r:?}");
    assert_eq!(
        iso.git_out(&bare, &["rev-parse", "refs/heads/main"]),
        main_before
    );
    assert_eq!(iso.git_out(&bare, &["rev-parse", "refs/heads/feat"]), head);
    let calls = fs::read_to_string(log).expect("gh log");
    let create = calls.find("pr create").expect("pr create");
    let ci = calls
        .find(&format!("--commit {head} --event pull_request"))
        .expect("exact-SHA CI query");
    let merge = calls
        .find(&format!(
            "pr merge 7 --repo {} --squash --delete-branch --match-head-commit {head}",
            bare.to_string_lossy().trim_end_matches(".git")
        ))
        .expect("pr merge");
    assert!(create < ci && ci < merge, "{calls}");
    let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
    for needle in [
        "\"mode\": \"pr\"",
        "\"head_branch\": \"feat\"",
        "\"pr_number\": 7",
        "\"state\": \"passed\"",
        "\"state\": \"MERGED\"",
    ] {
        assert!(status.contains(needle), "{needle}: {status}");
    }
}

#[test]
fn land_pr_refuses_to_merge_after_failed_ci_or_onto_the_target() {
    let iso = Isolated::new();
    let (repo, bare) = iso.with_remote("prfail");
    iso.git(&repo, &["checkout", "-q", "-b", "feat"]);
    iso.commit_file(&repo, "feat.txt");
    let (gh, log) = iso.fake_gh(&bare, "failure");
    let env = [("OMS_GH_BIN", gh.to_str().unwrap())];
    let args = ["--pr", "--gate", "true", "--ignore-siblings", "--no-update"];
    let r = iso.run_with_services("land", &repo, &args, &env);
    assert_eq!(r.0, 1, "{r:?}");
    assert!(r.1.contains("CI failure"), "{r:?}");
    let calls = fs::read_to_string(log).expect("gh log");
    assert!(
        calls.contains("pr create") && !calls.contains("pr merge"),
        "{calls}"
    );
    // the target is never a PR head, explicitly or by environment default
    let r = iso.run_with_services(
        "land",
        &repo,
        &[&args[..], &["--head-branch", "main"]].concat(),
        &env,
    );
    assert_refused(&r, "PR head branch must differ from the target branch");
    let r = iso.run_env(
        "land",
        &repo,
        &["--gate", "true"],
        &[("OMS_LAND_MODE", "sometimes")],
    );
    assert_refused(&r, "OMS_LAND_MODE must be pr, auto or direct");
}

#[test]
fn land_pr_refuses_diverged_remote_branch_and_ci_wait_zero_leaves_pr_open() {
    let iso = Isolated::new();
    let (repo, bare) = iso.with_remote("prdiv");
    iso.git(&repo, &["checkout", "-q", "-b", "other"]);
    iso.commit_file(&repo, "other.txt");
    iso.git(&repo, &["push", "-q", "origin", "other:refs/heads/feat"]);
    iso.git(&repo, &["checkout", "-q", "main"]);
    iso.git(&repo, &["checkout", "-q", "-b", "feat"]);
    let head = iso.commit_file(&repo, "feat.txt");
    let (gh, log) = iso.fake_gh(&bare, "success");
    let env = [("OMS_GH_BIN", gh.to_str().unwrap())];
    let args = ["--pr", "--gate", "true", "--ignore-siblings", "--no-update"];
    let r = iso.run_with_services("land", &repo, &args, &env);
    assert_eq!(r.0, 1, "{r:?}");
    assert!(r.1.contains("diverged"), "{r:?}");
    assert_ne!(iso.git_out(&bare, &["rev-parse", "refs/heads/feat"]), head);
    iso.git(&bare, &["update-ref", "-d", "refs/heads/feat"]);
    let r = iso.run_with_services(
        "land",
        &repo,
        &[&args[..], &["--ci-wait", "0"]].concat(),
        &env,
    );
    assert_eq!(r.0, 0, "{r:?}");
    assert!(r.1.contains("merge skipped because --ci-wait=0"), "{r:?}");
    let calls = fs::read_to_string(log).expect("gh log");
    assert!(
        calls.contains("pr create") && !calls.contains("pr merge"),
        "{calls}"
    );
}

#[test]
fn land_pr_defers_install_when_merge_commit_differs_from_head() {
    let iso = Isolated::new();
    let (repo, bare) = iso.with_remote("prsquash");
    iso.git(&repo, &["checkout", "-q", "-b", "feat"]);
    iso.commit_file(&repo, "feat.txt");
    let (gh, _) = iso.fake_gh(&bare, "success");
    let r = iso.run_with_services(
        "land",
        &repo,
        &["--pr", "--gate", "true", "--ignore-siblings"],
        &[("OMS_GH_BIN", gh.to_str().unwrap())],
    );
    assert_eq!(r.0, 0, "{r:?}");
    let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
    assert!(status.contains("merge-commit-pending-ci"), "{status}");
}

#[test]
fn land_local_merge_runs_gate_before_the_hooked_merge() {
    let iso = Isolated::new();
    let repo = iso.repo("local");
    iso.git(&repo, &["checkout", "-q", "-b", "feat"]);
    let feat = iso.commit_file(&repo, "feat.txt");
    iso.git(&repo, &["checkout", "-q", "main"]);
    iso.commit_file(&repo, "main.txt");
    let marker = iso.home.path().join("hook.log");
    let hook = repo.join(".git/hooks/pre-merge-commit");
    fs::create_dir_all(hook.parent().unwrap()).expect("hooks");
    fs::write(
        &hook,
        format!(
            "#!{}\n[ \"$OMS_LAND_ACTIVE\" = 1 ] || exit 1\nprintf 'ran %s\\n' \"$OMS_LAND_ACTIVE\" >> '{}'\n",
            which("bash").display(),
            marker.display()
        ),
    )
    .expect("hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).expect("chmod");
    let before = iso.git_out(&repo, &["rev-parse", "main"]);
    let base = ["--local-merge", "--from", "feat", "--ignore-siblings"];
    let failed = iso.run_with_services(
        "land",
        &repo,
        &[&base[..], &["--gate", "exit 1"]].concat(),
        &[],
    );
    assert_eq!(failed.0, 1, "{failed:?}");
    assert_eq!(iso.git_out(&repo, &["rev-parse", "main"]), before);
    assert!(!marker.exists());
    let passed = iso.run_with_services(
        "land",
        &repo,
        &[&base[..], &["--gate", "true"]].concat(),
        &[],
    );
    assert_eq!(passed.0, 0, "{passed:?}");
    assert_eq!(iso.git_out(&repo, &["rev-parse", "main^2"]), feat);
    assert_eq!(fs::read_to_string(&marker).expect("hook ran"), "ran 1\n");
    let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
    assert!(status.contains("\"mode\": \"local-merge\""), "{status}");
    assert!(
        status.contains(&format!("\"merged_from\": \"{feat}\"")),
        "{status}"
    );
    // a branch already merged is refused instead of reported as landed
    let again = iso.run_with_services(
        "land",
        &repo,
        &["--local-merge", "--from", "feat", "--gate", "true"],
        &[],
    );
    assert_refused(&again, "already contained in HEAD");
}

#[test]
fn land_direct_still_gates_then_pushes_the_target() {
    let iso = Isolated::new();
    let (repo, bare) = iso.with_remote("direct");
    let head = iso.commit_file(&repo, "next.txt");
    let args = ["--gate", "exit 1", "--ignore-siblings", "--ci-wait", "0"];
    let failed = iso.run_with_services("land", &repo, &args, &[]);
    assert_eq!(failed.0, 1, "{failed:?}");
    assert_ne!(iso.git_out(&bare, &["rev-parse", "refs/heads/main"]), head);
    let args = ["--gate", "true", "--ignore-siblings", "--ci-wait", "0"];
    let pushed = iso.run_with_services("land", &repo, &args, &[]);
    assert_eq!(pushed.0, 0, "{pushed:?}");
    assert_eq!(iso.git_out(&bare, &["rev-parse", "refs/heads/main"]), head);
    let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
    assert!(status.contains("\"mode\": \"direct\""), "{status}");
}

/// Multi-repo manifest fixture: repo-app depends on repo-core, repo-docs is
/// independent. Each repo has a candidate branch one commit ahead of main.
struct Manifest {
    path: PathBuf,
    receipt: PathBuf,
    repos: Vec<(&'static str, PathBuf, String)>,
}
impl Manifest {
    fn new(iso: &Isolated, verify: bool) -> Self {
        let dir = iso.home.path();
        let mut repos = Vec::new();
        let mut entries = Vec::new();
        for (id, deps) in [
            ("repo-core", ""),
            ("repo-app", "\"repo-core\""),
            ("repo-docs", ""),
        ] {
            let repo = iso.repo(id);
            iso.git(&repo, &["checkout", "-q", "-b", "cand"]);
            let sha = iso.commit_file(&repo, "cand.txt");
            iso.git(&repo, &["checkout", "-q", "main"]);
            let counter = dir.join(format!("{id}.count"));
            // Builtins only: the sandbox PATH holds nothing but git and bash.
            let check = if id == "repo-core" {
                format!(
                    "test -e {} && echo run >> {}",
                    dir.join("core.fixed").display(),
                    counter.display()
                )
            } else {
                format!("echo run >> {}", counter.display())
            };
            let cmd = if verify {
                format!(", \"verify_cmd\": [\"bash\", \"-c\", \"{check}\"]")
            } else {
                String::new()
            };
            entries.push(format!(
                "{{\"id\": \"{id}\", \"path\": \"{}\", \"target_ref\": \"main\", \"candidate_sha\": \"{sha}\", \"depends_on\": [{deps}]{cmd}}}",
                repo.display()
            ));
            repos.push((id, repo, sha));
        }
        let path = dir.join("manifest.json");
        fs::write(
            &path,
            format!("{{\"schema\": 1, \"repos\": [{}]}}", entries.join(", ")),
        )
        .unwrap();
        Self {
            path,
            receipt: dir.join("manifest-receipt.json"),
            repos,
        }
    }
    fn args<'a>(&'a self, extra: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec![
            "--check-manifest",
            self.path.to_str().unwrap(),
            "--manifest-receipt",
            self.receipt.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        args
    }
    fn main_of(&self, iso: &Isolated, id: &str) -> String {
        let repo = &self.repos.iter().find(|r| r.0 == id).unwrap().1;
        iso.git_out(repo, &["rev-parse", "main"])
    }
    fn candidate(&self, id: &str) -> &str {
        &self.repos.iter().find(|r| r.0 == id).unwrap().2
    }
    fn count(&self, iso: &Isolated, id: &str) -> usize {
        fs::read_to_string(iso.home.path().join(format!("{id}.count")))
            .map_or(0, |t| t.lines().count())
    }
}

#[test]
fn manifest_failure_holds_dependents_keeps_integrated_repos_and_resumes_idempotently() {
    let iso = Isolated::new();
    let m = Manifest::new(&iso, true);
    let before: Vec<String> = ["repo-core", "repo-app", "repo-docs"]
        .iter()
        .map(|id| m.main_of(&iso, id))
        .collect();
    let args = m.args(&["--execute-manifest"]);
    let held = iso.run_with_services("land", iso.home.path(), &args, &[]);
    assert_eq!(held.0, 3, "{held:?}");
    assert!(
        held.1.contains("\"status\": \"partial-hold\""),
        "{}",
        held.1
    );
    assert!(held.1.contains("\"mode\": \"execute\""));
    assert!(held.1.contains("\"manifest_sha256\":"));
    let receipt = fs::read_to_string(&m.receipt).unwrap();
    assert_eq!(receipt, held.1);
    assert_eq!(
        fs::metadata(&m.receipt).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let v = V::parse(receipt.as_bytes()).unwrap();
    assert_eq!(
        v.get("failed").array()[0].get("id").text(),
        Some("repo-core")
    );
    assert_eq!(
        v.get("blocked_dependents").array()[0].get("id").text(),
        Some("repo-app")
    );
    assert_eq!(v.get("integrated").array()[0].text(), Some("repo-docs"));
    // The independent repo moved; the failed one and its dependent did not.
    assert_eq!(m.main_of(&iso, "repo-docs"), m.candidate("repo-docs"));
    assert_eq!(m.main_of(&iso, "repo-core"), before[0]);
    assert_eq!(m.main_of(&iso, "repo-app"), before[1]);
    assert_eq!(m.count(&iso, "repo-docs"), 1);
    assert_eq!(m.count(&iso, "repo-app"), 0);

    // Fix lives outside the manifest, so the same manifest bytes resume.
    fs::write(iso.home.path().join("core.fixed"), "").unwrap();
    let resumed = iso.run_with_services("land", iso.home.path(), &args, &[]);
    assert_eq!(resumed.0, 0, "{resumed:?}");
    assert!(
        resumed.1.contains("\"status\": \"integrated\""),
        "{}",
        resumed.1
    );
    assert!(
        resumed.1.contains("already integrated (earlier receipt"),
        "{}",
        resumed.1
    );
    for id in ["repo-core", "repo-app", "repo-docs"] {
        assert_eq!(m.main_of(&iso, id), m.candidate(id), "{id}");
    }
    // Integrated repos were not re-verified; the others ran once each.
    assert_eq!(m.count(&iso, "repo-docs"), 1);
    assert_eq!(m.count(&iso, "repo-core"), 1);
    assert_eq!(m.count(&iso, "repo-app"), 1);
    // A third run is a no-op.
    let again = iso.run_with_services("land", iso.home.path(), &args, &[]);
    assert_eq!(again.0, 0, "{again:?}");
    assert_eq!(m.count(&iso, "repo-app"), 1);
}

#[test]
fn manifest_check_publishes_nothing_and_execution_needs_the_publication_fence() {
    let iso = Isolated::new();
    let m = Manifest::new(&iso, false);
    let before = m.main_of(&iso, "repo-core");
    // Verification alone needs no services and moves no branch.
    let checked = iso.run("land", iso.home.path(), &m.args(&[]));
    assert_eq!(checked.0, 0, "{checked:?}");
    assert!(
        checked.1.contains("\"status\": \"verified\""),
        "{}",
        checked.1
    );
    assert_eq!(m.main_of(&iso, "repo-core"), before);
    // Publication fails closed without a native fence.
    let refused = iso.run("land", iso.home.path(), &m.args(&["--execute-manifest"]));
    assert_eq!(refused.0, 2, "{refused:?}");
    assert_eq!(m.main_of(&iso, "repo-core"), before);
}

#[test]
fn manifest_rejects_malformed_graphs_and_missing_receipts_with_exit_two() {
    let iso = Isolated::new();
    let m = Manifest::new(&iso, false);
    let text = fs::read_to_string(&m.path).unwrap();
    let write = |t: &str| fs::write(&m.path, t).unwrap();
    for (broken, needle) in [
        (
            text.replace(
                "\"depends_on\": [\"repo-core\"]",
                "\"depends_on\": [\"ghost\"]",
            ),
            "unknown depends_on",
        ),
        (
            text.replace("\"id\": \"repo-docs\"", "\"id\": \"repo-core\""),
            "duplicate repo id",
        ),
        (
            text.replace("\"depends_on\": []", "\"depends_on\": [\"repo-app\"]"),
            "cycle",
        ),
        (
            text.replace(
                "\"depends_on\": [\"repo-core\"]",
                "\"depends_on\": \"repo-core\"",
            ),
            "depends_on must be an array",
        ),
        (text.replacen(&m.repos[0].2, "abc", 1), "40 hex"),
    ] {
        write(&broken);
        let r = iso.run("land", iso.home.path(), &m.args(&[]));
        assert_eq!(r.0, 2, "{needle}: {r:?}");
        assert!(r.2.contains(needle), "{needle}: {r:?}");
        assert!(!m.receipt.exists());
    }
    write(&text);
    let (code, _, err) = iso.run(
        "land",
        iso.home.path(),
        &["--check-manifest", m.path.to_str().unwrap()],
    );
    assert_eq!(code, 2);
    assert!(err.contains("required together"), "{err}");
}

#[test]
fn manifest_publish_refuses_checked_out_target_drift() {
    let iso = Isolated::new();
    let repo = iso.repo("drift");
    iso.git(&repo, &["checkout", "-q", "-b", "cand"]);
    let inter_sha = iso.commit_file(&repo, "inter.txt");
    let cand_sha = iso.commit_file(&repo, "cand.txt");
    iso.git(&repo, &["checkout", "-q", "main"]);

    let manifest_path = iso.home.path().join("drift-manifest.json");
    let receipt_path = iso.home.path().join("drift-receipt.json");
    let verify = format!("git -C {} merge -q --ff-only {inter_sha}", repo.display());
    fs::write(
        &manifest_path,
        format!(
            "{{\"schema\": 1, \"repos\": [{{\"id\": \"drift\", \"path\": \"{}\", \"target_ref\": \"main\", \"candidate_sha\": \"{cand_sha}\", \"depends_on\": [], \"verify_cmd\": [\"bash\", \"-c\", \"{verify}\"]}}]}}",
            repo.display()
        ),
    )
    .unwrap();

    let (code, out, err) = iso.run_with_services(
        "land",
        iso.home.path(),
        &[
            "--check-manifest",
            manifest_path.to_str().unwrap(),
            "--manifest-receipt",
            receipt_path.to_str().unwrap(),
            "--execute-manifest",
        ],
        &[],
    );
    assert_eq!(code, 3, "out={out} err={err}");
    assert!(
        out.contains("target branch moved during verification"),
        "{out}"
    );
    let receipt = fs::read_to_string(&receipt_path).unwrap();
    assert!(
        receipt.contains("target branch moved during verification"),
        "{receipt}"
    );
    assert_eq!(iso.git_out(&repo, &["rev-parse", "main"]), inter_sha);
}
