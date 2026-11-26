//! Native control-plane service adapters (`plan`, `autonomy`, `landing`, `graph`, `task`, `state`).
//!
//! Authority rules: an adapter only forwards to a public native producer, and
//! tool output, environment labels and room membership are never treated as a
//! grant. The landing authorities (plan lease/receipt compare-and-set, approval
//! reservation, coverage, failure reports, findings, admission context) live in
//! [`crate::services_landing`]; inputs no native producer can prove there
//! fail closed. Apart from host residue cleanup, which removes only proven
//! dead, clean, owned harness worktrees and exact install links, nothing here
//! writes a file itself: every mutation is performed by the owning domain
//! crate under its own lock.
//!
//! Child processes (`invoke`, `invoke_command`, verification, the publication
//! scan worker) always run through `oms_supervisor` with a wall deadline,
//! bounded output and cancellation; argv is never shell-parsed.
//! [`ControlServices::new`] enforces `Scope::WholeTree` (the supervisor's
//! exclusive subreaper reactor) on Linux and `Scope::ProcessGroup` on
//! non-Linux hosts; if a Linux caller cannot guarantee exclusive child
//! ownership, the supervisor fails closed rather than weakening containment.
use oms_command_graph::Error as GraphError;
#[cfg(target_os = "linux")]
use oms_command_landing::services::{BatchVerification, VerificationOutcome};
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
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
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
const SUPERVISE_REQUEST_LIMIT: usize = 4 * 1024 * 1024;
/// First argument of the dedicated publication-scan worker process. The binary's
/// entry point must route `<exe> SCAN_WORKER_FLAG ...` to [`scan_worker_main`]
/// before any other dispatch; the parent never runs the worker in-process.
pub(crate) const SCAN_WORKER_FLAG: &str = "--oms-publication-scan-worker";
/// Internal single-thread child supervisor trampoline used when a Linux caller
/// already holds extra threads (such as `patch-admit`'s live scope monitor)
/// and still requires exclusive `Scope::WholeTree` subreaper containment.
pub(crate) const SUPERVISE_WORKER_FLAG: &str = "--oms-supervise-worker";
pub(crate) const SUPERVISE_STARTED: &[u8] = b"OMS_SUPERVISE_STARTED\n";

/// Outcome of one fully supervised child. Exit codes follow the shell
/// convention used by the owning crates: 124 deadline, 125 output limit or
/// supervisor stop, 130 cancellation.
pub(crate) struct Child {
    pub(crate) code: i32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) elapsed: Duration,
    pub(crate) timed_out: bool,
    pub(crate) output_limited: bool,
    pub(crate) cancelled: bool,
    pub(crate) dropped: u64,
}

/// One supervised launch. `merged` joins stderr into stdout (acceptance
/// commands); otherwise the streams are captured separately.
pub(crate) struct Spawn<'a> {
    pub(crate) scope: Scope,
    pub(crate) argv: Vec<OsString>,
    pub(crate) cwd: Option<&'a Path>,
    pub(crate) clear_environment: bool,
    pub(crate) environment: Vec<(OsString, OsString)>,
    pub(crate) remove_environment: Vec<OsString>,
    pub(crate) merged: bool,
    pub(crate) drain: bool,
    pub(crate) stdin: Vec<u8>,
    pub(crate) wall: Duration,
    pub(crate) limit: usize,
    pub(crate) terminate_grace: Duration,
}
impl<'a> Spawn<'a> {
    pub(crate) fn new(scope: Scope, argv: Vec<OsString>, wall: Duration, limit: usize) -> Self {
        Self {
            scope,
            argv,
            cwd: None,
            clear_environment: false,
            environment: Vec::new(),
            remove_environment: Vec::new(),
            merged: false,
            drain: false,
            stdin: Vec::new(),
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
    run_child_started(spawn, interrupted, || Ok(()))
}

fn run_child_started(
    spawn: Spawn<'_>,
    interrupted: &AtomicBool,
    started: impl FnOnce() -> Result<(), String>,
) -> Result<Child, String> {
    let Spawn {
        scope,
        argv,
        cwd,
        clear_environment,
        environment,
        remove_environment,
        merged,
        drain,
        stdin,
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
    request.stdin = Some(stdin);
    request.clear_environment = clear_environment;
    request.environment = environment;
    let mut remove_environment = remove_environment;
    for key in ["OMS_NATIVE_OUTCOME", "OMS_NATIVE_DISPATCHED"] {
        if !remove_environment.iter().any(|v| v == key)
            && !request.environment.iter().any(|(k, _)| k == key)
        {
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
        on_limit: if drain {
            LimitAction::Drain
        } else {
            LimitAction::Terminate
        },
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
    let id = reactor
        .start(request)
        .map_err(|e| format!("launch failed: {e}"))?;
    started()?;
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
                let dropped =
                    done.stdout.dropped_bytes + done.stderr.as_ref().map_or(0, |s| s.dropped_bytes);
                return Ok(Child {
                    code,
                    stdout: done.stdout.bytes,
                    stderr: done.stderr.map_or_else(Vec::new, |s| s.bytes),
                    elapsed: done.duration,
                    timed_out: done.stop == Some(StopReason::Deadline),
                    output_limited: done.stop == Some(StopReason::OutputLimit),
                    cancelled: done.stop == Some(StopReason::Cancelled),
                    dropped,
                });
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn batch_verification_result(child: Child) -> BatchVerification {
    let complete = |raw: &[u8]| {
        const STATUS_LIMIT: usize = 4096;
        if raw.len() > STATUS_LIMIT {
            return false;
        }
        let Some(raw) = raw.strip_suffix(b"\n") else {
            return false;
        };
        let mut frames = raw.split(|byte| *byte == b'\n');
        let (Some(start), Some(exit), None) = (frames.next(), frames.next(), frames.next()) else {
            return false;
        };
        let (Ok(start), Ok(exit)) = (
            serde_json::from_slice::<Value>(start),
            serde_json::from_slice::<Value>(exit),
        ) else {
            return false;
        };
        // Bubblewrap can publish child-pid before mount setup. Only its
        // terminal frame distinguishes a command exit from a setup error.
        start["child-pid"].as_u64().is_some_and(|pid| pid > 0)
            && exit.as_object().is_some_and(|row| row.len() == 1)
            && exit["exit-code"].as_i64() == Some(i64::from(child.code))
    };
    let outcome = match (
        child.cancelled,
        child.timed_out,
        child.output_limited,
        child.code,
    ) {
        (true, _, _, _) => VerificationOutcome::Cancelled,
        (_, true, _, _) => VerificationOutcome::TimedOut,
        (_, _, true, _) => VerificationOutcome::OutputLimited,
        (_, _, _, 125..=127) => VerificationOutcome::Unavailable,
        _ if !complete(&child.stdout) => VerificationOutcome::Unavailable,
        (_, _, _, 0) => VerificationOutcome::Passed,
        _ => VerificationOutcome::Failed,
    };
    BatchVerification {
        outcome,
        verifier_exit: matches!(
            outcome,
            VerificationOutcome::Passed | VerificationOutcome::Failed
        )
        .then_some(child.code),
        elapsed: child.elapsed,
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
pub(crate) fn state_repository_for(work: &Path) -> Result<PathBuf, String> {
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
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    supervise_worker: Vec<OsString>,
    gh_bin: OsString,
    scope: Scope,
    /// Approval grant validated before admission and reserved only by the
    /// landing fence, so a rejected admission never burns the approval.
    landing_grant: std::sync::Mutex<Option<crate::services_landing::Grant>>,
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
    if rev_dir.parent() != Some(source.join("target/oms-native").as_path()) {
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
        let supervise_worker = if exe.file_name().and_then(|v| v.to_str()) == Some("oms") {
            vec![exe.as_os_str().to_owned(), SUPERVISE_WORKER_FLAG.into()]
        } else {
            Vec::new()
        };
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
            supervise_worker,
            gh_bin: "gh".into(),
            scope,
            landing_grant: std::sync::Mutex::new(None),
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
            supervise_worker: Vec::new(),
            gh_bin: "gh".into(),
            scope: Scope::ProcessGroup,
            landing_grant: std::sync::Mutex::new(None),
        }
    }
    /// Override the `gh` executable path in tests without mutating process `PATH`.
    #[cfg(test)]
    fn with_gh(mut self, gh: impl Into<OsString>) -> Self {
        self.gh_bin = gh.into();
        self
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
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn supervise_worker(&self) -> &[OsString] {
        &self.supervise_worker
    }
}

impl oms_command_plan::Services for ControlServices<'_> {
    fn team_withdrawn(
        &self,
        context: &oms_command_plan::Context,
        team_id: &str,
        spec_sha256: &str,
    ) -> oms_command_plan::Result<Option<oms_command_plan::json::Value>> {
        crate::services_runtime::resources::team_withdrawn(
            context,
            team_id,
            spec_sha256,
            self,
            self.interrupted,
        )
    }
    fn lifecycle_writer_before(
        &self,
        repo: &Path,
        deadline: Instant,
    ) -> oms_command_plan::Result<Box<dyn LifecycleGuard + '_>> {
        oms_command_plan::Services::lifecycle_writer_before(&self.local, repo, deadline)
    }
    fn state_root(&self, repo: &Path) -> oms_command_plan::Result<Option<PathBuf>> {
        let state = state_repository_for(repo).map_err(plan_error)?;
        let own = std::fs::canonicalize(repo)
            .map_err(|_| plan_error("cannot resolve physical repository"))?;
        Ok((state != own).then_some(state))
    }
    fn query_state_root(
        &self,
        repo: &Path,
        requested: &Path,
    ) -> oms_command_plan::Result<Option<PathBuf>> {
        use oms_reference::repository::{Resolver, managed_scratch_parent};
        // Context already resolved Git's physical root. Check the original
        // scratch spelling as well: a substituted checkout must not turn into
        // a normal read of the symlink target's unrelated Plan.
        let selected = requested
            .ancestors()
            .find_map(|path| managed_scratch_parent(path).map(|parent| (path, parent)));
        if selected.is_none() && managed_scratch_parent(repo).is_none() {
            return self.state_root(repo);
        }
        let resolver = Resolver::from_env().map_err(plan_error)?;
        if resolver.child {
            return self.state_root(repo);
        }
        if let Some((worktree, parent)) = selected
            && !([worktree, &parent]
                .iter()
                .all(|p| std::fs::symlink_metadata(p).is_ok_and(|entry| entry.is_dir()))
                && std::fs::canonicalize(worktree).is_ok_and(|path| repo.starts_with(&path)))
        {
            return Err(plan_error("selected scratch checkout was substituted"));
        }
        if managed_scratch_parent(repo).is_none() {
            return self.state_root(repo);
        }
        resolver
            .verified_scratch_repository(repo, self.interrupted)
            .map(Some)
            .map_err(plan_error)
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
        let child = self
            .run_supervised(spawn, self.interrupted)
            .map_err(plan_error)?;
        let exit = child.code.clamp(0, 255) as u8;
        Ok(PlanExecution {
            exit,
            // Plan expects the runner to propagate the observed command status.
            // Incomplete cleanup or drainage has already returned an error above.
            runner_exit: exit,
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
    fn group_membership_before(
        &self,
        gid: u32,
        deadline: Instant,
        interrupted: &AtomicBool,
    ) -> oms_command_plan::Result<oms_command_plan::services::GroupMembership> {
        crate::services_runtime::resolve_group_membership_before(gid, deadline, interrupted)
    }
    fn publication_product(
        &self,
        repo: &Path,
        commit: &str,
        record: &oms_command_plan::json::Value,
        patch: &[u8],
    ) -> oms_command_plan::Result<oms_command_plan::services::PublicationProduct> {
        use oms_command_plan::{Action, Context, Plan, Request, completion, json};
        if self.interrupted.load(Ordering::Acquire) {
            return Err(oms_command_plan::Error {
                code: 130,
                message: "publication proof interrupted".into(),
            });
        }
        let execution = std::fs::canonicalize(repo).map_err(|e| plan_error(e.to_string()))?;
        let state = state_repository_for(&execution).map_err(plan_error)?;
        let cwd = std::env::current_dir().map_err(|e| plan_error(e.to_string()))?;
        let context = Context::from_environment_before(
            &Request::new(Action::Status),
            &execution,
            &cwd,
            Some(&state),
            self.interrupted,
            Instant::now() + Duration::from_secs(10),
        )?;
        if context.execution_repo != execution {
            return Err(plan_error("publication execution repository differs"));
        }
        let plan = Plan::open(context, self, self.interrupted)?;
        let proof = completion::commit_landing(&plan, commit, None)?;
        let push_base = proof.receipt.get("push").get("base").str()?;
        let captured = serde_json::from_slice(&json::canonical(record)?)
            .map_err(|e| plan_error(e.to_string()))?;
        let native = crate::services_collab::CollabServices::new(self.interrupted, &self.source)
            .map_err(plan_error)?;
        oms_panel_core::results::verify_publication_contents(
            &execution,
            commit,
            &captured,
            patch,
            push_base,
            &native,
            self.interrupted,
        )
        .map_err(|e| oms_command_plan::Error {
            code: e.code,
            message: e.message,
        })?;
        let again = completion::commit_landing(&plan, commit, Some(&json::sha(&proof.raw)))?;
        if self.interrupted.load(Ordering::Acquire) {
            return Err(oms_command_plan::Error {
                code: 130,
                message: "publication proof interrupted".into(),
            });
        }
        if again.raw != proof.raw {
            return Err(plan_error("publication proof changed"));
        }
        Ok(oms_command_plan::services::PublicationProduct {
            commit: commit.into(),
            patch_sha256: json::sha(patch),
            accepted_tree: record.text("tree").into(),
            base: record.text("base").into(),
            raw_proof: proof.raw,
        })
    }
    fn read_managed_patch(
        &self,
        state_repo: &Path,
        path: &Path,
        limit: usize,
    ) -> oms_command_plan::Result<Option<Vec<u8>>> {
        let index = oms_reference::capture::CaptureIndex::load(state_repo).map_err(plan_error)?;
        index.read_patch(path, limit).map(Some).map_err(plan_error)
    }
}

impl<'a> ControlServices<'a> {
    fn run_supervised(&self, spawn: Spawn<'_>, interrupted: &AtomicBool) -> Result<Child, String> {
        #[cfg(target_os = "linux")]
        if spawn.scope == Scope::WholeTree
            && !self.supervise_worker.is_empty()
            && std::fs::read_dir("/proc/self/task").is_ok_and(|entries| entries.count() != 1)
        {
            return run_via_supervise_worker(&self.supervise_worker, &spawn, interrupted);
        }
        run_child(spawn, interrupted)
    }
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
            spawn = spawn.terminate_grace(Duration::from_secs(5));
        }
        let child = self
            .run_supervised(spawn, interrupted)
            .map_err(|e| autonomy_error(2, e))?;
        Ok((child.code, child.stdout, child.stderr))
    }
    fn github_get(
        &self,
        endpoint: &str,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<Vec<u8>> {
        if interrupted.load(Ordering::Acquire) {
            return Err(autonomy_error(130, "interrupted"));
        }
        let argv = vec![
            self.gh_bin.clone(),
            "api".into(),
            "--hostname".into(),
            "github.com".into(),
            "--method".into(),
            "GET".into(),
            endpoint.into(),
        ];
        let mut spawn = Spawn::new(self.scope, argv, Duration::from_secs(20), 512 * 1024)
            .environment(vec![
                ("GH_PROMPT_DISABLED".into(), "1".into()),
                ("GH_PAGER".into(), "cat".into()),
            ]);
        if self.scope == Scope::ProcessGroup {
            spawn = spawn.terminate_grace(Duration::from_secs(5));
        }
        let child = self
            .run_supervised(spawn, interrupted)
            .map_err(|e| autonomy_error(2, e))?;
        if child.cancelled {
            return Err(autonomy_error(130, "interrupted"));
        }
        if child.code != 0 || child.timed_out || child.output_limited {
            return Err(autonomy_error(
                2,
                "GitHub source unavailable or exceeded its bounded GET",
            ));
        }
        Ok(child.stdout)
    }
}

impl oms_command_autonomy::Services for ControlServices<'_> {
    fn classify_autonomy_failure(
        &self,
        text: &str,
        exit: i32,
    ) -> oms_command_autonomy::Result<Value> {
        let args = vec![
            "classify".into(),
            "--exit".into(),
            exit.to_string().into(),
            "--".into(),
            text.into(),
        ];
        let (code, raw, _) = oms_command_failure::run(&args, &self.source, self.interrupted);
        if code != 0 {
            return Err(autonomy_error(
                code,
                "native failure classification unavailable",
            ));
        }
        oms_command_autonomy::value::parse(&raw)
    }

    fn discovery_section(
        &self,
        repo: &Path,
        section: &str,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<Value> {
        if section != "acceptance" {
            return Err(autonomy_error(2, "native discovery section unavailable"));
        }
        let services =
            crate::services_runtime::RuntimeServices::new(self.interrupted, &self.source)
                .map_err(|e| autonomy_error(2, e))?;
        let roots = oms_runtime::Services::resolve_roots(&services, repo, repo.as_os_str())?;
        let runtime = oms_runtime::storage::Runtime::open(
            &roots.execution_repo,
            &roots.state_repo,
            &self.source,
            &services,
            interrupted,
        )?;
        let envelope = oms_runtime::projection::envelope(&runtime)?;
        oms_command_autonomy::value::parse(&oms_runtime::value::canonical(&envelope)?)
    }

    fn external_observation(
        &self,
        repo: &Path,
        state: &Path,
        binding: &Value,
        refresh: bool,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<Value> {
        let services =
            crate::services_runtime::RuntimeServices::new(self.interrupted, &self.source)
                .map_err(|e| autonomy_error(2, e))?;
        let roots = oms_runtime::Services::resolve_roots(&services, repo, repo.as_os_str())?;
        if roots.state_repo != state {
            return Err(autonomy_error(2, "external wait state repository changed"));
        }
        let runtime = oms_runtime::storage::Runtime::open(
            &roots.execution_repo,
            &roots.state_repo,
            &self.source,
            &services,
            interrupted,
        )?;
        let binding = oms_runtime::value::from_serde(binding)?;
        let row = oms_runtime::external::observe(&runtime, &binding, refresh)?;
        oms_command_autonomy::value::parse(&oms_runtime::value::canonical(&row)?)
    }

    fn experience_context(
        &self,
        repo: &Path,
        query: &str,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<Value> {
        use oms_command_autonomy::value;
        let services =
            crate::services_runtime::RuntimeServices::new(self.interrupted, &self.source)
                .map_err(|e| autonomy_error(2, e))?;
        let roots = oms_runtime::Services::resolve_roots(&services, repo, repo.as_os_str())?;
        let runtime = oms_runtime::storage::Runtime::open(
            &roots.execution_repo,
            &roots.state_repo,
            &self.source,
            &services,
            interrupted,
        )?;
        let head = &runtime.repository.head;
        let clean_output = self
            .run_supervised(
                Spawn::new(
                    self.scope,
                    [
                        "git",
                        "--no-optional-locks",
                        "-c",
                        "core.fsmonitor=false",
                        "-c",
                        "core.hooksPath=/dev/null",
                        "status",
                        "--porcelain=v1",
                        "--untracked-files=all",
                    ]
                    .map(OsString::from)
                    .to_vec(),
                    Duration::from_secs(2),
                    64 * 1024,
                )
                .cwd(repo),
                interrupted,
            )
            .map_err(|e| autonomy_error(2, e))?;
        let current_clean = clean_output.code == 0 && clean_output.stdout.is_empty();
        let tree_output = self
            .run_supervised(
                Spawn::new(
                    self.scope,
                    ["git", "--no-optional-locks", "rev-parse", "HEAD^{tree}"]
                        .map(OsString::from)
                        .to_vec(),
                    Duration::from_secs(2),
                    128,
                )
                .cwd(repo),
                interrupted,
            )
            .map_err(|e| autonomy_error(2, e))?;
        let tree = String::from_utf8_lossy(&tree_output.stdout)
            .trim()
            .to_owned();
        let spec = runtime
            .bytes("PROJECT.md", 1024 * 1024)?
            .ok_or_else(|| autonomy_error(2, "experience requires the current project contract"))?;
        let spec_sha = value::hash(spec.as_ref());
        let progress = runtime
            .bytes(".oms/plan/progress.jsonl", 4 * 1024 * 1024)?
            .unwrap_or_default();
        if !progress.is_empty() && !progress.ends_with(b"\n") {
            return Err(autonomy_error(
                2,
                "experience progress source has a torn tail",
            ));
        }
        let mut facts = Vec::new();
        for line in progress.split(|b| *b == b'\n').filter(|v| !v.is_empty()) {
            if facts.len() >= 4096 {
                return Err(autonomy_error(2, "experience source row bound exceeded"));
            }
            facts.push(value::parse(line)?);
        }
        let args = [
            "show",
            "--repo",
            roots
                .state_repo
                .to_str()
                .ok_or_else(|| autonomy_error(2, "experience requires a Unicode repository"))?,
            "--recent",
            "32",
            "--json",
        ]
        .map(OsString::from);
        let mut read_journal =
            oms_command_autonomy::Invocation::new("journal", repo, args.to_vec());
        read_journal.wall = Duration::from_secs(5);
        read_journal.output_limit = 256 * 1024;
        // The journal's own lock remains authoritative, inside a bounded
        // native observer process so a busy writer cannot stall planning.
        let (journal_code, journal_raw, _) = self
            .invoke_with_scope(&read_journal, self.scope, interrupted)
            .unwrap_or((2, Vec::new(), Vec::new()));
        let journal = if journal_code == 0 && journal_raw.len() <= 256 * 1024 {
            value::parse(&journal_raw)?
        } else {
            Value::Null
        };
        let mut entries = vec![];
        let mut seen = std::collections::BTreeSet::new();
        for event in journal["events"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .rev()
        {
            let id = value::text(&event["source"], "id");
            if !matches!(
                value::text(&event["source"], "type"),
                "autopilot" | "goal-drive"
            ) {
                continue;
            }
            let Some(fact) = facts
                .iter()
                .rev()
                .find(|r| r["run_id"] == id || r["result_sha256"] == id)
            else {
                continue;
            };
            if !current_clean
                || tree_output.code != 0
                || fact["worktree_clean"] != true
                || fact["tree"] != tree
                || fact["spec_sha256"] != spec_sha
                || !(fact["base_sha"] == head.as_str() || fact["head"] == head.as_str())
            {
                continue;
            }
            if !matches!(
                value::text(fact, "kind"),
                "terminal" | "autopilot-external-result"
            ) {
                continue;
            }
            if fact["kind"] == "autopilot-external-result" {
                let Ok(current) = oms_runtime::external::observe(
                    &runtime,
                    &oms_runtime::value::from_serde(&fact["binding"])?,
                    false,
                ) else {
                    continue;
                };
                let current = value::parse(&oms_runtime::value::canonical(&current)?)?;
                if current["identity_valid"] != true
                    || current["status"] != "terminal"
                    || current["result_sha256"] != fact["result_sha256"]
                {
                    continue;
                }
            }
            let sha = value::hash(value::canonical(fact)?);
            if !seen.insert(sha.clone()) {
                continue;
            }
            let item = serde_json::json!({"kind":"observed-outcome","journal_event_id":event["event_id"],"source_path":".oms/plan/progress.jsonl","source_sha256":sha,
                "head":head,"spec_sha256":spec_sha,"status":fact["status"],"reason":fact["reason"],"cause":fact["cause"],
                "outcome":fact["observation"]["outcome"],"progress_sha256":fact["observation"]["progress_sha256"],"result_sha256":fact["result_sha256"]});
            if value::canonical(&item)?.len() <= 2048 {
                entries.push(item);
            }
            if entries.len() == 6 {
                break;
            }
        }
        // The existing provider view applies curation, retirement, source
        // citation validation and query-ranked project/global fallback. Only
        // its relevant section is retained; unrelated recency is not injected.
        let (memory_code, memory_raw, _) = oms_command_memory::provider_context(
            repo,
            &query.chars().take(300).collect::<String>(),
            interrupted,
        );
        let memory = if memory_code == 0 {
            String::from_utf8_lossy(&memory_raw)
                .find("### relevant recall (")
                .map(|at| {
                    String::from_utf8_lossy(&memory_raw)[at..]
                        .chars()
                        .take(4096)
                        .collect::<String>()
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        runtime.invalidate(Path::new("PROJECT.md"));
        let final_view = oms_runtime::Services::repository_view(&services, repo)?;
        if final_view.head != *head
            || final_view.worktree_state != runtime.repository.worktree_state
            || runtime
                .bytes("PROJECT.md", 1024 * 1024)?
                .as_ref()
                .is_none_or(|raw| value::hash(raw.as_ref()) != spec_sha)
        {
            return Err(autonomy_error(
                2,
                "experience applicability changed during read",
            ));
        }
        let result = serde_json::json!({"schema":1,"advisory":true,"repo_head":head,"spec_sha256":spec_sha,
            "source_refs":[{"path":".oms/plan/progress.jsonl","sha256":value::hash(progress.as_ref())}],
            "entries":entries,"memory_recall":memory,"journal_available":journal_code==0,"memory_available":memory_code==0,
            "authority":"reference only; current source and fresh verification govern"});
        if value::canonical(&result)?.len() > 16 * 1024 {
            return Err(autonomy_error(2, "experience context exceeds byte bound"));
        }
        Ok(result)
    }

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
    fn list_issues(
        &self,
        owner: &str,
        repo: &str,
        label: &str,
        limit: u64,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<Vec<u8>> {
        let endpoint =
            oms_command_autonomy::services::issues_list_endpoint(owner, repo, label, limit)?;
        self.github_get(&endpoint, interrupted)
    }
    fn repair_feedback(
        &self,
        repo: &Path,
        state: &Path,
        event_id: &str,
        head: &str,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<Value> {
        let row = oms_command_landing::feedback::repair_candidate(
            repo,
            state,
            event_id,
            head,
            interrupted,
        )
        .map_err(|e| autonomy_error(e.code, e.message))?;
        oms_command_autonomy::value::parse(&row.json(false).map_err(|e| autonomy_error(2, e))?)
    }
    fn revalidate_repair_feedback(
        &self,
        repo: &Path,
        evidence: &Value,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<()> {
        let row = oms_command_landing::V::parse(&oms_command_autonomy::value::canonical(evidence)?)
            .map_err(|e| autonomy_error(2, e))?;
        oms_command_landing::feedback::revalidate_repair(repo, &row, interrupted)
            .map_err(|e| autonomy_error(e.code, e.message))
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
        let child = self
            .run_supervised(spawn.cwd(cwd).environment(environment), cancel)
            .map_err(GraphError::new)?;
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
    fn admission_context(&self) -> Option<&dyn oms_reference::binding::Context> {
        Some(&crate::services_landing::AdmissionContext)
    }
    fn landing_review(
        &self,
        request: &oms_command_landing::services::LandingRequest<'_>,
    ) -> oms_command_landing::Result<oms_command_landing::V> {
        crate::services_landing::landing_review(self, request, self.interrupted)
    }
    fn panel_accept(
        &self,
        request: &oms_command_landing::services::LandingRequest<'_>,
        fence: &oms_command_landing::V,
    ) -> oms_command_landing::Result<()> {
        let refuse = |message| oms_command_landing::Error::new(2, message);
        let fence: Value = serde_json::from_slice(&fence.json(false)?)
            .map_err(|_| refuse("malformed panel acceptance fence"))?;
        if !request.task.is_empty()
            || request.policy.ml
            || request.policy.allow_test_reduction
            || request.policy.allow_restructure
            || request.policy.accept_encoding_loss
            || request.policy.accept_salvaged
            || fence["snapshot"]["candidate"]["patch_sha256"].as_str() != Some(request.patch_sha256)
            || fence["base"].as_str() != Some(request.base)
            || fence["verify"].as_str() != Some(request.policy.verify.as_str())
            || fence["allow_verifier_change"].as_bool()
                != Some(request.policy.allow_verifier_change)
            || fence["allow_floor_change"].as_str().unwrap_or_default()
                != request.policy.allow_floor_change
        {
            return Err(refuse("panel acceptance does not match this exact landing"));
        }
        let collab = crate::services_collab::CollabServices::new(self.interrupted, &self.source)
            .map_err(|e| oms_command_landing::Error::new(2, e))?;
        oms_panel_core::results::verify_accept_fence(
            request.state_repo,
            request.execution_repo,
            &fence,
            &collab,
            self.interrupted,
        )
        .map_err(|e| oms_command_landing::Error::new(e.code, e.message))?;
        let expected = oms_command_plan::json::parse(
            &serde_json::to_vec(&fence["snapshot"]["task"])
                .map_err(|_| refuse("malformed panel acceptance task"))?,
        )?;
        let context = oms_command_plan::Context::from_selected_state_before(
            &oms_command_plan::Request::new(oms_command_plan::Action::EvidenceSnapshot),
            request.execution_repo,
            request.execution_repo,
            request.state_repo,
            self.interrupted,
            Instant::now() + Duration::from_secs(10),
        )?;
        oms_command_plan::Plan::open(context, self, self.interrupted)?
            .verify_panel_review_lease(&expected)?;
        Ok(())
    }
    fn landing_approval(
        &self,
        request: &oms_command_landing::services::LandingRequest<'_>,
        approval: &str,
        grant: &str,
        version: &str,
        request_only: bool,
    ) -> oms_command_landing::Result<oms_command_landing::V> {
        let (value, reserved) = crate::services_landing::landing_approval(
            self,
            request,
            approval,
            grant,
            version,
            request_only,
            self.interrupted,
        )?;
        if let Some(reserved) = reserved {
            *self.landing_grant.lock().map_err(|_| {
                oms_command_landing::Error::new(2, "landing approval state is poisoned")
            })? = Some(reserved);
        }
        Ok(value)
    }
    fn fence_landing(
        &self,
        request: &oms_command_landing::services::LandingRequest<'_>,
        intent: &oms_command_landing::V,
    ) -> oms_command_landing::Result<()> {
        let grant = self
            .landing_grant
            .lock()
            .map_err(|_| oms_command_landing::Error::new(2, "landing approval state is poisoned"))?
            .take();
        crate::services_landing::fence_landing(self, request, intent, grant, self.interrupted)
    }
    fn complete_landing(
        &self,
        request: &oms_command_landing::services::LandingRequest<'_>,
        receipt: &oms_command_landing::V,
    ) -> oms_command_landing::Result<()> {
        crate::services_landing::complete_landing(self, request, receipt, self.interrupted)
    }
    fn recover_landing(
        &self,
        request: &oms_command_landing::services::LandingRequest<'_>,
        intent: &oms_command_landing::V,
        applied: bool,
    ) -> oms_command_landing::Result<()> {
        crate::services_landing::recover_landing(self, request, intent, applied, self.interrupted)
    }
    fn coverage(
        &self,
        repo: &Path,
        ids: &[String],
        evidence: Option<&oms_command_landing::V>,
    ) -> oms_command_landing::Result<oms_command_landing::V> {
        let runtime = crate::services_runtime::RuntimeServices::new(self.interrupted, &self.source)
            .map_err(|e| oms_command_landing::Error::new(3, e))?;
        crate::services_landing::coverage(
            &runtime,
            &self.source,
            repo,
            ids,
            evidence,
            self.interrupted,
        )
    }
    fn cleanup(
        &self,
        repo: &Path,
        apply: bool,
        interrupted: &AtomicBool,
    ) -> oms_command_landing::Result<oms_command_landing::V> {
        crate::services_landing::cleanup(&self.source, repo, apply, interrupted)
    }
    fn failure_report(
        &self,
        repo: &Path,
        fingerprint: &str,
    ) -> oms_command_landing::Result<oms_command_landing::V> {
        crate::services_landing::failure_report(
            crate::registry::PUBLIC,
            repo,
            fingerprint,
            self.interrupted,
        )
    }
    fn review_findings(&self, repo: &Path) -> oms_command_landing::Result<oms_command_landing::V> {
        crate::services_landing::review_findings(repo, self.interrupted)
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
        let child = self
            .run_supervised(spawn, interrupted)
            .map_err(|e| oms_command_landing::Error::new(2, e))?;
        Ok(LandingExecution {
            code: child.code,
            stdout: child.stdout,
            stderr: child.stderr,
            elapsed: child.elapsed,
        })
    }
    #[cfg(target_os = "linux")]
    fn supervise_batch(
        &self,
        request: &VerifyRequest,
        interrupted: &AtomicBool,
    ) -> oms_command_landing::Result<BatchVerification> {
        let unavailable =
            || oms_command_landing::Error::unavailable("isolated Linux batch verification");
        if self.scope != Scope::WholeTree {
            return Err(unavailable());
        }
        let (program, args) = request.argv.split_first().ok_or_else(unavailable)?;
        let state = request.cwd.join(".git");
        let private_home = state.join("batch-home");
        let private_tmp = state.join("batch-tmp");
        let cargo = state.join("batch-cargo");
        let rustup = state.join("batch-rustup");
        for directory in [&private_home, &private_tmp, &cargo, &rustup] {
            std::fs::create_dir(directory)?;
        }
        let mut mounts: Vec<_> = [
            "/usr",
            "/bin",
            "/sbin",
            "/lib",
            "/lib64",
            "/lib32",
            "/etc/ld.so.cache",
            "/etc/alternatives",
            "/etc/passwd",
            "/etc/group",
            "/etc/nsswitch.conf",
            "/etc/os-release",
            "/etc/localtime",
        ]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .map(|path| (path.clone(), path))
        .collect();
        let host_home = std::env::home_dir().ok_or_else(unavailable)?;
        let mut mount_cache = |name, default: &str, target: &Path, items: &[&str]| {
            let root = std::env::var_os(name)
                .filter(|value| Path::new(value).is_absolute())
                .map(PathBuf::from)
                .unwrap_or_else(|| host_home.join(default));
            if let Ok(root) = root.canonicalize() {
                for item in items {
                    if let Ok(source) = root.join(item).canonicalize()
                        && source.starts_with(&root)
                    {
                        mounts.push((source, target.join(item)));
                    }
                }
            }
        };
        mount_cache(
            "CARGO_HOME",
            ".cargo",
            &cargo,
            &[
                "bin/cargo",
                "bin/rustup",
                "bin/rustc",
                "bin/rustfmt",
                "bin/rustdoc",
                "bin/clippy-driver",
                "bin/cargo-clippy",
                "bin/cargo-fmt",
                "registry/cache",
                "registry/index",
                "registry/src",
                "git/db",
                "git/checkouts",
            ],
        );
        mount_cache("RUSTUP_HOME", ".rustup", &rustup, &["toolchains"]);
        let (wrapper, arguments) =
            oms_supervisor::sandbox::isolated_checkout(program, args, &request.cwd, &mounts)
                .map_err(|_| unavailable())?;
        let argv = std::iter::once(wrapper).chain(arguments).collect();
        let path = std::env::join_paths([
            cargo.join("bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .map_err(|_| unavailable())?;
        let environment = vec![
            ("PATH".into(), path),
            ("HOME".into(), private_home.into_os_string()),
            ("TMPDIR".into(), private_tmp.into_os_string()),
            ("CARGO_HOME".into(), cargo.into_os_string()),
            ("RUSTUP_HOME".into(), rustup.into_os_string()),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ];
        let mut spawn = Spawn::new(self.scope, argv, request.wall, VERIFY_OUTPUT_LIMIT)
            .cwd(&request.cwd)
            .environment(environment);
        spawn.clear_environment = true;
        let child = self
            .run_supervised(spawn, interrupted)
            .map_err(|_| unavailable())?;
        Ok(batch_verification_result(child))
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
        let child = self
            .run_supervised(spawn.cwd(&request.repo), interrupted)
            .map_err(|e| fail(2, e))?;
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
        if !repo.join("scripts/oms").is_file() || !repo.join("crates/oms/Cargo.toml").is_file() {
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
        let child = self
            .run_supervised(
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
    fn scratch_activity(&self, repo: &Path, stop: &AtomicBool) -> Option<oms_command_state::Value> {
        let display = oms_panel_core::scratch_activity::observe(repo, stop);
        oms_command_state::json::parse(&serde_json::to_vec(&display).ok()?).ok()
    }
    fn plan_services(&self) -> &(dyn oms_command_plan::Services + Sync) {
        self
    }
    fn runtime_envelope(
        &self,
        repo: &Path,
        interrupted: &AtomicBool,
    ) -> Option<oms_command_state::Result<oms_command_state::Value>> {
        Some((|| {
            let services =
                crate::services_runtime::RuntimeServices::new(self.interrupted, &self.source)
                    .map_err(|e| state_err(2, e))?;
            let roots = oms_runtime::Services::resolve_roots(&services, repo, repo.as_os_str())?;
            let mut runtime = oms_runtime::storage::Runtime::open(
                &roots.execution_repo,
                &roots.state_repo,
                &self.source,
                &services,
                interrupted,
            )?;
            runtime.cwd = repo.to_path_buf();
            oms_runtime::projection::envelope(&runtime)
        })())
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
        oms_command_operations::lock::pid_alive(i64::from(pid))
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

const LIFECYCLE_REFERENCE_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum BoundedLifecycleRead {
    Missing,
    Bytes(Vec<u8>),
    ExceedsBound,
}

fn read_lifecycle_references(
    repo: &Path,
    max_bytes: usize,
) -> oms_command_state::Result<BoundedLifecycleRead> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, open, openat, statat};
    use rustix::io::Errno;
    use std::io::Read as _;

    let lifecycle_dir = repo.join(".oms/lifecycle");
    let dir_fd = match open(
        &lifecycle_dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(BoundedLifecycleRead::Missing),
        Err(Errno::LOOP) => {
            return Err(state_err(
                2,
                format!("refusing symlinked path: {}", lifecycle_dir.display()),
            ));
        }
        Err(Errno::NOTDIR) => {
            return Err(state_err(2, "refusing non-directory .oms/lifecycle"));
        }
        Err(err) => {
            if let Some(meta) = no_symlink_metadata(&lifecycle_dir)?
                && !meta.is_dir()
            {
                return Err(state_err(2, "refusing non-directory .oms/lifecycle"));
            }
            return Err(state_err(1, std::io::Error::from(err).to_string()));
        }
    };
    let dir_stat = fstat(&dir_fd).map_err(|e| state_err(1, std::io::Error::from(e).to_string()))?;
    if FileType::from_raw_mode(dir_stat.st_mode) != FileType::Directory {
        return Err(state_err(2, "refusing non-directory .oms/lifecycle"));
    }

    let lifecycle_path = lifecycle_dir.join("events.jsonl");
    let file_fd = match openat(
        &dir_fd,
        "events.jsonl",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(BoundedLifecycleRead::Missing),
        Err(Errno::LOOP) => {
            return Err(state_err(
                2,
                format!("refusing symlinked path: {}", lifecycle_path.display()),
            ));
        }
        Err(Errno::ISDIR) => {
            return Err(state_err(
                2,
                format!("refusing non-regular file: {}", lifecycle_path.display()),
            ));
        }
        Err(err) => {
            if let Ok(st) = statat(&dir_fd, "events.jsonl", AtFlags::SYMLINK_NOFOLLOW) {
                let ft = FileType::from_raw_mode(st.st_mode);
                if ft == FileType::Symlink {
                    return Err(state_err(
                        2,
                        format!("refusing symlinked path: {}", lifecycle_path.display()),
                    ));
                }
                if ft != FileType::RegularFile {
                    return Err(state_err(
                        2,
                        format!("refusing non-regular file: {}", lifecycle_path.display()),
                    ));
                }
            }
            return Err(state_err(1, std::io::Error::from(err).to_string()));
        }
    };

    let file_stat =
        fstat(&file_fd).map_err(|e| state_err(1, std::io::Error::from(e).to_string()))?;
    if FileType::from_raw_mode(file_stat.st_mode) != FileType::RegularFile {
        return Err(state_err(
            2,
            format!("refusing non-regular file: {}", lifecycle_path.display()),
        ));
    }

    let hint = usize::try_from(file_stat.st_size)
        .unwrap_or(max_bytes)
        .min(max_bytes)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(hint);
    std::fs::File::from(file_fd)
        .take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| state_err(1, e.to_string()))?;
    if bytes.len() > max_bytes {
        return Ok(BoundedLifecycleRead::ExceedsBound);
    }
    Ok(BoundedLifecycleRead::Bytes(bytes))
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

    // Open `.oms/lifecycle/events.jsonl` once without following symlinks and
    // bound the read on that handle so an unguarded writer crossing 32 MiB
    // mid-pass defers capture deletion rather than failing the whole gc plan.
    let lifecycle_bytes = match read_lifecycle_references(&repo, LIFECYCLE_REFERENCE_LIMIT)? {
        BoundedLifecycleRead::Missing => None,
        BoundedLifecycleRead::Bytes(bytes) => Some(bytes),
        BoundedLifecycleRead::ExceedsBound => {
            return Ok(oms_command_state::GcHookResult::Deferred(
                "landing capture retention deferred: lifecycle references exceed 32 MiB; captures retained".into(),
            ));
        }
    };

    let mut reference_haystack = Vec::new();
    for bytes in [
        read_bounded_regular(&repo.join(".oms/plan/tasks.json"), 32 * 1024 * 1024)?,
        lifecycle_bytes,
        read_bounded_regular(&repo.join(".oms/artifacts/index.jsonl"), 32 * 1024 * 1024)?,
    ]
    .into_iter()
    .flatten()
    {
        reference_haystack.extend_from_slice(&bytes);
        reference_haystack.push(b'\n');
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
        // Object-form plans key each task by its ID; the key is protected too.
        let task_list: Vec<(Option<&String>, &Value)> = match &root {
            Value::Array(items) => items.iter().map(|t| (None, t)).collect(),
            Value::Object(map) => match map.get("tasks") {
                Some(Value::Array(items)) => items.iter().map(|t| (None, t)).collect(),
                Some(Value::Object(entries)) => entries.iter().map(|(k, t)| (Some(k), t)).collect(),
                Some(_) => {
                    return Err(state_err(
                        2,
                        "malformed .oms/plan/tasks.json: tasks must be an array or object",
                    ));
                }
                None => Vec::new(),
            },
            _ => return Err(state_err(2, "malformed .oms/plan/tasks.json")),
        };
        for (key, task) in task_list {
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
                tokens.extend(key.filter(|k| !k.is_empty()).cloned());
            }
        }
    }

    let lifecycle_dir = repo.join(".oms/lifecycle");
    if let Some(dir_meta) = no_symlink_metadata(&lifecycle_dir)?
        && !dir_meta.is_dir()
    {
        return Err(state_err(2, "refusing non-directory .oms/lifecycle"));
    }
    let events_path = lifecycle_dir.join("events.jsonl");
    if let Some(meta) = no_symlink_metadata(&events_path)? {
        if !meta.is_file() {
            return Err(state_err(
                2,
                format!("refusing non-regular file: {}", events_path.display()),
            ));
        }
        let (_, attempts) =
            oms_command_operations::load_projection(repo).map_err(|e| state_err(2, e))?;
        for attempt in attempts {
            let terminal = attempt
                .get("state")
                .text()
                .or_else(|| attempt.get("status").text())
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
        let dir = std::fs::canonicalize(dir).unwrap();
        chmod_repo_private(&dir);
        dir
    }

    fn chmod_repo_private(root: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let dir_mode = if dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("oms-landing-capture-"))
            {
                0o700
            } else {
                0o750
            };
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(dir_mode)).unwrap();
            for entry in std::fs::read_dir(&dir).unwrap() {
                let p = entry.unwrap().path();
                let meta = std::fs::symlink_metadata(&p).unwrap();
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    stack.push(p);
                } else {
                    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
                }
            }
        }
    }

    fn seed_released_landing_capture(
        repo: &Path,
        landing_id: &str,
        generation: &str,
        patch_bytes: &[u8],
    ) -> (PathBuf, PathBuf, String) {
        let cap_dir = repo
            .join(".oms/landing-patches")
            .join(format!("oms-landing-capture-{generation}"));
        std::fs::create_dir_all(&cap_dir).unwrap();
        let cap_patch = cap_dir.join(format!("{landing_id}.patch"));
        std::fs::write(&cap_patch, patch_bytes).unwrap();
        age_file(&cap_patch, 60);

        let managed = oms_reference::obj([
            ("version", 1usize.into()),
            ("generation", generation.into()),
            ("landing_id", landing_id.into()),
            (
                "namespace",
                oms_reference::digest(repo.as_os_str().as_bytes()).into(),
            ),
            ("directory", cap_dir.to_str().unwrap().into()),
            (
                "bytes",
                oms_reference::V::Num((2 * patch_bytes.len()).to_string()),
            ),
            ("sha256", oms_reference::digest(patch_bytes).into()),
        ]);
        let mut ledger = Vec::new();
        for (event, ts) in [
            ("capture-reserved", "2026-11-01T00:00:00Z"),
            ("capture-released", "2026-11-01T00:00:01Z"),
        ] {
            let row = oms_reference::obj([
                ("schema", 1usize.into()),
                ("event", event.into()),
                ("landing_id", landing_id.into()),
                ("ts", ts.into()),
                ("managed_capture", managed.clone()),
            ]);
            ledger.extend_from_slice(&oms_reference::encoded(&row).unwrap());
        }
        std::fs::write(repo.join(".oms/landings.jsonl"), &ledger).unwrap();
        chmod_repo_private(repo);
        let rel = cap_patch
            .strip_prefix(repo)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        (cap_dir, cap_patch, rel)
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
        chmod_repo_private(&repo);
        let ok = host.gc_landing_captures(&repo, 30, true).unwrap();
        assert_eq!(ok, GcHookResult::Reclaimed(vec![]));
        assert!(legacy_patch.exists());

        // Released capture is reclaimed while loose unproven patch stays intact.
        let generation = "ab".repeat(16);
        let (cap_dir, cap_patch, cap_rel) = seed_released_landing_capture(
            &repo,
            "land-20261120T000000Z-unit",
            &generation,
            b"diff --git a/x b/x\n",
        );
        assert_eq!(
            host.gc_landing_captures(&repo, 30, false).unwrap(),
            GcHookResult::Reclaimed(vec![("landing-capture".to_string(), cap_rel.clone())])
        );
        assert!(cap_patch.exists());
        assert_eq!(
            host.gc_landing_captures(&repo, 30, true).unwrap(),
            GcHookResult::Reclaimed(vec![("landing-capture".to_string(), cap_rel)])
        );
        assert!(!cap_patch.exists());
        assert!(!cap_dir.exists());
        assert!(legacy_patch.exists());

        // Single-handle lifecycle reader accepts exact bound, defers bound+1, and
        // fails closed on symlinks and non-regular files.
        assert_eq!(
            read_lifecycle_references(&repo, 64).unwrap(),
            BoundedLifecycleRead::Missing
        );
        let lifecycle_dir = repo.join(".oms/lifecycle");
        std::fs::create_dir_all(&lifecycle_dir).unwrap();
        let events_path = lifecycle_dir.join("events.jsonl");
        std::fs::write(&events_path, vec![b'x'; 64]).unwrap();
        assert_eq!(
            read_lifecycle_references(&repo, 64).unwrap(),
            BoundedLifecycleRead::Bytes(vec![b'x'; 64])
        );
        std::fs::write(&events_path, vec![b'x'; 65]).unwrap();
        assert_eq!(
            read_lifecycle_references(&repo, 64).unwrap(),
            BoundedLifecycleRead::ExceedsBound
        );
        std::fs::remove_file(&events_path).unwrap();
        let outside = repo.join("outside-lifecycle.jsonl");
        std::fs::write(&outside, b"{}\n").unwrap();
        std::os::unix::fs::symlink(&outside, &events_path).unwrap();
        assert_eq!(read_lifecycle_references(&repo, 64).unwrap_err().code, 2);
        std::fs::remove_file(&events_path).unwrap();
        std::fs::create_dir(&events_path).unwrap();
        assert_eq!(read_lifecycle_references(&repo, 64).unwrap_err().code, 2);
        std::fs::remove_dir_all(&lifecycle_dir).unwrap();

        // Malformed row fails closed with exit code 2.
        std::fs::write(repo.join(".oms/landings.jsonl"), b"{not json}\n").unwrap();
        let err = host.gc_landing_captures(&repo, 30, false).unwrap_err();
        assert_eq!(err.code, 2);

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn gc_dispatch_logs_defers_protects_active_and_reclaims_aged_settled() {
        if let Some(repo_raw) = std::env::var_os("OMS_GC_OVERSIZED_TEST_REPO") {
            let repo = PathBuf::from(repo_raw);
            let apply = std::env::var_os("OMS_GC_OVERSIZED_TEST_APPLY").is_some_and(|v| v == "1");
            let stopped = Arc::new(AtomicBool::new(false));
            let host = ControlServices::new(&stopped, &repo).unwrap();
            let mut runtime_svc =
                crate::services_runtime::RuntimeServices::new(&stopped, &repo).unwrap();
            let mode_flag = if apply { "--apply" } else { "--dry-run" };
            let gc_args = [
                OsString::from(mode_flag),
                OsString::from("--repo"),
                repo.as_os_str().to_owned(),
            ];
            let (code, stdout, stderr) = oms_command_state::run_with_host(
                "gc",
                &gc_args,
                &repo,
                &stopped,
                &mut runtime_svc,
                &host,
            );
            std::fs::write(repo.join("gc-child.code"), code.to_string()).unwrap();
            std::fs::write(repo.join("gc-child.stdout"), stdout).unwrap();
            std::fs::write(repo.join("gc-child.stderr"), stderr).unwrap();
            return;
        }

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

        std::fs::remove_file(plan_dir.join("tasks.json")).unwrap();
        std::fs::remove_file(&active_log).unwrap();
        std::fs::remove_file(&unsettled).unwrap();

        let lifecycle_dir = repo.join(".oms/lifecycle");
        std::fs::create_dir_all(&lifecycle_dir).unwrap();
        let events_path = lifecycle_dir.join("events.jsonl");

        let locks_dir = repo.join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();
        let set_locks_private = || {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&locks_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        set_locks_private();

        let run_gc = |target_repo: &Path, apply: bool| {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "services_control::gc_host_tests::gc_dispatch_logs_defers_protects_active_and_reclaims_aged_settled",
                    "--nocapture",
                ])
                .env("OMS_GC_OVERSIZED_TEST_REPO", target_repo)
                .env("OMS_GC_OVERSIZED_TEST_APPLY", if apply { "1" } else { "0" })
                .env("OMS_LOCK_DIR", &locks_dir)
                .env_remove("OMS_HARNESS_CHILD")
                .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
                .env_remove("OMS_STATE_REPO")
                .env_remove("OMS_FAIL_LEDGER")
                .env_remove("OMS_ATTEMPT_STALE_SECONDS")
                .status()
                .unwrap();
            assert!(status.success());
            let code: i32 = std::fs::read_to_string(target_repo.join("gc-child.code"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let stdout = std::fs::read(target_repo.join("gc-child.stdout")).unwrap();
            let stderr = std::fs::read(target_repo.join("gc-child.stderr")).unwrap();
            (code, stdout, stderr)
        };
        let run_gc_dry = |target_repo: &Path| run_gc(target_repo, false);

        // First prove that with a normal-sized lifecycle log, `gc --apply` reclaims
        // the aged released landing capture.
        let capture_landing_id = "land-20261120T000000Z-oversized-check";
        let capture_generation = "cd".repeat(16);
        let patch_bytes = b"diff --git a/x b/x\n";
        std::fs::write(
            &events_path,
            concat!(
                r#"{"schema":1,"event_id":"ev-norm-c","attempt_id":"att-norm-0","ts":"2026-11-20T00:00:00Z","seq":1,"event_type":"attempt.created","from_state":null,"to_state":"queued","task_id":"task-norm-0"}"#,
                "\n",
                r#"{"schema":1,"event_id":"ev-norm-s","attempt_id":"att-norm-0","ts":"2026-11-20T00:00:01Z","seq":2,"event_type":"attempt.state_changed","from_state":"queued","to_state":"cancelled"}"#,
                "\n",
            ),
        )
        .unwrap();
        let (normal_cap_dir, normal_cap_patch, normal_cap_rel) = seed_released_landing_capture(
            &repo,
            capture_landing_id,
            &capture_generation,
            patch_bytes,
        );
        set_locks_private();
        let (norm_code, norm_stdout, norm_stderr) = run_gc(&repo, true);
        assert_eq!(
            norm_code,
            0,
            "normal gc --apply failed: {}",
            String::from_utf8_lossy(&norm_stderr)
        );
        let norm_out = String::from_utf8_lossy(&norm_stdout);
        assert!(
            norm_out
                .lines()
                .any(|line| line == format!("- landing-capture: {normal_cap_rel}")),
            "released capture should be reclaimed with a normal lifecycle log: {norm_out}"
        );
        assert!(!normal_cap_patch.exists());
        assert!(!normal_cap_dir.exists());

        // Now re-seed the exact same released capture and build an oversized (>32 MiB)
        // valid lifecycle log at runtime (without referencing `capture_landing_id`, so
        // only the size-bound deferral protects the capture).
        let pad = "a".repeat(220);
        let refs_json = (0..32)
            .map(|k| format!(r#""note_{k:02}":"{pad}""#))
            .collect::<Vec<_>>()
            .join(",");
        let min_oversized = 32 * 1024 * 1024 + 4096;
        let mut events_bytes = Vec::with_capacity(min_oversized + 16 * 1024);
        let mut idx = 0usize;
        while events_bytes.len() <= 32 * 1024 * 1024 {
            use std::io::Write as _;
            writeln!(
                events_bytes,
                r#"{{"schema":1,"event_id":"ev-c-{idx}","attempt_id":"att-term-{idx}","ts":"2026-11-20T00:00:00Z","seq":1,"event_type":"attempt.created","from_state":null,"to_state":"queued","task_id":"task-term-{idx}","refs":{{{refs_json}}}}}"#,
            )
            .unwrap();
            writeln!(
                events_bytes,
                r#"{{"schema":1,"event_id":"ev-s-{idx}","attempt_id":"att-term-{idx}","ts":"2026-11-20T00:00:01Z","seq":2,"event_type":"attempt.state_changed","from_state":"queued","to_state":"cancelled"}}"#,
            )
            .unwrap();
            idx += 1;
        }
        {
            use std::io::Write as _;
            writeln!(
                events_bytes,
                r#"{{"schema":1,"event_id":"ev-live-oversized","attempt_id":"att-live-oversized","ts":"2026-11-20T00:00:02Z","seq":1,"event_type":"attempt.created","from_state":null,"to_state":"queued","task_id":"task-live-oversized"}}"#,
            )
            .unwrap();
        }
        assert!(events_bytes.len() > 32 * 1024 * 1024);
        std::fs::write(&events_path, &events_bytes).unwrap();
        drop(events_bytes);

        let live_attempt_log = logs_dir.join("att-live-oversized.log");
        let term_attempt_log = logs_dir.join("att-term-0.log");
        for path in [&live_attempt_log, &term_attempt_log] {
            std::fs::write(path, b"{}\n").unwrap();
            age_file(path, 60);
        }

        let (cap_dir, cap_patch, cap_rel) = seed_released_landing_capture(
            &repo,
            capture_landing_id,
            &capture_generation,
            patch_bytes,
        );
        set_locks_private();

        let (gc_code, gc_stdout, gc_stderr) = run_gc_dry(&repo);
        assert_eq!(
            gc_code,
            0,
            "gc --dry-run failed: {}",
            String::from_utf8_lossy(&gc_stderr)
        );
        let gc_out = String::from_utf8_lossy(&gc_stdout);
        assert!(
            gc_out.lines().any(
                |line| line == "- dispatch-logs: .oms/panel-state/dispatch-logs/att-term-0.log"
            ),
            "aged terminal-attempt log should be listed for reclaim: {gc_out}"
        );
        assert!(
            !gc_out.contains("att-live-oversized.log"),
            "non-terminal attempt log must stay protected: {gc_out}"
        );
        assert!(
            !gc_out.contains(&cap_rel),
            "oversized lifecycle log must not schedule landing capture for reclaim: {gc_out}"
        );
        let advisory_lines: Vec<&str> = gc_out
            .lines()
            .filter(|line| line.contains(".oms/lifecycle/events.jsonl"))
            .collect();
        assert_eq!(advisory_lines.len(), 1, "unexpected gc output: {gc_out}");
        assert!(
            advisory_lines[0].contains("oms agent-events compact")
                && advisory_lines[0].contains("--apply"),
            "advisory should suggest compact: {}",
            advisory_lines[0]
        );
        assert!(term_attempt_log.exists());
        assert!(live_attempt_log.exists());
        assert!(
            gc_out
                .contains("landing capture retention deferred: lifecycle references exceed 32 MiB")
        );
        assert_eq!(std::fs::read(&cap_patch).unwrap(), patch_bytes);

        // Run `gc --apply` against the oversized log: the released capture survives,
        // landing capture retention reports deferred, and other gc steps still apply.
        let (apply_code, apply_stdout, apply_stderr) = run_gc(&repo, true);
        assert_eq!(
            apply_code,
            0,
            "oversized gc --apply failed: {}",
            String::from_utf8_lossy(&apply_stderr)
        );
        let apply_out = String::from_utf8_lossy(&apply_stdout);
        assert!(
            apply_out.lines().any(
                |line| line == "- dispatch-logs: .oms/panel-state/dispatch-logs/att-term-0.log"
            ),
            "other gc steps must still reclaim eligible items under --apply: {apply_out}"
        );
        assert!(
            !apply_out.contains("att-live-oversized.log"),
            "non-terminal attempt log must stay protected under --apply: {apply_out}"
        );
        assert!(
            !apply_out.contains(&cap_rel),
            "deferred landing capture must not be reclaimed under --apply: {apply_out}"
        );
        assert!(
            apply_out
                .contains("landing capture retention deferred: lifecycle references exceed 32 MiB"),
            "landing capture deferral should be reported under --apply: {apply_out}"
        );
        let apply_advisories: Vec<&str> = apply_out
            .lines()
            .filter(|line| line.contains(".oms/lifecycle/events.jsonl"))
            .collect();
        assert_eq!(
            apply_advisories.len(),
            1,
            "unexpected gc --apply output: {apply_out}"
        );
        assert!(!term_attempt_log.exists());
        assert!(live_attempt_log.exists());
        assert!(cap_dir.is_dir());
        assert_eq!(std::fs::read(&cap_patch).unwrap(), patch_bytes);

        for apply in [false, true] {
            assert!(matches!(
                host.gc_landing_captures(&repo, 30, apply).unwrap(),
                GcHookResult::Deferred(reason) if reason.contains("lifecycle references exceed 32 MiB")
            ));
            assert_eq!(std::fs::read(&cap_patch).unwrap(), patch_bytes);
        }
        assert_eq!(
            host.gc_dispatch_logs(&repo, 30, false).unwrap(),
            GcHookResult::Reclaimed(vec![])
        );

        // Malformed, non-regular and symlinked lifecycle logs still fail closed,
        // through landing/dispatch hooks and through the gc planner's own
        // `lifecycle_events_len` check at `gc.rs`.
        std::fs::write(&events_path, b"{not valid json}\n").unwrap();
        let bad_err = host.gc_dispatch_logs(&repo, 30, false).unwrap_err();
        assert_eq!(bad_err.code, 2);
        assert_eq!(run_gc_dry(&repo).0, 2);

        std::fs::remove_file(&events_path).unwrap();
        std::fs::create_dir(&events_path).unwrap();
        assert_eq!(
            host.gc_landing_captures(&repo, 30, false).unwrap_err().code,
            2
        );
        let non_reg_err = host.gc_dispatch_logs(&repo, 30, false).unwrap_err();
        assert_eq!(non_reg_err.code, 2);
        assert_eq!(run_gc_dry(&repo).0, 2);

        std::fs::remove_dir(&events_path).unwrap();
        let outside = repo.join("outside-events.jsonl");
        std::fs::write(&outside, b"").unwrap();
        std::os::unix::fs::symlink(&outside, &events_path).unwrap();
        assert_eq!(
            host.gc_landing_captures(&repo, 30, false).unwrap_err().code,
            2
        );
        let symlink_err = host.gc_dispatch_logs(&repo, 30, false).unwrap_err();
        assert_eq!(symlink_err.code, 2);
        assert_eq!(run_gc_dry(&repo).0, 2);

        // Remove both `logs_dir` and landing state so `run_gc_dry` skips
        // `host.gc_landing_captures` and `host.gc_dispatch_logs` and exercises
        // the planner's own `lifecycle_events_len` path in `gc.rs`.
        std::fs::remove_dir_all(&logs_dir).unwrap();
        std::fs::remove_file(repo.join(".oms/landings.jsonl")).unwrap();
        std::fs::remove_dir_all(repo.join(".oms/landing-patches")).unwrap();

        let (sym_code, _, sym_stderr) = run_gc_dry(&repo);
        assert_eq!(sym_code, 2);
        assert!(
            String::from_utf8_lossy(&sym_stderr).contains("refusing symlinked path:"),
            "expected planner symlink error, got: {}",
            String::from_utf8_lossy(&sym_stderr)
        );

        std::fs::remove_file(&events_path).unwrap();
        std::fs::create_dir(&events_path).unwrap();
        let (dir_code, _, dir_stderr) = run_gc_dry(&repo);
        assert_eq!(dir_code, 2);
        assert!(
            String::from_utf8_lossy(&dir_stderr).contains("refusing non-regular file:"),
            "expected planner non-regular error, got: {}",
            String::from_utf8_lossy(&dir_stderr)
        );

        std::fs::remove_dir(&events_path).unwrap();
        std::fs::remove_dir(&lifecycle_dir).unwrap();
        std::fs::write(&lifecycle_dir, b"not a directory\n").unwrap();
        let (nondir_code, _, nondir_stderr) = run_gc_dry(&repo);
        assert_eq!(nondir_code, 2);
        assert!(
            String::from_utf8_lossy(&nondir_stderr)
                .contains("refusing non-directory .oms/lifecycle"),
            "expected planner non-directory .oms/lifecycle error, got: {}",
            String::from_utf8_lossy(&nondir_stderr)
        );
        std::fs::remove_file(&lifecycle_dir).unwrap();
        std::fs::create_dir_all(&lifecycle_dir).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&lifecycle_dir, std::fs::Permissions::from_mode(0o750))
                .unwrap();
        }

        std::fs::write(&events_path, b"{not valid json}\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&events_path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        let (malformed_code, _, malformed_stderr) = run_gc_dry(&repo);
        assert_eq!(malformed_code, 2);
        assert!(
            !malformed_stderr.is_empty(),
            "expected planner lifecycle parse error on stderr"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn gc_dispatch_logs_reads_object_and_array_plans_and_rejects_malformed() {
        let repo = temp_repo("dispatch-schema3");
        let logs_dir = repo.join(".oms/panel-state/dispatch-logs");
        let plan_dir = repo.join(".oms/plan");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::create_dir_all(&plan_dir).unwrap();

        let active_id_log = logs_dir.join("task-active.log");
        let active_attempt_log = logs_dir.join("att-live-1.log");
        let done_log = logs_dir.join("task-done.log");
        for path in [&active_id_log, &active_attempt_log, &done_log] {
            std::fs::write(path, b"log\n").unwrap();
            age_file(path, 60);
        }

        let stopped = Arc::new(AtomicBool::new(false));
        let host = ControlServices::new(&stopped, &repo).unwrap();

        // Schema-3 object-form plan protects active task tokens and reclaims done task logs.
        std::fs::write(
            plan_dir.join("tasks.json"),
            br#"{"schema":3,"plan_id":"plan_test_schema3","tasks":{"task-active":{"id":"task-active","state":"running","attempt_id":"att-live-1"},"task-done":{"id":"task-done","state":"done","attempt_id":"att-old-1"}}}"#,
        )
        .unwrap();
        let dry = host.gc_dispatch_logs(&repo, 30, false).unwrap();
        assert_eq!(
            dry,
            GcHookResult::Reclaimed(vec![(
                "dispatch-logs".to_string(),
                ".oms/panel-state/dispatch-logs/task-done.log".to_string(),
            )])
        );
        let applied = host.gc_dispatch_logs(&repo, 30, true).unwrap();
        assert_eq!(dry, applied);
        assert!(!done_log.exists());
        assert!(active_id_log.exists());
        assert!(active_attempt_log.exists());

        // Legacy top-level array form and object-with-tasks-array form continue to work.
        std::fs::write(&done_log, b"log\n").unwrap();
        age_file(&done_log, 60);
        for legacy_plan in [
            br#"[{"id":"task-active","state":"running","attempt_id":"att-live-1"},{"id":"task-done","state":"done"}]"#.as_slice(),
            br#"{"schema":2,"tasks":[{"id":"task-active","state":"running","attempt_id":"att-live-1"},{"id":"task-done","state":"done"}]}"#.as_slice(),
        ] {
            std::fs::write(plan_dir.join("tasks.json"), legacy_plan).unwrap();
            let dry_legacy = host.gc_dispatch_logs(&repo, 30, false).unwrap();
            assert_eq!(
                dry_legacy,
                GcHookResult::Reclaimed(vec![(
                    "dispatch-logs".to_string(),
                    ".oms/panel-state/dispatch-logs/task-done.log".to_string(),
                )])
            );
        }

        // Schema-3 plan with a non-object task entry is rejected.
        std::fs::write(
            plan_dir.join("tasks.json"),
            br#"{"schema":3,"plan_id":"plan_test_schema3","tasks":{"bad":"not-an-object"}}"#,
        )
        .unwrap();
        let err = host.gc_dispatch_logs(&repo, 30, false).unwrap_err();
        assert_eq!(err.code, 2);

        // A tasks field that is neither an array nor an object is rejected.
        std::fs::write(
            plan_dir.join("tasks.json"),
            br#"{"schema":3,"plan_id":"plan_test_schema3","tasks":"bad"}"#,
        )
        .unwrap();
        let err = host.gc_dispatch_logs(&repo, 30, false).unwrap_err();
        assert_eq!(err.code, 2);

        // The plan engine ignores the stored schema, so object-form tasks read under any schema;
        // an active entry without an inner id is still protected by its key.
        std::fs::write(
            plan_dir.join("tasks.json"),
            br#"{"schema":2,"tasks":{"task-active":{"state":"running","attempt_id":"att-live-1"},"task-done":{"id":"task-done","state":"done"}}}"#,
        )
        .unwrap();
        assert_eq!(
            host.gc_dispatch_logs(&repo, 30, false).unwrap(),
            GcHookResult::Reclaimed(vec![(
                "dispatch-logs".to_string(),
                ".oms/panel-state/dispatch-logs/task-done.log".to_string(),
            )])
        );

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

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[usize::from(b >> 4)] as char);
        out.push(HEX[usize::from(b & 0x0f)] as char);
    }
    out
}

fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    let raw = text.as_bytes();
    if !raw.len().is_multiple_of(2) {
        return Err("odd hex byte length".into());
    }
    let nibble = |b: u8| -> Result<u8, String> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err("invalid hex digit".into()),
        }
    };
    let mut out = Vec::with_capacity(raw.len() / 2);
    for &[hi, lo] in raw.as_chunks::<2>().0 {
        out.push((nibble(hi)? << 4) | nibble(lo)?);
    }
    Ok(out)
}

#[cfg(target_os = "linux")]
pub(crate) fn supervise_worker_request(
    spawn: &Spawn<'_>,
    acknowledge_start: bool,
) -> Result<Vec<u8>, String> {
    let argv: Vec<String> = spawn
        .argv
        .iter()
        .map(|a| hex_encode(a.as_bytes()))
        .collect();
    let cwd = spawn.cwd.map(|p| hex_encode(p.as_os_str().as_bytes()));
    let environment: Vec<[String; 2]> = spawn
        .environment
        .iter()
        .map(|(k, v)| [hex_encode(k.as_bytes()), hex_encode(v.as_bytes())])
        .collect();
    let remove_environment: Vec<String> = spawn
        .remove_environment
        .iter()
        .map(|k| hex_encode(k.as_bytes()))
        .collect();
    let payload = serde_json::json!({
        "schema": 1,
        "acknowledge_start": acknowledge_start,
        "argv": argv,
        "cwd": cwd,
        "clear_environment": spawn.clear_environment,
        "environment": environment,
        "remove_environment": remove_environment,
        "merged": spawn.merged,
        "drain": spawn.drain,
        "stdin": hex_encode(&spawn.stdin),
        "wall_ms": u64::try_from(spawn.wall.as_millis().max(1)).map_err(|_| "execution deadline overflow")?,
        "limit": spawn.limit,
        "terminate_grace_ms": u64::try_from(spawn.terminate_grace.as_millis()).map_err(|_| "terminate grace overflow")?,
    });
    let encoded = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
    if encoded.len() > SUPERVISE_REQUEST_LIMIT {
        return Err("supervised request exceeds IPC byte limit".into());
    }
    Ok(encoded)
}

#[cfg(target_os = "linux")]
pub(crate) fn run_via_supervise_worker(
    worker: &[OsString],
    spawn: &Spawn<'_>,
    interrupted: &AtomicBool,
) -> Result<Child, String> {
    let encoded = supervise_worker_request(spawn, false)?;
    let outer_limit = spawn
        .limit
        .saturating_mul(4)
        .saturating_add(256 * 1024)
        .clamp(1024 * 1024, 128 * 1024 * 1024);
    let mut outer = Spawn::new(
        Scope::ProcessGroup,
        worker.to_vec(),
        spawn.wall.saturating_add(Duration::from_secs(30)),
        outer_limit,
    )
    .terminate_grace(spawn.terminate_grace.saturating_add(Duration::from_secs(5)));
    outer.cwd = spawn.cwd;
    outer.stdin = encoded;
    let done = run_child(outer, interrupted)?;
    if done.output_limited || done.timed_out {
        return Err("native supervise worker exceeded bounds".into());
    }
    if done.stdout.is_empty() && done.code == 130 {
        return Ok(Child {
            code: 130,
            stdout: Vec::new(),
            stderr: Vec::new(),
            elapsed: done.elapsed,
            timed_out: false,
            output_limited: false,
            cancelled: true,
            dropped: 0,
        });
    }
    supervise_worker_result(&done.stdout).map_err(|error| {
        let detail = String::from_utf8_lossy(&done.stderr);
        let detail = detail.trim();
        if detail.is_empty() {
            error
        } else {
            detail.to_owned()
        }
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn supervise_worker_result(raw: &[u8]) -> Result<Child, String> {
    let result: Value = serde_json::from_slice(raw)
        .map_err(|_| "native supervise worker returned invalid completion".to_owned())?;
    if result["ok"] != true {
        return Err(result["error"]
            .as_str()
            .unwrap_or("native supervise worker refused")
            .to_owned());
    }
    let code = result["code"]
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .ok_or("native supervise worker returned invalid exit code")?;
    let stdout = hex_decode(
        result["stdout"]
            .as_str()
            .ok_or("native supervise worker returned invalid stdout")?,
    )?;
    let stderr = hex_decode(
        result["stderr"]
            .as_str()
            .ok_or("native supervise worker returned invalid stderr")?,
    )?;
    let elapsed_ms = result["elapsed_ms"]
        .as_u64()
        .ok_or("native supervise worker returned invalid elapsed time")?;
    Ok(Child {
        code,
        stdout,
        stderr,
        elapsed: Duration::from_millis(elapsed_ms),
        timed_out: result["timed_out"] == true,
        output_limited: result["output_limited"] == true,
        cancelled: result["cancelled"] == true,
        dropped: result["dropped"].as_u64().unwrap_or(0),
    })
}

/// Entry point of the single-threaded child supervisor trampoline
/// ([`SUPERVISE_WORKER_FLAG`]). Reads one JSON `Spawn` descriptor from stdin,
/// runs it inside [`Scope::WholeTree`] on Linux, and writes one JSON outcome to stdout.
pub(crate) fn supervise_worker_main(args: &[OsString], interrupted: &AtomicBool) -> i32 {
    use std::io::{Read, Write};
    let report_json = |payload: Value| -> i32 {
        match serde_json::to_vec(&payload) {
            Ok(bytes) => {
                let mut out = std::io::stdout().lock();
                if out.write_all(&bytes).is_err() || out.flush().is_err() {
                    return 1;
                }
                0
            }
            Err(_) => 1,
        }
    };
    if !args.is_empty() {
        return report_json(
            serde_json::json!({"ok": false, "error": "supervise worker takes no arguments"}),
        );
    }
    let mut raw = Vec::new();
    if let Err(e) = std::io::stdin()
        .take((SUPERVISE_REQUEST_LIMIT + 1) as u64)
        .read_to_end(&mut raw)
    {
        return report_json(serde_json::json!({"ok": false, "error": e.to_string()}));
    }
    if raw.len() > SUPERVISE_REQUEST_LIMIT {
        return report_json(
            serde_json::json!({"ok": false, "error": "supervise request exceeds size limit"}),
        );
    }
    let parsed: Value = match serde_json::from_slice::<Value>(&raw) {
        Ok(v) if v["schema"] == 1 => v,
        _ => {
            return report_json(
                serde_json::json!({"ok": false, "error": "invalid supervise request"}),
            );
        }
    };
    let decode_vec = |key: &str| -> Result<Vec<OsString>, String> {
        parsed[key]
            .as_array()
            .ok_or_else(|| format!("missing {key}"))?
            .iter()
            .map(|item| {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("invalid {key} entry"))?;
                Ok(OsString::from_vec(hex_decode(s)?))
            })
            .collect()
    };
    let argv = match decode_vec("argv") {
        Ok(v) => v,
        Err(e) => return report_json(serde_json::json!({"ok": false, "error": e})),
    };
    let cwd = match parsed.get("cwd").and_then(|v| v.as_str()) {
        Some(hex) => match hex_decode(hex) {
            Ok(bytes) => Some(PathBuf::from(OsString::from_vec(bytes))),
            Err(e) => return report_json(serde_json::json!({"ok": false, "error": e})),
        },
        None => None,
    };
    let environment: Vec<(OsString, OsString)> = match parsed["environment"]
        .as_array()
        .ok_or_else(|| "missing environment".to_owned())
        .and_then(|items| {
            items
                .iter()
                .map(|pair| {
                    let k = pair[0]
                        .as_str()
                        .ok_or_else(|| "invalid env key".to_owned())?;
                    let v = pair[1]
                        .as_str()
                        .ok_or_else(|| "invalid env value".to_owned())?;
                    Ok((
                        OsString::from_vec(hex_decode(k)?),
                        OsString::from_vec(hex_decode(v)?),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()
        }) {
        Ok(v) => v,
        Err(e) => return report_json(serde_json::json!({"ok": false, "error": e})),
    };
    let remove_environment = match decode_vec("remove_environment") {
        Ok(v) => v,
        Err(e) => return report_json(serde_json::json!({"ok": false, "error": e})),
    };
    let stdin = match parsed.get("stdin").and_then(|v| v.as_str()) {
        Some(hex) => match hex_decode(hex) {
            Ok(bytes) => bytes,
            Err(e) => return report_json(serde_json::json!({"ok": false, "error": e})),
        },
        None => Vec::new(),
    };
    let (Some(merged), Some(wall_ms), Some(limit), Some(grace_ms)) = (
        parsed["merged"].as_bool(),
        parsed["wall_ms"].as_u64(),
        parsed["limit"]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok()),
        parsed["terminate_grace_ms"].as_u64(),
    ) else {
        return report_json(serde_json::json!({"ok": false, "error": "invalid supervise bounds"}));
    };
    #[cfg(target_os = "linux")]
    let scope = Scope::WholeTree;
    #[cfg(not(target_os = "linux"))]
    let scope = Scope::ProcessGroup;
    let mut spawn = Spawn::new(scope, argv, Duration::from_millis(wall_ms), limit)
        .environment(environment)
        .terminate_grace(Duration::from_millis(grace_ms));
    spawn.cwd = cwd.as_deref();
    spawn.clear_environment = parsed["clear_environment"] == true;
    spawn.remove_environment = remove_environment;
    spawn.merged = merged;
    spawn.drain = parsed["drain"] == true;
    spawn.stdin = stdin;
    let started = || {
        if parsed["acknowledge_start"] == true {
            let mut out = std::io::stdout().lock();
            out.write_all(SUPERVISE_STARTED)
                .and_then(|()| out.flush())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    match run_child_started(spawn, interrupted, started) {
        Ok(child) => report_json(serde_json::json!({
            "ok": true,
            "code": child.code,
            "stdout": hex_encode(&child.stdout),
            "stderr": hex_encode(&child.stderr),
            "elapsed_ms": u64::try_from(child.elapsed.as_millis()).unwrap_or(u64::MAX),
            "timed_out": child.timed_out,
            "output_limited": child.output_limited,
            "cancelled": child.cancelled,
            "dropped": child.dropped,
        })),
        Err(error) => report_json(serde_json::json!({
            "ok": false,
            "error": error,
        })),
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
    fn failure_candidates(
        &mut self,
        repo: &Path,
        active_task_id: &[u8],
        command: &[u8],
    ) -> oms_command_task::Result<Vec<String>> {
        oms_command_state::task_failure_candidates(repo, active_task_id, command, self.interrupted)
            .map_err(|error| task_error(error.to_string()))
    }
    fn observe_failure(
        &mut self,
        request: FailureObservation<'_>,
    ) -> oms_command_task::Result<Vec<u8>> {
        let command = OsString::from_vec(request.command.to_vec());
        let mut args: Vec<OsString> = vec!["--repo".into(), request.repo.as_os_str().to_owned()];
        if request.exit == 0 {
            // The ledger records the accepted rerun and clears the aggregate
            // only when its exact evidence covers every currently open row.
            args.extend(["resolve".into(), "--cmd".into(), command]);
            let _ = oms_command_state::observe_task_verification(
                &args,
                request.repo,
                self.interrupted,
                request.verification,
            );
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
        let (code, stdout, stderr) = oms_command_state::observe_task_verification(
            &args,
            request.repo,
            self.interrupted,
            request.verification,
        );
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
    fn data_segment_limit() -> (Option<u64>, Option<u64>) {
        let limit = rustix::process::getrlimit(rustix::process::Resource::Data);
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

    #[cfg(target_os = "linux")]
    #[test]
    fn batch_bubblewrap_status_discriminates_real_exit_and_setup_refusal() {
        let checkout = Tmp::new();
        let mut mounts: Vec<_> = ["/usr", "/bin", "/lib", "/lib64", "/etc/ld.so.cache"]
            .into_iter()
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .map(|path| (path.clone(), path))
            .collect();
        let run = |mounts: &[(PathBuf, PathBuf)], script: &str| -> Option<Child> {
            let (wrapper, arguments) = oms_supervisor::sandbox::isolated_checkout(
                std::ffi::OsStr::new("/bin/sh"),
                &osv(&["-c", script]),
                checkout.path(),
                mounts,
            )
            .ok()?;
            let argv = std::iter::once(wrapper).chain(arguments).collect();
            let mut spawn = Spawn::new(Scope::ProcessGroup, argv, Duration::from_secs(5), 4096)
                .cwd(checkout.path());
            spawn.clear_environment = true;
            Some(run_child(spawn, &AtomicBool::new(false)).unwrap())
        };
        let Some(probe) = run(&mounts, "exit 0") else {
            eprintln!("system bubblewrap unavailable; status fixture skipped");
            return;
        };
        if probe.code != 0 {
            eprintln!("bubblewrap namespaces unavailable; status fixture skipped");
            return;
        }
        assert_eq!(
            batch_verification_result(probe).outcome,
            VerificationOutcome::Passed
        );
        let child = run(&mounts, "printf 'private fixture output'; exit 1").unwrap();
        assert_eq!(child.code, 1);
        assert_eq!(child.stderr, b"private fixture output");
        let status = batch_verification_result(child);
        assert_eq!(status.outcome, VerificationOutcome::Failed);
        assert_eq!(status.verifier_exit, Some(1));

        mounts.push((
            checkout.path().join("missing-mount"),
            "/missing-mount".into(),
        ));
        let child = run(&mounts, "exit 1").unwrap();
        assert_eq!(child.code, 1);
        let status = batch_verification_result(child);
        assert_eq!(status.outcome, VerificationOutcome::Unavailable);
        assert_eq!(status.verifier_exit, None);
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
        let data_before = data_segment_limit();
        control
            .publication_scan(&scan_request(repo.path(), &base, &head, 4 << 30), &stop)
            .expect("clean range passes");
        assert_eq!(before, address_space_limit());
        assert_eq!(data_before, data_segment_limit());
        // A starved memory budget cannot pass: the worker fails closed.
        let starved = if cfg!(target_os = "macos") {
            1
        } else {
            1 << 20
        };
        assert!(
            control
                .publication_scan(&scan_request(repo.path(), &base, &head, starved), &stop)
                .is_err()
        );
        assert_eq!(before, address_space_limit());
        assert_eq!(data_before, data_segment_limit());
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
        assert!(
            leak.message.contains("sensitive-publish-payload"),
            "{}",
            leak.message
        );
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
        for exit in [0, 1, 7] {
            let plan = oms_command_plan::Services::supervise(
                &control,
                &SupervisedCommand {
                    argv: sh(&format!("printf plan; exit {exit}")),
                    cwd: root.path().to_owned(),
                    wall: Duration::from_secs(20),
                    output_limit: 1024,
                    label: "t",
                    remove_environment: Vec::new(),
                    environment: Vec::new(),
                },
            )
            .unwrap();
            assert_eq!(
                (plan.exit, plan.runner_exit, plan.output.as_slice()),
                (exit, exit, b"plan".as_slice())
            );
            assert!(
                !plan.launch_error
                    && !plan.supervision_error
                    && !plan.timed_out
                    && !plan.output_limited
            );
        }
        // Exercise the actual acceptance consumer too: an ordinary failed
        // check must remain a failure, so goal-drive can dispatch the first
        // worker. The Plan supervision-mismatch guard is left intact.
        let repo = Tmp::repo(&[(".gitignore", ".oms/\n"), ("check.sh", "exit 1\n")]);
        let plan_call = |args: &[&str]| {
            oms_command_plan::run_with_services(
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
                repo.path(),
                &stop,
                &control,
            )
        };
        let (code, out, err) = plan_call(&[
            "init",
            "--goal",
            "make the failing check pass",
            "--accept",
            "bash check.sh",
            "--accept-files",
            "check.sh",
        ]);
        assert_eq!(
            code,
            0,
            "{}{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        let (code, out, err) = plan_call(&["accept"]);
        assert_eq!(
            code,
            3,
            "{}{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        assert!(String::from_utf8_lossy(&out).contains("plan-accept: fail (exit 1,"));
        assert!(!String::from_utf8_lossy(&out).contains("acceptance-supervision-failed"));
    }

    #[test]
    fn goal_commit_reads_only_certified_managed_landing_patches() {
        use oms_command_plan::{Action, Context, Plan, Request, json};
        use std::os::unix::fs::MetadataExt;

        let project = "# Project\n## Status\n- State: confirmed\n## Goal\n- Goal: commit the exact landed patch\n## Scope\n- Allowed: a.txt\n## Acceptance\n- [id:C1] The assigned change is checked\n## Verification\n- Required checks: bash check.sh\n- Required check files: check.sh\n";
        let repo = Tmp::repo(&[
            (".gitignore", ".oms/\n"),
            ("a.txt", "before\n"),
            ("check.sh", "test \"$(cat a.txt)\" = after\n"),
            ("PROJECT.md", project),
        ]);
        git(repo.path(), &["config", "user.name", "t"]);
        git(repo.path(), &["config", "user.email", "t@example.invalid"]);
        let spec_sha = json::sha(project.as_bytes());
        let branch = format!("oms/autopilot-{}", &spec_sha[..12]);
        git(repo.path(), &["switch", "-qc", &branch]);
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, repo.path(), Vec::new());
        let context = Context::from_environment_at(
            &Request::new(Action::Status),
            repo.path(),
            repo.path(),
            None,
        )
        .unwrap();
        let plan = Plan::open(context, &control, &stop).unwrap();
        plan.execute(
            &Request::new(Action::Init)
                .with("goal", "commit the exact landed patch")
                .with("accept", "bash check.sh"),
        )
        .unwrap();
        let ordinary = oms_command_plan::storage::Store::open(repo.path()).unwrap();
        let base = git(repo.path(), &["rev-parse", "HEAD"]);
        let plan_sha = json::sha(plan.load().unwrap().raw.unwrap());
        let proposal = serde_json::json!({"schema":1,"kind":"agent-plan-proposal","spec_sha256":spec_sha,"plan_sha256":plan_sha,"base_sha":base,"id_prefix":"t","allowed_envelope":["a.txt"],"acceptance_files":["check.sh"],
            "tasks":[{"id":"t1","title":"implement the assigned change","allowed":["a.txt"],"verify":"bash check.sh","depends":[],"covers":["C1"]}]});
        let proposal_raw = serde_json::to_vec(&proposal).unwrap();
        let proposal_sha = json::sha(&proposal_raw);
        let proposal_path = repo.path().join(".oms/plan/proposal-milestone.json");
        ordinary
            .publish_once(&proposal_path, &proposal_raw, 1024 * 1024)
            .unwrap();
        plan.execute(
            &Request::new(Action::ApplyProposal)
                .with("proposal", proposal_path.as_os_str())
                .with("expected-proposal-sha256", &proposal_sha)
                .with("expected-plan-sha256", &plan_sha)
                .with("allowed-envelope", "a.txt")
                .with("accept-files", "check.sh"),
        )
        .unwrap();
        let issued = plan
            .execute(
                &Request::new(Action::Claim)
                    .with("id", "t1")
                    .with("provider", "claude")
                    .flag("json"),
            )
            .unwrap();
        let issued = json::parse(&issued.stdout).unwrap();
        let lease = issued.text("lease_id");
        std::fs::write(repo.path().join("a.txt"), "after\n").unwrap();
        let patch = format!("{}\n", git(repo.path(), &["diff", "--binary"]));
        git(repo.path(), &["checkout", "--", "a.txt"]);
        let source = repo.path().join(".oms/source.patch");
        let report = repo.path().join(".oms/report.md");
        std::fs::write(&source, &patch).unwrap();
        std::fs::write(&report, "The assigned change passed its check.\n").unwrap();
        let digest = json::sha(patch.as_bytes());
        let landing = "land-20261123T000000Z-0123456789abcdef";
        let frozen = repo
            .path()
            .join(format!(".oms/landing-patches/{landing}.patch"));
        let capture = {
            let mut producer = oms_reference::producer::Producer::open(repo.path(), &stop).unwrap();
            let capture = producer
                .allocate(&source, landing, &digest, repo.path(), &stop)
                .unwrap();
            producer.publish(&capture, &frozen, &stop).unwrap();
            capture
        };
        let _capture = Tmp(capture.parent().unwrap().to_owned());
        assert_eq!(std::fs::metadata(&frozen).unwrap().nlink(), 2);
        // The general Plan store must continue refusing multiply linked files.
        assert!(
            ordinary
                .read(&frozen, 16 * 1024 * 1024)
                .unwrap_err()
                .message
                .contains("unsafe plan ownership, permissions or links")
        );
        plan.execute(
            &Request::new(Action::Review)
                .with("id", "t1")
                .with("lease-id", lease)
                .with("artifact", report.as_os_str())
                .with("patch", source.as_os_str()),
        )
        .unwrap();
        plan.execute(
            &Request::new(Action::Land)
                .with("id", "t1")
                .with("lease-id", lease)
                .with("expected-review-patch", source.as_os_str())
                .with("expected-review-patch-sha256", &digest)
                .with("expected-review-verify", "bash check.sh")
                .with("expected-review-executor-id", "")
                .with("expected-review-executor-soul-sha256", "")
                .with("expected-review-lease-id", lease),
        )
        .unwrap();
        let snapshot = plan.load().unwrap();
        let landing_receipt = json::receipt_digest(snapshot.task("t1").unwrap()).unwrap();
        plan.execute(
            &Request::new(Action::Finish)
                .with("id", "t1")
                .with("lease-id", lease)
                .with("expected-landing-receipt-sha256", &landing_receipt)
                .with("patch", frozen.as_os_str()),
        )
        .unwrap();
        let snapshot = plan.load().unwrap();
        let task = snapshot.task("t1").unwrap();
        assert_eq!(task.get("covers").strings().unwrap(), ["C1"]);
        assert!(task.get("completion_receipt").object().is_err());
        assert!(task.get("landing").object().is_err());
        let intent = serde_json::json!({"schema":1,"event":"intent","landing_id":landing,
            "task":"t1","worktree":repo.path(),"plan_id":snapshot.plan.text("plan_id"),
            "lease":lease,"plan_receipt_sha":landing_receipt,"plan_done_receipt_sha":json::receipt_digest(task).unwrap(),
            "patch_sha":digest,"base_sha":base,"patch":frozen,"approval":"","approval_version":""});
        let mut landing_record = json::parse(&serde_json::to_vec(&intent).unwrap()).unwrap();
        let hash = oms_command_plan::patch_landing_receipt_digest(&landing_record).unwrap();
        landing_record
            .set("receipt_sha", json::string(hash))
            .unwrap();
        ordinary
            .append(".oms/landings.jsonl", &landing_record)
            .unwrap();
        std::fs::write(repo.path().join("a.txt"), "after\n").unwrap();
        let run = || {
            oms_command_autonomy::run_with_services(
                "goal-drive",
                &["--commit-task".into(), "t1".into()],
                repo.path(),
                &stop,
                &control,
            )
        };
        let (code, _, _) = run();
        assert_ne!(code, 0, "outstanding intent cannot authorize a commit");
        assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), base);
        let ledger = repo.path().join(".oms/landings.jsonl");
        let before_terminal = std::fs::read(&ledger).unwrap();
        landing_record
            .set("event", json::string("complete"))
            .unwrap();
        ordinary
            .append(".oms/landings.jsonl", &landing_record)
            .unwrap();
        let (code, out, err) = run();
        assert_eq!(
            code,
            0,
            "{}{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        assert!(String::from_utf8_lossy(&out).contains("committed task=t1 commit="));
        let committed = git(repo.path(), &["rev-parse", "HEAD"]);
        assert_ne!(committed, base);
        assert_eq!(git(repo.path(), &["rev-parse", "HEAD^"]), base);
        assert!(git(repo.path(), &["status", "--porcelain"]).is_empty());

        // Read the actual local Finish through the public status and native
        // criterion projection. A task's coverage declaration is insufficient;
        // this execution feedback comes from a successful supervised check.
        let verified = oms_command_plan::Services::supervise(
            &control,
            &oms_command_plan::services::SupervisedCommand {
                argv: sh("bash check.sh"),
                cwd: repo.path().to_owned(),
                wall: Duration::from_secs(20),
                output_limit: 1024,
                label: "milestone-check",
                remove_environment: Vec::new(),
                environment: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!((verified.exit, verified.runner_exit), (0, 0));
        assert!(!verified.launch_error && !verified.supervision_error && !verified.timed_out);
        let source_path = ".oms/artifacts/_verify-scope-milestone.json";
        let source_bytes = serde_json::to_vec(
            &serde_json::json!({"head":committed,"exit":verified.exit,"covers":["C1"]}),
        )
        .unwrap();
        ordinary
            .publish_once(source_path, &source_bytes, 1024)
            .unwrap();
        ordinary
            .publish_once(".oms/artifacts/index.jsonl", b"", 0)
            .unwrap();
        let verification = serde_json::json!({"schema":1,"kind":"review-verify","event_id":"evt_milestone","operation_id":"review-milestone","provider":"local","verified_head":committed,
            "plan_id":snapshot.plan.text("plan_id"),"covers":["C1"],"source":source_path,"source_sha256":json::sha(&source_bytes),"exit":verified.exit,
            "dependency_digests":{"PROJECT.md":spec_sha,".oms/plan/tasks.json":json::sha(ordinary.read(".oms/plan/tasks.json", 8*1024*1024).unwrap())}});
        ordinary
            .append(
                ".oms/artifacts/index.jsonl",
                &json::parse(&serde_json::to_vec(&verification).unwrap()).unwrap(),
            )
            .unwrap();
        let route = serde_json::json!({"model":"fixture","fallback_model":"","reasoning_effort":"auto","timeout":"1m"});
        let receipt = serde_json::json!({"schema":1,"kind":"autopilot-run","stage":"parked","repo":repo.path(),"spec_sha256":spec_sha,
            "providers":{"planner":"claude","worker":"claude","reviewer":"codex"},"routing":{"provider_timeout":"1m","planner":route,"worker":route,"reviewer":route},
            "contract":{"allowed":"a.txt","base":"main","base_sha":base,"remote":"origin","max_cycles":4,"initial_tasks":1,"replan_tasks":1,"auto_repair":false,"retry_known":false,"review_mode":"gate","draft_pr":false,"allow_verifier_change":false,"allow_floor_change":"","collaboration":"off","milestones":[{"id":"first","criteria":["C1"]}]},
            "proposal":{"path":proposal_path,"sha256":proposal_sha},"branch":branch,"owner_id":format!("owner_{}","1".repeat(32)),"updated":"2026-11-23T00:00:00Z"});
        oms_command_autonomy::receipt::validate(&receipt).unwrap();
        ordinary
            .publish_once(
                ".oms/plan/autopilot-run.json",
                &serde_json::to_vec(&receipt).unwrap(),
                64 * 1024,
            )
            .unwrap();
        // The in-process test harness supplies only the repository view and
        // invokes the existing Plan owner directly. Whole-tree supervision is
        // exercised by the separate native_entry_contract milestone fixture.
        struct MilestoneHost<'a, 'b> {
            control: &'a ControlServices<'b>,
            stop: &'a AtomicBool,
        }
        impl oms_command_plan::Services for MilestoneHost<'_, '_> {
            fn read_managed_patch(
                &self,
                state: &Path,
                path: &Path,
                limit: usize,
            ) -> oms_command_plan::Result<Option<Vec<u8>>> {
                self.control.read_managed_patch(state, path, limit)
            }
            fn group_membership_resolver(
                &self,
            ) -> Option<oms_command_plan::services::GroupResolver> {
                self.control.group_membership_resolver()
            }
        }
        impl oms_runtime::Services for MilestoneHost<'_, '_> {
            fn repository_view(
                &self,
                repo: &Path,
            ) -> oms_runtime::Result<oms_runtime::services::RepositoryView> {
                Ok(oms_runtime::services::RepositoryView {
                    head: git(repo, &["rev-parse", "HEAD"]),
                    branch: git(repo, &["symbolic-ref", "--short", "HEAD"]),
                    worktree_state: String::new(),
                })
            }
            fn plan_status(
                &self,
                repo: &Path,
                state: &Path,
                _: &[u8],
            ) -> oms_runtime::Result<Vec<u8>> {
                assert_eq!(repo, state);
                let (code, out, err) = oms_command_plan::run_with_services(
                    &["status".into(), "--json".into()],
                    repo,
                    self.stop,
                    self.control,
                );
                assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
                Ok(out)
            }
            fn group_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
                self.control.group_membership_resolver()
            }
        }
        impl oms_command_autonomy::Services for MilestoneHost<'_, '_> {
            fn discovery_section(
                &self,
                repo: &Path,
                section: &str,
                stop: &AtomicBool,
            ) -> oms_command_autonomy::Result<Value> {
                assert_eq!(section, "acceptance");
                let runtime = oms_runtime::storage::Runtime::open(repo, repo, repo, self, stop)?;
                let projection = oms_runtime::projection::envelope(&runtime)?;
                oms_command_autonomy::value::parse(&oms_runtime::value::canonical(&projection)?)
            }
        }
        let status_host = MilestoneHost {
            control: &control,
            stop: &stop,
        };
        let criterion = || {
            let (code, out, err) = oms_command_autonomy::run_with_services(
                "autopilot",
                &["status".into()],
                repo.path(),
                &stop,
                &status_host,
            );
            assert_eq!(
                code,
                0,
                "{}{}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&err)
            );
            let status: Value = serde_json::from_slice(&out).unwrap();
            status["next_action"]["progress"]["criteria"]
                .as_array()
                .unwrap_or_else(|| panic!("{status}"))
                .iter()
                .find(|c| c["id"] == "C1")
                .unwrap()
                .clone()
        };
        let proof = criterion();
        assert_eq!(proof["state"], "verified", "{proof}");
        assert!(
            proof["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["status"] == "verified"
                    && e["stale"] == false
                    && e["signal"] == "execution_feedback")
        );
        assert_eq!(proof["claimed_tasks"], serde_json::json!(["t1"]));
        assert_eq!(proof["completed_tasks"], serde_json::json!(["t1"]));
        let valid = std::fs::read(&ledger).unwrap();
        std::fs::write(&ledger, b"").unwrap();
        assert_eq!(
            criterion()["completed_tasks"],
            serde_json::json!([]),
            "done alone is not completed landing proof"
        );
        for (field, foreign) in [
            ("lease", "lease-foreign".to_owned()),
            ("plan_id", format!("plan_{}", "f".repeat(32))),
            ("plan_done_receipt_sha", "f".repeat(64)),
            ("patch_sha", "f".repeat(64)),
            (
                "worktree",
                repo.path().join("foreign").to_string_lossy().into_owned(),
            ),
        ] {
            let mut changed = landing_record.clone();
            changed.set(field, json::string(foreign)).unwrap();
            std::fs::write(&ledger, &before_terminal).unwrap();
            ordinary.append(".oms/landings.jsonl", &changed).unwrap();
            assert_eq!(
                criterion()["completed_tasks"],
                serde_json::json!([]),
                "foreign {field} must not complete the task"
            );
        }
        std::fs::write(&ledger, &valid).unwrap();
        assert_eq!(criterion()["completed_tasks"], serde_json::json!(["t1"]));

        // A real publication's anchor identity alone is insufficient after its
        // bytes change. Re-entry must neither commit nor accept those bytes.
        std::fs::write(&frozen, "changed capture\n").unwrap();
        let (code, _, err) = run();
        assert_eq!(code, 2, "{}", String::from_utf8_lossy(&err));
        assert!(String::from_utf8_lossy(&err).contains("native generation anchor changed"));
        assert_eq!(criterion()["completed_tasks"], serde_json::json!([]));
        std::fs::write(&frozen, &patch).unwrap();

        // Matching bytes with two links do not create a native certificate.
        let retained = frozen.with_extension("retained");
        std::fs::rename(&frozen, &retained).unwrap();
        let foreign = repo.path().join(".oms/foreign.patch");
        std::fs::write(&foreign, &patch).unwrap();
        std::fs::hard_link(&foreign, &frozen).unwrap();
        assert_eq!(std::fs::metadata(&frozen).unwrap().nlink(), 2);
        let (code, _, err) = run();
        assert_eq!(code, 2, "{}", String::from_utf8_lossy(&err));
        assert!(String::from_utf8_lossy(&err).contains("landing publication replaced"));
        assert_eq!(criterion()["completed_tasks"], serde_json::json!([]));
        assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), committed);
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
            b"{}"
        );
        assert!(control.project_lifecycle(b"not json\n").is_err());
        assert!(control.group_membership_resolver().is_some());
        // No approval ledger entry: refused, never an empty grant.
        assert!(
            oms_command_plan::Services::landing_approval(&control, repo.path(), "apr-missing")
                .is_err()
        );
        assert!(oms_command_landing::Services::plan(&control).is_some());
        assert!(oms_command_landing::Services::admission_context(&control).is_some());
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
        let lifecycle = control.project_lifecycle(review_events.as_bytes()).unwrap();
        assert_eq!(lifecycle.attempts.object().unwrap().len(), 2);
        assert_eq!(
            lifecycle.attempts.get("att_review1").text("state"),
            "review"
        );
        assert_eq!(
            lifecycle.attempts.get("att_stale1").text("state"),
            "working"
        );
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
    #[test]
    fn team_contract_enrollment_uses_native_lifecycle_and_room_proofs() {
        use oms_command_operations::value::obj;
        use oms_command_operations::{StartRequest, append_lifecycle, create_attempt, new_event};
        use oms_command_peer::admission::{
            InteractiveContext, InteractiveRequest, begin_interactive_with_executor,
        };
        use oms_command_peer::process::{
            Executor, Invocation, Native, PhysicalAttempt, PhysicalContract, RootLocator,
            TeamLocator,
        };
        use oms_command_plan::{Context, Plan, Request, json};
        use oms_command_telemetry::store::{physical, resources, usage};
        if std::env::var_os("OMS_TEAM_REQUEST_REGISTER").is_some() {
            let repo = PathBuf::from(std::env::var_os("OMS_NATIVE_PHYSICAL_FIXTURE").unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap();
            let control = services(&stop, source, Vec::new());
            let previous = std::env::var("OMS_TEAM_REQUEST_PREVIOUS").unwrap();
            let request = Request::parse(&osv(&[
                "team-register",
                "--team-spec",
                "team.json",
                "--expected-team-sha256",
                &previous,
            ]))
            .unwrap();
            let context = Context::from_environment(&request, &repo).unwrap();
            Plan::open(context, &control, &stop)
                .unwrap()
                .execute(&request)
                .unwrap();
            return;
        }
        if let Some(mode) = std::env::var_os("OMS_TEAM_REQUEST_RUN") {
            let repo = PathBuf::from(std::env::var_os("OMS_NATIVE_PHYSICAL_FIXTURE").unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap();
            let collab = crate::services_collab::CollabServices::new(&stop, source).unwrap();
            let digest = std::env::var("OMS_TEAM_REQUEST_SPEC").unwrap();
            let mut args = osv(&[
                "--providers",
                "fixture",
                "--prompt",
                mode.to_str().unwrap(),
                "--model",
                "fixture",
                "--reasoning-effort",
                "high",
                "--task-id",
                "task-a",
                "--team-id",
                "team-a",
                "--team-spec-sha256",
                &digest,
                "--team-member",
                "lead-a",
                "--team-member-generation",
                "1",
                "--team-requests",
                "--print-timeout",
                if mode == "protocol-timeout" {
                    "8s"
                } else {
                    "15s"
                },
            ]);
            if mode.to_str().unwrap().contains("proposal") && mode != "protocol-proposal-disabled" {
                args.extend(osv(&[
                    "--team-request-actions",
                    "report_blocker,propose_tasks",
                ]));
            }
            let handler = if mode == "protocol-cancel" {
                Some(
                    signal_hook::flag::register(signal_hook::consts::SIGUSR1, Arc::clone(&stop))
                        .unwrap(),
                )
            } else {
                None
            };
            // Exercise the real issuance boundary while the adapter waits for
            // its hello. This fixture-owned lock grants no session authority.
            let issuance_store = (mode == "protocol-issuance-blocked")
                .then(|| oms_panel_core::terminals::controller_store(&repo, true).unwrap());
            let issuance_lock = issuance_store.as_ref().map(|store| {
                store
                    .lock_at(
                        Path::new("team-request-issuance.lock"),
                        Duration::from_secs(1),
                    )
                    .unwrap()
            });
            let (output, spawns) = if mode == "protocol-prespawn-requalify" {
                let policy_path = repo.join("team-policy.json");
                let saved_policy = std::fs::read(&policy_path).unwrap();
                let staged_bad = repo.join("team-policy.json.invalid");
                std::fs::write(&staged_bad, b"{}\n").unwrap();
                let enrolled_participant = std::sync::Mutex::new(None::<String>);
                let collab = collab.with_after_team_room_join(|participant| {
                    let raw = std::fs::read(repo.join(".oms/threads/room-team.jsonl")).unwrap();
                    let rows: Vec<_> = raw
                        .split(|b| *b == b'\n')
                        .filter(|line| !line.is_empty())
                        .map(|line| {
                            oms_command_room::value::parse(line, oms_command_room::MAX_ROW).unwrap()
                        })
                        .collect();
                    let state = oms_command_room::project(&rows).unwrap();
                    let current = state
                        .list("participants")
                        .iter()
                        .find(|p| p.text("participant") == participant)
                        .expect("Room join must exist before second binding check");
                    assert!(
                        current.flag("joined"),
                        "Room enrollment must be active before policy swap"
                    );
                    *enrolled_participant.lock().unwrap() = Some(participant.to_owned());
                    std::fs::rename(&staged_bad, &policy_path).unwrap();
                });
                let out = oms_command_peer::run_with_executor(
                    "peer-ask", &args, &repo, source, &stop, &collab,
                );
                let _ = std::fs::remove_file(&staged_bad);
                std::fs::write(&policy_path, saved_policy).unwrap();
                let enrolled_name = enrolled_participant
                    .lock()
                    .unwrap()
                    .take()
                    .expect("pre-spawn requalification hook did not run");
                let spawns = collab.team_child_spawns();
                assert_eq!(
                    spawns, 0,
                    "pre-spawn requalification refusal must never spawn the provider"
                );
                assert!(
                    !repo.join("prespawn-provider-spawned").exists(),
                    "provider adapter ran despite pre-spawn requalification failure"
                );
                let raw = std::fs::read(repo.join(".oms/threads/room-team.jsonl")).unwrap();
                let rows: Vec<_> = raw
                    .split(|b| *b == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(|line| {
                        oms_command_room::value::parse(line, oms_command_room::MAX_ROW).unwrap()
                    })
                    .collect();
                let after_state = oms_command_room::project(&rows).unwrap();
                assert!(
                    after_state
                        .list("participants")
                        .iter()
                        .find(|p| p.text("participant") == enrolled_name)
                        .is_some_and(|p| !p.flag("joined")),
                    "pre-spawn requalification failure must leave Room enrollment for {enrolled_name}"
                );
                (out, spawns)
            } else {
                let out = oms_command_peer::run_with_executor(
                    "peer-ask", &args, &repo, source, &stop, &collab,
                );
                (out, collab.team_child_spawns())
            };
            drop(issuance_lock);
            if let Some(handler) = handler {
                signal_hook::low_level::unregister(handler);
            }
            if mode == "protocol-timeout" || mode == "protocol-cancel" {
                let calls = usage::load(&repo).unwrap();
                let attempt = calls
                    .calls
                    .last()
                    .unwrap()
                    .receipt
                    .identity
                    .attempt_id
                    .as_ref()
                    .unwrap();
                let (_, projected) = oms_command_operations::load_projection(&repo).unwrap();
                assert_eq!(
                    projected
                        .iter()
                        .find(|a| a.get("attempt_id").text() == Some(attempt))
                        .unwrap()
                        .get("state")
                        .text(),
                    Some(if mode == "protocol-timeout" {
                        "timed_out"
                    } else {
                        "cancelled"
                    })
                );
            }
            let success = mode == "protocol-positive"
                || mode == "protocol-fragment"
                || mode == "protocol-proposal-positive";
            let stdout = String::from_utf8_lossy(&output.1);
            let stderr = String::from_utf8_lossy(&output.2);
            let diagnostic = || {
                stdout
                    .lines()
                    .next()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .and_then(|artifact| {
                        artifact
                            .split_once("\n## Diagnostics\n\n")
                            .map(|(_, body)| body.to_owned())
                    })
                    .unwrap_or_else(|| stderr.clone().into_owned())
            };
            if success {
                assert_eq!(output.0, 0, "{mode:?}: {}", diagnostic());
            } else {
                assert_ne!(output.0, 0, "{mode:?}: invalid protocol succeeded");
            }
            if mode != "protocol-prespawn-requalify" {
                assert_eq!(spawns, 1, "{mode:?} expected 1 spawn: {}", diagnostic());
            }
            assert!(!stdout.contains("nonce"));
            assert!(!stderr.contains("nonce"));
            let (required, forbidden): (&[&str], &[&str]) = match mode.to_str().unwrap() {
                "protocol-lost" | "protocol-proposal-lost" => {
                    (&["channel-unconsumed-response"], &["protocol-failed"])
                }
                "protocol-conflict" | "protocol-proposal-conflict" => (
                    &["protocol-failed"],
                    &["channel-no-request", "channel-unconsumed-response"],
                ),
                "protocol-early" | "protocol-unsupported" => {
                    (&["protocol-failed", "channel-no-request"], &[])
                }
                "protocol-partial" => (&["channel-partial-frame"], &[]),
                "protocol-issuance-blocked" => (
                    &[
                        "Team channel issuance failed; pending intent retained (stage=issuance-lock)",
                        "room-enrollment-retained; enrollment-stage=missing-cleanup-proof",
                    ],
                    &[],
                ),
                "protocol-prespawn-requalify" => (&["policy"], &[]),
                _ => (&[], &[]),
            };
            if !required.is_empty() {
                let seen = diagnostic();
                assert!(
                    required.iter().all(|cue| seen.contains(cue))
                        && forbidden.iter().all(|cue| !seen.contains(cue)),
                    "{mode:?}: unexpected channel phase: {seen}"
                );
            }
            return;
        }
        if std::env::var_os("OMS_NATIVE_PHYSICAL_FIXTURE").is_none() {
            let policy = resources::Policy {
                domain: "native-fixture".into(),
                epoch: "epoch-a".into(),
                host_generation_sha256: crate::services_runtime::observed_host_generation_before(
                    Instant::now() + Duration::from_secs(5),
                    &AtomicBool::new(false),
                )
                .unwrap(),
                max_teams: 8,
                max_invocations: 8,
                max_open_processes: 8,
                max_write_wip: 8,
                reviewer_headroom: 1,
            };
            let policy = String::from_utf8(policy.to_json().unwrap()).unwrap();
            let fixture = Tmp::repo(&[
                ("team-policy.json", policy.as_str()),
                ("src/lib.rs", "pub fn fixture() {}\n"),
            ]);
            std::fs::create_dir(fixture.path().join("caller")).unwrap();
            let private_state = Tmp::new();
            std::fs::set_permissions(private_state.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact","services_control::tests::team_contract_enrollment_uses_native_lifecycle_and_room_proofs","--nocapture"])
                .current_dir(fixture.path().join("caller"))
                .env("OMS_NATIVE_PHYSICAL_FIXTURE",fixture.path()).env("OMS_STATE_REPO","..")
                .env("XDG_STATE_HOME",private_state.path()).env("HOME",private_state.path());
            for key in [
                "OMS_HARNESS_CHILD",
                "OMS_HARNESS_DELEGATE_DEPTH",
                "OMS_ROOM_ID",
                "OMS_ROOM_PARTICIPANT",
                "OMS_PANEL_MAIN_ATTEMPT",
                "OMS_ATTEMPT_ID",
                "OMS_HARNESS_CALL_ID",
                "OMS_CAMPAIGN_MANIFEST_SHA256",
                "OMS_OPERATION_ID",
                "OMS_AUTOPILOT_OWNER_ID",
            ] {
                child.env_remove(key);
            }
            assert!(
                child.status().unwrap().success(),
                "native physical fixture child failed"
            );
            return;
        }
        let repo = PathBuf::from(std::env::var_os("OMS_NATIVE_PHYSICAL_FIXTURE").unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, repo.as_path(), Vec::new());
        let collab = crate::services_collab::CollabServices::new(&stop, repo.as_path()).unwrap();
        if let Some(selected) = std::env::var_os("OMS_NATIVE_PHYSICAL_GUARD_OWNER") {
            let selected = selected.to_str().unwrap();
            let attempt = PhysicalAttempt {
                state_root: repo.as_path(),
                contract_repo: repo.as_path(),
                execution_repo: repo.as_path(),
                team: None,
                root: Some(RootLocator {
                    attempt: selected,
                    participant: selected,
                    room_id: "room-team",
                }),
                provider: "codex",
                account: Some("work"),
                selected_model: Some("fixture"),
                selected_effort: None,
                task_id: None,
                write: true,
                persistent_session: false,
            };
            let before = physical::load(repo.as_path(), &stop).unwrap().len();
            let error = collab
                .with_physical_contract(
                    &attempt,
                    Instant::now() + Duration::from_secs(5),
                    &stop,
                    Box::new(|_| panic!("child/depth must refuse before registration")),
                )
                .unwrap_err();
            assert!(error.message.contains("parent-only"), "{error:?}");
            assert_eq!(physical::load(repo.as_path(), &stop).unwrap().len(), before);
            return;
        }
        let room = oms_command_room::Room::open(
            repo.as_path(),
            oms_command_room::Session {
                agent: "codex".into(),
                ..Default::default()
            },
            &collab,
            &stop,
            true,
            false,
        )
        .unwrap();
        room.create(
            Some("room-team"),
            "Native team fixture",
            Some("task-a"),
            None,
            None,
        )
        .unwrap();
        let owner = create_attempt(
            repo.as_path(),
            &StartRequest {
                provider: "codex".into(),
                tool: "panel-main".into(),
                refs: obj([
                    ("panel_room_id", "room-team".into()),
                    ("panel_role", "main".into()),
                ]),
                ..Default::default()
            },
            &stop,
        )
        .unwrap();
        for state in ["starting", "working"] {
            let event = new_event(
                &owner,
                0usize.into(),
                "attempt.state_changed",
                obj([("to_state", state.into())]),
            )
            .unwrap();
            append_lifecycle(repo.as_path(), event, &stop).unwrap();
        }
        room.join("room-team", oms_command_room::Join::new(&owner, "codex"))
            .unwrap();
        let policy_bytes = std::fs::read(repo.join("team-policy.json")).unwrap();
        let resource_policy = resources::Policy::parse(&policy_bytes).unwrap();
        resources::activate(
            repo.as_path(),
            &resource_policy,
            None,
            &stop,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        let root = RootLocator {
            attempt: &owner,
            participant: &owner,
            room_id: "room-team",
        };
        let run_local = |call: oms_command_peer::admission::InteractiveCall| {
            let count = physical::load(repo.as_path(), &stop).unwrap().len();
            call.fence_with_executor(&collab, &stop).unwrap();
            assert_eq!(
                physical::load(repo.as_path(), &stop).unwrap().len(),
                count,
                "qualification cannot register or resend"
            );
            oms_command_operations::lock::FileLock::acquire_before(
                &oms_command_operations::event_path(repo.as_path()),
                &stop,
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap()
            .check()
            .unwrap();
            for relative in [
                ".oms/landings.jsonl",
                ".oms/threads/room-team.jsonl",
                ".oms/delegations/.marker-set-lock-target",
                ".oms/plan/tasks.json",
            ] {
                let lock = oms_lock::Lock::for_file(
                    &repo.join(relative),
                    oms_lock::Settings::from_env().unwrap(),
                )
                .unwrap();
                lock.try_acquire().unwrap().check().unwrap();
            }
            let tag = oms_supervisor::process::PhysicalCall::new(call.call_id()).unwrap();
            let done = Native
                .execute(
                    Invocation {
                        argv: sh("printf native-physical"),
                        cwd: repo.clone(),
                        input: None,
                        environment: vec![],
                        remove_environment: vec![],
                        timeout: Duration::from_secs(5),
                        max_bytes: 1024,
                        merge: false,
                        physical_call: Some(tag.clone()),
                        launch_fence: Some(call.launch_fence()),
                    },
                    &stop,
                )
                .unwrap();
            assert_eq!(done.code, 0);
            assert_eq!(done.stdout, b"native-physical");
            assert_eq!(done.cleanup_complete, Some(true));
            assert!(
                done.owned_cleanup
                    .as_ref()
                    .is_some_and(|proof| proof.call() == &tag)
            );
            call.finish_with_cleanup(None, done.cleanup_complete, done.owned_cleanup.as_ref())
                .unwrap();
            let occupancy = resources::snapshot(repo.as_path(), &stop)
                .unwrap()
                .unwrap()
                .occupancy;
            assert_eq!(occupancy.invocations, 0);
            assert_eq!(occupancy.open_processes, 0);
        };
        assert!(!repo.join(".oms/plan/tasks.json").exists());
        let main_call = begin_interactive_with_executor(
            InteractiveRequest {
                state_root: repo.as_path(),
                provider: "codex",
                selected_model: Some("fixture"),
                account: Some("work"),
                attempt: Some(&owner),
                resume_session: None,
            },
            InteractiveContext {
                contract_repo: repo.as_path(),
                execution_repo: repo.as_path(),
                root: Some(root),
                team: None,
                task_id: None,
            },
            &collab,
            &stop,
        )
        .unwrap();
        assert_eq!(physical::load(repo.as_path(), &stop).unwrap().len(), 1);
        run_local(main_call);
        assert!(
            !repo.join(".oms/plan/tasks.json").exists(),
            "Root admission must not require or initialize a Plan"
        );
        for (child_flag, depth) in [("1", "0"), ("0", "1")] {
            let status = Command::new(std::env::current_exe().unwrap()).args(["--exact","services_control::tests::team_contract_enrollment_uses_native_lifecycle_and_room_proofs","--nocapture"])
                .current_dir(repo.join("caller")).env("OMS_NATIVE_PHYSICAL_GUARD_OWNER",&owner)
                .env("OMS_HARNESS_CHILD",child_flag).env("OMS_HARNESS_DELEGATE_DEPTH",depth).status().unwrap();
            assert!(
                status.success(),
                "Root locator cannot override child/depth guards"
            );
        }
        let call_as = |values: &[&str], attempt: &str, participant: &str, room_id: &str| {
            let request = Request::parse(&osv(values)).unwrap();
            let mut context = Context::from_environment(&request, repo.as_path()).unwrap();
            context.execution_repo = repo.as_path().into();
            context.state_repo = repo.as_path().into();
            context.plan_file = repo.as_path().join(".oms/plan/tasks.json");
            context.panel_main_attempt = attempt.into();
            context.room_id = room_id.into();
            context.participant = participant.into();
            Plan::open(context, &control, &stop)
                .unwrap()
                .execute(&request)
        };
        let call = |values: &[&str]| call_as(values, &owner, &owner, "room-team");
        call(&["init", "--goal", "bounded fixture"]).unwrap();
        call(&[
            "add",
            "--id",
            "task-a",
            "--title",
            "native team fixture",
            "--allowed",
            "src",
            "--verify",
            "true",
        ])
        .unwrap();
        let plan =
            json::parse(&std::fs::read(repo.as_path().join(".oms/plan/tasks.json")).unwrap())
                .unwrap();
        let task = plan.get("tasks").get("task-a");
        let mut spec: Value = serde_json::from_slice(include_bytes!(
            "../../oms-command-plan/tests/fixtures/team-spec.json"
        ))
        .unwrap();
        spec["plan_id"] = plan.text("plan_id").into();
        spec["task_sha256"] = json::hash(task).unwrap().into();
        spec["objective_sha256"] = json::sha(b"bounded fixture").into();
        spec["criterion_sha256"] = oms_command_plan::team::criterion_digest(&plan, task)
            .unwrap()
            .into();
        spec["base_sha"] = git(repo.as_path(), &["rev-parse", "HEAD"]).into();
        let policy = json::sha(&policy_bytes);
        spec["policy_sha256"] = policy.clone().into();
        for member in spec["members"].as_array_mut().unwrap() {
            member["route"]["policy_sha256"] = policy.clone().into();
            if member["role"] == "worker" {
                member["access"] = "write".into();
                // The interactive launcher supplies no per-call effort override.
                member["route"]["requested_effort"] = Value::Null;
            }
        }
        let bind_team_owner = |spec: &mut Value, attempt: &str, room_id: &str| {
            let (events, _) = oms_command_operations::load_projection(repo.as_path()).unwrap();
            let created = events
                .iter()
                .find(|event| {
                    event.get("attempt_id").text() == Some(attempt)
                        && event.get("event_type").text() == Some("attempt.created")
                })
                .unwrap();
            spec["room_id"] = room_id.into();
            spec["root_owner"] = attempt.into();
            spec["root_owner_generation"] = created.get("event_id").text().unwrap().into();
            for member in spec["members"].as_array_mut().unwrap() {
                member["root_owner"] = attempt.into();
                if member["role"] == "lead" {
                    member["logical_parent"] = attempt.into();
                }
            }
        };
        bind_team_owner(&mut spec, &owner, "room-team");
        for has_lineage in [true, false] {
            let id = room
                .create(None, "Native swap fixture", None, None, None)
                .unwrap();
            let start = |provider: &str, member: Option<&str>| {
                let mut refs = obj([
                    ("panel_room_id", id.as_str().into()),
                    ("panel_role", "main".into()),
                    ("panel_owner", provider.into()),
                ]);
                if let Some(member) = member {
                    refs.set("panel_room_participant", member);
                }
                create_attempt(
                    repo.as_path(),
                    &StartRequest {
                        provider: provider.into(),
                        tool: "panel-main".into(),
                        refs,
                        ..Default::default()
                    },
                    &stop,
                )
                .unwrap()
            };
            let advance = |attempt: &str, states: &[&str]| {
                for state in states {
                    let mut change = obj([("to_state", (*state).into())]);
                    if *state == "abandoned" {
                        change.set("reason_code", "main_swapped");
                    }
                    let event =
                        new_event(attempt, 0usize.into(), "attempt.state_changed", change).unwrap();
                    append_lifecycle(repo.as_path(), event, &stop).unwrap();
                }
            };
            let member = start("codex", None);
            room.join(&id, oms_command_room::Join::new(&member, "codex"))
                .unwrap();
            advance(&member, &["starting", "working", "blocked", "abandoned"]);
            let (provider, harness) = if has_lineage {
                let request = oms_panel_core::release::MainSwapRequest {
                    attempt_id: member.clone(),
                    provider: "antigravity".into(),
                    model: "fixture-model".into(),
                    harness: "agy".into(),
                };
                let event = oms_panel_core::Services::commit_main_swap(
                    &collab,
                    repo.as_path(),
                    &id,
                    &member,
                    &request,
                    Instant::now() + Duration::from_secs(5),
                    &stop,
                )
                .unwrap();
                assert_eq!(event["room"], id);
                ("antigravity", "agy")
            } else {
                ("codex", "codex")
            };
            let replacement = start(provider, Some(&member));
            advance(&replacement, &["starting", "working"]);
            for (participant, accepted) in [(&member, has_lineage), (&replacement, false)] {
                let attempt = PhysicalAttempt {
                    state_root: repo.as_path(),
                    contract_repo: repo.as_path(),
                    execution_repo: repo.as_path(),
                    root: Some(RootLocator {
                        attempt: &replacement,
                        participant,
                        room_id: &id,
                    }),
                    team: None,
                    provider,
                    account: None,
                    selected_model: None,
                    selected_effort: None,
                    task_id: None,
                    write: true,
                    persistent_session: false,
                };
                let error = collab
                    .with_physical_contract(
                        &attempt,
                        Instant::now() + Duration::from_secs(5),
                        &stop,
                        Box::new(|contract| {
                            assert!(accepted);
                            assert_eq!(contract.resource.unwrap().root_owner, replacement);
                            Err(oms_command_peer::Error::new(
                                75,
                                "fixture reached physical root registration",
                            ))
                        }),
                    )
                    .unwrap_err();
                let expected = if accepted {
                    "fixture reached physical root registration"
                } else if participant == &member {
                    "canonical Room lineage"
                } else {
                    "participant differs"
                };
                assert!(error.message.contains(expected), "{error:?}");
            }
            if has_lineage {
                let mut swapped_spec = spec.clone();
                bind_team_owner(&mut swapped_spec, &replacement, &id);
                std::fs::write(
                    repo.join("team-swapped.json"),
                    serde_json::to_vec(&swapped_spec).unwrap(),
                )
                .unwrap();
                let error = call_as(
                    &["team-register", "--team-spec", "team-swapped.json"],
                    &replacement,
                    &member,
                    &id,
                )
                .unwrap_err();
                assert_eq!(
                    error.message,
                    "team root owner is not enrolled in the current room"
                );

                let next = room.rollover(&id, Some(&member)).unwrap();
                let before = room.records(&next).unwrap();
                let error = room
                    .swap_main(&id, &member, provider, "fixture-model", &member, harness)
                    .unwrap_err();
                assert!(error.message.contains("room continues as"), "{error:?}");
                assert_eq!(room.records(&next).unwrap(), before);
            }
            advance(&replacement, &["cancelled"]);
        }
        std::fs::write(
            repo.as_path().join("team.json"),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();
        let before = std::fs::read(oms_command_operations::event_path(repo.as_path())).unwrap();
        call(&["team-register", "--team-spec", "team.json"]).unwrap();
        let source =
            serde_json::json!({"attempt_id": owner, "refs": {"panel_room_id": "room-team"}});
        let refusal = oms_panel_core::Services::check_main_swap_source(
            &collab,
            repo.as_path(),
            &source,
            &stop,
        )
        .unwrap_err();
        assert!(
            refusal.message.contains("active Team contracts"),
            "{refusal:?}"
        );
        let mut unrelated = source;
        unrelated["attempt_id"] = "unrelated-attempt".into();
        oms_panel_core::Services::check_main_swap_source(
            &collab,
            repo.as_path(),
            &unrelated,
            &stop,
        )
        .unwrap();
        let view: Value =
            serde_json::from_slice(&call(&["team-show", "--id", "team-a"]).unwrap().stdout)
                .unwrap();
        assert_eq!(view["phase"], "planned");
        assert_eq!(view["spec"]["root_owner"], owner);
        assert_eq!(view["verified"], Value::Null);
        let request = Request::new(oms_command_plan::Action::TeamShow);
        let mut context = Context::from_environment(&request, repo.as_path()).unwrap();
        context.execution_repo = repo.as_path().into();
        context.state_repo = repo.as_path().into();
        context.plan_file = repo.as_path().join(".oms/plan/tasks.json");
        context.panel_main_attempt = owner.clone();
        context.room_id = "room-team".into();
        context.participant = owner.clone();
        let admission_plan = Plan::open(context, &control, &stop).unwrap();
        let request = oms_command_plan::team::AdmissionRequest {
            team_id: "team-a".into(),
            spec_sha256: view["history"][0]["sha256"].as_str().unwrap().into(),
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
        let guard = admission_plan
            .team_admission_before(request, Instant::now() + Duration::from_secs(5))
            .unwrap();
        guard.check().unwrap();
        assert_eq!(guard.binding().text("root_owner"), owner);
        assert!(
            oms_command_operations::lock::FileLock::acquire_before(
                &oms_command_operations::event_path(repo.as_path()),
                &stop,
                Instant::now() + Duration::from_millis(20)
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(oms_command_operations::event_path(repo.as_path())).unwrap(),
            before
        );
        assert_eq!(
            usage::load(repo.as_path()).unwrap().calls.len(),
            1,
            "contract-only checks create no physical receipt"
        );
        drop(guard);
        oms_command_operations::lock::FileLock::acquire_before(
            &oms_command_operations::event_path(repo.as_path()),
            &stop,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap()
        .check()
        .unwrap();
        assert_eq!(
            std::fs::read(oms_command_operations::event_path(repo.as_path())).unwrap(),
            before
        );
        let team = TeamLocator {
            team_id: "team-a".into(),
            spec_sha256: view["history"][0]["sha256"].as_str().unwrap().into(),
            member_id: "worker-a".into(),
            member_generation: 1,
        };
        let member_call = begin_interactive_with_executor(
            InteractiveRequest {
                state_root: repo.as_path(),
                provider: "codex",
                selected_model: Some("fixture"),
                account: Some("other-account"),
                attempt: None,
                resume_session: None,
            },
            InteractiveContext {
                contract_repo: repo.as_path(),
                execution_repo: repo.as_path(),
                root: Some(root),
                team: Some(&team),
                task_id: Some("task-a"),
            },
            &collab,
            &stop,
        )
        .unwrap();
        assert_eq!(physical::load(repo.as_path(), &stop).unwrap().len(), 2);
        let claim = resources::snapshot(repo.as_path(), &stop).unwrap().unwrap();
        assert_eq!(claim.occupancy.teams, 1);
        assert_eq!(claim.occupancy.invocations, 1);
        assert_eq!(claim.occupancy.open_processes, 1);
        run_local(member_call);
        let receipts = usage::load(repo.as_path()).unwrap();
        assert_eq!(receipts.calls.len(), 2);
        assert!(
            receipts
                .calls
                .iter()
                .all(|call| call.receipt.cleanup_complete == Some(true)
                    && call.receipt.cost == usage::Cost::Unknown)
        );
        let lead = TeamLocator {
            member_id: "lead-a".into(),
            ..team.clone()
        };
        for selected_effort in [None, Some("medium")] {
            let attempt = PhysicalAttempt {
                state_root: repo.as_path(),
                contract_repo: repo.as_path(),
                execution_repo: repo.as_path(),
                team: Some(&lead),
                root: Some(root),
                provider: "codex",
                account: Some("work"),
                selected_model: Some("fixture"),
                selected_effort,
                task_id: Some("task-a"),
                write: false,
                persistent_session: false,
            };
            let before_calls = physical::load(repo.as_path(), &stop).unwrap().len();
            let error = collab
                .with_physical_contract(
                    &attempt,
                    Instant::now() + Duration::from_secs(5),
                    &stop,
                    Box::new(|_| panic!("effort mismatch reached registration")),
                )
                .unwrap_err();
            assert!(error.message.contains("physical route"), "{error:?}");
            assert_eq!(
                physical::load(repo.as_path(), &stop).unwrap().len(),
                before_calls
            );
        }
        let reject = |member: &TeamLocator,
                      selected: RootLocator<'_>,
                      account: &str,
                      model: &str,
                      task: &str,
                      execution: &Path,
                      cancelled: bool,
                      elapsed: bool| {
            let attempt = PhysicalAttempt {
                state_root: repo.as_path(),
                contract_repo: repo.as_path(),
                execution_repo: execution,
                team: Some(member),
                root: Some(selected),
                provider: "codex",
                account: Some(account),
                selected_model: Some(model),
                selected_effort: None,
                task_id: Some(task),
                write: true,
                persistent_session: false,
            };
            let count = physical::load(repo.as_path(), &stop).unwrap().len();
            let interruption = AtomicBool::new(cancelled);
            let deadline = if elapsed {
                Instant::now()
            } else {
                Instant::now() + Duration::from_secs(5)
            };
            let error = collab
                .with_physical_contract(
                    &attempt,
                    deadline,
                    &interruption,
                    Box::new(|_| panic!("invalid contract reached registration")),
                )
                .unwrap_err();
            assert_eq!(physical::load(repo.as_path(), &stop).unwrap().len(), count);
            error
        };
        let mut invalid_member = team.clone();
        invalid_member.member_generation = 2;
        reject(
            &invalid_member,
            root,
            "other-account",
            "fixture",
            "task-a",
            repo.as_path(),
            false,
            false,
        );
        reject(
            &team,
            RootLocator {
                attempt: "foreign-main",
                participant: "foreign-main",
                room_id: "room-team",
            },
            "other-account",
            "fixture",
            "task-a",
            repo.as_path(),
            false,
            false,
        );
        reject(
            &team,
            root,
            "wrong-account",
            "fixture",
            "task-a",
            repo.as_path(),
            false,
            false,
        );
        reject(
            &team,
            root,
            "other-account",
            "wrong-model",
            "task-a",
            repo.as_path(),
            false,
            false,
        );
        reject(
            &team,
            root,
            "other-account",
            "fixture",
            "wrong-task",
            repo.as_path(),
            false,
            false,
        );
        let wrong_base = Tmp::repo(&[("different", "different base\n")]);
        reject(
            &team,
            root,
            "other-account",
            "fixture",
            "task-a",
            wrong_base.path(),
            false,
            false,
        );
        assert_eq!(
            reject(
                &team,
                root,
                "other-account",
                "fixture",
                "task-a",
                repo.as_path(),
                true,
                false
            )
            .code,
            130
        );
        assert_eq!(
            reject(
                &team,
                root,
                "other-account",
                "fixture",
                "task-a",
                repo.as_path(),
                false,
                true
            )
            .code,
            75
        );
        let retain_pending = |id: &str, contract: PhysicalContract<'_>| {
            let registered = physical::begin_bound_checked(
                repo.as_path(),
                physical::Request {
                    identity: usage::Identity {
                        call_id: id.into(),
                        operation_id: format!("operation-{id}"),
                        provider: "codex".into(),
                        selected_model: Some("fixture".into()),
                        account: Some("other-account".into()),
                        task_id: Some("task-a".into()),
                        ..Default::default()
                    },
                    cumulative: None,
                    write: true,
                    campaign_manifest_sha256: None,
                },
                contract.resource.expect("native contract resource binding"),
                &stop,
                Instant::now() + Duration::from_secs(5),
                |_, _| Ok(()),
            )
            .unwrap();
            assert!(matches!(registered, physical::Registration::New(_)));
            oms_supervisor::process::PhysicalCall::new(id).unwrap()
        };
        let attempt = PhysicalAttempt {
            state_root: repo.as_path(),
            contract_repo: repo.as_path(),
            execution_repo: repo.as_path(),
            team: Some(&team),
            root: Some(root),
            provider: "codex",
            account: Some("other-account"),
            selected_model: Some("fixture"),
            selected_effort: None,
            task_id: Some("task-a"),
            write: true,
            persistent_session: false,
        };
        let policy_error = collab
            .with_physical_contract(
                &attempt,
                Instant::now() + Duration::from_secs(10),
                &stop,
                Box::new(|contract| {
                    let call = retain_pending("policy-held", contract);
                    std::fs::write(repo.join("team-policy.json"), b"{}\n").unwrap();
                    Ok(call)
                }),
            )
            .unwrap_err();
        assert!(policy_error.message.contains("policy"), "{policy_error:?}");
        std::fs::write(repo.join("team-policy.json"), &policy_bytes).unwrap();
        {
            // Even when execution_repo initially resolves to contract_repo,
            // post-registration check_execution() must re-verify Git HEAD before
            // the resource fence and final guard.check().
            let linked = Tmp::new();
            let exec_link = linked.path().join("exec-repo");
            std::os::unix::fs::symlink(repo.as_path(), &exec_link).unwrap();
            let linked_attempt = PhysicalAttempt {
                execution_repo: &exec_link,
                ..attempt
            };
            let exec_error = collab
                .with_physical_contract(
                    &linked_attempt,
                    Instant::now() + Duration::from_secs(10),
                    &stop,
                    Box::new(|_| {
                        std::fs::remove_file(&exec_link).unwrap();
                        std::os::unix::fs::symlink(wrong_base.path(), &exec_link).unwrap();
                        Ok(
                            oms_supervisor::process::PhysicalCall::new("unregistered-exec-probe")
                                .unwrap(),
                        )
                    }),
                )
                .unwrap_err();
            assert!(
                exec_error
                    .message
                    .contains("physical execution base differs"),
                "{exec_error:?}"
            );
        }
        let assert_retained = |expected: usize, open: u64| {
            assert_eq!(
                physical::load(repo.as_path(), &stop).unwrap().len(),
                expected
            );
            let receipts = usage::load(repo.as_path()).unwrap();
            assert_eq!(receipts.calls.len(), expected);
            for id in ["policy-held", "expiry-held"]
                .into_iter()
                .take(expected - 2)
            {
                let receipt = &receipts
                    .calls
                    .iter()
                    .find(|call| call.receipt.identity.call_id == id)
                    .unwrap()
                    .receipt;
                assert_eq!(receipt.phase, usage::Phase::Pending);
                assert_eq!(receipt.cost, usage::Cost::Unknown);
                assert_eq!(receipt.cleanup_complete, None);
            }
            let occupancy = resources::snapshot(repo.as_path(), &stop)
                .unwrap()
                .unwrap()
                .occupancy;
            assert_eq!(occupancy.invocations, open);
            assert_eq!(occupancy.open_processes, open);
        };
        assert_retained(3, 1);
        // Use a real native contract expiry. Wait at most twelve seconds for a
        // minute boundary so the existing UTC producer supplies the calendar.
        let expiry = loop {
            let now = oms_command_autonomy::value::now();
            let second = now[17..19].parse::<u64>().unwrap();
            if second <= 47 {
                break format!("{}{:02}Z", &now[..17], second + 12);
            }
            std::thread::sleep(Duration::from_secs(60 - second));
        };
        spec["generation"] = 2.into();
        spec["previous_sha256"] = team.spec_sha256.clone().into();
        spec["expires_at"] = expiry.into();
        std::fs::write(repo.join("team.json"), serde_json::to_vec(&spec).unwrap()).unwrap();
        call(&[
            "team-register",
            "--team-spec",
            "team.json",
            "--expected-team-sha256",
            &team.spec_sha256,
        ])
        .unwrap();
        let next: Value =
            serde_json::from_slice(&call(&["team-show", "--id", "team-a"]).unwrap().stdout)
                .unwrap();
        let next_team = TeamLocator {
            spec_sha256: next["history"][1]["sha256"].as_str().unwrap().into(),
            ..team.clone()
        };
        let attempt = PhysicalAttempt {
            team: Some(&next_team),
            ..attempt
        };
        let expiry_error = collab
            .with_physical_contract(
                &attempt,
                Instant::now() + Duration::from_secs(20),
                &stop,
                Box::new(|contract| {
                    let expires_at = contract.expires_at.unwrap();
                    let call = retain_pending("expiry-held", contract);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap();
                    let expires_at = Duration::from_secs(expires_at.try_into().unwrap());
                    if let Some(remaining) = expires_at.checked_sub(now) {
                        std::thread::sleep(remaining);
                    }
                    Ok(call)
                }),
            )
            .unwrap_err();
        assert!(expiry_error.message.contains("expired"), "{expiry_error:?}");
        assert_retained(4, 2);
        spec["generation"] = 3.into();
        spec["previous_sha256"] = next_team.spec_sha256.clone().into();
        spec["expires_at"] = "2200-01-01T00:00:00Z".into();
        std::fs::write(repo.join("team.json"), serde_json::to_vec(&spec).unwrap()).unwrap();
        call(&[
            "team-register",
            "--team-spec",
            "team.json",
            "--expected-team-sha256",
            &next_team.spec_sha256,
        ])
        .unwrap();
        let active: Value =
            serde_json::from_slice(&call(&["team-show", "--id", "team-a"]).unwrap().stdout)
                .unwrap();
        let active_team = TeamLocator {
            spec_sha256: active["history"][2]["sha256"].as_str().unwrap().into(),
            ..team.clone()
        };
        {
            let lock = oms_lock::Lock::for_file(
                &repo.join(".oms/threads/room-team.jsonl"),
                oms_lock::Settings::from_env().unwrap(),
            )
            .unwrap();
            let held = lock.try_acquire().unwrap();
            let deadline = Instant::now() + Duration::from_millis(100);
            let bounded = oms_command_room::Room::open(
                repo.as_path(),
                oms_command_room::Session::default(),
                &collab,
                &stop,
                false,
                false,
            )
            .unwrap()
            .with_deadline(deadline);
            let mut join = oms_command_room::Join::new("bounded-probe", "codex");
            join.role = "advisor".into();
            join.parent = Some(owner.clone());
            let start = Instant::now();
            let error = bounded.join("room-team", join.clone()).unwrap_err();
            assert_eq!(
                crate::services_collab::prelaunch_error(
                    oms_command_peer::Error::new(75, error.message),
                    &stop,
                    deadline,
                    None,
                )
                .code,
                124,
                "actual late Room deadline must retain timeout classification"
            );
            assert!(start.elapsed() < Duration::from_secs(1));
            let cancelled = AtomicBool::new(false);
            let deadline = Instant::now() + Duration::from_secs(2);
            let bounded = oms_command_room::Room::open(
                repo.as_path(),
                oms_command_room::Session::default(),
                &collab,
                &cancelled,
                false,
                false,
            )
            .unwrap()
            .with_deadline(deadline);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    std::thread::sleep(Duration::from_millis(30));
                    cancelled.store(true, Ordering::Release);
                });
                let start = Instant::now();
                let error = bounded.join("room-team", join).unwrap_err();
                assert_eq!(
                    crate::services_collab::prelaunch_error(
                        oms_command_peer::Error::new(75, error.message),
                        &cancelled,
                        deadline,
                        None,
                    )
                    .code,
                    130,
                    "actual late Room cancellation must retain cancellation classification"
                );
                assert!(start.elapsed() < Duration::from_secs(1));
            });
            drop(held);
        }
        {
            let probe = oms_command_room::Room::open(
                repo.as_path(),
                oms_command_room::Session::default(),
                &collab,
                &stop,
                false,
                false,
            )
            .unwrap();
            let mut join = oms_command_room::Join::new("bounded-native-probe", "codex");
            join.role = "advisor".into();
            join.parent = Some(owner.clone());
            probe.join("room-team", join).unwrap();
            let generation = probe
                .status("room-team", false)
                .unwrap()
                .list("participants")
                .iter()
                .find(|p| p.text("participant") == "bounded-native-probe")
                .unwrap()
                .number("seq");
            let held = oms_command_operations::lock::FileLock::acquire_before(
                &oms_command_operations::event_path(repo.as_path()),
                &stop,
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
            let bounded = oms_command_room::Room::open(
                repo.as_path(),
                oms_command_room::Session::default(),
                &collab,
                &stop,
                false,
                false,
            )
            .unwrap()
            .with_deadline(Instant::now() + Duration::from_millis(100));
            let start = Instant::now();
            assert!(
                bounded
                    .leave_if_enrolled("room-team", "bounded-native-probe", generation, &owner)
                    .is_err()
            );
            assert!(start.elapsed() < Duration::from_secs(1));
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_services =
                crate::services_collab::CollabServices::new(&cancel, repo.as_path()).unwrap();
            let bounded = oms_command_room::Room::open(
                repo.as_path(),
                oms_command_room::Session::default(),
                &cancel_services,
                &cancel,
                false,
                false,
            )
            .unwrap()
            .with_deadline(Instant::now() + Duration::from_secs(2));
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    std::thread::sleep(Duration::from_millis(30));
                    cancel.store(true, Ordering::Release);
                });
                let start = Instant::now();
                assert!(
                    bounded
                        .leave_if_enrolled("room-team", "bounded-native-probe", generation, &owner)
                        .is_err()
                );
                assert!(start.elapsed() < Duration::from_secs(1));
            });
            drop(held);
            probe.leave("room-team", "bounded-native-probe").unwrap();
        }
        let adapters = repo.join("adapters");
        std::fs::create_dir(&adapters).unwrap();
        let adapter = adapters.join("oms-agent-adapter-fixture");
        let script = r#"#!/bin/sh
set -eu
prompt_file=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --prompt-file ]; then shift; prompt_file=$1; fi
  shift
done
prompt=$(cat "$prompt_file")
case "$prompt" in *protocol-prespawn-requalify*) : > "$OMS_NATIVE_PHYSICAL_FIXTURE/prespawn-provider-spawned"; exit 99;; esac
IFS= read -r hello
field() { printf '%s' "${2-$hello}" | sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p"; }
num() { printf '%s' "$hello" | sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p"; }
session=$(field session)
nonce=$(field nonce)
generation=$(num connection_generation)
binding=$(field binding_sha256)
hello_expiry=$(num expires_at)
expiry=${OMS_TEAM_REQUEST_EXPIRY:-$hello_expiry}
[ "$expiry" -le "$hello_expiry" ]
case "$prompt" in *protocol-wrong*) nonce=wrong;; esac
printf '{"schema":1,"kind":"hello","session":"%s","nonce":"%s","connection_generation":%s}\n' "$session" "$nonce" "$generation"
case "$prompt" in *protocol-cancel*) kill -USR1 "$PPID"; while :; do sleep 1; done;; *protocol-timeout*) while :; do sleep 1; done;; esac
case "$prompt" in *protocol-oversize*) printf '%8193s' x; while :; do sleep 1; done;; *protocol-partial*) printf '{'; exit 0;; esac
for sequence in 1 2; do
  action=report_blocker
  case "$prompt" in *protocol-unsupported*) action=request_dispatch;; esac
  text=bounded-blocker
  case "$prompt:$sequence" in *protocol-conflict*:2) text=changed;; esac
  request_id=report-one
  case "$prompt" in
    *protocol-early*) request_id=report-early;;
    *protocol-fragment*) request_id=report-fragment;;
  esac
  request=$(printf '{"schema":1,"kind":"request","session":"%s","nonce":"%s","connection_generation":%s,"sequence":%s,"binding_sha256":"%s","body":{"schema":1,"request_id":"%s","action":"%s","text":"%s","expires_at":%s}}' "$session" "$nonce" "$generation" "$sequence" "$binding" "$request_id" "$action" "$text" "$expiry")
  case "$prompt" in *protocol-proposal-*)
    request_id=proposal-one
    case "$prompt" in
      *protocol-proposal-disabled*) context=$(cat "$OMS_NATIVE_PHYSICAL_FIXTURE/.oms/plan/team-specs/$OMS_TEAM_SPEC_SHA256.json"); task_sha=$(field task_sha256 "$context"); [ "$(field task_id "$context")" = task-a ];;
      *) task_sha=$(field task_sha256); [ "$(field task_id)" = task-a ];;
    esac
    proposed_id=proposed-a
    proposed_title=Inspect-source
    allowed=src/lib.rs
    depends='[]'
    case "$prompt" in
      *protocol-proposal-stale*) task_sha=$(printf '%064d' 0); request_id=proposal-stale;;
      *protocol-proposal-wide*) allowed=outside/file.rs; request_id=proposal-wide;;
      *protocol-proposal-forbidden*) allowed=src/secret/private; request_id=proposal-forbidden;;
      *protocol-proposal-existing*) proposed_id=task-a; request_id=proposal-existing;;
      *protocol-proposal-cycle*) depends='["proposed-a"]'; request_id=proposal-cycle;;
      *protocol-proposal-conflict*) if [ "$sequence" = 2 ]; then proposed_title=Changed; fi;;
    esac
    request=$(printf '{"schema":1,"kind":"request","session":"%s","nonce":"%s","connection_generation":%s,"sequence":%s,"binding_sha256":"%s","body":{"schema":2,"request_id":"%s","action":"propose_tasks","summary":"Bounded-decomposition","expected_task_sha256":"%s","tasks":[{"id":"%s","title":"%s","allowed":["%s"],"depends":%s}],"expires_at":%s}}' "$session" "$nonce" "$generation" "$sequence" "$binding" "$request_id" "$task_sha" "$proposed_id" "$proposed_title" "$allowed" "$depends" "$expiry")
    ;;
  esac
  case "$prompt" in *protocol-early*) printf '{"schema":1,"kind":"ack","session":"%s","nonce":"%s","connection_generation":%s,"sequence":%s,"request_id":"%s","receipt_challenge":"predicted"}\n' "$session" "$nonce" "$generation" "$sequence" "$request_id";; esac
  case "$prompt" in *protocol-fragment*) printf '%s' "$request" | cut -c 1-20 | tr -d '\n'; sleep 0.02; printf '%s\n' "$request" | cut -c 21-;; *) printf '%s\n' "$request";; esac
  IFS= read -r response || exit 3
  case "$response" in *'"response"'*) :;; *) exit 4;; esac
  case "$prompt" in *protocol-lost*|*protocol-proposal-lost*) exit 0;; esac
  challenge=$(field receipt_challenge "$response")
  printf '{"schema":1,"kind":"ack","session":"%s","nonce":"%s","connection_generation":%s,"sequence":%s,"request_id":"%s","receipt_challenge":"%s"}\n' "$session" "$nonce" "$generation" "$sequence" "$request_id" "$challenge"
done
"#;
        std::fs::write(&adapter, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(adapter.with_file_name("oms-agent-adapter-fixture.oms-team-request-v1.json"),serde_json::to_vec(&serde_json::json!({"schema":1,"protocol":"oms-team-request-v1","binary_sha256":json::sha(script.as_bytes())})).unwrap()).unwrap();
        std::fs::set_permissions(
            adapters.join("oms-agent-adapter-fixture.oms-team-request-v1.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        spec["generation"] = 4.into();
        spec["forbidden_paths"] = serde_json::json!(["src/secret"]);
        spec["previous_sha256"] = active_team.spec_sha256.clone().into();
        for member in spec["members"].as_array_mut().unwrap() {
            if member["id"] == "lead-a" {
                member["route"]["provider"] = "fixture".into();
                member["route"]["harness"] = "fixture".into();
                member["route"]["account"] = Value::Null;
            }
        }
        std::fs::write(repo.join("team.json"), serde_json::to_vec(&spec).unwrap()).unwrap();
        // Custom provider registration is scoped to each real child invocation;
        // no process-global environment mutation grants the fixture authority.
        let register=Command::new(std::env::current_exe().unwrap()).args(["--exact","services_control::tests::team_contract_enrollment_uses_native_lifecycle_and_room_proofs","--nocapture"])
            .current_dir(repo.join("caller")).env("OMS_TEAM_REQUEST_REGISTER","1").env("OMS_TEAM_REQUEST_PREVIOUS",&active_team.spec_sha256).env("OMS_PANEL_MAIN_ATTEMPT",&owner).env("OMS_ROOM_ID","room-team").env("OMS_ROOM_PARTICIPANT",&owner).env("OMS_PROVIDER_ADAPTER_DIR",&adapters).status().unwrap();
        assert!(register.success());
        let updated =
            json::parse(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap()).unwrap();
        let head = updated
            .get("teams")
            .get("team-a")
            .get("versions")
            .array()
            .unwrap()
            .last()
            .unwrap()
            .text("sha256")
            .to_owned();
        let task_before = json::hash(updated.get("tasks")).unwrap();
        let physical_before = physical::load(repo.as_path(), &stop).unwrap().len();
        let mut report_expiry = None;
        let mut report_count = 1;
        for mode in [
            "protocol-positive",
            "protocol-lost",
            "protocol-conflict",
            "protocol-wrong",
            "protocol-early",
            "protocol-timeout",
            "protocol-cancel",
            "protocol-fragment",
            "protocol-oversize",
            "protocol-partial",
            "protocol-unsupported",
        ] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "services_control::tests::team_contract_enrollment_uses_native_lifecycle_and_room_proofs", "--nocapture"])
                .current_dir(repo.join("caller")).env("OMS_TEAM_REQUEST_RUN", mode).env_remove("OMS_TEAM_REQUEST_EXPIRY").env("OMS_TEAM_REQUEST_SPEC", &head).env("OMS_PANEL_MAIN_ATTEMPT", &owner).env("OMS_ROOM_ID", "room-team").env("OMS_ROOM_PARTICIPANT", &owner).env("OMS_PROVIDER_ADAPTER_DIR", &adapters);
            if matches!(mode, "protocol-lost" | "protocol-conflict") {
                child.env("OMS_TEAM_REQUEST_EXPIRY", report_expiry.as_ref().unwrap());
            }
            let status = child.status().unwrap();
            assert!(status.success(), "native Team protocol case {mode} failed");
            let current_room = oms_command_room::Room::open(
                repo.as_path(),
                oms_command_room::Session::default(),
                &collab,
                &stop,
                false,
                false,
            )
            .unwrap();
            let projection =
                oms_command_room::project(&current_room.records("room-team").unwrap()).unwrap();
            assert!(
                projection
                    .list("participants")
                    .iter()
                    .filter(|p| p.text("participant").starts_with("team-call_"))
                    .all(|p| !p.flag("joined")),
                "sealed cleanup must settle the exact enrollment"
            );
            let ledger =
                json::parse(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap()).unwrap();
            assert_eq!(json::hash(ledger.get("tasks")).unwrap(), task_before);
            let record = ledger.get("teams").get("team-a");
            assert!(record.get("request_schema").is_int(2));
            let requests = record.get("requests").array().unwrap();
            if mode == "protocol-fragment" {
                report_count += 1;
                let fragment = requests.last().unwrap();
                assert_eq!(
                    fragment.get("request").text("request_id"),
                    "report-fragment"
                );
                assert_eq!(fragment.get("outcome").text("status"), "recorded");
            }
            assert_eq!(
                requests.len(),
                report_count,
                "unexpected report effect for {mode}"
            );
            if mode == "protocol-positive" {
                let request = &requests[0];
                assert_eq!(request.get("request").text("request_id"), "report-one");
                assert_eq!(request.get("outcome").text("status"), "recorded");
                report_expiry = Some(
                    request
                        .get("request")
                        .get("expires_at")
                        .int()
                        .unwrap()
                        .to_string(),
                );
            }
        }
        assert_eq!(
            physical::load(repo.as_path(), &stop).unwrap().len(),
            physical_before + 11,
            "no hidden provider resend"
        );
        let native_occupancy = resources::snapshot(repo.as_path(), &stop)
            .unwrap()
            .unwrap()
            .occupancy;
        assert_eq!(
            (
                native_occupancy.invocations,
                native_occupancy.open_processes
            ),
            (2, 2),
            "only earlier unknown liabilities remain"
        );
        let mut proposal_expiry = None;
        for (index, mode) in [
            "protocol-proposal-disabled",
            "protocol-proposal-positive",
            "protocol-proposal-lost",
            "protocol-proposal-conflict",
            "protocol-proposal-stale",
            "protocol-proposal-wide",
            "protocol-proposal-forbidden",
            "protocol-proposal-cycle",
            "protocol-proposal-existing",
        ]
        .into_iter()
        .enumerate()
        {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "services_control::tests::team_contract_enrollment_uses_native_lifecycle_and_room_proofs", "--nocapture"])
                .current_dir(repo.join("caller")).env("OMS_TEAM_REQUEST_RUN", mode).env_remove("OMS_TEAM_REQUEST_EXPIRY").env("OMS_TEAM_REQUEST_SPEC", &head).env("OMS_PANEL_MAIN_ATTEMPT", &owner).env("OMS_ROOM_ID", "room-team").env("OMS_ROOM_PARTICIPANT", &owner).env("OMS_PROVIDER_ADAPTER_DIR", &adapters);
            if matches!(index, 2 | 3) {
                child.env("OMS_TEAM_REQUEST_EXPIRY", proposal_expiry.as_ref().unwrap());
            }
            let status = child.status().unwrap();
            assert!(status.success(), "native Team proposal case {mode} failed");
            let ledger =
                json::parse(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap()).unwrap();
            assert_eq!(
                json::hash(ledger.get("tasks")).unwrap(),
                task_before,
                "proposals must not mutate the canonical task graph"
            );
            let record = ledger.get("teams").get("team-a");
            assert_eq!(
                record.get("requests").array().unwrap().len(),
                report_count + usize::from(index > 0),
                "proposal replay/invalid input must not create another effect"
            );
            assert!(
                record
                    .get("request_schema")
                    .is_int(if index == 0 { 2 } else { 3 })
            );
            if index > 0 {
                assert_eq!(
                    record
                        .get("requests")
                        .array()
                        .unwrap()
                        .last()
                        .unwrap()
                        .get("outcome")
                        .text("status"),
                    "proposed"
                );
                if index == 1 {
                    proposal_expiry = Some(
                        record
                            .get("requests")
                            .array()
                            .unwrap()
                            .last()
                            .unwrap()
                            .get("request")
                            .get("expires_at")
                            .int()
                            .unwrap()
                            .to_string(),
                    );
                }
            }
            if matches!(index, 1 | 8) {
                let normal: Value =
                    serde_json::from_slice(&call(&["team-show", "--id", "team-a"]).unwrap().stdout)
                        .unwrap();
                assert!(
                    normal.get("requests").is_none(),
                    "legacy Team view bytes stay opt-in"
                );
                let before_read = std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap();
                let with_requests: Value = serde_json::from_slice(
                    &call(&["team-show", "--id", "team-a", "--requests"])
                        .unwrap()
                        .stdout,
                )
                .unwrap();
                assert_eq!(
                    with_requests["requests"].as_array().unwrap().len(),
                    report_count + 1
                );
                assert_eq!(
                    std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap(),
                    before_read,
                    "request projection must not write"
                );
            }
            let projection =
                oms_command_room::project(&room.records("room-team").unwrap()).unwrap();
            assert!(
                projection
                    .list("participants")
                    .iter()
                    .filter(|p| p.text("participant").starts_with("team-call_"))
                    .all(|p| !p.flag("joined"))
            );
        }
        assert_eq!(
            physical::load(repo.as_path(), &stop).unwrap().len(),
            physical_before + 20,
            "proposal replay must not resend providers"
        );
        let retained = updated.get("teams").get("team-a").opt("requests").cloned();
        assert!(
            retained.is_none(),
            "feature off contracts have no request ledger"
        );
        let before_generation =
            json::parse(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap())
                .unwrap()
                .get("teams")
                .get("team-a")
                .get("requests")
                .clone();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "services_control::tests::team_contract_enrollment_uses_native_lifecycle_and_room_proofs", "--nocapture"])
            .current_dir(repo.join("caller")).env("OMS_TEAM_REQUEST_RUN", "protocol-issuance-blocked")
            .env_remove("OMS_TEAM_REQUEST_EXPIRY").env("OMS_TEAM_REQUEST_SPEC", &head)
            .env("OMS_PANEL_MAIN_ATTEMPT", &owner).env("OMS_ROOM_ID", "room-team")
            .env("OMS_ROOM_PARTICIPANT", &owner).env("OMS_PROVIDER_ADAPTER_DIR", &adapters)
            .status().unwrap();
        assert!(status.success(), "blocked Team issuance diagnostic failed");
        let blocked_projection =
            oms_command_room::project(&room.records("room-team").unwrap()).unwrap();
        assert!(
            blocked_projection
                .list("participants")
                .iter()
                .rfind(|p| p.text("participant").starts_with("team-call_"))
                .is_some_and(|p| p.flag("joined")),
            "post-spawn issuance failure without OwnedCleanup must retain Room enrollment"
        );
        assert_eq!(
            physical::load(repo.as_path(), &stop).unwrap().len(),
            physical_before + 21,
            "failed issuance must not resend the provider"
        );
        assert_eq!(
            json::parse(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap())
                .unwrap()
                .get("teams")
                .get("team-a")
                .get("requests"),
            &before_generation,
            "failed issuance cannot publish a Team request"
        );
        spec["generation"] = 5.into();
        spec["previous_sha256"] = head.clone().into();
        std::fs::write(repo.join("team.json"), serde_json::to_vec(&spec).unwrap()).unwrap();
        let status=Command::new(std::env::current_exe().unwrap()).args(["--exact","services_control::tests::team_contract_enrollment_uses_native_lifecycle_and_room_proofs","--nocapture"])
            .current_dir(repo.join("caller")).env("OMS_TEAM_REQUEST_REGISTER","1").env("OMS_TEAM_REQUEST_PREVIOUS",&head).env("OMS_PANEL_MAIN_ATTEMPT",&owner).env("OMS_ROOM_ID","room-team").env("OMS_ROOM_PARTICIPANT",&owner).env("OMS_PROVIDER_ADAPTER_DIR",&adapters).status().unwrap();
        assert!(status.success());
        let gen5_plan =
            json::parse(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap()).unwrap();
        assert_eq!(
            gen5_plan.get("teams").get("team-a").get("requests"),
            &before_generation,
            "generation changes preserve dedup history"
        );
        let gen5_head = gen5_plan
            .get("teams")
            .get("team-a")
            .get("versions")
            .array()
            .unwrap()
            .last()
            .unwrap()
            .text("sha256")
            .to_owned();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "services_control::tests::team_contract_enrollment_uses_native_lifecycle_and_room_proofs", "--nocapture"])
            .current_dir(repo.join("caller")).env("OMS_TEAM_REQUEST_RUN", "protocol-prespawn-requalify")
            .env_remove("OMS_TEAM_REQUEST_EXPIRY").env("OMS_TEAM_REQUEST_SPEC", &gen5_head)
            .env("OMS_PANEL_MAIN_ATTEMPT", &owner).env("OMS_ROOM_ID", "room-team")
            .env("OMS_ROOM_PARTICIPANT", &owner).env("OMS_PROVIDER_ADAPTER_DIR", &adapters)
            .status().unwrap();
        assert!(
            status.success(),
            "pre-spawn Team requalification refusal failed"
        );
        let prespawn_projection =
            oms_command_room::project(&room.records("room-team").unwrap()).unwrap();
        assert!(
            !prespawn_projection
                .list("participants")
                .iter()
                .rfind(|p| p.text("participant").starts_with("team-call_"))
                .unwrap()
                .flag("joined"),
            "pre-spawn requalification failure must settle Room enrollment"
        );
        assert_eq!(
            physical::load(repo.as_path(), &stop).unwrap().len(),
            physical_before + 22,
            "failed pre-spawn requalification must not resend the provider"
        );
        assert_eq!(
            json::parse(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap())
                .unwrap()
                .get("teams")
                .get("team-a")
                .get("requests"),
            &before_generation,
            "failed pre-spawn requalification cannot publish a Team request"
        );
        let cancel = new_event(
            &owner,
            0usize.into(),
            "attempt.state_changed",
            obj([("to_state", "cancelled".into())]),
        )
        .unwrap();
        append_lifecycle(repo.as_path(), cancel, &stop).unwrap();
        assert_eq!(
            json::parse(&std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap())
                .unwrap()
                .get("teams")
                .get("team-a")
                .get("requests"),
            &before_generation,
            "owner cancellation preserves dedup history"
        );
        let held: Value =
            serde_json::from_slice(&call(&["team-show", "--id", "team-a"]).unwrap().stdout)
                .unwrap();
        assert_eq!(held["phase"], "held");
        assert!(held["reason"].as_str().unwrap().contains("owner"));
        reject(
            &active_team,
            root,
            "other-account",
            "fixture",
            "task-a",
            repo.as_path(),
            false,
            false,
        );
        let reference = view["history"][0]["path"].as_str().unwrap();
        let mut corrupt = std::fs::read(repo.as_path().join(reference)).unwrap();
        corrupt.push(b' ');
        std::fs::write(repo.as_path().join(reference), corrupt).unwrap();
        assert!(
            call(&["team-show", "--id", "team-a"])
                .unwrap_err()
                .message
                .contains("digest")
        );
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
        // In-worktree state: the ledger is a tracked file of the task repo.
        let repo = Tmp::repo(&[("a", "x"), (".oms/failures.jsonl", "")]);
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
                    verification: None,
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
        // A real verify resolves its tracked ledger row after the receipt;
        // that append is not source drift, while a source edit still is.
        let run = |task: &mut ControlTask<'_>, args: &[&str]| {
            let (code, out, err) =
                oms_command_task::run_with_services(&osv(args), repo.path(), &stop, task);
            (code, [out, err].concat())
        };
        let (code, log) = run(
            &mut task,
            &["init", "--goal", "ledger", "--verify", "test ! -e broken"],
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&log));
        std::fs::write(repo.path().join("broken"), "").unwrap();
        assert_eq!(run(&mut task, &["verify"]).0, 1);
        std::fs::remove_file(repo.path().join("broken")).unwrap();
        let (code, log) = run(&mut task, &["verify"]);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&log));
        let ledger = std::fs::read_to_string(repo.path().join(".oms/failures.jsonl")).unwrap();
        let rows: Vec<_> = ledger.lines().filter(|l| l.contains("broken")).collect();
        assert_eq!(rows.len(), 1, "{ledger}");
        assert!(ledger.lines().last().unwrap().contains("\"resolved\""));
        assert_eq!(
            git(repo.path(), &["diff", "--name-only"]),
            ".oms/failures.jsonl"
        );
        let status =
            |task: &mut ControlTask<'_>| String::from_utf8(run(task, &["status"]).1).unwrap();
        assert!(status(&mut task).contains("verification: fresh"));
        std::fs::write(repo.path().join("a"), "y").unwrap();
        assert!(status(&mut task).contains("verification: stale"));
        // Restore the source and finish this task before the independent lineage scenario.
        std::fs::write(repo.path().join("a"), "x").unwrap();
        let (code, log) = run(&mut task, &["close"]);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&log));

        // Same command, different tasks and a failure appended after selection:
        // only failures observed for this exact task before rerun may be linked.
        let command = "if test -f .oms/late-failure.json; then cat .oms/late-failure.json >> .oms/failures.jsonl; fi; test -f proof-ready";
        let call = |task: &mut ControlTask<'_>, args: &[&str]| {
            oms_command_task::run_with_services(&osv(args), repo.path(), &stop, task)
        };
        let (code, _, err) = call(
            &mut task,
            &[
                "init",
                "--goal",
                "exact recovery fixture",
                "--verify",
                command,
            ],
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let packet_path = repo.path().join(".oms/task/current.md");
        let packet_a = std::fs::read(&packet_path).unwrap();
        let task_a = oms_command_task::packet::metadata(&packet_a, "task_id")
            .unwrap()
            .to_vec();
        let ledger_path = repo.path().join(".oms/failures.jsonl");
        let rows = || {
            std::fs::read_to_string(&ledger_path)
                .unwrap()
                .lines()
                .map(|line| oms_command_plan::json::parse(line.as_bytes()).unwrap())
                .collect::<Vec<_>>()
        };
        let digest =
            |row: &oms_command_plan::json::Value| oms_command_plan::json::hash(row).unwrap();
        let verify = |task: &mut ControlTask<'_>, expected: i32| {
            let (code, _, err) = call(task, &["verify"]);
            assert_eq!(code, expected, "{}", String::from_utf8_lossy(&err));
        };
        verify(&mut task, 1);
        let failure_a = rows().pop().unwrap();
        assert_eq!(failure_a.get("verification").text("result"), "failed");
        assert_eq!(
            failure_a
                .get("verification")
                .text("active_task_id")
                .as_bytes(),
            task_a
        );
        assert_eq!(
            failure_a
                .get("verification")
                .get("prior_failures")
                .array()
                .unwrap()
                .len(),
            0
        );

        std::fs::write(
            &packet_path,
            oms_command_task::packet::set_metadata(&packet_a, "task_id", b"task-other"),
        )
        .unwrap();
        verify(&mut task, 1);
        let failure_b = rows().pop().unwrap();
        std::fs::write(&packet_path, &packet_a).unwrap();
        let before_late = std::fs::read(&ledger_path).unwrap();
        verify(&mut task, 1);
        let late_failure = rows().pop().unwrap();
        // Replay a genuine producer record at the deterministic execution seam.
        std::fs::write(&ledger_path, before_late).unwrap();
        std::fs::write(
            repo.path().join(".oms/late-failure.json"),
            oms_command_plan::json::encoded(&late_failure).unwrap(),
        )
        .unwrap();
        std::fs::write(repo.path().join("proof-ready"), "ready\n").unwrap();
        verify(&mut task, 0);
        let resolved = rows().pop().unwrap();
        assert_eq!(
            resolved.text("event"),
            "verified",
            "other and late failures stay open"
        );
        let evidence = resolved.get("verification");
        assert_eq!(evidence.text("result"), "accepted");
        assert_eq!(evidence.text("active_task_id").as_bytes(), task_a);
        assert_eq!(
            evidence.text("command_sha256"),
            oms_command_task::command_hash(command.as_bytes())
        );
        assert_eq!(
            evidence.text("outcome_sha256"),
            oms_command_plan::json::sha(std::fs::read(&packet_path).unwrap())
        );
        assert_eq!(
            evidence.text("verified_state").as_bytes(),
            oms_command_task::packet::metadata(
                &std::fs::read(&packet_path).unwrap(),
                "verified_state"
            )
            .unwrap()
        );
        let prior = evidence.get("prior_failures").array().unwrap();
        assert_eq!(prior, &[oms_command_plan::json::string(digest(&failure_a))]);
        assert!(!prior.contains(&oms_command_plan::json::string(digest(&failure_b))));
        assert!(!prior.contains(&oms_command_plan::json::string(digest(&late_failure))));
        assert_eq!(
            oms_command_state::task_failure_candidates(
                repo.path(),
                &task_a,
                command.as_bytes(),
                &stop
            )
            .unwrap(),
            [digest(&late_failure)]
        );
        assert_eq!(
            oms_command_state::task_failure_candidates(
                repo.path(),
                b"task-other",
                command.as_bytes(),
                &stop
            )
            .unwrap(),
            [digest(&failure_b)]
        );
        // The exact IDs are native packet reference operations in the lifecycle
        // ledger, not guessed worker/Lead attempt IDs or time-window matches.
        let lifecycle =
            std::fs::read_to_string(repo.path().join(".oms/lifecycle/events.jsonl")).unwrap();
        for key in ["snapshot_attempt_id", "outcome_attempt_id"] {
            assert!(!evidence.text(key).is_empty());
            assert!(lifecycle.lines().any(|line| {
                serde_json::from_str::<Value>(line).unwrap()["attempt_id"].as_str()
                    == Some(evidence.text(key))
            }));
        }
        let (code, out, err) = oms_command_state::run(
            "fail-ledger",
            &osv(&[
                "show",
                "--fingerprint",
                resolved.text("observed_fingerprint"),
                "--json",
            ]),
            repo.path(),
            &stop,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["failure"]["resolved"], false);
        assert_eq!(shown["failure"]["failure_sha256"], digest(&late_failure));
        assert_eq!(
            shown["failure"]["resolution_verification"]["prior_failures"][0],
            digest(&failure_a)
        );
        let (code, _, _) = oms_command_state::run(
            "fail-ledger",
            &osv(&["check", "--cmd", command, "--ignore-state"]),
            repo.path(),
            &stop,
        );
        assert_eq!(
            code, 3,
            "partial accepted evidence must not clear retry guards"
        );
        let (code, out, err) = oms_command_memory::run_with_source(
            &osv(&["search", "--text", "proof-ready", "--json"]),
            repo.path(),
            repo.path(),
            &stop,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let entries = String::from_utf8(out).unwrap();
        assert!(!entries.is_empty());
        for line in entries.lines() {
            let row: Value = serde_json::from_str(line).unwrap();
            assert_eq!(row["kind"], "failure", "{line}");
        }
        let (code, out, err) = oms_command_memory::run_with_source(
            &osv(&["health", "--json"]),
            repo.path(),
            repo.path(),
            &stop,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let health: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(health["failures"]["open_fingerprints"], 1);

        // Later exact passes can accumulate safely; the fingerprint closes
        // only after every open failure has its own accepted task/command link.
        std::fs::remove_file(repo.path().join(".oms/late-failure.json")).unwrap();
        verify(&mut task, 0);
        assert_eq!(rows().pop().unwrap().text("event"), "verified");
        std::fs::write(
            &packet_path,
            oms_command_task::packet::set_metadata(&packet_a, "task_id", b"task-other"),
        )
        .unwrap();
        verify(&mut task, 0);
        assert_eq!(rows().pop().unwrap().text("event"), "resolved");
        let (code, _, _) = oms_command_state::run(
            "fail-ledger",
            &osv(&["check", "--cmd", command, "--ignore-state"]),
            repo.path(),
            &stop,
        );
        assert_eq!(code, 0);
        std::fs::write(&packet_path, &packet_a).unwrap();

        // Missing/invalid/over-bound evidence stays unknown; no empty success.
        let accepted_ledger = std::fs::read(&ledger_path).unwrap();
        std::fs::write(
            &ledger_path,
            [accepted_ledger.as_slice(), b"torn-row\n"].concat(),
        )
        .unwrap();
        assert!(
            oms_command_state::task_failure_candidates(
                repo.path(),
                &task_a,
                command.as_bytes(),
                &stop
            )
            .is_err()
        );
        verify(&mut task, 0);
        let last = std::fs::read_to_string(&ledger_path)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_owned();
        let last = oms_command_plan::json::parse(last.as_bytes()).unwrap();
        assert_eq!(last.text("event"), "verified");
        assert!(matches!(
            last.get("verification").get("prior_failures"),
            oms_command_plan::json::Value::Null
        ));
        let mut bounded = Vec::new();
        for index in 0..129 {
            let mut row = failure_a.clone();
            let mut identity = row.get("verification").clone();
            identity
                .set(
                    "outcome_attempt_id",
                    oms_command_plan::json::string(format!("fixture-outcome-{index}")),
                )
                .unwrap();
            row.set("verification", identity).unwrap();
            bounded.extend(oms_command_plan::json::encoded(&row).unwrap());
        }
        std::fs::write(&ledger_path, bounded).unwrap();
        assert!(
            oms_command_state::task_failure_candidates(
                repo.path(),
                &task_a,
                command.as_bytes(),
                &stop
            )
            .is_err()
        );

        let resolve_manually = || {
            let (code, _, err) = oms_command_state::run(
                "fail-ledger",
                &osv(&[
                    "resolve",
                    "--cmd",
                    command,
                    "--how",
                    "operator handled failure",
                ]),
                repo.path(),
                &stop,
            );
            assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
            let row = rows().pop().unwrap();
            assert_eq!(row.text("event"), "resolved");
            assert!(row.opt("verification").is_none());
            row
        };
        resolve_manually();
        let candidates = || {
            oms_command_state::task_failure_candidates(
                repo.path(),
                &task_a,
                command.as_bytes(),
                &stop,
            )
            .unwrap()
        };
        assert!(
            candidates().is_empty(),
            "manual resolution resets old identities"
        );

        let proof = repo.path().join("proof-ready");
        std::fs::remove_file(&proof).unwrap();
        verify(&mut task, 1);
        let fresh_failure = rows().pop().unwrap();
        let fresh_digest = digest(&fresh_failure);
        assert_eq!(candidates().as_slice(), std::slice::from_ref(&fresh_digest));
        std::fs::write(&proof, "ready\n").unwrap();
        verify(&mut task, 0);
        let settled = rows().pop().unwrap();
        assert_eq!(settled.text("event"), "resolved");
        assert_eq!(
            settled
                .get("verification")
                .get("prior_failures")
                .array()
                .unwrap(),
            &[oms_command_plan::json::string(fresh_digest)]
        );
        assert!(status(&mut task).contains("verification: fresh"));

        // The command's successful receipt still stands if a manual reset occurs
        // after selection. It cannot claim recovery of that reset failure.
        std::fs::remove_file(&proof).unwrap();
        verify(&mut task, 1);
        assert_eq!(candidates().len(), 1);
        let before_reset = std::fs::read(&ledger_path).unwrap();
        let reset = resolve_manually();
        std::fs::write(&ledger_path, before_reset).unwrap();
        std::fs::write(
            repo.path().join(".oms/late-failure.json"),
            oms_command_plan::json::encoded(&reset).unwrap(),
        )
        .unwrap();
        std::fs::write(&proof, "ready\n").unwrap();
        verify(&mut task, 0);
        let latest = rows().pop().unwrap();
        assert_eq!(latest.text("event"), "verified");
        assert!(
            latest.opt("fingerprint").is_none(),
            "legacy memory must not reopen the manual resolution"
        );
        assert_eq!(
            latest.text("observed_fingerprint"),
            reset.text("fingerprint")
        );
        assert!(matches!(
            latest.get("verification").get("prior_failures"),
            oms_command_plan::json::Value::Null
        ));
        let (code, out, err) = oms_command_state::run(
            "fail-ledger",
            &osv(&["show", "--fingerprint", reset.text("fingerprint"), "--json"]),
            repo.path(),
            &stop,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["failure"]["resolved"], true);
        assert_eq!(shown["failure"]["how"], reset.text("how"));
        assert!(shown["failure"].get("resolution_verification").is_none());
        assert!(candidates().is_empty());
        assert!(status(&mut task).contains("verification: fresh"));
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
    fn invoke_concurrent_timeout_reaps_process_group() {
        use oms_command_autonomy::{Invocation, Services};
        let dir = Tmp::new();
        let stop = Arc::new(AtomicBool::new(false));
        let mut control = ControlServices::new(&stop, dir.path()).unwrap();
        let pid_file = dir.path().join("child.pid");
        let ready_file = dir.path().join("child.ready");
        let script = format!(
            "printf '%s\\n' \"$$\" > \"{pid}\"; : > \"{ready}\"; exec sleep 30",
            pid = pid_file.display(),
            ready = ready_file.display(),
        );
        control.native = sh(&script);
        let mut inv = Invocation::new("peer-delegate", dir.path(), vec![]);
        inv.wall = Duration::from_millis(300);
        let (code, _, _) = control.invoke_concurrent(&inv, &stop).unwrap();
        assert_eq!(code, 124);
        assert!(ready_file.exists());
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "process {pid} survived invoke_concurrent deadline");
    }

    #[test]
    fn staged_dispatch_launcher_requires_staged_binary_and_manifest() {
        let dir = Tmp::new();
        let source = dir.path();
        let rev = "0123456789abcdef0123456789abcdef01234567";
        let staged_dir = source.join("target/oms-native").join(rev);
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
        let bad_rev = source.join("target/oms-native/not-a-sha/oms");
        assert!(staged_dispatch_launcher(&bad_rev, source).is_empty());
    }

    #[test]
    fn control_services_list_issues_invokes_bounded_read_only_gh_get() {
        use std::os::unix::fs::PermissionsExt;

        let dir = Tmp::new();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let log = dir.path().join("gh.log");
        let fake_gh = dir.path().join("fake-gh");
        let script = format!(
            "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             printf '%s|%s|%s\\n' \"${{GH_PROMPT_DISABLED:-}}\" \"${{GH_PAGER:-}}\" \"$*\" >> \"{}\"\n\
             endpoint=\"${{*: -1}}\"\n\
             case \"$endpoint\" in\n\
               *labels=fail-exit*)\n\
                 printf 'internal auth error\\n' >&2\n\
                 exit 1\n\
                 ;;\n\
               *labels=fail-size*)\n\
                 dd if=/dev/zero bs=1024 count=600 2>/dev/null | tr '\\0' 'a'\n\
                 exit 0\n\
                 ;;\n\
             esac\n\
             printf '%s\\n' '[{{\"number\":7,\"state\":\"open\",\"title\":\"Harden intake\",\"html_url\":\"https://github.com/Org/repo/issues/7\",\"updated_at\":\"2026-01-02T03:04:05Z\",\"labels\":[{{\"name\":\"oms:ready\"}}],\"body\":\"RAW_BODY_MUST_NOT_APPEAR\"}},{{\"number\":8,\"state\":\"open\",\"title\":\"Ignore PR\",\"html_url\":\"https://github.com/Org/repo/pull/8\",\"updated_at\":\"2026-01-02T03:04:05Z\",\"labels\":[{{\"name\":\"oms:ready\"}}],\"pull_request\":{{}}}}]'\n",
            log.display(),
        );
        std::fs::write(&fake_gh, script).unwrap();
        std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let control = services(&stop, dir.path(), sh("exit 0")).with_gh(&fake_gh);
        let args: Vec<OsString> = [
            "issues",
            "--github",
            "Org/repo",
            "--label",
            "oms:ready",
            "--limit",
            "5",
            "--json",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let (code, stdout, stderr) =
            oms_command_autonomy::run_with_services("intent", &args, &repo, &stop, &control);
        assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
        let out_text = String::from_utf8(stdout).unwrap();
        assert!(!out_text.contains("RAW_BODY_MUST_NOT_APPEAR"));
        let got: Value = serde_json::from_str(&out_text).unwrap();
        assert_eq!(got["count"], 1);
        assert_eq!(got["issues"][0]["number"], 7);
        assert_eq!(
            got["issues"][0]["url"],
            "https://github.com/Org/repo/issues/7"
        );
        assert_eq!(got["issues"][0]["labels"], serde_json::json!(["oms:ready"]));
        assert_eq!(
            got["issues"][0]["next_step"],
            "oms intent draft --from-issue https://github.com/Org/repo/issues/7 --dry-run"
        );

        let recorded = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            recorded.trim(),
            "1|cat|api --hostname github.com --method GET repos/Org/repo/issues?state=open&labels=oms%3Aready&per_page=5"
        );

        // Non-zero exit fails closed without echoing gh stderr.
        let fail_args: Vec<OsString> = [
            "issues",
            "--github",
            "Org/repo",
            "--label",
            "fail-exit",
            "--limit",
            "2",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let (code, stdout, stderr) =
            oms_command_autonomy::run_with_services("intent", &fail_args, &repo, &stop, &control);
        assert_eq!(code, 2);
        assert!(!String::from_utf8_lossy(&stdout).contains("internal auth error"));
        assert!(!String::from_utf8_lossy(&stderr).contains("internal auth error"));

        // Oversized response (>512 KiB) is terminated and fails closed.
        let big_args: Vec<OsString> = [
            "issues",
            "--github",
            "Org/repo",
            "--label",
            "fail-size",
            "--limit",
            "2",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let (code, _, _) =
            oms_command_autonomy::run_with_services("intent", &big_args, &repo, &stop, &control);
        assert_eq!(code, 2);

        // No state files are ever created by read-only issue discovery.
        assert!(!repo.join(".oms").exists());
    }
}
