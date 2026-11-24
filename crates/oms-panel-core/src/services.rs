//! Fixed command routing; argv never passes through a shell.
use crate::{Error, Result, Value, checkpoint, decode, require};
use oms_supervisor::process::{Progress, Reactor, Request, Scope, StopReason, Streams};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeCommand {
    PeerAsk,
    Cleanup,
    ProviderCatalog,
    Delegate,
    Review,
    Admit,
    Land,
    Room,
    Events,
    Plan,
    Task,
    Bundle,
    Telemetry,
    Failure,
    Advise,
}
impl NativeCommand {
    pub fn verb(self) -> &'static str {
        match self {
            Self::PeerAsk => "peer-ask",
            Self::Cleanup => "cleanup",
            Self::ProviderCatalog => "models",
            Self::Delegate => "peer-delegate",
            Self::Review => "peer-review",
            Self::Admit => "patch-admit",
            Self::Land => "patch-land",
            Self::Room => "room",
            Self::Events | Self::Bundle => "agent-events",
            Self::Plan => "agent-plan",
            Self::Task => "agent-task",
            Self::Telemetry => "artifact-index",
            Self::Failure => "failure-analyze",
            Self::Advise => "advise",
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct CommandOutput {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub cleanup_complete: bool,
    pub output_complete: bool,
    pub limited: bool,
}
impl CommandOutput {
    pub fn success(&self) -> Result<()> {
        require(
            self.code == 0 && self.cleanup_complete && self.output_complete && !self.limited,
            "native command failed or completion evidence is incomplete",
        )
    }
    pub fn json(&self) -> Result<Value> {
        self.success()?;
        decode(&self.stdout, 16 * 1024 * 1024)
    }
    fn native(v: (i32, Vec<u8>, Vec<u8>)) -> Self {
        Self {
            code: v.0,
            stdout: v.1,
            stderr: v.2,
            cleanup_complete: true,
            output_complete: true,
            limited: false,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ActivityHeartbeat {
    pub repo: PathBuf,
    pub attempt: String,
    pub actor: String,
}
#[derive(Clone, Debug)]
pub struct ExecutionRequest {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    /// Explicit values override ambient removals of the same name.
    pub environment: Vec<(OsString, OsString)>,
    pub remove_environment: Vec<OsString>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub output_limit: usize,
    pub heartbeat: Option<ActivityHeartbeat>,
}
impl ExecutionRequest {
    pub fn new(argv: Vec<OsString>, cwd: &Path) -> Self {
        Self {
            argv,
            cwd: cwd.into(),
            environment: vec![],
            remove_environment: vec![],
            stdin: None,
            timeout: Duration::from_secs(3600),
            output_limit: 8 * 1024 * 1024,
            heartbeat: None,
        }
    }
}
/// One reserved Panel dispatch. This travels only over the controller's
/// authenticated staged channel, independent of the provider environment.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchLaunch {
    pub nonce: String,
    pub repo: PathBuf,
    pub execution: PathBuf,
    pub room: String,
    pub main: String,
    pub main_attempt: String,
    pub main_created: String,
    pub main_provider: String,
    pub member: String,
    pub task: String,
    pub provider: String,
    pub membership_sha256: String,
    pub reservation_sha256: String,
    pub reservation_generation: [i128; 5],
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchProcess {
    pub pid: u32,
    pub birth: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchProof {
    pub launch: DispatchLaunch,
    pub stage: DispatchProcess,
    pub provider: DispatchProcess,
    pub call: String,
    pub attempt: String,
    pub exit: i32,
}

/// In-process hosts can provide their existing supervised process reactor.
pub trait PeerExecutor {
    fn execute(
        &self,
        request: &ExecutionRequest,
        interrupted: &AtomicBool,
    ) -> Result<CommandOutput>;
}
/// A read-only host proof that can also run on the background Board worker.
pub type WorkspaceGroupResolver =
    fn(u32, std::time::Instant, &AtomicBool) -> Option<oms_command_plan::services::GroupMembership>;

/// Only these domain services may produce authority. Display snapshots confer none.
pub trait Services: PeerExecutor {
    fn workspace_group_resolver(&self) -> Option<WorkspaceGroupResolver> {
        None
    }
    /// Complete host account-database proof for a group writable only by this UID.
    fn workspace_group(
        &self,
        gid: u32,
        deadline: std::time::Instant,
        stop: &AtomicBool,
    ) -> Option<oms_command_plan::services::GroupMembership> {
        self.workspace_group_resolver()?(gid, deadline, stop)
    }
    /// Team/source readers share the caller's selected roots. Contextual hosts
    /// can bind them directly; subprocess hosts pass an explicit state locator.
    fn team_contracts(
        &self,
        execution: &Path,
        state: &Path,
        stop: &AtomicBool,
    ) -> Result<CommandOutput> {
        let execution = std::fs::canonicalize(execution).map_err(Error::new)?;
        let state = std::fs::canonicalize(state).map_err(Error::new)?;
        let ambient = std::env::var_os("OMS_STATE_REPO").filter(|value| !value.is_empty());
        let same_state = ambient.as_ref().is_none_or(|value| {
            std::fs::canonicalize(execution.join(value)).ok().as_ref() == Some(&state)
        });
        if execution == state && same_state {
            return self.command(
                NativeCommand::Plan,
                &execution,
                &args(&["team-list", "--json"]),
                stop,
            );
        }
        let mut request = ExecutionRequest::new(
            vec![
                "team-list".into(),
                "--json".into(),
                "--repo".into(),
                execution.as_os_str().into(),
            ],
            &execution,
        );
        request
            .environment
            .push(("OMS_STATE_REPO".into(), state.as_os_str().into()));
        self.command_context(NativeCommand::Plan, &request, stop)
    }
    /// Native interactive calls reuse the parent's canonical physical factory.
    /// Unavailable hosts retain legacy admission, which refuses active domains.
    fn physical_executor(&self) -> Option<&dyn oms_command_peer::process::Executor> {
        None
    }
    /// Native host handshake: fence the pane generation, obtain a bounded
    /// handoff, confirm old-native exit, append Room::swap_main under its native
    /// operations read guard, and start the replacement in that same pane.
    /// The adapter owns interrupted-host reconciliation and must never replay
    /// an unconfirmed input/launch. Core independently checks lifecycle/room proof.
    /// Called with the panel startup reservation held. The source wrapper owns
    /// the lifetime session lock until exit, and the replacement acquires it
    /// during normal startup.
    fn swap_main_host(
        &self,
        _repo: &Path,
        _request: &crate::release::MainSwapRequest,
        _main: &Value,
        _stop: &AtomicBool,
    ) -> Result<Value> {
        Err(Error::new("native main swap host adapter unavailable"))
    }
    /// Read-only check for source-owned contracts and current host admission.
    fn check_main_swap_source(
        &self,
        _repo: &Path,
        _main: &Value,
        _stop: &AtomicBool,
    ) -> Result<()> {
        Err(Error::new("native main swap source preflight unavailable"))
    }
    /// Append the canonical Room swap after positively proving source exit.
    /// Keep lifecycle read authority inside the real Room producer.
    fn commit_main_swap(
        &self,
        _repo: &Path,
        _room: &str,
        _participant: &str,
        _request: &crate::release::MainSwapRequest,
        _deadline: std::time::Instant,
        _stop: &AtomicBool,
    ) -> Result<Value> {
        Err(Error::new("native main swap Room writer unavailable"))
    }
    /// Lossless Unicode normalization/transliteration inspection belongs to the
    /// native encoding producer. ASCII patches need no Unicode adapter.
    fn patch_encoding_warnings(&self, patch: &[u8]) -> Result<Vec<Value>> {
        require(
            patch.is_ascii(),
            "native Unicode patch inspection adapter unavailable",
        )?;
        Ok(vec![])
    }
    /// Raw canonical publication receipt selected by the native plan's finish
    /// proof. The core rechecks its digest and exact replay base. Never return
    /// a synthesized success receipt or infer one from a Git commit alone.
    fn publication_receipt(
        &self,
        _repo: &Path,
        _execution: &Path,
        _commit: &str,
        _receipt_sha256: &str,
        _stop: &AtomicBool,
    ) -> Result<Vec<u8>> {
        Err(Error::new("native publication receipt adapter unavailable"))
    }
    /// Complete canonical room history, oldest sealed predecessor first. Display
    /// projections and lifecycle refs cannot authenticate main swaps.
    fn room_history(&self, repo: &Path, room_id: &str, stop: &AtomicBool) -> Result<Vec<Value>> {
        let local = oms_command_room::LocalServices;
        let room = oms_command_room::Room::open(
            repo,
            oms_command_room::Session::from_env(),
            &local,
            stop,
            false,
            true,
        )
        .map_err(Error::new)?;
        let head = room.resolve(room_id).map_err(Error::new)?;
        let mut rooms = room.sealed_predecessors(&head).map_err(Error::new)?;
        rooms.reverse();
        rooms.push(head);
        let mut output = Vec::new();
        for ident in rooms {
            for row in room.records(&ident).map_err(Error::new)? {
                let bytes =
                    oms_command_room::value::encode(&row, false, true).map_err(Error::new)?;
                output.push(crate::decode(&bytes, oms_command_room::MAX_ROW)?);
            }
        }
        Ok(output)
    }
    /// Parent app integration returns its durable delivery receipt. Unavailable
    /// delivery leaves the core's pending intent and requires an explicit retry.
    fn deliver_result(
        &self,
        _repo: &Path,
        _message: &str,
        _interrupted: &AtomicBool,
    ) -> Result<Value> {
        Err(Error::new("native app result delivery service unavailable"))
    }
    fn register_result(&self, repo: &Path, result: &Value, interrupted: &AtomicBool) -> Result<()> {
        let field = |name| {
            result[name]
                .as_str()
                .ok_or_else(|| Error::new("result registration field missing"))
        };
        oms_command_telemetry::store::register_panel_result(
            repo,
            field("owner")?,
            field("task_id")?,
            Path::new(field("artifact")?),
            interrupted,
        )
        .map(|_| ())
        .map_err(telemetry_error)
    }
    fn command(
        &self,
        command: NativeCommand,
        repo: &Path,
        args: &[OsString],
        interrupted: &AtomicBool,
    ) -> Result<CommandOutput>;
    fn model_family(&self, provider: &str, model: &str) -> Result<String> {
        crate::routing::model_family(provider, model).map(str::to_owned)
    }
    /// Terminal adapters must prove recorded host identity, not mere pane absence.
    fn host_absent(&self, repo: &Path, attempt: &Value) -> Result<bool> {
        crate::host::absent(repo, attempt, self)
    }
    fn accept(
        &self,
        repo: &Path,
        request: &crate::results::AcceptRequest,
        interrupted: &AtomicBool,
    ) -> Result<Value> {
        crate::results::accept_native(repo, request, self, interrupted)
    }
    /// Reconciles write custody only after canonical acceptance and every
    /// component registration. The native producer must revalidate the saved
    /// acceptance under its owner guards; these arguments grant no authority.
    fn accepted_writes(
        &self,
        _repo: &Path,
        _request: &crate::results::AcceptRequest,
        _record: &Value,
        _interrupted: &AtomicBool,
    ) -> Result<Option<Value>> {
        Ok(None)
    }
    fn finalize_authority(
        &self,
        repo: &Path,
        request: &crate::results::FinalizeRequest,
        interrupted: &AtomicBool,
    ) -> Result<Value> {
        crate::results::finalize_snapshot(repo, request, self, interrupted)
    }
    fn dispatch_context(
        &self,
        command: NativeCommand,
        request: &ExecutionRequest,
        _launch: Option<&DispatchLaunch>,
        interrupted: &AtomicBool,
    ) -> Result<(CommandOutput, Option<DispatchProof>)> {
        self.command_context(command, request, interrupted)
            .map(|output| (output, None))
    }
    fn command_context(
        &self,
        command: NativeCommand,
        request: &ExecutionRequest,
        interrupted: &AtomicBool,
    ) -> Result<CommandOutput> {
        require(
            request.environment.is_empty() && request.remove_environment.is_empty(),
            "contextual native command service required",
        )?;
        self.command(command, &request.cwd, &request.argv, interrupted)
    }
}

/// The executable must be the sibling native OMS binary; no provider CLI lookup
/// or legacy interpreter fallback is performed. Native read domains are direct.
pub struct SubcommandServices {
    pub executable: PathBuf,
    workspace_group: Option<WorkspaceGroupResolver>,
}
impl SubcommandServices {
    pub fn sibling() -> Result<Self> {
        let current = std::env::current_exe().map_err(Error::new)?;
        Ok(Self {
            executable: current
                .parent()
                .ok_or_else(|| Error::new("executable directory unavailable"))?
                .join("oms"),
            workspace_group: None,
        })
    }
    pub fn at(executable: PathBuf) -> Result<Self> {
        require(
            executable.is_absolute(),
            "OMS adapter executable must be absolute",
        )?;
        Ok(Self {
            executable,
            workspace_group: None,
        })
    }
    pub fn with_workspace_group(mut self, resolver: Option<WorkspaceGroupResolver>) -> Self {
        self.workspace_group = resolver;
        self
    }
}
impl PeerExecutor for SubcommandServices {
    fn execute(
        &self,
        request: &ExecutionRequest,
        interrupted: &AtomicBool,
    ) -> Result<CommandOutput> {
        checkpoint(interrupted)?;
        require(
            !request.argv.is_empty() && !request.argv[0].is_empty() && request.argv.len() <= 256,
            "process argv must contain 1..256 arguments",
        )?;
        require(
            !request.timeout.is_zero()
                && request.timeout <= Duration::from_secs(86400)
                && request.output_limit > 0
                && request.output_limit <= 256 * 1024 * 1024,
            "invalid process limits",
        )?;
        let mut child = Request::new(request.argv.clone());
        child.cwd = Some(std::fs::canonicalize(&request.cwd).map_err(Error::new)?);
        child.stdin = request.stdin.clone();
        child.environment = request.environment.clone();
        child.remove_environment = request
            .remove_environment
            .iter()
            .filter(|name| !request.environment.iter().any(|(key, _)| key == *name))
            .cloned()
            .collect();
        child.streams = Streams::Separate;
        child.capture.max_bytes = request.output_limit;
        child.deadline = Some(Instant::now() + request.timeout);
        child.scope = Scope::ProcessGroup;
        let mut reactor = Reactor::new();
        let job = reactor.start(child).map_err(Error::new)?;
        let mut cancelling = false;
        let mut heartbeat_at = Instant::now();
        let mut heartbeat_failed = false;
        loop {
            if let Some(activity) = &request.heartbeat
                && !cancelling
                && heartbeat_at.elapsed() >= Duration::from_secs(60)
            {
                let output = oms_command_operations::run(
                    oms_command_operations::App::Events,
                    &args(&[
                        "heartbeat",
                        "--attempt",
                        &activity.attempt,
                        "--actor",
                        &activity.actor,
                    ]),
                    &activity.repo,
                    interrupted,
                );
                heartbeat_failed |= output.0 != 0;
                heartbeat_at = Instant::now();
            }
            if interrupted.load(std::sync::atomic::Ordering::Acquire) && !cancelling {
                reactor
                    .stop(job, StopReason::Signal(2))
                    .map_err(Error::new)?;
                cancelling = true;
            }
            match reactor
                .poll(job, Duration::from_millis(50))
                .map_err(Error::new)?
            {
                Progress::Pending(_) => {}
                Progress::Complete(done) => {
                    let cleanup_complete = done.cleanup_complete();
                    let output_complete = done.output_complete();
                    let limited =
                        done.stdout.limited || done.stderr.as_ref().is_some_and(|v| v.limited);
                    let mut stderr = done.stderr.map(|v| v.bytes).unwrap_or_default();
                    if heartbeat_failed {
                        stderr.extend_from_slice(b"\nOMS activity update unavailable\n");
                    }
                    return Ok(CommandOutput {
                        code: done.exit.map(|v| v.shell_code()).unwrap_or(75),
                        stdout: done.stdout.bytes,
                        stderr,
                        cleanup_complete,
                        output_complete,
                        limited,
                    });
                }
            }
        }
    }
}
impl Services for SubcommandServices {
    fn workspace_group_resolver(&self) -> Option<WorkspaceGroupResolver> {
        self.workspace_group
    }
    fn command_context(
        &self,
        command: NativeCommand,
        request: &ExecutionRequest,
        interrupted: &AtomicBool,
    ) -> Result<CommandOutput> {
        require(
            self.executable.is_absolute()
                && self.executable.file_name().is_some_and(|v| v == "oms"),
            "adapter requires sibling native oms executable",
        )?;
        let mut request = request.clone();
        let mut argv = vec![
            self.executable.clone().into_os_string(),
            command.verb().into(),
            "--repo".into(),
            request.cwd.clone().into_os_string(),
        ];
        argv.extend(request.argv);
        request.argv = argv;
        self.execute(&request, interrupted)
    }
    fn command(
        &self,
        command: NativeCommand,
        repo: &Path,
        args: &[OsString],
        interrupted: &AtomicBool,
    ) -> Result<CommandOutput> {
        checkpoint(interrupted)?;
        let native = match command {
            NativeCommand::Room => Some(oms_command_room::run(args, repo, interrupted)),
            NativeCommand::Events => Some(oms_command_operations::run(
                oms_command_operations::App::Events,
                args,
                repo,
                interrupted,
            )),
            NativeCommand::Bundle => Some(oms_command_attempt_bundle::run(args, repo, interrupted)),
            NativeCommand::Telemetry if oms_command_telemetry::owns(args) => {
                Some(oms_command_telemetry::run(args, repo, interrupted))
            }
            NativeCommand::Failure => Some(oms_command_failure::run(args, repo, interrupted)),
            NativeCommand::Task
                if args
                    .first()
                    .and_then(|v| v.to_str())
                    .is_some_and(|v| matches!(v, "show" | "list" | "status")) =>
            {
                Some(oms_command_task::run(args, repo, interrupted))
            }
            _ => None,
        };
        if let Some(v) = native {
            return Ok(CommandOutput::native(v));
        }
        require(
            self.executable.is_absolute()
                && self.executable.file_name().is_some_and(|v| v == "oms"),
            "adapter requires the sibling native oms executable",
        )?;
        let mut argv = vec![
            self.executable.clone().into_os_string(),
            command.verb().into(),
            "--repo".into(),
            repo.into(),
        ];
        argv.extend_from_slice(args);
        self.execute(&ExecutionRequest::new(argv, repo), interrupted)
    }
}
pub(crate) fn telemetry_error(error: oms_command_telemetry::store::StoreError) -> Error {
    use oms_command_telemetry::store::StoreError;
    use oms_safe_fs::MutationPhase;
    Error {
        code: error.exit_code(),
        publication_uncertain: matches!(
            &error,
            StoreError::Storage {
                phase: MutationPhase::PublicationUncertain | MutationPhase::Published,
                ..
            }
        ),
        ..Error::new(error)
    }
}
pub(crate) fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
pub(crate) fn query<S: Services + ?Sized>(
    service: &S,
    command: NativeCommand,
    repo: &Path,
    values: &[&str],
    interrupted: &AtomicBool,
) -> Result<Value> {
    service
        .command(command, repo, &args(values), interrupted)?
        .json()
}
