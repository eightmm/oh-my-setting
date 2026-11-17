//! Host integration for native producers and bounded external commands.
use crate::{CommandResult, Error, Result, checkpoint, value};
use oms_command_plan::services::{
    Execution, LifecycleGuard, ProjectedLifecycle, ProjectedRoom, SupervisedCommand,
};
use oms_supervisor::capture::{CaptureOptions, LimitAction};
use oms_supervisor::process::{Progress, Reactor, Request, Scope, StopReason, Streams};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Invocation {
    /// Public native OMS verb, never a shell command or script path.
    pub tool: String,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub wall: Duration,
    pub output_limit: usize,
    pub environment: Vec<(OsString, OsString)>,
}
#[derive(Clone, Debug)]
pub struct ModelRoute {
    pub provider: String,
    pub model_class: String,
    pub primary: String,
    pub fallback: String,
    pub effort: String,
    pub fallback_effort: String,
}
impl ModelRoute {
    pub fn contract(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.model_class, self.primary, self.fallback, self.effort, self.fallback_effort
        )
    }
}
pub struct JournalEvent<'a> {
    pub repo: &'a Path,
    pub source_kind: &'static str,
    pub source_path: &'a Path,
    pub source_id: &'a str,
    pub outcome: &'a str,
    pub verification: &'static str,
}
impl Invocation {
    pub fn new(tool: &str, cwd: &Path, args: Vec<OsString>) -> Self {
        Self {
            tool: tool.into(),
            args,
            cwd: cwd.into(),
            wall: Duration::from_secs(1800),
            output_limit: 8 * 1024 * 1024,
            environment: vec![],
        }
    }
}

/// Extend the plan producer's service contract instead of creating alternate
/// approval, lifecycle, room or acceptance authorities. Host callbacks must
/// dispatch native implementations and preserve their exact exit/output.
/// `Sync` lets `plan-run --parallel` share one host across worker threads.
pub trait Services: oms_command_plan::Services + Sync {
    /// Resolve the native registry/model-routing contract without calling a
    /// provider. The returned exact route is used for both failure identity
    /// and delegation, preventing defaults from moving between those steps.
    fn resolve_route(
        &self,
        _repo: &Path,
        _assignment: &serde_json::Value,
        _role: &str,
        _interrupted: &AtomicBool,
    ) -> Result<ModelRoute> {
        Err(Error::new(2, "native model-routing service unavailable"))
    }
    /// Adapter to the canonical native Work Journal source observer. Errors
    /// remain advisory after the source producer durably completed its work.
    fn observe_journal(&self, _event: JournalEvent<'_>, _interrupted: &AtomicBool) -> Result<()> {
        Err(Error::new(2, "native journal source observer unavailable"))
    }
    /// Read-only native producer section for bounded work discovery. Missing
    /// evidence is reported as unavailable, never as an empty successful scan.
    fn discovery_section(
        &self,
        _repo: &Path,
        _section: &str,
        _interrupted: &AtomicBool,
    ) -> Result<serde_json::Value> {
        Err(Error::new(
            2,
            "native discovery projection service unavailable",
        ))
    }
    fn provider_names(&self) -> Result<Vec<String>> {
        Ok(["codex", "claude", "antigravity"]
            .iter()
            .map(|s| s.to_string())
            .collect())
    }
    fn read_issue(&self, _endpoint: &str, _interrupted: &AtomicBool) -> Result<Vec<u8>> {
        Err(Error::new(
            2,
            "native bounded GitHub GET service unavailable",
        ))
    }
    /// Resolve an explicitly reviewed feedback identity through the native
    /// landing observer and fresh exact-head GitHub GETs. This is evidence,
    /// never a grant to dispatch or change a task's scope.
    fn repair_feedback(
        &self,
        _repo: &Path,
        _state: &Path,
        _event_id: &str,
        _head: &str,
        _interrupted: &AtomicBool,
    ) -> Result<serde_json::Value> {
        Err(Error::new(2, "native feedback repair evidence unavailable"))
    }
    /// Repeat the remote head/revision/diagnosis check immediately before
    /// dispatch. An unavailable response is unknown and must refuse a send.
    fn revalidate_repair_feedback(
        &self,
        _repo: &Path,
        _evidence: &serde_json::Value,
        _interrupted: &AtomicBool,
    ) -> Result<()> {
        Err(Error::new(
            2,
            "native feedback repair revalidation unavailable",
        ))
    }
    fn work_repository(&self, path: &Path, _interrupted: &AtomicBool) -> Result<PathBuf> {
        let physical = std::fs::canonicalize(path)
            .map_err(|_| Error::new(2, "cannot resolve physical repository"))?;
        let git = oms_command_plan::repository::Git {
            repo: &physical,
            interrupted: _interrupted,
            group_resolver: self.group_membership_resolver(),
        };
        if let Ok(root) = git.text(&["rev-parse", "--show-toplevel"]) {
            std::fs::canonicalize(root)
                .map_err(|_| Error::new(2, "cannot resolve physical worktree"))
        } else {
            Ok(physical)
        }
    }
    fn state_repository(&self, work: &Path, _interrupted: &AtomicBool) -> Result<PathBuf> {
        let state = std::env::var_os("OMS_STATE_REPO")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| work.into());
        std::fs::canonicalize(state)
            .map_err(|_| Error::new(2, "cannot resolve canonical state repository"))
    }
    fn invoke(&self, _command: &Invocation, _interrupted: &AtomicBool) -> Result<CommandResult> {
        Err(Error::new(
            2,
            "native OMS command service unavailable; parent CLI injection is required",
        ))
    }
    /// Variant used by multi-threaded controllers (`plan-run --parallel`) where
    /// process-wide single-thread subreaper ownership is unavailable; children
    /// themselves remain single-threaded and retain their own containment.
    fn invoke_concurrent(
        &self,
        command: &Invocation,
        interrupted: &AtomicBool,
    ) -> Result<CommandResult> {
        self.invoke(command, interrupted)
    }
    /// Native context-pack validator; an orientation pack never changes scope.
    fn validate_context_pack(
        &self,
        _repo: &Path,
        _path: &Path,
        _interrupted: &AtomicBool,
    ) -> Result<serde_json::Value> {
        Err(Error::new(
            2,
            "native context-pack validator service unavailable",
        ))
    }
    /// Producer returns the executing native controller's immutable identity.
    fn controller_digest(&self) -> Result<String> {
        Err(Error::new(
            2,
            "native executing controller identity unavailable",
        ))
    }
}

/// Read-only local projections and native process supervision. Provider and
/// publication invocations require `run_with_services`; the library does not
/// guess whether its embedding executable supports child OMS routing.
pub struct LocalServices<'a> {
    interrupted: &'a AtomicBool,
}
impl<'a> LocalServices<'a> {
    pub fn new(interrupted: &'a AtomicBool) -> Self {
        Self { interrupted }
    }
}
struct EventsGuard {
    _lock: oms_command_operations::lock::FileLock,
}
impl LifecycleGuard for EventsGuard {}
impl oms_command_plan::Services for LocalServices<'_> {
    fn lifecycle_writer(
        &self,
        repo: &Path,
    ) -> oms_command_plan::Result<Box<dyn LifecycleGuard + '_>> {
        oms_command_operations::lock::FileLock::acquire(
            &oms_command_operations::event_path(repo),
            self.interrupted,
        )
        .map(|lock| Box::new(EventsGuard { _lock: lock }) as Box<dyn LifecycleGuard>)
        .map_err(plan_error)
    }
    fn project_lifecycle(&self, raw: &[u8]) -> oms_command_plan::Result<ProjectedLifecycle> {
        let rows = raw
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(oms_command_operations::value::V::parse)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(plan_error)?;
        let projection = oms_command_operations::project_attempts(&rows).map_err(plan_error)?;
        let mut attempts = oms_command_plan::json::empty();
        for item in projection {
            let id = item
                .get("attempt_id")
                .text()
                .ok_or_else(|| plan_error("missing attempt identity"))?;
            attempts.set(
                id,
                oms_command_plan::json::parse(&item.json(false).map_err(plan_error)?)?,
            )?;
        }
        Ok(ProjectedLifecycle {
            source_sha256: value::hash(raw),
            attempts,
        })
    }
    fn project_room(&self, _room: &str, raw: &[u8]) -> oms_command_plan::Result<ProjectedRoom> {
        let rows = raw
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| oms_command_room::value::parse(line, 1024 * 1024))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| plan_error(e.to_string()))?;
        let projected = oms_command_room::project(&rows).map_err(|e| plan_error(e.to_string()))?;
        let encoded = oms_command_room::value::encode(&projected, false, true)
            .map_err(|e| plan_error(e.to_string()))?;
        Ok(ProjectedRoom {
            source_sha256: value::hash(raw),
            state: oms_command_plan::json::parse(&encoded)?,
        })
    }
    fn supervise(&self, command: &SupervisedCommand) -> oms_command_plan::Result<Execution> {
        let mut request = Request::new(command.argv.clone());
        request.cwd = Some(command.cwd.clone());
        request.environment = command.environment.clone();
        request.remove_environment = command
            .remove_environment
            .iter()
            .map(OsString::from)
            .collect();
        let execution = supervise(
            request,
            command.wall,
            command.output_limit,
            self.interrupted,
        )
        .map_err(|e| plan_error(e.message))?;
        Ok(Execution {
            exit: execution.0.clamp(0, 255) as u8,
            runner_exit: 0,
            timed_out: execution.0 == 124,
            output_limited: execution.0 == 125,
            launch_error: false,
            supervision_error: false,
            output: execution.1,
            duration_s: execution.3,
        })
    }
}
impl Services for LocalServices<'_> {
    fn read_issue(&self, endpoint: &str, interrupted: &AtomicBool) -> Result<Vec<u8>> {
        crate::require(
            regex::Regex::new(r"^repos/[A-Za-z0-9-]+/[A-Za-z0-9_.-]+/issues/[1-9][0-9]*$")
                .unwrap()
                .is_match(endpoint),
            "invalid issue GET endpoint",
        )?;
        let mut request = Request::new(vec![
            "gh".into(),
            "api".into(),
            "--hostname".into(),
            "github.com".into(),
            "--method".into(),
            "GET".into(),
            endpoint.into(),
        ]);
        request.environment = vec![
            ("GH_PROMPT_DISABLED".into(), "1".into()),
            ("GH_PAGER".into(), "cat".into()),
        ];
        let (code, raw, _, _) =
            supervise(request, Duration::from_secs(20), 512 * 1024, interrupted)?;
        crate::require(
            code == 0,
            "GitHub source unavailable or exceeded its bounded GET",
        )?;
        Ok(raw)
    }
}
fn plan_error(message: impl Into<String>) -> oms_command_plan::Error {
    oms_command_plan::Error {
        code: 2,
        message: message.into(),
    }
}

/// Native whole-tree execution used by the host's `invoke` adapter. A host
/// that shares a supervisor reactor should inject that reactor instead of
/// opening a second exclusive reaper. Unsupported containment refuses.
pub fn supervise(
    mut request: Request,
    wall: Duration,
    limit: usize,
    interrupted: &AtomicBool,
) -> Result<(i32, Vec<u8>, Vec<u8>, u64)> {
    checkpoint(interrupted)?;
    crate::require(
        !wall.is_zero() && limit > 0,
        "execution requires positive wall and output bounds",
    )?;
    request.scope = Scope::WholeTree;
    request.deadline = Instant::now().checked_add(wall);
    crate::require(request.deadline.is_some(), "execution deadline overflow")?;
    request.capture = CaptureOptions {
        max_bytes: limit,
        on_limit: LimitAction::Terminate,
        lines: None,
    };
    request.stderr_capture = Some(request.capture.clone());
    let mut reactor = Reactor::dedicated().map_err(|e| Error::new(2, e))?;
    let id = reactor
        .start(request)
        .map_err(|e| Error::new(2, e.to_string()))?;
    let mut cancelled = false;
    loop {
        if interrupted.load(std::sync::atomic::Ordering::Acquire) && !cancelled {
            reactor
                .stop(id, StopReason::Cancelled)
                .map_err(|e| Error::new(2, e))?;
            cancelled = true;
        }
        match reactor
            .poll(id, Duration::from_millis(50))
            .map_err(|e| Error::new(2, e))?
        {
            Progress::Pending(_) => {}
            Progress::Complete(done) => {
                crate::require(
                    done.cleanup_complete() && done.output_complete(),
                    "native supervision incomplete; reconcile before retry",
                )?;
                let code = match done.stop {
                    Some(StopReason::Deadline) => 124,
                    Some(StopReason::OutputLimit) => 125,
                    Some(StopReason::Cancelled) => 130,
                    Some(_) => 125,
                    None => done.exit.map_or(125, |e| e.shell_code()),
                };
                return Ok((
                    code,
                    done.stdout.bytes,
                    done.stderr.map_or_else(Vec::new, |s| s.bytes),
                    done.duration.as_secs(),
                ));
            }
        }
    }
}

/// Optional clean child adapter. The host supplies the native binary prefix
/// (usually `current_exe`); argv is never interpreted by a shell.
pub fn invoke_binary(
    prefix: &[OsString],
    command: &Invocation,
    interrupted: &AtomicBool,
) -> Result<CommandResult> {
    crate::require(
        !prefix.is_empty() && crate::value::id(&command.tool, 64),
        "invalid native command prefix or verb",
    )?;
    let mut argv = prefix.to_vec();
    argv.push(command.tool.clone().into());
    argv.extend(command.args.clone());
    let mut request = Request::new(argv);
    request.streams = Streams::Separate;
    request.cwd = Some(command.cwd.clone());
    request.environment = command.environment.clone();
    let (code, out, err, _) = supervise(request, command.wall, command.output_limit, interrupted)?;
    Ok((code, out, err))
}
