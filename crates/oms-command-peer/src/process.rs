//! Execution uses the supervisor reactor's owned child, cancellation and drainage.
use crate::{Error, Result};
use oms_supervisor::process::{Progress, Reactor, Request, Scope, StopReason, Streams};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptHeartbeat {
    pub repo: PathBuf,
    pub attempt: String,
}

/// Display-only view of a running provider's growing stdout prefix. It never
/// changes the completed result or its proof.
#[derive(Clone)]
pub struct StreamTap(pub std::sync::Arc<dyn Fn(&[u8]) + Send + Sync>);
impl std::fmt::Debug for StreamTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StreamTap")
    }
}

#[derive(Clone, Debug)]
pub struct Invocation {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub input: Option<Vec<u8>>,
    pub environment: Vec<(OsString, OsString)>,
    pub remove_environment: Vec<OsString>,
    pub timeout: Duration,
    pub max_bytes: usize,
    pub merge: bool,
    pub physical_call: Option<oms_supervisor::process::PhysicalCall>,
    pub launch_fence: Option<LaunchFence>,
    pub heartbeat: Option<AttemptHeartbeat>,
    pub stream: Option<StreamTap>,
}
#[derive(Clone, Debug)]
pub struct Completed {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub complete: bool,
    /// Cleanup of the executor's supervision scope, not proof of bounded native
    /// fanout. Missing execution acknowledgement stays unknown even with an exit code.
    pub cleanup_complete: Option<bool>,
    pub owned_cleanup: Option<oms_supervisor::process::OwnedCleanup>,
    pub elapsed: Duration,
}

/// Fixed native reads for an explicitly requested next-session prompt. These
/// projections cannot select provider calls, verification or state mutations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoffProjection {
    TaskStatus,
    EvidenceMatrix,
    NextStep,
    JournalDecisions,
    ReviewFindings,
    AutopilotStatus,
}
impl HandoffProjection {
    pub fn command(self, repo: &Path, state: &Path, timeout: Duration) -> Invocation {
        let (tool, args): (&str, &[&str]) = match self {
            Self::TaskStatus => ("agent-task", &["status", "--json"]),
            Self::EvidenceMatrix => ("runtime", &["evidence", "matrix", "--limit", "100"]),
            Self::NextStep => ("runtime", &["next", "--explain", "--limit", "100"]),
            Self::JournalDecisions => ("journal", &["show", "--decisions", "--json"]),
            Self::ReviewFindings => (
                "peer-review",
                &["findings", "list", "--status", "open", "--json"],
            ),
            Self::AutopilotStatus => ("autopilot", &["status"]),
        };
        let mut argv = vec!["oms".into(), tool.into()];
        argv.extend(args.iter().map(|arg| OsString::from(*arg)));
        argv.extend(["--repo".into(), repo.as_os_str().to_owned()]);
        Invocation {
            argv,
            cwd: repo.to_owned(),
            input: None,
            environment: vec![("OMS_STATE_REPO".into(), state.as_os_str().to_owned())],
            remove_environment: Vec::new(),
            timeout: timeout.min(Duration::from_secs(40)),
            max_bytes: 65536,
            merge: false,
            physical_call: None,
            launch_fence: None,
            heartbeat: None,
            stream: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamLocator {
    pub team_id: String,
    pub spec_sha256: String,
    pub member_id: String,
    pub member_generation: i128,
}
const TEAM_LOCATORS: [(&str, &str); 4] = [
    ("team-id", "OMS_TEAM_ID"),
    ("team-spec-sha256", "OMS_TEAM_SPEC_SHA256"),
    ("team-member", "OMS_TEAM_MEMBER"),
    ("team-member-generation", "OMS_TEAM_MEMBER_GENERATION"),
];
impl TeamLocator {
    fn decode(values: &[String]) -> Result<Self> {
        let identifier = |value: &str| {
            !value.is_empty()
                && value.len() <= 160
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        };
        let generation = values[3].parse::<i128>().ok().filter(|n| *n > 0);
        if !identifier(&values[0])
            || !identifier(&values[2])
            || values[1].len() != 64
            || !values[1]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !values[3].bytes().all(|b| b.is_ascii_digit())
            || generation.is_none()
        {
            return Err(Error::new(2, "invalid explicit Team locator bundle"));
        }
        Ok(Self {
            team_id: values[0].clone(),
            spec_sha256: values[1].clone(),
            member_id: values[2].clone(),
            member_generation: generation.unwrap(),
        })
    }
    pub(crate) fn from_args(args: &crate::cli::Args) -> Result<Option<Self>> {
        if TEAM_LOCATORS.iter().all(|(flag, _)| !args.has(flag)) {
            return Ok(None);
        }
        let values = TEAM_LOCATORS
            .iter()
            .map(|(flag, _)| {
                if args.all(flag).len() != 1 {
                    return Err(Error::new(
                        2,
                        "Team locators require all four options exactly once",
                    ));
                }
                args.os(flag)
                    .and_then(|value| value.to_str())
                    .map(str::to_owned)
                    .ok_or_else(|| Error::new(2, "Team locator requires UTF-8 text"))
            })
            .collect::<Result<Vec<_>>>()?;
        Self::decode(&values).map(Some)
    }
    pub(crate) fn append_args(
        args: &crate::cli::Args,
        extra: &mut Vec<(OsString, OsString)>,
    ) -> Result<()> {
        if Self::from_args(args)?.is_some() {
            for (flag, env) in TEAM_LOCATORS {
                extra.push((env.into(), args.os(flag).unwrap().into()));
            }
        }
        Ok(())
    }
    pub(crate) fn from_extra(extra: &[(OsString, OsString)]) -> Result<Option<Self>> {
        if TEAM_LOCATORS
            .iter()
            .all(|(_, key)| !extra.iter().any(|(name, _)| name == key))
        {
            return Ok(None);
        }
        let values = TEAM_LOCATORS
            .iter()
            .map(|(_, key)| {
                let found = extra
                    .iter()
                    .filter(|(name, _)| name == key)
                    .collect::<Vec<_>>();
                if found.len() != 1 {
                    return Err(Error::new(2, "incomplete or duplicate Team metadata"));
                }
                found[0]
                    .1
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::new(2, "Team metadata requires UTF-8 text"))
            })
            .collect::<Result<Vec<_>>>()?;
        Self::decode(&values).map(Some)
    }
}
pub struct PhysicalAttempt<'a> {
    pub state_root: &'a Path,
    pub contract_repo: &'a Path,
    pub execution_repo: &'a Path,
    pub team: Option<&'a TeamLocator>,
    pub root: Option<RootLocator<'a>>,
    pub provider: &'a str,
    pub account: Option<&'a str>,
    pub selected_model: Option<&'a str>,
    /// Exact effort literal sent in this invocation, never applied/support evidence.
    pub selected_effort: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub write: bool,
    pub persistent_session: bool,
}
#[derive(Clone, Copy)]
pub struct RootLocator<'a> {
    pub attempt: &'a str,
    pub participant: &'a str,
    pub room_id: &'a str,
}
pub(crate) struct OwnedPhysicalAttempt {
    state: PathBuf,
    contract: PathBuf,
    execution: PathBuf,
    root: Option<(String, String, String)>,
    team: Option<TeamLocator>,
    provider: String,
    account: Option<String>,
    selected_model: Option<String>,
    selected_effort: Option<String>,
    task_id: Option<String>,
    write: bool,
    persistent_session: bool,
}
impl PhysicalAttempt<'_> {
    pub(crate) fn owned(&self) -> OwnedPhysicalAttempt {
        OwnedPhysicalAttempt {
            state: self.state_root.into(),
            contract: self.contract_repo.into(),
            execution: self.execution_repo.into(),
            root: self.root.map(|root| {
                (
                    root.attempt.into(),
                    root.participant.into(),
                    root.room_id.into(),
                )
            }),
            team: self.team.cloned(),
            provider: self.provider.into(),
            account: self.account.map(str::to_owned),
            selected_model: self.selected_model.map(str::to_owned),
            selected_effort: self.selected_effort.map(str::to_owned),
            task_id: self.task_id.map(str::to_owned),
            write: self.write,
            persistent_session: self.persistent_session,
        }
    }
}
impl OwnedPhysicalAttempt {
    pub(crate) fn borrow(&self) -> PhysicalAttempt<'_> {
        PhysicalAttempt {
            state_root: &self.state,
            contract_repo: &self.contract,
            execution_repo: &self.execution,
            root: self
                .root
                .as_ref()
                .map(|(attempt, participant, room_id)| RootLocator {
                    attempt,
                    participant,
                    room_id,
                }),
            team: self.team.as_ref(),
            provider: &self.provider,
            account: self.account.as_deref(),
            selected_model: self.selected_model.as_deref(),
            selected_effort: self.selected_effort.as_deref(),
            task_id: self.task_id.as_deref(),
            write: self.write,
            persistent_session: self.persistent_session,
        }
    }
}
#[derive(Clone, Copy, Default)]
pub struct PhysicalContract<'a> {
    pub resource: Option<&'a oms_command_telemetry::store::resources::Binding>,
    pub expires_at: Option<i64>,
}
#[derive(Clone, Debug)]
pub struct LaunchFence {
    deadline: Instant,
    expires_at: Option<i64>,
    campaign_deadline: Option<i64>,
    plan_checkout: Option<PlanCheckout>,
}

#[derive(Clone, Debug)]
struct PlanCheckout {
    plan: crate::plan_delegate::PlanLaunch,
    worktree: PathBuf,
}

impl LaunchFence {
    pub(crate) fn new(deadline: Instant, expires_at: Option<i64>) -> Self {
        Self {
            deadline,
            expires_at,
            campaign_deadline: None,
            plan_checkout: None,
        }
    }
    pub(crate) fn with_campaign_deadline(mut self, deadline: Option<i64>) -> Self {
        self.campaign_deadline = deadline;
        self
    }
    pub(crate) fn with_plan_checkout(
        mut self,
        plan: crate::plan_delegate::PlanLaunch,
        cwd: &Path,
    ) -> Result<Self> {
        if plan.source.canonicalize()? != plan.source || cwd.canonicalize()? != cwd {
            return crate::fail("Plan source or worker checkout no longer has its physical path");
        }
        self.plan_checkout = Some(PlanCheckout {
            plan,
            worktree: cwd.to_owned(),
        });
        Ok(self)
    }

    /// Recheck the actual child cwd and retain Plan custody through a single
    /// native spawn. The caller handles supervision after this returns.
    pub fn with_start_authority<T>(
        &self,
        request: Request,
        plan_services: Option<&dyn oms_command_plan::Services>,
        interrupted: &AtomicBool,
        spawn: impl FnOnce(Request) -> T,
    ) -> Result<T> {
        self.check(interrupted)?;
        let cwd = request
            .cwd
            .as_deref()
            .ok_or_else(|| Error::new(2, "provider cwd is absent"))?;
        let Some(checkout) = &self.plan_checkout else {
            return Ok(spawn(request));
        };
        let plan = &checkout.plan;
        let services = plan_services.ok_or_else(|| {
            Error::new(
                2,
                "Plan provider start requires the owning native Plan adapter",
            )
        })?;
        let cwd = cwd.to_owned();
        let check_checkout = || {
            if plan.source.canonicalize()? != plan.source
                || cwd.canonicalize()? != checkout.worktree
            {
                return crate::fail("Plan source or worker checkout changed before provider start");
            }
            let git = oms_command_plan::repository::Git {
                repo: &checkout.worktree,
                interrupted,
                group_resolver: None,
                deadline: Some(self.deadline),
                group_snapshot: None,
            };
            crate::delegate::Worktree::check_bound_base(
                &checkout.worktree,
                plan.base_sha(),
                |args| git.run(args).map_err(|e| Error::new(e.code, e.message)),
            )
        };
        plan.with_running_before(services, interrupted, self.deadline, check_checkout, || {
            self.check(interrupted)?;
            Ok(spawn(request))
        })
    }

    pub fn check(&self, interrupted: &AtomicBool) -> Result<()> {
        crate::check(interrupted)?;
        if Instant::now() >= self.deadline {
            return Err(Error::new(
                75,
                "provider admission deadline expired; pending intent retained",
            ));
        }
        if self.expires_at.is_some() || self.campaign_deadline.is_some() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|time| i64::try_from(time.as_secs()).ok())
                .ok_or_else(|| Error::new(75, "provider launch clock unavailable"))?;
            if self
                .campaign_deadline
                .is_some_and(|deadline| now >= deadline)
            {
                return Err(Error::new(
                    3,
                    "campaign deadline expired; pending intent retained",
                ));
            }
            if self.expires_at.is_some_and(|expires_at| now >= expires_at) {
                return Err(Error::new(
                    75,
                    "Team contract expired; pending intent retained",
                ));
            }
        }
        Ok(())
    }
}
/// Wall-clock budget for native physical and Team admission and requalification.
pub const NATIVE_ADMISSION_BUDGET: Duration = Duration::from_secs(10);
/// Called at most once under the native parent's contract guards. A successful
/// callback acknowledges one physical registration; guards end before execution.
pub type RegisterPhysical<'a> =
    Box<dyn FnOnce(PhysicalContract<'_>) -> Result<oms_supervisor::process::PhysicalCall> + 'a>;
/// Only the private New-registration producer can lend this launch binding.
/// It is not a controller credential; the native owner must still enroll and
/// start its actual child before issuing a channel session.
pub struct TeamLaunchBinding<'a> {
    pub(crate) pending: &'a crate::usage::Pending,
    pub(crate) attempt: &'a PhysicalAttempt<'a>,
    pub(crate) protocol: &'a TeamProtocol,
    pub(crate) actions: &'a [TeamRequestAction],
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum TeamRequestAction {
    ReportBlocker,
    ProposeTasks,
}
impl TeamRequestAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::ReportBlocker => "report_blocker",
            Self::ProposeTasks => "propose_tasks",
        }
    }
    pub(crate) fn parse_list(value: &str) -> Result<Vec<Self>> {
        if value.is_empty() {
            return Ok(vec![Self::ReportBlocker]);
        }
        let mut actions = Vec::new();
        for action in value.split(',') {
            let action = match action {
                "report_blocker" => Self::ReportBlocker,
                "propose_tasks" => Self::ProposeTasks,
                _ => return Err(Error::new(2, "unsupported Team request action")),
            };
            if actions.contains(&action) {
                return Err(Error::new(2, "duplicate Team request action"));
            }
            actions.push(action);
        }
        actions.sort();
        Ok(actions)
    }
}
#[derive(Clone, Debug)]
pub struct TeamProtocol {
    pub(crate) binary: PathBuf,
    pub(crate) binary_sha256: String,
    pub(crate) manifest: PathBuf,
    pub(crate) manifest_sha256: String,
}
impl TeamProtocol {
    /// Checks declared selection bytes again; this is not an executing-image pin.
    pub fn check(&self) -> Result<()> {
        for (path, expected, maximum) in [
            (&self.binary, &self.binary_sha256, 16 * 1024 * 1024),
            (&self.manifest, &self.manifest_sha256, 8192),
        ] {
            if crate::storage::hash(&crate::providers::team_protocol_file(path, maximum)?)
                != *expected
            {
                return Err(Error::new(75, "Team request adapter selection changed"));
            }
        }
        Ok(())
    }
}
impl TeamLaunchBinding<'_> {
    /// Selected parent policy data; this list does not authenticate a channel.
    pub fn actions(&self) -> &[TeamRequestAction] {
        self.actions
    }
    pub fn attempt(&self) -> &PhysicalAttempt<'_> {
        self.attempt
    }
    pub fn identity(&self) -> &oms_command_telemetry::store::usage::Identity {
        &self.pending.receipt.identity
    }
    pub fn check(&self, executor: &dyn Executor, stop: &AtomicBool) -> Result<()> {
        self.protocol.check()?;
        self.pending.qualify(self.attempt, executor, stop)
    }
}
pub trait Executor: Sync {
    /// Maximum independent Council seats whose child cleanup this host can
    /// preserve concurrently. Sync alone does not grant process-wide subreaper
    /// ownership; embedded/default executors retain serial calls.
    fn council_parallelism(&self) -> usize {
        1
    }
    /// Native Plan mutation services supplied only by the owning host. The
    /// read-only peer projections never implement this delegation authority.
    fn plan_delegate_services(&self) -> Option<&dyn oms_command_plan::Services> {
        None
    }
    /// Advisory source-checked context from the existing journal/memory owners.
    /// This read cannot dispatch work or grant acceptance/resume authority.
    fn experience_context(
        &self,
        _repo: &Path,
        _query: &str,
        _interrupted: &AtomicBool,
    ) -> Result<oms_command_autonomy::services::ExperienceContext> {
        Err(Error::new(2, "native experience context unavailable"))
    }
    /// A host with an existing staged native launcher may execute only this
    /// fixed read. Missing integration is an explicit gap, never a PATH fallback.
    fn handoff_projection(
        &self,
        _projection: HandoffProjection,
        _repo: &Path,
        _state: &Path,
        _timeout: Duration,
        _interrupted: &AtomicBool,
    ) -> Result<Completed> {
        Err(Error::new(
            2,
            "native handoff projection service unavailable",
        ))
    }
    /// Read-only discovery preserves the host's qualified parser and group proof.
    fn context_dependencies(
        &self,
        repo: &Path,
        targets: &[std::path::PathBuf],
        interrupted: &AtomicBool,
    ) -> oms_command_plan::Result<Vec<oms_runtime::services::RelatedSource>> {
        oms_command_graph::project::context_dependencies(repo, targets, None, interrupted).map_err(
            |e| oms_command_plan::Error {
                code: 2,
                message: e.to_string(),
            },
        )
    }
    fn group_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        None
    }
    fn with_physical_contract(
        &self,
        attempt: &PhysicalAttempt<'_>,
        _deadline: Instant,
        interrupted: &AtomicBool,
        register: RegisterPhysical<'_>,
    ) -> Result<()> {
        crate::check(interrupted)?;
        if attempt.team.is_some() {
            return Err(Error::new(
                75,
                "canonical team admission service unavailable",
            ));
        }
        register(PhysicalContract::default()).map(|_| ())
    }
    fn execute_team_requests(
        &self,
        _request: Invocation,
        _binding: TeamLaunchBinding<'_>,
        _interrupted: &AtomicBool,
    ) -> Result<Completed> {
        Err(Error::new(75, "native Team request channel unavailable"))
    }
    /// Actual provider launch boundary. Observers may bind this typed identity
    /// to the native process they supervise. Generic executors are unchanged.
    fn execute_provider(
        &self,
        request: Invocation,
        _identity: &oms_command_telemetry::store::usage::Identity,
        _direct: bool,
        interrupted: &AtomicBool,
    ) -> Result<Completed> {
        self.execute(request, interrupted)
    }
    fn execute(&self, request: Invocation, interrupted: &AtomicBool) -> Result<Completed>;
}
pub struct Native;
impl Executor for Native {
    fn execute(&self, input: Invocation, interrupted: &AtomicBool) -> Result<Completed> {
        self.execute_with_plan_services(input, None, interrupted)
    }
}
impl Native {
    /// Start an invocation using the host's existing Plan adapter when bound.
    pub fn execute_with_plan_services(
        &self,
        input: Invocation,
        plan_services: Option<&dyn oms_command_plan::Services>,
        interrupted: &AtomicBool,
    ) -> Result<Completed> {
        crate::check(interrupted)?;
        if let Some(fence) = &input.launch_fence {
            fence.check(interrupted)?;
        }
        let allow_whole_tree = !matches!(
            input
                .argv
                .first()
                .and_then(|a| std::path::Path::new(a).file_name())
                .and_then(|n| n.to_str()),
            Some("git" | "ssh")
        );
        let stream = input.stream;
        let mut request = Request::new(input.argv);
        request.cwd = Some(input.cwd);
        request.stdin = input.input;
        request.environment = input.environment;
        request.remove_environment = input.remove_environment;
        request.physical_call = input.physical_call;
        request.streams = if input.merge {
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
                .checked_add(input.timeout)
                .ok_or_else(|| Error::new(2, "execution deadline overflow"))?,
        );
        request.capture.max_bytes = input.max_bytes;
        let started = if let Some(fence) = &input.launch_fence {
            fence.with_start_authority(request, plan_services, interrupted, |request| {
                reactor.start(request)
            })?
        } else {
            reactor.start(request)
        };
        let job = started.map_err(|e| Error::new(125, format!("process could not start: {e}")))?;
        let mut cancelled = false;
        loop {
            if interrupted.load(Ordering::Acquire) && !cancelled {
                reactor.stop(job, StopReason::Cancelled)?;
                cancelled = true;
            }
            match reactor.poll(job, Duration::from_millis(50))? {
                Progress::Pending(snapshot) => {
                    if let Some(tap) = &stream
                        && !snapshot.stdout.limited
                    {
                        (tap.0)(&snapshot.stdout.bytes);
                    }
                }
                Progress::Complete(result) => {
                    let code = match result.stop {
                        Some(StopReason::Deadline) => 124,
                        Some(StopReason::Cancelled) => 130,
                        Some(StopReason::OutputLimit | StopReason::SupervisorError) => 125,
                        _ => result.exit.map_or(125, |e| e.shell_code()),
                    };
                    let complete = result.stop.is_none()
                        && result.output_complete()
                        && result.cleanup_complete()
                        && result.supervision_error.is_none();
                    let cleanup_complete = Some(result.cleanup_complete());
                    let owned_cleanup = result.owned_cleanup().cloned();
                    return Ok(Completed {
                        code,
                        stdout: result.stdout.bytes,
                        stderr: result.stderr.map_or_else(Vec::new, |s| s.bytes),
                        complete,
                        cleanup_complete,
                        owned_cleanup,
                        elapsed: result.duration,
                    });
                }
            }
        }
    }
}
pub(crate) fn invoke(
    context: &crate::Context<'_>,
    cwd: &std::path::Path,
    argv: Vec<OsString>,
    input: Option<Vec<u8>>,
    timeout: u64,
    merge: bool,
) -> Result<Completed> {
    invoke_env(context, cwd, argv, input, Vec::new(), timeout, merge)
}

pub(crate) fn invoke_env(
    context: &crate::Context<'_>,
    cwd: &std::path::Path,
    argv: Vec<OsString>,
    input: Option<Vec<u8>>,
    environment: Vec<(OsString, OsString)>,
    timeout: u64,
    merge: bool,
) -> Result<Completed> {
    context.executor.execute(
        Invocation {
            argv,
            cwd: cwd.to_owned(),
            input,
            environment,
            remove_environment: Vec::new(),
            timeout: Duration::from_secs(timeout),
            max_bytes: 64 * 1024 * 1024,
            merge,
            physical_call: None,
            launch_fence: None,
            heartbeat: context.heartbeat.cloned(),
            stream: None,
        },
        context.interrupted,
    )
}
pub(crate) fn git(
    context: &crate::Context<'_>,
    cwd: &std::path::Path,
    args: &[&str],
) -> Result<Vec<u8>> {
    git_input(context, cwd, args, None)
}
pub(crate) fn git_input(
    context: &crate::Context<'_>,
    cwd: &std::path::Path,
    args: &[&str],
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut argv: Vec<OsString> = [
        "git",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "diff.external=",
        "-c",
        "core.attributesFile=/dev/null",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    argv.extend(args.iter().map(OsString::from));
    let remove_environment = std::env::vars_os()
        .filter_map(|(key, _)| {
            key.to_str()
                .is_some_and(|s| s.starts_with("GIT_"))
                .then_some(key)
        })
        .collect();
    let result = context.executor.execute(
        Invocation {
            argv,
            cwd: cwd.to_owned(),
            input,
            environment: vec![
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ],
            remove_environment,
            timeout: Duration::from_secs(30),
            max_bytes: 64 * 1024 * 1024,
            merge: false,
            physical_call: None,
            launch_fence: None,
            heartbeat: None,
            stream: None,
        },
        context.interrupted,
    )?;
    if result.code != 0 || !result.complete {
        return crate::fail("git operation failed or output was incomplete");
    }
    Ok(result.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn explicit_team_locators_preserve_large_generations_and_reject_ambiguous_metadata() {
        assert_eq!(
            TeamRequestAction::parse_list("").unwrap(),
            [TeamRequestAction::ReportBlocker]
        );
        assert_eq!(
            TeamRequestAction::parse_list("propose_tasks,report_blocker").unwrap(),
            [
                TeamRequestAction::ReportBlocker,
                TeamRequestAction::ProposeTasks
            ]
        );
        for invalid in [
            "request_dispatch",
            "report_blocker,report_blocker",
            "propose_tasks,",
            "all",
        ] {
            assert!(TeamRequestAction::parse_list(invalid).is_err());
        }
        assert!(
            crate::peer::parse(&["--team-request-actions".into(), "propose_tasks".into()]).is_err()
        );
        let valid = vec![
            "--team-id".into(),
            "team-a".into(),
            "--team-spec-sha256".into(),
            "a".repeat(64).into(),
            "--team-member".into(),
            "member-a".into(),
            "--team-member-generation".into(),
            "18446744073709551617".into(),
        ];
        let args = crate::peer::parse(&valid).unwrap();
        let locator = TeamLocator::from_args(&args).unwrap().unwrap();
        assert_eq!(locator.member_generation, i128::from(u64::MAX) + 2);
        let mut extra = Vec::new();
        TeamLocator::append_args(&args, &mut extra).unwrap();
        assert_eq!(TeamLocator::from_extra(&extra).unwrap(), Some(locator));
        assert_eq!(TeamLocator::from_extra(&[]).unwrap(), None);
        for count in 1..4 {
            assert!(TeamLocator::from_extra(&extra[..count]).is_err());
        }
        let mut duplicate = extra.clone();
        duplicate.push(extra[0].clone());
        assert!(TeamLocator::from_extra(&duplicate).is_err());
        let mut duplicate = valid.clone();
        duplicate.extend(["--team-id".into(), "team-b".into()]);
        assert!(crate::peer::parse(&duplicate).is_err());
        for invalid in ["0", "-1", "170141183460469231731687303715884105728", ""] {
            let mut args = valid.clone();
            *args.last_mut().unwrap() = invalid.into();
            assert!(crate::peer::parse(&args).is_err());
        }
        let mut nontext = valid;
        nontext[1] = OsString::from_vec(vec![0xff]);
        assert!(crate::peer::parse(&nontext).is_err());
        let mut nontext = extra;
        nontext[1].1 = OsString::from_vec(vec![0xff]);
        assert!(TeamLocator::from_extra(&nontext).is_err());
    }

    #[test]
    fn expired_launch_fences_refuse_before_native_spawn() {
        let stop = AtomicBool::new(false);
        let request = Invocation {
            argv: vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            cwd: std::env::current_dir().unwrap(),
            input: None,
            environment: Vec::new(),
            remove_environment: Vec::new(),
            timeout: Duration::from_secs(1),
            max_bytes: 1024,
            merge: false,
            physical_call: None,
            launch_fence: Some(LaunchFence::new(
                Instant::now() + Duration::from_secs(1),
                Some(1),
            )),
            heartbeat: None,
            stream: None,
        };
        assert_eq!(Native.execute(request.clone(), &stop).unwrap_err().code, 75);
        let mut deadline = request;
        deadline.launch_fence = Some(LaunchFence::new(Instant::now(), None));
        assert_eq!(Native.execute(deadline, &stop).unwrap_err().code, 75);
    }
}
