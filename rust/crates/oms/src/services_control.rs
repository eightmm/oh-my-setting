//! Native control-plane service adapters (`plan`, `autonomy`, `landing`, `graph`, `task`, `state`).
//!
//! Authority rules: an adapter only forwards to a public native producer. Where
//! no producer exists (landing lease/approval compare-and-set, main-reviewer
//! capture, admission context, the canonical publication fence) the trait's
//! fail-closed default is kept; tool output, environment labels and room
//! membership are never treated as a grant. Nothing here writes a file itself:
//! every mutation is performed by the owning domain crate under its own lock.
//!
//! Child processes (`invoke`, `invoke_command`, verification, the publication
//! scan worker) always run through `oms_supervisor` with a wall deadline,
//! bounded output and cancellation; argv is never shell-parsed.
//! [`ControlServices::new`] enforces `Scope::WholeTree` (the supervisor's
//! exclusive subreaper reactor) on Linux and `Scope::ProcessGroup` on
//! non-Linux hosts; if a Linux caller cannot guarantee exclusive child
//! ownership, the supervisor fails closed rather than weakening containment.
use oms_command_graph::Error as GraphError;
use oms_command_landing::services::{
    Execution as LandingExecution, PublicationGuard, PublicationScan, VerifyRequest,
};
use oms_command_plan::services::{
    ApprovalGrant, Execution as PlanExecution, LifecycleGuard, ProjectedLifecycle, ProjectedRoom,
    SupervisedCommand,
};
use oms_command_task::services::{
    FailureObservation, JournalObservation, MemoryOutcome, ReferenceOperation, VerificationStatus,
    VerifyObservation,
};
use oms_supervisor::capture::{CaptureOptions, LimitAction};
use oms_supervisor::process::{Progress, Reactor, Request, Scope, StopReason, Streams};
use serde_json::Value;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const DEFAULT_WALL: Duration = Duration::from_secs(1800);
const COMMAND_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const VERIFY_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const SCAN_OUTPUT_LIMIT: usize = 64 * 1024;
const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;
/// First argument of the dedicated publication-scan worker process. The binary's
/// entry point must route `<exe> SCAN_WORKER_FLAG ...` to [`scan_worker_main`]
/// before any other dispatch; the parent never runs the worker in-process.
pub(crate) const SCAN_WORKER_FLAG: &str = "--oms-publication-scan-worker";

/// Outcome of one fully supervised child. Exit codes follow the shell
/// convention used by the owning crates: 124 deadline, 125 output limit or
/// supervisor stop, 130 cancellation.
struct Child {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    elapsed: Duration,
    timed_out: bool,
    output_limited: bool,
}

/// One supervised launch. `merged` joins stderr into stdout (acceptance
/// commands); otherwise the streams are captured separately.
struct Spawn<'a> {
    scope: Scope,
    argv: Vec<OsString>,
    cwd: Option<&'a Path>,
    environment: Vec<(OsString, OsString)>,
    remove_environment: Vec<OsString>,
    merged: bool,
    wall: Duration,
    limit: usize,
    terminate_grace: Duration,
}
impl<'a> Spawn<'a> {
    fn new(scope: Scope, argv: Vec<OsString>, wall: Duration, limit: usize) -> Self {
        Self {
            scope,
            argv,
            cwd: None,
            environment: Vec::new(),
            remove_environment: Vec::new(),
            merged: false,
            wall,
            limit,
            terminate_grace: Duration::from_secs(2),
        }
    }
    fn cwd(mut self, cwd: &'a Path) -> Self {
        self.cwd = Some(cwd);
        self
    }
    fn environment(mut self, environment: Vec<(OsString, OsString)>) -> Self {
        self.environment = environment;
        self
    }
    fn terminate_grace(mut self, grace: Duration) -> Self {
        self.terminate_grace = grace;
        self
    }
}

fn run_child(spawn: Spawn<'_>, interrupted: &AtomicBool) -> Result<Child, String> {
    let Spawn {
        scope,
        argv,
        cwd,
        environment,
        remove_environment,
        merged,
        wall,
        limit,
        terminate_grace,
    } = spawn;
    if interrupted.load(Ordering::Acquire) {
        return Err("interrupted".into());
    }
    if argv.is_empty() || wall.is_zero() || limit == 0 {
        return Err("execution requires a command and positive wall and output bounds".into());
    }
    let mut request = Request::new(argv);
    request.cwd = cwd.map(Path::to_owned);
    request.stdin = Some(Vec::new());
    request.environment = environment;
    let mut remove_environment = remove_environment;
    for key in ["OMS_NATIVE_OUTCOME", "OMS_NATIVE_DISPATCHED"] {
        if !remove_environment.iter().any(|v| v == key) {
            remove_environment.push(OsString::from(key));
        }
    }
    request.remove_environment = remove_environment;
    request.streams = if merged {
        Streams::Merged
    } else {
        Streams::Separate
    };
    request.scope = scope;
    request.terminate_grace = terminate_grace;
    request.deadline = Some(
        Instant::now()
            .checked_add(wall)
            .ok_or("execution deadline overflow")?,
    );
    request.capture = CaptureOptions {
        max_bytes: limit,
        on_limit: LimitAction::Terminate,
        lines: None,
    };
    if !merged {
        request.stderr_capture = Some(request.capture.clone());
    }
    let mut reactor = if scope == Scope::WholeTree {
        Reactor::dedicated().map_err(|e| e.to_string())?
    } else {
        Reactor::new()
    };
    let id = reactor.start(request).map_err(|e| e.to_string())?;
    let mut cancelled = false;
    loop {
        if interrupted.load(Ordering::Acquire) && !cancelled {
            reactor
                .stop(id, StopReason::Cancelled)
                .map_err(|e| e.to_string())?;
            cancelled = true;
        }
        match reactor
            .poll(id, Duration::from_millis(50))
            .map_err(|e| e.to_string())?
        {
            Progress::Pending(_) => {}
            Progress::Complete(done) => {
                // A stop (deadline, cancel, output limit) deliberately cuts the
                // stream: the exit code reports it. A natural exit must have
                // drained every byte, and cleanup is always required.
                if !done.cleanup_complete() || (done.stop.is_none() && !done.output_complete()) {
                    return Err("native supervision incomplete; reconcile before retry".into());
                }
                let code = match done.stop {
                    Some(StopReason::Deadline) => 124,
                    Some(StopReason::Cancelled) => 130,
                    Some(_) => 125,
                    None => done.exit.map_or(125, |e| e.shell_code()),
                };
                return Ok(Child {
                    code,
                    stdout: done.stdout.bytes,
                    stderr: done.stderr.map_or_else(Vec::new, |s| s.bytes),
                    elapsed: done.duration,
                    timed_out: done.stop == Some(StopReason::Deadline),
                    output_limited: done.stop == Some(StopReason::OutputLimit),
                });
            }
        }
    }
}

fn plan_error(message: impl Into<String>) -> oms_command_plan::Error {
    oms_command_plan::Error {
        code: 2,
        message: message.into(),
    }
}
fn autonomy_error(code: i32, message: impl Into<String>) -> oms_command_autonomy::Error {
    oms_command_autonomy::Error::new(code, message)
}
fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Explicit `OMS_STATE_REPO` wins; otherwise the room producer resolves the
/// physical work root and refuses unverified managed scratch worktrees.
fn state_repository_for(work: &Path) -> Result<PathBuf, String> {
    if let Some(state) = std::env::var_os("OMS_STATE_REPO").filter(|v| !v.is_empty()) {
        return std::fs::canonicalize(state)
            .map_err(|_| "cannot resolve canonical state repository".to_owned());
    }
    oms_command_room::Services::state_repository(&oms_command_room::LocalServices, work)
        .map_err(|e| e.to_string())
}

/// Control-plane adapter shared by `plan`, `autonomy`, `landing`, `graph` and
/// `state`. Holds only a cancellation flag and validated paths, so it is `Sync`.
pub(crate) struct ControlServices<'a> {
    interrupted: &'a Arc<AtomicBool>,
    local: oms_command_autonomy::LocalServices<'a>,
    source: PathBuf,
    native: Vec<OsString>,
    scan_worker: Vec<OsString>,
    scope: Scope,
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
    if rev.len() != 40 || !rev.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Vec::new();
    }
    if rev_dir.parent() != Some(source.join("rust/target/oms-native").as_path()) {
        return Vec::new();
    }
    let manifest = rev_dir.join("manifest.json");
    if !std::fs::symlink_metadata(&manifest).is_ok_and(|m| m.file_type().is_file()) {
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

impl<'a> ControlServices<'a> {
    /// `source` is the validated native source/install root. Child commands and
    /// the scan worker re-execute this very binary (resolved, existing file).
    /// Enforces `Scope::WholeTree` on Linux and `Scope::ProcessGroup` on
    /// non-Linux hosts.
    pub(crate) fn new(interrupted: &'a Arc<AtomicBool>, source: &Path) -> Result<Self, String> {
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
        #[cfg(target_os = "linux")]
        let scope = Scope::WholeTree;
        #[cfg(not(target_os = "linux"))]
        let scope = Scope::ProcessGroup;
        Ok(Self {
            interrupted,
            local: oms_command_autonomy::LocalServices::new(interrupted.as_ref()),
            source,
            native,
            scan_worker: vec![exe.into(), SCAN_WORKER_FLAG.into()],
            scope,
        })
    }
    /// Test constructor overriding child launchers with `ProcessGroup` scope so
    /// multi-threaded `cargo test` harnesses can run process-group cases while
    /// production code uses [`ControlServices::new`].
    #[cfg(test)]
    fn with_launchers(
        interrupted: &'a Arc<AtomicBool>,
        source: PathBuf,
        native: Vec<OsString>,
        scan_worker: Vec<OsString>,
    ) -> Self {
        Self {
            interrupted,
            local: oms_command_autonomy::LocalServices::new(interrupted.as_ref()),
            source,
            native,
            scan_worker,
            scope: Scope::ProcessGroup,
        }
    }
    /// Opt into whole-tree containment in tests.
    #[cfg(test)]
    fn whole_tree(mut self) -> Self {
        self.scope = Scope::WholeTree;
        self
    }
    /// Task-lifecycle producers bound to this adapter's source root.
    pub(crate) fn task(&self) -> ControlTask<'_> {
        ControlTask {
            interrupted: self.interrupted,
            source: &self.source,
        }
    }
}

impl oms_command_plan::Services for ControlServices<'_> {
    fn state_root(&self, repo: &Path) -> oms_command_plan::Result<Option<PathBuf>> {
        let state = state_repository_for(repo).map_err(plan_error)?;
        let own = std::fs::canonicalize(repo)
            .map_err(|_| plan_error("cannot resolve physical repository"))?;
        Ok((state != own).then_some(state))
    }
    fn lifecycle_writer(
        &self,
        repo: &Path,
    ) -> oms_command_plan::Result<Box<dyn LifecycleGuard + '_>> {
        oms_command_plan::Services::lifecycle_writer(&self.local, repo)
    }
    fn project_lifecycle(&self, raw: &[u8]) -> oms_command_plan::Result<ProjectedLifecycle> {
        oms_command_plan::Services::project_lifecycle(&self.local, raw)
    }
    fn project_room(&self, room: &str, raw: &[u8]) -> oms_command_plan::Result<ProjectedRoom> {
        oms_command_plan::Services::project_room(&self.local, room, raw)
    }
    /// Acceptance commands run with merged output, the caller's environment and
    /// the named variables removed from the entire command tree.
    fn supervise(&self, command: &SupervisedCommand) -> oms_command_plan::Result<PlanExecution> {
        let mut spawn = Spawn::new(
            self.scope,
            command.argv.clone(),
            command.wall,
            command.output_limit,
        )
        .cwd(&command.cwd)
        .environment(command.environment.clone());
        spawn.merged = true;
        spawn.remove_environment = command
            .remove_environment
            .iter()
            .map(OsString::from)
            .collect();
        let child = run_child(spawn, self.interrupted).map_err(plan_error)?;
        Ok(PlanExecution {
            exit: child.code.clamp(0, 255) as u8,
            runner_exit: 0,
            timed_out: child.timed_out,
            output_limited: child.output_limited,
            launch_error: false,
            supervision_error: false,
            output: child.stdout,
            duration_s: child.elapsed.as_secs(),
        })
    }
    /// Public projection comes from the native `approval-inbox show --json`;
    /// the private stream is the validated ledger. Both are read twice around
    /// the projection and refused if the ledger changed in between.
    fn landing_approval(
        &self,
        repo: &Path,
        approval_id: &str,
    ) -> oms_command_plan::Result<ApprovalGrant> {
        let path = oms_command_operations::approval_path(repo, false).map_err(plan_error)?;
        let before = oms_command_operations::storage::read_bytes(&path).map_err(plan_error)?;
        let (code, public_record, stderr) = oms_command_operations::run(
            oms_command_operations::App::Approvals,
            &[
                "--repo".into(),
                repo.into(),
                "show".into(),
                "--approval".into(),
                approval_id.into(),
                "--json".into(),
            ],
            repo,
            self.interrupted,
        );
        if code != 0 {
            return Err(plan_error(format!(
                "native approval projection failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        let after = oms_command_operations::storage::read_bytes(&path).map_err(plan_error)?;
        if before != after {
            return Err(plan_error("approval ledger changed during projection"));
        }
        Ok(ApprovalGrant {
            public_record,
            private_rows: before,
        })
    }
    fn normalize_provider(&self, provider: &str) -> oms_command_plan::Result<String> {
        oms_command_peer::providers::normalize(provider).map_err(|e| plan_error(e.message))
    }
    fn group_membership_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        Some(crate::services_runtime::resolve_group_membership)
    }
}

impl<'a> ControlServices<'a> {
    fn invoke_with_scope(
        &self,
        command: &oms_command_autonomy::Invocation,
        scope: Scope,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<oms_command_autonomy::CommandResult> {
        if self.native.is_empty() || !native_verb(&command.tool) {
            return Err(autonomy_error(2, "invalid native command prefix or verb"));
        }
        let mut argv = self.native.clone();
        argv.push(command.tool.clone().into());
        argv.extend(command.args.iter().cloned());
        let mut spawn = Spawn::new(scope, argv, command.wall, command.output_limit)
            .cwd(&command.cwd)
            .environment(command.environment.clone());
        if scope == Scope::ProcessGroup {
            spawn = spawn.terminate_grace(Duration::from_secs(4));
        }
        let child = run_child(spawn, interrupted).map_err(|e| autonomy_error(2, e))?;
        Ok((child.code, child.stdout, child.stderr))
    }
}

impl oms_command_autonomy::Services for ControlServices<'_> {
    /// Resolves explicit assignments and `config/panel-routing.json` tier defaults
    /// using the native `oms-command-peer` routing table, failing closed on missing
    /// tiers or `provider-default`.
    fn resolve_route(
        &self,
        _repo: &Path,
        assignment: &Value,
        role: &str,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<oms_command_autonomy::ModelRoute> {
        if interrupted.load(Ordering::Acquire) {
            return Err(autonomy_error(130, "interrupted"));
        }
        let provider = oms_command_peer::providers::normalize(&text_of(assignment, "provider"))
            .map_err(|e| autonomy_error(2, e.message))?;
        let primary_raw = text_of(assignment, "model");
        let fallback = text_of(assignment, "fallback_model");
        let effort_raw = text_of(assignment, "reasoning_effort");
        if primary_raw == "provider-default" || fallback == "provider-default" {
            return Err(autonomy_error(
                2,
                "reserved model value provider-default is refused",
            ));
        }
        for model in [&primary_raw, &fallback] {
            if !model.is_empty() {
                oms_command_peer::providers::model_name(model)
                    .map_err(|e| autonomy_error(2, e.message))?;
            }
        }
        let role_name = if role.is_empty() { "worker" } else { role };
        let resolved = oms_command_peer::providers::resolve_route(
            &self.source,
            &provider,
            &primary_raw,
            &effort_raw,
            role_name,
        )
        .map_err(|e| autonomy_error(2, e.message))?;
        if resolved.provider != provider {
            return Err(autonomy_error(
                2,
                "route resolution altered normalized provider",
            ));
        }
        if resolved.model.is_empty() || resolved.model == "provider-default" {
            return Err(autonomy_error(
                2,
                "policy route unavailable for provider and role; name the model explicitly",
            ));
        }
        oms_command_peer::providers::model_name(&resolved.model)
            .map_err(|e| autonomy_error(2, e.message))?;
        let effort = if resolved.effort.is_empty() {
            "auto".to_owned()
        } else {
            resolved.effort
        };
        if !["auto", "low", "medium", "high", "xhigh", "max", "ultra"].contains(&effort.as_str()) {
            return Err(autonomy_error(2, "invalid reasoning effort"));
        }
        Ok(oms_command_autonomy::ModelRoute {
            provider: resolved.provider,
            model_class: if resolved.explicit {
                "explicit".into()
            } else {
                "policy".into()
            },
            primary: resolved.model,
            fallback,
            effort,
            fallback_effort: String::new(),
        })
    }
    fn observe_journal(
        &self,
        event: oms_command_autonomy::JournalEvent<'_>,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<()> {
        let args: Vec<OsString> = [
            "observe".as_ref(),
            "--repo".as_ref(),
            event.repo.as_os_str(),
            "--source-type".as_ref(),
            event.source_kind.as_ref(),
            "--source-file".as_ref(),
            event.source_path.as_os_str(),
            "--source-id".as_ref(),
            event.source_id.as_ref(),
            "--outcome".as_ref(),
            event.outcome.as_ref(),
            "--verification-status".as_ref(),
            event.verification.as_ref(),
        ]
        .iter()
        .map(|v: &&std::ffi::OsStr| (*v).to_owned())
        .collect();
        let (code, _, stderr) =
            oms_command_journal::run_with_source(&args, event.repo, &self.source, interrupted);
        if code == 0 {
            Ok(())
        } else {
            Err(autonomy_error(
                code,
                format!(
                    "native journal observer refused: {}",
                    String::from_utf8_lossy(&stderr).trim()
                ),
            ))
        }
    }
    fn read_issue(
        &self,
        endpoint: &str,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<Vec<u8>> {
        oms_command_autonomy::Services::read_issue(&self.local, endpoint, interrupted)
    }
    fn state_repository(
        &self,
        work: &Path,
        _interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<PathBuf> {
        state_repository_for(work).map_err(|e| autonomy_error(2, e))
    }
    fn invoke(
        &self,
        command: &oms_command_autonomy::Invocation,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<oms_command_autonomy::CommandResult> {
        self.invoke_with_scope(command, self.scope, interrupted)
    }
    fn invoke_concurrent(
        &self,
        command: &oms_command_autonomy::Invocation,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<oms_command_autonomy::CommandResult> {
        // Worker threads in `plan-run --parallel` run concurrently in one
        // controller process where `Scope::WholeTree` refuses multi-threaded
        // callers. Each child `oms` process still enforces its own containment.
        self.invoke_with_scope(command, Scope::ProcessGroup, interrupted)
    }
    fn controller_digest(&self) -> oms_command_autonomy::Result<String> {
        oms_reference::controller::Controller::capture_current()
            .and_then(|c| c.digest().map(str::to_owned))
            .map_err(|e| autonomy_error(2, e.to_string()))
    }
    fn validate_context_pack(
        &self,
        repo: &Path,
        path: &Path,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<Value> {
        if interrupted.load(Ordering::Acquire) {
            return Err(autonomy_error(130, "interrupted"));
        }
        if path.as_os_str().is_empty() {
            return Err(autonomy_error(2, "invalid pack path"));
        }
        let target = if path.is_absolute() {
            path.to_path_buf()
        } else {
            repo.join(path)
        };
        let _groups_guard = oms_command_graph::with_group_resolver(Some(
            crate::services_runtime::resolve_group_membership,
        ));
        oms_command_graph::project::context::validate_context_pack(&target, repo)
            .map_err(|e| autonomy_error(2, e.to_string()))
    }
}

/// Reject anything that is not a plain native verb before it reaches argv:
/// option-looking, path-like or empty values never select a command.
fn native_verb(tool: &str) -> bool {
    !tool.is_empty()
        && tool.len() <= 64
        && tool.as_bytes()[0].is_ascii_lowercase()
        && tool
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl oms_command_graph::HostServices for ControlServices<'_> {
    fn install_root(&self) -> Option<&Path> {
        Some(&self.source)
    }
    fn invoke_command(
        &self,
        tool: &str,
        args: &[OsString],
        cwd: &Path,
        env: &[(String, String)],
        timeout: Option<Duration>,
        cancel: &AtomicBool,
    ) -> oms_command_graph::Result<(i32, Vec<u8>, Vec<u8>, bool)> {
        if self.native.is_empty() || !native_verb(tool) {
            return Err(GraphError::new(format!(
                "tool capability '{tool}' requires its canonical host adapter"
            )));
        }
        let mut argv = self.native.clone();
        argv.push(tool.into());
        argv.extend(args.iter().cloned());
        let environment = env
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect();
        let spawn = Spawn::new(
            self.scope,
            argv,
            timeout.unwrap_or(DEFAULT_WALL),
            COMMAND_OUTPUT_LIMIT,
        );
        let child =
            run_child(spawn.cwd(cwd).environment(environment), cancel).map_err(GraphError::new)?;
        Ok((child.code, child.stdout, child.stderr, child.timed_out))
    }
}

struct NativePublicationGuard(#[allow(dead_code)] oms_lock::Guard);
impl PublicationGuard for NativePublicationGuard {}

fn ensure_private_locks_dir(repo: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    for dir in [repo.join(".oms"), repo.join(".oms/locks")] {
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
    }
}

impl oms_command_landing::Services for ControlServices<'_> {
    fn plan(&self) -> Option<&dyn oms_command_plan::Services> {
        Some(self)
    }
    fn supervise(
        &self,
        request: &VerifyRequest,
        interrupted: &AtomicBool,
    ) -> oms_command_landing::Result<LandingExecution> {
        let spawn = Spawn::new(
            self.scope,
            request.argv.clone(),
            request.wall,
            VERIFY_OUTPUT_LIMIT,
        );
        let spawn = spawn
            .cwd(&request.cwd)
            .environment(request.environment.clone());
        let child =
            run_child(spawn, interrupted).map_err(|e| oms_command_landing::Error::new(2, e))?;
        Ok(LandingExecution {
            code: child.code,
            stdout: child.stdout,
            stderr: child.stderr,
            elapsed: child.elapsed,
        })
    }
    /// The worker lowers `RLIMIT_AS` for its whole process lifetime, so it only
    /// ever runs in a dedicated re-executed child; this parent's limits are
    /// never touched. Only a zero exit passes.
    fn publication_scan(
        &self,
        request: &PublicationScan,
        interrupted: &AtomicBool,
    ) -> oms_command_landing::Result<()> {
        let fail = |code: i32, message: String| oms_command_landing::Error::new(code, message);
        if self.scan_worker.is_empty() {
            return Err(oms_command_landing::Error::unavailable(
                "bounded native publication scan worker",
            ));
        }
        let mut argv = self.scan_worker.clone();
        argv.extend(scan_arguments(request).map_err(|e| fail(2, e))?);
        let spawn = Spawn::new(self.scope, argv, request.wall, SCAN_OUTPUT_LIMIT);
        let child = run_child(spawn.cwd(&request.repo), interrupted).map_err(|e| fail(2, e))?;
        match child.code {
            0 => Ok(()),
            130 => Err(fail(130, "publication scan interrupted".into())),
            _ if child.timed_out => Err(fail(3, "publication scan exceeded deadline".into())),
            code => {
                let detail = String::from_utf8_lossy(&child.stderr);
                let detail = detail.lines().next().unwrap_or("").trim();
                Err(fail(
                    if code == 3 { 3 } else { 2 },
                    if detail.is_empty() {
                        format!("publication scan worker failed (exit {code})")
                    } else {
                        detail.to_owned()
                    },
                ))
            }
        }
    }
    fn check_python_syntax(&self, path: &Path, source: &[u8]) -> Option<bool> {
        crate::check_python_syntax_with_companion(path, source)
    }
    fn publication_guard(
        &self,
        repo: &Path,
        interrupted: &AtomicBool,
    ) -> oms_command_landing::Result<Box<dyn PublicationGuard + '_>> {
        let canonical = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        ensure_private_locks_dir(&canonical);
        let mut settings = crate::services_runtime::resolve_lock_settings()
            .map_err(|(code, msg)| oms_command_landing::Error::new(code, msg))?;
        settings.cwd = canonical.clone();
        let target = canonical.join(".oms/locks/publication.lock");
        let lock = oms_lock::Lock::for_file(&target, settings.clone())
            .map_err(|e| oms_command_landing::Error::new(e.exit_code(), e.to_string()))?;
        let started = Instant::now();
        loop {
            if interrupted.load(Ordering::Acquire) {
                return Err(oms_command_landing::Error::new(
                    130,
                    "publication guard interrupted",
                ));
            }
            match lock.try_acquire() {
                Ok(guard) => return Ok(Box::new(NativePublicationGuard(guard))),
                Err(oms_lock::Error::Busy) if started.elapsed() < settings.timeout => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(oms_lock::Error::Busy) => {
                    return Err(oms_command_landing::Error::new(75, "publication lock busy"));
                }
                Err(err) => {
                    return Err(oms_command_landing::Error::new(
                        err.exit_code(),
                        err.to_string(),
                    ));
                }
            }
        }
    }
    fn wait_siblings(
        &self,
        repo: &Path,
        budget: Duration,
        interrupted: &AtomicBool,
    ) -> oms_command_landing::Result<()> {
        let canonical = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        let tmp_dir = canonical.join(".oms/tmp");
        let self_attempt = std::env::var("OMS_ATTEMPT_ID").unwrap_or_default();
        let main_attempt = std::env::var("OMS_PANEL_MAIN_ATTEMPT").unwrap_or_default();
        let mut settings = crate::services_runtime::resolve_lock_settings()
            .map_err(|(code, msg)| oms_command_landing::Error::new(code, msg))?;
        settings.cwd = canonical.clone();
        let started = Instant::now();
        loop {
            if interrupted.load(Ordering::Acquire) {
                return Err(oms_command_landing::Error::new(
                    130,
                    "interrupted waiting for sibling worktrees",
                ));
            }
            let mut active = false;
            if let Ok(entries) = std::fs::read_dir(&tmp_dir) {
                for entry in entries.flatten() {
                    if !entry.file_name().to_string_lossy().starts_with("peer-") {
                        continue;
                    }
                    let owner = entry.path().join("owner.json");
                    if !std::fs::symlink_metadata(&owner).is_ok_and(|m| m.file_type().is_file()) {
                        continue;
                    }
                    if let Ok(lock) = oms_lock::Lock::for_file(&owner, settings.clone())
                        && matches!(lock.try_acquire(), Err(oms_lock::Error::Busy))
                    {
                        active = true;
                        break;
                    }
                }
            }
            if !active && let Ok((_, rows)) = oms_command_operations::load_projection(&canonical) {
                let now_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                active = rows.iter().any(|row| {
                    let state = row.get("state").text().unwrap_or("");
                    let role = row.get("refs").get("panel_role").text().unwrap_or("");
                    let aid = row.get("attempt_id").text().unwrap_or("");
                    let fresh =
                        oms_command_operations::projection::stamp(row.get("last_heartbeat_at"))
                            .or_else(|| {
                                oms_command_operations::projection::stamp(row.get("updated_at"))
                            })
                            .is_some_and(|ts| now_epoch.saturating_sub(ts) <= 300);
                    matches!(state, "starting" | "working" | "verifying")
                        && fresh
                        && row.get("tool").text() == Some("peer-delegate")
                        && matches!(role, "worker" | "delegate")
                        && !aid.is_empty()
                        && aid != self_attempt
                        && aid != main_attempt
                });
            }
            if !active {
                return Ok(());
            }
            if started.elapsed() >= budget {
                return Err(oms_command_landing::Error::new(
                    75,
                    "timed out waiting for active sibling worktrees",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn after_land(
        &self,
        repo: &Path,
        receipt: &oms_command_landing::V,
        interrupted: &AtomicBool,
    ) -> oms_command_landing::Result<oms_command_landing::V> {
        if !repo.join("scripts/oms").is_file() || !repo.join("rust/Cargo.toml").is_file() {
            return Ok(oms_command_landing::obj([
                ("rc", 0usize.into()),
                ("status", "not-applicable".into()),
            ]));
        }
        let sha = receipt
            .get("merge")
            .get("commit")
            .text()
            .filter(|s| !s.is_empty())
            .or_else(|| receipt.get("sha").text().filter(|s| !s.is_empty()))
            .filter(|s| s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .ok_or_else(|| {
                oms_command_landing::Error::new(
                    2,
                    "landing receipt missing verified 40-char commit SHA",
                )
            })?;
        let mut argv = if self.native.is_empty() {
            vec![self.source.join("scripts/oms").into()]
        } else {
            self.native.clone()
        };
        argv.push("update".into());
        argv.push("--ref".into());
        argv.push("edge".into());
        let spawn = Spawn::new(
            self.scope,
            argv,
            Duration::from_secs(600),
            COMMAND_OUTPUT_LIMIT,
        );
        let child = run_child(
            spawn.cwd(repo).environment(vec![
                ("OH_MY_SETTING_UPDATE_EXPECTED_TARGET".into(), sha.into()),
                ("OH_MY_SETTING_UPDATE_TARGET_COMMIT".into(), sha.into()),
            ]),
            interrupted,
        )
        .map_err(|e| oms_command_landing::Error::new(2, e))?;
        let rc = usize::try_from(child.code).unwrap_or(1);
        let mut result = oms_command_landing::obj([("rc", rc.into())]);
        if child.code != 0 {
            let summary = String::from_utf8_lossy(&child.stderr).trim().to_owned();
            if !summary.is_empty() {
                result.set("stderr", summary);
            }
        }
        Ok(result)
    }
}

impl oms_command_state::HostServices for ControlServices<'_> {
    fn plan_services(&self) -> &(dyn oms_command_plan::Services + Sync) {
        self
    }
    /// Scoped to the exact repository (`--repo`) and run with the full native
    /// task services, so the state projection reads the same reference
    /// ownership the task command itself would use.
    fn task_status(
        &self,
        repo: &Path,
        interrupted: &AtomicBool,
    ) -> Option<oms_command_state::Output> {
        let mut task = self.task();
        Some(oms_command_task::run_with_services(
            &task_status_arguments(repo),
            repo,
            interrupted,
            &mut task,
        ))
    }
    fn gc_landing_captures(
        &self,
        repo: &Path,
        days: u64,
        apply: bool,
    ) -> oms_command_state::Result<oms_command_state::GcHookResult> {
        gc_landing_captures_native(repo, days, apply)
    }
    fn gc_dispatch_logs(
        &self,
        repo: &Path,
        days: u64,
        apply: bool,
    ) -> oms_command_state::Result<oms_command_state::GcHookResult> {
        gc_dispatch_logs_native(repo, days, apply)
    }
}

fn state_err(code: i32, message: impl Into<String>) -> oms_command_state::Error {
    oms_command_state::Error {
        code,
        message: message.into(),
    }
}

fn cutoff_epoch(days: u64) -> oms_command_state::Result<i64> {
    let seconds = i64::try_from(days)
        .ok()
        .and_then(|d| d.checked_mul(86_400))
        .ok_or_else(|| state_err(2, "retention days out of range"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| state_err(1, "system clock before unix epoch"))?;
    Ok((now.as_secs() as i64).saturating_sub(seconds))
}

fn is_older_than(meta: &std::fs::Metadata, cutoff: i64) -> bool {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|d| (d.as_secs() as i64) < cutoff)
}

fn no_symlink_metadata(path: &Path) -> oms_command_state::Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(state_err(
            2,
            format!("refusing symlinked path: {}", path.display()),
        )),
        Ok(meta) => Ok(Some(meta)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(state_err(1, err.to_string())),
    }
}

fn validate_gc_repo(repo: &Path) -> oms_command_state::Result<PathBuf> {
    let oms_dir = repo.join(".oms");
    if let Some(meta) = no_symlink_metadata(&oms_dir)?
        && !meta.is_dir()
    {
        return Err(state_err(2, "refusing non-directory .oms state"));
    }
    std::fs::canonicalize(repo).map_err(|e| state_err(2, e.to_string()))
}

fn read_bounded_regular(
    path: &Path,
    max_bytes: usize,
) -> oms_command_state::Result<Option<Vec<u8>>> {
    let Some(meta) = no_symlink_metadata(path)? else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Err(state_err(
            2,
            format!("refusing non-regular file: {}", path.display()),
        ));
    }
    if meta.len() > max_bytes as u64 {
        return Err(state_err(
            2,
            format!("state file exceeds safety bound: {}", path.display()),
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| state_err(1, e.to_string()))?;
    if bytes.len() > max_bytes {
        return Err(state_err(
            2,
            format!("state file exceeds safety bound: {}", path.display()),
        ));
    }
    Ok(Some(bytes))
}

fn acquire_gc_locks(
    targets: &[(&str, PathBuf)],
    prefix: &str,
) -> oms_command_state::Result<Result<Vec<oms_lock::Guard>, String>> {
    let settings =
        oms_lock::Settings::from_env().map_err(|e| state_err(e.exit_code(), e.to_string()))?;
    let mut guards = Vec::with_capacity(targets.len());
    for (label, target) in targets {
        let lock = oms_lock::Lock::for_file(target, settings.clone())
            .map_err(|e| state_err(e.exit_code(), e.to_string()))?;
        match lock.try_acquire() {
            Ok(guard) => guards.push(guard),
            Err(oms_lock::Error::Busy) => {
                drop(guards);
                return Ok(Err(format!("{prefix}: {label} lock busy")));
            }
            Err(err) => return Err(state_err(err.exit_code(), err.to_string())),
        }
    }
    Ok(Ok(guards))
}

fn local_hostname() -> String {
    #[cfg(target_os = "linux")]
    if let Ok(raw) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let name = raw.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    if let Ok(output) = std::process::Command::new("uname").arg("-n").output()
        && output.status.success()
    {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    String::new()
}

fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let stat_path = PathBuf::from(format!("/proc/{pid}/stat"));
        match std::fs::read_to_string(stat_path) {
            Ok(raw) => {
                let state = raw
                    .rsplit(") ")
                    .next()
                    .and_then(|rest| rest.as_bytes().first().copied());
                !matches!(state, Some(b'Z'))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => true,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        match std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        {
            Ok(out) if out.status.success() => true,
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
                stderr.contains("operation not permitted") || stderr.contains("eperm")
            }
            Err(_) => true,
        }
    }
}

fn has_live_delegation(repo: &Path) -> oms_command_state::Result<bool> {
    let dir = repo.join(".oms/delegations");
    let Some(meta) = no_symlink_metadata(&dir)? else {
        return Ok(false);
    };
    if !meta.is_dir() {
        return Err(state_err(2, "refusing non-directory .oms/delegations"));
    }
    let current_host = local_hostname();
    let entries = std::fs::read_dir(&dir).map_err(|e| state_err(1, e.to_string()))?;
    for (seen, entry) in entries.enumerate() {
        if seen >= 50_000 {
            return Err(state_err(2, "delegations directory exceeds entry bound"));
        }
        let entry = entry.map_err(|e| state_err(1, e.to_string()))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || !name_str.ends_with(".json") {
            continue;
        }
        let path = entry.path();
        let Ok(Some(bytes)) = read_bounded_regular(&path, 1024 * 1024) else {
            return Ok(true);
        };
        let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok(true);
        };
        if let Some(host) = parsed.get("host").and_then(Value::as_str)
            && !host.is_empty()
            && host != current_host
        {
            return Ok(true);
        }
        let Some(pid_u64) = parsed.get("pid").and_then(Value::as_u64) else {
            return Ok(true);
        };
        let Ok(pid) = u32::try_from(pid_u64) else {
            return Ok(true);
        };
        if pid == 0 || is_process_alive(pid) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_landings_rows(raw: &[u8]) -> oms_command_state::Result<()> {
    const KNOWN_EVENTS: &[&str] = &[
        "intent",
        "complete",
        "abandoned",
        "capture-reserved",
        "capture-terminal",
        "capture-release-ready",
        "capture-released",
    ];
    let text = std::str::from_utf8(raw)
        .map_err(|_| state_err(2, "malformed UTF-8 in .oms/landings.jsonl"))?;
    let mut intents: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(trimmed).map_err(|e| {
            state_err(
                2,
                format!(
                    "malformed JSON in .oms/landings.jsonl line {}: {e}",
                    idx + 1
                ),
            )
        })?;
        let Some(obj) = row.as_object() else {
            return Err(state_err(
                2,
                format!("landing row {} must be a JSON object", idx + 1),
            ));
        };
        let Some(landing_id) = obj
            .get("landing_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Err(state_err(
                2,
                format!("landing row {} has no landing_id", idx + 1),
            ));
        };
        let Some(event) = obj.get("event").and_then(Value::as_str) else {
            return Err(state_err(2, format!("landing {landing_id} has no event")));
        };
        if !KNOWN_EVENTS.contains(&event) {
            return Err(state_err(
                2,
                format!("landing {landing_id} has unknown event {event:?}"),
            ));
        }
        match event {
            "intent" => {
                let Some(patch) = obj
                    .get("patch")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    return Err(state_err(
                        2,
                        format!("landing intent {landing_id} has no patch"),
                    ));
                };
                if let Some(prev) = intents.get(landing_id)
                    && prev != patch
                {
                    return Err(state_err(
                        2,
                        format!("landing {landing_id} has conflicting intent paths"),
                    ));
                }
                intents.insert(landing_id.to_string(), patch.to_string());
            }
            "complete" | "abandoned" if !intents.contains_key(landing_id) => {
                return Err(state_err(
                    2,
                    format!("landing {landing_id} is terminal without an intent"),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn gc_landing_captures_native(
    repo: &Path,
    days: u64,
    apply: bool,
) -> oms_command_state::Result<oms_command_state::GcHookResult> {
    if let Err(msg) = oms_reference::producer::parent_guard() {
        return Ok(oms_command_state::GcHookResult::Deferred(msg));
    }
    let cutoff = cutoff_epoch(days)?;
    let repo = validate_gc_repo(repo)?;
    let locks = [
        ("landings", repo.join(".oms/landings.jsonl")),
        (
            "marker-set",
            repo.join(".oms/delegations/.marker-set-lock-target"),
        ),
        ("plan", repo.join(".oms/plan/tasks.json")),
        ("lifecycle", repo.join(".oms/lifecycle/events.jsonl")),
        ("artifacts", repo.join(".oms/artifacts/index.jsonl")),
    ];
    let _guards = match acquire_gc_locks(&locks, "landing capture retention deferred")? {
        Ok(guards) => guards,
        Err(reason) => return Ok(oms_command_state::GcHookResult::Deferred(reason)),
    };
    let landings_path = repo.join(".oms/landings.jsonl");
    if let Some(raw) = read_bounded_regular(&landings_path, 32 * 1024 * 1024)? {
        validate_landings_rows(&raw)?;
    }
    let patches_dir = repo.join(".oms/landing-patches");
    if let Some(meta) = no_symlink_metadata(&patches_dir)?
        && !meta.is_dir()
    {
        return Err(state_err(2, "refusing non-directory .oms/landing-patches"));
    }
    let index = oms_reference::capture::CaptureIndex::load(&repo).map_err(|e| state_err(2, e))?;
    index.check().map_err(|e| state_err(2, e))?;

    let mut reference_haystack = Vec::new();
    for rel in [
        ".oms/plan/tasks.json",
        ".oms/lifecycle/events.jsonl",
        ".oms/artifacts/index.jsonl",
    ] {
        if let Some(bytes) = read_bounded_regular(&repo.join(rel), 32 * 1024 * 1024)? {
            reference_haystack.extend_from_slice(&bytes);
            reference_haystack.push(b'\n');
        }
    }

    let released_ids: std::collections::BTreeSet<String> = index
        .rows()
        .iter()
        .filter(|row| row.get("event").text() == Some("capture-released"))
        .filter_map(|row| row.get("landing_id").text().map(str::to_string))
        .collect();

    let canonical_patches = std::fs::canonicalize(&patches_dir).ok();
    let mut reclaimed = Vec::new();
    for reservation in index.reservations() {
        let Some(landing_id) = reservation.get("landing_id").text() else {
            continue;
        };
        let Some(dir_str) = reservation.get("directory").text() else {
            continue;
        };
        if !released_ids.contains(landing_id) {
            continue;
        }
        if reference_haystack
            .windows(landing_id.len())
            .any(|w| w == landing_id.as_bytes())
        {
            continue;
        }
        let dir_path = PathBuf::from(dir_str);
        let Some(dir_meta) = no_symlink_metadata(&dir_path)? else {
            continue;
        };
        if !dir_meta.is_dir() {
            continue;
        }
        let Some(patches_root) = canonical_patches.as_ref() else {
            continue;
        };
        let Ok(canon_dir) = std::fs::canonicalize(&dir_path) else {
            continue;
        };
        if !canon_dir.starts_with(patches_root) || canon_dir == *patches_root {
            continue;
        }
        let mut dir_entries = Vec::new();
        let mut eligible = true;
        for entry in std::fs::read_dir(&dir_path).map_err(|e| state_err(1, e.to_string()))? {
            let entry = entry.map_err(|e| state_err(1, e.to_string()))?;
            let path = entry.path();
            let Some(meta) = no_symlink_metadata(&path)? else {
                continue;
            };
            if !meta.is_file() || !is_older_than(&meta, cutoff) {
                eligible = false;
                break;
            }
            dir_entries.push(path);
        }
        if !eligible || dir_entries.is_empty() {
            continue;
        }
        dir_entries.sort();
        for path in &dir_entries {
            let rel = path
                .strip_prefix(&repo)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            if apply {
                std::fs::remove_file(path).map_err(|e| state_err(1, e.to_string()))?;
            }
            reclaimed.push(("landing-capture".to_string(), rel));
        }
        if apply {
            let _ = std::fs::remove_dir(&dir_path);
        }
    }
    reclaimed.sort();
    Ok(oms_command_state::GcHookResult::Reclaimed(reclaimed))
}

fn collect_task_protected_tokens(value: &Value, tokens: &mut std::collections::BTreeSet<String>) {
    const PROTECTED_KEYS: &[&str] = &[
        "id",
        "task",
        "task_id",
        "attempt",
        "attempt_id",
        "call_id",
        "dispatch_log",
        "log",
        "artifact",
    ];
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if PROTECTED_KEYS.contains(&k.as_str())
                    && let Some(s) = v.as_str()
                    && !s.is_empty()
                {
                    tokens.insert(s.to_string());
                } else {
                    collect_task_protected_tokens(v, tokens);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_task_protected_tokens(item, tokens);
            }
        }
        _ => {}
    }
}

fn collect_protected_dispatch_tokens(
    repo: &Path,
) -> oms_command_state::Result<std::collections::BTreeSet<String>> {
    const KNOWN_STATES: &[&str] = &[
        "ready",
        "claimed",
        "running",
        "review",
        "landing",
        "blocked",
        "done",
        "cancelled",
    ];
    const ACTIVE_STATES: &[&str] = &["claimed", "running", "review", "landing", "blocked"];

    let mut tokens = std::collections::BTreeSet::new();
    let tasks_path = repo.join(".oms/plan/tasks.json");
    if let Some(raw) = read_bounded_regular(&tasks_path, 32 * 1024 * 1024)? {
        let root: Value = serde_json::from_slice(&raw)
            .map_err(|e| state_err(2, format!("malformed .oms/plan/tasks.json: {e}")))?;
        let task_list: Vec<&Value> = match &root {
            Value::Array(items) => items.iter().collect(),
            Value::Object(map) => match map.get("tasks") {
                Some(Value::Array(items)) => items.iter().collect(),
                Some(_) => {
                    return Err(state_err(
                        2,
                        "malformed .oms/plan/tasks.json: tasks must be an array",
                    ));
                }
                None => Vec::new(),
            },
            _ => return Err(state_err(2, "malformed .oms/plan/tasks.json")),
        };
        for task in task_list {
            let Some(obj) = task.as_object() else {
                return Err(state_err(
                    2,
                    "malformed .oms/plan/tasks.json: task entry must be an object",
                ));
            };
            let state = obj.get("state").and_then(Value::as_str).ok_or_else(|| {
                state_err(2, "malformed .oms/plan/tasks.json: task missing state")
            })?;
            if !KNOWN_STATES.contains(&state) {
                return Err(state_err(
                    2,
                    format!("malformed .oms/plan/tasks.json: unknown task state {state:?}"),
                ));
            }
            if ACTIVE_STATES.contains(&state) {
                collect_task_protected_tokens(task, &mut tokens);
            }
        }
    }

    let events_path = repo.join(".oms/lifecycle/events.jsonl");
    if read_bounded_regular(&events_path, 32 * 1024 * 1024)?.is_some() {
        let (_, attempts) =
            oms_command_operations::load_projection(repo).map_err(|e| state_err(2, e))?;
        for attempt in attempts {
            let terminal = attempt
                .get("status")
                .text()
                .is_some_and(|s| oms_command_operations::TERMINAL_STATES.contains(&s));
            if terminal {
                continue;
            }
            for key in ["attempt_id", "attempt", "task", "task_id", "call_id"] {
                if let Some(val) = attempt.get(key).text()
                    && !val.is_empty()
                {
                    tokens.insert(val.to_string());
                }
            }
        }
    }

    Ok(tokens)
}

fn json_bytes_protect(raw: &[u8], protected: &std::collections::BTreeSet<String>) -> bool {
    let Ok(val) = serde_json::from_slice::<Value>(raw) else {
        return true;
    };
    let mut embedded = std::collections::BTreeSet::new();
    collect_task_protected_tokens(&val, &mut embedded);
    if embedded.iter().any(|tok| protected.contains(tok)) {
        return true;
    }
    if val.get("state").and_then(Value::as_str).is_some_and(|s| {
        matches!(
            s,
            "claimed" | "running" | "review" | "landing" | "starting" | "working"
        )
    }) {
        return true;
    }
    if let Some(pid_u64) = val.get("pid").and_then(Value::as_u64)
        && let Ok(pid) = u32::try_from(pid_u64)
        && is_process_alive(pid)
    {
        let same_host = val
            .get("host")
            .and_then(Value::as_str)
            .is_none_or(|h| h.is_empty() || h == local_hostname());
        if same_host {
            return true;
        }
    }
    false
}

fn path_matches_protected(rel: &str, protected: &std::collections::BTreeSet<String>) -> bool {
    protected
        .iter()
        .any(|tok| !tok.is_empty() && rel.contains(tok.as_str()))
}

fn gc_dispatch_logs_native(
    repo: &Path,
    days: u64,
    apply: bool,
) -> oms_command_state::Result<oms_command_state::GcHookResult> {
    let cutoff = cutoff_epoch(days)?;
    let repo = validate_gc_repo(repo)?;
    if has_live_delegation(&repo)? {
        return Ok(oms_command_state::GcHookResult::Deferred(
            "dispatch log retention deferred: active delegations present".into(),
        ));
    }
    let locks = [
        (
            "marker-set",
            repo.join(".oms/delegations/.marker-set-lock-target"),
        ),
        ("plan", repo.join(".oms/plan/tasks.json")),
        ("lifecycle", repo.join(".oms/lifecycle/events.jsonl")),
    ];
    let _guards = match acquire_gc_locks(&locks, "dispatch log retention deferred")? {
        Ok(guards) => guards,
        Err(reason) => return Ok(oms_command_state::GcHookResult::Deferred(reason)),
    };
    if has_live_delegation(&repo)? {
        return Ok(oms_command_state::GcHookResult::Deferred(
            "dispatch log retention deferred: active delegations present".into(),
        ));
    }

    let protected = collect_protected_dispatch_tokens(&repo)?;
    let mut reclaimed = Vec::new();
    let mut delete_paths = Vec::new();

    for (dir_rel, suffixes) in [
        (
            ".oms/panel-state/dispatch-logs",
            [".log", ".stream", ".transcript", ".activity.json"].as_slice(),
        ),
        (
            ".oms/artifacts/panel-dispatch",
            ["-process.json"].as_slice(),
        ),
    ] {
        let dir = repo.join(dir_rel);
        let Some(meta) = no_symlink_metadata(&dir)? else {
            continue;
        };
        if !meta.is_dir() {
            return Err(state_err(2, format!("refusing non-directory {dir_rel}")));
        }
        let entries = std::fs::read_dir(&dir).map_err(|e| state_err(1, e.to_string()))?;
        for (seen, entry) in entries.enumerate() {
            if seen >= 50_000 {
                return Err(state_err(2, format!("{dir_rel} exceeds entry bound")));
            }
            let entry = entry.map_err(|e| state_err(1, e.to_string()))?;
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !suffixes.iter().any(|suf| name_str.ends_with(suf)) {
                continue;
            }
            let Some(fmeta) = no_symlink_metadata(&path)? else {
                continue;
            };
            if !fmeta.is_file() || !is_older_than(&fmeta, cutoff) {
                continue;
            }
            let rel = format!("{dir_rel}/{name_str}");
            if path_matches_protected(&rel, &protected) {
                continue;
            }
            if name_str.ends_with(".json")
                && let Some(bytes) = read_bounded_regular(&path, 8 * 1024 * 1024)?
                && json_bytes_protect(&bytes, &protected)
            {
                continue;
            }
            reclaimed.push(("dispatch-logs".to_string(), rel));
            delete_paths.push(path);
        }
    }

    let artifacts_dir = repo.join(".oms/artifacts");
    if let Some(meta) = no_symlink_metadata(&artifacts_dir)? {
        if !meta.is_dir() {
            return Err(state_err(2, "refusing non-directory .oms/artifacts"));
        }
        let mut bases: std::collections::BTreeMap<String, PathBuf> =
            std::collections::BTreeMap::new();
        let mut settled: std::collections::BTreeMap<String, PathBuf> =
            std::collections::BTreeMap::new();
        let entries = std::fs::read_dir(&artifacts_dir).map_err(|e| state_err(1, e.to_string()))?;
        for (seen, entry) in entries.enumerate() {
            if seen >= 50_000 {
                return Err(state_err(2, ".oms/artifacts exceeds entry bound"));
            }
            let entry = entry.map_err(|e| state_err(1, e.to_string()))?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let Some(rest) = name_str.strip_prefix("panel-call-") else {
                continue;
            };
            if !name_str.ends_with(".json") {
                continue;
            }
            if let Some(id) = rest.strip_suffix("-settled.json") {
                if !id.is_empty() {
                    settled.insert(id.to_string(), entry.path());
                }
            } else if let Some(id) = rest.strip_suffix(".json")
                && !id.is_empty()
            {
                bases.insert(id.to_string(), entry.path());
            }
        }
        for (id, settled_path) in settled {
            let Some(smeta) = no_symlink_metadata(&settled_path)? else {
                continue;
            };
            if !smeta.is_file() || !is_older_than(&smeta, cutoff) {
                continue;
            }
            let settled_rel = format!(".oms/artifacts/panel-call-{id}-settled.json");
            if path_matches_protected(&settled_rel, &protected) {
                continue;
            }
            if let Some(bytes) = read_bounded_regular(&settled_path, 8 * 1024 * 1024)?
                && json_bytes_protect(&bytes, &protected)
            {
                continue;
            }
            if let Some(base_path) = bases.get(&id) {
                let Some(bmeta) = no_symlink_metadata(base_path)? else {
                    continue;
                };
                if !bmeta.is_file() || !is_older_than(&bmeta, cutoff) {
                    continue;
                }
                let base_rel = format!(".oms/artifacts/panel-call-{id}.json");
                if path_matches_protected(&base_rel, &protected) {
                    continue;
                }
                if let Some(bytes) = read_bounded_regular(base_path, 8 * 1024 * 1024)?
                    && json_bytes_protect(&bytes, &protected)
                {
                    continue;
                }
                reclaimed.push(("panel-call-artifact".to_string(), base_rel));
                delete_paths.push(base_path.clone());
            }
            reclaimed.push(("panel-call-artifact".to_string(), settled_rel));
            delete_paths.push(settled_path);
        }
    }

    if apply {
        for path in delete_paths {
            std::fs::remove_file(&path).map_err(|e| state_err(1, e.to_string()))?;
        }
    }
    reclaimed.sort();
    Ok(oms_command_state::GcHookResult::Reclaimed(reclaimed))
}

#[cfg(test)]
mod gc_host_tests {
    use super::*;
    use oms_command_state::{GcHookResult, HostServices as _};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn age_file(path: &Path, days_ago: u64) {
        let target = SystemTime::now()
            .checked_sub(Duration::from_secs(days_ago * 86_400 + 60))
            .unwrap_or(UNIX_EPOCH);
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open for mtime")
            .set_modified(target)
            .expect("set mtime");
    }

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oms-gc-host-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".oms")).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn gc_landing_captures_rejects_malformed_jsonl_and_preserves_unproven_patches() {
        let repo = temp_repo("landing");
        std::fs::create_dir_all(repo.join(".oms/landing-patches")).unwrap();
        let legacy_patch = repo.join(".oms/landing-patches/land-123.patch");
        std::fs::write(&legacy_patch, b"diff --git a/x b/x\n").unwrap();
        age_file(&legacy_patch, 60);

        let stopped = Arc::new(AtomicBool::new(false));
        let host = ControlServices::new(&stopped, &repo).unwrap();

        // Empty/valid landings ledger preserves unproven legacy land-*.patch.
        std::fs::write(repo.join(".oms/landings.jsonl"), b"").unwrap();
        let ok = host.gc_landing_captures(&repo, 30, true).unwrap();
        assert_eq!(ok, GcHookResult::Reclaimed(vec![]));
        assert!(legacy_patch.exists());

        // Malformed row fails closed with exit code 2.
        std::fs::write(repo.join(".oms/landings.jsonl"), b"{not json}\n").unwrap();
        let err = host.gc_landing_captures(&repo, 30, false).unwrap_err();
        assert_eq!(err.code, 2);

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn gc_dispatch_logs_defers_protects_active_and_reclaims_aged_settled() {
        let repo = temp_repo("dispatch");
        let logs_dir = repo.join(".oms/panel-state/dispatch-logs");
        let artifacts_dir = repo.join(".oms/artifacts");
        let delegations_dir = repo.join(".oms/delegations");
        let plan_dir = repo.join(".oms/plan");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        std::fs::create_dir_all(&delegations_dir).unwrap();
        std::fs::create_dir_all(&plan_dir).unwrap();

        std::fs::write(
            plan_dir.join("tasks.json"),
            br#"{"tasks":[{"id":"task-active","state":"running"},{"id":"task-done","state":"done"}]}"#,
        )
        .unwrap();

        let active_log = logs_dir.join("task-active.log");
        let old_log = logs_dir.join("task-done.log");
        let unsettled = artifacts_dir.join("panel-call-open.json");
        let settled_base = artifacts_dir.join("panel-call-done.json");
        let settled_mark = artifacts_dir.join("panel-call-done-settled.json");
        for path in [
            &active_log,
            &old_log,
            &unsettled,
            &settled_base,
            &settled_mark,
        ] {
            std::fs::write(path, b"{}\n").unwrap();
            age_file(path, 60);
        }

        let stopped = Arc::new(AtomicBool::new(false));
        let host = ControlServices::new(&stopped, &repo).unwrap();

        // Live delegation marker defers retention.
        let marker = delegations_dir.join("active.json");
        std::fs::write(&marker, format!("{{\"pid\":{}}}\n", std::process::id())).unwrap();
        let deferred = host.gc_dispatch_logs(&repo, 30, true).unwrap();
        assert!(matches!(deferred, GcHookResult::Deferred(_)));
        assert!(old_log.exists());
        std::fs::remove_file(&marker).unwrap();

        // Lock contention defers retention.
        let settings = oms_lock::Settings::from_env().unwrap();
        let guard = oms_lock::Lock::for_file(&repo.join(".oms/plan/tasks.json"), settings)
            .unwrap()
            .try_acquire()
            .unwrap();
        let busy = host.gc_dispatch_logs(&repo, 30, true).unwrap();
        assert!(matches!(busy, GcHookResult::Deferred(_)));
        drop(guard);

        // Dry run reports without deleting.
        let dry = host.gc_dispatch_logs(&repo, 30, false).unwrap();
        assert_eq!(
            dry,
            GcHookResult::Reclaimed(vec![
                (
                    "dispatch-logs".to_string(),
                    ".oms/panel-state/dispatch-logs/task-done.log".to_string(),
                ),
                (
                    "panel-call-artifact".to_string(),
                    ".oms/artifacts/panel-call-done-settled.json".to_string(),
                ),
                (
                    "panel-call-artifact".to_string(),
                    ".oms/artifacts/panel-call-done.json".to_string(),
                ),
            ])
        );
        assert!(old_log.exists());
        assert!(settled_base.exists());
        assert!(settled_mark.exists());

        // Apply reclaims only aged settled/unprotected artifacts and preserves active/unsettled ones.
        let applied = host.gc_dispatch_logs(&repo, 30, true).unwrap();
        assert_eq!(dry, applied);
        assert!(!old_log.exists());
        assert!(!settled_base.exists());
        assert!(!settled_mark.exists());
        assert!(active_log.exists());
        assert!(unsettled.exists());

        let _ = std::fs::remove_dir_all(&repo);
    }
}
fn task_status_arguments(repo: &Path) -> Vec<OsString> {
    vec![
        "status".into(),
        "--json".into(),
        "--repo".into(),
        repo.as_os_str().to_owned(),
    ]
}

fn scan_arguments(request: &PublicationScan) -> Result<Vec<OsString>, String> {
    let safe = |value: &str| {
        !value.is_empty() && !value.starts_with('-') && value.bytes().all(|b| b.is_ascii_graphic())
    };
    if !safe(&request.base) || !safe(&request.head) {
        return Err("publication scan revisions must be plain Git revisions".into());
    }
    let wall = u64::try_from(request.wall.as_millis()).map_err(|_| "scan wall overflow")?;
    let mut args: Vec<OsString> = vec![
        request.repo.as_os_str().to_owned(),
        request.base.clone().into(),
        request.head.clone().into(),
    ];
    args.extend(
        [
            wall,
            request.max_objects,
            request.max_object_bytes,
            request.max_total_bytes,
            request.memory_bytes,
        ]
        .map(|n| OsString::from(n.to_string())),
    );
    Ok(args)
}

/// Entry point of the dedicated scan worker process: `args` are the words after
/// [`SCAN_WORKER_FLAG`]. Never call this from a process that keeps running
/// other work; it permanently lowers the address-space limit.
pub(crate) fn scan_worker_main(args: &[OsString], interrupted: &AtomicBool) -> i32 {
    let report = |code: i32, message: &str| {
        eprintln!("{message}");
        code
    };
    let [repo, base, head, wall, objects, one, total, memory] = args else {
        return report(2, "publication scan worker: invalid request");
    };
    let number = |v: &OsString| v.to_str().and_then(|s| s.parse::<u64>().ok());
    let (Some(wall), Some(max_objects), Some(max_object_bytes), Some(max_total_bytes)) =
        (number(wall), number(objects), number(one), number(total))
    else {
        return report(2, "publication scan worker: invalid limits");
    };
    let Some(memory_bytes) = number(memory) else {
        return report(2, "publication scan worker: invalid limits");
    };
    let (Some(base), Some(head)) = (base.to_str(), head.to_str()) else {
        return report(2, "publication scan worker: invalid revisions");
    };
    let request = PublicationScan {
        repo: PathBuf::from(repo),
        base: base.into(),
        head: head.into(),
        wall: Duration::from_millis(wall),
        max_objects,
        max_object_bytes,
        max_total_bytes,
        memory_bytes,
    };
    if scan_arguments(&request).is_err() {
        return report(2, "publication scan worker: invalid revisions");
    }
    match oms_command_landing::review::scan_worker(&request, interrupted) {
        Ok(()) => 0,
        Err(e) => report(e.code.clamp(1, 255), &e.message),
    }
}

/// Native task producers: reference ownership, journal, memory, fail-ledger,
/// verify observations and skill-forge contract hints.
pub(crate) struct ControlTask<'a> {
    interrupted: &'a Arc<AtomicBool>,
    source: &'a Path,
}

fn task_error(message: impl AsRef<str>) -> oms_command_task::Error {
    oms_command_task::Error::new(1, message)
}

impl oms_command_task::Services for ControlTask<'_> {
    fn state_repository(
        &mut self,
        work: &Path,
        _interrupted: &AtomicBool,
    ) -> oms_command_task::Result<PathBuf> {
        state_repository_for(work).map_err(task_error)
    }
    fn begin_reference(
        &mut self,
        repo: &Path,
        target: &Path,
    ) -> oms_command_task::Result<Box<dyn ReferenceOperation>> {
        oms_reference::scope::native_task_operation(repo, target, Arc::clone(self.interrupted))
    }
    fn observe_journal(&mut self, request: JournalObservation<'_>) -> oms_command_task::Result<()> {
        let mut args: Vec<OsString> = vec![
            "observe".into(),
            "--repo".into(),
            request.repo.as_os_str().to_owned(),
            "--source-type".into(),
            "agent-task".into(),
            "--source-file".into(),
            request.path.as_os_str().to_owned(),
            "--operation".into(),
            request.operation.as_str().into(),
        ];
        if let Some(status) = request.verification {
            args.push("--verification-status".into());
            args.push(
                match status {
                    VerificationStatus::Passed => "passed",
                    VerificationStatus::NotVerified => "not_verified",
                }
                .into(),
            );
        }
        let (code, _, stderr) = oms_command_journal::run_with_source(
            &args,
            request.repo,
            self.source,
            self.interrupted,
        );
        if code == 0 {
            Ok(())
        } else {
            Err(oms_command_task::Error::new(
                code,
                format!(
                    "journal observer refused: {}",
                    String::from_utf8_lossy(&stderr).trim()
                ),
            ))
        }
    }
    fn promote_memory(&mut self, request: MemoryOutcome<'_>) -> oms_command_task::Result<()> {
        let (Ok(agent), Ok(text)) = (
            std::str::from_utf8(request.agent),
            std::str::from_utf8(request.text),
        ) else {
            return Err(task_error("memory outcome must be UTF-8"));
        };
        let (code, _, stderr) = oms_command_memory::append_project_note(
            request.repo,
            agent,
            request.source,
            text,
            self.interrupted,
        );
        if code == 0 {
            Ok(())
        } else {
            Err(oms_command_task::Error::new(
                code,
                String::from_utf8_lossy(&stderr).trim(),
            ))
        }
    }
    fn observe_failure(
        &mut self,
        request: FailureObservation<'_>,
    ) -> oms_command_task::Result<Vec<u8>> {
        let command = OsString::from_vec(request.command.to_vec());
        let mut args: Vec<OsString> = vec!["--repo".into(), request.repo.as_os_str().to_owned()];
        if request.exit == 0 {
            // A passing gate clears its own row; the shell ignored this result.
            args.extend(["resolve".into(), "--cmd".into(), command]);
            let _ = oms_command_state::run("fail-ledger", &args, request.repo, self.interrupted);
            return Ok(Vec::new());
        }
        args.extend([
            "record".into(),
            "--kind".into(),
            "verify".into(),
            "--cmd".into(),
            command,
            "--exit".into(),
            request.exit.to_string().into(),
        ]);
        if !request.summary.is_empty() {
            args.push("--summary".into());
            args.push(OsString::from_vec(request.summary.to_vec()));
        }
        let (code, stdout, stderr) =
            oms_command_state::run("fail-ledger", &args, request.repo, self.interrupted);
        if code != 0 {
            return Err(task_error("fail-ledger did not record the failure"));
        }
        // Only the advisor line is surfaced; the rest is bookkeeping.
        let mut advisory = Vec::new();
        for line in [stdout, stderr].concat().split(|b| *b == b'\n') {
            if line.windows(10).any(|w| w == b"oms advise") {
                advisory.extend_from_slice(line);
                advisory.push(b'\n');
            }
        }
        Ok(advisory)
    }
    fn observe_verify(
        &mut self,
        repo: &Path,
        output: &[u8],
    ) -> oms_command_task::Result<VerifyObservation> {
        let runners: Vec<String> = oms_command_task::observe::scan_text(output)
            .into_keys()
            .collect();
        let interrupted = self.interrupted;
        let fingerprint = oms_command_task::observe::fingerprint(repo, &runners, |tool| {
            // Version probes: fixed names, five-second deadline, stdin closed,
            // cwd `/`; unavailable means no version rather than a guess.
            let spawn = Spawn::new(
                Scope::ProcessGroup,
                vec![tool.into(), "--version".into()],
                Duration::from_secs(5),
                PROBE_OUTPUT_LIMIT,
            );
            let probe = run_child(spawn.cwd(Path::new("/")), interrupted)
                .ok()
                .filter(|c| c.code == 0)?;
            oms_command_task::observe::version(&probe.stdout, &probe.stderr)
        })?;
        Ok(oms_command_task::observe::note_lines(output, &fingerprint))
    }
    fn skill_contracts(
        &mut self,
        repo: &Path,
    ) -> oms_command_task::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let (code, stdout, _) = oms_command_providers_skills::run_with_source(
            "skill-forge",
            &["--repo".into(), repo.into(), "contracts".into()],
            repo,
            self.source,
            self.interrupted,
        );
        if code != 0 {
            return Err(task_error("skill-forge contracts unavailable"));
        }
        Ok(stdout
            .split(|b| *b == b'\n')
            .filter_map(|row| {
                row.iter()
                    .position(|b| *b == b'\t')
                    .map(|i| row.split_at(i))
            })
            .filter(|(name, _)| !name.is_empty())
            .map(|(name, rest)| (name.to_vec(), rest[1..].to_vec()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oms_command_autonomy::Services as _;
    use oms_command_graph::HostServices as _;
    use oms_command_landing::Services as _;
    use oms_command_plan::Services as _;
    use std::process::Command;
    use std::sync::atomic::AtomicU32;

    static NEXT: AtomicU32 = AtomicU32::new(0);
    /// Scratch directory under TMPDIR, removed on drop.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "oms-control-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(std::fs::canonicalize(dir).unwrap())
        }
        fn path(&self) -> &Path {
            &self.0
        }
        /// A committed Git repository holding `files`.
        fn repo(files: &[(&str, &str)]) -> Self {
            let tmp = Self::new();
            git(tmp.path(), &["init", "-q", "-b", "main"]);
            for (name, body) in files {
                let target = tmp.path().join(name);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(target, body).unwrap();
            }
            commit(tmp.path(), "base");
            tmp
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out);
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    fn commit(dir: &Path, message: &str) -> String {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }
    fn sh(script: &str) -> Vec<OsString> {
        vec!["/bin/sh".into(), "-c".into(), script.into(), "sh".into()]
    }
    fn services<'a>(
        stop: &'a Arc<AtomicBool>,
        root: &Path,
        native: Vec<OsString>,
    ) -> ControlServices<'a> {
        ControlServices::with_launchers(stop, root.to_owned(), native, Vec::new())
    }
    fn address_space_limit() -> (Option<u64>, Option<u64>) {
        let limit = rustix::process::getrlimit(rustix::process::Resource::As);
        (limit.current, limit.maximum)
    }

    /// Child half of the scan-worker tests: the test binary re-executes itself
    /// with `--ignored` and the worker request as the trailing words.
    #[test]
    #[ignore]
    fn scan_worker_child_entry() {
        let args: Vec<OsString> = std::env::args_os().rev().take(8).collect();
        let args: Vec<OsString> = args.into_iter().rev().collect();
        let stop = Arc::new(AtomicBool::new(false));
        std::process::exit(scan_worker_main(&args, &stop));
    }
    fn self_scan_launcher() -> Vec<OsString> {
        let exe = std::env::current_exe().unwrap();
        vec![
            exe.into(),
            "services_control::tests::scan_worker_child_entry".into(),
            "--exact".into(),
            "--ignored".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ]
    }
    fn scan_request(repo: &Path, base: &str, head: &str, memory: u64) -> PublicationScan {
        PublicationScan {
            repo: repo.to_owned(),
            base: base.into(),
            head: head.into(),
            wall: Duration::from_secs(60),
            max_objects: 1000,
            max_object_bytes: 1 << 20,
            max_total_bytes: 1 << 24,
            memory_bytes: memory,
        }
    }

    #[test]
    fn publication_scan_runs_in_child_and_keeps_parent_limit() {
        let repo = Tmp::repo(&[("a.txt", "one\n")]);
        let base = git(repo.path(), &["rev-parse", "HEAD"]);
        std::fs::write(repo.path().join("b.txt"), "two\n").unwrap();
        let head = commit(repo.path(), "clean");
        let stop = Arc::new(AtomicBool::new(false));
        let mut control = services(&stop, repo.path(), Vec::new());
        control.scan_worker = self_scan_launcher();
        let before = address_space_limit();
        control
            .publication_scan(&scan_request(repo.path(), &base, &head, 4 << 30), &stop)
            .expect("clean range passes");
        assert_eq!(before, address_space_limit());
        // A starved address space cannot pass: the worker fails closed.
        assert!(
            control
                .publication_scan(&scan_request(repo.path(), &base, &head, 1 << 20), &stop)
                .is_err()
        );
        assert_eq!(before, address_space_limit());
    }

    #[test]
    fn publication_scan_refuses_sensitive_payload_and_bad_requests() {
        let repo = Tmp::repo(&[("a.txt", "one\n")]);
        let base = git(repo.path(), &["rev-parse", "HEAD"]);
        let marker = ["---", "--BEG", "IN RSA PRI", "VATE K", "EY-----\n"].concat();
        std::fs::write(repo.path().join("k.txt"), marker).unwrap();
        let head = commit(repo.path(), "leak");
        let stop = Arc::new(AtomicBool::new(false));
        let mut control = services(&stop, repo.path(), Vec::new());
        // No launcher configured: unavailable, never an in-process scan.
        let none =
            control.publication_scan(&scan_request(repo.path(), &base, &head, 4 << 30), &stop);
        assert!(none.unwrap_err().message.contains("unavailable"));
        control.scan_worker = self_scan_launcher();
        let leak = control
            .publication_scan(&scan_request(repo.path(), &base, &head, 4 << 30), &stop)
            .unwrap_err();
        assert_eq!(leak.code, 3, "{}", leak.message);
        for bad in ["", "-x", "a b"] {
            assert!(
                control
                    .publication_scan(&scan_request(repo.path(), bad, &head, 4 << 30), &stop)
                    .is_err(),
                "{bad:?}"
            );
        }
        let mut zero = scan_request(repo.path(), &base, &head, 4 << 30);
        zero.wall = Duration::ZERO;
        assert!(control.publication_scan(&zero, &stop).is_err());
    }

    #[test]
    fn publication_scan_enforces_deadline_and_cancellation() {
        let repo = Tmp::repo(&[("a.txt", "one\n")]);
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let stop = Arc::new(AtomicBool::new(false));
        let mut control = services(&stop, repo.path(), Vec::new());
        control.scan_worker = sh("sleep 20");
        let mut request = scan_request(repo.path(), &head, &head, 4 << 30);
        request.wall = Duration::from_secs(1);
        let started = Instant::now();
        let error = control.publication_scan(&request, &stop).unwrap_err();
        assert!(error.message.contains("deadline"), "{}", error.message);
        assert!(started.elapsed() < Duration::from_secs(15));
        // A nonzero worker exit is a refusal even with no output.
        control.scan_worker = sh("exit 9");
        assert!(
            control
                .publication_scan(&scan_request(repo.path(), &head, &head, 1), &stop)
                .is_err()
        );
        stop.store(true, Ordering::Release);
        assert!(
            control
                .publication_scan(&scan_request(repo.path(), &head, &head, 1), &stop)
                .is_err()
        );
    }

    #[test]
    fn scan_worker_main_rejects_malformed_requests() {
        let stop = Arc::new(AtomicBool::new(false));
        let words = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(scan_worker_main(&[], &stop), 2);
        assert_eq!(
            scan_worker_main(&words(&["r", "b", "h", "x", "1", "1", "1", "1"]), &stop),
            2
        );
        assert_eq!(
            scan_worker_main(&words(&["r", "-b", "h", "1", "1", "1", "1", "1"]), &stop),
            2
        );
        assert_eq!(
            scan_worker_main(&words(&["r", "b", "h", "1", "1", "1", "1"]), &stop),
            2
        );
    }

    #[test]
    fn invoke_command_supervises_native_children() {
        let root = Tmp::new();
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(
            &stop,
            root.path(),
            sh(r#"printf '%s|' "$@"; printf '%s' "$CONTROL_PROBE" >&2; exit 3"#),
        );
        assert_eq!(control.install_root(), Some(root.path()));
        let env = [("CONTROL_PROBE".to_owned(), "ok".to_owned())];
        let (code, out, err, timed_out) = control
            .invoke_command(
                "goal-drive",
                &["a".into(), "b c".into()],
                root.path(),
                &env,
                Some(Duration::from_secs(20)),
                &stop,
            )
            .unwrap();
        assert_eq!((code, timed_out), (3, false));
        assert_eq!(out, b"goal-drive|a|b c|");
        assert_eq!(err, b"ok");
        // Deadline: reported as a timeout, whole tree reaped.
        let slow = services(&stop, root.path(), sh("sleep 20"));
        let started = Instant::now();
        let (code, _, _, timed_out) = slow
            .invoke_command(
                "x",
                &[],
                root.path(),
                &[],
                Some(Duration::from_secs(1)),
                &stop,
            )
            .unwrap();
        assert_eq!((code, timed_out), (124, true));
        assert!(started.elapsed() < Duration::from_secs(15));
        // Zero timeout, non-verbs and an unconfigured binary refuse.
        assert!(
            slow.invoke_command("x", &[], root.path(), &[], Some(Duration::ZERO), &stop)
                .is_err()
        );
        for tool in ["", "-x", "../x", "A", "a b", "1a"] {
            assert!(
                control
                    .invoke_command(tool, &[], root.path(), &[], None, &stop)
                    .is_err(),
                "{tool:?}"
            );
        }
        let unset = services(&stop, root.path(), Vec::new());
        assert!(
            unset
                .invoke_command("x", &[], root.path(), &[], None, &stop)
                .is_err()
        );
        stop.store(true, Ordering::Release);
        assert!(
            control
                .invoke_command("x", &[], root.path(), &[], None, &stop)
                .is_err()
        );
    }

    #[test]
    fn autonomy_invoke_and_supervise_use_native_supervision() {
        let root = Tmp::new();
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, root.path(), sh(r#"printf '%s' "$*"; exit 4"#));
        let command =
            oms_command_autonomy::Invocation::new("peer-ask", root.path(), vec!["x".into()]);
        let (code, out, _) =
            oms_command_autonomy::Services::invoke(&control, &command, &stop).unwrap();
        assert_eq!((code, out.as_slice()), (4, b"peer-ask x".as_slice()));
        assert!(
            services(&stop, root.path(), Vec::new())
                .invoke(&command, &stop)
                .is_err()
        );
        let supervised = oms_command_landing::Services::supervise(
            &control,
            &VerifyRequest {
                argv: sh(
                    "printf '%s' \"${OMS_NATIVE_OUTCOME:-}${OMS_NATIVE_DISPATCHED:-}out\"; printf err >&2; exit 3",
                ),
                cwd: root.path().to_owned(),
                wall: Duration::from_secs(20),
                environment: Vec::new(),
            },
            &stop,
        )
        .unwrap();
        assert_eq!(
            (supervised.code, supervised.stdout, supervised.stderr),
            (3, b"out".to_vec(), b"err".to_vec())
        );
        let plan = oms_command_plan::Services::supervise(
            &control,
            &SupervisedCommand {
                argv: sh("printf plan; exit 0"),
                cwd: root.path().to_owned(),
                wall: Duration::from_secs(20),
                output_limit: 1024,
                label: "t",
                remove_environment: Vec::new(),
                environment: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!((plan.exit, plan.output.as_slice()), (0, b"plan".as_slice()));
    }

    #[test]
    fn plan_and_autonomy_resolve_explicit_and_policy_routes() {
        let root = Tmp::repo(&[("a", "x")]);
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, root.path(), Vec::new());
        let route = |v: Value| control.resolve_route(root.path(), &v, "worker", &stop);
        let ok = route(serde_json::json!({"provider": "agy", "model": "m-1", "fallback_model": "m-0", "reasoning_effort": "high"})).unwrap();
        assert_eq!(
            (
                ok.provider.as_str(),
                ok.model_class.as_str(),
                ok.primary.as_str(),
                ok.fallback.as_str(),
                ok.effort.as_str()
            ),
            ("antigravity", "explicit", "m-1", "m-0", "high")
        );
        assert_eq!(
            route(serde_json::json!({"provider": "codex", "model": "m"}))
                .unwrap()
                .effort,
            "auto"
        );
        for bad in [
            serde_json::json!({"provider": "codex"}),
            serde_json::json!({"provider": "codex", "model": "provider-default"}),
            serde_json::json!({"provider": "codex", "model": "m", "fallback_model": "provider-default"}),
            serde_json::json!({"provider": "codex", "model": "m", "reasoning_effort": "extreme"}),
            serde_json::json!({"provider": "Bad Name", "model": "m"}),
            serde_json::json!({"provider": "codex:injected", "model": ""}),
            serde_json::json!({"provider": "codex", "model": "-m"}),
            serde_json::json!({"provider": "codex", "model": "m", "fallback_model": "-f"}),
        ] {
            assert!(route(bad.clone()).is_err(), "{bad}");
        }

        let policy_source = Tmp::repo(&[(
            "config/panel-routing.json",
            r#"{
              "schema": 2,
              "tiers": {
                "routine": [
                  {"provider": "codex", "model": "gpt-policy-1", "effort": "medium"}
                ],
                "light": [
                  {"provider": "pi", "model": "public-only-m", "effort": "low", "public_only": true},
                  {"provider": "claude", "model": "claude-light-1", "effort": ""}
                ]
              },
              "roles": {
                "worker": {"tier": "routine"},
                "researcher": {"tier": "light"}
              }
            }"#,
        )]);
        let policy_control = services(&stop, policy_source.path(), Vec::new());
        let policy_route = policy_control
            .resolve_route(
                policy_source.path(),
                &serde_json::json!({"provider": "codex", "reasoning_effort": "auto"}),
                "worker",
                &stop,
            )
            .unwrap();
        assert_eq!(policy_route.provider, "codex");
        assert_eq!(policy_route.model_class, "policy");
        assert_eq!(policy_route.primary, "gpt-policy-1");
        assert_eq!(policy_route.effort, "medium");

        let empty_effort_route = policy_control
            .resolve_route(
                policy_source.path(),
                &serde_json::json!({"provider": "claude"}),
                "researcher",
                &stop,
            )
            .unwrap();
        assert_eq!(empty_effort_route.model_class, "policy");
        assert_eq!(empty_effort_route.primary, "claude-light-1");
        assert_eq!(empty_effort_route.effort, "auto");

        // Missing or public-only-only tier entries never fall back to provider-default.
        assert!(
            policy_control
                .resolve_route(
                    policy_source.path(),
                    &serde_json::json!({"provider": "pi"}),
                    "researcher",
                    &stop,
                )
                .is_err()
        );
        assert!(
            policy_control
                .resolve_route(
                    policy_source.path(),
                    &serde_json::json!({"provider": "codex"}),
                    "unknown-role",
                    &stop,
                )
                .is_err()
        );

        stop.store(true, Ordering::Release);
        assert_eq!(
            route(serde_json::json!({"provider": "codex", "model": "m"}))
                .unwrap_err()
                .code,
            130
        );
        stop.store(false, Ordering::Release);
        assert_eq!(control.normalize_provider("agy").unwrap(), "antigravity");
        assert_eq!(control.normalize_provider("codex").unwrap(), "codex");
        assert!(control.normalize_provider("../x").is_err());
        assert!(control.normalize_provider("").is_err());
    }

    #[test]
    fn plan_services_delegate_lifecycle_and_state_root() {
        let repo = Tmp::repo(&[("a", "x")]);
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, repo.path(), Vec::new());
        assert!(
            control.state_root(repo.path()).unwrap().is_none()
                || std::env::var_os("OMS_STATE_REPO").is_some()
        );
        let guard = control.lifecycle_writer(repo.path()).unwrap();
        drop(guard);
        let projected = control.project_lifecycle(b"").unwrap();
        assert_eq!(
            oms_command_plan::json::canonical(&projected.attempts).unwrap(),
            b"[]"
        );
        assert!(control.project_lifecycle(b"not json\n").is_err());
        assert!(control.group_membership_resolver().is_some());
        // No approval ledger entry: refused, never an empty grant.
        assert!(
            oms_command_plan::Services::landing_approval(&control, repo.path(), "apr-missing")
                .is_err()
        );
        assert!(oms_command_landing::Services::plan(&control).is_some());
        assert!(oms_command_landing::Services::admission_context(&control).is_none());
        let pub_guard =
            oms_command_landing::Services::publication_guard(&control, repo.path(), &stop).unwrap();
        let canonical = std::fs::canonicalize(repo.path()).unwrap();
        let mut settings = crate::services_runtime::resolve_lock_settings().unwrap();
        settings.cwd = canonical.clone();
        let contender =
            oms_lock::Lock::for_file(&canonical.join(".oms/locks/publication.lock"), settings)
                .unwrap();
        assert!(matches!(
            contender.try_acquire(),
            Err(oms_lock::Error::Busy)
        ));
        drop(pub_guard);
        let events_dir = canonical.join(".oms/lifecycle");
        std::fs::create_dir_all(&events_dir).unwrap();
        let review_events = concat!(
            "{\"actor\":{\"kind\":\"owner\",\"name\":\"agent-events\"},\"attempt_id\":\"att_review1\",\"base_sha\":\"0123456789abcdef0123456789abcdef01234567\",\"event_id\":\"aevt_1\",\"event_type\":\"attempt.created\",\"from_state\":null,\"provider\":\"claude\",\"refs\":{\"panel_role\":\"worker\"},\"run_id\":\"run_1\",\"schema\":1,\"seq\":1,\"task_id\":\"t1\",\"to_state\":\"queued\",\"tool\":\"peer-delegate\",\"ts\":\"2999-01-01T00:00:00Z\"}\n",
            "{\"actor\":{\"kind\":\"owner\",\"name\":\"provider-router\"},\"attempt_id\":\"att_review1\",\"event_id\":\"aevt_2\",\"event_type\":\"attempt.state_changed\",\"from_state\":\"queued\",\"idempotency_key\":\"k1\",\"schema\":1,\"seq\":2,\"to_state\":\"starting\",\"ts\":\"2999-01-01T00:00:00Z\"}\n",
            "{\"actor\":{\"kind\":\"owner\",\"name\":\"provider-router\"},\"attempt_id\":\"att_review1\",\"event_id\":\"aevt_3\",\"event_type\":\"attempt.state_changed\",\"from_state\":\"starting\",\"idempotency_key\":\"k2\",\"schema\":1,\"seq\":3,\"to_state\":\"working\",\"ts\":\"2999-01-01T00:00:00Z\"}\n",
            "{\"actor\":{\"kind\":\"owner\",\"name\":\"provider-router\"},\"attempt_id\":\"att_review1\",\"event_id\":\"aevt_4\",\"event_type\":\"attempt.state_changed\",\"from_state\":\"working\",\"idempotency_key\":\"k3\",\"schema\":1,\"seq\":4,\"to_state\":\"review\",\"ts\":\"2999-01-01T00:00:00Z\"}\n",
            "{\"actor\":{\"kind\":\"owner\",\"name\":\"agent-events\"},\"attempt_id\":\"att_stale1\",\"base_sha\":\"0123456789abcdef0123456789abcdef01234567\",\"event_id\":\"aevt_5\",\"event_type\":\"attempt.created\",\"from_state\":null,\"provider\":\"codex\",\"refs\":{\"panel_role\":\"delegate\"},\"run_id\":\"run_2\",\"schema\":1,\"seq\":1,\"task_id\":\"t2\",\"to_state\":\"queued\",\"tool\":\"peer-delegate\",\"ts\":\"2026-01-01T00:00:00Z\"}\n",
            "{\"actor\":{\"kind\":\"owner\",\"name\":\"provider-router\"},\"attempt_id\":\"att_stale1\",\"event_id\":\"aevt_6\",\"event_type\":\"attempt.state_changed\",\"from_state\":\"queued\",\"idempotency_key\":\"k4\",\"schema\":1,\"seq\":2,\"to_state\":\"starting\",\"ts\":\"2026-01-01T00:00:00Z\"}\n",
            "{\"actor\":{\"kind\":\"owner\",\"name\":\"provider-router\"},\"attempt_id\":\"att_stale1\",\"event_id\":\"aevt_7\",\"event_type\":\"attempt.state_changed\",\"from_state\":\"starting\",\"idempotency_key\":\"k5\",\"schema\":1,\"seq\":3,\"to_state\":\"working\",\"ts\":\"2026-01-01T00:00:00Z\"}\n",
        );
        std::fs::write(events_dir.join("events.jsonl"), review_events).unwrap();
        let (_, projected_rows) = oms_command_operations::load_projection(&canonical).unwrap();
        assert_eq!(projected_rows.len(), 2);
        assert!(
            oms_command_landing::Services::wait_siblings(
                &control,
                repo.path(),
                Duration::from_millis(50),
                &stop,
            )
            .is_ok()
        );
    }

    fn osv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }
    /// An initialised active packet written directly: `init` itself is refused
    /// inside a harness child, which would make the tests environment-dependent.
    fn packet(repo: &Path, id: &str) -> PathBuf {
        let path = repo.join(".oms/task/current.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let raw = oms_command_task::packet::ensure(
            Vec::new(),
            "2026-01-02T03:04:05Z",
            id,
            b"test-session",
        );
        std::fs::write(&path, raw).unwrap();
        path
    }

    #[test]
    fn task_status_is_scoped_to_the_exact_repository() {
        let repo = Tmp::repo(&[("a", "x")]);
        let other = Tmp::repo(&[("b", "y")]);
        packet(repo.path(), "task-scoped-1");
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, repo.path(), Vec::new());
        // Arguments carry --repo, so a caller whose working directory differs
        // from the selected repository still reads the selected one.
        assert_eq!(
            task_status_arguments(repo.path()),
            osv(&["status", "--json", "--repo", repo.path().to_str().unwrap()])
        );
        let (code, out, err) =
            oms_command_state::HostServices::task_status(&control, repo.path(), &stop).unwrap();
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert!(String::from_utf8_lossy(&out).contains("task-scoped-1"));
        let (_, out, _) =
            oms_command_state::HostServices::task_status(&control, other.path(), &stop).unwrap();
        assert!(!String::from_utf8_lossy(&out).contains("task-scoped-1"));
        // The state host seam is this adapter, serving the same plan services.
        let plan: &(dyn oms_command_plan::Services + Sync) =
            oms_command_state::HostServices::plan_services(&control);
        assert!(plan.group_membership_resolver().is_some());
    }

    #[test]
    fn task_reference_journal_and_memory_use_native_producers() {
        let repo = Tmp::repo(&[("a", "x")]);
        let path = packet(repo.path(), "task-ref-1");
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, repo.path(), Vec::new());
        let mut task = control.task();
        // A reference operation is a real lifecycle attempt: nonterminal until
        // finished, and an owned write validates against the capture index.
        let mut operation =
            oms_command_task::Services::begin_reference(&mut task, repo.path(), &path).unwrap();
        let terminal = |repo: &Path| {
            oms_command_operations::load_projection(repo)
                .unwrap()
                .1
                .iter()
                .all(|a| a.get("terminal") == &oms_command_operations::value::V::Bool(true))
        };
        assert!(!terminal(repo.path()));
        let before = std::fs::read(&path).unwrap();
        operation
            .validate(&oms_command_task::services::ReferenceWrite {
                repo: repo.path(),
                target: &path,
                operation: "update",
                arguments: &[b"plain".to_vec()],
                before: &before,
                proposed: &before,
                source_session: b"s",
            })
            .unwrap();
        operation.finish(0).unwrap();
        assert!(terminal(repo.path()));
        // A validation refusal settles as refuse-no-write without altering the target.
        let refused_repo = Tmp::repo(&[("a", "x")]);
        let refused_path = packet(refused_repo.path(), "task-ref-refuse");
        let refused_before = std::fs::read(&refused_path).unwrap();
        let mut refused_task = control.task();
        let mut refused = oms_command_task::Services::begin_reference(
            &mut refused_task,
            refused_repo.path(),
            &refused_path,
        )
        .unwrap();
        assert!(!terminal(refused_repo.path()));
        std::fs::write(
            refused_repo.path().join(".oms/landings.jsonl"),
            b"not-json\n",
        )
        .unwrap();
        assert!(
            refused
                .validate(&oms_command_task::services::ReferenceWrite {
                    repo: refused_repo.path(),
                    target: &refused_path,
                    operation: "update",
                    arguments: &[b"plain".to_vec()],
                    before: &refused_before,
                    proposed: &refused_before,
                    source_session: b"s",
                })
                .is_err()
        );
        assert!(terminal(refused_repo.path()));
        assert_eq!(std::fs::read(&refused_path).unwrap(), refused_before);
        // A failing finish leaves a blocked, still-unresolved attempt.
        let mut failed =
            oms_command_task::Services::begin_reference(&mut task, repo.path(), &path).unwrap();
        failed.finish(1).unwrap();
        assert!(!terminal(repo.path()));
        // Journal observation of the packet and memory promotion.
        oms_command_task::Services::observe_journal(
            &mut task,
            JournalObservation {
                repo: repo.path(),
                path: &path,
                operation: oms_command_task::services::JournalOperation::Update,
                verification: None,
            },
        )
        .unwrap();
        oms_command_task::Services::promote_memory(
            &mut task,
            MemoryOutcome {
                repo: repo.path(),
                agent: b"agent",
                text: b"Closed task: ship it\n",
                source: "task_close",
                task_id: b"task-ref-1",
                verification: VerificationStatus::Passed,
            },
        )
        .unwrap();
        let memory = std::fs::read_to_string(repo.path().join(".oms/memory/shared.md")).unwrap();
        assert!(memory.contains("Closed task: ship it"));
    }

    #[test]
    fn task_services_refuse_invalid_inputs() {
        let repo = Tmp::repo(&[("a", "x")]);
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, repo.path(), Vec::new());
        let mut task = control.task();
        // Reference writes outside any managed namespace validate; malformed
        // memory (non-UTF-8) and unreadable ledgers are refused, not skipped.
        let bad = MemoryOutcome {
            repo: repo.path(),
            agent: b"agent",
            text: &[0xff, 0xfe],
            source: "task_close",
            task_id: b"t",
            verification: VerificationStatus::Passed,
        };
        assert!(oms_command_task::Services::promote_memory(&mut task, bad).is_err());
        let missing = repo.path().join("nope/current.md");
        assert!(
            oms_command_task::Services::observe_journal(
                &mut task,
                JournalObservation {
                    repo: repo.path(),
                    path: &missing,
                    operation: oms_command_task::services::JournalOperation::Update,
                    verification: None,
                }
            )
            .is_err()
        );
        assert!(
            oms_command_task::Services::skill_contracts(&mut task, repo.path())
                .map(|rows| rows.is_empty())
                .unwrap_or(true)
        );
    }

    #[test]
    fn failure_and_verify_observations_use_native_ledgers() {
        let repo = Tmp::repo(&[("a", "x")]);
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, repo.path(), Vec::new());
        let mut task = control.task();
        let record = |task: &mut ControlTask<'_>, exit: i32| {
            oms_command_task::Services::observe_failure(
                task,
                FailureObservation {
                    repo: repo.path(),
                    command: b"make check",
                    exit,
                    summary: b"FAIL one",
                },
            )
        };
        record(&mut task, 1).unwrap();
        let (code, out, _) = oms_command_state::run(
            "fail-ledger",
            &osv(&[
                "--repo",
                repo.path().to_str().unwrap(),
                "list",
                "--unresolved",
                "--json",
            ]),
            repo.path(),
            &stop,
        );
        assert_eq!(code, 0);
        assert!(String::from_utf8_lossy(&out).contains("make check"));
        // A passing run resolves the row; the advisory stays empty.
        assert!(record(&mut task, 0).unwrap().is_empty());
        let (_, out, _) = oms_command_state::run(
            "fail-ledger",
            &osv(&[
                "--repo",
                repo.path().to_str().unwrap(),
                "list",
                "--unresolved",
                "--json",
            ]),
            repo.path(),
            &stop,
        );
        assert!(!String::from_utf8_lossy(&out).contains("make check"));
        let observed = oms_command_task::Services::observe_verify(
            &mut task,
            repo.path(),
            b"running 3 tests\ntest result: ok. 0 passed; 0 failed\n",
        )
        .unwrap();
        assert!(observed.lines.starts_with(b"env: "));
    }

    #[test]
    fn autonomy_journal_digest_and_state_repository() {
        let repo = Tmp::repo(&[("a", "x")]);
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, repo.path(), Vec::new());
        let digest = control.controller_digest().unwrap();
        assert!(digest.len() >= 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()));
        let state =
            oms_command_autonomy::Services::state_repository(&control, repo.path(), &stop).unwrap();
        assert_eq!(state, std::fs::canonicalize(repo.path()).unwrap());
        let source = repo.path().join("a");
        let event = oms_command_autonomy::JournalEvent {
            repo: repo.path(),
            source_kind: "autopilot",
            source_path: &source,
            source_id: "run-1",
            outcome: "done",
            verification: "not_verified",
        };
        control.observe_journal(event, &stop).unwrap();
        let bad = oms_command_autonomy::JournalEvent {
            repo: repo.path(),
            source_kind: "not-a-kind",
            source_path: &source,
            source_id: "run-2",
            outcome: "done",
            verification: "not_verified",
        };
        assert!(control.observe_journal(bad, &stop).is_err());
        // Unavailable producers keep their fail-closed defaults, while native
        // context-pack validation validates relative and repo-scoped paths.
        assert!(
            control
                .discovery_section(repo.path(), "metrics", &stop)
                .is_err()
        );
        assert!(
            control
                .validate_context_pack(repo.path(), Path::new("missing-pack.json"), &stop)
                .is_err()
        );
        let digest = "0".repeat(64);
        std::fs::write(
            repo.path().join("good-pack.json"),
            serde_json::json!({
                "pack_digest": digest,
                "files": ["a"],
            })
            .to_string(),
        )
        .unwrap();
        let valid = control
            .validate_context_pack(repo.path(), Path::new("good-pack.json"), &stop)
            .unwrap();
        assert_eq!(valid["pack_digest"], serde_json::json!(digest));
        std::fs::write(
            repo.path().join("bad-pack.json"),
            serde_json::json!({
                "pack_digest": digest,
                "files": ["/etc/passwd"],
            })
            .to_string(),
        )
        .unwrap();
        assert!(
            control
                .validate_context_pack(repo.path(), Path::new("bad-pack.json"), &stop)
                .is_err()
        );
    }

    #[test]
    fn whole_tree_containment_is_opt_in_and_never_silently_weaker() {
        let root = Tmp::new();
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, root.path(), sh("exit 0")).whole_tree();
        // The test process is multithreaded, so the supervisor refuses the
        // exclusive reactor instead of falling back to group containment.
        let error = control
            .invoke_command("x", &[], root.path(), &[], None, &stop)
            .unwrap_err();
        #[cfg(target_os = "linux")]
        assert!(error.message.contains("exclusive"), "{}", error.message);
        #[cfg(not(target_os = "linux"))]
        assert!(error.message.contains("unavailable"), "{}", error.message);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn control_services_new_enforces_whole_tree_on_linux() {
        let dir = Tmp::new();
        let stop = Arc::new(AtomicBool::new(false));
        let control = ControlServices::new(&stop, dir.path()).unwrap();
        // Production ControlServices::new enforces Scope::WholeTree on Linux
        // and refuses silent ProcessGroup fallback inside multithreaded runners.
        let err = oms_command_landing::Services::supervise(
            &control,
            &VerifyRequest {
                argv: sh("exit 0"),
                cwd: dir.path().to_owned(),
                wall: Duration::from_secs(1),
                environment: Vec::new(),
            },
            &stop,
        )
        .unwrap_err();
        assert!(err.message.contains("exclusive"), "{}", err.message);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn control_services_invoke_concurrent_works_across_threads_on_linux() {
        use oms_command_autonomy::{Invocation, Services};
        let dir = Tmp::new();
        let stop = Arc::new(AtomicBool::new(false));
        let mut control = ControlServices::new(&stop, dir.path()).unwrap();
        control.native = sh("printf '%s\\n' \"$1\"");
        let inv = || Invocation::new("peer-delegate", dir.path(), vec![]);
        let single_err = control.invoke(&inv(), &stop).unwrap_err();
        assert!(
            single_err.message.contains("exclusive"),
            "{}",
            single_err.message
        );
        let (a, b) = std::thread::scope(|s| {
            let h1 = s.spawn(|| control.invoke_concurrent(&inv(), &stop));
            let h2 = s.spawn(|| control.invoke_concurrent(&inv(), &stop));
            (h1.join().unwrap().unwrap(), h2.join().unwrap().unwrap())
        });
        assert_eq!(a.0, 0);
        assert_eq!(b.0, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn invoke_concurrent_timeout_terminates_separate_pgrp_descendants_on_sigterm() {
        use oms_command_autonomy::{Invocation, Services};
        let dir = Tmp::new();
        let pid_file = dir.path().join("grand.pid");
        let ready_file = dir.path().join("grand.ready");
        let stop = Arc::new(AtomicBool::new(false));
        let mut control = ControlServices::new(&stop, dir.path()).unwrap();
        let script = format!(
            "g=''; trap 'if [ -n \"$g\" ]; then kill -KILL -\"$g\" 2>/dev/null || kill -KILL \"$g\" 2>/dev/null || true; fi; exit 143' TERM; \
             setsid /bin/sh -c 'trap \"\" TERM; exec sleep 30' </dev/null >/dev/null 2>&1 & g=$!; \
             printf '%s\\n' \"$g\" > \"{pid}\"; : > \"{ready}\"; \
             while kill -0 \"$g\" 2>/dev/null; do sleep 0.05; done; exit 0",
            pid = pid_file.display(),
            ready = ready_file.display(),
        );
        control.native = sh(&script);
        let mut inv = Invocation::new("peer-delegate", dir.path(), vec![]);
        inv.wall = Duration::from_millis(400);
        let (code, out, err) = control.invoke_concurrent(&inv, &stop).unwrap();
        assert_eq!(code, 124);
        assert!(
            ready_file.exists(),
            "missing ready file: code={code} out={:?} err={:?}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err),
        );
        let gpid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let alive = std::process::Command::new("kill")
            .args(["-0", &gpid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(
            !alive,
            "separate process group descendant {gpid} survived invoke_concurrent SIGTERM cancellation"
        );
    }

    #[test]
    fn staged_dispatch_launcher_requires_staged_binary_and_manifest() {
        let dir = Tmp::new();
        let source = dir.path();
        let rev = "0123456789abcdef0123456789abcdef01234567";
        let staged_dir = source.join("rust/target/oms-native").join(rev);
        std::fs::create_dir_all(&staged_dir).unwrap();
        let exe = staged_dir.join("oms");
        std::fs::write(&exe, b"bin").unwrap();

        // Missing manifest fails closed with an empty launcher vector.
        assert!(staged_dispatch_launcher(&exe, source).is_empty());

        std::fs::write(staged_dir.join("manifest.json"), b"{}").unwrap();
        assert_eq!(
            staged_dispatch_launcher(&exe, source),
            vec![
                exe.as_os_str().to_owned(),
                OsString::from("--oms-dispatch"),
                source.as_os_str().to_owned(),
                OsString::from(rev),
                OsString::from("--"),
            ]
        );

        let unstaged = source.join("target/debug/oms");
        assert!(staged_dispatch_launcher(&unstaged, source).is_empty());
        assert!(staged_dispatch_launcher(&staged_dir.join("other"), source).is_empty());
        let bad_rev = source.join("rust/target/oms-native/not-a-sha/oms");
        assert!(staged_dispatch_launcher(&bad_rev, source).is_empty());
    }
}
