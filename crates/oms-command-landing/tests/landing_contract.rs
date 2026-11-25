//! Contract tests for the landing command surface. Behavioural cases run in a
//! re-executed copy of this test binary with a scrubbed environment and a PATH
//! that holds only `git` and `bash`, so no `python3`, `OMS_*` or `GIT_*` state leaks in.
#![forbid(unsafe_code)]

use oms_command_landing::services::{Execution, PublicationGuard, Services};
use oms_command_landing::{TOOLS, V, obj, owns, run, run_with_services, usage};
use sha2::{Digest, Sha256};
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
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    if let Some(marker) = std::env::var_os("OMS_CT_INTERRUPT_AT") {
        let marker = PathBuf::from(marker);
        let cancelled = flag.clone();
        std::thread::spawn(move || {
            while !marker.is_file() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            cancelled.store(true, Ordering::Release);
        });
    }
    let (code, out, err) = if tool == "exact-ci-observation" {
        let identity = V::parse(args[0].to_str().unwrap().as_bytes()).unwrap();
        match oms_command_landing::ci::observe_attempt(&cwd, &cwd, &identity, &flag)
            .and_then(|v| v.json(false).map_err(oms_command_landing::Error::from))
        {
            Ok(raw) => (0, raw, vec![]),
            Err(e) => (e.code, vec![], e.message.into_bytes()),
        }
    } else if tool == "feedback-candidate" {
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

#[cfg(target_os = "linux")]
const ADMISSION_STARTUP_FAULT: &str = "OMS_CT_ADMISSION_STARTUP_FAULT";

#[cfg(target_os = "linux")]
struct AdmissionStartupFault {
    native: oms_reference::controller::Controller,
    foreground: std::thread::ThreadId,
    event_path: PathBuf,
    retained_lock: Option<std::sync::Mutex<Option<oms_command_operations::lock::FileLock>>>,
}

#[cfg(target_os = "linux")]
impl oms_reference::controller::ExecutingImage for AdmissionStartupFault {
    fn verify_current(&self, image: &fs::File) -> oms_reference::Result<()> {
        use std::os::unix::fs::MetadataExt;
        self.native.check()?;
        let bound = image.metadata().map_err(|e| e.to_string())?;
        let live = fs::metadata("/proc/self/exe").map_err(|e| e.to_string())?;
        assert_eq!((bound.dev(), bound.ino()), (live.dev(), live.ino()));
        if std::thread::current().id() == self.foreground {
            return Ok(());
        }
        if let Some(retained) = &self.retained_lock {
            // Scope registration has finished. Keep a real competing lock alive
            // until the foreground returns and releases its last controller clone.
            let mut retained = retained.lock().unwrap();
            if retained.is_none() {
                *retained = Some(oms_command_operations::lock::FileLock::acquire(
                    &self.event_path,
                    &AtomicBool::new(false),
                )?);
            }
        } else if oms_command_operations::lock::runtime_lock_dir(&self.event_path)?.is_dir() {
            // The native producer acquired the lock and projected the ledger.
            return Err("fixture stops the initial heartbeat before append".into());
        }
        Ok(())
    }
}

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
    #[cfg(target_os = "linux")]
    fn controller(&self) -> oms_command_landing::Result<oms_reference::controller::Controller> {
        use oms_reference::controller::Controller;
        let native = Controller::capture_current()?;
        let Ok(mode) = std::env::var(ADMISSION_STARTUP_FAULT) else {
            return Ok(native);
        };
        assert!(matches!(mode.as_str(), "lock" | "projection"));
        Ok(Controller::from_authenticated_descriptor(
            fs::File::open("/proc/self/exe")?,
            std::sync::Arc::new(AdmissionStartupFault {
                native,
                foreground: std::thread::current().id(),
                event_path: oms_command_operations::event_path(&std::env::current_dir()?),
                retained_lock: (mode == "lock").then(|| std::sync::Mutex::new(None)),
            }),
        )?)
    }
    fn supervise(
        &self,
        request: &oms_command_landing::services::VerifyRequest,
        _: &AtomicBool,
    ) -> oms_command_landing::Result<Execution> {
        #[cfg(target_os = "linux")]
        assert!(
            std::env::var_os(ADMISSION_STARTUP_FAULT).is_none(),
            "an unready reference monitor must block the admission verifier"
        );
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
    #[cfg(target_os = "linux")]
    fn supervise_batch(
        &self,
        request: &oms_command_landing::services::VerifyRequest,
        interrupted: &AtomicBool,
    ) -> oms_command_landing::Result<oms_command_landing::services::BatchVerification> {
        use oms_command_landing::services::{BatchVerification, VerificationOutcome::*};
        use oms_supervisor::process::{Progress, Reactor, Request, StopReason};
        use std::time::{Duration, Instant};
        let mode = std::env::var("OMS_CT_BATCH_MODE").unwrap_or_default();
        assert_ne!(
            mode, "never",
            "stale or invalid preview launched a verifier"
        );
        assert!(!request.cwd.starts_with(std::env::current_dir().unwrap()));
        assert_eq!(request.wall, Duration::from_secs(1800));
        if mode == "setup-unavailable" {
            return Ok(BatchVerification {
                outcome: Unavailable,
                verifier_exit: None,
                elapsed: Duration::ZERO,
            });
        }
        let mut command = Request::new(request.argv.clone());
        command.cwd = Some(request.cwd.clone());
        command.capture.max_bytes = 512;
        command.capture.on_limit = oms_supervisor::capture::LimitAction::Terminate;
        command.deadline = Some(Instant::now() + Duration::from_secs(5));
        let mut reactor = Reactor::new();
        let job = reactor.start(command).unwrap();
        if mode == "cancel" {
            interrupted.store(true, Ordering::Release);
            reactor.stop(job, StopReason::Cancelled).unwrap();
        }
        if let (Ok(branch), Ok(new), Ok(old)) = (
            std::env::var("OMS_CT_BATCH_MOVE_REF"),
            std::env::var("OMS_CT_BATCH_MOVE_TO"),
            std::env::var("OMS_CT_BATCH_MOVE_FROM"),
        ) {
            assert!(
                Command::new("git")
                    .args(["update-ref", &branch, &new, &old])
                    .status()
                    .unwrap()
                    .success()
            );
        }
        loop {
            if let Progress::Complete(done) = reactor.poll(job, Duration::from_millis(10)).unwrap()
            {
                assert!(done.cleanup_complete());
                let exit = done.exit.map(|exit| exit.shell_code());
                let outcome = match (done.stop, exit) {
                    (Some(StopReason::Cancelled), _) => Cancelled,
                    (Some(StopReason::OutputLimit), _) => OutputLimited,
                    (None, Some(0)) => Passed,
                    (None, Some(1)) => Failed,
                    _ => panic!("unexpected verifier completion: {:?}", done.stop),
                };
                return Ok(BatchVerification {
                    outcome,
                    verifier_exit: if done.stop.is_none() { exit } else { None },
                    elapsed: done.duration,
                });
            }
        }
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
        repo: &Path,
        budget: std::time::Duration,
        _: &AtomicBool,
    ) -> oms_command_landing::Result<()> {
        if budget.is_zero()
            && let Some(log) = std::env::var_os("OMS_CT_SIBLING_CHECKS")
        {
            RecordingServices(log.into()).log(repo.display().to_string());
            if std::env::var_os("OMS_CT_LATE_SIBLING").as_deref() == Some(repo.as_os_str()) {
                return Err(oms_command_landing::Error::new(
                    75,
                    "fixture sibling active",
                ));
            }
        }
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
fn owns_exactly_the_thirteen_tools() {
    assert_eq!(TOOLS.len(), 13);
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
            "--pr, --direct and --local-merge are mutually exclusive",
        ),
        (
            "land",
            vec!["--pr", "--direct"],
            "--pr, --direct and --local-merge are mutually exclusive",
        ),
        (
            "land",
            vec!["--pr", "--merge-method", "merge"],
            "--merge-method requires --merge",
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
    fs::write(
        repo.join("verify-product.sh"),
        b"test \"$(<a.txt)\" = $'one\\ntwo'\n",
    )
    .unwrap();
    iso.git(&repo, &["add", "verify-product.sh"]);
    iso.git(&repo, &["commit", "-qm", "fixture verifier"]);
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
    let verify = "bash verify-product.sh";
    iso.git(&repo, &["apply", "--cached", patch]);
    let tree = iso.command(&repo).arg("write-tree").output().unwrap();
    assert!(tree.status.success());
    let tree = String::from_utf8(tree.stdout).unwrap();
    iso.git(&repo, &["reset", "-q", "HEAD", "--", "a.txt"]);
    let patch_sha = oms_command_plan::json::sha(fs::read(repo.join(patch)).unwrap());
    let verify_sha = oms_command_plan::json::sha(verify.as_bytes());
    let manifest = obj([
        ("base_sha", base.trim().into()),
        ("patch_sha256", patch_sha.as_str().into()),
        ("tree", tree.trim().into()),
        ("verify_sha256", verify_sha.into()),
    ]);
    let manifest_raw = manifest.json(false).unwrap();
    let manifest_sha = oms_command_plan::json::sha(&manifest_raw);
    let panel = obj([
        ("schema", 1usize.into()),
        ("base", base.trim().into()),
        ("patch_sha256", patch_sha.into()),
        (
            "snapshot",
            obj([
                ("composition", manifest),
                (
                    "candidate",
                    obj([("artifact_sha256", manifest_sha.as_str().into())]),
                ),
                (
                    "task",
                    obj([("claimed_by_participant", "fixture-main".into())]),
                ),
            ]),
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
            "--verify",
            verify,
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
    let rows = fs::read(repo.join(".oms/artifacts/index.jsonl")).unwrap();
    let rows = oms_command_plan::storage::rows(&rows).unwrap();
    let proof = oms_command_plan::panel_composition::verify_landing(
        &rows,
        &oms_command_plan::json::parse(&manifest_raw).unwrap(),
        &manifest_sha,
        "fixture-main",
    )
    .unwrap();
    let report = fs::read(repo.join(proof.text("artifact"))).unwrap();
    assert_eq!(
        oms_command_plan::json::sha(report),
        proof.text("artifact_sha256")
    );
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

#[cfg(target_os = "linux")]
#[test]
fn patch_admit_timeout_stops_before_verification_and_publication() {
    use std::time::{Duration, Instant};
    let iso = Isolated::new();
    for (fault, last_phase, state, events_expected) in [
        ("projection", "lifecycle-projection-ready", "blocked", 4),
        ("lock", "initial-heartbeat-started", "working", 3),
    ] {
        let repo = iso.repo(fault);
        let patch = real_patch(&iso, &repo);
        let patch_bytes = fs::read(repo.join(patch)).unwrap();
        let report = iso.tmp.join("admission.md");
        let started = Instant::now();
        let (code, out, err) = iso.run_with_services(
            "patch-admit",
            &repo,
            &[
                "--patch",
                patch,
                "--report",
                report.to_str().unwrap(),
                "--verify",
                "true",
            ],
            &[(ADMISSION_STARTUP_FAULT, fault), ("OMS_LOCK_TIMEOUT", "8")],
        );
        let elapsed = started.elapsed();
        assert_eq!(code, 2, "{out}{err}");
        assert!(out.is_empty(), "{out}");
        assert!(
            (Duration::from_millis(1500)..Duration::from_secs(7)).contains(&elapsed),
            "{fault}: unexpected startup timeout and cleanup duration: {elapsed:?} ({err})"
        );
        let phase = format!(
            "error: reference monitor did not become ready (last_phase={last_phase}, phase_elapsed_ms="
        );
        let (reported, disposition) = err
            .trim_end()
            .strip_prefix(&phase)
            .and_then(|detail| detail.split_once(')'))
            .unwrap_or_else(|| panic!("{err}"));
        assert!(reported.parse::<u64>().is_ok_and(|ms| ms <= 1500), "{err}");
        if fault == "lock" {
            assert!(disposition.contains("lifecycle attempt remains"), "{err}");
        } else {
            assert_eq!(disposition, "", "{err}");
        }
        assert!(!report.exists());
        assert!(!repo.join(".oms/artifacts/index.jsonl").exists());
        assert_eq!(fs::read_dir(&iso.tmp).unwrap().count(), 0);
        assert_eq!(fs::read(repo.join(patch)).unwrap(), patch_bytes);
        iso.git(&repo, &["diff", "--exit-code", "HEAD", "--"]);

        let (events, attempts) = oms_command_operations::load_projection(&repo).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].get("tool").text(), Some("patch-admit"));
        assert_eq!(attempts[0].get("state").text(), Some(state));
        assert_eq!(events.len(), events_expected);
        assert!(events.iter().all(|event| {
            matches!(
                event.get("event_type").text(),
                Some("attempt.created" | "attempt.state_changed")
            )
        }));
    }
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

#[test]
fn exact_ci_observation_binds_repo_head_run_attempt_and_rechecks_after_gets() {
    let f = FeedbackFixture::new();
    let head = String::from_utf8(
        f.isolated
            .command(&f.repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    fs::write(&f.gh,r#"#!/bin/bash
[[ "$1 $2 $3" == 'api --method GET' ]] || exit 93
n=0
if [[ -f "$OMS_CT_CALLS" ]]; then read -r n < "$OMS_CT_CALLS"; fi
n=$((n+1)); printf '%s\n' "$n" > "$OMS_CT_CALLS"
if [[ "$OMS_CT_MODE" == interrupt ]]; then exit 130; fi
case "${@: -1}" in
'repos/example/project/actions/runs/9/attempts/1/jobs?per_page=100') printf '%s\n' "$OMS_CT_JOBS";;
'repos/example/project/actions/runs/9')
  if [[ "$n" == 3 && "$OMS_CT_MODE" == moved ]]; then git commit --allow-empty -qm changed || exit 91; fi
  if [[ "$n" == 3 && "$OMS_CT_MODE" == rerun ]]; then printf '%s\n' "$OMS_CT_RERUN"; else printf '%s\n' "$OMS_CT_RUN"; fi;;
*) exit 94;;
esac
"#).unwrap();
    let identity = obj([
        ("repository", "example/project".into()),
        ("head_sha", head.clone().into()),
        ("workflow_id", 7usize.into()),
        ("run_id", 9usize.into()),
        ("run_attempt", 1usize.into()),
    ]);
    let run = obj([
        ("id", 9usize.into()),
        ("workflow_id", 7usize.into()),
        ("run_attempt", 1usize.into()),
        ("head_sha", head.clone().into()),
        ("repository", obj([("full_name", "EXAMPLE/Project".into())])),
        ("status", "completed".into()),
        ("conclusion", "success".into()),
        ("updated_at", "2026-11-23T10:00:00Z".into()),
    ]);
    let job = obj([
        ("id", 11usize.into()),
        ("run_id", 9usize.into()),
        ("run_attempt", 1usize.into()),
        ("head_sha", head.into()),
        (
            "run_url",
            "https://api.github.com/repos/example/project/actions/runs/9".into(),
        ),
        ("name", "rust".into()),
        ("status", "completed".into()),
        ("conclusion", "success".into()),
        ("steps", V::Arr(vec!["not retained".into()])),
    ]);
    let calls = f.isolated.home.path().join("exact-ci-calls");
    let query = |run: &V, job: &V, mode: &str| {
        let _ = fs::remove_file(&calls);
        let encode = |v: &V| String::from_utf8(v.json(false).unwrap()).unwrap();
        let mut rerun = run.clone();
        rerun.set("run_attempt", 2usize);
        f.isolated.run_env(
            "exact-ci-observation",
            &f.repo,
            &[&encode(&identity)],
            &[
                ("OMS_GH_BIN", f.gh.to_str().unwrap()),
                ("OMS_CT_CALLS", calls.to_str().unwrap()),
                ("OMS_CT_MODE", mode),
                ("OMS_CT_RUN", &encode(run)),
                ("OMS_CT_RERUN", &encode(&rerun)),
                (
                    "OMS_CT_JOBS",
                    &encode(&obj([
                        ("total_count", 1usize.into()),
                        ("jobs", V::Arr(vec![job.clone()])),
                    ])),
                ),
            ],
        )
    };
    let ok = query(&run, &job, "");
    assert_eq!(ok.0, 0, "{ok:?}");
    let ledger = f.repo.join(".oms/ci.jsonl");
    let first = fs::read(&ledger).unwrap();
    assert!(!String::from_utf8_lossy(&first).contains("not retained"));
    assert_eq!(query(&run, &job, "").0, 0);
    assert_eq!(
        fs::read(&ledger).unwrap(),
        first,
        "repeated observation does not grow source stream"
    );
    for field in ["run_id", "run_attempt", "head_sha", "run_url"] {
        let mut wrong = job.clone();
        wrong.set(
            field,
            match field {
                "head_sha" => "b".repeat(40).into(),
                "run_url" => "https://api.github.com/repos/foreign/project/actions/runs/9".into(),
                _ => V::from(99usize),
            },
        );
        assert_eq!(query(&run, &wrong, "").0, 2, "{field}");
        assert_eq!(fs::read(&ledger).unwrap(), first);
    }
    let mut foreign = run.clone();
    foreign.set("repository", obj([("full_name", "foreign/project".into())]));
    assert_eq!(query(&foreign, &job, "").0, 2);
    assert_eq!(
        query(&run, &job, "rerun").0,
        2,
        "a rerun during observation is not the old successful attempt"
    );
    assert_eq!(query(&run, &job, "interrupt").0, 130);
    assert_eq!(
        query(&run, &job, "moved").0,
        2,
        "local HEAD is rechecked after network observations"
    );
    assert_eq!(
        fs::read(&ledger).unwrap(),
        first,
        "refused observations do not mutate retained evidence"
    );
}

#[test]
fn ci_attention_observes_maintenance_without_certifying_head_or_writing_state() {
    let fixture = FeedbackFixture::new();
    fs::write(&fixture.gh, r#"#!/bin/bash
[[ "$1 $2 $3" == 'api --method GET' ]] || exit 93
case "$OMS_CT_INTERRUPT:${@: -1}" in
  'default:repos/example/project'|'schedule:repos/example/project/actions/runs?event=schedule&branch=main&per_page=20'|'manual:repos/example/project/actions/runs?event=workflow_dispatch&branch=main&per_page=20') exit 130;;
esac
case "${@: -1}" in
  'repos/example/project') printf '%s\n' '{"default_branch":"main"}';;
  'repos/example/project/actions/runs?event=schedule&branch=main&per_page=20'|'repos/example/project/actions/runs?event=schedule&branch=release%2Fnightly&per_page=20') printf '%s\n' "$OMS_CT_SCHEDULE";;
  'repos/example/project/actions/runs?event=workflow_dispatch&branch=main&per_page=20'|'repos/example/project/actions/runs?event=workflow_dispatch&branch=release%2Fnightly&per_page=20')
    [[ "$OMS_CT_MANUAL" != null ]] || { printf 'private transport detail' >&2; exit 2; }
    printf '%s\n' "$OMS_CT_MANUAL";;
  *) exit 94;;
esac
"#).unwrap();
    let row = |event: &str,
               id: usize,
               workflow: usize,
               number: usize,
               attempt: usize,
               status: &str,
               conclusion: &str| {
        obj([
            ("id", id.into()),
            ("workflow_id", workflow.into()),
            ("run_number", number.into()),
            ("run_attempt", attempt.into()),
            ("event", event.into()),
            ("head_branch", "main".into()),
            ("head_sha", "a".repeat(40).into()),
            ("updated_at", "2026-11-23T03:23:00Z".into()),
            ("status", status.into()),
            ("conclusion", conclusion.into()),
            (
                "html_url",
                format!("https://github.com/example/project/actions/runs/{id}").into(),
            ),
        ])
    };
    let envelope = |rows: Vec<V>, total: usize| {
        obj([
            ("total_count", total.into()),
            ("workflow_runs", V::Arr(rows)),
        ])
    };
    let query = |schedule: &V, manual: &V, args: &[&str]| {
        let encode = |v: &V| String::from_utf8(v.json(false).unwrap()).unwrap();
        fixture.isolated.run_env(
            "ci-status",
            &fixture.repo,
            args,
            &[
                ("OMS_GH_BIN", fixture.gh.to_str().unwrap()),
                ("OMS_CT_SCHEDULE", &encode(schedule)),
                ("OMS_CT_MANUAL", &encode(manual)),
            ],
        )
    };
    let failed = row("schedule", 71, 7, 1, 1, "completed", "failure");
    let passed = row("schedule", 72, 7, 2, 1, "completed", "success");
    let manual = envelope(
        vec![row(
            "workflow_dispatch",
            81,
            8,
            1,
            1,
            "completed",
            "failure",
        )],
        1,
    );
    // Select by run order, not input ordering or the local checkout's unrelated SHA.
    let schedule = envelope(vec![failed.clone(), passed.clone()], 2);
    let output = query(&schedule, &manual, &["attention", "--json"]);
    assert_eq!(output.0, 1, "{output:?}");
    let report = V::parse(output.1.as_bytes()).unwrap();
    assert_eq!(report.get("kind").text(), Some("oms-ci-attention"));
    assert_eq!(report.get("branch").text(), Some("main"));
    assert_eq!(report.get("observations").array().len(), 2);
    assert_eq!(
        report.get("observations").array()[0].get("state").text(),
        Some("clear")
    );
    assert_eq!(
        report.get("observations").array()[1].get("state").text(),
        Some("failed")
    );
    let output = query(&schedule, &manual, &["attention", "main"]);
    assert!(
        output
            .1
            .contains("workflow_dispatch workflow 8 failed failure"),
        "{output:?}"
    );
    assert!(!output.1.contains("actions/runs/71"));
    assert!(output.1.contains("updated 2026-11-23T03:23:00Z"));

    let mut selected = passed.clone();
    selected.set("head_branch", "release/nightly");
    let mut selected_manual = row("workflow_dispatch", 81, 8, 1, 1, "completed", "success");
    selected_manual.set("head_branch", "release/nightly");
    let output = query(
        &envelope(vec![selected], 1),
        &envelope(vec![selected_manual], 1),
        &["attention", "release/nightly", "--json"],
    );
    assert_eq!(output.0, 0, "percent-encoded branch: {output:?}");
    assert_eq!(
        V::parse(output.1.as_bytes()).unwrap().get("branch").text(),
        Some("release/nightly")
    );

    let another_workflow = envelope(
        vec![
            passed.clone(),
            row("schedule", 91, 9, 1, 1, "completed", "failure"),
        ],
        2,
    );
    let output = query(&another_workflow, &manual, &["attention"]);
    assert_eq!(output.0, 1, "{output:?}");
    assert!(
        output.1.contains("schedule workflow 9 failed failure"),
        "{output:?}"
    );

    // A successful later attempt resolves the observation; queued work never counts as green.
    let retry = envelope(
        vec![failed, row("schedule", 71, 7, 1, 2, "completed", "success")],
        2,
    );
    let green = envelope(
        vec![row(
            "workflow_dispatch",
            81,
            8,
            1,
            2,
            "completed",
            "success",
        )],
        1,
    );
    assert_eq!(query(&retry, &green, &["attention"]).0, 0);
    let pending = envelope(
        vec![passed.clone(), row("schedule", 73, 7, 3, 1, "queued", "")],
        2,
    );
    let output = query(&pending, &green, &["attention"]);
    assert_eq!(output.0, 2, "{output:?}");
    assert!(output.1.contains("pending queued"), "{output:?}");

    for bad in [
        V::Null,
        envelope(vec![], 0),
        obj([("workflow_runs", V::Arr(vec![]))]),
        envelope(
            vec![row("schedule", 72, 7, 2, 1, "completed", "skipped")],
            1,
        ),
    ] {
        let output = query(&bad, &green, &["attention"]);
        assert_eq!(output.0, 2, "{output:?}");
        assert!(output.1.contains("unknown"), "{output:?}");
    }
    let output = query(&schedule, &V::Null, &["attention"]);
    assert_eq!(output.0, 2, "{output:?}");
    assert!(!format!("{}{}", output.1, output.2).contains("private transport detail"));
    for (key, value) in [
        ("event", "push"),
        ("head_branch", "other"),
        ("head_sha", "short"),
        ("html_url", "https://example.invalid/foreign"),
        (
            "html_url",
            "https://github.com/example/project/actions/runs/73",
        ),
        (
            "html_url",
            "https://github.com/example/project/Actions/runs/72",
        ),
        ("updated_at", "not-a-date"),
    ] {
        let mut foreign = passed.clone();
        foreign.set(key, value);
        let output = query(&envelope(vec![foreign], 1), &green, &["attention"]);
        assert_eq!(output.0, 2, "{key}: {output:?}");
        assert!(output.1.contains("unknown"));
    }
    let bounded = envelope(vec![passed], 100);
    let output = query(&bounded, &green, &["attention"]);
    assert_eq!(output.0, 0, "{output:?}");
    assert!(output.1.contains("bounded window"), "{output:?}");
    assert_eq!(
        query(&bounded, &green, &["attention", "--limit", "101"]).0,
        2
    );
    for phase in ["default", "schedule", "manual"] {
        let output = fixture.isolated.run_env(
            "ci-status",
            &fixture.repo,
            &["attention", "--json"],
            &[
                ("OMS_GH_BIN", fixture.gh.to_str().unwrap()),
                ("OMS_CT_INTERRUPT", phase),
                (
                    "OMS_CT_SCHEDULE",
                    &String::from_utf8(bounded.json(false).unwrap()).unwrap(),
                ),
                (
                    "OMS_CT_MANUAL",
                    &String::from_utf8(green.json(false).unwrap()).unwrap(),
                ),
            ],
        );
        assert_eq!(output.0, 130, "interrupted {phase}: {output:?}");
        assert!(
            output.1.is_empty(),
            "interruption must not publish an observation"
        );
    }
    fixture.isolated.git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/EXAMPLE/Project.git",
        ],
    );
    let script = fs::read_to_string(&fixture.gh)
        .unwrap()
        .replace("repos/example/project", "repos/EXAMPLE/Project");
    fs::write(&fixture.gh, script).unwrap();
    let output = query(&bounded, &green, &["attention", "--json"]);
    assert_eq!(output.0, 0, "GitHub repository casing: {output:?}");
    assert!(
        !fixture.repo.join(".oms").exists(),
        "read-only attention created state"
    );
    assert!(
        !fixture.repo.join(".gitignore").exists(),
        "read-only attention changed ignores"
    );
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
    /// Like `with_remote`, but the push destination reads as GitHub: a bare
    /// repository inside the work tree at the relative path `github.com/owner/NAME.git`.
    fn with_github_remote(&self, name: &str) -> (PathBuf, PathBuf) {
        let repo = self.repo(name);
        let bare = repo.join(format!("github.com/owner/{name}.git"));
        fs::create_dir_all(&bare).expect("bare dir");
        self.git(&bare, &["init", "-q", "--bare", "-b", "main"]);
        fs::create_dir_all(repo.join(".git/info")).expect("info dir");
        fs::write(repo.join(".git/info/exclude"), "/github.com/\n").expect("exclude");
        let url = format!("github.com/owner/{name}.git");
        self.git(&repo, &["remote", "add", "origin", &url]);
        self.git(&repo, &["push", "-q", "origin", "main"]);
        strip_group_world_write(&repo);
        (repo, bare)
    }
    fn commit_file(&self, repo: &Path, file: &str) -> String {
        self.commit_as(repo, file, &format!("{file}\n"), &[&format!("add {file}")])
    }
    /// Commit `text` as `file`; each message is one `-m` paragraph (subject, body...).
    fn commit_as(&self, repo: &Path, file: &str, text: &str, message: &[&str]) -> String {
        let path = repo.join(file);
        fs::create_dir_all(path.parent().unwrap()).expect("parent");
        fs::write(path, text).expect("write");
        self.git(repo, &["add", file]);
        let mut args = vec!["commit", "-q"];
        for m in message {
            args.extend(["-m", m]);
        }
        self.git(repo, &args);
        strip_group_world_write(repo);
        self.git_out(repo, &["rev-parse", "HEAD"])
    }
    /// Fake `gh`: logs argv, answers JSON queries from the bare remote, and
    /// reports CI as `ci` for the exact `--commit` it is asked about. It keeps
    /// the created PR body in `gh.pr.body`. With `gh.pr.squash` present, `pr
    /// merge` squashes the head tree onto main; `gh.pr.moved` first moves main
    /// by one empty commit, as a concurrent merge would.
    fn fake_gh(&self, bare: &Path, ci: &str) -> (PathBuf, PathBuf) {
        let log = self.home.path().join("gh.log");
        let state = self.home.path().join("gh.pr");
        let script = self.home.path().join("fake-gh");
        let body = format!(
            r#"#!{bash}
printf '%s\n' "$*" >> '{log}'
head=; commit=; event=; body=; match=; base=
args=("$@")
for ((i = 0; i < ${{#args[@]}}; i++)); do
  case "${{args[i]}}" in
    --base) base=${{args[i+1]}} ;;
    --head) head=${{args[i+1]}} ;;
    --commit) commit=${{args[i+1]}} ;;
    --event) event=${{args[i+1]}} ;;
    --body-file) body=${{args[i+1]}} ;;
    --match-head-commit) match=${{args[i+1]}} ;;
  esac
done
squash() {{
  git --git-dir='{bare}' -c user.name=T -c user.email=t@example.invalid commit-tree "$@" -p refs/heads/main -m squash
}}
case "$1 $2" in
  "pr list")
    if [ -z "$head" ]; then
      if [ -f '{state}.dependents' ]; then
        IFS= read -rd '' text < '{state}.dependents'; printf '%s' "$text"
      else echo '[]'; fi
    elif [ -f '{state}' ]; then
      tip=$(git --git-dir='{bare}' rev-parse "refs/heads/$head")
      printf '[{{"number":7,"url":"https://example.invalid/pull/7","headRefOid":"%s","baseRefName":"%s","headRefName":"%s","isCrossRepository":false}}]\n' "$tip" "$base" "$head"
    else echo '[]'; fi ;;
  "pr create") : > '{state}'; IFS= read -rd '' text < "$body"; printf '%s' "$text" > '{state}.body'; echo https://example.invalid/pull/7 ;;
  "pr merge")
    if [ -f '{state}.moved' ]; then
      git --git-dir='{bare}' update-ref refs/heads/main "$(squash 'refs/heads/main^{{tree}}')"
    fi
    if [ -f '{state}.squash' ]; then
      git --git-dir='{bare}' update-ref refs/heads/main "$(squash "$match^{{tree}}")"
    fi ;;
  "pr view")
    if [ -f '{state}.view' ]; then cat '{state}.view'
    else printf '{{"state":"MERGED","mergeCommit":{{"oid":"%s"}}}}\n' "$(git --git-dir='{bare}' rev-parse refs/heads/main)"; fi ;;
  "run view")
    if [ -f "{state}.jobs$3" ]; then cat "{state}.jobs$3"
    elif IFS= read -r oid < '{state}.ci-head'; then
      printf '{{"status":"completed","conclusion":"{ci}","headSha":"%s","jobs":[{{"name":"project tests","status":"completed","conclusion":"{ci}"}}]}}\n' "$oid"
    else exit 1; fi ;;
  "run list")
    printf '%s\n' "$commit" > '{state}.ci-head'
    if [ "$event" = pull_request ]; then
      printf '[{{"databaseId":99,"status":"completed","conclusion":"{ci}","headSha":"%s"}}]\n' "$commit"
    elif [ "$event" = push ] && [ -f '{state}.pushci' ]; then
      printf '[{{"databaseId":98,"status":"completed","conclusion":"{ci}","headSha":"%s"}}]\n' "$commit"
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
fn land_to_github_opens_a_pr_and_merges_only_with_merge() {
    check_publish_then_merge(false);
}

#[cfg(target_os = "linux")]
#[test]
fn released_flock_allows_the_same_published_request_to_merge() {
    check_publish_then_merge(true);
}

fn check_publish_then_merge(flock: bool) {
    let iso = Isolated::new();
    if flock {
        std::os::unix::fs::symlink(which("flock"), iso.bin.join("flock")).unwrap();
    }
    let (repo, bare) = iso.with_github_remote("pr");
    let head = iso.commit_as(
        &repo,
        "feat.txt",
        "feat\n",
        &["feat: Add Feature X, now!", "Users asked for X."],
    );
    let main_before = iso.git_out(&bare, &["rev-parse", "refs/heads/main"]);
    let (gh, log) = iso.fake_gh(&bare, "success");
    let env = [("OMS_GH_BIN", gh.to_str().unwrap())];
    // the gate passes once only, so a resumed land must reuse its result
    let gate = r#"bash -c '! test -e "$HOME/gate-ran" && : > "$HOME/gate-ran"'"#;
    let args = ["--gate", gate, "--ignore-siblings", "--no-update"];
    // the default on a GitHub destination opens a PR and never merges
    let r = iso.run_with_services("land", &repo, &args, &env);
    assert_eq!(r.0, 0, "{r:?}");
    assert!(
        r.1.contains("pull request #7 open: https://example.invalid/pull/7; CI success; not merged \u{2014} pass --merge only when the user asked to merge"),
        "{r:?}"
    );
    assert_eq!(
        iso.git_out(&bare, &["rev-parse", "refs/heads/main"]),
        main_before
    );
    let branch = "refs/heads/oms/feat/add-feature-x-now";
    assert_eq!(iso.git_out(&bare, &["rev-parse", branch]), head);
    let calls = fs::read_to_string(&log).expect("gh log");
    assert!(
        calls.contains("pr create") && !calls.contains("pr merge"),
        "{calls}"
    );
    let body = fs::read_to_string(iso.home.path().join("gh.pr.body")).expect("PR body");
    let short = iso.git_out(&repo, &["rev-parse", "--short", "HEAD"]);
    assert_eq!(
        body,
        format!(
            "## Summary\n- {short} feat: Add Feature X, now!\n\n## Motivation\nUsers asked for X.\n\n## Verification\n- Local gate `{gate}` passed (scope unknown) on exact head {head}\n- `oms land` checks CI on this exact head; merge only with explicit `--merge`\n"
        )
    );
    let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
    for needle in [
        "\"state\": \"awaiting-merge\"",
        "\"reason\": \"awaiting-merge\"",
    ] {
        assert!(status.contains(needle), "{needle}: {status}");
    }
    // --merge (any method) resumes the same receipt and merges after exact-SHA CI
    let merge = ["--merge", "--merge-method", "merge"];
    let r = iso.run_with_services("land", &repo, &[&args[..], &merge].concat(), &env);
    assert_eq!(r.0, 0, "{r:?}");
    assert!(r.1.contains("merged PR #7 (merge)"), "{r:?}");
    let calls = fs::read_to_string(log).expect("gh log");
    let ci = calls
        .rfind(&format!("--commit {head} --event pull_request"))
        .expect("exact-SHA CI query");
    let merge = calls
        .find(&format!(
            "pr merge 7 --repo github.com/owner/pr --merge --delete-branch --match-head-commit {head}"
        ))
        .expect("pr merge");
    assert!(
        ci < merge && calls.matches("pr create").count() == 1,
        "{calls}"
    );
    let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
    for needle in [
        "\"mode\": \"pr\"",
        "\"head_branch\": \"oms/feat/add-feature-x-now\"",
        "\"pr_number\": 7",
        "\"state\": \"passed\"",
        "\"state\": \"MERGED\"",
    ] {
        assert!(status.contains(needle), "{needle}: {status}");
    }
    // the resumed merge keeps the first run's push row, which plan completion
    // reads as the landed range
    let row = V::parse(status.as_bytes()).expect("status json");
    assert_eq!(row.get("push").get("rc"), &0usize.into(), "{status}");
    assert_eq!(row.get("push").get("moved"), &1usize.into(), "{status}");
    assert_eq!(
        row.get("push").get("base").text(),
        Some(main_before.as_str())
    );
    let merged = row.get("merge").get("commit").text().unwrap_or_default();
    assert!(
        merged.len() == 40 && merged.bytes().all(|b| b.is_ascii_hexdigit()),
        "{status}"
    );
}

#[test]
fn pr_ci_yields_to_an_independent_land_and_fences_its_receipt() {
    use std::time::{Duration, Instant};
    let scenarios = [
        "unchanged",
        "receipt-changed",
        "head-moved",
        "target-moved",
        "interrupted",
        "legacy-writer",
        "pr-gone",
        #[cfg(target_os = "linux")]
        "flock-interrupted",
    ];
    // A rerun after the observed green only matters when a merge would follow.
    let cases = scenarios
        .map(|s| (false, s))
        .into_iter()
        .chain(scenarios.map(|s| (true, s)))
        .chain(
            [
                "rerun-pending",
                "rerun-failed",
                "detail-pending",
                "detail-failed",
                "detail-unavailable",
                "detail-malformed",
                "late-target",
                "late-sibling",
                "late-receipt",
            ]
            .map(|s| (true, s)),
        );
    for (merge, scenario) in cases {
        let iso = Isolated::new();
        let flock = scenario == "flock-interrupted";
        let interrupted = flock || scenario == "interrupted";
        if flock {
            std::os::unix::fs::symlink(which("flock"), iso.bin.join("flock")).unwrap();
        }
        let (state, bare) = iso.with_remote("ci-concurrent");
        let a = if scenario == "late-sibling" {
            let pid = std::process::id().to_string();
            let (code, out, err) =
                iso.run("scratch-worktree", &state, &["add", "--owner-pid", &pid]);
            assert_eq!(code, 0, "{err}");
            PathBuf::from(out.trim())
        } else {
            state.clone()
        };
        let b = iso.home.path().join("checkout-b");
        iso.git(
            &a,
            &["worktree", "add", "-q", "-b", "b", b.to_str().unwrap()],
        );
        iso.git(&a, &["checkout", "-qb", "a"]);
        let head = iso.commit_file(&a, "a.txt");
        let independent = iso.commit_file(&b, "b.txt");
        let (gh_a, log) = iso.fake_gh(&bare, "success");
        let original = fs::read_to_string(&gh_a).unwrap();
        let gh_b = iso.home.path().join("gh-b");
        fs::write(
            &gh_b,
            original
                .replace("gh.pr", "gh.pr-b")
                .replace("\"number\":7", "\"number\":8")
                .replace("pull/7", "pull/8"),
        )
        .unwrap();
        fs::set_permissions(&gh_b, fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(which("sleep"), iso.bin.join("sleep")).unwrap();
        let gh_a_script = original
            .replace(
                "\"pr list\")",
                r#""pr list")
    if [ -f "$HOME/pr-gone" ]; then echo '[]'; exit 0; fi
"#,
            )
            .replace(
                "\"run view\")",
                r#""run view")
    if [ -f "$HOME/detail-observed" ] && [ -f "$HOME/detail" ]; then
        IFS= read -r detail < "$HOME/detail"
        if [ "$detail" = unavailable ]; then exit 1; fi
        if [ "$detail" = move ]; then
            IFS= read -r oid < "$HOME/independent-oid"
            IFS= read -r remote < "$HOME/fixture-remote"
            git --git-dir="$remote" update-ref refs/heads/main "$oid" || exit 1
        else
            printf '%s\n' "$detail"
            exit 0
        fi
    fi
    : > "$HOME/detail-observed"
"#,
            );
        fs::write(
            &gh_a,
            gh_a_script.replace(
                "\"run list\")",
                r#""run list")
    if [ -f "$HOME/observed" ] && [ -f "$HOME/rerun" ]; then
        IFS= read -r verdict < "$HOME/rerun"
        printf '[{"databaseId":99,%s,"headSha":"%s"}]\n' "$verdict" "$commit"
        exit 0
    fi
    if [ -f "$HOME/observed" ] && [ -f "$HOME/tamper" ]; then
        # Changed after writer reentry: only the merge-request CAS can see it.
        IFS= read -r target < "$HOME/tamper"
        printf ' ' >> "$target"
        printf '[{"databaseId":99,"status":"completed","conclusion":"success","headSha":"%s"}]\n' "$commit"
        exit 0
    fi
    : > "$HOME/observing"
    for ((tick=0; tick < 500; tick++)); do
        [ ! -f "$HOME/release-ci" ] || break
        sleep 0.025
    done
    [ -f "$HOME/release-ci" ] || exit 1
    : > "$HOME/observed"
"#,
            ),
        )
        .unwrap();
        let publish = [
            "--pr",
            "--why",
            "fixture",
            "--gate",
            "true",
            "--no-update",
            "--ignore-siblings",
        ];
        let merging = [publish.as_slice(), &["--merge"]].concat();
        let guarded = merging
            .iter()
            .copied()
            .filter(|s| *s != "--ignore-siblings")
            .collect::<Vec<_>>();
        let args = if matches!(scenario, "late-sibling" | "late-receipt") {
            guarded.as_slice()
        } else if merge {
            merging.as_slice()
        } else {
            publish.as_slice()
        };
        let main_before = iso.git_out(&bare, &["rev-parse", "refs/heads/main"]);
        if merge && scenario == "unchanged" {
            fs::write(iso.home.path().join("gh.pr.squash"), b"").unwrap();
        }
        let cancel = iso.home.path().join("interrupt");
        let mut env = vec![
            ("OMS_GH_BIN", gh_a.to_str().unwrap()),
            ("OMS_LOCK_TIMEOUT", "2"),
        ];
        let checks = iso.home.path().join("final-sibling-checks");
        if merge && matches!(scenario, "late-sibling" | "late-receipt" | "unchanged") {
            env.push(("OMS_CT_SIBLING_CHECKS", checks.to_str().unwrap()));
            if scenario != "late-receipt" {
                env.push(("OMS_CT_LATE_SIBLING", state.to_str().unwrap()));
            }
        }
        if interrupted {
            env.push(("OMS_CT_INTERRUPT_AT", cancel.to_str().unwrap()));
        }
        std::thread::scope(|scope| {
            let running = scope.spawn(|| iso.run_with_services("land", &a, args, &env));
            let start = Instant::now();
            while !iso.home.path().join("observing").exists() {
                assert!(
                    start.elapsed() < Duration::from_secs(10) && !running.is_finished(),
                    "publisher did not enter CI observation"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            let status = iso.run("land", &a, &["status", "--json", "--active-exit"]);
            assert_eq!(status.0, if flock { 4 } else { 0 }, "{status:?}");
            let row = V::parse(status.1.as_bytes()).unwrap();
            assert_eq!(row.get("pr_number"), &7usize.into());
            let stamp = row.get("request").text().unwrap();
            let directory = fs::read_dir(iso.home.path().join(".local/state/oh-my-setting/land"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let receipt = directory.join(format!("{stamp}.json"));
            let mut expected = fs::read(&receipt).unwrap();
            let observed = iso.run(
                "land",
                &a,
                &[
                    "wait",
                    "--reviewed-head",
                    &head,
                    "--request",
                    stamp,
                    "--json",
                ],
            );
            let result = V::parse(observed.1.as_bytes()).unwrap();
            assert_eq!(result.get("activity").text(), Some("held"));
            assert_eq!(result.get("phase").text(), Some("pr-recorded"));
            let duplicate = iso.run_with_services("land", &a, &merging, &env);
            assert_eq!(duplicate.0, 75, "{duplicate:?}");
            assert_eq!(fs::read(&receipt).unwrap(), expected);
            let mut legacy_guard = None;
            if scenario == "unchanged" {
                let other = iso.run_with_services(
                    "land",
                    &b,
                    &publish,
                    &[
                        ("OMS_GH_BIN", gh_b.to_str().unwrap()),
                        ("OMS_LOCK_TIMEOUT", "2"),
                    ],
                );
                assert_eq!(other.0, 0, "{other:?}");
                assert!(other.1.contains("pull request #8 open"));
                assert_eq!(fs::read(&receipt).unwrap(), expected);
            } else if scenario == "receipt-changed" {
                expected.push(b' ');
                fs::write(&receipt, &expected).unwrap();
            } else if interrupted {
                fs::write(&cancel, b"").unwrap();
            } else if scenario == "pr-gone" {
                fs::write(iso.home.path().join("pr-gone"), b"").unwrap();
            } else if scenario == "late-receipt" {
                let path = format!("{}\n", receipt.display());
                fs::write(iso.home.path().join("tamper"), path).unwrap();
                expected.push(b' ');
            } else if let Some(kind) = scenario.strip_prefix("rerun-") {
                let verdict = if kind == "pending" {
                    "\"status\":\"in_progress\",\"conclusion\":\"\""
                } else {
                    "\"status\":\"completed\",\"conclusion\":\"failure\""
                };
                fs::write(iso.home.path().join("rerun"), format!("{verdict}\n")).unwrap();
            } else if let Some(kind) = scenario.strip_prefix("detail-") {
                let response = match kind {
                    "pending" => {
                        format!(r#"{{"headSha":"{head}","status":"in_progress","conclusion":""}}"#)
                    }
                    "failed" => format!(
                        r#"{{"headSha":"{head}","status":"completed","conclusion":"failure"}}"#
                    ),
                    "malformed" => format!(
                        r#"{{"headSha":"{head}","status":"completed","conclusion":"success","jobs":[{{}}]}}"#
                    ),
                    _ => "unavailable".to_owned(),
                };
                fs::write(iso.home.path().join("detail"), format!("{response}\n")).unwrap();
            } else if matches!(scenario, "target-moved" | "late-target") {
                // The independent commit exists in B but is not on the bare mirror yet.
                iso.git(&b, &["push", "-q", "origin", "HEAD:refs/heads/b"]);
                if scenario == "target-moved" {
                    iso.git(&bare, &["update-ref", "refs/heads/main", &independent]);
                } else {
                    fs::write(iso.home.path().join("detail"), b"move\n").unwrap();
                    fs::write(
                        iso.home.path().join("independent-oid"),
                        format!("{independent}\n"),
                    )
                    .unwrap();
                    fs::write(
                        iso.home.path().join("fixture-remote"),
                        format!("{}\n", bare.display()),
                    )
                    .unwrap();
                }
            } else if scenario == "head-moved" {
                iso.git(&bare, &["update-ref", "refs/heads/a", "refs/heads/main"]);
            } else if scenario == "legacy-writer" {
                // Previous versions take active first, publish their holder, then
                // must take the receipt lock before gate/push/PR/merge.
                let mut settings = oms_lock::Settings::from_env().unwrap();
                settings.home = iso.home.path().into();
                settings.temp_dir = iso.tmp.clone();
                settings.cwd = a.clone();
                settings.search_path = iso.bin.clone().into_os_string();
                settings.lock_dir = None;
                settings.mode = oms_lock::Mode::Mkdir;
                let global =
                    oms_lock::Lock::for_file(&directory.join("active"), settings.clone()).unwrap();
                legacy_guard = Some(global.try_acquire().unwrap());
                fs::write(
                    directory.join("active-holder"),
                    fs::read(directory.join(format!("{stamp}.ci-holder"))).unwrap(),
                )
                .unwrap();
                let legacy_write = oms_lock::Lock::for_file(&receipt, settings)
                    .unwrap()
                    .try_acquire();
                assert!(matches!(legacy_write, Err(oms_lock::Error::Busy)));
            }
            if !interrupted {
                fs::write(iso.home.path().join("release-ci"), b"").unwrap();
            }
            let final_result = running.join().unwrap();
            let context = format!("merge={merge} {scenario}: {final_result:?}");
            let merges = || {
                fs::read_to_string(&log)
                    .unwrap()
                    .lines()
                    .filter(|l| l.starts_with("pr merge"))
                    .count()
            };
            if merge {
                match scenario {
                    "late-sibling" => {
                        assert_ne!(a, state);
                        assert_eq!(
                            fs::read_to_string(&checks).unwrap(),
                            format!("{}\n{}\n", a.display(), state.display())
                        );
                        assert_eq!(final_result.0, 75, "{context}");
                    }
                    "late-receipt" => assert_eq!(
                        fs::read_to_string(&checks).unwrap(),
                        format!("{}\n", a.display())
                    ),
                    "unchanged" => assert!(!checks.exists(), "--ignore-siblings scanned a root"),
                    _ => {}
                }
            }
            if scenario == "unchanged" {
                assert_eq!(final_result.0, 0, "{context}");
                let done = if merge {
                    "merged PR #7"
                } else {
                    "CI success; not merged"
                };
                assert!(final_result.1.contains(done), "{context}");
                let row = V::parse(&fs::read(&receipt).unwrap()).unwrap();
                assert_eq!(
                    row.get("ci").get("scope").text(),
                    Some("unknown"),
                    "{context}"
                );

                assert_eq!(merges(), usize::from(merge), "{context}");
            } else if scenario.starts_with("rerun-") || scenario.starts_with("detail-") {
                assert_eq!(final_result.0, 1, "{context}");
                let row = V::parse(&fs::read(&receipt).unwrap()).unwrap();
                assert_eq!(row.get("state").text(), Some("failed"), "{context}");
                let conclusion = match scenario.split_once('-').unwrap().1 {
                    "failed" => "failure",
                    "pending" => "pending",
                    _ => "unknown",
                };
                assert_eq!(row.get("ci").get("conclusion").text(), Some(conclusion));
                assert!(!row.has("merge"), "{context}");
                assert_eq!(row.get("update").get("reason").text(), Some("not-reached"));
                assert_eq!(merges(), 0, "{context}");
            } else {
                assert_ne!(final_result.0, 0, "{context}");
                assert_eq!(fs::read(&receipt).unwrap(), expected, "{context}");
                assert_eq!(merges(), 0, "{context}");
                if interrupted {
                    assert_eq!(final_result.0, 130, "{context}");
                    fs::write(iso.home.path().join("release-ci"), b"").unwrap();
                    let resume = iso.run_with_services("land", &a, args, &env[..2]);
                    assert_eq!(resume.0, 0, "{resume:?}");
                    let resumed = V::parse(&fs::read(&receipt).unwrap()).unwrap();
                    assert_eq!(resumed.get("request").text(), Some(stamp));
                    let state = if merge { "passed" } else { "awaiting-merge" };
                    assert_eq!(resumed.get("state").text(), Some(state));
                    assert_eq!(merges(), usize::from(merge), "{context}");
                }
                if scenario == "late-sibling" {
                    assert!(
                        final_result.2.contains("fixture sibling active"),
                        "{context}"
                    );
                }
                if legacy_guard.is_some() {
                    assert_eq!(final_result.0, 75, "{context}");
                }
            }
            let target = iso.git_out(&bare, &["rev-parse", "refs/heads/main"]);
            if merge && scenario == "unchanged" {
                assert_ne!(target, main_before);
                assert_eq!(iso.git_out(&bare, &["rev-parse", "main^"]), main_before);
                assert_eq!(
                    iso.git_out(&bare, &["rev-parse", "main^{tree}"]),
                    iso.git_out(&a, &["rev-parse", "HEAD^{tree}"])
                );
            } else if matches!(scenario, "target-moved" | "late-target") {
                assert_eq!(target, independent);
                assert_eq!(iso.git_out(&bare, &["rev-parse", "refs/heads/a"]), head);
            } else {
                assert_eq!(target, main_before);
            }
            let calls = fs::read_to_string(&log).unwrap();
            assert_eq!(
                calls.lines().filter(|l| l.starts_with("pr create")).count(),
                if scenario == "unchanged" { 2 } else { 1 }
            );
            if merge && scenario == "unchanged" {
                // B published while A was still observing, before A merged.
                assert!(calls.rfind("pr create").unwrap() < calls.find("pr merge 7").unwrap());
            }
        });
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
    // a subject-only commit carries no motivation; refused before the gate
    let r = iso.run_with_services("land", &repo, &args, &env);
    assert_refused(
        &r,
        "a pull request needs a motivation: pass --why TEXT or write a commit body",
    );
    let args = [&args[..], &["--why", "fixture"]].concat();
    let r = iso.run_with_services("land", &repo, &[&args[..], &["--merge"]].concat(), &env);
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
    let r = iso.run_with_services(
        "land",
        &repo,
        &[&args[..], &["--merge", "--ci-wait", "0"]].concat(),
        &env,
    );
    assert_refused(&r, "--merge needs --ci-wait above zero");
    // shared sync and receipt handoff act only after a real merge
    let r = iso.run_with_services(
        "land",
        &repo,
        &[&args[..], &["--sync-shared", "shared"]].concat(),
        &env,
    );
    assert_refused(&r, "need --merge in pull-request mode");
    let r = iso.run_env(
        "land",
        &repo,
        &["--gate", "true"],
        &[("OMS_LAND_MODE", "auto")],
    );
    assert_refused(&r, "OMS_LAND_MODE must be pr or direct");
}

#[test]
fn land_rejects_a_foreign_or_mismatched_pr_before_binding_its_receipt() {
    let iso = Isolated::new();
    let (repo, bare) = iso.with_remote("pr-identity");
    iso.git(&repo, &["checkout", "-qb", "candidate"]);
    iso.commit_file(&repo, "candidate.txt");
    let (gh, log) = iso.fake_gh(&bare, "success");
    let original = fs::read_to_string(&gh).unwrap();
    fs::write(iso.home.path().join("gh.pr"), b"").unwrap();
    for (field, replacement) in [
        (
            r#""isCrossRepository":false"#,
            r#""isCrossRepository":true"#,
        ),
        (r#""headRefName":"%s""#, r#""headRefName":"other-%s""#),
        (r#""baseRefName":"%s""#, r#""baseRefName":"other-%s""#),
    ] {
        assert!(original.contains(field));
        fs::write(&gh, original.replace(field, replacement)).unwrap();
        let result = iso.run_with_services(
            "land",
            &repo,
            &[
                "--pr",
                "--merge",
                "--why",
                "fixture",
                "--gate",
                "true",
                "--ignore-siblings",
                "--no-update",
            ],
            &[("OMS_GH_BIN", gh.to_str().unwrap())],
        );
        assert_refused(&result, "open PR does not match");
        let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
        let row = V::parse(status.as_bytes()).unwrap();
        assert!(!row.has("pr_number") && !row.has("pr_url") && !row.has("merge"));
    }
    let calls = fs::read_to_string(log).unwrap();
    assert!(
        !calls.contains("pr merge") && !calls.contains("run list") && !calls.contains("pr create")
    );
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
    let args = [
        "--pr",
        "--why",
        "fixture",
        "--gate",
        r#"bash -c 'git rev-parse HEAD >> "$HOME/gated-pr-heads"'"#,
        "--ignore-siblings",
        "--no-update",
    ];
    let r = iso.run_with_services("land", &repo, &[&args[..], &["--merge"]].concat(), &env);
    assert_eq!(r.0, 1, "{r:?}");
    assert!(
        r.1.contains("diverged") && r.1.contains("merge the existing remote PR head"),
        "{r:?}"
    );
    assert_ne!(iso.git_out(&bare, &["rev-parse", "refs/heads/feat"]), head);
    iso.git(&bare, &["update-ref", "-d", "refs/heads/feat"]);
    let r = iso.run_with_services(
        "land",
        &repo,
        &[&args[..], &["--ci-wait", "0"]].concat(),
        &env,
    );
    assert_eq!(r.0, 0, "{r:?}");
    assert!(r.1.contains("CI skipped; not merged"), "{r:?}");
    let calls = fs::read_to_string(&log).expect("gh log");
    assert!(
        calls.contains("pr create --repo")
            && calls.contains("--head feat --title")
            && !calls.contains("pr merge"),
        "{calls}"
    );

    iso.git(&repo, &["checkout", "-q", "main"]);
    let target = iso.commit_file(&repo, "upstream.txt");
    iso.git(&repo, &["push", "-q", "origin", "main"]);
    iso.git(&repo, &["checkout", "-q", "feat"]);
    iso.commit_file(&repo, "pr-fix.txt");
    let gated = iso.home.path().join("gated-pr-heads");
    let original_gates = fs::read_to_string(&gated).unwrap();
    assert_eq!(original_gates.lines().collect::<Vec<_>>(), [head.as_str()]);
    let stale = iso.run_with_services("land", &repo, &args, &env);
    assert_refused(&stale, "merge it into your PR branch");
    assert_eq!(fs::read_to_string(&gated).unwrap(), original_gates);
    assert_eq!(fs::read_to_string(&log).unwrap(), calls);
    assert_eq!(iso.git_out(&bare, &["rev-parse", "refs/heads/feat"]), head);

    iso.git(&repo, &["fetch", "-q", "origin"]);
    iso.git(&repo, &["merge", "--no-edit", "origin/main"]);
    let updated = iso.git_out(&repo, &["rev-parse", "HEAD"]);
    let r = iso.run_with_services("land", &repo, &args, &env);
    assert_eq!(r.0, 0, "{r:?}");
    assert!(
        r.1.contains("pull request #7 open") && r.1.contains("CI success"),
        "{r:?}"
    );
    assert_eq!(
        iso.git_out(&bare, &["rev-parse", "refs/heads/main"]),
        target
    );
    assert_eq!(
        iso.git_out(&bare, &["rev-parse", "refs/heads/feat"]),
        updated
    );
    iso.git(&bare, &["merge-base", "--is-ancestor", &head, &updated]);
    assert_eq!(
        fs::read_to_string(&gated)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [head.as_str(), updated.as_str()]
    );
    let calls = fs::read_to_string(log).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.starts_with("pr create "))
            .count(),
        1
    );
    assert!(!calls.contains("pr merge"));
}

#[test]
fn land_reconcile_observes_a_merged_pr_after_the_target_moved_and_writes_nothing() {
    check_read_only_reconciliation(false);
}

#[test]
fn land_reconcile_observes_a_squash_during_publish_only_ci_without_finishing_its_receipt() {
    check_read_only_reconciliation(true);
}

fn check_read_only_reconciliation(squash_during_ci: bool) {
    let iso = Isolated::new();
    let (repo, bare) = iso.with_github_remote("recon");
    let head = iso.commit_as(&repo, "feat.txt", "feat\n", &["feat: recover", "Reason."]);
    let main_before = iso.git_out(&bare, &["rev-parse", "refs/heads/main"]);
    let (gh, log) = iso.fake_gh(&bare, "success");
    // Only transport goes to the isolated bare repo. Identity queries observe
    // the actual configured GitHub destination for issuance and reconciliation.
    let public = "https://github.com/owner/recon.git";
    iso.git(&repo, &["remote", "set-url", "origin", public]);
    let git = iso.bin.join("git");
    fs::remove_file(&git).unwrap();
    fs::write(
        &git,
        format!(
            r#"#!{bash}
for a in "$@"; do
    case "$a" in ls-remote|push|fetch)
        exec '{real_git}' -c 'url.{bare}.insteadOf={public}' "$@";;
    esac
done
exec '{real_git}' "$@"
"#,
            bash = which("bash").display(),
            real_git = which("git").display(),
            bare = bare.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(which("cat"), iso.bin.join("cat")).unwrap();
    let fake = fs::read_to_string(&gh)
        .unwrap()
        .replace(
            "https://example.invalid/pull/7",
            "https://github.com/owner/recon/pull/7",
        )
        .replace(
            "case \"$1 $2\" in",
            r#"case "$1 $2" in
  "api --method")
     case "${!#}" in
       repos/owner/recon/actions/workflows/test.yml/runs*event=pull_request*) cat "$HOME/gh.run";;
       repos/owner/recon/actions/runs/99/attempts/1/jobs*) cat "$HOME/gh.jobs";;
       repos/owner/recon/pulls*) cat "$HOME/gh.pulls";;
       *) exit 1;;
     esac ;;"#,
        );
    let fake = if squash_during_ci {
        fake.replace(
            "\"run list\")",
            &format!(r#""run list")
                if [ ! -f "$HOME/squashed" ]; then
                    git --git-dir='{bare}' update-ref refs/heads/main "$(squash "$commit^{{tree}}")" || exit 1
                    : > "$HOME/squashed"
                fi"#, bare = bare.display()),
        )
    } else {
        fake
    };
    fs::write(&gh, fake).unwrap();
    let env = [("OMS_GH_BIN", gh.to_str().unwrap())];
    let args = ["--gate", "true", "--ignore-siblings", "--no-update"];
    let r = iso.run_with_services("land", &repo, &args, &env);
    if squash_during_ci {
        assert_refused(&r, "ref changed during CI");
    } else {
        assert_eq!(r.0, 0, "{r:?}");
    }
    let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
    let receipt = V::parse(status.as_bytes()).expect("status json");
    let (code, observed, err) = iso.run(
        "land",
        &repo,
        &[
            "wait",
            "--reviewed-head",
            &head,
            "--request",
            receipt.get("request").text().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let observed = V::parse(observed.as_bytes()).unwrap();
    assert_eq!(
        observed.get("receipt_state").text(),
        Some(if squash_during_ci {
            "running"
        } else {
            "awaiting-merge"
        })
    );
    assert_eq!(observed.get("phase").text(), Some("pr-recorded"));
    let branch = receipt.get("head_branch").text().unwrap().to_owned();
    // main moved, then the PR merged with exactly the reviewed tree (PR 469 pattern)
    let commit = |tree: &str, parent: &str, msg: &str| {
        iso.git_out(
            &bare,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.invalid",
                "commit-tree",
                tree,
                "-p",
                parent,
                "-m",
                msg,
            ],
        )
    };
    let moved = commit(&format!("{main_before}^{{tree}}"), &main_before, "moved");
    let merge = if squash_during_ci {
        iso.git_out(&bare, &["rev-parse", "refs/heads/main"])
    } else {
        let merge = commit(&format!("{head}^{{tree}}"), &moved, "squash");
        iso.git(&bare, &["update-ref", "refs/heads/main", &merge]);
        merge
    };
    iso.git(&repo, &["fetch", "-q", "origin", "main"]);
    strip_group_world_write(&repo);
    let home = iso.home.path();
    let view = |state: &str, head: &str, cross: bool, merge: &str| {
        fs::write(
            home.join("gh.pr.view"),
            format!(
                r#"{{"number":7,"state":"{state}","url":"https://github.com/owner/recon/pull/7","statusCheckRollup":[{{"__typename":"CheckRun","name":"gate","conclusion":"SUCCESS","detailsUrl":"https://github.com/owner/recon/actions/runs/99/job/101"}}],"headRefOid":"{head}","headRefName":"{branch}","baseRefName":"main","isCrossRepository":{cross},"mergeCommit":{{"oid":"{merge}"}}}}"#
            ),
        )
        .expect("view");
    };
    let job_id = 101;
    let prefix = "https://github.com/owner/recon/actions/runs/99/job";
    let run = format!(
        r#"{{"workflow_runs":[{{"id":99,"run_attempt":1,"head_sha":"{head}","head_branch":"{branch}","status":"completed","conclusion":"success","event":"pull_request","path":".github/workflows/test.yml","repository":{{"full_name":"owner/recon","id":11}},"head_repository":{{"full_name":"owner/recon","id":11}},"pull_requests":[]}}]}}"#
    );
    let pulls = format!(
        r#"[{{"number":7,"head":{{"sha":"{head}","ref":"{branch}","repo":{{"full_name":"owner/recon"}}}},"base":{{"ref":"main","repo":{{"full_name":"owner/recon"}}}}}}]"#
    );
    let jobs = ["gate","lint","focused","native (ubuntu-latest, copy)"].iter().enumerate().map(|(n,name)| {
        let id=job_id+n;
        format!(r#"{{"id":{id},"run_id":99,"run_attempt":1,"head_sha":"{head}","status":"completed","name":"{name}","conclusion":"success","html_url":"{prefix}/{id}"}}"#)
    }).collect::<Vec<_>>().join(",");
    let jobs = format!(r#"{{"total_count":4,"jobs":[{jobs}]}}"#);
    fs::write(home.join("gh.run"), &run).unwrap();
    fs::write(home.join("gh.jobs"), &jobs).unwrap();
    fs::write(home.join("gh.pulls"), &pulls).unwrap();
    view("MERGED", &head, false, &merge);
    let reconcile = |extra: &[&str]| {
        let mut a = vec!["reconcile", "--reviewed-head", head.as_str(), "--json"];
        a.extend(extra);
        let r = iso.run_with_services("land", &repo, &a, &env);
        (r.0, V::parse(r.1.as_bytes()).unwrap_or(V::Null), r)
    };
    let observed = || {
        (
            iso.run("land", &repo, &["status", "--json"]).1,
            iso.git_out(&repo, &["for-each-ref"]),
            iso.git_out(&bare, &["for-each-ref"]),
            iso.git_out(&repo, &["status", "--porcelain"]),
        )
    };
    let before = observed();
    // the existing delivery path still refuses the moved target
    let r = iso.run_with_services("land", &repo, &args, &env);
    assert_ne!(r.0, 0, "{r:?}");
    assert!(r.2.contains("remote target is not an ancestor"), "{r:?}");
    let (code, row, r) = reconcile(&[]);
    assert_eq!(code, 0, "{r:?}");
    assert_eq!(row.get("verdict").text(), Some("verified"), "{r:?}");
    assert_eq!(row.get("reason").text(), Some("same-tree"), "{r:?}");
    assert_eq!(row.get("writes").text(), Some("none"), "{r:?}");
    assert_eq!(
        row.get("merge_commit").text(),
        Some(merge.as_str()),
        "{r:?}"
    );
    assert_eq!(observed(), before);
    if squash_during_ci {
        let stamp = receipt.get("request").text().unwrap();
        let directory = fs::read_dir(home.join(".local/state/oh-my-setting/land"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let path = directory.join(format!("{stamp}.json"));
        let raw = fs::read(&path).unwrap();
        assert_eq!(reconcile(&["--request", stamp]).0, 0);
        assert_eq!(fs::read(&path).unwrap(), raw);
        let mut partial = V::parse(&raw).unwrap();
        partial.set("push", obj([("rc", "pending".into())]));
        fs::write(&path, partial.json(false).unwrap()).unwrap();
        assert_eq!(
            reconcile(&[]).1.get("reason").text(),
            Some("receipt-identity-mismatch")
        );
        assert!(!fs::read_to_string(&log).unwrap().contains("pr merge"));
        return;
    }
    for (what, reason, setup) in [
        ("foreign", "pr-foreign-repository", 0),
        ("moved head", "pr-head-moved", 1),
        ("open", "pr-not-merged", 2),
    ] {
        match setup {
            0 => view("MERGED", &head, true, &merge),
            1 => view("MERGED", &merge, false, &merge),
            _ => view("OPEN", &head, false, &merge),
        }
        let (code, row, r) = reconcile(&[]);
        assert_eq!(code, 1, "{what}: {r:?}");
        assert_eq!(row.get("verdict").text(), Some("refused"), "{what}: {r:?}");
        assert_eq!(row.get("reason").text(), Some(reason), "{what}: {r:?}");
    }
    view("MERGED", &head, false, &merge);
    // a conflicting or stale request selection and extra authority are refused
    let stale = format!("{head}-{}", "0".repeat(24));
    let (code, row, r) = reconcile(&["--request", &stale]);
    assert_eq!(
        (code, row.get("verdict").text()),
        (1, Some("refused")),
        "{r:?}"
    );
    let r = iso.run_with_services(
        "land",
        &repo,
        &["reconcile", "--reviewed-head", &head, "--merge"],
        &env,
    );
    assert_eq!(r.0, 2, "{r:?}");
    let association = format!(
        r#"[{{"number":7,"head":{{"sha":"{head}","ref":"{branch}","repo":{{"id":11}}}},"base":{{"ref":"main","repo":{{"id":11}}}}}}]"#
    );
    let linked = run.replace("[]", &association);
    fs::write(home.join("gh.run"), &linked).unwrap();
    fs::remove_file(home.join("gh.pulls")).unwrap();
    assert_eq!(reconcile(&[]).1.get("reason").text(), Some("same-tree"));
    fs::write(
        home.join("gh.run"),
        linked.replace("\"number\":7", "\"number\":8"),
    )
    .unwrap();
    assert_eq!(
        reconcile(&[]).1.get("reason").text(),
        Some("pr-ci-unverified")
    );
    fs::write(home.join("gh.pulls"), &pulls).unwrap();
    fs::write(home.join("gh.run"), &run).unwrap();
    for (name, original, altered) in [
        ("gh.run", &run, run.replace("pull_request\"", "push\"")),
        ("gh.run", &run, run.replace("test.yml", "other.yml")),
        ("gh.run", &run, run.replace("owner/recon", "outside/recon")),
        ("gh.jobs", &jobs, jobs.replace("job/101", "job/999")),
        (
            "gh.jobs",
            &jobs,
            jobs.replace("run_attempt\":1", "run_attempt\":2"),
        ),
        (
            "gh.pulls",
            &pulls,
            format!("[{0},{0}]", &pulls[1..pulls.len() - 1]),
        ),
    ] {
        fs::write(home.join(name), altered).unwrap();
        let (code, row, r) = reconcile(&[]);
        assert_eq!(
            (code, row.get("reason").text()),
            (1, Some("pr-ci-unverified")),
            "{r:?}"
        );
        fs::write(home.join(name), original).unwrap();
    }
    fs::write(home.join("gh.run"), run.replace("success", "cancelled")).unwrap();
    assert_eq!(
        reconcile(&[]).1.get("reason").text(),
        Some("pr-ci-not-successful")
    );
    fs::write(home.join("gh.run"), run.replace("completed", "queued")).unwrap();
    assert_eq!(reconcile(&[]).1.get("reason").text(), Some("pr-ci-pending"));
    fs::write(home.join("gh.run"), &run).unwrap();
    assert_eq!(reconcile(&[]).1.get("verdict").text(), Some("verified"));
    assert_eq!(observed(), before);
    iso.git(&repo, &["remote", "set-url", "--push", "origin", public]);
    iso.git(
        &repo,
        &[
            "remote",
            "set-url",
            "--add",
            "--push",
            "origin",
            "https://github.com/other/recon.git",
        ],
    );
    assert_eq!(
        reconcile(&[]).1.get("reason").text(),
        Some("destination-ambiguous")
    );
    iso.git(&repo, &["config", "--unset-all", "remote.origin.pushurl"]);
    let manifest_path = home.join("never-created-receipt");
    let refused = iso.run_with_services(
        "land",
        &repo,
        &[
            "reconcile",
            "--check-manifest",
            "missing",
            "--manifest-receipt",
            manifest_path.to_str().unwrap(),
            "--execute-manifest",
            "--reviewed-head",
            &head,
        ],
        &env,
    );
    assert_eq!(refused.0, 2);
    assert!(!manifest_path.exists());
    // a different merged tree on a moved base cannot be proven by name
    let other = commit(&format!("{main_before}^{{tree}}"), &moved, "other");
    iso.git(&bare, &["update-ref", "refs/heads/main", &other]);
    iso.git(&repo, &["fetch", "-q", "origin", "main"]);
    view("MERGED", &head, false, &other);
    let (code, row, r) = reconcile(&[]);
    assert_eq!(code, 1, "{r:?}");
    assert_eq!(row.get("verdict").text(), Some("unverified"), "{r:?}");
    assert_eq!(
        row.get("reason").text(),
        Some("merged-tree-differs"),
        "{r:?}"
    );
    // An observer cannot substitute a local replacement tree for the GitHub merge.
    iso.git(&repo, &["replace", &other, &head]);
    assert_eq!(
        reconcile(&[]).1.get("reason").text(),
        Some("merged-tree-differs")
    );
    iso.git(&repo, &["replace", "-d", &other]);
    // Matching first parent + a green run cannot certify an unrelated tree.
    let direct = commit(
        &format!("{main_before}^{{tree}}"),
        &main_before,
        "unrelated",
    );
    iso.git(&bare, &["update-ref", "refs/heads/main", &direct]);
    iso.git(&repo, &["fetch", "-q", "origin", "main"]);
    view("MERGED", &head, false, &direct);
    assert_eq!(
        reconcile(&[]).1.get("reason").text(),
        Some("merged-tree-differs")
    );
    // an untrusted receipt is never read
    let found = Command::new("find")
        .args([home.to_str().unwrap(), "-name", &format!("{head}-*.json")])
        .output()
        .expect("find");
    let path = String::from_utf8_lossy(&found.stdout).trim().to_owned();
    assert!(!path.is_empty() && !path.contains('\n'), "{path}");
    let saved = fs::read(&path).unwrap();
    for (field, value) in [("state", "running"), ("repo", "elsewhere")] {
        let mut wrong = V::parse(&saved).unwrap();
        wrong.set(field, value);
        fs::write(&path, wrong.json(false).unwrap()).unwrap();
        assert_eq!(
            reconcile(&[]).1.get("reason").text(),
            Some("receipt-identity-mismatch")
        );
    }
    fs::write(&path, &saved).unwrap();
    let stamp = receipt.get("request").text().unwrap();
    for index in 0..2048 {
        fs::write(
            Path::new(&path)
                .parent()
                .unwrap()
                .join(format!("scan-{index}")),
            [],
        )
        .unwrap();
    }
    assert_eq!(
        reconcile(&[]).1.get("reason").text(),
        Some("receipt-scan-limit-use-request")
    );
    assert_eq!(
        reconcile(&["--request", stamp]).1.get("reason").text(),
        Some("merged-tree-differs")
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).expect("chmod");
    let (code, row, r) = reconcile(&["--request", stamp]);
    assert_eq!(code, 1, "{r:?}");
    assert_eq!(row.get("reason").text(), Some("receipt-untrusted"), "{r:?}");
    let calls = fs::read_to_string(log).expect("gh log");
    assert!(
        !calls.contains("pr merge") && calls.matches("pr create").count() == 1,
        "{calls}"
    );
}

#[test]
fn land_wait_follows_only_the_exact_private_receipt_and_holder() {
    let iso = Isolated::new();
    let (repo, _) = iso.with_remote("observe");
    let head = iso.commit_file(&repo, "next.txt");
    let result = iso.run_with_services(
        "land",
        &repo,
        &["--gate", "exit 1", "--ignore-siblings"],
        &[],
    );
    assert_eq!(result.0, 1, "{result:?}");
    let row = V::parse(iso.run("land", &repo, &["status", "--json"]).1.as_bytes()).unwrap();
    let stamp = row.get("request").text().unwrap();
    let directory = fs::read_dir(iso.home.path().join(".local/state/oh-my-setting/land"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let path = directory.join(format!("{stamp}.json"));
    let holder_path = directory.join("active-holder");
    let saved = fs::read(&path).unwrap();
    let saved_holder = fs::read(&holder_path).unwrap();
    let wait = |request: &str, extra: &[&str]| {
        let mut args = vec![
            "wait",
            "--reviewed-head",
            &head,
            "--request",
            request,
            "--json",
        ];
        args.extend(extra);
        iso.run("land", &repo, &args)
    };
    let json = |r: (i32, String, String)| {
        assert_eq!(r.0, 0, "{r:?}");
        V::parse(r.1.as_bytes()).unwrap()
    };
    let first = json(wait(stamp, &[]));
    assert_eq!(first.get("event").text(), Some("terminal"));
    assert_eq!(first.get("receipt_state").text(), Some("failed"));
    let cursor = first.get("cursor").text().unwrap();
    assert_eq!(
        json(wait(stamp, &["--after", cursor, "--wait-seconds", "50"])),
        first
    );
    let foreign = format!("{head}-{}", "a".repeat(24));
    assert_ne!(wait(&foreign, &[]).0, 0);
    assert!(
        wait(&foreign, &["--after", cursor])
            .2
            .contains("cursor does not match")
    );
    for args in [
        vec!["--wait-seconds", "51"],
        vec!["--merge"],
        vec!["--request", stamp],
    ] {
        assert_ne!(wait(stamp, &args).0, 0);
    }
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert_eq!(fs::read(&holder_path).unwrap(), saved_holder);

    // If the remote head already existed, the PR writer records the exact PR
    // and publication state without recording a successful push of its own.
    let mut preexisting = V::parse(&saved).unwrap();
    preexisting.set("state", "awaiting-merge");
    preexisting.set("mode", "pr");
    preexisting.set("gate", obj([("rc", 0usize.into())]));
    preexisting.set("pr_number", 7usize);
    preexisting.set("pr_url", "https://example.invalid/pull/7");
    fs::write(&path, preexisting.json(false).unwrap()).unwrap();
    assert_eq!(
        json(wait(stamp, &[])).get("phase").text(),
        Some("pr-recorded")
    );

    let mut running = V::parse(&saved).unwrap();
    running.set("state", "running");
    running.set(
        "gate",
        obj([
            ("rc", "pending".into()),
            ("command", "private fixture text".into()),
        ]),
    );
    fs::write(&path, running.json(false).unwrap()).unwrap();
    let orphan = json(wait(stamp, &[]));
    assert_eq!(orphan.get("event").text(), Some("holder-unconfirmed"));
    assert_eq!(orphan.get("activity").text(), Some("not-held"));

    let mut settings = oms_lock::Settings::from_env().unwrap();
    settings.home = iso.home.path().to_owned();
    settings.cwd = repo.clone();
    settings.temp_dir = iso.tmp.clone();
    settings.lock_dir = None;
    settings.mode = oms_lock::Mode::Mkdir;
    let lock = oms_lock::Lock::for_file(&directory.join("active"), settings.clone()).unwrap();
    let guard = lock.try_acquire().unwrap();
    let pid = std::process::id();
    let birth = oms_command_operations::lock::process_start(pid).unwrap();
    let mut holder = obj([
        ("receipt", format!("{stamp}.json").into()),
        ("sha", head.clone().into()),
        ("pid", (pid as usize).into()),
        ("start", birth.into()),
    ]);
    fs::write(&holder_path, holder.json(false).unwrap()).unwrap();
    let live = json(wait(stamp, &["--after", cursor]));
    assert_eq!(live.get("event").text(), Some("changed"));
    assert_eq!(live.get("activity").text(), Some("held"));
    let cursor = live.get("cursor").text().unwrap();
    let before = (fs::read(&path).unwrap(), fs::read(&holder_path).unwrap());
    let end = std::thread::scope(|s| {
        let observe = || json(wait(stamp, &["--after", cursor, "--wait-seconds", "1"]));
        let second = s.spawn(observe);
        let first = observe();
        assert_eq!(second.join().unwrap(), first);
        first
    });
    assert_eq!(end.get("event").text(), Some("timeout"));
    assert_eq!(end.get("cursor").text(), Some(cursor));
    assert_eq!(
        before,
        (fs::read(&path).unwrap(), fs::read(&holder_path).unwrap())
    );
    let output = end.json(false).unwrap();
    let output = std::str::from_utf8(&output).unwrap();
    for private in [
        "private fixture text",
        "command",
        "pid",
        repo.to_str().unwrap(),
        stamp,
    ] {
        assert!(!output.contains(private), "{output}");
    }
    holder.set("start", "not-this-process");
    fs::write(&holder_path, holder.json(false).unwrap()).unwrap();
    assert_eq!(
        json(wait(stamp, &["--after", cursor]))
            .get("activity")
            .text(),
        Some("process-ended")
    );
    holder.set("receipt", format!("{foreign}.json"));
    fs::write(&holder_path, holder.json(false).unwrap()).unwrap();
    assert_eq!(
        json(wait(stamp, &[])).get("activity").text(),
        Some("unmatched")
    );
    drop(guard);
    #[cfg(target_os = "linux")]
    {
        // A released flock inode alone cannot identify a holder. Only a live
        // global writer with this exact checkout request is a positive match.
        std::os::unix::fs::symlink(which("flock"), iso.bin.join("flock")).unwrap();
        settings.mode = oms_lock::Mode::Flock;
        let owner = oms_lock::Lock::for_file(
            &directory.join(format!("{stamp}.ci-owner")),
            settings.clone(),
        )
        .unwrap();
        owner.try_acquire().unwrap().release().unwrap();
        let global = oms_lock::Lock::for_file(&directory.join("active"), settings).unwrap();
        let live = global.try_acquire().unwrap();
        assert_eq!(
            json(wait(stamp, &[])).get("activity").text(),
            Some("unverified")
        );
        holder.set("receipt", format!("{stamp}.json"));
        holder.set(
            "start",
            oms_command_operations::lock::process_start(pid).unwrap(),
        );
        let bytes = holder.json(false).unwrap();
        fs::write(&holder_path, &bytes).unwrap();
        assert_eq!(json(wait(stamp, &[])).get("activity").text(), Some("held"));
        assert_eq!(fs::read(&holder_path).unwrap(), bytes);
        live.release().unwrap();
        assert_eq!(
            json(wait(stamp, &[])).get("activity").text(),
            Some("unverified")
        );
    }
    running.set("repo", "unrelated");
    fs::write(&path, running.json(false).unwrap()).unwrap();
    assert_ne!(wait(stamp, &[]).0, 0);
    fs::write(&path, &saved).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    assert_ne!(wait(stamp, &[]).0, 0);
}

#[test]
fn land_pr_installs_a_tree_equal_squash_and_defers_when_the_base_moved() {
    for moved in [false, true] {
        let iso = Isolated::new();
        let (repo, bare) = iso.with_github_remote("prsquash");
        iso.commit_as(
            &repo,
            "scripts/check.sh",
            "exit 0\n",
            &["fix: add gate", "Fixture gate."],
        );
        let (gh, log) = iso.fake_gh(&bare, "success");
        fs::write(iso.home.path().join("gh.pr.squash"), "").expect("squash flag");
        if moved {
            fs::write(iso.home.path().join("gh.pr.moved"), "").expect("moved flag");
        }
        let r = iso.run_with_services(
            "land",
            &repo,
            &[
                "--merge",
                "--gate",
                "bash scripts/check.sh --parallel",
                "--ignore-siblings",
            ],
            &[("OMS_GH_BIN", gh.to_str().unwrap())],
        );
        assert_eq!(r.0, 0, "{r:?}");
        let calls = fs::read_to_string(&log).expect("gh log");
        assert!(calls.contains("--squash --delete-branch"), "{calls}");
        let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
        let row = V::parse(status.as_bytes()).expect("status json");
        assert_eq!(
            row.get("merge").get("tree_equal"),
            &V::Bool(true),
            "{status}"
        );
        if moved {
            assert_eq!(
                row.get("update").get("reason").text(),
                Some("merge-commit-pending-ci"),
                "{status}"
            );
        } else {
            assert_eq!(row.get("update").get("rc"), &0usize.into(), "{status}");
            assert_eq!(row.get("state").text(), Some("passed"), "{status}");
            assert_eq!(row.get("push").get("moved"), &1usize.into(), "{status}");
            let merged = row.get("merge").get("commit").text().unwrap_or_default();
            assert_eq!(row.get("merge").get("state").text(), Some("MERGED"));
            assert_eq!(
                iso.git_out(&repo, &["rev-parse", &format!("{merged}^")]),
                row.get("push").get("base").text().unwrap_or_default()
            );
        }
    }
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
    // a GitHub target takes a direct push only for a docs-only change of at most 2 lines
    let (repo, bare) = iso.with_github_remote("directgh");
    let main = iso.git_out(&bare, &["rev-parse", "refs/heads/main"]);
    let refusal = "direct land to main is limited to a docs-only change of at most 2 lines; land without --direct to open a pull request";
    iso.commit_file(&repo, "code.rs");
    let r = iso.run_with_services("land", &repo, &args, &[("OMS_LAND_MODE", "direct")]);
    assert_refused(&r, refusal);
    assert_eq!(iso.git_out(&bare, &["rev-parse", "refs/heads/main"]), main);
    iso.git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
    iso.commit_as(&repo, "README.md", "one\ntwo\nthree\n", &["docs: grow"]);
    let r = iso.run_with_services("land", &repo, &[&args[..], &["--direct"]].concat(), &[]);
    assert_refused(&r, refusal);
    iso.git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
    let head = iso.commit_as(&repo, "README.md", "one\ntwo\n", &["docs: note"]);
    let r = iso.run_with_services("land", &repo, &[&args[..], &["--direct"]].concat(), &[]);
    assert_eq!(r.0, 0, "{r:?}");
    assert_eq!(iso.git_out(&bare, &["rev-parse", "refs/heads/main"]), head);
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
fn batch_preview_pins_the_order_and_leaves_source_unchanged() {
    let iso = Isolated::new();
    let repo = iso.repo("ordered-preview");
    let version = iso.git_out(&repo, &["version"]);
    let mut version = version.split_whitespace().nth(2).unwrap().split('.');
    let major = version.next().unwrap().parse::<u32>().unwrap();
    let minor = version.next().unwrap().parse::<u32>().unwrap();
    if (major, minor) < (2, 38) {
        let branch = "refs/heads/main";
        let (code, out, err) = iso.run("land", &repo, &["batch-preview", "--base", branch, branch]);
        assert_eq!(code, 2, "{out}{err}");
        assert!(err.contains("requires Git 2.38"), "{err}");
        return;
    }
    let base = iso.git_out(&repo, &["rev-parse", "HEAD"]);
    let branch = |name: &str, from: &str, file: &str, content: &str| {
        iso.git(&repo, &["checkout", "-qb", name, from]);
        fs::write(repo.join(file), content).unwrap();
        iso.git(&repo, &["add", "--", file]);
        iso.git(&repo, &["commit", "-qm", name]);
        iso.git_out(&repo, &["rev-parse", "HEAD"])
    };
    let first = "refs/heads/oms/first";
    let first_alias = "refs/remotes/origin/first";
    let stacked = "refs/heads/oms/stacked";
    let independent = "refs/remotes/origin/independent";
    let conflicting = "refs/heads/oms/conflicting";
    let target = "refs/remotes/origin/main";
    let first_oid = branch("oms/first", &base, "a.txt", "first\n");
    let stacked_oid = branch("oms/stacked", &first_oid, "stacked.txt", "stacked\n");
    let independent_oid = branch("oms/independent", &base, "independent.txt", "solo\n");
    let conflicting_oid = branch("oms/conflicting", &base, "a.txt", "other\n");
    iso.git(&repo, &["checkout", "-q", "main"]);
    iso.git(&repo, &["update-ref", target, &base]);
    iso.git(&repo, &["update-ref", first_alias, &first_oid]);
    iso.git(&repo, &["update-ref", independent, &independent_oid]);
    fs::write(repo.join("local.txt"), "user work\n").unwrap();
    let source = || {
        (
            iso.git_out(
                &repo,
                &["for-each-ref", "--format=%(refname) %(objectname)"],
            ),
            iso.git_out(&repo, &["rev-parse", "HEAD"]),
            iso.git_out(&repo, &["worktree", "list", "--porcelain"]),
            iso.git_out(&repo, &["cat-file", "--batch-all-objects", "--batch-check"]),
            fs::read(repo.join(".git/index")).unwrap(),
            fs::read_to_string(repo.join("local.txt")).unwrap(),
        )
    };
    let before = source();
    let preview = |candidates: &[&str], code: i32| {
        let mut args = vec!["batch-preview", "--base", target, "--json"];
        args.extend_from_slice(candidates);
        let (actual, out, err) = iso.run("land", &repo, &args);
        assert_eq!(actual, code, "{out}{err}");
        V::parse(out.as_bytes()).unwrap()
    };
    let clean = preview(&[first, independent, stacked, first_alias], 0);
    assert_eq!(clean.get("status").text(), Some("clean"));
    assert_eq!(clean.get("base").get("oid").text(), Some(base.as_str()));
    let rows = clean.get("steps").array();
    assert_eq!(rows[0].get("oid").text(), Some(first_oid.as_str()));
    assert_eq!(rows[1].get("oid").text(), Some(independent_oid.as_str()));
    assert_eq!(rows[2].get("oid").text(), Some(stacked_oid.as_str()));
    assert_eq!(rows[2].get("depends_on").array().len(), 1);
    assert_eq!(rows[2].get("depends_on").array()[0].text(), Some(first));
    assert_eq!(rows[3].get("ref").text(), Some(first_alias));
    assert_eq!(rows[3].get("oid").text(), Some(first_oid.as_str()));
    assert_eq!(rows[3].get("status").text(), Some("already_present"));

    assert_eq!(
        preview(&[conflicting], 0).get("status").text(),
        Some("clean")
    );
    let user_git_config = iso.home.path().join(".config/git");
    fs::create_dir_all(&user_git_config).unwrap();
    let user_attributes = user_git_config.join("attributes");
    fs::write(&user_attributes, "a.txt merge=union\n").unwrap();
    let blocked = preview(
        &[first, independent, stacked, conflicting, "refs/heads/main"],
        1,
    );
    assert_eq!(blocked.get("status").text(), Some("conflict"));
    assert!(matches!(blocked.get("tree_oid"), V::Null));
    assert_eq!(
        blocked.get("conflict_paths").array()[0].text(),
        Some("a.txt")
    );
    let rows = blocked.get("steps").array();
    assert_eq!(rows[3].get("oid").text(), Some(conflicting_oid.as_str()));
    assert_eq!(rows[3].get("status").text(), Some("conflict"));
    assert_eq!(rows[4].get("status").text(), Some("blocked"));
    fs::remove_file(user_attributes).unwrap();

    let wrong_order = preview(&[stacked, first_alias, first], 3);
    let rows = wrong_order.get("steps").array();
    assert_eq!(rows[0].get("status").text(), Some("out_of_order"));
    assert_eq!(rows[0].get("depends_on").array().len(), 1);
    assert_eq!(
        rows[0].get("depends_on").array()[0].text(),
        Some(first_alias)
    );
    assert_eq!(rows[1].get("status").text(), Some("blocked"));
    assert_eq!(rows[2].get("status").text(), Some("blocked"));
    assert!(matches!(wrong_order.get("tree_oid"), V::Null));
    for extra in ["--pr", "--merge", "--local-merge", "--execute-manifest"] {
        let (code, out, err) = iso.run(
            "land",
            &repo,
            &["batch-preview", "--base", target, first, extra],
        );
        assert_eq!(code, 2, "{out}{err}");
        assert!(err.contains("accepts only"), "{err}");
    }
    assert_eq!(source(), before);
    assert!(!repo.join(".oms").exists());
    assert!(fs::read_dir(&iso.tmp).unwrap().next().is_none());

    // Simulate an independent ref update after pinning, during merge-tree.
    let git = iso.bin.join("git");
    let real_git = fs::read_link(&git).unwrap();
    fs::remove_file(&git).unwrap();
    fs::write(
        &git,
        r#"#!/bin/bash
for arg in "$@"; do
    if [[ "$arg" == merge-tree ]]; then
        "$PREVIEW_TEST_GIT" -C "$PREVIEW_TEST_REPO" update-ref "$PREVIEW_TEST_REF" "$PREVIEW_TEST_OID" || exit $?
        break
    fi
done
exec "$PREVIEW_TEST_GIT" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
    let (code, out, err) = iso.run_env(
        "land",
        &repo,
        &["batch-preview", "--base", target, first, "--json"],
        &[
            ("PREVIEW_TEST_GIT", real_git.to_str().unwrap()),
            ("PREVIEW_TEST_REPO", repo.to_str().unwrap()),
            ("PREVIEW_TEST_REF", first),
            ("PREVIEW_TEST_OID", &conflicting_oid),
        ],
    );
    assert_eq!(code, 3, "{out}{err}");
    let stale = V::parse(out.as_bytes()).unwrap();
    assert_eq!(stale.get("status").text(), Some("stale"));
    assert_eq!(stale.get("moved_refs").array()[0].text(), Some(first));
    assert!(matches!(stale.get("tree_oid"), V::Null));
    fs::remove_file(&git).unwrap();
    std::os::unix::fs::symlink(real_git, git).unwrap();
    iso.git(&repo, &["update-ref", first, &first_oid]);
    assert_eq!(source(), before);
    assert!(fs::read_dir(&iso.tmp).unwrap().next().is_none());

    iso.git(&repo, &["checkout", "-qb", "expected", &base]);
    for head in [first, independent, stacked] {
        iso.git(&repo, &["merge", "-q", "--no-ff", "--no-edit", head]);
    }
    let expected = iso.git_out(&repo, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(clean.get("tree_oid").text(), Some(expected.as_str()));
}

#[cfg(target_os = "linux")]
#[test]
fn batch_check_copied_candidates_are_bounded_and_cannot_supply_hosted_ci() {
    let iso = Isolated::new();
    let repo = iso.repo("ordered-check");
    let version = iso.git_out(&repo, &["version"]);
    let mut parts = version.split_whitespace().nth(2).unwrap().split('.');
    if (
        parts.next().unwrap().parse::<u32>().unwrap(),
        parts.next().unwrap().parse::<u32>().unwrap(),
    ) < (2, 38)
    {
        return;
    }
    fs::create_dir(repo.join("scripts")).unwrap();
    fs::write(
        repo.join("scripts/check.sh"),
        r#"#!/bin/bash
set -eu
[[ "$1 $2 $3 $5 $6" == "--affected --ci-parity --changed-from --changed-to HEAD" ]]
git merge-base --is-ancestor "$4" HEAD
[[ -z "$(git remote)" && ! -e .git/objects/info/alternates && ! -e user-untracked ]]
if [[ ${OMS_CT_BATCH_MODE:-} == output ]]; then
    for ((i = 0; i < 1024; i++)); do
        printf 'private-verifier-output\n'
    done
fi
[[ ! -e first-flag || ! -e final-flag ]]
"#,
    )
    .unwrap();
    fs::write(
        repo.join(".gitattributes"),
        "final-flag filter=fixture-smudge\n",
    )
    .unwrap();
    iso.git(&repo, &["add", "scripts/check.sh", ".gitattributes"]);
    iso.git(&repo, &["commit", "-qm", "verifier"]);
    let base = iso.git_out(&repo, &["rev-parse", "HEAD"]);
    let pin = |name: &str, from: &str, file: &str| {
        iso.git(&repo, &["checkout", "-qb", name, from]);
        fs::write(repo.join(file), "fixture\n").unwrap();
        iso.git(&repo, &["add", file]);
        iso.git(&repo, &["commit", "-qm", name]);
        iso.git_out(&repo, &["rev-parse", "HEAD"])
    };
    let first_oid = pin("oms/first", &base, "first-flag");
    let stacked_oid = pin("oms/stacked", &first_oid, "stacked-flag");
    let side_oid = pin("oms/side", &base, "final-flag");
    iso.git(&repo, &["checkout", "-q", "main"]);
    fs::write(
        repo.join("user-untracked"),
        "untouched
",
    )
    .unwrap();
    let base_ref = "refs/heads/main";
    let first = "refs/heads/oms/first";
    let stacked = "refs/heads/oms/stacked";
    let side = "refs/heads/oms/side";
    let names = [first, stacked, side];
    let state = || {
        (
            iso.git_out(&repo, &["rev-parse", "HEAD"]),
            iso.git_out(
                &repo,
                &["for-each-ref", "--format=%(refname) %(objectname)"],
            ),
            iso.git_out(&repo, &["worktree", "list", "--porcelain"]),
            iso.git_out(&repo, &["cat-file", "--batch-all-objects", "--batch-check"]),
            fs::read(repo.join(".git/index")).unwrap(),
            fs::read(repo.join("user-untracked")).unwrap(),
        )
    };
    let before = state();
    let preview = |refs: &[&str]| {
        let mut args = vec!["batch-preview", "--base", base_ref, "--json"];
        args.extend(refs);
        let (code, out, err) =
            iso.run_with_services("land", &repo, &args, &[("OMS_CT_BATCH_MODE", "never")]);
        assert_eq!(code, 0, "{out}{err}");
        V::parse(out.as_bytes()).unwrap()
    };
    let checked = |refs: &[&str], digest: &str, mode: &str, expected_code| {
        let mut args = vec![
            "batch-check",
            "--base",
            base_ref,
            "--expected-preview-sha256",
            digest,
            "--json",
        ];
        args.extend(refs);
        let (code, out, err) =
            iso.run_with_services("land", &repo, &args, &[("OMS_CT_BATCH_MODE", mode)]);
        assert_eq!(code, expected_code, "{out}{err}");
        assert!(!out.contains("private-verifier-output"), "{out}");
        assert!(out.len() < 8192, "unexpected diagnostic size");
        (out.clone(), V::parse(out.as_bytes()).unwrap())
    };
    let full = preview(&names);
    let digest = full.get("preview_sha256").text().unwrap();
    assert_eq!(digest.len(), 64);
    assert_eq!(
        full.get("steps").array()[1].get("depends_on").array(),
        &[V::from(first)]
    );
    let (_, stale) = checked(&[first, side, stacked], digest, "never", 3);
    assert_eq!(stale.get("status").text(), Some("stale"));
    assert!(stale.get("checks").array().is_empty());
    iso.git(&repo, &["update-ref", side, &stacked_oid, &side_oid]);
    let (_, stale) = checked(&names, digest, "never", 3);
    assert_eq!(stale.get("status").text(), Some("stale"));
    assert!(stale.get("checks").array().is_empty());
    iso.git(&repo, &["update-ref", side, &side_oid, &stacked_oid]);
    assert_eq!(state(), before);

    let mut race = vec![
        "batch-check",
        "--base",
        base_ref,
        "--expected-preview-sha256",
        digest,
        "--json",
    ];
    race.extend(names);
    let (code, out, err) = iso.run_with_services(
        "land",
        &repo,
        &race,
        &[
            ("OMS_CT_BATCH_MOVE_REF", side),
            ("OMS_CT_BATCH_MOVE_TO", &stacked_oid),
            ("OMS_CT_BATCH_MOVE_FROM", &side_oid),
        ],
    );
    assert_eq!(code, 3, "{out}{err}");
    let stale = V::parse(out.as_bytes()).unwrap();
    assert_eq!(stale.get("moved_refs").array(), &[V::from(side)]);
    assert_eq!(
        stale.get("checks").array().len(),
        1,
        "stale run cannot launch a prefix"
    );
    assert_eq!(
        stale.get("checks").array()[0].get("tree_oid"),
        full.get("tree_oid")
    );
    iso.git(&repo, &["update-ref", side, &side_oid, &stacked_oid]);
    assert_eq!(state(), before);

    let (_, outcome) = checked(&names, digest, "", 1);
    assert_eq!(outcome.get("status").text(), Some("failed"));
    assert_eq!(outcome.get("hosted_ci_evidence"), &V::Bool(false));
    let rows = outcome.get("checks").array();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("tree_oid"), full.get("tree_oid"));
    assert_eq!(rows[0].get("candidate_count").py(), "3");
    assert_eq!(rows[0].get("outcome").text(), Some("failed"));
    assert_eq!(
        rows[1].get("tree_oid"),
        full.get("steps").array()[1].get("tree_oid")
    );
    assert_eq!(rows[1].get("candidate_count").py(), "2");
    assert_eq!(rows[1].get("outcome").text(), Some("passed"));
    assert!(
        rows.iter()
            .all(|row| row.get("scope").text() == Some("affected-ci-parity")
                && row.get("duration_ms").integer(false))
    );

    for (mode, code, status) in [
        ("cancel", 130, "cancelled"),
        ("output", 125, "output_limited"),
        ("setup-unavailable", 2, "unavailable"),
    ] {
        let (_, result) = checked(&names, digest, mode, code);
        assert_eq!(result.get("status").text(), Some(status));
        assert_eq!(result.get("checks").array().len(), 1);
    }
    let alone = preview(&[side]);
    let sentinel = iso.home.path().join("checkout-filter-ran");
    let global_config = iso.home.path().join(".gitconfig");
    let filter = format!(
        "[filter \"fixture-smudge\"]\n    smudge = \"printf fixture > '{}'; /bin/cat\"\n",
        sentinel.display(),
    );
    fs::write(&global_config, filter).unwrap();
    let (code, local_success, error) = iso.run_with_services(
        "land",
        &repo,
        &[
            "batch-check",
            "--base",
            base_ref,
            "--expected-preview-sha256",
            alone.get("preview_sha256").text().unwrap(),
            side,
            "--json",
        ],
        &[
            ("GIT_CONFIG_GLOBAL", global_config.to_str().unwrap()),
            ("GIT_CONFIG_NOSYSTEM", "0"),
        ],
    );
    assert!(
        !sentinel.exists(),
        "private Git copy ran an ambient smudge filter"
    );
    assert_eq!(code, 0, "{local_success}{error}");
    let parsed = V::parse(local_success.as_bytes()).unwrap();
    assert_eq!(parsed.get("status").text(), Some("passed"));
    assert_eq!(parsed.get("hosted_ci_evidence"), &V::Bool(false));
    let gh = iso.bin.join("gh");
    fs::write(
        &gh,
        b"#!/bin/bash\nprintf '%s' \"$OMS_CT_BATCH_DIAGNOSTIC\"\n",
    )
    .unwrap();
    fs::set_permissions(gh, fs::Permissions::from_mode(0o700)).unwrap();
    let (code, output, error) = iso.run_env(
        "ci-status",
        &repo,
        &[],
        &[("OMS_CT_BATCH_DIAGNOSTIC", &local_success)],
    );
    assert_eq!(code, 2, "{output}{error}");
    assert!(error.contains("unreadable run list"), "{output}{error}");

    assert_eq!(state(), before);
    assert!(!repo.join(".oms").exists());
    assert!(fs::read_dir(&iso.tmp).unwrap().next().is_none());
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

const GH_PROJECT: &str = "# P\n\n## Collaboration\n\n- GitHub project: example-org/example-repo\n";
const GH_COMMENTS: &str = "repos/example-org/example-repo/issues/3/comments?per_page=100&page=1";
const GH_PULL: &str = "repos/example-org/example-repo/pulls/7";
const GH_MERGED: &str = r#""state":"closed","merged_at":"2026-11-01T00:00:00Z","merged":true"#;
const GH_POST: [&str; 6] = ["post-evidence", "--issue", "3", "--pull", "7", "--yes"];
const GH_USER_GET: &str = "api --method GET -H Accept: application/vnd.github+json user";

/// `oms github` sandbox: a repo with a github.com origin and a bash-builtin
/// fake `gh` that logs argv, answers GETs from `GET-<endpoint>` files, and
/// turns a POST into the stored comment GitHub would list, answering with
/// that comment. Switch files in `dir` alter the answer: `post.fail` exits 1,
/// `post.interrupt` dies of SIGINT, and `post.issue`, `post.login`,
/// `post.htmlseg`, `post.htmlissue`, `post.frag` and `post.body` replace the
/// echoed issue_url number, author, html_url segment, number and fragment id,
/// and JSON-escaped body. Endpoints match case-insensitively, as on GitHub.
struct GhRepo {
    repo: PathBuf,
    gh: PathBuf,
    log: PathBuf,
    dir: PathBuf,
}
impl GhRepo {
    fn new(iso: &Isolated, name: &str, project: bool) -> Self {
        let repo = iso.repo(name);
        iso.git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:example-org/example-repo.git",
            ],
        );
        if project {
            fs::write(repo.join("PROJECT.md"), GH_PROJECT).unwrap();
        }
        let dir = iso.home.path().join(format!("{name}-gh"));
        fs::create_dir_all(&dir).unwrap();
        let (gh, log) = (dir.join("fake-gh"), dir.join("gh.log"));
        let script = format!(
            r#"#!{bash}
printf '%s\n' "$*" >> '{log}'
dir='{dir}' method=GET endpoint= body=
args=("$@")
after=
for ((i = 0; i < ${{#args[@]}}; i++)); do
  case "${{args[i]}}" in
    after=*) after=${{args[i]#after=}} ;;
  esac
done
after_key=${{after//[^a-zA-Z0-9]/_}}
case "$1 $2" in
  "pr list") panel=panel-prs ;;
  "issue list") panel=panel-issues ;;
  "issue view"|"pr view") panel=panel-lookup ;;
  "api graphql")
    case "$*" in
      *'projectV2(number'*)
        panel=panel-board
        if [ -n "$after_key" ] && {{ [ -f "$dir/panel-board-after-$after_key" ] || [ -f "$dir/panel-board-after-$after_key-timeout" ]; }}; then
          panel="panel-board-after-$after_key"
        fi ;;
      *'work:issues('*)
        panel=panel-inventory
        if [ -n "$after_key" ] && {{ [ -f "$dir/panel-inventory-issues-after-$after_key" ] || [ -f "$dir/panel-inventory-issues-after-$after_key-timeout" ]; }}; then
          panel="panel-inventory-issues-after-$after_key"
        elif [ -f "$dir/panel-inventory-issues" ] || [ -f "$dir/panel-inventory-issues-timeout" ]; then
          panel=panel-inventory-issues
        fi ;;
      *'work:pullRequests('*)
        panel=panel-inventory
        if [ -n "$after_key" ] && {{ [ -f "$dir/panel-inventory-pulls-after-$after_key" ] || [ -f "$dir/panel-inventory-pulls-after-$after_key-timeout" ]; }}; then
          panel="panel-inventory-pulls-after-$after_key"
        elif [ -f "$dir/panel-inventory-pulls" ] || [ -f "$dir/panel-inventory-pulls-timeout" ]; then
          panel=panel-inventory-pulls
        fi ;;
      *) panel=panel-projects ;;
    esac ;;
  *) panel= ;;
esac
if [ -n "$panel" ]; then
  if [ -f "$dir/$panel-timeout" ]; then exit 124; fi
  [ -f "$dir/$panel" ] || exit 1
  printf '%s\n' "$(<"$dir/$panel")"
  if [ -f "$dir/$panel-failed" ]; then exit 1; fi
  exit 0
fi
for ((i = 0; i < ${{#args[@]}}; i++)); do
  case "${{args[i]}}" in
    --method) method=${{args[i+1]}} ;;
    repos/*|user) endpoint=${{args[i]}} ;;
    body=*) body=${{args[i]#body=}} ;;
  esac
done
key=${{endpoint,,}}; key=${{key//[^a-z0-9]/_}}
if [ "$method" = POST ]; then
  e=${{body//\\/\\\\}}; e=${{e//\"/\\\"}}; e=${{e//$'\t'/\\t}}; e=${{e//$'\r'/\\r}}; e=${{e//$'\n'/\\n}}
  api=https://api.github.com/repos/example-org/example-repo/issues
  web=https://github.com/example-org/example-repo
  printf '[{{"id":4242,"user":{{"login":"oms-bot"}},"body":"%s","issue_url":"%s/3","html_url":"%s/issues/3#issuecomment-4242"}}]\n' "$e" "$api" "$web" > "$dir/GET-${{key}}_per_page_100_page_1"
  [ -f "$dir/post.fail" ] && exit 1
  [ -f "$dir/post.interrupt" ] && {{ kill -INT $$; exit 130; }}
  issue=3 login=oms-bot seg=issues hissue=3 frag=4242
  [ -f "$dir/post.issue" ] && issue=$(<"$dir/post.issue")
  [ -f "$dir/post.htmlseg" ] && seg=$(<"$dir/post.htmlseg")
  [ -f "$dir/post.htmlissue" ] && hissue=$(<"$dir/post.htmlissue")
  [ -f "$dir/post.login" ] && login=$(<"$dir/post.login")
  [ -f "$dir/post.frag" ] && frag=$(<"$dir/post.frag")
  [ -f "$dir/post.body" ] && e=$(<"$dir/post.body")
  printf '{{"id":4242,"user":{{"login":"%s"}},"body":"%s","issue_url":"%s/%s","html_url":"%s/%s/%s#issuecomment-%s"}}\n' "$login" "$e" "$api" "$issue" "$web" "$seg" "$hissue" "$frag"
  exit 0
fi
[ -f "$dir/GET-$key" ] || exit 1
printf '%s\n' "$(<"$dir/GET-$key")"
"#,
            bash = which("bash").display(),
            log = log.display(),
            dir = dir.display(),
        );
        fs::write(&gh, script).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
        Self { repo, gh, log, dir }
    }
    fn respond(&self, endpoint: &str, body: &str) {
        let key: String = endpoint
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        fs::write(self.dir.join(format!("GET-{key}")), body).unwrap();
    }
    /// A declared repo whose pull #7 is cached as merged, authenticated as
    /// `oms-bot`, with an empty comment list on issue #3.
    fn ready(iso: &Isolated, name: &str) -> Self {
        let r = Self::new(iso, name, true);
        r.respond(
            GH_PULL,
            &gh_item("pull", 7, "example-org/example-repo", GH_MERGED),
        );
        let (code, _, err) = r.run(iso, &["refresh", "--pull", "7", "--ttl-s", "600"]);
        assert_eq!(code, 0, "{err}");
        r.respond(GH_COMMENTS, "[]");
        r.respond("user", r#"{"login":"oms-bot"}"#);
        r
    }
    fn run(&self, iso: &Isolated, args: &[&str]) -> (i32, String, String) {
        self.run_in(iso, &self.repo, args)
    }
    fn run_in(&self, iso: &Isolated, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        iso.run_env(
            "github",
            cwd,
            args,
            &[("OMS_GH_BIN", self.gh.to_str().unwrap())],
        )
    }
    fn calls(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
    fn posts(&self) -> usize {
        self.calls().matches("--method POST").count()
    }
    fn user_gets(&self) -> usize {
        self.calls().lines().filter(|l| *l == GH_USER_GET).count()
    }
    fn cache(&self) -> Option<String> {
        fs::read_to_string(self.repo.join(".oms/github/cache.jsonl")).ok()
    }
    fn ledger_path(&self) -> PathBuf {
        self.repo.join(".oms/github/writes.jsonl")
    }
    fn ledger(&self) -> String {
        fs::read_to_string(self.ledger_path()).unwrap_or_default()
    }
    fn phases(&self) -> Vec<String> {
        self.ledger()
            .lines()
            .map(|l| {
                let tail = l.split("\"phase\":\"").nth(1).expect("phase field");
                tail.split('"').next().unwrap().to_owned()
            })
            .collect()
    }
}

/// The evidence marker for issue #3 and pull #7 of the sandbox origin.
fn gh_marker() -> String {
    let key =
        Sha256::digest(b"post-evidence|example-org/example-repo#3|example-org/example-repo#7");
    format!("<!-- oms-evidence:{key:x} -->")
}

/// A one-comment list page as GitHub returns it for `issue`; `body` must
/// already be JSON-escaped.
fn gh_comment(id: u32, login: &str, body: &str, issue: u32) -> String {
    format!(
        r#"[{{"id":{id},"user":{{"login":"{login}"}},"body":"{body}","issue_url":"https://api.github.com/repos/example-org/example-repo/issues/{issue}","html_url":"https://github.com/example-org/example-repo/issues/{issue}#issuecomment-{id}"}}]"#
    )
}

/// The JSON-escaped comment post-evidence sends for issue #3 and pull #7.
fn gh_evidence(receipt: &str) -> String {
    format!(
        "OMS merge evidence for example-org/example-repo#3\\n\\n- Pull request: \
         example-org/example-repo#7\\n- Merge state: merged\\n- Land receipt: {receipt}\\n\\n{}\\n",
        gh_marker()
    )
}

/// Write a land receipt into `repo`'s `delivery::state_directory`, whose
/// name ends in the POSIX cksum of the repository path.
fn gh_land_receipt(iso: &Isolated, repo: &Path, file: &str, url: &str, sha: &str) {
    let raw = repo.as_os_str().as_encoded_bytes();
    let mut crc = 0u32;
    let mut add = |b: u8| {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    };
    raw.iter().for_each(|b| add(*b));
    let mut size = raw.len();
    while size != 0 {
        add((size & 255) as u8);
        size >>= 8;
    }
    let name = repo.file_name().unwrap().to_str().unwrap();
    let state = iso.home.path().join(".local/state");
    let dir = state
        .join("oh-my-setting/land")
        .join(format!("{name}-{}-{}", !crc, raw.len()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(file),
        format!(r#"{{"pr_number":7,"pr_url":"{url}","sha":"{sha}"}}"#),
    )
    .unwrap();
    strip_group_world_write(&state);
}

fn gh_item(kind: &str, number: u32, slug: &str, extra: &str) -> String {
    let (html, api) = if kind == "issue" {
        ("issues", "issues")
    } else {
        ("pull", "pulls")
    };
    format!(
        r#"{{"number":{number},"html_url":"https://github.com/{slug}/{html}/{number}","url":"https://api.github.com/repos/{slug}/{api}/{number}","title":"Fix\u0007 LEAK-title","labels":[{{"name":"bug"}}],"body":"LEAK-body",{extra}}}"#
    )
}

#[test]
fn github_status_is_offline_and_refresh_caches_only_validated_rows() {
    let iso = Isolated::new();
    let r = GhRepo::new(&iso, "ghstatus", true);
    fs::create_dir_all(r.repo.join(".oms/task")).unwrap();
    fs::write(
        r.repo.join(".oms/task/current.md"),
        "# Task\n\n- github_issue: example-org/example-repo#3\n\n## Goal\n\nx\n",
    )
    .unwrap();
    fs::create_dir_all(r.repo.join(".oms/plan")).unwrap();
    fs::write(
        r.repo.join(".oms/plan/tasks.json"),
        r#"{"tasks":{"T1":{"issue":"example-org/example-repo#3"}}}"#,
    )
    .unwrap();
    let (code, out, err) = r.run(&iso, &["status", "--json"]);
    assert_eq!(code, 0, "{err}");
    for needle in [
        "\"project\": \"example-org/example-repo\"",
        "\"origin\": \"example-org/example-repo\"",
        "\"freshness\": \"unavailable\"",
        "\"task\": \"T1\"",
    ] {
        assert!(out.contains(needle), "{needle}: {out}");
    }
    let report = V::parse(out.as_bytes()).unwrap();
    let navigation = report.get("navigation");
    for (key, url) in [
        ("repository", "https://github.com/example-org/example-repo"),
        (
            "issues",
            "https://github.com/example-org/example-repo/issues",
        ),
        (
            "pull_requests",
            "https://github.com/example-org/example-repo/pulls",
        ),
        (
            "actions",
            "https://github.com/example-org/example-repo/actions",
        ),
        ("wiki", "https://github.com/example-org/example-repo/wiki"),
    ] {
        assert_eq!(navigation.get(key).text(), Some(url), "{key}: {out}");
    }
    let links = report.get("linked").array();
    assert_eq!(links.len(), 2);
    for (row, source) in links.iter().zip(["task", "plan"]) {
        assert_eq!(row.get("source").text(), Some(source));
        assert_eq!(
            row.get("url").text(),
            Some("https://github.com/example-org/example-repo/issues/3")
        );
    }
    assert!(r.calls().is_empty() && r.cache().is_none());
    let issue = "repos/example-org/example-repo/issues/3";
    let open = r#""state":"open""#;
    for (case, body) in [
        (
            "wrong repo",
            gh_item("issue", 3, "other-org/example-repo", open),
        ),
        (
            "number",
            gh_item("issue", 4, "example-org/example-repo", open),
        ),
        (
            "state",
            gh_item("issue", 3, "example-org/example-repo", r#""state":"x""#),
        ),
        (
            "pull as issue",
            gh_item(
                "issue",
                3,
                "example-org/example-repo",
                r#""state":"open","pull_request":{}"#,
            ),
        ),
        ("malformed", r#"{"number":3,"#.to_owned()),
        (
            "duplicate key",
            format!(
                "{{\"number\":4,{}",
                &gh_item("issue", 3, "example-org/example-repo", open)[1..]
            ),
        ),
    ] {
        r.respond(issue, &body);
        let (code, out, err) = r.run(&iso, &["refresh", "--issue", "3"]);
        assert_ne!(code, 0, "{case}: {out}");
        assert!(!err.contains("LEAK"), "{case}: {err}");
        assert!(r.cache().is_none(), "{case}");
    }
    fs::remove_file(
        r.dir
            .join(format!("GET-{}", issue.replace(['/', '-'], "_"))),
    )
    .unwrap();
    let (code, _, err) = r.run(&iso, &["refresh", "--issue", "3"]);
    assert_ne!(code, 0);
    assert!(err.contains("gh not authenticated"), "{err}");
    assert!(r.cache().is_none());
    let (code, _, err) = r.run(&iso, &["refresh", "--issue", "other-org/example-repo#3"]);
    assert_eq!(code, 2);
    assert!(err.contains("same-repository"), "{err}");

    r.respond(
        issue,
        &gh_item("issue", 3, "example-org/example-repo", open),
    );
    let (code, out, err) = r.run(&iso, &["refresh", "--linked", "--ttl-s", "600"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "refreshed example-org/example-repo#3 (issue, open)\n");
    let cache = r.cache().unwrap();
    assert_eq!(cache.lines().count(), 1, "{cache}");
    assert!(cache.contains("\"Fix  LEAK-title\"") && cache.contains("\"ttl_s\":600"));
    assert!(!cache.contains("LEAK-body") && cache.contains("\"body_sha256\":"));
    let before = r.calls();
    let (code, out, _) = r.run(&iso, &["status"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("task: example-org/example-repo#3 [fresh] open Fix  LEAK-title")
            && out.contains("plan T1: example-org/example-repo#3 [fresh]"),
        "{out}"
    );
    for line in [
        "repository: https://github.com/example-org/example-repo",
        "issues: https://github.com/example-org/example-repo/issues",
        "pull requests: https://github.com/example-org/example-repo/pulls",
        "actions: https://github.com/example-org/example-repo/actions",
        "wiki: https://github.com/example-org/example-repo/wiki",
    ] {
        assert!(out.lines().any(|actual| actual == line), "{out}");
    }
    assert_eq!(
        out.matches("https://github.com/example-org/example-repo/issues/3")
            .count(),
        2
    );
    assert_eq!(r.calls(), before, "status must never invoke gh");

    fs::write(
        r.repo.join("PROJECT.md"),
        GH_PROJECT.replace("example-org/", "other-org/"),
    )
    .unwrap();
    let (code, out, _) = r.run(&iso, &["status"]);
    assert_eq!(code, 1);
    assert!(
        out.contains("error: PROJECT.md declares a different GitHub project"),
        "{out}"
    );
    assert!(!out.contains("https://github.com/"), "{out}");
    let (code, out, _) = r.run(&iso, &["status", "--json"]);
    assert_eq!(code, 1, "{out}");
    let report = V::parse(out.as_bytes()).unwrap();
    assert_eq!(report.get("navigation"), &V::Null);
    assert!(
        report
            .get("linked")
            .array()
            .iter()
            .all(|row| row.get("url") == &V::Null)
    );
    let (code, _, err) = r.run(&iso, &["refresh", "--issue", "3"]);
    assert_eq!(code, 2);
    assert!(err.contains("different GitHub project"), "{err}");
    assert_eq!(r.calls(), before);

    // A clean checkout has no PROJECT.md: the committed declaration alone
    // turns the mode on, and only its HEAD blob counts.
    let plain = GhRepo::new(&iso, "ghplain", false);
    let (code, out, err) = plain.run(&iso, &["status"]);
    assert_eq!(
        (code, out.lines().next()),
        (0, Some("project mode: off")),
        "{err}"
    );
    let t = GhRepo::new(&iso, "ghtracked", false);
    let tracked = t.repo.join(".github/oms-project.md");
    fs::create_dir_all(tracked.parent().unwrap()).unwrap();
    fs::write(&tracked, GH_PROJECT).unwrap();
    iso.git(&t.repo, &["add", ".github/oms-project.md"]);
    iso.git(&t.repo, &["commit", "-q", "-m", "declare"]);
    strip_group_world_write(&t.repo);
    fs::write(&tracked, GH_PROJECT.replace("example-org/", "other-org/")).unwrap();
    let (code, out, err) = t.run(&iso, &["status", "--json"]);
    assert_eq!(code, 0, "{err}");
    for needle in [
        "\"project\": \"example-org/example-repo\"",
        "\"project_source\": \"tracked\"",
        "\"project_active\": true",
    ] {
        assert!(out.contains(needle), "{needle}: {out}");
    }
    assert_eq!(
        V::parse(out.as_bytes())
            .unwrap()
            .get("navigation")
            .get("repository")
            .text(),
        Some("https://github.com/example-org/example-repo")
    );
    fs::write(t.repo.join("PROJECT.md"), GH_PROJECT).unwrap();
    let (code, out, _) = t.run(&iso, &["status"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.starts_with("project mode: on (example-org/example-repo, both)\n"),
        "{out}"
    );
    // A fork inherits the upstream's committed declaration without opting in.
    fs::remove_file(t.repo.join("PROJECT.md")).unwrap();
    iso.git(
        &t.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:fork-org/example-repo.git",
        ],
    );
    fs::create_dir_all(t.repo.join(".oms/task")).unwrap();
    fs::write(
        t.repo.join(".oms/task/current.md"),
        "# Task\n\n- github_issue: #3\n",
    )
    .unwrap();
    let (code, out, err) = t.run(&iso, &["status"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.starts_with(
            "project mode: inactive (.github/oms-project.md names example-org/example-repo; \
             origin is fork-org/example-repo)\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("task: fork-org/example-repo#3 [unavailable]"),
        "{out}"
    );
    assert!(!out.contains("https://github.com/"), "{out}");
    let (code, out, _) = t.run(&iso, &["status", "--json"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("\"project_active\": false")
            && out.contains("\"project_inactive_reason\": \"fork\""),
        "{out}"
    );
    let report = V::parse(out.as_bytes()).unwrap();
    assert_eq!(report.get("navigation"), &V::Null);
    let links = report.get("linked").array();
    assert_eq!(links.len(), 1, "{out}");
    assert_eq!(
        links[0].get("reference").text(),
        Some("fork-org/example-repo#3")
    );
    assert_eq!(links[0].get("url"), &V::Null);
    for args in [
        &["refresh", "--linked"][..],
        &["post-evidence", "--issue", "3", "--pull", "7", "--yes"],
    ] {
        let (code, _, err) = t.run(&iso, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(
            err.contains("project mode is not active"),
            "{args:?}: {err}"
        );
    }
    iso.git(&t.repo, &["remote", "remove", "origin"]);
    let (code, out, err) = t.run(&iso, &["status"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.starts_with(
            "project mode: inactive (.github/oms-project.md names example-org/example-repo; \
             origin is missing or not a github.com remote)\n"
        ),
        "{out}"
    );
    let (_, out, _) = t.run(&iso, &["status", "--json"]);
    assert!(
        out.contains("\"project_inactive_reason\": \"origin\""),
        "{out}"
    );
    assert_eq!(
        plain.calls() + &t.calls(),
        "",
        "status must never invoke gh"
    );
}

#[test]
fn github_adopt_merged_applies_only_the_previewed_selection() {
    let iso = Isolated::new();
    let r = GhRepo::new(&iso, "ghadopt", true);
    let list =
        "repos/example-org/example-repo/pulls?state=closed&sort=updated&direction=desc&per_page=5";
    let merged = r#""state":"closed","merged_at":"2026-11-01T00:00:00Z""#;
    let unmerged = r#""state":"closed","merged_at":null"#;
    let listing = format!(
        "[{},{}]",
        gh_item("pull", 7, "example-org/example-repo", merged),
        gh_item("pull", 8, "example-org/example-repo", unmerged)
    );
    r.respond(list, &listing);
    let preview = |r: &GhRepo| {
        let (code, out, err) = r.run(&iso, &["adopt-merged", "--limit", "5"]);
        assert_eq!(code, 0, "{err}");
        let sha = out
            .lines()
            .find_map(|l| l.strip_prefix("selection-sha256: "))
            .expect("selection sha")
            .to_owned();
        (out, sha)
    };
    let (out, sha) = preview(&r);
    assert!(
        out.contains("example-org/example-repo#7  Fix  LEAK-title") && !out.contains("#8"),
        "{out}"
    );
    assert!(r.cache().is_none(), "the preview writes nothing");
    for args in [
        vec!["adopt-merged", "--limit", "5", "--apply"],
        vec![
            "adopt-merged",
            "--limit",
            "5",
            "--apply",
            "--expected-sha256",
            &"0".repeat(64),
        ],
        vec!["adopt-merged", "--limit", "51"],
        vec!["adopt-merged", "--limit", "5", "--expected-sha256", &sha],
    ] {
        let (code, _, err) = r.run(&iso, &args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(r.cache().is_none(), "{args:?}");
    }
    let apply: [&str; 6] = [
        "adopt-merged",
        "--limit",
        "5",
        "--apply",
        "--expected-sha256",
        &sha,
    ];
    // A title edit between the preview and --apply changes the selection.
    r.respond(list, &listing.replace("LEAK-title", "LEAK-edited"));
    let (code, _, err) = r.run(&iso, &apply);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("rerun the preview") && r.cache().is_none(),
        "{err}"
    );
    // '|' survives title and label sanitizing, so field boundaries must be
    // part of the digest: moving it from the title into a label is an edit.
    let fields = r#""title":"Fix\u0007 LEAK-title","labels":[{"name":"bug"}]"#;
    let split = |title: &str, label: &str| {
        listing.replace(
            fields,
            &format!(r#""title":"{title}","labels":[{{"name":"{label}"}}]"#),
        )
    };
    r.respond(list, &split("Fix|LEAK", "title"));
    let (_, moved) = preview(&r);
    assert_ne!(moved, sha);
    r.respond(list, &split("Fix", "LEAK|title"));
    let (code, _, err) = r.run(
        &iso,
        &[
            "adopt-merged",
            "--limit",
            "5",
            "--apply",
            "--expected-sha256",
            &moved,
        ],
    );
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("rerun the preview") && r.cache().is_none(),
        "{err}"
    );
    r.respond(list, &listing);
    let (code, out, err) = r.run(&iso, &apply);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("adopted 1 merged pull request(s)"), "{out}");
    let cache = r.cache().unwrap();
    assert_eq!(cache.lines().count(), 1);
    assert!(
        cache.contains("\"reference\":\"example-org/example-repo#7\"")
            && cache.contains("\"state\":\"merged\"")
    );
}

#[test]
fn github_post_evidence_is_gated_and_never_reposts() {
    let iso = Isolated::new();
    let off = GhRepo::new(&iso, "ghoff", false);
    let (code, _, err) = off.run(&iso, &["post-evidence", "--issue", "3", "--pull", "7"]);
    assert_eq!(code, 2);
    assert!(err.contains("pass --yes"), "{err}");
    let args = GH_POST;
    let (code, _, err) = off.run(&iso, &args);
    assert_eq!(code, 2);
    assert!(err.contains("requires PROJECT.md"), "{err}");
    assert_eq!(off.calls(), "");

    for (name, fail) in [("ghpost", false), ("ghunknown", true)] {
        let r = GhRepo::new(&iso, name, true);
        let (code, _, err) = r.run(&iso, &args);
        assert_eq!(code, 2);
        assert!(err.contains("run `oms github refresh --pull REF`"), "{err}");
        assert_eq!(r.calls(), "", "no cache row, no network");
        r.respond(
            GH_PULL,
            &gh_item("pull", 7, "example-org/example-repo", GH_MERGED),
        );
        assert_eq!(
            r.run(&iso, &["refresh", "--pull", "7", "--ttl-s", "600"]).0,
            0
        );
        // A forged marker by another author must not suppress the post.
        r.respond(GH_COMMENTS, &gh_comment(1, "intruder", &gh_marker(), 3));
        r.respond("user", r#"{"login":"OMS-bot"}"#);
        // The same pull number on another remote is not this pull's receipt.
        let foreign = "f".repeat(40);
        gh_land_receipt(
            &iso,
            &r.repo,
            "other.json",
            "https://github.com/other-org/other-repo/pull/7",
            &foreign,
        );
        if fail {
            fs::write(r.dir.join("post.fail"), "").unwrap();
        }
        let (code, out, err) = r.run(&iso, &args);
        if fail {
            assert_eq!(code, 1, "{out}");
            assert!(err.contains("outcome is unknown"), "{err}");
            assert_eq!(r.phases(), ["prepared", "unknown"], "{}", r.ledger());
        } else {
            assert_eq!(code, 0, "{err}");
            assert_eq!(
                out,
                "posted evidence comment 4242 on example-org/example-repo#3\n"
            );
            assert_eq!(r.phases(), ["prepared", "complete"], "{}", r.ledger());
        }
        let text = r.ledger();
        assert!(text.contains("Merge state: merged") && !text.contains("oms-evidence"));
        assert!(
            text.contains("Land receipt: not recorded locally")
                && !text.contains(&foreign)
                && !r.calls().contains(&foreign),
            "{text}"
        );
        if fail {
            // A retry resends only the prepared body, as the prepared account.
            let (head, newest) = text.trim_end().rsplit_once('\n').unwrap();
            let sha = format!("`{}`", "a".repeat(40));
            let marker = gh_marker();
            let key = marker
                .strip_prefix("<!-- oms-evidence:")
                .and_then(|m| m.strip_suffix(" -->"))
                .unwrap();
            for (n, tampered) in [
                text.replace(key, &"0".repeat(64)),
                text.replace(
                    r#""target":"example-org/example-repo#3""#,
                    r##""target":"#3""##,
                ),
                text.replace(
                    "Pull request: example-org/example-repo#7",
                    "Pull request: example-org/example-repo#8",
                ),
                text.replace("not recorded locally", "not recorded locally (edited)"),
                // A renderable body on the newest row alone still disagrees.
                format!("{head}\n{}\n", newest.replace("not recorded locally", &sha)),
            ]
            .into_iter()
            .enumerate()
            {
                assert_ne!(tampered, text, "edit {n}");
                fs::write(r.ledger_path(), &tampered).unwrap();
                let (code, _, err) = r.run(&iso, &args);
                assert_eq!(code, 2, "edit {n}: {err}");
                assert!(
                    err.contains(
                        "evidence receipt is inconsistent; repair .oms/github/writes.jsonl"
                    ),
                    "edit {n}: {err}"
                );
                assert_eq!(r.ledger(), tampered, "edit {n}: no receipt row is added");
            }
            fs::write(r.ledger_path(), &text).unwrap();
            r.respond("user", r#"{"login":"other-bot"}"#);
            let before = r.user_gets();
            let (code, _, err) = r.run(&iso, &args);
            assert_eq!(code, 2, "{err}");
            assert!(
                err.contains(
                    "authenticated GitHub account differs from the one that prepared this \
                     evidence; nothing was posted"
                ),
                "{err}"
            );
            assert_eq!(r.user_gets(), before + 1, "a retry re-reads the account");
            assert_eq!(r.ledger(), text);
            r.respond("user", r#"{"login":"OMS-bot"}"#);
            // An own marker comment reconciles only with the prepared body.
            let exact = gh_evidence("not recorded locally");
            let edited = exact.replace("Merge state: merged", "Merge state: open");
            r.respond(GH_COMMENTS, &gh_comment(4242, "oms-bot", &edited, 3));
            let (code, _, err) = r.run(&iso, &args);
            assert_eq!(code, 2, "{err}");
            assert!(
                err.contains(
                    "evidence marker comment 4242 does not carry the prepared receipt body; \
                     nothing was posted"
                ),
                "{err}"
            );
            assert_eq!((r.posts(), r.ledger()), (1, text.clone()));
            // The prepared body without its final newline still reconciles.
            let listed = exact.strip_suffix("\\n").unwrap();
            r.respond(GH_COMMENTS, &gh_comment(4242, "oms-bot", listed, 3));
        }
        // Another spelling of the same origin maps to the same receipt, and a
        // retry under it keeps the receipt's own spellings.
        let respell = || {
            iso.git(
                &r.repo,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "https://github.com/Example-Org/Example-Repo",
                ],
            )
        };
        if fail {
            respell();
        }
        let users = r.user_gets();
        let (code, out, err) = r.run(&iso, &args);
        assert_eq!(code, 0, "{err}");
        if fail {
            assert!(
                out.contains("already present on example-org/example-repo#3 (comment 4242)"),
                "{out}"
            );
            assert_eq!(r.phases(), ["prepared", "unknown", "complete"]);
            assert_eq!(r.user_gets(), users + 1, "a retry re-reads the account");
            let ledger = r.ledger();
            let complete = ledger.lines().last().unwrap();
            assert!(
                complete.contains(r#""target":"example-org/example-repo#3""#)
                    && complete.contains(r#""pull":"example-org/example-repo#7""#),
                "{complete}"
            );
        } else {
            assert!(out.contains("already posted"), "{out}");
            assert_eq!(r.phases(), ["prepared", "complete"]);
            assert_eq!(r.user_gets(), users, "a completed receipt is offline");
            // A complete row must still name a real comment id.
            let text = r.ledger();
            let tampered = text.replace(r#""comment_id":"4242""#, r#""comment_id":"x""#);
            assert_ne!(tampered, text);
            fs::write(r.ledger_path(), &tampered).unwrap();
            let (code, _, err) = r.run(&iso, &args);
            assert_eq!(code, 2, "{err}");
            assert!(
                err.contains("evidence receipt is inconsistent; repair .oms/github/writes.jsonl"),
                "{err}"
            );
            assert_eq!(r.ledger(), tampered);
            fs::write(r.ledger_path(), &text).unwrap();
        }
        respell();
        let (code, out, err) = r.run(&iso, &args);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("already posted"), "{out}");
        assert_eq!(r.posts(), 1, "{}", r.calls());
    }
}

/// Only a POST answer naming the target issue, the exact sent body and the
/// authenticated author completes a receipt; any other answer, or an
/// interrupt, leaves it unknown for a marker-checked rerun.
#[test]
fn github_post_evidence_completes_only_on_a_confirmed_answer() {
    let iso = Isolated::new();
    let marker = gh_marker();
    let foreign = GhRepo::ready(&iso, "ghforeign");
    foreign.respond(GH_COMMENTS, &gh_comment(9, "oms-bot", &marker, 4));
    let (code, _, err) = foreign.run(&iso, &GH_POST);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("GitHub comment list does not belong to the target issue; nothing was posted"),
        "{err}"
    );
    assert_eq!(foreign.posts(), 0);
    assert!(
        !foreign.phases().iter().any(|p| p == "complete"),
        "{}",
        foreign.ledger()
    );

    let r = GhRepo::ready(&iso, "ghconfirm");
    let edited = format!("Edited\\n{marker}\\n");
    for (n, (switch, value)) in [
        ("post.issue", "4"),
        ("post.login", "intruder"),
        (
            "post.body",
            "OMS merge evidence for example-org/example-repo#3",
        ),
        ("post.body", edited.as_str()),
        ("post.frag", "4243"),
        ("post.htmlissue", "4"),
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(r.dir.join(switch), value).unwrap();
        let (code, out, err) = r.run(&iso, &GH_POST);
        assert_eq!(code, 1, "{switch}={value}: {out}{err}");
        assert!(
            err.contains("comment POST response did not confirm the target issue, body and author"),
            "{switch}={value}: {err}"
        );
        let phases = r.phases();
        assert!(
            phases.first().is_some_and(|p| p == "prepared")
                && phases.last().is_some_and(|p| p == "unknown")
                && !phases.iter().any(|p| p == "complete"),
            "{switch}={value}: {}",
            r.ledger()
        );
        assert_eq!(r.posts(), n + 1, "{switch}={value}");
        if value == edited {
            // That answer's own marker comment is not the prepared one either.
            r.respond(GH_COMMENTS, &gh_comment(4242, "oms-bot", value, 3));
            let ledger = r.ledger();
            let (code, _, err) = r.run(&iso, &GH_POST);
            assert_eq!(code, 2, "{err}");
            assert!(
                err.contains(
                    "evidence marker comment 4242 does not carry the prepared receipt body"
                ),
                "{err}"
            );
            assert_eq!((r.posts(), r.ledger()), (n + 1, ledger));
        }
        fs::remove_file(r.dir.join(switch)).unwrap();
        // Hide the stored comment so the next variant posts again.
        r.respond(GH_COMMENTS, "[]");
    }
    // GitHub may link an issue comment through the pull web path.
    fs::write(r.dir.join("post.htmlseg"), "pull").unwrap();
    let (code, out, err) = r.run(&iso, &GH_POST);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "posted evidence comment 4242 on example-org/example-repo#3\n"
    );
    assert_eq!(r.phases().last().map(String::as_str), Some("complete"));

    let r = GhRepo::ready(&iso, "ghcancel");
    fs::write(r.dir.join("post.interrupt"), "").unwrap();
    let (code, _, err) = r.run(&iso, &GH_POST);
    assert_eq!(code, 130, "{err}");
    assert!(
        err.contains("interrupted") && err.contains("outcome is unknown"),
        "{err}"
    );
    assert_eq!(r.phases(), ["prepared", "unknown"], "{}", r.ledger());
    fs::remove_file(r.dir.join("post.interrupt")).unwrap();
    let (code, out, err) = r.run(&iso, &GH_POST);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("already present on example-org/example-repo#3 (comment 4242)"),
        "{out}"
    );
    assert_eq!(r.phases(), ["prepared", "unknown", "complete"]);
    assert_eq!(r.posts(), 1, "{}", r.calls());
}

/// A registered scratch checkout resolves to the primary checkout's state
/// repository, so every checkout shares one cache and one write ledger.
#[test]
fn github_state_is_shared_by_scratch_checkouts() {
    let iso = Isolated::new();
    // Only the execution checkout declares the project; state still goes
    // to the primary.
    let r = GhRepo::new(&iso, "ghshared", false);
    let scratch = iso
        .home
        .path()
        .join(".cache/oh-my-setting/worktrees/oh-my-setting-scratch.gh1");
    fs::create_dir_all(&scratch).unwrap();
    let wt = scratch.join("wt");
    iso.git(
        &r.repo,
        &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
    );
    strip_group_world_write(&r.repo);
    fs::write(
        scratch.join(".oh-my-setting-tmp"),
        format!(
            "kind=oh-my-setting-temp\ntemporary=1\nrepo={}\nworktree={}\n",
            r.repo.display(),
            wt.display()
        ),
    )
    .unwrap();
    fs::write(wt.join("PROJECT.md"), GH_PROJECT).unwrap();
    strip_group_world_write(&iso.home.path().join(".cache"));
    r.respond(
        GH_PULL,
        &gh_item("pull", 7, "example-org/example-repo", GH_MERGED),
    );
    let (code, _, err) = r.run_in(&iso, &wt, &["refresh", "--pull", "7", "--ttl-s", "600"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        r.cache().is_some(),
        "the cache lives in the primary checkout"
    );
    r.respond(GH_COMMENTS, "[]");
    r.respond("user", r#"{"login":"oms-bot"}"#);
    // Only a receipt naming this origin's pull exactly is cited.
    let landed = "b".repeat(40);
    gh_land_receipt(
        &iso,
        &r.repo,
        "land.json",
        "https://github.com/example-org/example-repo/pull/7",
        &landed,
    );
    let (code, out, err) = r.run_in(&iso, &wt, &GH_POST);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "posted evidence comment 4242 on example-org/example-repo#3\n"
    );
    assert_eq!(r.phases(), ["prepared", "complete"], "{}", r.ledger());
    assert!(
        r.ledger().contains(&format!("- Land receipt: `{landed}`")),
        "{}",
        r.ledger()
    );
    assert!(
        !wt.join(".oms/github").exists(),
        "the scratch checkout keeps no GitHub state"
    );
    fs::write(r.repo.join("PROJECT.md"), GH_PROJECT).unwrap();
    let (code, out, err) = r.run(&iso, &GH_POST);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("already posted"), "{out}");
    assert_eq!(r.posts(), 1, "{}", r.calls());
}

#[test]
fn land_pr_merge_preserves_head_branch_when_dependent_pr_is_open_or_unverified() {
    for (dependents_payload, expected_reason, expected_notice) in [
        (
            "[{\"number\":345}]\n",
            "open-dependent-pr",
            "branch kept: open dependent PR targets head branch",
        ),
        (
            "not-json\n",
            "dependent-pr-check-unverified",
            "branch kept: dependent PR check unverified",
        ),
    ] {
        let iso = Isolated::new();
        let (repo, bare) = iso.with_github_remote("prstack");
        let head = iso.commit_as(
            &repo,
            "stack.txt",
            "base slice\n",
            &["feat: base slice", "Stacked base PR."],
        );
        let (gh, log) = iso.fake_gh(&bare, "success");
        fs::write(iso.home.path().join("gh.pr.dependents"), dependents_payload)
            .expect("dependents payload");
        let r = iso.run_with_services(
            "land",
            &repo,
            &[
                "--merge",
                "--gate",
                "true",
                "--ignore-siblings",
                "--no-update",
            ],
            &[("OMS_GH_BIN", gh.to_str().unwrap())],
        );
        assert_eq!(r.0, 0, "{r:?}");
        assert!(r.1.contains(expected_notice), "{r:?}");
        let calls = fs::read_to_string(&log).expect("gh log");
        assert!(
            calls.contains(
                "pr list --repo github.com/owner/prstack --state open --base oms/feat/base-slice --json number --limit 1"
            ),
            "{calls}"
        );
        assert!(
            calls.contains(&format!(
                "pr merge 7 --repo github.com/owner/prstack --squash --match-head-commit {head}"
            )),
            "{calls}"
        );
        assert!(!calls.contains("--delete-branch"), "{calls}");
        let (_, status, _) = iso.run("land", &repo, &["status", "--json"]);
        let row = V::parse(status.as_bytes()).expect("status json");
        assert_eq!(
            row.get("merge").get("delete_branch"),
            &V::Bool(false),
            "{status}"
        );
        assert_eq!(
            row.get("merge").get("delete_branch_skipped").text(),
            Some(expected_reason),
            "{status}"
        );
    }
}

#[test]
fn github_panel_reads_one_explicit_project_board_without_guessing() {
    let iso = Isolated::new();
    let r = GhRepo::new(&iso, "desktop-board", false);
    let url = "https://github.com/orgs/example-org/projects/3";
    let field = |id: &str, name: &str, opts: &str| {
        format!(
            r#"{{"__typename":"ProjectV2SingleSelectField","id":"{id}","name":"{name}","options":[{opts}]}}"#
        )
    };
    let fields = [
        field("F_status", "Status", r#"{"id":"o1","name":"Ready"}"#),
        field("F_prio", "Priority", r#"{"id":"o2","name":"P1"}"#),
        field("F_prio2", "Priority", r#"{"id":"o3","name":"P2"}"#),
    ]
    .join(",");
    let value = |field: &str, option: &str, name: &str| {
        format!(
            r#"{{"__typename":"ProjectV2ItemFieldSingleSelectValue","optionId":"{option}","name":"{name}","field":{{"id":"{field}"}}}}"#
        )
    };
    let issue = |n: u32, state: &str, repo: &str, labels: &str, extra: &str| {
        format!(
            r#"{{"type":"ISSUE","isArchived":false,"content":{{"__typename":"Issue","number":{n},"title":"Work {n}","state":"{state}","url":"https://github.com/{repo}/issues/{n}","repository":{{"nameWithOwner":"{repo}"}},"issueType":null,"blockedBy":{{"pageInfo":{{"hasNextPage":false}},"nodes":[]}},"labels":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{labels}]}}}},"fieldValues":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{extra}]}}}}"#
        )
    };
    let mine = "example-org/example-repo";
    let plan = r.repo.join(".oms/plan");
    fs::create_dir_all(&plan).unwrap();
    fs::write(
        plan.join("tasks.json"),
        r##"{"tasks":{"existing":{"issue":"#7","state":"in_progress"}}}"##,
    )
    .unwrap();
    let items = [
        issue(1, "OPEN", mine, "", &value("F_status", "o1", "Ready")),
        issue(1, "OPEN", mine, "", ""),
        issue(2, "OPEN", mine, r#"{"name":"Epic"}"#, ""),
        issue(3, "CLOSED", mine, "", ""),
        issue(4, "OPEN", "elsewhere/project", "", ""),
        r#"{"type":"DRAFT_ISSUE","isArchived":false,"content":null,"fieldValues":{"pageInfo":{"hasNextPage":false},"nodes":[]}}"#.to_string(),
        issue(5, "OPEN", mine, "", "").replace(r#""isArchived":false"#, r#""isArchived":true"#),
        issue(6, "OPEN", mine, "", &value("F_status", "zz", "Odd <b>name</b>")),
        issue(7, "OPEN", mine, "", ""),
        issue(8, "OPEN", mine, "", "").replace(r#""blockedBy":{"pageInfo":{"hasNextPage":false},"nodes":[]}"#,r#""blockedBy":{"pageInfo":{"hasNextPage":false},"nodes":[{"state":"OPEN"}]}"#),
        issue(9, "OPEN", mine, "", "").replace(r#""blockedBy":{"pageInfo":{"hasNextPage":false},"nodes":[]}"#,r#""blockedBy":null"#),
        issue(10, "OPEN", mine, "", "").replace(r#""issueType":null"#,r#""issueType":{"name":"Epic"}"#),
        issue(11, "OPEN", mine, "", "").replace(r#""labels":{"pageInfo":{"hasNextPage":false}"#,r#""labels":{"pageInfo":{"hasNextPage":true}"#),
    ]
    .join(",");
    let board_reply = |more: bool, id: &str, fields: &str, items: &str| {
        format!(
            r#"{{"data":{{"organization":{{"login":"example-org","projectV2":{{"id":"{id}","number":3,"title":"Board","url":"{url}","closed":false,"fields":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{fields}]}},"items":{{"pageInfo":{{"hasNextPage":{more},"endCursor":"cGFnZTE="}},"nodes":[{items}]}}}}}}}}}}"#
        )
    };
    let reply = |more, id: &str| board_reply(more, id, &fields, &items);
    fs::write(r.dir.join("panel-board"), reply(true, "PVT_abc")).unwrap();
    fs::write(r.dir.join("panel-projects"),r#"{"data":{"repository":{"nameWithOwner":"example-org/example-repo","projectsV2":{"pageInfo":{"hasNextPage":false},"nodes":[{"id":"PVT_abc","url":"https://github.com/orgs/example-org/projects/3","number":3,"title":"Board","closed":false,"owner":{"__typename":"Organization","login":"example-org"},"items":{"pageInfo":{"hasNextPage":false},"nodes":[]}}]}}}}"#).unwrap();
    let run = |args: &[&str]| {
        let mut a = vec!["panel-status", "--json"];
        a.extend(args);
        let (code, out, err) = r.run(&iso, &a);
        (code, V::parse(out.as_bytes()).ok(), err)
    };
    let (code, out, err) = run(&["--project-id", "PVT_abc"]);
    assert_eq!(code, 0, "{err}");
    let b = out.unwrap();
    let b = b.get("github").get("board");
    assert_eq!(b.get("status").text(), Some("available"));
    assert_eq!(b.get("incomplete"), &V::Bool(true));
    assert!(b.get("nextCursor").text().is_some());
    assert_eq!(b.get("board").get("id").text(), Some("PVT_abc"));
    let missing: Vec<_> = b
        .get("board")
        .get("fields")
        .get("missing")
        .array()
        .iter()
        .filter_map(|v| v.text())
        .collect();
    assert_eq!(
        missing,
        ["Priority", "Area", "Phase"],
        "duplicate Priority is ambiguous, not guessed"
    );
    let class: Vec<_> = b
        .get("items")
        .array()
        .iter()
        .map(|i| i.get("class").text().unwrap())
        .collect();
    assert_eq!(
        class,
        [
            "candidate",
            "duplicate",
            "epic",
            "closed",
            "foreign",
            "draft",
            "archived",
            "unverified",
            "plan_linked",
            "blocked",
            "unverified",
            "epic",
            "unknown"
        ]
    );
    let rows = b.get("items").array();
    assert_eq!(
        rows[0].get("url").text(),
        Some("https://github.com/example-org/example-repo/issues/1")
    );
    assert_eq!(
        rows[0].get("fields").get("Status").get("optionId").text(),
        Some("o1")
    );
    assert_eq!(rows[7].get("fields").get("Status"), &V::Null);
    assert!(
        rows[4].get("url").text().is_none(),
        "foreign item exposes no detail"
    );
    // A wrong node id, wrong cursor, or a different board never silently substitutes.
    let (_, out, _) = run(&["--project-id", "PVT_other"]);
    assert_eq!(
        out.unwrap().get("github").get("board").get("reason").text(),
        Some("board_not_in_catalog")
    );
    assert_ne!(
        run(&["--project-id", "PVT_abc", "--cursor", "bogus.cGFnZTE="]).0,
        0
    );
    assert_ne!(
        r.run(
            &iso,
            &[
                "panel-status",
                "--project",
                "https://github.com/orgs/example-org/repo"
            ]
        )
        .0,
        0
    );
    fs::write(r.dir.join("panel-board"), reply(false, "PVT_replaced")).unwrap();
    assert_eq!(
        run(&["--project-id", "PVT_abc"])
            .1
            .unwrap()
            .get("github")
            .get("board")
            .get("reason")
            .text(),
        Some("identity_mismatch")
    );
    // Cursor continuation completes the board.
    let cursor = b.get("nextCursor").text().unwrap().to_string();
    fs::write(r.dir.join("panel-board"), reply(false, "PVT_abc")).unwrap();
    let (code, out, err) = run(&["--project-id", "PVT_abc", "--cursor", &cursor]);
    assert_eq!(code, 0, "{err}");
    let done = out.unwrap();
    assert!(matches!(
        done.get("github").get("board").get("nextCursor"),
        V::Null
    ));
    assert!(r.calls().contains("-f after=cGFnZTE="));
    // Page-local advice uses the exact four single-select identities. Browsing
    // keeps its previous classifications even when stricter evidence is absent.
    let assert_advice = |row: &V, state: &str, reason: &str| {
        let sidecar = row.get("recommendation");
        assert_eq!(sidecar.get("state").text(), Some(state));
        assert_eq!(sidecar.get("reason").text(), Some(reason));
    };
    assert_advice(&rows[0], "excluded", "duplicate");
    assert_advice(&rows[1], "excluded", "duplicate");
    assert_advice(&rows[8], "excluded", "plan_linked");
    assert_advice(&rows[9], "excluded", "blocked");
    let fields = [
        field(
            "F_status",
            "Status",
            r#"{"id":"s1","name":"Ready"},{"id":"s2","name":"Backlog"}"#,
        ),
        field(
            "F_prio",
            "Priority",
            r#"{"id":"p0","name":"P0"},{"id":"p1","name":"P1"},{"id":"p2","name":"P2"},{"id":"p3","name":"P3"}"#,
        ),
        field("F_area", "Area", r#"{"id":"a1","name":"Runtime"}"#),
        field("F_phase", "Phase", r#"{"id":"f1","name":"Foundation"}"#),
    ]
    .join(",");
    let ready = [
        value("F_status", "s1", "Ready"),
        value("F_prio", "p1", "P1"),
        value("F_area", "a1", "Runtime"),
        value("F_phase", "f1", "Foundation"),
    ]
    .join(",");
    let cards = |field_values: &str| issue(20, "OPEN", mine, "", field_values);
    let query_board = |schema: &str, contents: &str| {
        fs::write(
            r.dir.join("panel-board"),
            board_reply(true, "PVT_abc", schema, contents),
        )
        .unwrap();
        let (code, reply, err) = run(&["--project-id", "PVT_abc"]);
        assert_eq!(code, 0, "{err}");
        reply.unwrap().get("github").get("board").clone()
    };
    let examples = [
        (cards(&ready), "recommended", "ready_with_priority"),
        (
            cards(&ready.replace("p1", "p0").replace("P1", "P0")),
            "recommended",
            "ready_with_priority",
        ),
        (
            cards(&ready.replace("p1", "p2").replace("P1", "P2")),
            "recommended",
            "ready_with_priority",
        ),
        (
            cards(&format!("{ready},{}", value("F_status", "s1", "Ready"))),
            "unverified",
            "item_evidence_incomplete",
        ),
        (
            cards(&ready.replace("s1", "s2").replace("Ready", "Backlog")),
            "excluded",
            "status_not_ready",
        ),
        (
            cards(&ready.replace("p1", "p3").replace("P1", "P3")),
            "unverified",
            "priority_unrecognized",
        ),
        (
            cards(&ready.replace("Runtime", "Other")),
            "unverified",
            "field_selection_unverified",
        ),
        (
            cards(&ready.replace("F_phase", "F_removed")),
            "unverified",
            "field_selection_unverified",
        ),
        (
            cards(&ready).replace(
                r#""blockedBy":{"pageInfo":{"hasNextPage":false}"#,
                r#""blockedBy":{"pageInfo":{"hasNextPage":true}"#,
            ),
            "unverified",
            "dependencies_unknown",
        ),
    ];
    for (item, state, reason) in examples {
        let page = query_board(&fields, &item);
        assert_advice(&page.get("items").array()[0], state, reason);
        assert_eq!(
            page.get("incomplete"),
            &V::Bool(true),
            "a recommendation does not exhaust later pages"
        );
    }
    let item = cards(&ready);
    for malformed in [
        fields.replace(r#""name":"P3""#, r#""name":"P1""#),
        fields.replace("F_area", "F_phase"),
        fields.replace(r#""name":"Phase""#, r#""name":"Renamed phase""#),
    ] {
        assert_advice(
            &query_board(&malformed, &item).get("items").array()[0],
            "unverified",
            "field_schema_unverified",
        );
    }
    let archived = item.replace(r#""isArchived":false"#, r#""isArchived":true"#);
    let page = query_board(&fields, &format!("{item},{archived}"));
    assert_advice(&page.get("items").array()[0], "excluded", "duplicate");
    let foreign = issue(20, "OPEN", "elsewhere/project", "", &ready);
    let page = query_board(&fields, &format!("{item},{foreign}"));
    assert_advice(
        &page.get("items").array()[0],
        "recommended",
        "ready_with_priority",
    );
    for errors in [r#"[{"type":"NOT_FOUND"}]"#, "null"] {
        let partial = board_reply(false, "PVT_abc", &fields, &item).replacen(
            '{',
            &format!(r#"{{"errors":{errors},"#),
            1,
        );
        fs::write(r.dir.join("panel-board"), partial).unwrap();
        let page = run(&["--project-id", "PVT_abc"]).1.unwrap();
        assert_advice(
            &page.get("github").get("board").get("items").array()[0],
            "unverified",
            "partial_response",
        );
    }
    let plan_file = plan.join("tasks.json");
    for raw in [
        "",
        r#"{"tasks":[]}"#,
        r#"{"tasks":{"broken":null}}"#,
        r##"{"tasks":{"duplicate":{"issue":"#20"},"duplicate":{}}}"##,
    ] {
        fs::write(&plan_file, raw).unwrap();
        let page = query_board(&fields, &item);
        assert_eq!(
            page.get("items").array()[0].get("class").text(),
            Some("candidate")
        );
        assert_advice(
            &page.get("items").array()[0],
            "unverified",
            "plan_unverified",
        );
    }
    fs::remove_file(&plan_file).unwrap();
    assert_advice(
        &query_board(&fields, &item).get("items").array()[0],
        "recommended",
        "ready_with_priority",
    );

    let packet_dir = r.repo.join(".oms/task");
    fs::create_dir_all(&packet_dir).unwrap();
    fs::write(
        packet_dir.join("current.md"),
        "# Task\n- github_issue: #19\n- github_issue: #20\n",
    )
    .unwrap();
    assert_advice(
        &query_board(&fields, &item).get("items").array()[0],
        "unverified",
        "plan_unverified",
    );

    // Missing scope reports the reason only for read:project; other scopes and FORBIDDEN
    // never leak drift-only or misleading panel reasons to the shared board panel.
    fs::write(r.dir.join("panel-board"), r#"{"data":null,"errors":[{"type":"INSUFFICIENT_SCOPES","message":"requires read:project"}]}"#).unwrap();
    fs::write(r.dir.join("panel-board-failed"), "").unwrap();
    let (_, out, _) = run(&["--project-id", "PVT_abc"]);
    let denied = out.unwrap();
    assert_eq!(
        denied.get("github").get("board").get("reason").text(),
        Some("insufficient_scope")
    );
    fs::write(
        r.dir.join("panel-board"),
        r#"{"data":null,"errors":[{"type":"INSUFFICIENT_SCOPES","message":"requires repo"}]}"#,
    )
    .unwrap();
    let (_, out, _) = run(&["--project-id", "PVT_abc"]);
    assert_eq!(
        out.unwrap().get("github").get("board").get("reason"),
        &V::Null
    );
    fs::write(
        r.dir.join("panel-board"),
        r#"{"data":null,"errors":[{"type":"FORBIDDEN","message":"forbidden"}]}"#,
    )
    .unwrap();
    let (_, out, _) = run(&["--project-id", "PVT_abc"]);
    assert_eq!(
        out.unwrap().get("github").get("board").get("reason"),
        &V::Null
    );
    assert_eq!(r.posts(), 0);
}

#[test]
fn github_panel_matches_current_and_verified_linked_worktrees() {
    let iso = Isolated::new();
    let r = GhRepo::new(&iso, "desktop-git", false);
    let linked = iso.home.path().join("worktree");
    iso.git(
        &r.repo,
        &[
            "worktree",
            "add",
            "-qb",
            "feature",
            linked.to_str().unwrap(),
        ],
    );
    let head = iso.git_out(&r.repo, &["rev-parse", "HEAD"]);
    let old = "a".repeat(40);
    let row = |n: i32, branch: &str, oid: &str, fork: bool| {
        format!(
            r#"{{"number":{n},"title":"Team PR","state":"OPEN","isDraft":false,"headRefName":"{branch}","baseRefName":"main","headRefOid":"{oid}","isCrossRepository":{fork},"statusCheckRollup":[{{"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"}},{{"__typename":"StatusContext","state":"PENDING"}},{{"__typename":"StatusContext","context":"lint","state":"FAILURE"}},{{"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED"}}],"url":"https://github.com/example-org/example-repo/pull/{n}"}}"#
        )
    };
    let prs = [
        row(1, "main", &old, false),
        row(2, "feature", &head, false),
        row(3, "feature", &head, true),
        row(4, "feature", &old, false),
        row(5, "unrelated", &head, false),
    ];
    fs::write(r.dir.join("panel-prs"), format!("[{}]", prs.join(","))).unwrap();
    fs::write(r.dir.join("panel-issues"), r#"[{"number":3,"title":"Open issue","state":"OPEN","url":"https://github.com/example-org/example-repo/issues/3","body":"private text"},{"number":4,"title":"Foreign issue","state":"OPEN","url":"https://github.com/elsewhere/project/issues/4"}]"#).unwrap();
    let board_reply = r#"{"data":{"repository":{"nameWithOwner":"EXAMPLE-org/example-repo","projectsV2":{"pageInfo":{"hasNextPage":false},"nodes":[{"number":4,"title":"Board","url":"https://github.com/orgs/example-org/projects/4","closed":false,"owner":{"__typename":"Organization","login":"example-org"},"items":{"pageInfo":{"hasNextPage":true},"nodes":[{"type":"PULL_REQUEST","content":{"__typename":"PullRequest","number":2,"state":"OPEN","title":"Linked work","url":"https://github.com/example-org/example-repo/pull/2","repository":{"nameWithOwner":"example-org/example-repo"}}},{"type":"ISSUE","content":{"__typename":"Issue","number":1,"state":"OPEN","title":"Foreign issue","url":"https://github.com/elsewhere/project/issues/1","repository":{"nameWithOwner":"elsewhere/project"}}},{"type":"REDACTED","content":null}]}}]}}}}"#;
    fs::write(r.dir.join("panel-projects"), board_reply).unwrap();
    let get = |path: &Path| {
        let (code, out, err) = r.run_in(&iso, path, &["panel-status", "--json"]);
        assert_eq!(code, 0, "{err}");
        assert!(
            !["Foreign issue", "private text", "secret-filename"]
                .iter()
                .any(|text| out.contains(text))
        );
        V::parse(out.as_bytes()).unwrap()
    };
    fs::write(r.repo.join("secret-filename"), "private").unwrap();
    let data = get(&r.repo);
    assert!(matches!(data.get("git").get("dirty"), V::Bool(true)));
    assert_eq!(data.get("git").get("head").text(), Some(head.as_str()));
    assert!(matches!(data.get("git").get("ahead"), V::Null));
    let github = data.get("github");
    assert_eq!(github.get("status").text(), Some("available"));
    let work = github.get("work");
    let issues = work.get("issues");
    assert_eq!(issues.get("status").text(), Some("available"));
    assert_eq!(issues.get("items").array().len(), 1);
    assert_eq!(issues.get("incomplete"), &V::Bool(true));
    let picker = work.get("pullRequests");
    assert_eq!(picker.get("status").text(), Some("available"));
    assert_eq!(
        picker.get("items").array().len(),
        5,
        "includes fork and other-branch PRs"
    );
    assert_eq!(picker.get("incomplete"), &V::Bool(false));
    let projects = work.get("projects");
    assert_eq!(projects.get("status").text(), Some("available"));
    let boards = projects.get("items").array();
    assert_eq!(boards.len(), 1);
    assert_eq!(
        boards[0].get("url").text(),
        Some("https://github.com/orgs/example-org/projects/4")
    );
    assert_eq!(boards[0].get("incomplete"), &V::Bool(true));
    let contents = boards[0].get("items").array();
    assert_eq!(contents.len(), 1);
    assert_eq!(
        contents[0].get("url").text(),
        Some("https://github.com/example-org/example-repo/pull/2")
    );
    let rows = github.get("pullRequests").array();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("scope").text(), Some("checkout"));
    assert!(matches!(rows[0].get("matchesLocalHead"), V::Bool(false)));
    assert_eq!(
        rows[1].get("url").text(),
        Some("https://github.com/example-org/example-repo/pull/2")
    );
    assert_eq!(rows[1].get("scope").text(), Some("linkedWorktree"));
    assert_eq!(rows[1].get("title").text(), Some("Team PR"));
    assert_eq!(
        rows[1].get("checks").get("items").array()[0]
            .get("name")
            .text(),
        Some("lint")
    );
    for key in ["passed", "failed", "pending", "other"] {
        assert!(matches!(rows[1].get("checks").get(key),V::Num(n) if n=="1"));
    }
    let issue_page = r#"{"data":{"repository":{"nameWithOwner":"example-org/example-repo","work":{"pageInfo":{"hasNextPage":true,"endCursor":"cGFnZTE="},"nodes":[{"number":3,"title":"Scoped issue","url":"https://github.com/example-org/example-repo/issues/3","state":"OPEN"}]}}}}"#;
    fs::write(r.dir.join("panel-inventory"), issue_page).unwrap();
    let inventory = |collection: &str, after: Option<&str>| {
        let mut args = vec!["panel-status", "--inventory", collection];
        if let Some(s) = after {
            args.extend(["--cursor", s]);
        }
        r.run(&iso, &args)
    };
    let (code, page, err) = inventory("issues", None);
    assert_eq!(code, 0, "{err}");
    let first = V::parse(page.as_bytes())
        .unwrap()
        .get("github")
        .get("inventory")
        .clone();
    assert_eq!(first.get("items").array().len(), 1);
    let cursor = first.get("nextCursor").text().unwrap();
    assert_ne!(inventory("pullRequests", Some(cursor)).0, 0);
    assert_ne!(inventory("issues", Some("--help")).0, 0);
    let repeated = V::parse(inventory("issues", Some(cursor)).1.as_bytes()).unwrap();
    let repeated = repeated.get("github").get("inventory");
    assert_eq!(repeated.get("nextCursor"), &V::Null);
    assert_eq!(repeated.get("incomplete"), &V::Bool(true));
    let last = issue_page
        .replace("true", "false")
        .replace("/issues/3", "/issues/8")
        .replace(r#""number":3"#, r#""number":8"#);
    fs::write(r.dir.join("panel-inventory"), &last).unwrap();
    let (code, page, err) = inventory("issues", Some(cursor));
    assert_eq!(code, 0, "{err}");
    let page = V::parse(page.as_bytes()).unwrap();
    let page = page.get("github").get("inventory");
    assert_eq!(
        page.get("items").array()[0].get("number"),
        &V::Num("8".into())
    );
    assert_eq!(page.get("incomplete"), &V::Bool(false));
    assert_eq!(page.get("nextCursor"), &V::Null);
    fs::write(
        r.dir.join("panel-inventory"),
        last.replace("/issues/8", "/pull/8"),
    )
    .unwrap();
    let page = V::parse(inventory("pullRequests", None).1.as_bytes()).unwrap();
    assert_eq!(
        page.get("github").get("inventory").get("items").array()[0]
            .get("kind")
            .text(),
        Some("pull")
    );
    let lookup = |kind: &str, no: &str| {
        let (code, out, err) = r.run(&iso, &["panel-status", "--json", kind, no]);
        assert_eq!(code, 0, "{err}");
        assert!(!out.contains("extra body"));
        V::parse(out.as_bytes())
            .unwrap()
            .get("github")
            .get("lookup")
            .clone()
    };
    let issue = r#"{"number":447,"title":"Older task","url":"https://github.com/example-org/example-repo/issues/447","state":"OPEN","body":"extra body"}"#;
    fs::write(r.dir.join("panel-lookup"), issue).unwrap();
    assert_eq!(
        lookup("--issue", "447").get("item").get("title").text(),
        Some("Older task")
    );
    assert_eq!(
        lookup("--issue", "446").get("status").text(),
        Some("unavailable")
    );
    assert_eq!(
        lookup("--pull", "447").get("status").text(),
        Some("unavailable")
    );
    for (payload, status) in [
        (issue.replace("OPEN", "CLOSED"), "closed"),
        (issue.replace("example-org", "foreign"), "unavailable"),
    ] {
        fs::write(r.dir.join("panel-lookup"), payload).unwrap();
        let data = lookup("--issue", "447");
        assert_eq!(data.get("status").text(), Some(status));
        assert_eq!(data.get("item"), &V::Null);
    }
    let pr = issue.replace("/issues/", "/pull/");
    fs::write(r.dir.join("panel-lookup"), pr).unwrap();
    assert_eq!(
        lookup("--pull", "447").get("status").text(),
        Some("available")
    );
    for args in [
        vec!["panel-status", "--issue", "0"],
        vec!["panel-status", "--issue", "2", "--pull", "3"],
    ] {
        assert_ne!(r.run(&iso, &args).0, 0);
    }
    let data = get(&linked);
    assert_eq!(data.get("git").get("branch").text(), Some("feature"));
    assert!(matches!(data.get("git").get("dirty"), V::Bool(false)));
    assert_eq!(
        data.get("github").get("pullRequests").array()[0]
            .get("scope")
            .text(),
        Some("checkout")
    );
    fs::write(
        r.dir.join("panel-projects"),
        board_reply.replace("EXAMPLE-org/example-repo", "other/project"),
    )
    .unwrap();
    assert_eq!(
        get(&r.repo)
            .get("github")
            .get("work")
            .get("projects")
            .get("status")
            .text(),
        Some("unavailable")
    );
    fs::write(r.dir.join("panel-projects"), r#"{"data":null,"errors":[{"type":"INSUFFICIENT_SCOPES","message":"requires read:project"}]}"#).unwrap();
    fs::write(r.dir.join("panel-projects-failed"), "").unwrap();
    let denied = get(&r.repo);
    let denied = denied.get("github").get("work").get("projects");
    assert_eq!(denied.get("status").text(), Some("unavailable"));
    assert_eq!(denied.get("reason").text(), Some("insufficient_scope"));
    fs::remove_file(r.dir.join("panel-projects")).unwrap();
    let data = get(&r.repo);
    let work = data.get("github").get("work");
    assert_eq!(
        work.get("projects").get("status").text(),
        Some("unavailable")
    );
    assert_eq!(work.get("issues").get("status").text(), Some("available"));
    assert_eq!(
        work.get("pullRequests").get("status").text(),
        Some("available")
    );
    // Several large Unicode titles and check names share one MCP response budget.
    let long_label = "🧪".repeat(160);
    let check =
        format!(r#"{{"__typename":"StatusContext","state":"PENDING","context":"{long_label}"}}"#);
    let wide = (1..=9).map(|id| format!(r#"{{"number":{id},"title":"{long_label}","state":"OPEN","isDraft":false,"headRefName":"main","baseRefName":"main","headRefOid":"{head}","isCrossRepository":false,"statusCheckRollup":[{}],"url":"https://github.com/example-org/example-repo/pull/{id}"}}"#, [check.as_str();6].join(","))).collect::<Vec<_>>();
    fs::write(r.dir.join("panel-prs"), format!("[{}]", wide.join(","))).unwrap();
    let bounded = get(&r.repo);
    let github = bounded.get("github");
    assert!(bounded.json(false).unwrap().len() < 52_000);
    assert_eq!(github.get("incomplete"), &V::Bool(true));
    assert!(
        !github.get("pullRequests").array().is_empty()
            && github.get("pullRequests").array().len() < 8
    );
    fs::write(r.dir.join("panel-prs"), "[]").unwrap();
    assert!(
        get(&r.repo)
            .get("github")
            .get("pullRequests")
            .array()
            .is_empty()
    );
    fs::remove_file(r.dir.join("panel-prs")).unwrap();
    assert_eq!(
        get(&r.repo).get("github").get("status").text(),
        Some("unavailable")
    );
    let calls = r.calls();
    for call in calls.lines() {
        assert!(
            call.starts_with(
                "pr list --repo github.com/example-org/example-repo --state open --limit 51 --json "
            ) || ["issue view 447", "issue view 446", "pr view 447"].iter().any(|s|
                call == format!("{s} --repo github.com/example-org/example-repo --json number,title,url,state"))
              || call
                == "issue list --repo github.com/example-org/example-repo --state open --limit 13 --json number,title,url,state"
                || (call.starts_with("api graphql --hostname github.com -f query=query(")
                    && ((call.contains("projectsV2(first:6)") && call.contains("items(first:8)")
                         && call.ends_with("-f owner=example-org -f name=example-repo"))
                       || ((call.contains("work:issues(first:12") || call.contains("work:pullRequests(first:12"))
                         && call.contains("orderBy:{field:CREATED_AT,direction:ASC}")
                         && (call.ends_with("-f owner=example-org -f name=example-repo") || call.ends_with("-f name=example-repo -f after=cGFnZTE="))))),
            "{call}"
        );
    }
    let nested = r.repo.join("nested-project");
    fs::create_dir_all(nested.join(".oms")).unwrap();
    let (code, out, err) = r.run(
        &iso,
        &["panel-status", "--repo", nested.to_str().unwrap(), "--json"],
    );
    assert_eq!(code, 0, "{err}");
    let nested = V::parse(out.as_bytes()).unwrap();
    assert!(matches!(nested.get("git").get("available"), V::Bool(false)));
    assert!(matches!(nested.get("github").get("repository"), V::Null));
    iso.git(&r.repo, &["remote", "remove", "origin"]);
    assert_eq!(
        get(&r.repo).get("github").get("status").text(),
        Some("unsupported")
    );
    assert_eq!(r.calls(), calls);
}

#[test]
fn github_board_drift_reports_status_mismatches_missing_exclusions_and_completeness() {
    let iso = Isolated::new();
    let r = GhRepo::new(&iso, "board-drift", false);
    let url = "https://github.com/orgs/example-org/projects/3";
    let mine = "example-org/example-repo";

    // Help documents the new --drift flag, and invalid combinations fail with exit 2.
    let (code, help, err) = inproc("github", &["--help"]);
    assert_eq!(code, 0, "{err}");
    assert!(help.contains("--drift"), "{help}");
    assert_refused(
        &r.run(&iso, &["panel-status", "--drift"]),
        "--drift requires --project-id NODE_ID",
    );
    assert_refused(
        &r.run(
            &iso,
            &[
                "panel-status",
                "--project-id",
                "PVT_abc",
                "--drift",
                "--cursor",
                "abc.123",
            ],
        ),
        "--drift cannot combine with --cursor",
    );

    let status_field = |opts: &str| {
        format!(
            r#"{{"__typename":"ProjectV2SingleSelectField","id":"F_status","name":"Status","options":[{opts}]}}"#
        )
    };
    let value = |option: &str, name: &str| {
        format!(
            r#"{{"__typename":"ProjectV2ItemFieldSingleSelectValue","optionId":"{option}","name":"{name}","field":{{"id":"F_status"}}}}"#
        )
    };
    let issue_card = |n: u32, state: &str, repo: &str, archived: bool, extra: &str| {
        format!(
            r#"{{"type":"ISSUE","isArchived":{archived},"content":{{"__typename":"Issue","number":{n},"title":"Issue {n}","state":"{state}","url":"https://github.com/{repo}/issues/{n}","repository":{{"nameWithOwner":"{repo}"}},"issueType":null,"blockedBy":{{"pageInfo":{{"hasNextPage":false}},"nodes":[]}},"labels":{{"pageInfo":{{"hasNextPage":false}},"nodes":[]}}}},"fieldValues":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{extra}]}}}}"#
        )
    };
    let pr_card = |n: u32, state: &str, repo: &str, archived: bool, extra: &str| {
        format!(
            r#"{{"type":"PULL_REQUEST","isArchived":{archived},"content":{{"__typename":"PullRequest","number":{n},"title":"PR {n}","state":"{state}","url":"https://github.com/{repo}/pull/{n}","repository":{{"nameWithOwner":"{repo}"}}}},"fieldValues":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{extra}]}}}}"#
        )
    };
    let board_reply = |fields_json: &str, more: bool, cursor: &str, items_json: &str| {
        format!(
            r#"{{"data":{{"organization":{{"login":"example-org","projectV2":{{"id":"PVT_abc","number":3,"title":"Roadmap","url":"{url}","closed":false,"fields":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{fields_json}]}},"items":{{"pageInfo":{{"hasNextPage":{more},"endCursor":"{cursor}"}},"nodes":[{items_json}]}}}}}}}}}}"#
        )
    };
    let work_page = |nodes: &str, more: bool, cursor: &str| {
        format!(
            r#"{{"data":{{"repository":{{"nameWithOwner":"{mine}","work":{{"pageInfo":{{"hasNextPage":{more},"endCursor":"{cursor}"}},"nodes":[{nodes}]}}}}}}}}"#
        )
    };
    let open_node = |n: u32, seg: &str, title: &str| {
        format!(
            r#"{{"number":{n},"title":"{title}","state":"OPEN","url":"https://github.com/{mine}/{seg}/{n}"}}"#
        )
    };

    fs::write(
        r.dir.join("panel-projects"),
        r#"{"data":{"repository":{"nameWithOwner":"example-org/example-repo","projectsV2":{"pageInfo":{"hasNextPage":false},"nodes":[{"id":"PVT_abc","url":"https://github.com/orgs/example-org/projects/3","number":3,"title":"Roadmap","closed":false,"owner":{"__typename":"Organization","login":"example-org"},"items":{"pageInfo":{"hasNextPage":false},"nodes":[]}}]}}}}"#,
    )
    .unwrap();

    // Multi-page board: page 1 -> page 2 (complete pagination).
    let std_fields = status_field(
        r#"{"id":"o_back","name":"Backlog"},{"id":"o_prog","name":"In Progress"},{"id":"o_done","name":"Done"}"#,
    );
    let page1_items = [
        // Closed issue still in Backlog -> closed_not_done
        issue_card(392, "CLOSED", mine, false, &value("o_back", "Backlog")),
        // Merged PR still in In Progress -> merged_not_done
        pr_card(448, "MERGED", mine, false, &value("o_prog", "In Progress")),
        // Open issue marked Done -> open_in_done
        issue_card(401, "OPEN", mine, false, &value("o_done", "Done")),
        // Healthy open issue in Backlog -> no drift, present on board
        issue_card(300, "OPEN", mine, false, &value("o_back", "Backlog")),
        // Healthy closed issue in Done -> no drift
        issue_card(299, "CLOSED", mine, false, &value("o_done", "Done")),
        // Foreign repository item -> excluded (not drift, no details leaked)
        issue_card(900, "CLOSED", "other-org/secret-repo", false, &value("o_back", "Backlog")),
        // Archived foreign repository item -> excluded as foreignRepository, not archived
        issue_card(901, "OPEN", "other-org/secret-repo", true, &value("o_back", "Backlog")),
        // Archived local item -> excluded (not drift, even though open)
        issue_card(350, "OPEN", mine, true, &value("o_back", "Backlog")),
        // Draft item -> excluded
        r#"{"type":"DRAFT_ISSUE","isArchived":false,"content":null,"fieldValues":{"pageInfo":{"hasNextPage":false},"nodes":[]}}"#.to_string(),
    ]
    .join(",");
    let page2_items = [
        // Closed PR with unset Status -> closed_not_done (status: null)
        pr_card(410, "CLOSED", mine, false, ""),
        // Open PR on board -> not missing
        pr_card(420, "OPEN", mine, false, &value("o_prog", "In Progress")),
    ]
    .join(",");
    fs::write(
        r.dir.join("panel-board"),
        board_reply(&std_fields, true, "page1cur", &page1_items),
    )
    .unwrap();
    fs::write(
        r.dir.join("panel-board-after-page1cur"),
        board_reply(&std_fields, false, "page2cur", &page2_items),
    )
    .unwrap();

    // Open issues: #300 and #401 (on board), #350 (archived on board), plus 21 missing open issues (#450..#470) to test bounding + omitted count!
    let mut open_issue_nodes = vec![
        open_node(300, "issues", "On board open"),
        open_node(401, "issues", "Issue 401"),
        open_node(350, "issues", "Archived open"),
    ];
    for n in 450..=470 {
        open_issue_nodes.push(open_node(n, "issues", &format!("Missing issue {n}")));
    }
    fs::write(
        r.dir.join("panel-inventory-issues"),
        work_page(&open_issue_nodes.join(","), false, ""),
    )
    .unwrap();
    let open_pr_nodes = [
        open_node(420, "pull", "On board PR"),
        open_node(461, "pull", "Missing PR 461"),
    ]
    .join(",");
    fs::write(
        r.dir.join("panel-inventory-pulls"),
        work_page(&open_pr_nodes, false, ""),
    )
    .unwrap();

    // 1. Complete JSON readout.
    let (code, out, err) = r.run(
        &iso,
        &[
            "panel-status",
            "--project-id",
            "PVT_abc",
            "--drift",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        !out.contains("other-org/secret-repo")
            && !out.contains("Issue 900")
            && !out.contains("Issue 901"),
        "foreign repo details must not leak: {out}"
    );
    let doc = V::parse(out.as_bytes()).expect("drift json");
    let observed = doc.get("github").get("observedAt").text().unwrap();
    assert!(
        observed.ends_with('Z') && observed.contains('T'),
        "{observed}"
    );
    let d = doc.get("github").get("drift");
    assert_eq!(d.get("status").text(), Some("available"));
    assert_eq!(d.get("observedAt").text(), Some(observed));
    assert_eq!(d.get("partial"), &V::Bool(false));
    assert_eq!(d.get("incomplete"), &V::Bool(false));
    assert_eq!(d.get("completeness").get("complete"), &V::Bool(true));
    assert_eq!(
        d.get("completeness").get("board").get("pages"),
        &V::Num("2".into())
    );
    let opts: Vec<_> = d
        .get("statusField")
        .get("options")
        .array()
        .iter()
        .filter_map(|v| v.text())
        .collect();
    assert_eq!(opts, ["Backlog", "In Progress", "Done"]);
    assert_eq!(
        d.get("statusField").get("doneMatchRule").text(),
        Some("exact_configured_names")
    );
    assert_eq!(d.get("statusField").get("hasDoneOption"), &V::Bool(true));
    let matched: Vec<_> = d
        .get("statusField")
        .get("doneMatchedOptions")
        .array()
        .iter()
        .filter_map(|v| v.text())
        .collect();
    assert_eq!(matched, ["Done"]);

    // Status drift assertions: closed issue #392, merged PR #448, open issue #401, closed PR #410.
    let sd = d.get("statusDrift");
    assert_eq!(sd.get("total"), &V::Num("4".into()));
    assert_eq!(sd.get("closedNotDone"), &V::Num("2".into()));
    assert_eq!(sd.get("mergedNotDone"), &V::Num("1".into()));
    assert_eq!(sd.get("openInDone"), &V::Num("1".into()));
    let sd_summary: Vec<_> = sd
        .get("items")
        .array()
        .iter()
        .map(|i| {
            (
                i.get("kind").text().unwrap(),
                i.get("number").py(),
                i.get("state").text().unwrap(),
                i.get("status").text(),
                i.get("drift").text().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        sd_summary,
        [
            (
                "pull",
                "448".into(),
                "MERGED",
                Some("In Progress"),
                "merged_not_done"
            ),
            ("pull", "410".into(), "CLOSED", None, "closed_not_done"),
            ("issue", "401".into(), "OPEN", Some("Done"), "open_in_done"),
            (
                "issue",
                "392".into(),
                "CLOSED",
                Some("Backlog"),
                "closed_not_done"
            ),
        ]
    );

    // Missing from board: 21 open issues + 1 open PR = 22 total, bounded to 20 shown + 2 omitted.
    let missing = d.get("missingFromBoard");
    assert_eq!(missing.get("unverified"), &V::Bool(false));
    assert_eq!(missing.get("total"), &V::Num("22".into()));
    assert_eq!(missing.get("issuesTotal"), &V::Num("21".into()));
    assert_eq!(missing.get("pullRequestsTotal"), &V::Num("1".into()));
    assert_eq!(missing.get("items").array().len(), 20);
    assert_eq!(missing.get("omitted"), &V::Num("2".into()));
    assert!(
        !missing
            .get("items")
            .array()
            .iter()
            .any(|i| i.get("number").py() == "350"),
        "archived item is excluded, not missing-from-board drift"
    );
    let unverified_missing = d.get("unverifiedMissing");
    assert_eq!(unverified_missing.get("total"), &V::Num("0".into()));
    assert!(unverified_missing.get("items").array().is_empty());

    // Excluded (not drift): 2 foreign (including 1 archived foreign), 1 archived local, 1 draft.
    let ex = d.get("excluded");
    assert_eq!(ex.get("foreignRepository"), &V::Num("2".into()));
    assert_eq!(ex.get("archived"), &V::Num("1".into()));
    assert_eq!(ex.get("draft"), &V::Num("1".into()));
    assert_eq!(ex.get("total"), &V::Num("4".into()));

    // 2. Short human text readout.
    let (code, text, err) = r.run(
        &iso,
        &["panel-status", "--project-id", "PVT_abc", "--drift"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        text.contains("Board drift: Roadmap (#3, PVT_abc)"),
        "{text}"
    );
    assert!(text.contains("Observed at: "), "{text}");
    assert!(
        text.contains(
            "Completeness: complete\n  board: complete (11 items / 2 pages)\n  open issues: complete (24 items / 1 pages)\n  open PRs: complete (2 items / 1 pages)\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("Status options: Backlog, In Progress, Done"),
        "{text}"
    );
    assert!(
        text.contains("Done-like options: Done\n  rule: exact configured names only ("),
        "{text}"
    );
    assert!(
        text.contains("pull #448 [MERGED] status=\"In Progress\" (merged_not_done)"),
        "{text}"
    );
    assert!(
        text.contains("issue #392 [CLOSED] status=\"Backlog\" (closed_not_done)"),
        "{text}"
    );
    assert!(
        text.contains("issue #401 [OPEN] status=\"Done\" (open_in_done)"),
        "{text}"
    );
    assert!(
        text.contains(
            "Missing from board (open in repo): 22 (21 issues, 1 PRs; showing 20, 2 more omitted)"
        ),
        "{text}"
    );
    assert!(text.contains("2 more not shown"), "{text}");
    assert!(
        text.contains("Excluded (not drift): 4 (foreign repository: 2, archived: 1, draft: 1)"),
        "{text}"
    );
    for line in text.lines() {
        assert!(
            line.chars().count() <= 120,
            "drift line exceeds 120 columns ({} chars): {line}",
            line.chars().count()
        );
    }

    // 3. Board with NO configured done-like option (exact names only, no fuzzy guess for "Shipped" / "done").
    let no_done_fields = status_field(
        r#"{"id":"o_back","name":"Backlog"},{"id":"o_ship","name":"Shipped"},{"id":"o_lower","name":"done"}"#,
    );
    let no_done_items = [
        issue_card(392, "CLOSED", mine, false, &value("o_ship", "Shipped")),
        issue_card(393, "OPEN", mine, false, &value("o_lower", "done")),
    ]
    .join(",");
    fs::write(
        r.dir.join("panel-board"),
        board_reply(&no_done_fields, false, "", &no_done_items),
    )
    .unwrap();
    let (code, text, err) = r.run(
        &iso,
        &["panel-status", "--project-id", "PVT_abc", "--drift"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        text.contains("Status options: Backlog, Shipped, done"),
        "{text}"
    );
    assert!(
        text.contains("Done-like options: none on board\n  rule: exact configured names only ("),
        "{text}"
    );
    assert!(
        text.contains("issue #392 [CLOSED] status=\"Shipped\" (closed_not_done)"),
        "{text}"
    );
    assert!(
        !text.contains("open_in_done"),
        "lowercase 'done' must not fuzzy-match configured 'Done': {text}"
    );

    let read_drift = || {
        let (code, out, err) = r.run(
            &iso,
            &[
                "panel-status",
                "--project-id",
                "PVT_abc",
                "--drift",
                "--json",
            ],
        );
        assert_eq!(code, 0, "{err}");
        V::parse(out.as_bytes())
            .unwrap()
            .get("github")
            .get("drift")
            .clone()
    };
    let archived = issue_card(470, "OPEN", mine, true, "");
    let complete_board = board_reply(&std_fields, false, "", &archived);
    for incomplete_board in [
        complete_board.replacen('{', r#"{"errors":null,"#, 1),
        complete_board.replace(r#""__typename":"Issue""#, r#""__typename":null"#),
        complete_board.replace(
            &format!(r#""nameWithOwner":"{mine}""#),
            r#""nameWithOwner":null"#,
        ),
        complete_board.replace(r#""number":470"#, r#""number":null"#),
    ] {
        fs::write(r.dir.join("panel-board"), incomplete_board).unwrap();
        let drift = read_drift();
        assert_eq!(
            drift.get("completeness").get("board").get("complete"),
            &V::Bool(false)
        );
        assert_eq!(
            drift.get("missingFromBoard").get("unverified"),
            &V::Bool(true)
        );
        assert!(
            drift
                .get("missingFromBoard")
                .get("items")
                .array()
                .is_empty()
        );
        assert!(
            !drift
                .get("unverifiedMissing")
                .get("items")
                .array()
                .is_empty()
        );
    }
    // A partial inventory limits the total. An item actually observed there can
    // still be compared with a complete board, including local archived members.
    let issues_page = work_page(&open_issue_nodes.join(","), false, "");
    fs::write(r.dir.join("panel-board"), &complete_board).unwrap();
    fs::write(
        r.dir.join("panel-inventory-issues"),
        issues_page.replacen('{', r#"{"errors":{},"#, 1),
    )
    .unwrap();
    let drift = read_drift();
    assert_eq!(
        drift.get("completeness").get("board").get("complete"),
        &V::Bool(true)
    );
    assert_eq!(
        drift.get("completeness").get("openIssues").get("complete"),
        &V::Bool(false)
    );
    assert_eq!(drift.get("partial"), &V::Bool(true));
    assert_eq!(
        drift.get("missingFromBoard").get("unverified"),
        &V::Bool(false)
    );
    assert!(
        drift
            .get("missingFromBoard")
            .get("items")
            .array()
            .iter()
            .all(|v| v.get("number") != &V::Num("470".into()))
    );
    fs::write(r.dir.join("panel-inventory-issues"), issues_page).unwrap();

    // 4. Partial page: hasNextPage=true with missing cursor -> reported as partial.
    fs::write(
        r.dir.join("panel-board"),
        board_reply(&std_fields, true, "", &page1_items),
    )
    .unwrap();
    let (code, out, err) = r.run(
        &iso,
        &[
            "panel-status",
            "--project-id",
            "PVT_abc",
            "--drift",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let partial_doc = V::parse(out.as_bytes()).unwrap();
    let pd = partial_doc.get("github").get("drift");
    assert_eq!(pd.get("partial"), &V::Bool(true));
    assert_eq!(pd.get("reason").text(), Some("partial_response"));
    assert_eq!(
        pd.get("completeness").get("board").get("complete"),
        &V::Bool(false)
    );
    assert_eq!(pd.get("missingFromBoard").get("unverified"), &V::Bool(true));
    assert_eq!(pd.get("missingFromBoard").get("total"), &V::Num("0".into()));
    assert!(pd.get("missingFromBoard").get("items").array().is_empty());
    assert_eq!(
        pd.get("unverifiedMissing").get("total"),
        &V::Num("23".into())
    );

    // 5. Rate limit on page 2 -> reported as partial with rate_limited reason.
    fs::write(
        r.dir.join("panel-board"),
        board_reply(&std_fields, true, "page1cur", &page1_items),
    )
    .unwrap();
    fs::write(
        r.dir.join("panel-board-after-page1cur"),
        r#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#,
    )
    .unwrap();
    fs::write(r.dir.join("panel-board-after-page1cur-failed"), "").unwrap();
    let (code, out, err) = r.run(
        &iso,
        &[
            "panel-status",
            "--project-id",
            "PVT_abc",
            "--drift",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let rl_doc = V::parse(out.as_bytes()).unwrap();
    let rld = rl_doc.get("github").get("drift");
    assert_eq!(rld.get("partial"), &V::Bool(true));
    assert_eq!(rld.get("reason").text(), Some("rate_limited"));
    assert_eq!(
        rld.get("missingFromBoard").get("unverified"),
        &V::Bool(true)
    );
    let (_, rl_text, _) = r.run(
        &iso,
        &["panel-status", "--project-id", "PVT_abc", "--drift"],
    );
    assert!(
        rl_text.contains(
            "Completeness: partial - rate limit (rate_limited)\n  board: partial (rate_limited;"
        ),
        "{rl_text}"
    );
    assert!(
        rl_text.contains("Missing from board (open in repo): unverified (board read partial)"),
        "{rl_text}"
    );
    for line in rl_text.lines() {
        assert!(
            line.chars().count() <= 120,
            "partial drift line exceeds 120 columns ({} chars): {line}",
            line.chars().count()
        );
    }
    fs::remove_file(r.dir.join("panel-board-after-page1cur-failed")).unwrap();

    // 6. Timeout on page 2 -> reported as partial with timeout reason; PR #420 from page 2
    // must NOT be reported missing, and unseen items go under unverifiedMissing.
    fs::write(r.dir.join("panel-board-after-page1cur-timeout"), "").unwrap();
    let (code, out, err) = r.run(
        &iso,
        &[
            "panel-status",
            "--project-id",
            "PVT_abc",
            "--drift",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let to_doc = V::parse(out.as_bytes()).unwrap();
    let tod = to_doc.get("github").get("drift");
    assert_eq!(tod.get("partial"), &V::Bool(true));
    assert_eq!(tod.get("reason").text(), Some("timeout"));
    let to_missing = tod.get("missingFromBoard");
    assert_eq!(to_missing.get("unverified"), &V::Bool(true));
    assert_eq!(to_missing.get("total"), &V::Num("0".into()));
    assert_eq!(to_missing.get("issuesTotal"), &V::Num("0".into()));
    assert_eq!(to_missing.get("pullRequestsTotal"), &V::Num("0".into()));
    assert!(
        to_missing.get("items").array().is_empty(),
        "PR #420 must not be reported missing when board walk timed out: {out}"
    );
    let to_unverified = tod.get("unverifiedMissing");
    assert_eq!(to_unverified.get("total"), &V::Num("23".into()));
    assert_eq!(to_unverified.get("issuesTotal"), &V::Num("21".into()));
    assert_eq!(to_unverified.get("pullRequestsTotal"), &V::Num("2".into()));
    assert_eq!(to_unverified.get("items").array().len(), 20);
    assert_eq!(to_unverified.get("omitted"), &V::Num("3".into()));
    let (code, to_text, err) = r.run(
        &iso,
        &["panel-status", "--project-id", "PVT_abc", "--drift"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        to_text.contains("Missing from board (open in repo): unverified (board read partial)"),
        "{to_text}"
    );
    assert!(
        to_text.contains(
            "Not seen on read pages (board read partial): 23 (21 issues, 2 PRs; showing 20, 3 more omitted)"
        ),
        "{to_text}"
    );
    assert!(
        !to_text.contains("Missing from board (open in repo): 23"),
        "{to_text}"
    );
    for line in to_text.lines() {
        assert!(
            line.chars().count() <= 120,
            "timeout drift line exceeds 120 columns ({} chars): {line}",
            line.chars().count()
        );
    }
    fs::remove_file(r.dir.join("panel-board-after-page1cur-timeout")).unwrap();

    // 7. Missing permission (INSUFFICIENT_SCOPES and drift-only FORBIDDEN) reported with reason in JSON and text.
    fs::write(
        r.dir.join("panel-board"),
        r#"{"data":null,"errors":[{"type":"INSUFFICIENT_SCOPES","message":"requires read:project"}]}"#,
    )
    .unwrap();
    fs::write(r.dir.join("panel-board-failed"), "").unwrap();
    let (code, out, err) = r.run(
        &iso,
        &[
            "panel-status",
            "--project-id",
            "PVT_abc",
            "--drift",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let denied = V::parse(out.as_bytes()).unwrap();
    let dd = denied.get("github").get("drift");
    assert_eq!(dd.get("status").text(), Some("unavailable"));
    assert_eq!(dd.get("partial"), &V::Bool(true));
    assert_eq!(dd.get("reason").text(), Some("insufficient_scope"));
    assert_eq!(dd.get("missingFromBoard").get("unverified"), &V::Bool(true));
    let (_, denied_text, _) = r.run(
        &iso,
        &["panel-status", "--project-id", "PVT_abc", "--drift"],
    );
    assert!(
        denied_text.contains("Completeness: partial - permission missing (insufficient_scope)"),
        "{denied_text}"
    );
    fs::write(
        r.dir.join("panel-board"),
        r#"{"data":null,"errors":[{"type":"FORBIDDEN","message":"resource forbidden"}]}"#,
    )
    .unwrap();
    let (code, out, err) = r.run(
        &iso,
        &[
            "panel-status",
            "--project-id",
            "PVT_abc",
            "--drift",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let forbidden = V::parse(out.as_bytes()).unwrap();
    assert_eq!(
        forbidden.get("github").get("drift").get("reason").text(),
        Some("forbidden")
    );
    fs::remove_file(r.dir.join("panel-board-failed")).unwrap();

    // 8. Unlinked board is never guessed.
    let (code, out, err) = r.run(
        &iso,
        &[
            "panel-status",
            "--project-id",
            "PVT_other",
            "--drift",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let unlinked = V::parse(out.as_bytes()).unwrap();
    assert_eq!(
        unlinked.get("github").get("drift").get("reason").text(),
        Some("board_not_in_catalog")
    );

    // Strictly read-only: zero POST requests across all cases.
    assert_eq!(r.posts(), 0);
}
