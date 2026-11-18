//! Native collaboration, runs, hooks, lifecycle, providers-skills, journal, and MCP service adapters.
//!
//! Authority rules: nothing here writes state itself; every mutation belongs to
//! the owning domain crate. Native `oms` subcommands that cannot run in process
//! are launched only through the staged `--oms-dispatch <source> <rev> --`
//! launcher (validated like `ControlServices`); when it is unavailable the
//! adapter refuses before any child, wake record or log is created. Child
//! processes run through `oms_supervisor` with a deadline, bounded output and
//! cancellation, and argv is never shell-parsed.
//!
//! Known gaps, reported by the methods themselves rather than hidden:
//! `swap_main_host` keeps its fail-closed default; `tick_maintenance` reports the Codex plugin cache as
//! `skipped` (no native producer refreshes it); `refresh_catalogs` ages only
//! catalogs already on disk (a never-probed provider has none to age);
//! `install --star` has no native producer and is refused; and
//! `incoming_lifecycle` needs the incoming revision already staged under
//! `target/oms-native/<rev>/` (this adapter never builds it).
#[path = "services_incoming.rs"]
mod incoming;
#[path = "services_physical.rs"]
mod physical;
#[path = "services_team_requests.rs"]
mod team_requests;
use crate::services_control::ControlServices;
use oms_command_autonomy::receipt;
use oms_command_hooks::HostServices as HookHostServices;
use oms_command_lifecycle::IncomingPhase;
use oms_command_peer::process::{Completed, Executor as PeerProcessExecutor, Invocation};
use oms_mcp_server::{Captured, CommandExecutor, CommandRequest};
use oms_panel_core::services::{
    ActivityHeartbeat, CommandOutput, ExecutionRequest, NativeCommand, PeerExecutor,
    Services as PanelServices,
};
use oms_panel_core::tick::{ApprovedContract, AutopilotHost, ContractError};
use oms_supervisor::capture::{CaptureOptions, LimitAction};
use oms_supervisor::process::{Completion, Progress, Reactor, Request, Scope, StopReason, Streams};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

const MAX_ARGV: usize = 256;
const MAX_WALL: Duration = Duration::from_secs(86400);
const MAX_OUTPUT: usize = 256 * 1024 * 1024;
const MCP_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const STAGED_WALL: Duration = Duration::from_secs(3600);
const STAGED_OUTPUT: usize = 16 * 1024 * 1024;
const RECEIPT_LIMIT: u64 = 1024 * 1024;
const CATALOG_PERIOD: u64 = 86400;
const CATALOG_MAX_AGE: u64 = 604800;
const NO_LAUNCHER: &str = "staged native --oms-dispatch launcher unavailable";
pub(crate) const DETACH_WORKER_FLAG: &str = "--oms-detach-worker";
const DETACH_TEST_CHILD_ENV: &str = "OMS_DETACH_TEST_CHILD";
const DETACH_ARGV_LIMIT: usize = 1024 * 1024;
const DETACH_PID_PREFIX: &str = "OMS_DETACH_PID:";
const DETACH_ERR_PREFIX: &str = "OMS_DETACH_ERR:";
type Output = (i32, Vec<u8>, Vec<u8>);
static WAKE_SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) struct CollabServices<'a> {
    control: ControlServices<'a>,
    interrupted: &'a Arc<AtomicBool>,
    source: PathBuf,
    /// `[exe, --oms-dispatch, source, rev, --]`, or empty when no staged,
    /// manifest-backed native binary is executing.
    native: Vec<OsString>,
    /// Helper invocation prefix used by `spawn_detached` to start a detached
    /// process group with synchronous `exec` status verification.
    detach_worker: Vec<OsString>,
    /// Per-instance "one resume per tick run" marker. The first resume of any
    /// repository swept through this instance creates it; the file is removed
    /// when the instance drops, so each tick run starts clean.
    wake_once: PathBuf,
}

impl Drop for CollabServices<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.wake_once);
    }
}

fn staged_dispatch_launcher(exe: &Path, source: &Path) -> Vec<OsString> {
    if exe.file_name().and_then(|v| v.to_str()) != Some("oms") {
        return Vec::new();
    }
    let Some(rev_dir) = exe.parent() else {
        return Vec::new();
    };
    let Some(rev) = rev_dir.file_name().and_then(|v| v.to_str()) else {
        return Vec::new();
    };
    if !is_revision(rev) {
        return Vec::new();
    }
    if rev_dir.parent() != Some(source.join("target/oms-native").as_path()) {
        return Vec::new();
    }
    let is_file = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file());
    if !is_file(&rev_dir.join("manifest.json")) || !is_file(exe) {
        return Vec::new();
    }
    vec![
        exe.as_os_str().to_owned(),
        "--oms-dispatch".into(),
        source.as_os_str().to_owned(),
        rev.into(),
        "--".into(),
    ]
}

fn is_revision(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A plain native verb: never option-looking, path-like or empty.
fn is_verb(tool: &str) -> bool {
    !tool.is_empty()
        && tool.len() <= 64
        && tool.as_bytes()[0].is_ascii_lowercase()
        && tool
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl<'a> CollabServices<'a> {
    pub(crate) fn new(interrupted: &'a Arc<AtomicBool>, source: &Path) -> Result<Self, String> {
        let control = ControlServices::new(interrupted, source)?;
        let source = std::fs::canonicalize(source)
            .ok()
            .filter(|p| p.is_dir())
            .ok_or("native source root must be an existing directory")?;
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .ok()
            .filter(|p| p.is_file())
            .ok_or("cannot resolve the executing native binary")?;
        let native = staged_dispatch_launcher(&exe, &source);
        let detach_worker = default_detach_worker(&exe)?;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let wake_once = std::env::temp_dir().join(format!(
            "oms-wake-once-{}-{}-{nanos}",
            std::process::id(),
            WAKE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        Ok(Self {
            control,
            interrupted,
            source,
            native,
            detach_worker,
            wake_once,
        })
    }

    /// Test override of the staged launcher (the test binary is never staged).
    #[cfg(test)]
    fn with_native(mut self, native: Vec<OsString>) -> Self {
        self.native = native;
        self
    }

    /// Replace a leading bare `oms` with the staged launcher; fail closed when
    /// the launcher is unavailable. Other programs run as given.
    fn resolve_argv(&self, mut argv: Vec<OsString>) -> Result<Vec<OsString>, String> {
        if argv.first().is_some_and(|v| v == "oms") {
            if self.native.is_empty() {
                return Err(NO_LAUNCHER.into());
            }
            argv.splice(..1, self.native.iter().cloned());
        }
        Ok(argv)
    }

    fn check(&self, interrupted: &AtomicBool) -> Result<(), String> {
        if interrupted.load(Ordering::Acquire) {
            Err("interrupted".into())
        } else {
            Ok(())
        }
    }
}

pub(crate) fn prelaunch_error(
    error: oms_command_peer::Error,
    stop: &AtomicBool,
    deadline: Instant,
    expires_at: Option<i128>,
) -> oms_command_peer::Error {
    let expired = || {
        expires_at.is_some_and(|expiry| {
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(i128::MAX, |time| time.as_secs() as i128)
                >= expiry
        })
    };
    let code = if stop.load(Ordering::Acquire) {
        130
    } else if Instant::now() >= deadline || expired() {
        124
    } else {
        error.code
    };
    oms_command_peer::Error::new(code, error.message)
}

trait OwnedDuplexConsumer {
    fn check(&self) -> oms_command_peer::Result<()>;
    fn started(
        &mut self,
        reactor: &mut Reactor,
        job: oms_supervisor::process::JobId,
    ) -> oms_command_peer::Result<()>;
    fn event(
        &mut self,
        reactor: &mut Reactor,
        job: oms_supervisor::process::JobId,
        event: oms_supervisor::process::OutputEvent,
    ) -> oms_command_peer::Result<()>;
}

/// One supervised child. `cancel` names the stop recorded on interruption.
struct Job<'a> {
    argv: Vec<OsString>,
    cwd: &'a Path,
    stdin: Option<Vec<u8>>,
    environment: Vec<(OsString, OsString)>,
    remove_environment: Vec<OsString>,
    merged: bool,
    timeout: Duration,
    limit: usize,
    heartbeat: Option<&'a ActivityHeartbeat>,
    cancel: StopReason,
    physical_call: Option<oms_supervisor::process::PhysicalCall>,
    channel: Option<&'a mut dyn OwnedDuplexConsumer>,
    launch_fence: Option<&'a oms_command_peer::process::LaunchFence>,
}

impl<'a> Job<'a> {
    fn new(argv: Vec<OsString>, cwd: &'a Path, timeout: Duration, limit: usize) -> Self {
        Self {
            argv,
            cwd,
            stdin: Some(Vec::new()),
            environment: Vec::new(),
            remove_environment: Vec::new(),
            merged: false,
            timeout,
            limit,
            heartbeat: None,
            cancel: StopReason::Cancelled,
            physical_call: None,
            channel: None,
            launch_fence: None,
        }
    }
}

/// Returns the completion and whether an activity heartbeat failed. Bounds are
/// validated before anything starts.
fn supervise(job: Job<'_>, interrupted: &AtomicBool) -> Result<(Completion, bool), String> {
    if interrupted.load(Ordering::Acquire) {
        return Err("interrupted".into());
    }
    if job.argv.is_empty() || job.argv[0].is_empty() || job.argv.len() > MAX_ARGV {
        return Err("process argv must contain 1..256 arguments".into());
    }
    if job.timeout.is_zero() || job.timeout > MAX_WALL || job.limit == 0 || job.limit > MAX_OUTPUT {
        return Err("invalid process limits".into());
    }
    let program = job.argv.first().and_then(|a| Path::new(a).file_name());
    // A detached tmux session owns a persistent server when no server exists yet.
    let detached_tmux = program.is_some_and(|name| name == "tmux")
        && job.argv.get(1).is_some_and(|arg| arg == "new-session")
        && job.argv.get(2).is_some_and(|arg| arg == "-d");
    let allow_whole_tree =
        !detached_tmux && !matches!(program.and_then(|name| name.to_str()), Some("git" | "ssh"));
    let mut request = Request::new(job.argv);
    request.cwd = Some(std::fs::canonicalize(job.cwd).map_err(|e| e.to_string())?);
    request.stdin = job.stdin;
    let mut channel = job.channel;
    if channel.is_some() {
        request.stdin = None;
        request.duplex = Some(team_requests::options());
    }
    request.physical_call = job.physical_call;
    let mut remove_environment = job.remove_environment;
    for key in ["OMS_NATIVE_OUTCOME", "OMS_NATIVE_DISPATCHED"] {
        if !remove_environment.iter().any(|v| v == key) {
            remove_environment.push(OsString::from(key));
        }
    }
    request.remove_environment = remove_environment
        .into_iter()
        .filter(|name| !job.environment.iter().any(|(key, _)| key == name))
        .collect();
    request.environment = job.environment;
    request.streams = if job.merged {
        Streams::Merged
    } else {
        Streams::Separate
    };
    #[cfg(target_os = "linux")]
    let (scope, mut reactor) = if allow_whole_tree {
        match Reactor::dedicated() {
            Ok(dedicated) => (Scope::WholeTree, dedicated),
            Err(_) => (Scope::ProcessGroup, Reactor::new()),
        }
    } else {
        (Scope::ProcessGroup, Reactor::new())
    };
    #[cfg(not(target_os = "linux"))]
    let (scope, mut reactor) = {
        let _ = allow_whole_tree;
        (Scope::ProcessGroup, Reactor::new())
    };
    request.scope = scope;
    request.deadline = Some(
        Instant::now()
            .checked_add(job.timeout)
            .ok_or("execution deadline overflow")?,
    );
    request.capture = CaptureOptions {
        max_bytes: job.limit,
        on_limit: LimitAction::Terminate,
        lines: None,
    };
    if !job.merged {
        request.stderr_capture = Some(request.capture.clone());
    }
    if let Some(fence) = job.launch_fence {
        fence.check(interrupted).map_err(|e| e.message)?;
    }
    if let Some(channel) = &channel {
        channel.check().map_err(|e| e.message)?;
    }
    let id = reactor.start(request).map_err(|e| e.to_string())?;
    if let Some(channel) = &mut channel {
        channel
            .started(&mut reactor, id)
            .map_err(|_| "Team channel issuance failed; pending intent retained".to_owned())?;
    }
    let mut cancelling = false;
    let mut channel_failed = false;
    let mut beat = Instant::now();
    let mut heartbeat_failed = false;
    loop {
        if let Some(activity) = job.heartbeat
            && !cancelling
            && beat.elapsed() >= Duration::from_secs(60)
        {
            let out = oms_command_operations::run(
                oms_command_operations::App::Events,
                &[
                    "heartbeat".into(),
                    "--attempt".into(),
                    activity.attempt.clone().into(),
                    "--actor".into(),
                    activity.actor.clone().into(),
                ],
                &activity.repo,
                interrupted,
            );
            heartbeat_failed |= out.0 != 0;
            beat = Instant::now();
        }
        if interrupted.load(Ordering::Acquire) && !cancelling {
            reactor.stop(id, job.cancel).map_err(|e| e.to_string())?;
            cancelling = true;
        }
        if let Some(channel) = &channel
            && !channel_failed
            && channel.check().is_err()
        {
            channel_failed = true;
            reactor
                .stop(id, StopReason::Deadline)
                .map_err(|e| e.to_string())?;
        }
        if let Some(channel) = &mut channel {
            while let Some(event) = reactor.receive(id).map_err(|e| e.to_string())? {
                if !channel_failed && channel.event(&mut reactor, id, event).is_err() {
                    channel_failed = true;
                    reactor
                        .stop(id, StopReason::SupervisorError)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        if let Progress::Complete(mut done) = reactor
            .poll(id, Duration::from_millis(50))
            .map_err(|e| e.to_string())?
        {
            if let Some(channel) = &mut channel
                && let Some(terminal) = &mut done.duplex
            {
                while let Some(event) = terminal.output.pop_front() {
                    // A terminal child cannot receive new responses. Unconsumed
                    // requests remain incomplete instead of gaining an ACK.
                    if !channel_failed && channel.event(&mut reactor, id, event).is_err() {
                        channel_failed = true;
                    }
                }
            }
            return Ok((done, heartbeat_failed || channel_failed));
        }
    }
}

fn stop_code(done: &Completion) -> i32 {
    match done.stop {
        Some(StopReason::Deadline) => 124,
        Some(StopReason::Cancelled) => 130,
        Some(StopReason::OutputLimit | StopReason::SupervisorError) => 125,
        _ => done.exit.map_or(125, |e| e.shell_code()),
    }
}

/// Distinct, accurate text for every way the supervisor can stop a command.
fn stop_message(stop: StopReason, supervision: Option<&str>, limit: usize) -> String {
    match stop {
        StopReason::Cancelled => "interrupted".into(),
        StopReason::Signal(n) => format!("command stopped by signal {n}"),
        StopReason::Deadline => "command timed out".into(),
        StopReason::OutputLimit => format!("command output exceeded the {limit}-byte limit"),
        StopReason::ConsumerCutoff => {
            "output consumer cut the command off before it finished".into()
        }
        StopReason::SupervisorError => format!(
            "native supervisor failure: {}",
            supervision.unwrap_or("unspecified")
        ),
    }
}

/// Run one staged native command to completion with the update-parent marker
/// in its environment only. Child exit, deadline and cancellation map to
/// shell-style codes; incomplete cleanup is never reported as success.
fn run_staged(
    launcher: &[OsString],
    args: &[&str],
    cwd: &Path,
    environment: Vec<(OsString, OsString)>,
    interrupted: &AtomicBool,
) -> Output {
    let mut argv = launcher.to_vec();
    argv.extend(args.iter().map(OsString::from));
    let mut job = Job::new(argv, cwd, STAGED_WALL, STAGED_OUTPUT);
    job.environment = environment;
    let (done, _) = match supervise(job, interrupted) {
        Ok(v) => v,
        Err(e) => {
            let code = if e == "interrupted" { 130 } else { 2 };
            return (code, Vec::new(), format!("error: {e}\n").into_bytes());
        }
    };
    let code = if done.stop.is_none() && !(done.cleanup_complete() && done.output_complete()) {
        75
    } else {
        stop_code(&done)
    };
    let stderr = done.stderr.map_or_else(Vec::new, |s| s.bytes);
    (code, done.stdout.bytes, stderr)
}

fn default_detach_worker(exe: &Path) -> Result<Vec<OsString>, String> {
    if cfg!(test) {
        let test_exe = std::env::current_exe()
            .map_err(|e| format!("cannot resolve test executable for detach helper: {e}"))?;
        Ok(vec![
            test_exe.into(),
            "services_collab::tests::detach_worker_child_entry".into(),
            "--exact".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ])
    } else {
        Ok(vec![exe.as_os_str().to_owned(), DETACH_WORKER_FLAG.into()])
    }
}

fn encode_detach_argv(argv: &[OsString]) -> Result<Vec<u8>, String> {
    use std::os::unix::ffi::OsStrExt;
    let count = u32::try_from(argv.len()).map_err(|_| "detach argv overflow".to_owned())?;
    let mut framed = Vec::new();
    framed.extend_from_slice(&count.to_le_bytes());
    for arg in argv {
        let bytes = arg.as_bytes();
        let len = u32::try_from(bytes.len()).map_err(|_| "detach arg overflow".to_owned())?;
        framed.extend_from_slice(&len.to_le_bytes());
        framed.extend_from_slice(bytes);
        if framed.len() > DETACH_ARGV_LIMIT {
            return Err("detach argv exceeds byte budget".into());
        }
    }
    Ok(framed)
}

fn decode_detach_argv(raw: &[u8]) -> Result<Vec<OsString>, String> {
    use std::os::unix::ffi::OsStringExt;
    let mut pos = 0usize;
    let take_u32 = |pos: &mut usize| -> Result<usize, String> {
        let end = pos.checked_add(4).ok_or("detach framing overflow")?;
        let slice: [u8; 4] = raw
            .get(*pos..end)
            .and_then(|s| s.try_into().ok())
            .ok_or("truncated detach argv header")?;
        *pos = end;
        Ok(u32::from_le_bytes(slice) as usize)
    };
    let count = take_u32(&mut pos)?;
    if count == 0 || count > MAX_ARGV {
        return Err("invalid detach argv count".into());
    }
    let mut argv = Vec::with_capacity(count);
    for _ in 0..count {
        let len = take_u32(&mut pos)?;
        let end = pos.checked_add(len).ok_or("detach arg overflow")?;
        let bytes = raw.get(pos..end).ok_or("truncated detach arg")?;
        pos = end;
        argv.push(OsString::from_vec(bytes.to_vec()));
    }
    if pos != raw.len() || argv[0].is_empty() {
        return Err("malformed detach argv stream".into());
    }
    Ok(argv)
}

/// Entry point of the short-lived detach helper: reads raw framed `argv` from
/// stdin, spawns it in a new process group with `sink` (`stderr`) duplicated
/// onto stdout+stderr, and prints `OMS_DETACH_PID:<pid>` only after `execve`
/// succeeds (or `OMS_DETACH_ERR:<msg>` on `Command::spawn` failure) before exiting
/// immediately so the parent reaps the helper and keeps no direct child.
pub(crate) fn detach_worker_main() -> i32 {
    use std::io::{Read, Write};
    use std::os::fd::AsFd;
    use std::process::Stdio;
    let report_err = |msg: &str| -> i32 {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "\n{DETACH_ERR_PREFIX}{msg}");
        let _ = out.flush();
        1
    };
    let mut raw = Vec::new();
    if let Err(e) = std::io::stdin()
        .take((DETACH_ARGV_LIMIT + 1) as u64)
        .read_to_end(&mut raw)
    {
        return report_err(&e.to_string());
    }
    if raw.len() > DETACH_ARGV_LIMIT {
        return report_err("detach argv exceeds byte budget");
    }
    let argv = match decode_detach_argv(&raw) {
        Ok(v) => v,
        Err(e) => return report_err(&e),
    };
    let stderr_fd = std::io::stderr();
    let Ok(out_fd) = stderr_fd.as_fd().try_clone_to_owned() else {
        return report_err("cannot duplicate detach output fd");
    };
    let Ok(err_fd) = stderr_fd.as_fd().try_clone_to_owned() else {
        return report_err("cannot duplicate detach error fd");
    };
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_remove(DETACH_TEST_CHILD_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_fd))
        .stderr(Stdio::from(err_fd))
        .process_group(0);
    match cmd.spawn() {
        Ok(child) => {
            let mut out = std::io::stdout().lock();
            if writeln!(out, "\n{DETACH_PID_PREFIX}{}", child.id()).is_err() || out.flush().is_err()
            {
                return 1;
            }
            0
        }
        Err(e) => report_err(&e.to_string()),
    }
}

/// Start `argv` as a detached process group and return its pid without leaving
/// a direct child of this process, reporting synchronous `execve` failures
/// (`ENOENT`/`EACCES`/`ENOEXEC`) before returning. The helper is reaped before
/// this returns; the child has no stdin and both output streams go to `sink`.
fn spawn_detached(
    detach_worker: &[OsString],
    argv: Vec<OsString>,
    cwd: &Path,
    environment: &[(&str, &str)],
    sink: std::process::Stdio,
) -> Result<u32, String> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    if detach_worker.is_empty() {
        return Err("detach helper is unavailable".into());
    }
    if argv.is_empty() || argv[0].is_empty() || argv.len() > MAX_ARGV {
        return Err("process argv must contain 1..256 arguments".into());
    }
    let framed = encode_detach_argv(&argv)?;
    let mut helper = std::process::Command::new(&detach_worker[0]);
    helper
        .args(&detach_worker[1..])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(sink)
        .process_group(0);
    if cfg!(test) {
        helper.env(DETACH_TEST_CHILD_ENV, "1");
    }
    for (key, value) in environment {
        helper.env(key, value);
    }
    let mut helper = helper.spawn().map_err(|e| e.to_string())?;
    let write_res = helper
        .stdin
        .take()
        .ok_or("detach helper has no stdin")?
        .write_all(&framed);
    let mut text = String::new();
    let read = helper
        .stdout
        .take()
        .ok_or("detach helper has no output")?
        .take(4096)
        .read_to_string(&mut text);
    let status = helper.wait().map_err(|e| e.to_string())?;
    if let Err(e) = write_res
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(e.to_string());
    }
    read.map_err(|e| e.to_string())?;
    if let Some(msg) = text
        .lines()
        .find_map(|line| line.strip_prefix(DETACH_ERR_PREFIX))
    {
        return Err(format!("detached launch failed: {msg}"));
    }
    if !status.success() {
        return Err("detach helper failed".into());
    }
    text.lines()
        .find_map(|line| line.strip_prefix(DETACH_PID_PREFIX))
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| "detach helper returned no pid".into())
}

fn panel_error(message: impl ToString) -> oms_panel_core::Error {
    oms_panel_core::Error::new(message)
}

impl PeerExecutor for CollabServices<'_> {
    fn execute(
        &self,
        request: &ExecutionRequest,
        interrupted: &AtomicBool,
    ) -> oms_panel_core::Result<CommandOutput> {
        if interrupted.load(Ordering::Acquire) {
            return Err(oms_panel_core::Error {
                code: 130,
                ..panel_error("panel operation interrupted")
            });
        }
        let argv = self
            .resolve_argv(request.argv.clone())
            .map_err(panel_error)?;
        let (done, heartbeat_failed) = supervise(
            Job {
                stdin: request.stdin.clone(),
                environment: request.environment.clone(),
                remove_environment: request.remove_environment.clone(),
                heartbeat: request.heartbeat.as_ref(),
                cancel: StopReason::Signal(2),
                ..Job::new(argv, &request.cwd, request.timeout, request.output_limit)
            },
            interrupted,
        )
        .map_err(panel_error)?;
        let limited = done.stdout.limited || done.stderr.as_ref().is_some_and(|v| v.limited);
        let (cleanup_complete, output_complete) = (done.cleanup_complete(), done.output_complete());
        let mut stderr = done.stderr.map(|v| v.bytes).unwrap_or_default();
        if heartbeat_failed {
            stderr.extend_from_slice(b"\nOMS activity update unavailable\n");
        }
        Ok(CommandOutput {
            code: done.exit.map_or(75, |v| v.shell_code()),
            stdout: done.stdout.bytes,
            stderr,
            cleanup_complete,
            output_complete,
            limited,
        })
    }
}

impl PeerProcessExecutor for CollabServices<'_> {
    fn context_dependencies(
        &self,
        repo: &Path,
        targets: &[PathBuf],
        _: &AtomicBool,
    ) -> oms_command_plan::Result<Vec<oms_runtime::services::RelatedSource>> {
        let services =
            crate::services_runtime::RuntimeServices::new(self.interrupted, &self.source)
                .map_err(|message| oms_command_plan::Error { code: 2, message })?;
        oms_runtime::Services::context_dependencies(&services, repo, targets)
    }
    fn group_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        Some(crate::services_runtime::resolve_group_membership)
    }
    fn execute_team_requests(
        &self,
        input: Invocation,
        binding: oms_command_peer::process::TeamLaunchBinding<'_>,
        interrupted: &AtomicBool,
    ) -> oms_command_peer::Result<Completed> {
        self.team_requests(input, binding, interrupted)
    }

    fn with_physical_contract(
        &self,
        attempt: &oms_command_peer::process::PhysicalAttempt<'_>,
        deadline: Instant,
        interrupted: &AtomicBool,
        register: oms_command_peer::process::RegisterPhysical<'_>,
    ) -> oms_command_peer::Result<()> {
        self.physical_contract(attempt, deadline, interrupted, register)
    }
    fn execute(
        &self,
        input: Invocation,
        interrupted: &AtomicBool,
    ) -> oms_command_peer::Result<Completed> {
        let fail = |code, e: String| oms_command_peer::Error::new(code, e);
        let argv = self.resolve_argv(input.argv).map_err(|e| fail(2, e))?;
        if let Some(fence) = &input.launch_fence {
            fence.check(interrupted)?;
        }
        let (done, _) = supervise(
            Job {
                stdin: input.input,
                environment: input.environment,
                remove_environment: input.remove_environment,
                merged: input.merge,
                physical_call: input.physical_call,
                launch_fence: input.launch_fence.as_ref(),
                ..Job::new(argv, &input.cwd, input.timeout, input.max_bytes)
            },
            interrupted,
        )
        .map_err(|e| fail(if e == "interrupted" { 130 } else { 125 }, e))?;
        let complete = done.stop.is_none()
            && done.output_complete()
            && done.cleanup_complete()
            && done.supervision_error.is_none();
        let cleanup_complete = Some(done.cleanup_complete());
        let owned_cleanup = done.owned_cleanup().cloned();
        Ok(Completed {
            code: stop_code(&done),
            stdout: done.stdout.bytes,
            stderr: done.stderr.map_or_else(Vec::new, |s| s.bytes),
            complete,
            cleanup_complete,
            owned_cleanup,
            elapsed: done.duration,
        })
    }
}

fn output(v: Output) -> CommandOutput {
    CommandOutput {
        code: v.0,
        stdout: v.1,
        stderr: v.2,
        cleanup_complete: true,
        output_complete: true,
        limited: false,
    }
}

/// `oms` argv prefix (after the launcher) for running `command` in the staged
/// binary, matching each verb's own parser. `models`, `cleanup` and
/// `peer-review` take the repository from the working directory only (`models`
/// and `cleanup` reject `--repo`; `peer-review` keeps leading subcommands in
/// `argv[0]`). The failure namespace lives under `runtime`, and the bundle
/// parser accepts `--repo` only before its `show` action.
fn staged_prefix(command: NativeCommand, cwd: &Path) -> Vec<OsString> {
    let repo = |verb: &str| -> Vec<OsString> { vec![verb.into(), "--repo".into(), cwd.into()] };
    match command {
        NativeCommand::ProviderCatalog | NativeCommand::Cleanup | NativeCommand::Review => {
            vec![command.verb().into()]
        }
        NativeCommand::Failure => vec![
            "runtime".into(),
            "--repo".into(),
            cwd.into(),
            "failure".into(),
        ],
        _ => repo(command.verb()),
    }
}

fn room_error(message: impl ToString) -> oms_command_room::Error {
    oms_command_room::Error::new(message.to_string())
}

struct RoomOperations<'a> {
    repo: PathBuf,
    attempts: Vec<oms_command_room::Attempt>,
    fingerprint: String,
    deadline: Option<Instant>,
    stop: &'a AtomicBool,
    _guard: Box<dyn oms_command_plan::services::LifecycleGuard + 'a>,
}

fn room_operations_projection(
    repo: &Path,
) -> oms_command_room::Result<(String, Vec<oms_command_room::Attempt>)> {
    let (rows, projected) = oms_command_operations::load_projection(repo).map_err(room_error)?;
    let mut hash = Sha256::new();
    for row in rows {
        hash.update(row.json(false).map_err(room_error)?);
        hash.update(b"\n");
    }
    let attempts = projected
        .iter()
        .map(|row| {
            let raw = row.json(false).map_err(room_error)?;
            let row = oms_command_room::value::parse(&raw, oms_command_room::MAX_ROW)?;
            oms_command_room::Attempt::from_projection_row(&row)
        })
        .collect::<oms_command_room::Result<Vec<_>>>()?;
    Ok((format!("{:x}", hash.finalize()), attempts))
}

impl oms_command_room::OperationsRead for RoomOperations<'_> {
    fn attempts(&self) -> &[oms_command_room::Attempt] {
        &self.attempts
    }
    fn check(&self) -> oms_command_room::Result<()> {
        if self.stop.load(Ordering::Acquire) || self.deadline.is_some_and(|at| Instant::now() >= at)
        {
            return Err(oms_command_room::Error::new(
                "room operation interrupted or deadline expired",
            ));
        }
        if room_operations_projection(&self.repo)?.0 != self.fingerprint {
            return Err(oms_command_room::Error::new(
                "operations changed during room publication",
            ));
        }
        Ok(())
    }
}

fn room_delivery_receipt(
    value: Value,
) -> oms_command_room::Result<oms_command_room::DeliveryReceipt> {
    let status = value["status"]
        .as_str()
        .ok_or_else(|| oms_command_room::Error::new("native delivery status unavailable"))?
        .to_owned();
    let delivery_unknown = value["delivery_unknown"]
        .as_bool()
        .ok_or_else(|| oms_command_room::Error::new("native delivery uncertainty unavailable"))?;
    let turn_id = match value.get("turn_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if id.is_empty() => None,
        Some(Value::String(id)) => Some(id.clone()),
        _ => {
            return Err(oms_command_room::Error::new(
                "invalid native delivery turn identity",
            ));
        }
    };
    let raw = serde_json::to_vec(&value).map_err(room_error)?;
    Ok(oms_command_room::DeliveryReceipt {
        status,
        delivery_unknown,
        turn_id,
        fields: oms_command_room::value::parse(&raw, oms_command_room::MAX_ROW)?,
    })
}

impl oms_command_room::Services for CollabServices<'_> {
    fn state_repository(&self, repo: &Path) -> oms_command_room::Result<PathBuf> {
        oms_command_autonomy::Services::state_repository(&self.control, repo, self.interrupted)
            .map_err(room_error)
    }
    fn canonical_provider(&self, provider: &str) -> oms_command_room::Result<String> {
        oms_command_room::Services::canonical_provider(&oms_command_room::LocalServices, provider)
    }
    fn current_claude_session(&self) -> oms_command_room::Result<String> {
        self.check(self.interrupted).map_err(room_error)?;
        oms_command_hooks::notify::current_claude_session().map_err(room_error)
    }
    fn operations<'a>(
        &'a self,
        repo: &Path,
    ) -> oms_command_room::Result<Box<dyn oms_command_room::OperationsRead + 'a>> {
        self.check(self.interrupted).map_err(room_error)?;
        let guard = oms_command_plan::Services::lifecycle_writer(&self.control, repo)
            .map_err(room_error)?;
        let (fingerprint, attempts) = room_operations_projection(repo)?;
        Ok(Box::new(RoomOperations {
            repo: repo.to_owned(),
            attempts,
            fingerprint,
            deadline: None,
            stop: self.interrupted,
            _guard: guard,
        }))
    }
    fn operations_before<'a>(
        &'a self,
        repo: &Path,
        deadline: Instant,
    ) -> oms_command_room::Result<Box<dyn oms_command_room::OperationsRead + 'a>> {
        self.check(self.interrupted).map_err(room_error)?;
        let guard =
            oms_command_plan::Services::lifecycle_writer_before(&self.control, repo, deadline)
                .map_err(room_error)?;
        let (fingerprint, attempts) = room_operations_projection(repo)?;
        if Instant::now() >= deadline {
            return Err(room_error("native Room operation deadline expired"));
        }
        Ok(Box::new(RoomOperations {
            repo: repo.to_owned(),
            attempts,
            fingerprint,
            deadline: Some(deadline),
            stop: self.interrupted,
            _guard: guard,
        }))
    }
    fn delivery_available(&self, _: oms_command_room::App) -> bool {
        // This is availability of the compiled producer, not proof of a live
        // receiver. Its receipt preserves absent, busy and uncertain outcomes.
        true
    }
    fn deliver(
        &self,
        request: oms_command_room::DeliveryRequest<'_>,
    ) -> oms_command_room::Result<oms_command_room::DeliveryReceipt> {
        self.check(self.interrupted).map_err(room_error)?;
        let value = match request.app {
            oms_command_room::App::Codex => {
                if request.allow_wakeup || request.consumer.is_some() {
                    return Err(oms_command_room::Error::new(
                        "invalid Codex delivery options",
                    ));
                }
                oms_command_hooks::notify::deliver_codex(request.repository, request.text, 10.0)
            }
            oms_command_room::App::Claude => oms_command_hooks::notify::deliver_claude(
                request.repository,
                request.text,
                request.consumer.unwrap_or(""),
                request.allow_wakeup,
            ),
        };
        // Do not turn cancellation after submission into a no-delivery claim.
        room_delivery_receipt(value)
    }
}

impl PanelServices for CollabServices<'_> {
    fn team_contracts(
        &self,
        execution: &Path,
        state: &Path,
        stop: &AtomicBool,
    ) -> oms_panel_core::Result<CommandOutput> {
        let request =
            oms_command_plan::Request::new(oms_command_plan::Action::TeamList).flag("json");
        let context = oms_command_plan::Context::from_selected_state_before(
            &request,
            execution,
            execution,
            state,
            stop,
            Instant::now() + Duration::from_secs(10),
        )
        .map_err(|error| oms_panel_core::Error {
            code: error.code,
            ..panel_error(error)
        })?;
        let plan = oms_command_plan::Plan::open(context, &self.control, stop).map_err(|error| {
            oms_panel_core::Error {
                code: error.code,
                ..panel_error(error)
            }
        })?;
        let response = plan
            .execute(&request)
            .map_err(|error| oms_panel_core::Error {
                code: error.code,
                ..panel_error(error)
            })?;
        Ok(output((response.code, response.stdout, response.stderr)))
    }
    fn physical_executor(&self) -> Option<&dyn PeerProcessExecutor> {
        Some(self)
    }
    fn publication_receipt(
        &self,
        repo: &Path,
        execution: &Path,
        commit: &str,
        receipt_sha256: &str,
        stop: &AtomicBool,
    ) -> oms_panel_core::Result<Vec<u8>> {
        self.check(stop).map_err(panel_error)?;
        if receipt_sha256.len() != 64
            || !receipt_sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(panel_error("publication receipt digest is required"));
        }
        let request = oms_command_plan::Request::new(oms_command_plan::Action::Status);
        let mut context = oms_command_plan::Context::from_environment(&request, execution)
            .map_err(panel_error)?;
        context.state_repo = repo.to_owned();
        context.plan_file = repo.join(".oms/plan/tasks.json");
        let plan =
            oms_command_plan::Plan::open(context, &self.control, stop).map_err(panel_error)?;
        let proof =
            oms_command_plan::completion::commit_landing(&plan, commit, Some(receipt_sha256))
                .map_err(panel_error)?;
        self.check(stop).map_err(panel_error)?;
        Ok(proof.raw)
    }

    fn deliver_result(
        &self,
        repo: &Path,
        message: &str,
        stop: &AtomicBool,
    ) -> oms_panel_core::Result<Value> {
        self.check(stop).map_err(panel_error)?;
        // This producer targets only its saved Codex receiver. Claude wakeups
        // require the explicit recipient and authority on `room publish`.
        Ok(oms_command_hooks::notify::deliver_codex(
            repo, message, 10.0,
        ))
    }
    /// In process; the repository is the working directory every native parser
    /// defaults to, so arguments are passed through unchanged.
    fn command(
        &self,
        command: NativeCommand,
        repo: &Path,
        args: &[OsString],
        interrupted: &AtomicBool,
    ) -> oms_panel_core::Result<CommandOutput> {
        self.check(interrupted).map_err(|e| oms_panel_core::Error {
            code: 130,
            ..panel_error(e)
        })?;
        let verb = command.verb();
        let out = match command {
            NativeCommand::PeerAsk
            | NativeCommand::Delegate
            | NativeCommand::Review
            | NativeCommand::Advise => oms_command_peer::run_with_executor(
                verb,
                args,
                repo,
                &self.source,
                interrupted,
                self,
            ),
            NativeCommand::Cleanup | NativeCommand::Admit | NativeCommand::Land => {
                oms_command_landing::run_with_services(
                    verb,
                    args,
                    repo,
                    &self.source,
                    interrupted,
                    &self.control,
                )
            }
            NativeCommand::ProviderCatalog => oms_command_providers_skills::run_with_source(
                verb,
                args,
                repo,
                &self.source,
                interrupted,
            ),
            NativeCommand::Room => {
                oms_command_room::run_with_services(args, repo, interrupted, self)
            }
            NativeCommand::Events => oms_command_operations::run(
                oms_command_operations::App::Events,
                args,
                repo,
                interrupted,
            ),
            NativeCommand::Bundle => oms_command_attempt_bundle::run(args, repo, interrupted),
            NativeCommand::Telemetry => oms_command_telemetry::run(args, repo, interrupted),
            NativeCommand::Failure => oms_command_failure::run(args, repo, interrupted),
            NativeCommand::Plan => {
                oms_command_plan::run_with_services(args, repo, interrupted, &self.control)
            }
            NativeCommand::Task => {
                // Never lock process stdin: it may be a protocol stream.
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let code = oms_command_task::run_with_io(
                    args,
                    repo,
                    interrupted,
                    None,
                    &mut self.control.task(),
                    &mut std::io::empty(),
                    &mut out,
                    &mut err,
                );
                (code, out, err)
            }
        };
        Ok(output(out))
    }

    /// Without environment deltas this is `command`; with them the verb runs in
    /// the staged native binary so the delta reaches a real process.
    fn command_context(
        &self,
        command: NativeCommand,
        request: &ExecutionRequest,
        interrupted: &AtomicBool,
    ) -> oms_panel_core::Result<CommandOutput> {
        if request.environment.is_empty() && request.remove_environment.is_empty() {
            return PanelServices::command(self, command, &request.cwd, &request.argv, interrupted);
        }
        let mut argv: Vec<OsString> = vec!["oms".into()];
        argv.extend(staged_prefix(command, &request.cwd));
        argv.extend(request.argv.iter().cloned());
        let request = ExecutionRequest {
            argv,
            ..request.clone()
        };
        PeerExecutor::execute(self, &request, interrupted)
    }
}

/// Opens `<cwd>/.oms/tick` (creating it) without following links, then the
/// private append-only log inside it.
fn open_tick_log(log: &Path, cwd: &Path) -> Result<std::fs::File, String> {
    use rustix::fs::{Mode, OFlags, mkdirat, open, openat};
    use rustix::io::Errno;
    let root = std::fs::canonicalize(cwd).map_err(|_| "repository root is unavailable")?;
    let name = log
        .file_name()
        .filter(|_| log.parent() == Some(root.join(".oms/tick").as_path()))
        .or_else(|| {
            log.file_name()
                .filter(|_| log.parent() == Some(cwd.join(".oms/tick").as_path()))
        })
        .ok_or_else(|| format!("invalid launch log path {}", log.display()))?;
    let mut dir = open(
        &root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "repository root cannot be opened")?;
    for seg in [".oms", "tick"] {
        match mkdirat(&dir, seg, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(_) => return Err(format!("cannot create {seg} directory")),
        }
        dir = openat(
            &dir,
            seg,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| match e {
            Errno::LOOP | Errno::NOTDIR => format!("{seg} is a symlink or not a directory"),
            _ => format!("{seg} cannot be opened"),
        })?;
    }
    let fd = openat(
        &dir,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    let file = std::fs::File::from(fd);
    let meta = file
        .metadata()
        .map_err(|e| format!("cannot stat {}: {e}", log.display()))?;
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != users::get_effective_uid() {
        return Err(format!(
            "refusing {}: launch log must be an owned single-link regular file",
            log.display()
        ));
    }
    Ok(file)
}

impl CollabServices<'_> {
    /// A resume that could never launch must be refused before the tick
    /// records `fired` or `resumed`: with no staged launcher, an otherwise
    /// re-enterable approved contract is refused here.
    fn launcher_refusal(&self, base: String, approved_run: bool) -> String {
        if base.is_empty() && approved_run && self.native.is_empty() {
            format!("{NO_LAUNCHER}; nothing was recorded")
        } else {
            base
        }
    }
}

impl AutopilotHost for CollabServices<'_> {
    fn load_contract(&self, repo: &Path) -> Result<ApprovedContract, ContractError> {
        let checked = receipt::load_checked_receipt(repo)
            .map_err(|e| ContractError::Invalid(e.to_string()))?
            .ok_or(ContractError::Missing)?;
        let argv = receipt::approved_continuation(&checked.row);
        Ok(ApprovedContract {
            contract_digest: receipt::contract_digest(&checked.row),
            owner_id: checked.row["owner_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            stage: checked.row["stage"].as_str().unwrap_or_default().to_owned(),
            approved_run: argv.is_some(),
            argv: argv.unwrap_or_default(),
            row: checked.row,
            receipt_sha256: checked.sha256,
        })
    }

    fn reentry_refusal(&self, repo: &Path, contract: &ApprovedContract) -> Result<String, String> {
        let checked = receipt::load_checked_receipt(repo)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "autopilot receipt is missing".to_owned())?;
        if checked.sha256 != contract.receipt_sha256 || checked.row != contract.row {
            return Err("autopilot receipt changed before re-entry".into());
        }
        let base =
            receipt::reentry_refusal(repo, &checked, &self.control, self.interrupted.as_ref())
                .map_err(|e| e.to_string())?;
        Ok(self.launcher_refusal(base, contract.approved_run))
    }

    fn stale_approval(&self, repo: &Path, contract: &ApprovedContract, now: i64) -> String {
        receipt::stale_approval(
            repo,
            &contract.row,
            now,
            std::env::var(receipt::APPROVAL_MAX_AGE_ENV).ok().as_deref(),
        )
    }

    /// The staged native binary, never a bare `current_exe()`.
    fn executable(&self) -> std::io::Result<PathBuf> {
        self.native
            .first()
            .map(PathBuf::from)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Unsupported, NO_LAUNCHER))
    }

    /// Refuses before any log or child exists unless the staged launcher is
    /// available and `argv` is an `oms ...` continuation. The child is fully
    /// detached (see `spawn_detached`): its own process group, no stdin, the
    /// private log for both streams, `OMS_AUTOPILOT_WAKE_RESUME=1`, and no
    /// direct child of this process left behind. A kill between log creation
    /// and the spawn leaves only an empty private log.
    fn launch(&self, argv: &[String], log: &Path, cwd: &Path) -> Result<u32, String> {
        if argv.first().map(String::as_str) != Some("oms") {
            return Err("autopilot continuation must start with `oms`".into());
        }
        if self.native.is_empty() {
            return Err(NO_LAUNCHER.into());
        }
        self.check(self.interrupted)?;
        let existed = std::fs::symlink_metadata(log).is_ok();
        let file = open_tick_log(log, cwd)?;
        let mut full = self.native.clone();
        full.extend(argv.iter().skip(1).map(OsString::from));
        match spawn_detached(
            &self.detach_worker,
            full,
            cwd,
            &[("OMS_AUTOPILOT_WAKE_RESUME", "1")],
            file.into(),
        ) {
            Ok(pid) => Ok(pid),
            Err(e) => {
                if !existed {
                    let _ = std::fs::remove_file(log);
                }
                Err(e)
            }
        }
    }
}

/// Hook host: detached background work goes through the staged native binary.
struct HookHost {
    native: Vec<OsString>,
    detach_worker: Vec<OsString>,
}

impl HookHostServices for HookHost {
    fn supports_detach(&self) -> bool {
        !self.native.is_empty()
    }

    fn detach_command(&self, tool: &str, args: &[OsString], cwd: &Path) -> Result<(), String> {
        if self.native.is_empty() {
            return Err(NO_LAUNCHER.into());
        }
        if !is_verb(tool) {
            return Err(format!("detached launch for '{tool}' is refused"));
        }
        let mut argv = self.native.clone();
        argv.push(tool.into());
        argv.extend(args.iter().cloned());
        spawn_detached(
            &self.detach_worker,
            argv,
            cwd,
            &[],
            std::process::Stdio::null(),
        )
        .map(|_| ())
    }

    fn check_python_syntax(&self, path: &Path, source: &[u8]) -> Option<bool> {
        crate::check_python_syntax_with_companion(path, source)
    }
}

// ------------------------------------------------------------------ lifecycle

fn refuse(code: i32, message: impl AsRef<str>) -> Output {
    (
        code,
        Vec::new(),
        format!("error: {}\n", message.as_ref()).into_bytes(),
    )
}

fn env_text(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_days(name: &str, default: u64) -> Result<u64, String> {
    match env_text(name) {
        None => Ok(default),
        Some(v) if v.bytes().all(|b| b.is_ascii_digit()) => {
            v.parse().map_err(|_| format!("{name} must be an integer"))
        }
        Some(_) => Err(format!("{name} must be an integer")),
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` to Unix seconds.
fn parse_utc(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 20
        || (b[4], b[7], b[10], b[13], b[16], b[19]) != (b'-', b'-', b'T', b':', b':', b'Z')
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days from civil (proleptic Gregorian).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + s)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A timestamp at least `days` old; an unparseable one is never old.
fn aged(text: &str, days: u64) -> bool {
    parse_utc(text).is_some_and(|t| now_secs().saturating_sub(t) >= (days as i64) * 86400)
}

/// True when the file's mtime is at least `seconds` old, or it is absent.
fn file_older_than(path: &Path, seconds: u64) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age.as_secs() >= seconds)
}

/// `## Goal` section empty (and present), as the legacy tick judged it.
fn goal_is_empty(text: &str) -> bool {
    let (mut seen, mut empty, mut inside) = (false, true, false);
    for line in text.lines() {
        if line == "## Goal" {
            (inside, seen) = (true, true);
        } else if inside && line.starts_with("## ") {
            inside = false;
        } else if inside {
            let line = line.trim();
            if !line.is_empty() && line != "(... entry clipped ...)" {
                empty = false;
            }
        }
    }
    seen && empty
}

/// `artifact-index: N <kind> failure(s) resolved`: exactly one matching line.
fn resolved_count(stdout: &[u8], kind: &str) -> Option<u64> {
    let suffix = format!(" {kind} failure(s) resolved");
    let mut found = String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|l| {
            l.strip_prefix("artifact-index: ")?
                .strip_suffix(suffix.as_str())?
                .parse::<u64>()
                .ok()
        })
        .collect::<Vec<_>>();
    (found.len() == 1).then(|| found.remove(0))
}

fn json_of(out: &Output) -> Option<Value> {
    (out.0 == 0)
        .then(|| serde_json::from_slice(&out.1).ok())
        .flatten()
}

fn plan_idle(tasks_json: &[u8], days: u64) -> bool {
    let Ok(doc) = serde_json::from_slice::<Value>(tasks_json) else {
        return false;
    };
    let Some(tasks) = doc["tasks"].as_object().filter(|t| !t.is_empty()) else {
        return false;
    };
    tasks
        .values()
        .all(|t| t["state"] == "done" && t["updated"].as_str().is_some_and(|u| aged(u, days)))
}

/// Parsed `--connect-services` / `--peer-permissions` / `--notion-data-source`
/// / `--star` options of an install.
#[derive(Debug, PartialEq, Eq)]
struct Extras {
    connect: bool,
    permissions: bool,
    data_source: Option<String>,
}

fn plan_extras(args: &[OsString]) -> Result<Extras, String> {
    let mut extras = Extras {
        connect: false,
        permissions: false,
        data_source: None,
    };
    let mut iter = args.iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--connect-services" => extras.connect = true,
            "--peer-permissions" => extras.permissions = true,
            "--star" => {
                return Err("install --star has no native producer; install unchanged".into());
            }
            "--notion-data-source" => {
                extras.data_source = Some(iter.next().ok_or("--notion-data-source needs an ID")?);
            }
            other => {
                if let Some(id) = other.strip_prefix("--notion-data-source=") {
                    extras.data_source = Some(id.to_owned());
                }
            }
        }
    }
    if let Some(id) = &extras.data_source
        && (id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    {
        return Err("invalid --notion-data-source ID".into());
    }
    Ok(extras)
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file()))
}

fn capability_receipt() -> Option<PathBuf> {
    env_text("OMS_CAPABILITY_RECEIPT")
        .map(PathBuf::from)
        .or_else(|| {
            let config = env_text("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| env_text("HOME").map(|h| Path::new(&h).join(".config")))?;
            Some(config.join("oh-my-setting/capabilities.json"))
        })
}

/// Tool refresh verb: an install that chose its capabilities reapplies that
/// choice; an older install keeps the full tool refresh.
fn tools_refresh(profile_receipt: bool) -> &'static [&'static str] {
    if profile_receipt {
        &["install-profile", "--reapply", "--upgrade"]
    } else {
        &["install-tools", "--upgrade"]
    }
}

impl CollabServices<'_> {
    fn incoming(&self, source: &Path, revision: &str) -> Result<Vec<OsString>, String> {
        if !is_revision(revision) {
            return Err("incoming revision must be a full lowercase commit id".into());
        }
        let source = std::fs::canonicalize(source).map_err(|_| "source root is unavailable")?;
        let launcher = staged_dispatch_launcher(
            &source.join("target/oms-native").join(revision).join("oms"),
            &source,
        );
        if launcher.is_empty() {
            return Err(format!(
                "incoming staged native binary for {revision} is unavailable; stage it first"
            ));
        }
        Ok(launcher)
    }

    fn catalog_refresh(
        &self,
        dry_run: bool,
        stamp: &Path,
        cache: &Path,
        source: &Path,
        interrupted: &AtomicBool,
    ) -> Output {
        let stale = || -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(cache)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.strip_suffix(".models")?.to_owned();
                    (is_verb(&name.replace(['.', '_'], "-"))
                        && file_older_than(&e.path(), CATALOG_MAX_AGE))
                    .then_some(name)
                })
                .collect();
            names.sort();
            names
        };
        if dry_run {
            let text: String = if file_older_than(stamp, CATALOG_PERIOD) {
                stale()
                    .iter()
                    .map(|p| format!("would refresh catalog: {p}\n"))
                    .collect()
            } else {
                String::new()
            };
            return (0, text.into_bytes(), Vec::new());
        }
        let Some(parent) = stamp.parent() else {
            return refuse(2, "invalid catalog stamp path");
        };
        // Like the legacy refresh, an unusable stamp, a busy lock or a fresh
        // stamp is a quiet no-op: the attempt is claimed once a day.
        let claim = (|| -> Option<Vec<String>> {
            std::fs::create_dir_all(parent).ok()?;
            let settings = oms_lock::Settings::from_env().ok()?;
            let guard = oms_lock::Lock::for_file(stamp, settings)
                .ok()?
                .try_acquire()
                .ok()?;
            let stale_now = file_older_than(stamp, CATALOG_PERIOD).then(stale)?;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(stamp)
                .ok()?;
            file.set_modified(SystemTime::now()).ok()?;
            drop(guard);
            Some(stale_now)
        })();
        let Some(stale) = claim.filter(|s| !s.is_empty()) else {
            return (0, Vec::new(), Vec::new());
        };
        let providers = stale.join(",");
        let out = oms_command_providers_skills::run_with_source(
            "models",
            &[
                "--refresh".into(),
                "--json".into(),
                "--providers".into(),
                providers.clone().into(),
            ],
            source,
            source,
            interrupted,
        );
        // A failed listing never causes hourly retries; report, do not fail.
        let note = if out.0 == 0 {
            format!("refreshed catalogs: {providers}\n")
        } else {
            format!(
                "catalog refresh incomplete for: {providers} (exit {})\n",
                out.0
            )
        };
        (0, Vec::new(), note.into_bytes())
    }
}

impl oms_command_lifecycle::Services for CollabServices<'_> {
    /// `--connect-services [--notion-data-source ID]` and `--peer-permissions`
    /// run their native producers; options without one are refused.
    /// Preflight validates only (permissions are shown, never applied).
    fn install_extras(
        &self,
        phase: IncomingPhase,
        source: &Path,
        args: &[OsString],
        interrupted: &AtomicBool,
    ) -> Output {
        let extras = match plan_extras(args) {
            Ok(e) => e,
            Err(e) => return refuse(2, e),
        };
        let run = |tool: &str, args: Vec<OsString>| {
            oms_command_providers_skills::run_with_source(tool, &args, source, source, interrupted)
        };
        // The delegate grants apply only when Antigravity is installed, as installed
        // by the legacy installer.
        let grants = |mode: &str| -> Option<Vec<OsString>> {
            (extras.permissions && on_path("agy")).then(|| {
                [mode, "--profile", "delegate", "--allow-toolchains"]
                    .iter()
                    .map(OsString::from)
                    .collect()
            })
        };
        match phase {
            IncomingPhase::Preflight => grants("--print")
                .map_or((0, Vec::new(), Vec::new()), |a| {
                    run("provider-permissions", a)
                }),
            IncomingPhase::Reconcile => {
                let mut combined = (0, Vec::new(), Vec::new());
                let mut stages: Vec<Box<dyn FnOnce() -> Output + '_>> = Vec::new();
                if extras.connect {
                    let mut a: Vec<OsString> = vec!["--required".into()];
                    if let Some(id) = &extras.data_source {
                        a.extend(["--data-source-id".into(), id.into()]);
                    }
                    stages.push(Box::new(move || run("connect-services", a)));
                } else if let Some(id) = &extras.data_source {
                    let a: Vec<OsString> = ["configure", "--data-source-id", id, "--no-validate"]
                        .iter()
                        .map(OsString::from)
                        .collect();
                    stages.push(Box::new(move || {
                        oms_command_journal::run_with_source(&a, source, source, interrupted)
                    }));
                }
                if let Some(a) = grants("--apply") {
                    stages.push(Box::new(move || run("provider-permissions", a)));
                }
                for stage in stages {
                    let (code, stdout, stderr) = stage();
                    combined.1.extend(stdout);
                    combined.2.extend(stderr);
                    if code != 0 {
                        combined.0 = code;
                        break;
                    }
                }
                combined
            }
            _ => refuse(2, "install extras run only in preflight and reconcile"),
        }
    }

    /// Read-only proof that `main` requires the stable CI gate.
    fn hook_protection(&self, source: &Path, interrupted: &AtomicBool) -> Output {
        oms_command_landing::run_with_services(
            "ci-status",
            &["protection".into()],
            source,
            source,
            interrupted,
            &self.control,
        )
    }

    fn apply_project_template(
        &self,
        style: &str,
        destination: &Path,
        source: &Path,
        interrupted: &AtomicBool,
    ) -> Output {
        oms_command_providers_skills::run_with_source(
            "apply-project-template",
            &[style.into(), destination.into()],
            destination,
            source,
            interrupted,
        )
    }

    /// Supplied bytes only; the stdin of this process is never read. Background
    /// work a hook asks for is detached through the staged native binary (and
    /// is not attempted when it is unavailable). Advisory callers
    /// (Antigravity) treat any nonzero result as a diagnostic, so the hook's
    /// own exit code is passed through unchanged.
    fn provider_hook(
        &self,
        name: &str,
        input: &[u8],
        cwd: &Path,
        source: &Path,
        _advisory: bool,
        interrupted: &AtomicBool,
    ) -> Output {
        let host = HookHost {
            native: self.native.clone(),
            detach_worker: self.detach_worker.clone(),
        };
        oms_command_hooks::run_with_services(name, &[], input, cwd, source, interrupted, &host)
    }

    fn prepare_incoming_source(
        &self,
        source: &Path,
        revision: &str,
        interrupted: &AtomicBool,
    ) -> Result<Box<dyn oms_command_lifecycle::IncomingSourceLease + '_>, Output> {
        incoming::prepare(self, source, revision, interrupted)
    }

    /// Preflight proves the incoming revision is already staged and dispatchable
    /// before live checkout mutation. Reconcile, Verify and RefreshTools run that
    /// checkout's own staged `--oms-dispatch` frontdoor to completion with
    /// `OMS_NATIVE_UPDATE_PARENT` in the child's environment only.
    fn incoming_lifecycle(
        &self,
        phase: IncomingPhase,
        source: &Path,
        revision: &str,
        _receipt: &[u8],
        _args: &[OsString],
        interrupted: &AtomicBool,
    ) -> Output {
        if !is_revision(revision) {
            return refuse(2, "incoming revision must be a full lowercase commit id");
        }
        if std::env::var_os("OMS_NATIVE_UPDATE_PARENT").is_some() {
            return refuse(76, "recursive updates are refused");
        }
        let launcher = match self.incoming(source, revision) {
            Ok(l) => l,
            Err(e) => return refuse(2, e),
        };
        let parent = vec![(
            OsString::from("OMS_NATIVE_UPDATE_PARENT"),
            OsString::from(std::process::id().to_string()),
        )];
        if phase == IncomingPhase::Preflight {
            use std::os::unix::fs::PermissionsExt;
            let Some(canonical_source) = std::fs::canonicalize(source).ok().filter(|p| p.is_dir())
            else {
                return refuse(2, "source root is unavailable");
            };
            let (_dir, expected_oms_digest) = match crate::validate_staged_payload(
                &canonical_source,
                std::ffi::OsStr::new(revision),
                false,
            ) {
                Ok(v) => v,
                Err(e) => return refuse(2, e),
            };
            let dir_path = canonical_source.join("target/oms-native").join(revision);
            let mut cur: &Path = &dir_path;
            loop {
                let Ok(meta) = std::fs::symlink_metadata(cur) else {
                    return refuse(2, "invalid native payload ancestor");
                };
                if !meta.file_type().is_dir() || meta.permissions().mode() & 0o002 != 0 {
                    return refuse(2, "unsafe native payload ancestor permissions");
                }
                if cur == canonical_source {
                    break;
                }
                let Some(next) = cur.parent() else {
                    return refuse(2, "invalid native payload ancestor");
                };
                cur = next;
            }
            let sha_path = dir_path.join("oms.sha256");
            let Ok(sha_meta) = std::fs::symlink_metadata(&sha_path) else {
                return refuse(2, "invalid or mismatched oms.sha256");
            };
            if !sha_meta.file_type().is_file()
                || sha_meta.permissions().mode() & 0o002 != 0
                || sha_meta.len() != 65
            {
                return refuse(2, "invalid or mismatched oms.sha256");
            }
            let Ok(sha_bytes) = std::fs::read(&sha_path) else {
                return refuse(2, "invalid or mismatched oms.sha256");
            };
            if sha_bytes != format!("{expected_oms_digest}\n").as_bytes() {
                return refuse(2, "invalid or mismatched oms.sha256");
            }
            let out = run_staged(&launcher, &["list"], &self.source, parent, interrupted);
            if out.0 != 0 {
                return out;
            }
            let text = String::from_utf8_lossy(&out.1);
            if !text.contains("install") || !text.contains("doctor") {
                return refuse(2, "incoming staged binary failed list check");
            }
            return (0, Vec::new(), Vec::new());
        }
        let cwd = &self.source;
        match phase {
            IncomingPhase::Reconcile => {
                let out = run_staged(&launcher, &["install"], cwd, parent, interrupted);
                if out.0 != 0 {
                    return out;
                }
                let text = String::from_utf8_lossy(&out.1).into_owned();
                let path = text
                    .lines()
                    .rev()
                    .find_map(|l| {
                        l.strip_prefix("installed native OMS ")?
                            .split_once("; receipt: ")
                            .map(|(_, path)| path)
                    })
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute());
                let Some(path) = path else {
                    return refuse(2, "incoming install did not name its receipt");
                };
                match read_receipt(&path) {
                    Ok(bytes) => (0, bytes, out.2),
                    Err(e) => refuse(2, e),
                }
            }
            IncomingPhase::Verify => run_staged(&launcher, &["doctor"], cwd, parent, interrupted),
            IncomingPhase::RefreshTools => {
                let profile = capability_receipt()
                    .and_then(|p| std::fs::symlink_metadata(p).ok())
                    .is_some_and(|m| m.file_type().is_file());
                run_staged(&launcher, tools_refresh(profile), cwd, parent, interrupted)
            }
            IncomingPhase::Preflight => unreachable!("handled above"),
        }
    }

    /// The twelve-field maintenance receipt, produced by the native journal,
    /// CI, thread, task, plan, artifact and failure producers. The receipt
    /// mutex is not touched. Each step runs once; a step that did not run is
    /// `skipped`, never zero. The Codex plugin cache has no native refresh
    /// producer here, so `plugin_rc` is `skipped`.
    fn tick_maintenance(&self, repo: &Path, source: &Path, stop: &AtomicBool) -> Output {
        match self.maintenance(repo, source, stop) {
            Ok(row) => (0, serde_json::to_vec(&row).unwrap_or_default(), Vec::new()),
            Err(e) => refuse(2, e),
        }
    }

    fn tick_schedule(
        &self,
        args: &[OsString],
        repo: &Path,
        _source: &Path,
        interrupted: &AtomicBool,
    ) -> Output {
        oms_panel_core::tick::schedule_cli(args, repo, self, self, interrupted)
    }

    /// At most daily: claims `catalog-refresh.stamp` under its lock, then
    /// refreshes installed catalogs older than seven days. `dry_run` only
    /// reports.
    fn refresh_catalogs(&self, dry_run: bool, source: &Path, interrupted: &AtomicBool) -> Output {
        if !dry_run && env_text("OMS_HARNESS_CHILD").is_some_and(|v| v == "1") {
            return refuse(
                2,
                "a harness child cannot refresh parent-owned catalogs; return the request to the parent agent",
            );
        }
        let state = env_text("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env_text("HOME").map(|h| Path::new(&h).join(".local/state")));
        let cache = env_text("OMS_CAPABILITY_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                env_text("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| env_text("HOME").map(|h| Path::new(&h).join(".cache")))
                    .map(|c| c.join("oh-my-setting/capabilities"))
            });
        let (Some(state), Some(cache)) = (state, cache) else {
            return refuse(2, "HOME is required to locate the catalog state");
        };
        let stamp = state.join("oh-my-setting/catalog-refresh.stamp");
        self.catalog_refresh(dry_run, &stamp, &cache, source, interrupted)
    }

    fn garbage_collect(&self, repo: &Path, interrupted: &AtomicBool) -> Output {
        oms_command_state::run_with_host(
            "gc",
            &[
                OsString::from("--repo"),
                repo.as_os_str().to_os_string(),
                OsString::from("--apply"),
            ],
            repo,
            interrupted,
            &mut oms_command_runs::NoServices,
            &self.control,
        )
    }

    /// The marker makes one tick run resume at most one headless autopilot
    /// across every repository swept through this instance.
    fn wake_idle_main(
        &self,
        scheduled_only: bool,
        repo: &Path,
        interrupted: &AtomicBool,
    ) -> Output {
        oms_panel_core::tick::wake_idle_main_json_once(
            scheduled_only,
            repo,
            self,
            self,
            interrupted,
            Some(&self.wake_once),
        )
    }

    fn group_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        Some(crate::services_runtime::resolve_group_membership)
    }
}

fn read_bounded_regular(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() == 0 || meta.len() > limit {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit && !bytes.is_empty()).then_some(bytes)
}

fn read_receipt(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).map_err(|_| "incoming receipt is missing")?;
    if !meta.file_type().is_file() || meta.len() > RECEIPT_LIMIT {
        return Err("incoming receipt is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(RECEIPT_LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|_| "incoming receipt is unreadable")?;
    let row: Value = serde_json::from_slice(&bytes).map_err(|_| "incoming receipt is not JSON")?;
    if row["schema"] != 2 {
        return Err("incoming receipt is not schema 2".into());
    }
    Ok(bytes)
}

impl CollabServices<'_> {
    fn maintenance(&self, repo: &Path, source: &Path, stop: &AtomicBool) -> Result<Value, String> {
        let idle = env_days("OMS_TICK_THREAD_IDLE_DAYS", 7)?;
        let task_idle = env_days("OMS_TICK_TASK_IDLE_DAYS", 7)?;
        let plan_idle_days = env_days("OMS_TICK_PLAN_IDLE_DAYS", 14)?;
        let stale_days = env_days("OMS_TICK_FAILURE_STALE_DAYS", 14)?;
        let retire = !matches!(
            env_text("OMS_TICK_RETIRE").as_deref(),
            Some("0" | "false" | "no" | "off")
        );
        let repo_arg: OsString = repo.into();
        let with_repo = |rest: &[&str]| -> Vec<OsString> {
            let mut v: Vec<OsString> = vec!["--repo".into(), repo_arg.clone()];
            v.extend(rest.iter().map(OsString::from));
            v
        };
        let check = || -> Result<(), String> { self.check(stop) };

        // Journal: local tick, then both syncs; the first failure is the rc.
        check()?;
        let journal_off = matches!(
            env_text("OMS_WORK_JOURNAL").as_deref(),
            Some("0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF")
        );
        let journal_rc = if journal_off {
            json!("skipped")
        } else {
            let journal =
                |a: Vec<OsString>| oms_command_journal::run_with_source(&a, repo, source, stop).0;
            let mut tick: Vec<OsString> = vec!["tick".into()];
            tick.extend(with_repo(&["--local-only"]));
            let mut rc = journal(tick);
            if rc == 0 {
                let mut sync: Vec<OsString> = vec!["sync".into()];
                sync.extend(with_repo(&[]));
                rc = journal(sync.clone());
                if rc == 0 {
                    sync.extend(["--force".into(), "--today".into()]);
                    rc = journal(sync);
                }
            }
            json!(rc)
        };

        // CI: only for a GitHub origin; the failure of a refresh never fails the sweep.
        check()?;
        let github = env_text("OMS_CI_TICK").is_none_or(|v| v != "0") && {
            let (done, _) = supervise(
                Job::new(
                    ["git", "-C"]
                        .iter()
                        .map(OsString::from)
                        .chain([
                            repo_arg.clone(),
                            "remote".into(),
                            "get-url".into(),
                            "origin".into(),
                        ])
                        .collect(),
                    repo,
                    Duration::from_secs(30),
                    64 * 1024,
                ),
                stop,
            )?;
            let url = String::from_utf8_lossy(&done.stdout.bytes).into_owned();
            done.exit.is_some_and(|e| e.shell_code() == 0)
                && (url.contains("github.com:") || url.contains("github.com/"))
        };
        let ci_rc = if github {
            json!(
                oms_command_landing::run_with_services(
                    "ci-status",
                    &["tick".into()],
                    repo,
                    source,
                    stop,
                    &self.control,
                )
                .0
            )
        } else {
            json!("skipped")
        };

        // Idle threads: delegate to thread's locked stale closure (excludes rooms
        // and rechecks CURRENT/activity timestamps under each log lock).
        check()?;
        let cutoff = idle.saturating_mul(86400).to_string();
        let summary = format!("closed by oms tick: idle over {idle}d");
        let close_args: Vec<OsString> = vec![
            "close".into(),
            "--stale".into(),
            "--apply".into(),
            "--stale-ttl".into(),
            cutoff.into(),
            "--repo".into(),
            repo_arg.clone(),
            "--summary".into(),
            summary.into(),
        ];
        let close_out =
            oms_command_peer::run_with_executor("thread", &close_args, repo, source, stop, self);
        if close_out.0 != 0 {
            return Err(format!(
                "stale thread retirement failed (exit {})",
                close_out.0
            ));
        }
        let closed: Vec<String> = String::from_utf8_lossy(&close_out.2)
            .lines()
            .filter_map(|line| line.strip_prefix("thread: closed ").map(str::to_owned))
            .collect();
        check()?;

        // Goal-less idle task packet.
        let (mut tasks_closed, mut plans_retired, mut failures_retired) = (0u64, 0u64, 0u64);
        let task = |args: &[&str]| -> Output {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = oms_command_task::run_with_io(
                &with_repo(args),
                repo,
                stop,
                None,
                &mut self.control.task(),
                &mut std::io::empty(),
                &mut out,
                &mut err,
            );
            (code, out, err)
        };
        check()?;
        let packet = repo.join(".oms/task/current.md");
        let packet_bytes = if retire {
            read_bounded_regular(&packet, RECEIPT_LIMIT)
        } else {
            None
        };
        if let Some(packet_bytes) = packet_bytes
            .as_deref()
            .filter(|b| std::str::from_utf8(b).is_ok_and(goal_is_empty))
        {
            let status = json_of(&task(&["status", "--json"]));
            let idle_active = status.is_some_and(|s| {
                s["present"] == true
                    && s["status"] == "active"
                    && s["last_activity"]
                        .as_str()
                        .is_some_and(|t| aged(t, task_idle))
            });
            if idle_active {
                let reason = format!("closed by oms tick: goal-less packet idle over {task_idle}d");
                let sha = format!("{:x}", Sha256::digest(packet_bytes));
                if task(&["close", "--reason", &reason, "--if-unchanged-sha256", &sha]).0 == 0 {
                    tasks_closed += 1;
                }
            }
        }

        // Idle plan.
        let mut plan_retire_rc = json!("skipped");
        check()?;
        let plan = |args: &[&str]| {
            oms_command_plan::run_with_services(&with_repo(args), repo, stop, &self.control)
        };
        let plan_file = repo.join(".oms/plan/tasks.json");
        let plan_bytes = if retire {
            read_bounded_regular(&plan_file, 16 * 1024 * 1024)
        } else {
            None
        };
        if let Some(plan_bytes) = plan_bytes
            .as_deref()
            .filter(|b| plan_idle(b, plan_idle_days))
        {
            let checked = plan(&["retire", "--check"]);
            if checked.0 != 0 {
                plan_retire_rc = json!(checked.0);
            } else {
                let sha = json_of(&checked).and_then(|v| {
                    let sha = v["plan_sha256"].as_str()?.to_owned();
                    (v["schema"] == 1
                        && v["action"] == "plan-retire-check"
                        && v["active_task_blockers"] == json!([])
                        && v["would_write"] == false
                        && sha.len() == 64
                        && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
                    .then_some(sha)
                });
                let expected = format!("{:x}", Sha256::digest(plan_bytes));
                plan_retire_rc = match sha {
                    None => json!(1),
                    Some(sha) if sha != expected => json!("skipped"),
                    Some(sha) => {
                        let reason = format!(
                            "retired by oms tick: all tasks done and idle over {plan_idle_days}d"
                        );
                        let applied = plan(&[
                            "retire",
                            "--apply",
                            "--expected-plan-sha256",
                            &sha,
                            "--disposition",
                            "superseded",
                            "--reason",
                            &reason,
                        ]);
                        if applied.0 == 0 {
                            plans_retired += 1;
                        }
                        json!(applied.0)
                    }
                };
            }
        }

        // Artifact failures: superseded, then recovered.
        let artifacts = |action: &str, kind: &str| -> (Value, u64) {
            let out = oms_command_telemetry::run(&with_repo(&[action]), repo, stop);
            if out.0 != 0 {
                return (json!(out.0), 0);
            }
            resolved_count(&out.1, kind).map_or((json!(1), 0), |n| (json!(0), n))
        };
        check()?;
        let (artifact_supersede_rc, artifacts_superseded) =
            artifacts("resolve-superseded", "superseded");
        let (artifact_resolve_rc, artifacts_resolved) = artifacts("resolve-recovered", "recovered");

        // Stale one-shot failures.
        check()?;
        let ledger = |args: &[&str]| {
            oms_command_state::run_with_host(
                "fail-ledger",
                &with_repo(args),
                repo,
                stop,
                &mut oms_command_runs::NoServices,
                &self.control,
            )
        };
        if retire
            && let Some(list) = json_of(&ledger(&["list", "--unresolved", "--json"]))
                .filter(|v| v["schema"] == 1 && v["failures"].is_array())
        {
            for failure in list["failures"].as_array().into_iter().flatten() {
                check()?;
                let fingerprint = failure["fingerprint"].as_str().unwrap_or_default();
                if failure["attention"] != "stale"
                    || fingerprint.is_empty()
                    || fingerprint.contains(['\n', '\r'])
                    || !failure["ts"].as_str().is_some_and(|t| aged(t, stale_days))
                {
                    continue;
                }
                let how = format!(
                    "retired by oms tick: failures on a single commit older than {stale_days}d with no recurrence on another commit"
                );
                if ledger(&["resolve", "--fingerprint", fingerprint, "--how", &how]).0 == 0 {
                    failures_retired += 1;
                }
            }
        }

        Ok(json!({
            "journal_rc": journal_rc,
            "ci_rc": ci_rc,
            "plugin_rc": "skipped",
            "plan_retire_rc": plan_retire_rc,
            "artifact_resolve_rc": artifact_resolve_rc,
            "artifact_supersede_rc": artifact_supersede_rc,
            "threads_closed": closed,
            "tasks_closed": tasks_closed,
            "plans_retired": plans_retired,
            "artifacts_resolved": artifacts_resolved,
            "artifacts_superseded": artifacts_superseded,
            "failures_retired": failures_retired,
        }))
    }
}

impl CommandExecutor for CollabServices<'_> {
    /// Every verb runs in the staged native binary under supervision, so the
    /// timeout and cancellation are enforced on a real process group.
    fn execute(&self, request: &CommandRequest<'_>) -> Result<Captured, String> {
        let mut argv: Vec<OsString> = vec!["oms".into()];
        argv.extend(request.args.iter().cloned());
        let argv = self.resolve_argv(argv).map_err(|e| format!("error: {e}"))?;
        let (done, _) = supervise(
            Job::new(argv, request.cwd, request.timeout, MCP_OUTPUT_LIMIT),
            request.interrupted,
        )
        .map_err(|e| format!("error: {e}"))?;
        if let Some(stop) = done.stop {
            return Err(format!(
                "error: {}",
                stop_message(stop, done.supervision_error.as_deref(), MCP_OUTPUT_LIMIT)
            ));
        }
        if !done.cleanup_complete() || !done.output_complete() {
            return Err("error: native supervision incomplete".into());
        }
        let stderr = done.stderr.map_or_else(Vec::new, |s| s.bytes);
        Ok(Captured::from_output(
            done.exit.map_or(1, |e| e.shell_code()),
            &done.stdout.bytes,
            &stderr,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// Unique per process and per call, removed on drop even when a test fails.
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "oms-collab-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).expect("create fixture");
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn source() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }
    fn sh(script: &str) -> Vec<OsString> {
        ["/bin/sh", "-c", script, "sh"]
            .iter()
            .map(OsString::from)
            .collect()
    }
    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }
    /// The detached process must not be (or leave) a child of this process.
    fn assert_no_direct_child(pid: u32) {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        // Fields after the parenthesised name: state, then ppid.
        let ppid = stat
            .rsplit(')')
            .next()
            .and_then(|rest| rest.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u32>().ok());
        if cfg!(target_os = "linux") && !stat.is_empty() {
            assert_ne!(ppid, Some(std::process::id()), "{stat}");
        }
    }
    fn request(argv: &[&str], cwd: &Path) -> ExecutionRequest {
        ExecutionRequest::new(argv.iter().map(OsString::from).collect(), cwd)
    }

    #[test]
    fn receipt_host_refuses_missing_and_invalid_sources() {
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).expect("source is available");
        let root = Fixture::new();
        assert_eq!(
            host.load_contract(root.path()).unwrap_err(),
            ContractError::Missing
        );
        let plan = root.path().join(".oms/plan");
        std::fs::create_dir_all(&plan).expect("create plan");
        std::fs::write(plan.join("autopilot-run.json"), b"{").expect("write invalid receipt");
        assert!(matches!(
            host.load_contract(root.path()),
            Err(ContractError::Invalid(_))
        ));
    }

    #[test]
    fn room_delivery_adapter_preserves_uncertainty_and_requires_wakeup_authority() {
        let producer = json!({"status":"submitted", "submitted":true,
            "delivery_unknown":true, "persisted":false, "turn_id":"turn-fixture",
            "desktop_notification_observed":false});
        let receipt = room_delivery_receipt(producer).unwrap();
        assert_eq!(receipt.status, "submitted");
        assert!(receipt.delivery_unknown);
        assert_eq!(receipt.turn_id.as_deref(), Some("turn-fixture"));
        assert_eq!(receipt.fields["persisted"].as_bool(), Some(false));
        assert_eq!(
            receipt.fields["desktop_notification_observed"].as_bool(),
            Some(false)
        );
        assert!(room_delivery_receipt(json!({"status":"persisted"})).is_err());
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).unwrap();
        let root = Fixture::new();
        // False wake authority returns before native registry/socket discovery.
        let receipt = oms_command_room::Services::deliver(
            &host,
            oms_command_room::DeliveryRequest {
                repository: root.path(),
                app: oms_command_room::App::Claude,
                text: "test receipt",
                consumer: None,
                allow_wakeup: false,
            },
        )
        .unwrap();
        assert!(["wakeup_required", "disabled"].contains(&receipt.status.as_str()));
        assert!(!receipt.delivery_unknown);
        assert_eq!(receipt.fields["submitted"].as_bool(), Some(false));
        stop.store(true, Ordering::Release);
        assert!(PanelServices::deliver_result(&host, root.path(), "cancelled", &stop).is_err());
        assert!(
            oms_command_room::Services::deliver(
                &host,
                oms_command_room::DeliveryRequest {
                    repository: root.path(),
                    app: oms_command_room::App::Codex,
                    text: "cancelled",
                    consumer: None,
                    allow_wakeup: false,
                }
            )
            .is_err()
        );
    }

    #[test]
    fn room_operations_adapter_holds_the_producer_lock_and_checks_the_snapshot() {
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).unwrap();
        let root = Fixture::new();
        let read = oms_command_room::Services::operations(&host, root.path()).unwrap();
        assert!(read.attempts().is_empty());
        read.check().unwrap();
        let (ready, received) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                let _guard = oms_command_operations::lock::FileLock::acquire(
                    &oms_command_operations::event_path(root.path()),
                    &stop,
                )
                .unwrap();
                ready.send(()).unwrap();
            });
            assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
            drop(read);
            received.recv_timeout(Duration::from_secs(5)).unwrap();
            writer.join().unwrap();
        });
        let first = oms_command_operations::create_attempt(
            root.path(),
            &oms_command_operations::StartRequest {
                tool: "peer-delegate".into(),
                provider: "codex".into(),
                task_id: "first".into(),
                ..Default::default()
            },
            &stop,
        )
        .unwrap();
        let second = oms_command_operations::create_attempt(
            root.path(),
            &oms_command_operations::StartRequest {
                tool: "peer-delegate".into(),
                provider: "claude".into(),
                task_id: "second".into(),
                ..Default::default()
            },
            &stop,
        )
        .unwrap();
        let read = oms_command_room::Services::operations(&host, root.path()).unwrap();
        assert_eq!(
            read.attempts()
                .iter()
                .map(|a| a.attempt_id.as_str())
                .collect::<Vec<_>>(),
            [first.as_str(), second.as_str()]
        );
        assert!(!read.attempts()[0].terminal);
        assert_eq!(read.attempts()[1].provider, "claude");
        stop.store(true, Ordering::Release);
        assert!(read.check().is_err());
    }

    #[test]
    fn publication_adapter_never_synthesizes_a_missing_receipt() {
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).unwrap();
        let root = Fixture::new();
        assert!(
            PanelServices::publication_receipt(
                &host,
                root.path(),
                root.path(),
                &"a".repeat(40),
                "",
                &stop
            )
            .unwrap_err()
            .message
            .contains("digest is required")
        );
        assert!(
            PanelServices::publication_receipt(
                &host,
                root.path(),
                root.path(),
                &"a".repeat(40),
                &"b".repeat(64),
                &stop
            )
            .is_err()
        );
    }

    #[test]
    fn launch_refuses_before_any_side_effect_without_a_staged_launcher() {
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).expect("source is available");
        let repo = Fixture::new();
        let log = repo.path().join(".oms/tick/autopilot-x.log");
        assert!(host.executable().is_err());
        let err = host
            .launch(&strings(&["oms", "autopilot"]), &log, repo.path())
            .unwrap_err();
        assert!(err.contains("launcher unavailable"), "{err}");
        assert!(!repo.path().join(".oms").exists());
    }

    #[test]
    fn launch_requires_oms_argv_and_a_real_tick_directory() {
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source())
            .expect("source is available")
            .with_native(sh("exit 0"));
        let repo = Fixture::new();
        let log = repo.path().join(".oms/tick/autopilot-x.log");
        for argv in [strings(&[]), strings(&["autopilot"]), strings(&["sh"])] {
            assert!(host.launch(&argv, &log, repo.path()).is_err());
        }
        assert!(!repo.path().join(".oms").exists());
        // A log outside `.oms/tick` is refused.
        let elsewhere = repo.path().join("autopilot-x.log");
        assert!(
            host.launch(&strings(&["oms", "x"]), &elsewhere, repo.path())
                .is_err()
        );
        // A symlinked `.oms/tick` is refused, and nothing is written through it.
        let outside = Fixture::new();
        std::fs::create_dir(repo.path().join(".oms")).unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.path().join(".oms/tick")).unwrap();
        assert!(
            host.launch(&strings(&["oms", "x"]), &log, repo.path())
                .is_err()
        );
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn launch_detaches_with_the_wake_marker_and_a_private_log() {
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source())
            .expect("source is available")
            .with_native(sh(
                "printf '%s|%s' \"$OMS_AUTOPILOT_WAKE_RESUME\" \"$*\"; sleep 2",
            ));
        let repo = Fixture::new();
        let log = repo.path().join(".oms/tick/autopilot-x.log");
        let pid = host
            .launch(&strings(&["oms", "autopilot", "run"]), &log, repo.path())
            .expect("launch");
        assert!(pid > 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        let text = loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if !text.is_empty() || Instant::now() > deadline {
                break text;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(text, "1|autopilot run");
        assert_no_direct_child(pid);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn peer_executor_validates_bounds_and_supervises() {
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).expect("source is available");
        let dir = Fixture::new();
        let run = |r: &ExecutionRequest| PeerExecutor::execute(&host, r, &stop);

        let mut ok = request(
            &["/bin/sh", "-c", "printf out; printf err >&2; exit 3"],
            dir.path(),
        );
        let done = run(&ok).expect("runs");
        assert_eq!(
            (done.code, done.stdout, done.stderr),
            (3, b"out".to_vec(), b"err".to_vec())
        );
        assert!(done.cleanup_complete && done.output_complete && !done.limited);

        ok.argv = vec![];
        assert!(run(&ok).is_err());
        ok.argv = vec![OsString::new()];
        assert!(run(&ok).is_err());
        ok.argv = vec!["x".into(); MAX_ARGV + 1];
        assert!(run(&ok).is_err());
        let mut edge = request(&["true"], dir.path());
        edge.argv.extend((1..MAX_ARGV).map(|_| OsString::from("a")));
        assert_eq!(run(&edge).expect("256 args").code, 0);
        let mut limits = request(&["true"], dir.path());
        limits.timeout = Duration::ZERO;
        assert!(run(&limits).is_err());
        limits.timeout = MAX_WALL + Duration::from_secs(1);
        assert!(run(&limits).is_err());
        limits.timeout = MAX_WALL;
        limits.output_limit = 0;
        assert!(run(&limits).is_err());
        limits.output_limit = MAX_OUTPUT + 1;
        assert!(run(&limits).is_err());
        let missing = request(&["true"], &dir.path().join("absent"));
        assert!(run(&missing).is_err());

        let mut env = request(
            &["/bin/sh", "-c", "printf '%s%s' \"$A\" \"${B-unset}\""],
            dir.path(),
        );
        env.environment = vec![("A".into(), "1".into())];
        env.remove_environment = vec!["B".into()];
        assert_eq!(run(&env).unwrap().stdout, b"1unset");

        let mut slow = request(&["/bin/sh", "-c", "sleep 30"], dir.path());
        slow.timeout = Duration::from_millis(200);
        let done = run(&slow).expect("deadline stop");
        assert_ne!(done.code, 0);

        let mut big = request(&["/bin/sh", "-c", "head -c 4096 /dev/zero"], dir.path());
        big.output_limit = 16;
        assert!(run(&big).expect("limit stop").limited);

        let cancelled = AtomicBool::new(true);
        let err =
            PeerExecutor::execute(&host, &request(&["true"], dir.path()), &cancelled).unwrap_err();
        assert_eq!(err.code, 130);

        struct ExpiredChannel;
        impl OwnedDuplexConsumer for ExpiredChannel {
            fn check(&self) -> oms_command_peer::Result<()> {
                Err(oms_command_peer::Error::new(124, "expired channel"))
            }
            fn started(
                &mut self,
                _: &mut Reactor,
                _: oms_supervisor::process::JobId,
            ) -> oms_command_peer::Result<()> {
                panic!("expired channel started a child")
            }
            fn event(
                &mut self,
                _: &mut Reactor,
                _: oms_supervisor::process::JobId,
                _: oms_supervisor::process::OutputEvent,
            ) -> oms_command_peer::Result<()> {
                unreachable!()
            }
        }
        let marker = dir.path().join("expired-channel-started");
        let mut argv = sh("printf started > \"$1\"");
        argv.push(marker.as_os_str().to_owned());
        assert!(
            supervise(
                Job {
                    channel: Some(&mut ExpiredChannel),
                    ..Job::new(argv, dir.path(), Duration::from_secs(1), 1024)
                },
                &stop,
            )
            .is_err()
        );
        assert!(
            !marker.exists(),
            "prestart expiry must have no child effect"
        );
    }

    #[test]
    fn peer_executor_runs_oms_only_through_the_staged_launcher() {
        let stop = Arc::new(AtomicBool::new(false));
        let dir = Fixture::new();
        let bare = CollabServices::new(&stop, source()).expect("source is available");
        let err =
            PeerExecutor::execute(&bare, &request(&["oms", "x"], dir.path()), &stop).unwrap_err();
        assert!(err.message.contains("launcher unavailable"), "{err}");
        let host = bare.with_native(sh("printf '%s' \"$*\""));
        let done = PeerExecutor::execute(&host, &request(&["oms", "a", "b"], dir.path()), &stop)
            .expect("staged");
        assert_eq!(done.stdout, b"a b");
    }

    #[test]
    fn command_dispatches_in_process_and_context_uses_the_staged_binary() {
        let stop = Arc::new(AtomicBool::new(false));
        let dir = Fixture::new();
        let host = CollabServices::new(&stop, source())
            .expect("source is available")
            .with_native(sh("printf '%s' \"$*\""));
        let help = [OsString::from("--help")];
        for command in [
            NativeCommand::Telemetry,
            NativeCommand::Failure,
            NativeCommand::Room,
            NativeCommand::Events,
            NativeCommand::Task,
        ] {
            let out =
                PanelServices::command(&host, command, dir.path(), &help, &stop).expect("dispatch");
            assert!(out.cleanup_complete && out.output_complete, "{command:?}");
            assert_eq!(out.code, 0, "{command:?}: {:?}", out.stderr);
            assert!(!out.stdout.is_empty(), "{command:?}");
        }
        // Without deltas the in-process route answers (the staged echo would not).
        let plain = request(&["--help"], dir.path());
        let out = PanelServices::command_context(&host, NativeCommand::Telemetry, &plain, &stop)
            .expect("plain");
        assert!(out.stdout.len() > "telemetry".len());
        // With deltas the verb runs in the staged binary with `--repo`.
        let mut delta = request(&["x"], dir.path());
        delta.environment = vec![("K".into(), "V".into())];
        let out = PanelServices::command_context(&host, NativeCommand::Failure, &delta, &stop)
            .expect("staged");
        let want = format!("runtime --repo {} failure x", dir.path().display());
        assert_eq!(String::from_utf8_lossy(&out.stdout), want);

        let cancelled = AtomicBool::new(true);
        let err =
            PanelServices::command(&host, NativeCommand::Failure, dir.path(), &help, &cancelled)
                .unwrap_err();
        assert_eq!(err.code, 130);
        // No staged launcher: a delta is refused rather than run unstaged.
        let bare = CollabServices::new(&stop, source()).expect("source is available");
        assert!(
            PanelServices::command_context(&bare, NativeCommand::Failure, &delta, &stop).is_err()
        );
    }

    #[test]
    fn team_view_uses_selected_state_instead_of_ambient_repository() {
        const CHILD: &str = "OMS_TEST_TEAM_VIEW_SELECTED_ROOTS";
        if std::env::var_os(CHILD).is_none() {
            let execution = Fixture::new();
            let other = Fixture::new();
            for repo in [execution.path(), other.path()] {
                let out = std::process::Command::new("git")
                    .args(["init", "-q"])
                    .arg(repo)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{:?}", out.stderr);
            }
            std::fs::create_dir_all(other.path().join(".oms/plan")).unwrap();
            std::fs::write(other.path().join(".oms/plan/tasks.json"), b"invalid").unwrap();
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "services_collab::tests::team_view_uses_selected_state_instead_of_ambient_repository",
                    "--nocapture",
                ])
                .current_dir(execution.path())
                .env(CHILD, "1")
                .env("OMS_STATE_REPO", other.path())
                .env("OMS_HARNESS_CHILD", "1")
                .env("OMS_HARNESS_DELEGATE_DEPTH", "7")
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
        let execution = std::env::current_dir().unwrap().canonicalize().unwrap();
        let other = PathBuf::from(std::env::var_os("OMS_STATE_REPO").unwrap())
            .canonicalize()
            .unwrap();
        assert_ne!(execution, other);
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).unwrap();
        // The real ambient Plan resolver sees the invalid other repository.
        let ambient = PanelServices::command(
            &host,
            NativeCommand::Plan,
            &execution,
            &[
                "team-list".into(),
                "--json".into(),
                "--repo".into(),
                execution.as_os_str().into(),
            ],
            &stop,
        )
        .unwrap();
        assert_ne!(ambient.code, 0);
        let before = std::fs::read(other.join(".oms/plan/tasks.json")).unwrap();
        let report = oms_panel_core::teams::read_at(&execution, &execution, &host, &stop).unwrap();
        assert_eq!(report["sources"]["teams"]["status"], "available");
        assert_eq!(report["roots"], json!([]));
        assert!(
            !execution.join(".oms").exists(),
            "view must not create state"
        );
        assert_eq!(
            std::fs::read(other.join(".oms/plan/tasks.json")).unwrap(),
            before
        );

        // A selected shared-state root also stays distinct from execution.
        std::fs::write(other.join(".oms/plan/tasks.json"), b"{\"tasks\":{}}").unwrap();
        std::fs::create_dir_all(execution.join(".oms/plan")).unwrap();
        std::fs::write(execution.join(".oms/plan/tasks.json"), b"invalid").unwrap();
        let report = oms_panel_core::teams::read_at(&execution, &other, &host, &stop).unwrap();
        assert_eq!(report["sources"]["teams"]["status"], "available");
        assert_eq!(report["roots"], json!([]));
        assert_eq!(std::env::var("OMS_HARNESS_CHILD").unwrap(), "1");
        assert_eq!(std::env::var("OMS_HARNESS_DELEGATE_DEPTH").unwrap(), "7");
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            PanelServices::team_contracts(&host, &execution, &other, &cancelled)
                .unwrap_err()
                .code,
            130
        );
    }

    #[test]
    fn peer_process_executor_maps_stops_like_the_native_executor() {
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).expect("source is available");
        let dir = Fixture::new();
        let invoke = |script: &str, timeout: Duration| {
            PeerProcessExecutor::execute(
                &host,
                Invocation {
                    argv: sh(script),
                    cwd: dir.path().to_owned(),
                    input: Some(b"in".to_vec()),
                    environment: Vec::new(),
                    remove_environment: Vec::new(),
                    timeout,
                    max_bytes: 1024,
                    merge: false,
                    launch_fence: None,
                    physical_call: Some(
                        oms_supervisor::process::PhysicalCall::new(&format!(
                            "call_fixture_{}",
                            script.len()
                        ))
                        .unwrap(),
                    ),
                },
                &stop,
            )
        };
        let done = invoke("cat; exit 4", Duration::from_secs(10)).expect("runs");
        assert_eq!(
            (done.code, done.stdout.as_slice(), done.complete),
            (4, &b"in"[..], true)
        );
        let cleanup = done.owned_cleanup.as_ref().unwrap();
        assert_eq!(cleanup.call().id(), "call_fixture_11");
        assert_eq!(cleanup.scope(), Scope::ProcessGroup);
        let done = invoke("sleep 30", Duration::from_millis(200)).expect("deadline");
        assert_eq!((done.code, done.complete), (124, false));
        assert_eq!(done.cleanup_complete, Some(true));
        assert_eq!(
            done.owned_cleanup.as_ref().unwrap().call().id(),
            "call_fixture_8"
        );
        assert!(invoke("true", Duration::ZERO).is_err());
    }

    #[test]
    fn lifecycle_services_wire_native_producers() {
        use oms_command_lifecycle::Services as Lifecycle;
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).expect("source is available");
        let repo = Fixture::new();

        let out = host.provider_hook("skill-router", b"{}", repo.path(), source(), true, &stop);
        assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        assert!(!repo.path().join(".oms").exists());
        let unknown = host.provider_hook("not-a-hook", b"", repo.path(), source(), true, &stop);
        assert_eq!(unknown.0, 75);

        let out = host.tick_schedule(
            &[OsString::from("schedule"), "--help".into()],
            repo.path(),
            source(),
            &stop,
        );
        assert_eq!(out.0, 0);
        assert!(String::from_utf8_lossy(&out.1).contains("autopilot"));
        let out = host.tick_schedule(&[OsString::from("schedule")], repo.path(), source(), &stop);
        assert_eq!(out.0, 2, "an action is required");

        let out = host.wake_idle_main(true, repo.path(), &stop);
        assert!(
            out.0 == 0 || out.0 == 2,
            "{}",
            String::from_utf8_lossy(&out.2)
        );
        assert!(host.garbage_collect(repo.path(), &stop).0 != 130);

        let cancelled = AtomicBool::new(true);
        assert_eq!(host.wake_idle_main(false, repo.path(), &cancelled).0, 130);
        assert_eq!(host.garbage_collect(repo.path(), &cancelled).0, 130);
    }

    #[test]
    fn mcp_executor_runs_staged_native_and_honors_timeout_and_cancel() {
        let stop = Arc::new(AtomicBool::new(false));
        let dir = Fixture::new();
        let flag = AtomicBool::new(false);
        let bare = CollabServices::new(&stop, source()).expect("source is available");
        let args = [OsString::from("journal"), "--x".into()];
        let call = |host: &CollabServices<'_>, args: &[OsString], timeout, flag: &AtomicBool| {
            CommandExecutor::execute(
                host,
                &CommandRequest {
                    args,
                    cwd: dir.path(),
                    timeout,
                    interrupted: flag,
                },
            )
        };
        let err = call(&bare, &args, Duration::from_secs(5), &flag)
            .err()
            .expect("refused");
        assert!(err.contains("launcher unavailable"), "{err}");

        let host = bare.with_native(sh("printf '%s' \"$*\"; printf bad >&2; exit 3"));
        let ok = call(&host, &args, Duration::from_secs(10), &flag).expect("runs");
        assert_eq!(ok.code(), 3);
        assert_eq!(ok.stdout(), "journal --x");
        assert_eq!(ok.stderr(), "bad");

        let slow = host.with_native(sh("sleep 30"));
        let err = call(&slow, &args, Duration::from_millis(200), &flag)
            .err()
            .expect("refused");
        assert!(err.contains("timed out"), "{err}");
        assert!(call(&slow, &args, Duration::ZERO, &flag).is_err());
        let cancelled = AtomicBool::new(true);
        assert!(
            call(&slow, &args, Duration::from_secs(5), &cancelled)
                .err()
                .expect("refused")
                .contains("interrupted")
        );
    }

    #[test]
    fn staged_prefix_follows_each_verbs_parser() {
        let cwd = Path::new("/repo");
        let shape = |c| -> Vec<String> {
            staged_prefix(c, cwd)
                .iter()
                .map(|v| v.to_string_lossy().into_owned())
                .collect()
        };
        // No `--repo`: models and cleanup reject it, peer-review keeps subcommands first.
        for (command, verb) in [
            (NativeCommand::ProviderCatalog, "models"),
            (NativeCommand::Cleanup, "cleanup"),
            (NativeCommand::Review, "peer-review"),
        ] {
            assert_eq!(shape(command), [verb], "{command:?}");
        }
        assert_eq!(
            shape(NativeCommand::Failure),
            ["runtime", "--repo", "/repo", "failure"]
        );
        // The bundle verb is the shared `agent-events`; its show action follows.
        assert_eq!(
            shape(NativeCommand::Bundle),
            ["agent-events", "--repo", "/repo"]
        );
        assert_eq!(
            shape(NativeCommand::PeerAsk),
            ["peer-ask", "--repo", "/repo"]
        );
        assert_eq!(
            shape(NativeCommand::Plan),
            ["agent-plan", "--repo", "/repo"]
        );
    }

    #[test]
    fn command_context_deltas_keep_per_verb_argv_in_the_staged_binary() {
        let stop = Arc::new(AtomicBool::new(false));
        let dir = Fixture::new();
        let host = CollabServices::new(&stop, source())
            .expect("source is available")
            .with_native(sh("printf '%s' \"$*\""));
        let run = |command, argv: &[&str]| {
            let mut r = request(argv, dir.path());
            r.environment = vec![("K".into(), "V".into())];
            let out = PanelServices::command_context(&host, command, &r, &stop).expect("staged");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let cwd = dir.path().display();
        assert_eq!(
            run(NativeCommand::ProviderCatalog, &["--providers", "codex"]),
            "models --providers codex"
        );
        assert_eq!(
            run(NativeCommand::Cleanup, &["--dry-run"]),
            "cleanup --dry-run"
        );
        assert_eq!(
            run(NativeCommand::Review, &["verdicts", "--json"]),
            "peer-review verdicts --json"
        );
        assert_eq!(
            run(NativeCommand::Bundle, &["show", "--bundle"]),
            format!("agent-events --repo {cwd} show --bundle")
        );
    }

    #[test]
    fn a_missing_launcher_refuses_an_approved_resume_before_anything_is_recorded() {
        let stop = Arc::new(AtomicBool::new(false));
        let bare = CollabServices::new(&stop, source()).expect("source is available");
        let staged = CollabServices::new(&stop, source())
            .expect("source is available")
            .with_native(sh("exit 0"));
        let reason = bare.launcher_refusal(String::new(), true);
        assert!(reason.contains("launcher unavailable"), "{reason}");
        // A refusal that already exists, or a continuation that is not an approved run,
        // keeps its own wording; a staged launcher never adds one.
        assert_eq!(bare.launcher_refusal("parked".into(), true), "parked");
        assert_eq!(bare.launcher_refusal(String::new(), false), "");
        assert_eq!(staged.launcher_refusal(String::new(), true), "");
    }

    #[test]
    fn the_resume_marker_is_per_instance_and_removed_on_drop() {
        let stop = Arc::new(AtomicBool::new(false));
        let (first, second) = (
            CollabServices::new(&stop, source()).expect("source is available"),
            CollabServices::new(&stop, source()).expect("source is available"),
        );
        assert_ne!(first.wake_once, second.wake_once);
        assert!(!first.wake_once.exists());
        // The tick claims the run with create_new: one claimant wins per instance.
        let claim = |p: &Path| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(p)
                .is_ok()
        };
        let marker = first.wake_once.clone();
        assert!(claim(&marker));
        assert!(!claim(&marker));
        assert!(claim(&second.wake_once));
        drop(first);
        assert!(!marker.exists(), "removed with the instance");
        // Two repositories swept through one instance both complete without error.
        let (a, b) = (Fixture::new(), Fixture::new());
        for repo in [&a, &b] {
            let out =
                oms_command_lifecycle::Services::wake_idle_main(&second, false, repo.path(), &stop);
            assert!(
                out.0 == 0 || out.0 == 2,
                "{}",
                String::from_utf8_lossy(&out.2)
            );
        }
    }

    #[test]
    fn detach_worker_child_entry() {
        if std::env::var_os(DETACH_TEST_CHILD_ENV).is_some() {
            std::process::exit(detach_worker_main());
        }
    }

    #[test]
    fn launch_leaves_no_direct_child_and_the_helper_is_reaped() {
        use std::os::unix::fs::PermissionsExt;
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source())
            .expect("source is available")
            .with_native(sh("sleep 2"));
        let repo = Fixture::new();
        let log = repo.path().join(".oms/tick/autopilot-x.log");
        let pid = host
            .launch(&strings(&["oms", "x"]), &log, repo.path())
            .expect("launch");
        assert_no_direct_child(pid);
        // A fast-exiting command still succeeds and leaves no direct child.
        let helper = spawn_detached(
            &host.detach_worker,
            vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            repo.path(),
            &[],
            std::process::Stdio::null(),
        )
        .expect("detach");
        assert!(helper > 0);
        assert_no_direct_child(helper);
        // Synchronous execve failures (missing binary / non-executable file)
        // must fail launch cleanly rather than returning Ok(pid).
        let missing_bin = repo.path().join("no-such-binary");
        assert!(
            spawn_detached(
                &host.detach_worker,
                vec![missing_bin.clone().into()],
                repo.path(),
                &[],
                std::process::Stdio::null(),
            )
            .is_err()
        );
        let non_exec = repo.path().join("non-exec-oms");
        std::fs::write(&non_exec, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            spawn_detached(
                &host.detach_worker,
                vec![non_exec.clone().into()],
                repo.path(),
                &[],
                std::process::Stdio::null(),
            )
            .is_err()
        );
        let bad_host = CollabServices::new(&stop, source())
            .expect("source is available")
            .with_native(vec![non_exec.into()]);
        let failed_log = repo.path().join(".oms/tick/autopilot-failed.log");
        assert!(
            bad_host
                .launch(&strings(&["oms", "x"]), &failed_log, repo.path())
                .is_err()
        );
        assert!(!failed_log.exists());
    }

    #[test]
    fn stop_reasons_have_distinct_messages() {
        let messages = [
            stop_message(StopReason::Cancelled, None, 8),
            stop_message(StopReason::Signal(15), None, 8),
            stop_message(StopReason::Deadline, None, 8),
            stop_message(StopReason::OutputLimit, None, 8),
            stop_message(StopReason::ConsumerCutoff, None, 8),
            stop_message(StopReason::SupervisorError, Some("pipe closed"), 8),
        ];
        let unique: std::collections::BTreeSet<_> = messages.iter().collect();
        assert_eq!(unique.len(), messages.len(), "{messages:?}");
        assert!(messages[3].contains("8-byte"));
        assert!(messages[5].contains("pipe closed"));
        assert!(stop_message(StopReason::SupervisorError, None, 8).contains("unspecified"));
    }

    #[test]
    fn mcp_executor_reports_output_limit_distinctly() {
        let stop = Arc::new(AtomicBool::new(false));
        let dir = Fixture::new();
        let flag = AtomicBool::new(false);
        let host = CollabServices::new(&stop, source())
            .expect("source is available")
            .with_native(sh("head -c 9000000 /dev/zero"));
        let args = [OsString::from("journal")];
        let err = CommandExecutor::execute(
            &host,
            &CommandRequest {
                args: &args,
                cwd: dir.path(),
                timeout: Duration::from_secs(30),
                interrupted: &flag,
            },
        )
        .err()
        .expect("limit refused");
        assert!(
            err.contains("exceeded") && !err.contains("timed out"),
            "{err}"
        );
    }

    #[test]
    fn hook_host_detaches_only_through_the_staged_launcher() {
        let dir = Fixture::new();
        let detach_worker = default_detach_worker(source()).expect("test detach worker");
        let none = HookHost {
            native: vec![],
            detach_worker: detach_worker.clone(),
        };
        assert!(!none.supports_detach());
        assert!(none.detach_command("journal", &[], dir.path()).is_err());
        let host = HookHost {
            native: sh(
                "printf '%s' \"$*\" > detached.txt.tmp && mv -f detached.txt.tmp detached.txt; sleep 1",
            ),
            detach_worker,
        };
        assert!(host.supports_detach());
        for bad in ["", "../x", "Journal", "-x", "a b", "a/b"] {
            assert!(
                host.detach_command(bad, &[], dir.path()).is_err(),
                "{bad:?}"
            );
        }
        assert!(!dir.path().join("detached.txt").exists());
        host.detach_command("journal", &[OsString::from("sync")], dir.path())
            .expect("detach");
        let out = dir.path().join("detached.txt");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "journal sync");
    }

    #[test]
    fn provider_hook_uses_the_detach_adapter_and_never_reads_stdin() {
        use oms_command_lifecycle::Services as Lifecycle;
        let stop = Arc::new(AtomicBool::new(false));
        let repo = Fixture::new();
        let staged = CollabServices::new(&stop, source())
            .expect("source is available")
            .with_native(sh("exit 0"));
        let out = staged.provider_hook("skill-router", b"{}", repo.path(), source(), true, &stop);
        assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        let oversized = vec![b' '; 4 * 1024 * 1024];
        let out = staged.provider_hook(
            "skill-router",
            &oversized,
            repo.path(),
            source(),
            false,
            &stop,
        );
        assert_ne!(out.0, 75, "known hook, bounded input");
    }

    #[test]
    fn extras_are_validated_and_unsupported_options_refused() {
        let args = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            plan_extras(&args(&["--connect-services", "--peer-permissions"])).unwrap(),
            Extras {
                connect: true,
                permissions: true,
                data_source: None
            }
        );
        assert_eq!(
            plan_extras(&args(&[
                "--connect-services",
                "--notion-data-source=abc-1_2"
            ]))
            .unwrap()
            .data_source
            .as_deref(),
            Some("abc-1_2")
        );
        assert!(!plan_extras(&args(&["--yes"])).unwrap().connect);
        for bad in [
            &["--star"][..],
            &["--connect-services", "--notion-data-source"],
            &["--connect-services", "--notion-data-source="],
            &["--connect-services", "--notion-data-source", "a b"],
        ] {
            assert!(plan_extras(&args(bad)).is_err(), "{bad:?}");
        }
        let long = "a".repeat(65);
        assert!(
            plan_extras(&args(&[
                "--connect-services",
                &format!("--notion-data-source={long}")
            ]))
            .is_err()
        );
        let ok64 = "a".repeat(64);
        assert!(
            plan_extras(&args(&[
                "--connect-services",
                &format!("--notion-data-source={ok64}")
            ]))
            .is_ok()
        );
    }

    #[test]
    fn install_extras_preflight_never_mutates_and_refuses_bad_options() {
        use oms_command_lifecycle::Services as Lifecycle;
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).expect("source is available");
        let dir = Fixture::new();
        let a = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let pre = |args: &[&str]| {
            host.install_extras(IncomingPhase::Preflight, dir.path(), &a(args), &stop)
        };
        assert_eq!(pre(&["--connect-services"]).0, 0);
        assert_eq!(pre(&[]).0, 0);
        assert_eq!(pre(&["--star"]).0, 2);
        assert_eq!(
            pre(&["--notion-data-source", "x"]).0,
            0,
            "standalone ID is valid"
        );
        assert_eq!(pre(&["--notion-data-source", "a b"]).0, 2);
        let out = host.install_extras(IncomingPhase::Reconcile, dir.path(), &a(&["--star"]), &stop);
        assert_eq!(out.0, 2, "reconcile refuses an unsupported option too");
        assert_eq!(
            host.install_extras(IncomingPhase::Reconcile, dir.path(), &a(&[]), &stop)
                .0,
            0,
            "nothing requested, nothing run"
        );
        assert_eq!(
            host.install_extras(IncomingPhase::Verify, dir.path(), &a(&[]), &stop)
                .0,
            2
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn hook_protection_asks_the_native_ci_producer() {
        use oms_command_lifecycle::Services as Lifecycle;
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).expect("source is available");
        let dir = Fixture::new();
        // Not a repository: the producer refuses, so protection is never assumed.
        assert_ne!(host.hook_protection(dir.path(), &stop).0, 0);
        let cancelled = AtomicBool::new(true);
        assert_ne!(host.hook_protection(dir.path(), &cancelled).0, 0);
    }

    fn aged_file(path: &Path, days: u64) {
        std::fs::write(path, b"x").unwrap();
        let old = SystemTime::now() - Duration::from_secs(days * 86400 + 60);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    #[test]
    fn catalog_refresh_claims_the_daily_stamp_and_dry_run_only_reports() {
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).expect("source is available");
        let state = Fixture::new();
        let cache = Fixture::new();
        let stamp = state.path().join("nested/catalog-refresh.stamp");
        aged_file(&cache.path().join("alpha.models"), 8);
        aged_file(&cache.path().join("fresh.models"), 1);
        aged_file(&cache.path().join("alpha.env"), 30);

        // Dry run lists only a stale catalog, writes no stamp, creates no directory.
        let out = host.catalog_refresh(true, &stamp, cache.path(), source(), &stop);
        assert_eq!(out.0, 0);
        assert_eq!(
            String::from_utf8_lossy(&out.1),
            "would refresh catalog: alpha\n"
        );
        assert!(!stamp.exists() && !state.path().join("nested").exists());

        // A fresh stamp is a quiet no-op, even in dry-run.
        std::fs::create_dir_all(stamp.parent().unwrap()).unwrap();
        std::fs::write(&stamp, b"").unwrap();
        let out = host.catalog_refresh(true, &stamp, cache.path(), source(), &stop);
        assert_eq!((out.0, out.1.len()), (0, 0));
        let before = std::fs::metadata(&stamp).unwrap().modified().unwrap();
        let out = host.catalog_refresh(false, &stamp, cache.path(), source(), &stop);
        assert_eq!((out.0, out.1.len(), out.2.len()), (0, 0, 0));
        assert_eq!(
            std::fs::metadata(&stamp).unwrap().modified().unwrap(),
            before
        );

        // No stale catalog on disk: the day's attempt is still claimed, nothing refreshed.
        let empty = Fixture::new();
        aged_file(&stamp, 2);
        let out = host.catalog_refresh(false, &stamp, empty.path(), source(), &stop);
        assert_eq!((out.0, out.2.len()), (0, 0));
        assert!(
            std::fs::metadata(&stamp).unwrap().modified().unwrap()
                > before - Duration::from_secs(1)
        );
        assert!(!file_older_than(&stamp, CATALOG_PERIOD));
    }

    #[test]
    fn maintenance_receipt_has_exactly_the_native_fields() {
        use oms_command_lifecycle::Services as Lifecycle;
        let stop = Arc::new(AtomicBool::new(false));
        let host = CollabServices::new(&stop, source()).expect("source is available");
        let repo = Fixture::new();
        if std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1")
            || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH")
                .is_some_and(|v| !v.is_empty() && v != "0")
        {
            let out = host.tick_maintenance(repo.path(), source(), &stop);
            assert_ne!(out.0, 0);
            assert!(
                String::from_utf8_lossy(&out.2).contains("stale thread retirement failed (exit 2)")
            );
            if std::env::var_os("OMS_HARNESS_TEST_DELEGATED").is_none() {
                let child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "services_collab::tests::maintenance_receipt_has_exactly_the_native_fields",
                    ])
                    .env_remove("OMS_HARNESS_CHILD")
                    .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
                    .env("OMS_HARNESS_TEST_DELEGATED", "1")
                    .output()
                    .unwrap();
                assert!(
                    child.status.success(),
                    "{}{}",
                    String::from_utf8_lossy(&child.stdout),
                    String::from_utf8_lossy(&child.stderr)
                );
            }
            return;
        }
        if std::env::var_os("OMS_HARNESS_TEST_DELEGATED").is_none() {
            let delegated = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "services_collab::tests::maintenance_receipt_has_exactly_the_native_fields",
                ])
                .env("OMS_HARNESS_CHILD", "1")
                .env("OMS_HARNESS_TEST_DELEGATED", "1")
                .output()
                .unwrap();
            assert!(
                delegated.status.success(),
                "{}{}",
                String::from_utf8_lossy(&delegated.stdout),
                String::from_utf8_lossy(&delegated.stderr)
            );
        }
        let out = host.tick_maintenance(repo.path(), source(), &stop);
        assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        let row: Value = serde_json::from_slice(&out.1).expect("json");
        let keys: std::collections::BTreeSet<_> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let want: std::collections::BTreeSet<_> = [
            "journal_rc",
            "ci_rc",
            "plugin_rc",
            "plan_retire_rc",
            "artifact_resolve_rc",
            "artifact_supersede_rc",
            "threads_closed",
            "tasks_closed",
            "plans_retired",
            "artifacts_resolved",
            "artifacts_superseded",
            "failures_retired",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, want);
        for key in [
            "journal_rc",
            "ci_rc",
            "plugin_rc",
            "plan_retire_rc",
            "artifact_resolve_rc",
            "artifact_supersede_rc",
        ] {
            assert!(
                row[key] == "skipped" || row[key].is_u64(),
                "{key}: {}",
                row[key]
            );
        }
        assert_eq!(row["ci_rc"], "skipped", "no GitHub origin");
        assert_eq!(row["plugin_rc"], "skipped");
        assert_eq!(row["plan_retire_rc"], "skipped", "no plan file");
        assert_eq!(row["threads_closed"], json!([]));
        for key in [
            "tasks_closed",
            "plans_retired",
            "artifacts_resolved",
            "artifacts_superseded",
            "failures_retired",
        ] {
            assert_eq!(row[key], 0, "{key}");
        }
        // Cancellation stops the sweep with a refusal, never a receipt.
        let cancelled = AtomicBool::new(true);
        assert_ne!(
            host.tick_maintenance(repo.path(), source(), &cancelled).0,
            0
        );

        // Stale non-room non-current threads close under the log lock; aged room-*
        // threads, the active CURRENT thread, and fresh threads remain open.
        let threads = repo.path().join(".oms/threads");
        std::fs::create_dir_all(&threads).unwrap();
        std::fs::write(
            threads.join("th-stale-0001.jsonl"),
            "{\"schema\":1,\"ts\":\"2020-01-01T00:00:00Z\",\"thread\":\"th-stale-0001\",\"seq\":1,\"role\":\"topic\",\"agent\":\"agent\",\"text\":\"old\"}\n",
        )
        .unwrap();
        std::fs::write(
            threads.join("room-abcdef012345.jsonl"),
            concat!(
                "{\"schema\":1,\"ts\":\"2020-01-01T00:00:00Z\",\"thread\":\"room-abcdef012345\",\"seq\":1,\"role\":\"topic\",\"agent\":\"agent\",\"text\":\"room\"}\n",
                "{\"schema\":1,\"ts\":\"2020-01-01T00:00:00Z\",\"thread\":\"room-abcdef012345\",\"seq\":2,\"role\":\"note\",\"agent\":\"agent\",\"text\":\"hi\",\"room_event\":{\"kind\":\"created\"}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            threads.join("th-current-0001.jsonl"),
            "{\"schema\":1,\"ts\":\"2020-01-01T00:00:00Z\",\"thread\":\"th-current-0001\",\"seq\":1,\"role\":\"topic\",\"agent\":\"agent\",\"text\":\"current\"}\n",
        )
        .unwrap();
        std::fs::write(
            threads.join("CURRENT"),
            format!("th-current-0001 {} \n", now_secs()),
        )
        .unwrap();
        std::fs::write(
            threads.join("th-fresh-0001.jsonl"),
            "{\"schema\":1,\"ts\":\"2999-01-01T00:00:00Z\",\"thread\":\"th-fresh-0001\",\"seq\":1,\"role\":\"topic\",\"agent\":\"agent\",\"text\":\"fresh\"}\n",
        )
        .unwrap();
        let out = host.tick_maintenance(repo.path(), source(), &stop);
        assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        let row: Value = serde_json::from_slice(&out.1).expect("json");
        assert_eq!(row["threads_closed"], json!(["th-stale-0001"]));
        assert!(
            std::fs::read_to_string(threads.join("th-stale-0001.jsonl"))
                .unwrap()
                .contains("\"role\":\"closed\"")
        );
        for kept in [
            "room-abcdef012345.jsonl",
            "th-current-0001.jsonl",
            "th-fresh-0001.jsonl",
        ] {
            assert!(
                !std::fs::read_to_string(threads.join(kept))
                    .unwrap()
                    .contains("\"role\":\"closed\""),
                "{kept} should stay open"
            );
        }
    }

    #[test]
    fn maintenance_helpers_parse_boundaries() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2000-03-01T00:00:00Z"), Some(951868800));
        assert_eq!(parse_utc("2024-02-29T12:30:45Z"), Some(1709209845));
        for bad in [
            "",
            "2024-02-29",
            "2024-13-01T00:00:00Z",
            "2024-02-29T24:00:00Z",
            "2024-02-29T00:00:00+00:00",
            "abcd-02-29T00:00:00Z",
        ] {
            assert_eq!(parse_utc(bad), None, "{bad:?}");
        }
        assert!(aged("2000-01-01T00:00:00Z", 1));
        assert!(!aged("2999-01-01T00:00:00Z", 0));
        assert!(!aged("garbage", 0));
        assert!(goal_is_empty("# Task\n## Goal\n\n  \n## Plan\nx\n"));
        assert!(goal_is_empty("## Goal\n(... entry clipped ...)\n"));
        assert!(!goal_is_empty("## Goal\nship it\n"));
        assert!(!goal_is_empty("## Plan\nno goal section\n"));
        let line =
            |n: &str, k: &str| format!("noise\nartifact-index: {n} {k} failure(s) resolved\n");
        assert_eq!(
            resolved_count(line("3", "recovered").as_bytes(), "recovered"),
            Some(3)
        );
        assert_eq!(
            resolved_count(line("0", "recovered").as_bytes(), "recovered"),
            Some(0)
        );
        assert_eq!(
            resolved_count(line("3", "superseded").as_bytes(), "recovered"),
            None
        );
        assert_eq!(
            resolved_count(
                b"artifact-index: x recovered failure(s) resolved\n",
                "recovered"
            ),
            None
        );
        let twice = format!("{}{}", line("1", "recovered"), line("2", "recovered"));
        assert_eq!(resolved_count(twice.as_bytes(), "recovered"), None);
        let old = "2000-01-01T00:00:00Z";
        let done = |u: &str| format!(r#"{{"tasks":{{"a":{{"state":"done","updated":"{u}"}}}}}}"#);
        assert!(plan_idle(done(old).as_bytes(), 14));
        assert!(!plan_idle(done("2999-01-01T00:00:00Z").as_bytes(), 14));
        assert!(!plan_idle(br#"{"tasks":{}}"#, 14));
        assert!(!plan_idle(
            br#"{"tasks":{"a":{"state":"open","updated":"2000-01-01T00:00:00Z"}}}"#,
            14
        ));
        assert!(!plan_idle(b"{", 14));
        assert_eq!(env_days("OMS_TICK_COLLAB_TEST_UNSET", 7), Ok(7));
        assert_eq!(tools_refresh(true)[0], "install-profile");
        assert_eq!(tools_refresh(false), ["install-tools", "--upgrade"]);
    }

    /// A staged revision directory whose `oms` echoes its argument vector and
    /// writes a schema-2 receipt naming the update-parent marker it received.
    fn stage(source: &Path, rev: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let dir = source.join("target/oms-native").join(rev);
        std::fs::create_dir_all(&dir).unwrap();
        let body = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--oms-dispatch\" ] && [ \"$5\" = \"list\" ]; then printf 'install-profile\\ndoctor\\n'; exit 0; fi\n{script}\n"
        );
        let oms_path = dir.join("oms");
        std::fs::write(&oms_path, &body).unwrap();
        std::fs::set_permissions(&oms_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let digest = format!("{:x}", Sha256::digest(body.as_bytes()));
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                "{{\"schema\":1,\"source_commit\":\"{rev}\",\"artifacts\":{{\"oms\":\"{digest}\"}}}}"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("oms.sha256"), format!("{digest}\n")).unwrap();
    }

    #[test]
    fn incoming_lifecycle_runs_the_staged_checkout_and_returns_its_receipt() {
        use oms_command_lifecycle::Services as Lifecycle;
        let stop = Arc::new(AtomicBool::new(false));
        let fixture = Fixture::new();
        let root = std::fs::canonicalize(fixture.path()).unwrap();
        let host = CollabServices::new(&stop, &root).expect("source is available");
        let rev = "0123456789abcdef0123456789abcdef01234567";
        let run = |phase, revision: &str| {
            host.incoming_lifecycle(phase, &root, revision, b"{}", &[], &stop)
        };

        // Bad revisions never reach a child.
        for bad in [
            "",
            "abc",
            "0123456789ABCDEF0123456789abcdef01234567",
            "../../etc",
        ] {
            assert_eq!(run(IncomingPhase::Verify, bad).0, 2, "{bad:?}");
        }
        // Unstaged: every phase fails closed before any checkout or execution.
        for phase in [
            IncomingPhase::Preflight,
            IncomingPhase::Reconcile,
            IncomingPhase::Verify,
            IncomingPhase::RefreshTools,
        ] {
            let out = run(phase, rev);
            assert_eq!(out.0, 2);
            assert!(String::from_utf8_lossy(&out.2).contains("unavailable"));
        }
        // Preflight continues to fail closed even when crate manifests exist
        // on disk until the incoming revision is actually staged.
        std::fs::create_dir_all(root.join("crates/oms")).unwrap();
        std::fs::write(root.join("crates/oms/Cargo.toml"), b"").unwrap();
        std::fs::write(root.join("Cargo.toml"), b"").unwrap();
        let preflight = run(IncomingPhase::Preflight, rev);
        assert_eq!(preflight.0, 2);
        assert!(String::from_utf8_lossy(&preflight.2).contains("unavailable"));

        let receipt = root.join("receipt.json");
        stage(
            &root,
            rev,
            &format!(
                r#"shift 4
case "$1" in
install) printf '{{"schema":2,"parent":"%s"}}' "$OMS_NATIVE_UPDATE_PARENT" > '{r}'; echo "installed native OMS 0123456789abcdef0123456789abcdef01234567; receipt: {r}" ;;
*) printf 'ran %s' "$*"; test "$OMS_NATIVE_UPDATE_PARENT" = "$$PARENT" ;;
esac"#,
                r = receipt.display()
            )
            .replace("$$PARENT", &std::process::id().to_string()),
        );
        assert_eq!(
            run(IncomingPhase::Preflight, rev).0,
            0,
            "a staged binary is dispatchable"
        );
        let out = run(IncomingPhase::Reconcile, rev);
        assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        let row: Value = serde_json::from_slice(&out.1).unwrap();
        assert_eq!(row["schema"], 2);
        assert_eq!(row["parent"], std::process::id().to_string());
        let out = run(IncomingPhase::Verify, rev);
        assert_eq!(
            (out.0, String::from_utf8_lossy(&out.1).into_owned()),
            (0, "ran doctor".into())
        );
        let out = run(IncomingPhase::RefreshTools, rev);
        assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
        assert!(String::from_utf8_lossy(&out.1).starts_with("ran install-"));

        // A receipt that is not schema 2 is refused rather than returned.
        std::fs::write(&receipt, br#"{"schema":1}"#).unwrap();
        stage(
            &root,
            rev,
            &format!(
                "shift 4\necho \"installed native OMS x; receipt: {}\"",
                receipt.display()
            ),
        );
        assert_eq!(run(IncomingPhase::Reconcile, rev).0, 2);
        // A failing child keeps its exit status.
        stage(&root, rev, "exit 7");
        assert_eq!(run(IncomingPhase::Verify, rev).0, 7);
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            host.incoming_lifecycle(IncomingPhase::Verify, &root, rev, b"{}", &[], &cancelled)
                .0,
            130
        );
    }

    #[test]
    fn read_receipt_is_bounded_regular_json_of_schema_two() {
        let dir = Fixture::new();
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        assert!(read_receipt(&write("ok.json", br#"{"schema":2}"#)).is_ok());
        assert!(read_receipt(&dir.path().join("missing.json")).is_err());
        assert!(read_receipt(&write("one.json", br#"{"schema":1}"#)).is_err());
        assert!(read_receipt(&write("text.json", b"not json")).is_err());
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(dir.path().join("ok.json"), &link).unwrap();
        assert!(read_receipt(&link).is_err(), "symlinks are refused");
        let mut big = br#"{"schema":2,"pad":""#.to_vec();
        big.resize(RECEIPT_LIMIT as usize, b'a');
        big.extend_from_slice(b"\"}");
        assert!(
            read_receipt(&write("big.json", &big)).is_err(),
            "over the limit"
        );
        let mut exact = br#"{"schema":2,"pad":""#.to_vec();
        exact.resize(RECEIPT_LIMIT as usize - 2, b'a');
        exact.extend_from_slice(b"\"}");
        assert_eq!(exact.len() as u64, RECEIPT_LIMIT);
        assert!(
            read_receipt(&write("exact.json", &exact)).is_ok(),
            "equal to the limit"
        );
    }

    #[test]
    fn preflight_rejects_corrupt_or_tainted_staged_payload() {
        use oms_command_lifecycle::Services as Lifecycle;
        use std::os::unix::fs::PermissionsExt;
        let stop = Arc::new(AtomicBool::new(false));
        let fixture = Fixture::new();
        let root = std::fs::canonicalize(fixture.path()).unwrap();
        let host = CollabServices::new(&stop, &root).unwrap();
        let rev = "0123456789abcdef0123456789abcdef01234567";
        let dir = root.join("target/oms-native").join(rev);
        let run = || {
            host.incoming_lifecycle(IncomingPhase::Preflight, &root, rev, b"{}", &[], &stop)
                .0
        };

        stage(&root, rev, "exit 0");
        let preflight =
            host.incoming_lifecycle(IncomingPhase::Preflight, &root, rev, b"{}", &[], &stop);
        assert_eq!(preflight.0, 0, "{}", String::from_utf8_lossy(&preflight.2));

        // Corrupt manifest is refused before spawning.
        std::fs::write(dir.join("manifest.json"), b"not json").unwrap();
        assert_eq!(run(), 2);

        // Bad or missing oms.sha256 is refused.
        stage(&root, rev, "exit 0");
        std::fs::write(dir.join("oms.sha256"), format!("{}\n", "0".repeat(64))).unwrap();
        assert_eq!(run(), 2);
        std::fs::remove_file(dir.join("oms.sha256")).unwrap();
        assert_eq!(run(), 2);

        // Non-executable staged binary is refused.
        stage(&root, rev, "exit 0");
        std::fs::set_permissions(dir.join("oms"), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(run(), 2);

        // Foreign file inside payload dir is refused.
        stage(&root, rev, "exit 0");
        std::fs::write(dir.join("stray.txt"), b"unexpected").unwrap();
        assert_eq!(run(), 2);
        std::fs::remove_file(dir.join("stray.txt")).unwrap();

        // Failing `list` check or missing expected surface names fails closed.
        let oms_path = dir.join("oms");
        let bad_list = "#!/bin/sh\necho unexpected\nexit 0\n";
        std::fs::write(&oms_path, bad_list).unwrap();
        std::fs::set_permissions(&oms_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let digest = format!("{:x}", Sha256::digest(bad_list.as_bytes()));
        std::fs::write(
            dir.join("manifest.json"),
            format!("{{\"schema\":1,\"source_commit\":\"{rev}\",\"artifacts\":{{\"oms\":\"{digest}\"}}}}"),
        )
        .unwrap();
        std::fs::write(dir.join("oms.sha256"), format!("{digest}\n")).unwrap();
        assert_eq!(run(), 2);
    }
}
