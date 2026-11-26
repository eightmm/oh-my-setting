//! A single-threaded reactor is the sole reaper of every child it launches.
//!
//! `poll` never waits for process exit. A deadline stops execution, not kernel
//! teardown. Pending jobs remain in this reactor; callers must keep driving it.
//! Drop makes a bounded cleanup attempt. Retain and drive pending jobs when
//! cleanup evidence is required, especially for uninterruptible kernel waits.
use crate::capture::{Capture, CaptureOptions, Captured};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::{self as os, Pid, Signal, WaitId, WaitIdOptions, WaitOptions};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::marker::PhantomData;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub(crate) const MAX_DROP_WAIT: Duration = Duration::from_secs(5);
const POST_EXIT_DRAIN: Duration = Duration::from_millis(250);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Only the unreaped direct child is signal authority. No containment claim.
    Leader,
    /// A POSIX group is not whole-tree containment; session escapes are unknown.
    ProcessGroup,
    /// Requires the exclusive, pre-spawn Linux subreaper reactor.
    WholeTree,
}

/// A parent's physical-call correlation, not admission or permission to spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalCall(String);
impl PhysicalCall {
    pub fn new(id: &str) -> Result<Self, String> {
        if id.is_empty()
            || id.len() > 160
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err("invalid physical call correlation".into());
        }
        Ok(Self(id.into()))
    }
    pub fn id(&self) -> &str {
        &self.0
    }
}

/// Sealed by the owning reactor after cleanup. Scope stays explicit: leader or
/// POSIX-group cleanup cannot establish whole-tree or native-fanout bounds.
#[derive(Clone, Debug)]
pub struct OwnedCleanup {
    call: PhysicalCall,
    pid: u32,
    scope: Scope,
}
impl OwnedCleanup {
    pub fn call(&self) -> &PhysicalCall {
        &self.call
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn scope(&self) -> Scope {
        self.scope
    }
    pub fn matches(&self, call: &PhysicalCall, scope: Scope) -> bool {
        self.call == *call && self.scope == scope
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Streams {
    Merged,
    Separate,
}
/// Explicit queue and work bounds for an incremental raw-byte transport.
/// Protocol framing, UTF-8 validation and outstanding RPC limits belong to its
/// consumer; no hidden protocol or observer queue exists in the reactor.
#[derive(Clone, Debug)]
pub struct DuplexOptions {
    pub max_frame_bytes: usize,
    pub max_input_bytes: usize,
    pub max_input_frames: usize,
    pub max_output_bytes: usize,
    pub max_output_events: usize,
    pub max_read_bytes_per_turn: usize,
    pub max_write_bytes_per_turn: usize,
}
impl DuplexOptions {
    fn validate(&self) -> crate::Result<()> {
        if [
            self.max_frame_bytes,
            self.max_input_bytes,
            self.max_input_frames,
            self.max_output_bytes,
            self.max_output_events,
            self.max_read_bytes_per_turn,
            self.max_write_bytes_per_turn,
        ]
        .contains(&0)
            || self.max_frame_bytes > self.max_input_bytes
        {
            return Err(
                "duplex requires positive bounds and frame bytes within input bytes".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}
/// Sequence is contiguous across both streams within this process-local
/// generation. Separate pipes provide no global child-write ordering guarantee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputEvent {
    pub generation: u64,
    pub sequence: u64,
    pub stream: OutputStream,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    UnknownJob,
    NotDuplex,
    InputClosed,
    FrameTooLarge,
    QueueFull,
}
impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownJob => "unknown owned process",
            Self::NotDuplex => "owned process is not duplex",
            Self::InputClosed => "owned duplex input is closed",
            Self::FrameTooLarge => "duplex input frame exceeds byte bound",
            Self::QueueFull => "duplex input queue is full",
        })
    }
}
impl std::error::Error for SendError {}
#[derive(Clone, Debug)]
pub struct DuplexStatus {
    pub generation: u64,
    /// Retained frame bytes, including the written prefix of a partial frame.
    pub input_queued_bytes: usize,
    pub input_queued_frames: usize,
    pub input_accepted_bytes: u64,
    pub input_written_bytes: u64,
    pub input_abandoned_bytes: u64,
    pub input_close_requested: bool,
    pub input_closed: bool,
    pub input_broken_pipe: bool,
    /// True only when every accepted input byte was written to the pipe.
    /// This is not an acknowledgement that the child processed those bytes.
    pub input_drained: bool,
    pub output_queued_bytes: usize,
    pub output_queued_events: usize,
    pub next_sequence: u64,
    /// Cleanup/capture cutoff closed a reader without observing real EOF.
    pub output_closed_early: bool,
}
#[derive(Clone, Debug)]
pub struct DuplexCompletion {
    pub status: DuplexStatus,
    /// Events not yet taken with receive, in sequence order. Captures are
    /// independent snapshots and must never be replayed as incremental events.
    pub output: VecDeque<OutputEvent>,
}
struct Duplex {
    options: DuplexOptions,
    generation: u64,
    input: VecDeque<Vec<u8>>,
    input_offset: usize,
    input_bytes: usize,
    accepted: u64,
    written: u64,
    abandoned: u64,
    close_requested: bool,
    closed: bool,
    broken_pipe: bool,
    output: VecDeque<OutputEvent>,
    output_bytes: usize,
    sequence: u64,
    output_closed_early: bool,
    next_stream: OutputStream,
}
static NEXT_DUPLEX_GENERATION: AtomicU64 = AtomicU64::new(1);
impl Duplex {
    fn new(options: DuplexOptions) -> crate::Result<Self> {
        let generation = NEXT_DUPLEX_GENERATION
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| "duplex generation exhausted")?;
        Ok(Self {
            options,
            generation,
            input: VecDeque::new(),
            input_offset: 0,
            input_bytes: 0,
            accepted: 0,
            written: 0,
            abandoned: 0,
            close_requested: false,
            closed: false,
            broken_pipe: false,
            output: VecDeque::new(),
            output_bytes: 0,
            sequence: 1,
            output_closed_early: false,
            next_stream: OutputStream::Stdout,
        })
    }
    fn status(&self) -> DuplexStatus {
        DuplexStatus {
            generation: self.generation,
            input_queued_bytes: self.input_bytes,
            input_queued_frames: self.input.len(),
            input_accepted_bytes: self.accepted,
            input_written_bytes: self.written,
            input_abandoned_bytes: self.abandoned,
            input_close_requested: self.close_requested,
            input_closed: self.closed,
            input_broken_pipe: self.broken_pipe,
            input_drained: self.input.is_empty() && self.abandoned == 0,
            output_queued_bytes: self.output_bytes,
            output_queued_events: self.output.len(),
            next_sequence: self.sequence,
            output_closed_early: self.output_closed_early,
        }
    }
    fn output_capacity(&self) -> usize {
        if self.output.len() >= self.options.max_output_events {
            0
        } else {
            self.options.max_output_bytes - self.output_bytes
        }
    }
    fn abandon_input(&mut self) {
        self.closed = true;
        self.abandoned = self
            .abandoned
            .saturating_add(self.input_bytes.saturating_sub(self.input_offset) as u64);
        self.input_bytes = 0;
        self.input_offset = 0;
        self.input.clear();
    }
}
#[derive(Clone, Debug)]
pub struct Request {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<Vec<u8>>,
    /// Opt-in raw duplex; mutually exclusive with fixed stdin and merged streams.
    pub duplex: Option<DuplexOptions>,
    pub clear_environment: bool,
    pub environment: Vec<(OsString, OsString)>,
    pub remove_environment: Vec<OsString>,
    pub streams: Streams,
    pub capture: CaptureOptions,
    /// Separate stderr defaults to the byte policy without the stdout line cutoff.
    pub stderr_capture: Option<CaptureOptions>,
    pub deadline: Option<Instant>,
    pub scope: Scope,
    pub terminate_grace: Duration,
    pub physical_call: Option<PhysicalCall>,
}
impl Request {
    pub fn new(argv: Vec<OsString>) -> Self {
        Self {
            argv,
            cwd: None,
            stdin: None,
            duplex: None,
            clear_environment: false,
            environment: vec![],
            remove_environment: vec![],
            streams: Streams::Merged,
            capture: CaptureOptions::default(),
            stderr_capture: None,
            deadline: None,
            scope: Scope::ProcessGroup,
            terminate_grace: Duration::from_secs(2),
            physical_call: None,
        }
    }
}
/// Start failures distinguish refusal/setup from a payload exec failure.
#[derive(Debug)]
pub enum StartError {
    InvalidRequest(String),
    CapabilityUnavailable(&'static str),
    DeadlineExpired,
    Setup(String),
    Launch(std::io::Error),
}
impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(s) | Self::Setup(s) => f.write_str(s),
            Self::CapabilityUnavailable(s) => f.write_str(s),
            Self::DeadlineExpired => f.write_str("execution deadline elapsed before launch"),
            Self::Launch(e) => write!(f, "launch failed: {e}"),
        }
    }
}
impl std::error::Error for StartError {}
impl From<String> for StartError {
    fn from(value: String) -> Self {
        Self::Setup(value)
    }
}
impl From<&str> for StartError {
    fn from(value: &str) -> Self {
        Self::InvalidRequest(value.into())
    }
}
impl From<StartError> for String {
    fn from(value: StartError) -> Self {
        value.to_string()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct JobId(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Deadline,
    Cancelled,
    Signal(i32),
    OutputLimit,
    ConsumerCutoff,
    SupervisorError,
}
#[derive(Clone, Debug)]
pub struct SignalRecord {
    pub number: i32,
    pub pid: u32,
    pub group: bool,
    pub elapsed: Duration,
    /// ESRCH is a successful no-target observation, never proof of tree death.
    pub delivered: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}
impl Exit {
    pub fn shell_code(self) -> i32 {
        self.code.unwrap_or_else(|| 128 + self.signal.unwrap_or(0))
    }
}
#[derive(Clone, Debug)]
pub struct Completion {
    pub pid: u32,
    pub exit: Option<Exit>,
    pub stop: Option<StopReason>,
    pub stdout: Captured,
    /// Empty for Merged; merged ordering is the kernel's single-pipe order.
    pub stderr: Option<Captured>,
    pub duration: Duration,
    pub leader_reaped: bool,
    pub all_reaped: bool,
    pub group_quiescent: Option<bool>,
    pub scope: Scope,
    pub signals: Vec<SignalRecord>,
    pub supervision_error: Option<String>,
    pub duplex: Option<DuplexCompletion>,
    owned_cleanup: Option<OwnedCleanup>,
}
impl Completion {
    /// Uses sealed observations, unaffected by edits to public diagnostic fields.
    pub fn owned_cleanup(&self) -> Option<&OwnedCleanup> {
        self.owned_cleanup.as_ref()
    }
    pub fn cleanup_complete(&self) -> bool {
        self.leader_reaped
            && self.supervision_error.is_none()
            && match self.scope {
                Scope::Leader => true,
                Scope::ProcessGroup => self.group_quiescent == Some(true),
                Scope::WholeTree => self.all_reaped,
            }
    }
    pub fn output_complete(&self) -> bool {
        self.stdout.eof && self.stderr.as_ref().is_none_or(|s| s.eof)
    }
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub pid: u32,
    pub exit: Option<Exit>,
    pub stop: Option<StopReason>,
    pub stdout: Captured,
    pub stderr: Option<Captured>,
    pub elapsed: Duration,
    pub supervision_error: Option<String>,
    pub duplex: Option<DuplexStatus>,
}
#[derive(Debug)]
pub enum Progress {
    Pending(Snapshot),
    Complete(Completion),
}
struct Job {
    _child: Child,
    pid: Pid,
    #[cfg(target_os = "linux")]
    pidfd: Option<OwnedFd>,
    request: Request,
    out: Option<OwnedFd>,
    err: Option<OwnedFd>,
    input: Option<OwnedFd>,
    input_offset: usize,
    duplex: Option<Duplex>,
    stdout: Capture,
    stderr: Option<Capture>,
    started: Instant,
    exit: Option<Exit>,
    exited_at: Option<Instant>,
    stop: Option<StopReason>,
    stopping: Option<Instant>,
    kill_sent: bool,
    kill_sent_at: Option<Instant>,
    reaped: bool,
    all_reaped: bool,
    children_exhausted: bool,
    ownership_lost: bool,
    #[cfg(target_os = "linux")]
    adopted_signals: BTreeMap<i32, bool>,
    group_quiescent: Option<bool>,
    #[cfg(target_os = "macos")]
    group_probe: Option<GroupProbe>,
    #[cfg(target_os = "macos")]
    group_probe_after: Instant,
    signals: Vec<SignalRecord>,
    error: Option<String>,
    finished: bool,
}
#[cfg(target_os = "macos")]
struct GroupProbe {
    reactor: Reactor,
    id: JobId,
}

#[cfg(not(target_os = "macos"))]
fn pipe_cloexec() -> std::result::Result<(OwnedFd, OwnedFd), String> {
    rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn pipe_cloexec() -> std::result::Result<(OwnedFd, OwnedFd), String> {
    let (r, w) = rustix::pipe::pipe().map_err(|e| e.to_string())?;
    rustix::io::fcntl_setfd(&r, rustix::io::FdFlags::CLOEXEC).map_err(|e| e.to_string())?;
    rustix::io::fcntl_setfd(&w, rustix::io::FdFlags::CLOEXEC).map_err(|e| e.to_string())?;
    Ok((r, w))
}

/// Ordinary reactors wait only their exact child PIDs; they never use wait(-1).
/// Exclusive reactors are for dedicated single-purpose processes, not libraries
/// embedded alongside other child-spawning code. They allow exactly one tree.
pub struct Reactor {
    jobs: BTreeMap<JobId, Job>,
    next: u64,
    dedicated: bool,
    _single_thread: PhantomData<Rc<()>>,
}
impl Default for Reactor {
    fn default() -> Self {
        Self::new()
    }
}
impl Reactor {
    pub fn new() -> Self {
        Self {
            jobs: BTreeMap::new(),
            next: 1,
            dedicated: false,
            _single_thread: PhantomData,
        }
    }
    /// Caller must own ALL subsequent child creation and SIGCHLD disposition in
    /// this process. Initialization refuses existing children/threads/subreapers.
    /// No library constructor changes process-global subreaper state implicitly.
    pub fn dedicated() -> crate::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let tasks = std::fs::read_dir("/proc/self/task")
                .map_err(|e| e.to_string())?
                .count();
            if tasks != 1
                || !direct_children()?.is_empty()
                || os::child_subreaper().map_err(|e| e.to_string())?.is_some()
            {
                return Err("exclusive Linux child ownership unavailable".into());
            }
            os::set_child_subreaper(Some(Pid::INIT)).map_err(|e| e.to_string())?;
            let mut reactor = Self::new();
            reactor.dedicated = true;
            Ok(reactor)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err("native whole-tree child ownership unavailable on this platform".into())
        }
    }
    pub fn start(&mut self, request: Request) -> std::result::Result<JobId, StartError> {
        self.start_with_stdio(request, false)
    }
    pub(crate) fn start_detached(
        &mut self,
        request: Request,
    ) -> std::result::Result<JobId, StartError> {
        self.start_with_stdio(request, true)
    }
    fn start_with_stdio(
        &mut self,
        request: Request,
        detached: bool,
    ) -> std::result::Result<JobId, StartError> {
        if request.argv.first().is_none_or(|s| s.is_empty()) {
            return Err("execution requires a command".into());
        }
        if request.scope == Scope::WholeTree && !self.dedicated {
            return Err(StartError::CapabilityUnavailable(
                "whole-tree execution requires a dedicated Linux subreaper",
            ));
        }
        if self.dedicated && request.scope != Scope::WholeTree {
            return Err("dedicated reactor requires whole-tree requests".into());
        }
        if self.dedicated && !self.jobs.is_empty() {
            return Err("dedicated reactor already owns a tree".into());
        }
        if let Some(options) = &request.duplex {
            options.validate().map_err(StartError::InvalidRequest)?;
            if request.stdin.is_some() || request.streams != Streams::Separate || detached {
                return Err(
                    "duplex requires separate streams, no fixed stdin and an attached child".into(),
                );
            }
        }
        // Allocate the generation before spawn, preserving ownership on every
        // fallible setup path.
        let duplex = request.duplex.clone().map(Duplex::new).transpose()?;
        let next = self
            .next
            .checked_add(1)
            .ok_or("supervisor job id exhausted")?;
        let started = Instant::now();
        if request.deadline.is_some_and(|deadline| started >= deadline) {
            return Err(StartError::DeadlineExpired);
        }
        started
            .checked_add(request.terminate_grace)
            .ok_or("termination grace is too large")?;
        let (out_read, out_write) = pipe_cloexec()?;
        nonblocking(&out_read)?;
        let (err_read, err_write) = if request.streams == Streams::Separate {
            let (r, w) = pipe_cloexec()?;
            nonblocking(&r)?;
            (Some(r), w)
        } else {
            (
                None,
                rustix::io::fcntl_dupfd_cloexec(&out_write, 0).map_err(|e| e.to_string())?,
            )
        };
        // Prepare every fallible descriptor operation before spawning. A later
        // error must never lose an already-created child's ownership handle.
        let (input, child_input) = if request.stdin.is_some() || request.duplex.is_some() {
            let (r, w) = pipe_cloexec()?;
            nonblocking(&w)?;
            (Some(w), Stdio::from(r))
        } else {
            (None, Stdio::null())
        };
        let mut command = Command::new(&request.argv[0]);
        command
            .args(&request.argv[1..])
            .stdin(if detached { Stdio::null() } else { child_input })
            .stdout(if detached {
                Stdio::null()
            } else {
                Stdio::from(out_write)
            })
            .stderr(if detached {
                Stdio::null()
            } else {
                Stdio::from(err_write)
            });
        if request.scope != Scope::Leader {
            command.process_group(0);
        }
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        if request.clear_environment {
            command.env_clear();
        }
        command.envs(request.environment.iter().cloned());
        for name in &request.remove_environment {
            command.env_remove(name);
        }
        let child = command.spawn().map_err(StartError::Launch)?;
        // Child IDs returned by std::process are positive platform pid_t values.
        let pid = Pid::from_raw(child.id() as i32).expect("spawn returned invalid PID");
        #[cfg(target_os = "linux")]
        let pidfd = os::pidfd_open(pid, os::PidfdFlags::NONBLOCK).ok();
        let stderr = err_read.as_ref().filter(|_| !detached).map(|_| {
            let policy = request.stderr_capture.clone().unwrap_or_else(|| {
                let mut policy = request.capture.clone();
                policy.lines = None;
                policy
            });
            Capture::new(policy)
        });
        let mut stdout = Capture::new(request.capture.clone());
        stdout.eof = detached;
        let job = Job {
            _child: child,
            pid,
            #[cfg(target_os = "linux")]
            pidfd,
            stdout,
            stderr,
            out: if detached { None } else { Some(out_read) },
            err: if detached { None } else { err_read },
            input: if detached { None } else { input },
            input_offset: 0,
            duplex,
            request,
            started,
            exit: None,
            exited_at: None,
            stop: None,
            stopping: None,
            kill_sent: false,
            kill_sent_at: None,
            reaped: false,
            all_reaped: false,
            children_exhausted: false,
            ownership_lost: false,
            #[cfg(target_os = "linux")]
            adopted_signals: BTreeMap::new(),
            group_quiescent: None,
            #[cfg(target_os = "macos")]
            group_probe: None,
            #[cfg(target_os = "macos")]
            group_probe_after: started,
            signals: vec![],
            error: None,
            finished: false,
        };
        let id = JobId(self.next);
        self.next = next;
        self.jobs.insert(id, job);
        Ok(id)
    }
    /// Accept one complete consumer frame without blocking. Success means
    /// queue admission only; rejection does not copy or partially enqueue it.
    pub fn send(&mut self, id: JobId, frame: &[u8]) -> Result<(), SendError> {
        let job = self.jobs.get_mut(&id).ok_or(SendError::UnknownJob)?;
        let duplex = job.duplex.as_mut().ok_or(SendError::NotDuplex)?;
        if duplex.close_requested || duplex.closed || job.stopping.is_some() {
            return Err(SendError::InputClosed);
        }
        if frame.len() > duplex.options.max_frame_bytes {
            return Err(SendError::FrameTooLarge);
        }
        if duplex.input.len() >= duplex.options.max_input_frames
            || frame.len() > duplex.options.max_input_bytes - duplex.input_bytes
        {
            return Err(SendError::QueueFull);
        }
        duplex.input.push_back(frame.to_vec());
        duplex.input_bytes += frame.len();
        duplex.accepted = duplex.accepted.saturating_add(frame.len() as u64);
        Ok(())
    }
    /// Idempotent half-close. Previously accepted frames drain before EOF;
    /// deadline/cancellation may still abandon them with explicit status.
    pub fn close_input(&mut self, id: JobId) -> Result<(), SendError> {
        let job = self.jobs.get_mut(&id).ok_or(SendError::UnknownJob)?;
        let duplex = job.duplex.as_mut().ok_or(SendError::NotDuplex)?;
        duplex.close_requested = true;
        if duplex.input.is_empty() {
            job.input.take();
            duplex.closed = true;
        }
        Ok(())
    }
    /// Takes one incremental output chunk. Call before poll and consume the
    /// terminal queue from Completion after poll removes the finished job.
    pub fn receive(&mut self, id: JobId) -> crate::Result<Option<OutputEvent>> {
        let duplex = self
            .jobs
            .get_mut(&id)
            .ok_or("unknown owned process")?
            .duplex
            .as_mut()
            .ok_or("owned process is not duplex")?;
        let event = duplex.output.pop_front();
        if let Some(event) = &event {
            duplex.output_bytes -= event.bytes.len();
        }
        Ok(event)
    }
    /// Numeric provenance is retained; callers supply the real received signal.
    pub fn stop(&mut self, id: JobId, reason: StopReason) -> crate::Result<()> {
        let job = self.jobs.get_mut(&id).ok_or("unknown owned process")?;
        if let StopReason::Signal(number) = reason
            && Signal::from_named_raw(number).is_none()
        {
            return Err("unsupported signal number".into());
        }
        job.begin_stop(reason);
        Ok(())
    }
    pub fn snapshot(&self, id: JobId) -> crate::Result<Snapshot> {
        Ok(self
            .jobs
            .get(&id)
            .ok_or("unknown owned process")?
            .snapshot())
    }
    /// A zero budget performs one nonblocking turn. Maximum wait is bounded by
    /// the budget, the requested execution deadline and the escalation instant.
    /// Completed entries are removed only after waitid has actually reaped them.
    pub fn poll(&mut self, id: JobId, budget: Duration) -> crate::Result<Progress> {
        let job = self.jobs.get_mut(&id).ok_or("unknown owned process")?;
        job.turn(self.dedicated);
        if !job.finished && !budget.is_zero() {
            let now = Instant::now();
            // Keep the 50 ms floor even when `fds` is empty (pidfd unavailable
            // with output closed, terminate grace cleanup, or full duplex
            // output), bounded only by caller budget, execution deadline, and
            // signal escalation instants rather than dropping to a 2 ms spin.
            let mut wait = budget.min(POLL_INTERVAL);
            if job.stop.is_none()
                && !job.exited_with_eof()
                && let Some(deadline) = job.request.deadline
            {
                wait = wait.min(deadline.saturating_duration_since(now));
            }
            if let Some(stopping) = job.stopping
                && !job.kill_sent
                && job.group_quiescent != Some(true)
            {
                wait = wait
                    .min((stopping + job.request.terminate_grace).saturating_duration_since(now));
            }
            #[cfg(target_os = "macos")]
            if job.group_probe.is_some() {
                wait = wait.min(Duration::from_millis(10));
            }
            let mut fds = Vec::new();
            let output_ready = job.duplex.as_ref().is_none_or(|d| d.output_capacity() > 0);
            if output_ready && let Some(fd) = &job.out {
                fds.push(PollFd::new(fd, PollFlags::IN));
            }
            if output_ready && let Some(fd) = &job.err {
                fds.push(PollFd::new(fd, PollFlags::IN));
            }
            if job.duplex.as_ref().is_none_or(|d| !d.input.is_empty())
                && let Some(fd) = &job.input
            {
                fds.push(PollFd::new(fd, PollFlags::OUT));
            }
            #[cfg(target_os = "linux")]
            if job.exit.is_none()
                && let Some(fd) = &job.pidfd
            {
                fds.push(PollFd::new(fd, PollFlags::IN));
            }
            let timeout = Timespec {
                tv_sec: wait.as_secs() as _,
                tv_nsec: wait.subsec_nanos() as _,
            };
            if let Err(e) = poll(&mut fds, Some(&timeout))
                && e != Errno::INTR
            {
                job.fail(format!("process poll failed: {e}"));
            }
            // Read even on HUP/ERR, until read==0. !POLLIN is never EOF.
            job.turn(self.dedicated);
        }
        if job.finished {
            let job = self.jobs.remove(&id).expect("owned job disappeared");
            Ok(Progress::Complete(job.completion()))
        } else {
            Ok(Progress::Pending(job.snapshot()))
        }
    }
    pub(crate) fn stdout_prefix(&self, id: JobId) -> crate::Result<&[u8]> {
        Ok(self
            .jobs
            .get(&id)
            .ok_or("unknown owned process")?
            .stdout
            .prefix())
    }
    pub fn pending(&self) -> usize {
        self.jobs.len()
    }
    pub(crate) fn release_detached(&mut self, ids: &[JobId]) {
        for id in ids {
            self.jobs.remove(id);
        }
    }
}
impl Drop for Reactor {
    fn drop(&mut self) {
        for job in self.jobs.values_mut() {
            job.begin_stop(StopReason::SupervisorError);
        }
        let until = Instant::now() + MAX_DROP_WAIT;
        while !self.jobs.is_empty() && Instant::now() < until {
            let ids: Vec<_> = self.jobs.keys().copied().collect();
            for id in ids {
                let _ = self.poll(id, Duration::ZERO);
            }
            if !self.jobs.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // A pending uninterruptible child cannot be synchronously reaped with
        // a bounded destructor. Deliver the final signal while still owning it;
        // std::process::Child drop performs no blocking wait.
        for job in self.jobs.values_mut() {
            if !job.kill_sent {
                job.signal(Signal::KILL);
                job.kill_sent = true;
            }
        }
        #[cfg(target_os = "linux")]
        if self.dedicated {
            let _ = os::set_child_subreaper(None);
        }
    }
}
fn nonblocking(fd: &OwnedFd) -> crate::Result<()> {
    let flags = fcntl_getfl(fd).map_err(|e| e.to_string())?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(|e| e.to_string())
}
impl Job {
    fn exited_with_eof(&self) -> bool {
        self.exit.is_some() && self.stdout.eof && self.stderr.as_ref().is_none_or(|s| s.eof)
    }
    fn observe_group(&mut self) -> Option<bool> {
        #[cfg(not(target_os = "macos"))]
        {
            group_quiescent(self.pid)
        }
        #[cfg(target_os = "macos")]
        {
            // On XNU (`bsd/kern/kern_sig.c` `killpg1`), `kill(-pgid, 0)` returns `ESRCH`
            // once the group no longer exists, and `EPERM` both when an existing group
            // contains only unreaped zombies (`SZOMB`, `nfound == 0`) and when a member is
            // un-signalable. Only `ESRCH` short-circuits; `Ok` and `EPERM`/`EACCES` defer
            // to `/bin/ps` to inspect non-zombie rows.
            match os::test_kill_process_group(self.pid) {
                Err(Errno::SRCH) => {
                    self.group_probe = None;
                    return Some(true);
                }
                Ok(()) | Err(Errno::PERM | Errno::ACCESS) => {}
                Err(_) => return None,
            }
            // The helper stays owned across turns; never wait for ps from a
            // payload poll turn or drop a still-running helper on a timeout.
            if let Some(probe) = &mut self.group_probe {
                return match probe.reactor.poll(probe.id, Duration::ZERO) {
                    Ok(Progress::Complete(result)) => {
                        self.group_probe = None;
                        self.group_probe_after = Instant::now() + Duration::from_millis(100);
                        mac_group_result(self.pid, self.exit.is_some(), &result)
                    }
                    Ok(Progress::Pending(_)) => Some(false),
                    Err(_) => None,
                };
            }
            if self.group_quiescent == Some(true) || Instant::now() < self.group_probe_after {
                return self.group_quiescent;
            }
            self.group_probe_after = Instant::now() + Duration::from_millis(100);
            let mut reactor = Reactor::new();
            let id = reactor.start(mac_group_request()).ok()?;
            self.group_probe = Some(GroupProbe { reactor, id });
            Some(false)
        }
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            pid: self.pid.as_raw_nonzero().get() as u32,
            exit: self.exit,
            stop: self.stop,
            stdout: self.stdout.snapshot(),
            stderr: self.stderr.as_ref().map(Capture::snapshot),
            elapsed: self.started.elapsed(),
            supervision_error: self.error.clone(),
            duplex: self.duplex.as_ref().map(Duplex::status),
        }
    }
    fn completion(self) -> Completion {
        let physical_call = self.request.physical_call;
        let mut completion = Completion {
            pid: self.pid.as_raw_nonzero().get() as u32,
            exit: self.exit,
            stop: self.stop,
            stdout: self.stdout.snapshot(),
            stderr: self.stderr.as_ref().map(Capture::snapshot),
            duration: self.started.elapsed(),
            leader_reaped: self.reaped && !self.ownership_lost,
            all_reaped: self.all_reaped,
            group_quiescent: self.group_quiescent,
            scope: self.request.scope,
            signals: self.signals,
            supervision_error: self.error,
            duplex: self.duplex.map(|d| DuplexCompletion {
                status: d.status(),
                output: d.output,
            }),
            owned_cleanup: None,
        };
        if completion.cleanup_complete()
            && let Some(call) = physical_call
        {
            completion.owned_cleanup = Some(OwnedCleanup {
                call,
                pid: completion.pid,
                scope: completion.scope,
            });
        }
        completion
    }
    fn fail(&mut self, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
        }
        self.begin_stop(StopReason::SupervisorError);
    }
    fn begin_stop(&mut self, reason: StopReason) {
        if self.finished || self.stop.is_some() {
            return;
        }
        self.stop = Some(reason);
        if self.stopping.is_some() {
            let signal = match reason {
                StopReason::Signal(number) => {
                    Signal::from_named_raw(number).unwrap_or(Signal::TERM)
                }
                _ => Signal::TERM,
            };
            self.signal(signal);
        }
        self.begin_cleanup();
    }
    fn begin_cleanup(&mut self) {
        if self.stopping.is_some() {
            return;
        }
        self.stopping = Some(Instant::now());
        self.input.take();
        if let Some(duplex) = &mut self.duplex {
            duplex.abandon_input();
        }
        if self.request.scope == Scope::Leader && self.stop.is_none() && self.exit.is_some() {
            return;
        }
        let signal = match self.stop {
            Some(StopReason::Signal(number)) => {
                Signal::from_named_raw(number).unwrap_or(Signal::TERM)
            }
            _ => Signal::TERM,
        };
        self.signal(signal);
    }
    fn signal(&mut self, signal: Signal) {
        // The direct child is never reaped until every group signal decision
        // is complete. Sole-reaper ownership prevents numeric PID/PGID reuse.
        if self.reaped {
            return;
        }
        let group = self.request.scope != Scope::Leader;
        let result = if group {
            os::kill_process_group(self.pid, signal)
        } else {
            os::kill_process(self.pid, signal)
        };
        self.signals.push(SignalRecord {
            number: signal.as_raw(),
            pid: self.pid.as_raw_nonzero().get() as u32,
            group,
            elapsed: self.started.elapsed(),
            delivered: result.is_ok(),
        });
        let ignorable = matches!(result, Ok(()) | Err(Errno::SRCH))
            || (cfg!(target_os = "macos") && group && matches!(result, Err(Errno::PERM)));
        if let Err(e) = result
            && !ignorable
            && self.error.is_none()
        {
            self.error = Some(format!("owned process signal failed: {e}"));
        }
    }
    fn observe_exit(&mut self) {
        if self.reaped || self.exit.is_some() {
            return;
        }
        match os::waitid(
            WaitId::Pid(self.pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
        ) {
            Ok(Some(status)) => {
                if status.exited() || status.killed() || status.dumped() {
                    self.exit = Some(Exit {
                        code: status.exit_status(),
                        signal: status.terminating_signal(),
                    });
                    self.exited_at = Some(Instant::now());
                }
            }
            Ok(None) => {}
            Err(Errno::INTR) => {}
            Err(Errno::CHILD) => {
                // Another reaper or SIGCHLD auto-reap violated ownership. Never
                // signal this numeric identity again, even to clean up.
                self.reaped = true;
                self.ownership_lost = true;
                self.fail("owned child wait status lost; numeric signal authority revoked".into());
            }
            Err(e) => self.fail(format!("owned process observation failed: {e}")),
        }
    }
    fn turn(&mut self, dedicated: bool) {
        if self.finished {
            return;
        }
        self.observe_exit();
        let stderr_first = self
            .duplex
            .as_ref()
            .is_some_and(|d| d.next_stream == OutputStream::Stderr);
        if stderr_first {
            self.drain_output(OutputStream::Stderr);
            self.drain_output(OutputStream::Stdout);
        } else {
            self.drain_output(OutputStream::Stdout);
            self.drain_output(OutputStream::Stderr);
        }
        if self.exit.is_none() && self.stdout.eof && self.stderr.as_ref().is_none_or(|s| s.eof) {
            self.observe_exit();
        }
        self.write_input();
        // Preserve an already-observed child outcome and actual EOF even when
        // the caller resumes polling after the execution deadline has passed.
        if self.stop.is_none() {
            if self.exited_with_eof() {
                self.begin_cleanup();
            } else if self.stdout.should_stop()
                || self.stderr.as_ref().is_some_and(Capture::should_stop)
            {
                let cutoff = self.stdout.snapshot().cutoff
                    || self.stderr.as_ref().is_some_and(|s| s.snapshot().cutoff);
                self.begin_stop(if cutoff {
                    StopReason::ConsumerCutoff
                } else {
                    StopReason::OutputLimit
                });
            } else if self.request.deadline.is_some_and(|d| Instant::now() >= d) {
                self.begin_stop(StopReason::Deadline);
            } else if self.exit.is_some()
                && self
                    .exited_at
                    .is_some_and(|t| t.elapsed() >= POST_EXIT_DRAIN)
            {
                self.begin_cleanup();
            }
        }
        if self.stopping.is_none() {
            return;
        }
        self.group_quiescent = if self.request.scope == Scope::Leader {
            Some(self.exit.is_some())
        } else {
            self.observe_group()
        };
        if !self.kill_sent
            && self.group_quiescent != Some(true)
            && self
                .stopping
                .is_some_and(|s| s.elapsed() >= self.request.terminate_grace)
        {
            self.signal(Signal::KILL);
            self.kill_sent = true;
            self.kill_sent_at = Some(Instant::now());
        }
        self.observe_exit();
        // A kill is asynchronous. Only observed exit allows a nonblocking reap.
        // Final group signaling must happen BEFORE consuming the leader token.
        let post_kill_settled = self
            .kill_sent_at
            .is_some_and(|t| t.elapsed() >= POST_EXIT_DRAIN);
        let signals_done = self.group_quiescent == Some(true)
            || self.kill_sent && (self.group_quiescent.is_none() || post_kill_settled)
            || self.request.scope == Scope::Leader;
        if !self.reaped && self.exit.is_some() && signals_done {
            match os::waitpid(Some(self.pid), WaitOptions::NOHANG) {
                Ok(Some(_)) => self.reaped = true,
                Ok(None) | Err(Errno::INTR) => {}
                Err(Errno::CHILD) => {
                    self.reaped = true;
                    self.ownership_lost = true;
                    self.fail("owned child was reaped elsewhere".into());
                }
                Err(e) => self.fail(format!("owned child reap failed: {e}")),
            }
        }
        if dedicated && self.reaped {
            self.reap_adopted();
        }
        // Escaped descendants can keep a pipe open beyond group teardown.
        // Bound drainage without claiming read-zero or complete output.
        let drain_expired = self.exited_at.is_some_and(|t| {
            t.elapsed() >= POST_EXIT_DRAIN + self.request.terminate_grace + POST_EXIT_DRAIN
        });
        if (self.stop.is_some() || drain_expired)
            && (self.kill_sent || self.group_quiescent == Some(true))
        {
            if let Some(duplex) = &mut self.duplex {
                duplex.output_closed_early |= self.out.is_some() || self.err.is_some();
            }
            self.out.take();
            self.err.take();
        }
        if self.reaped && self.out.is_none() && self.err.is_none() {
            if self.request.scope == Scope::WholeTree && !self.children_exhausted {
                return;
            }
            if self.request.scope == Scope::ProcessGroup {
                self.group_quiescent = self.observe_group();
                if self.group_quiescent == Some(false) {
                    if !self.kill_sent_at.is_some_and(|t| {
                        t.elapsed() >= POST_EXIT_DRAIN + self.request.terminate_grace
                    }) {
                        return;
                    }
                    self.error.get_or_insert_with(|| {
                        "process group remained active after SIGKILL".into()
                    });
                }
                if self.group_quiescent.is_none() && self.error.is_none() {
                    self.error =
                        Some("process group cleanup could not be observed on this platform".into());
                }
            }
            self.finished = true;
        }
    }
    fn drain_output(&mut self, stream: OutputStream) {
        let (fd, capture) = match stream {
            OutputStream::Stdout => (&mut self.out, &mut self.stdout),
            OutputStream::Stderr => {
                let Some(capture) = self.stderr.as_mut() else {
                    return;
                };
                (&mut self.err, capture)
            }
        };
        if let Err(error) = drain(fd, capture, self.duplex.as_mut(), stream) {
            if let Some(duplex) = &mut self.duplex {
                duplex.output_closed_early |= fd.is_some();
            }
            fd.take();
            self.fail(error);
        }
    }
    fn write_input(&mut self) {
        if self.duplex.is_some() {
            self.write_duplex_input();
            return;
        }
        let Some(fd) = &self.input else {
            return;
        };
        let data = self.request.stdin.as_deref().unwrap_or_default();
        if self.input_offset == data.len() {
            self.input.take();
            return;
        }
        let end = data.len().min(self.input_offset.saturating_add(65536));
        match rustix::io::write(fd, &data[self.input_offset..end]) {
            Ok(0) => {
                self.input.take();
                self.fail("owned stdin made no progress".into());
            }
            Ok(n) => {
                self.input_offset += n;
                if self.input_offset == data.len() {
                    self.input.take();
                }
            }
            Err(Errno::AGAIN | Errno::INTR) => {}
            // A command may legitimately consume only a prefix of its input.
            Err(Errno::PIPE) => {
                self.input.take();
            }
            Err(e) => {
                self.input.take();
                self.fail(format!("owned stdin failed: {e}"));
            }
        }
    }
    fn write_duplex_input(&mut self) {
        let Some(fd) = &self.input else {
            return;
        };
        let duplex = self.duplex.as_mut().expect("duplex writer");
        let mut remaining = duplex.options.max_write_bytes_per_turn;
        // Bound zero-byte frames, EINTR and syscall work as well as byte work.
        for _ in 0..4 {
            let Some(frame) = duplex.input.front() else {
                break;
            };
            if duplex.input_offset == frame.len() {
                duplex.input_bytes -= frame.len();
                duplex.input.pop_front();
                duplex.input_offset = 0;
                continue;
            }
            if remaining == 0 {
                break;
            }
            let end = frame
                .len()
                .min(duplex.input_offset.saturating_add(remaining));
            match rustix::io::write(fd, &frame[duplex.input_offset..end]) {
                Ok(0) => {
                    self.input.take();
                    duplex.abandon_input();
                    self.fail("owned duplex stdin made no progress".into());
                    return;
                }
                Ok(n) => {
                    duplex.input_offset += n;
                    duplex.written = duplex.written.saturating_add(n as u64);
                    remaining -= n;
                    if duplex.input_offset == frame.len() {
                        duplex.input_bytes -= frame.len();
                        duplex.input.pop_front();
                        duplex.input_offset = 0;
                    }
                }
                Err(Errno::AGAIN) => break,
                Err(Errno::INTR) => continue,
                Err(Errno::PIPE) => {
                    self.input.take();
                    duplex.broken_pipe = true;
                    duplex.abandon_input();
                    return;
                }
                Err(e) => {
                    self.input.take();
                    duplex.abandon_input();
                    self.fail(format!("owned duplex stdin failed: {e}"));
                    return;
                }
            }
        }
        if duplex.input.is_empty() && duplex.close_requested {
            self.input.take();
            duplex.closed = true;
        }
    }
    fn reap_adopted(&mut self) {
        #[cfg(target_os = "linux")]
        {
            // Only dedicated() reaches wait(All); it owns ALL subsequent child
            // creation. ECHILD, not an empty proc scan or queued kill, proves
            // that no adopted children remain.
            match os::waitid(
                WaitId::All,
                WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
            ) {
                Err(Errno::CHILD) => {
                    self.children_exhausted = true;
                    self.all_reaped = !self.ownership_lost;
                    return;
                }
                Err(Errno::INTR) => return,
                Err(e) => {
                    self.fail(format!("adopted child observation failed: {e}"));
                    return;
                }
                Ok(_) => {}
            }
            if self.ownership_lost {
                return;
            }
            let children = match direct_children() {
                Ok(pids) => pids,
                Err(e) => {
                    self.fail(e);
                    return;
                }
            };
            let force = self
                .stopping
                .is_some_and(|s| s.elapsed() >= self.request.terminate_grace);
            let signal = if force {
                Signal::KILL
            } else {
                match self.stop {
                    Some(StopReason::Signal(n)) => {
                        Signal::from_named_raw(n).unwrap_or(Signal::TERM)
                    }
                    _ => Signal::TERM,
                }
            };
            for pid in children {
                match os::waitid(
                    WaitId::Pid(pid),
                    WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
                ) {
                    Ok(Some(_)) => {
                        // Every forwarding decision for this exited child is
                        // complete. Consume only this exact reserved identity.
                        match os::waitid(
                            WaitId::Pid(pid),
                            WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
                        ) {
                            Ok(Some(_)) => {
                                self.adopted_signals.remove(&pid.as_raw_nonzero().get());
                            }
                            Ok(None) | Err(Errno::INTR) => {}
                            Err(Errno::CHILD) => {
                                self.ownership_lost = true;
                                self.fail("adopted child wait status lost".into());
                                return;
                            }
                            Err(e) => self.fail(format!("adopted child reap failed: {e}")),
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(Errno::INTR) => continue,
                    Err(Errno::CHILD) => {
                        self.ownership_lost = true;
                        self.fail("adopted child ownership lost".into());
                        return;
                    }
                    Err(e) => {
                        self.fail(format!("adopted child observation failed: {e}"));
                        continue;
                    }
                }
                let raw = pid.as_raw_nonzero().get();
                if self
                    .adopted_signals
                    .get(&raw)
                    .is_some_and(|sent_force| *sent_force || !force)
                {
                    continue;
                }
                // Sole-reaper ownership pins even a concurrently exiting child.
                let result = os::kill_process(pid, signal);
                self.adopted_signals.insert(raw, force);
                self.signals.push(SignalRecord {
                    number: signal.as_raw(),
                    pid: raw as u32,
                    group: false,
                    elapsed: self.started.elapsed(),
                    delivered: result.is_ok(),
                });
                if let Err(e) = result
                    && e != Errno::SRCH
                {
                    self.fail(format!("adopted child signal failed: {e}"));
                }
            }
        }
    }
}
fn drain(
    fd: &mut Option<OwnedFd>,
    capture: &mut Capture,
    mut duplex: Option<&mut Duplex>,
    stream: OutputStream,
) -> crate::Result<()> {
    if capture.should_stop() {
        if let Some(d) = &mut duplex {
            d.output_closed_early |= fd.is_some();
        }
        fd.take();
        return Ok(());
    }
    let Some(opened) = fd.as_ref() else {
        return Ok(());
    };
    let mut buf = [0u8; 65536];
    // Bound per-turn work so an unbounded writer cannot starve cancellation.
    let mut remaining = duplex
        .as_ref()
        .map_or(4 * 65536, |d| d.options.max_read_bytes_per_turn);
    for _ in 0..4 {
        let capacity = duplex.as_ref().map_or(buf.len(), |d| d.output_capacity());
        let size = buf.len().min(remaining).min(capacity);
        if size == 0 {
            break;
        }
        if duplex.as_ref().is_some_and(|d| d.sequence == u64::MAX) {
            return Err("duplex output sequence exhausted".into());
        }
        match rustix::io::read(opened, &mut buf[..size]) {
            Ok(0) => {
                capture.eof = true;
                fd.take();
                return Ok(());
            }
            Ok(n) => {
                remaining -= n;
                if let Some(d) = &mut duplex {
                    d.output.push_back(OutputEvent {
                        generation: d.generation,
                        sequence: d.sequence,
                        stream,
                        bytes: buf[..n].to_vec(),
                    });
                    d.sequence += 1;
                    d.output_bytes += n;
                    d.next_stream = match stream {
                        OutputStream::Stdout => OutputStream::Stderr,
                        OutputStream::Stderr => OutputStream::Stdout,
                    };
                }
                capture.push(&buf[..n]);
                if capture.should_stop() {
                    if let Some(d) = &mut duplex {
                        d.output_closed_early = true;
                    }
                    fd.take();
                    return Ok(());
                }
            }
            Err(Errno::AGAIN) => return Ok(()),
            Err(Errno::INTR) => continue,
            Err(e) => return Err(format!("owned output read failed: {e}")),
        }
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn direct_children() -> crate::Result<Vec<Pid>> {
    let path = format!("/proc/self/task/{}/children", std::process::id());
    std::fs::read_to_string(path)
        .map_err(|e| e.to_string())?
        .split_whitespace()
        .map(|s| {
            s.parse::<i32>()
                .ok()
                .and_then(Pid::from_raw)
                .filter(|p| *p != Pid::INIT)
                .ok_or_else(|| "invalid direct-child process observation".into())
        })
        .collect()
}
/// Exclude zombies while the owned leader remains pinned for group signaling.
pub(crate) fn group_quiescent(pid: Pid) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        match os::test_kill_process_group(pid) {
            Err(Errno::SRCH) => return Some(true),
            Err(Errno::PERM | Errno::ACCESS) => return Some(false),
            Err(_) => return None,
            Ok(()) => {}
        }
        let group = pid.as_raw_nonzero().get().to_string();
        let entries = std::fs::read_dir("/proc").ok()?;
        for entry in entries {
            let entry = entry.ok()?;
            if !entry
                .file_name()
                .as_encoded_bytes()
                .iter()
                .all(u8::is_ascii_digit)
            {
                continue;
            }
            let raw = match std::fs::read_to_string(entry.path().join("stat")) {
                Ok(raw) => raw,
                Err(e)
                    if e.kind() == std::io::ErrorKind::NotFound
                        || e.raw_os_error() == Some(Errno::SRCH.raw_os_error()) =>
                {
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Some(false),
                Err(_) => return None,
            };
            let fields: Vec<_> = raw.get(raw.rfind(')')? + 2..)?.split_whitespace().collect();
            if fields.len() < 3 {
                return None;
            }
            if fields[2] == group && !matches!(fields[0], "Z" | "X") {
                return Some(false);
            }
        }
        Some(true)
    }
    #[cfg(target_os = "macos")]
    {
        match os::test_kill_process_group(pid) {
            Err(Errno::SRCH) => return Some(true),
            Ok(()) | Err(Errno::PERM | Errno::ACCESS) => {}
            Err(_) => return None,
        }
        // Synchronous command-side observations share the bounded producer.
        // Job::observe_group instead retains it and advances without waiting.
        let mut reactor = Reactor::new();
        let id = reactor.start(mac_group_request()).ok()?;
        let until = Instant::now() + Duration::from_millis(2500);
        let result = loop {
            match reactor.poll(id, Duration::from_millis(10)).ok()? {
                Progress::Complete(result) => break result,
                Progress::Pending(_) if Instant::now() >= until => return None,
                Progress::Pending(_) => {}
            }
        };
        mac_group_result(pid, false, &result)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}
#[cfg(target_os = "macos")]
fn mac_group_request() -> Request {
    let mut request = Request::new(vec![
        "/bin/ps".into(),
        "-axo".into(),
        "pid=,pgid=,state=".into(),
    ]);
    request.scope = Scope::Leader;
    request.environment = vec![("LC_ALL".into(), "C".into())];
    request.deadline = Some(Instant::now() + Duration::from_millis(2000));
    request.terminate_grace = Duration::ZERO;
    request.capture.max_bytes = 8 * 1024 * 1024;
    request.capture.on_limit = crate::capture::LimitAction::Terminate;
    request
}
#[cfg(any(target_os = "macos", test))]
fn mac_group_result(pid: Pid, leader_exited: bool, result: &Completion) -> Option<bool> {
    if !result.cleanup_complete()
        || !result.output_complete()
        || result.stop.is_some()
        || result.exit.is_none_or(|exit| exit.shell_code() != 0)
    {
        return None;
    }
    let target = pid.as_raw_nonzero().get();
    for row in std::str::from_utf8(&result.stdout.bytes).ok()?.lines() {
        if row.trim().is_empty() {
            continue;
        }
        let mut fields = row.split_whitespace();
        let first = fields.next()?;
        if first.eq_ignore_ascii_case("PID")
            || first.eq_ignore_ascii_case("PGID")
            || first.eq_ignore_ascii_case("STAT")
            || first.eq_ignore_ascii_case("STATE")
        {
            continue;
        }
        let row_pid = first.parse::<i32>().ok()?;
        let group = fields.next()?.parse::<i32>().ok()?;
        let state = fields.next()?;
        if group != target {
            continue;
        }
        if fields.next().is_some() {
            return None;
        }
        if leader_exited && row_pid == target {
            continue;
        }
        if !state.starts_with(['Z', 'X']) {
            return Some(false);
        }
    }
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Captured;

    fn fake_ps_completion(stdout: &[u8], code: Option<i32>) -> Completion {
        Completion {
            pid: 9999,
            exit: code.map(|c| Exit {
                code: Some(c),
                signal: None,
            }),
            stop: None,
            stdout: Captured {
                bytes: stdout.to_vec(),
                observed_bytes: stdout.len() as u64,
                dropped_bytes: 0,
                sha256: String::new(),
                eof: true,
                limited: false,
                cutoff: false,
            },
            stderr: None,
            duration: Duration::from_millis(5),
            leader_reaped: true,
            all_reaped: false,
            group_quiescent: Some(true),
            scope: Scope::Leader,
            signals: vec![],
            supervision_error: None,
            duplex: None,
            owned_cleanup: None,
        }
    }

    #[test]
    fn mac_group_result_ignores_exited_leader_and_handles_descendants_and_errors() {
        let leader = Pid::from_raw(4242).unwrap();

        // 1) Exited leader rendered as E / UE / ? / Z+ must still count as quiescent when alone.
        for state in ["E", "UE", "?", "Z+"] {
            let rows = format!("  4242   4242 {state}\n     1      1 Ss\n");
            assert_eq!(
                mac_group_result(leader, true, &fake_ps_completion(rows.as_bytes(), Some(0))),
                Some(true),
                "state={state}"
            );
        }

        // 2) Live non-leader descendant (S or R+) in same pgid reports non-quiescent.
        for state in ["S", "R+"] {
            let rows = format!("  4242   4242 E\n  4243   4242 {state}\n");
            assert_eq!(
                mac_group_result(leader, true, &fake_ps_completion(rows.as_bytes(), Some(0))),
                Some(false),
                "state={state}"
            );
        }

        // 3) Zombie non-leader descendant in same pgid reports quiescent.
        let zombie_desc = b"  4242   4242 Z\n  4243   4242 Z+\n";
        assert_eq!(
            mac_group_result(leader, true, &fake_ps_completion(zombie_desc, Some(0))),
            Some(true)
        );

        // 4) Header rows and extra tokens on unrelated groups still succeed.
        let with_header_and_unrelated =
            b"  PID  PGID STAT\n  4242   4242 E\n     1      1 Ss extra\n";
        assert_eq!(
            mac_group_result(
                leader,
                true,
                &fake_ps_completion(with_header_and_unrelated, Some(0))
            ),
            Some(true)
        );

        // 5) Malformed column count on target group or non-zero exit returns None.
        let too_few = b"  4242   4242\n";
        assert_eq!(
            mac_group_result(leader, true, &fake_ps_completion(too_few, Some(0))),
            None
        );
        let too_many = b"  4242   4242 Z extra\n";
        assert_eq!(
            mac_group_result(leader, true, &fake_ps_completion(too_many, Some(0))),
            None
        );
        assert_eq!(
            mac_group_result(leader, true, &fake_ps_completion(b"", Some(1))),
            None
        );
    }

    #[test]
    fn leader_scope_natural_exit_completes_without_sending_signals() {
        let mut reactor = Reactor::new();
        let mut request = Request::new(vec!["/bin/sh".into(), "-c".into(), "printf ok".into()]);
        request.scope = Scope::Leader;
        let call = PhysicalCall::new("call_leader_fixture").unwrap();
        request.physical_call = Some(call.clone());
        request.deadline = Some(Instant::now() + Duration::from_secs(5));
        let id = reactor.start(request).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let mut completion = loop {
            match reactor.poll(id, Duration::from_millis(50)).unwrap() {
                Progress::Complete(done) => break done,
                Progress::Pending(_) => {
                    assert!(Instant::now() < until, "leader job timed out");
                }
            }
        };
        assert!(completion.cleanup_complete());
        assert!(completion.output_complete());
        assert_eq!(completion.stop, None);
        assert_eq!(completion.exit.map(|e| e.shell_code()), Some(0));
        assert_eq!(completion.stdout.bytes, b"ok");
        assert!(completion.signals.is_empty(), "{:?}", completion.signals);
        let proof = completion.owned_cleanup().unwrap();
        assert!(proof.matches(&call, Scope::Leader));
        assert_eq!(proof.pid(), completion.pid);
        assert!(!proof.matches(&call, Scope::WholeTree));
        assert!(!proof.matches(&PhysicalCall::new("call_other").unwrap(), Scope::Leader));
        completion.scope = Scope::WholeTree;
        completion.all_reaped = true;
        assert_eq!(completion.owned_cleanup().unwrap().scope(), Scope::Leader);
        let mut synthetic = fake_ps_completion(b"", Some(0));
        synthetic.scope = Scope::WholeTree;
        synthetic.all_reaped = true;
        assert!(synthetic.cleanup_complete());
        assert!(synthetic.owned_cleanup().is_none());
        for invalid in ["", "call with space", "call/new", &"a".repeat(161)] {
            assert!(PhysicalCall::new(invalid).is_err());
        }
    }

    #[test]
    fn process_group_natural_exit_and_stop_complete_cleanly() {
        let mut reactor = Reactor::new();

        // 1) Natural exit under Scope::ProcessGroup.
        let mut natural = Request::new(vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf out; exit 3".into(),
        ]);
        natural.scope = Scope::ProcessGroup;
        let natural_call = PhysicalCall::new("call_group_fixture").unwrap();
        natural.physical_call = Some(natural_call.clone());
        natural.deadline = Some(Instant::now() + Duration::from_secs(5));
        let id = reactor.start(natural).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let done = loop {
            match reactor.poll(id, Duration::from_millis(50)).unwrap() {
                Progress::Complete(done) => break done,
                Progress::Pending(_) => assert!(Instant::now() < until, "natural job timed out"),
            }
        };
        assert!(done.cleanup_complete(), "{done:?}");
        assert!(done.supervision_error.is_none(), "{done:?}");
        assert_eq!(done.group_quiescent, Some(true), "{done:?}");
        assert!(done.leader_reaped, "{done:?}");
        assert_eq!(done.exit.map(|e| e.shell_code()), Some(3), "{done:?}");
        assert!(
            done.owned_cleanup()
                .unwrap()
                .matches(&natural_call, Scope::ProcessGroup)
        );
        assert!(
            !done
                .owned_cleanup()
                .unwrap()
                .matches(&natural_call, Scope::WholeTree)
        );

        // 2) Cancelled running child under Scope::ProcessGroup.
        let mut slow = Request::new(vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()]);
        slow.scope = Scope::ProcessGroup;
        let stopped_call = PhysicalCall::new("call_group_cancel_fixture").unwrap();
        slow.physical_call = Some(stopped_call.clone());
        slow.deadline = Some(Instant::now() + Duration::from_secs(30));
        let id = reactor.start(slow).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        reactor.stop(id, StopReason::Cancelled).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let done = loop {
            match reactor.poll(id, Duration::from_millis(50)).unwrap() {
                Progress::Complete(done) => break done,
                Progress::Pending(_) => assert!(Instant::now() < until, "cancelled job timed out"),
            }
        };
        assert!(done.cleanup_complete(), "{done:?}");
        assert!(done.supervision_error.is_none(), "{done:?}");
        assert_eq!(done.group_quiescent, Some(true), "{done:?}");
        assert!(done.leader_reaped, "{done:?}");
        assert!(
            done.owned_cleanup()
                .unwrap()
                .matches(&stopped_call, Scope::ProcessGroup)
        );
        assert!(
            !done
                .owned_cleanup()
                .unwrap()
                .matches(&natural_call, Scope::ProcessGroup)
        );
    }
    fn duplex_request(script: &str) -> Request {
        let mut request = Request::new(vec!["/bin/sh".into(), "-c".into(), script.into()]);
        request.streams = Streams::Separate;
        request.scope = Scope::Leader;
        request.deadline = Some(Instant::now() + Duration::from_secs(5));
        request.terminate_grace = Duration::ZERO;
        request.duplex = Some(DuplexOptions {
            max_frame_bytes: 32768,
            max_input_bytes: 65536,
            max_input_frames: 4,
            max_output_bytes: 1024,
            max_output_events: 8,
            max_read_bytes_per_turn: 7,
            max_write_bytes_per_turn: 11,
        });
        request
    }
    fn finish(reactor: &mut Reactor, id: JobId) -> Completion {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            match reactor.poll(id, Duration::from_millis(10)).unwrap() {
                Progress::Complete(done) => return done,
                Progress::Pending(_) => {
                    assert!(Instant::now() < until, "fixture cleanup timed out")
                }
            }
        }
    }
    fn collect_event(event: OutputEvent, events: &mut Vec<OutputEvent>) {
        if let Some(previous) = events.last() {
            assert_eq!(event.generation, previous.generation);
            assert_eq!(event.sequence, previous.sequence + 1);
        } else {
            assert_eq!(event.sequence, 1);
        }
        events.push(event);
    }
    fn stream_bytes(events: &[OutputEvent], stream: OutputStream) -> Vec<u8> {
        events
            .iter()
            .filter(|e| e.stream == stream)
            .flat_map(|e| e.bytes.iter().copied())
            .collect()
    }
    #[test]
    fn duplex_refuses_invalid_bounds_and_preserves_fixed_stdin() {
        let mut reactor = Reactor::new();
        for index in 0..8 {
            let mut request = duplex_request("exit 0");
            let options = request.duplex.as_mut().unwrap();
            match index {
                0 => options.max_frame_bytes = 0,
                1 => options.max_input_bytes = 0,
                2 => options.max_input_frames = 0,
                3 => options.max_output_bytes = 0,
                4 => options.max_output_events = 0,
                5 => options.max_read_bytes_per_turn = 0,
                6 => options.max_write_bytes_per_turn = 0,
                _ => options.max_frame_bytes = options.max_input_bytes + 1,
            }
            assert!(matches!(
                reactor.start(request),
                Err(StartError::InvalidRequest(_))
            ));
        }
        let mut merged = duplex_request("exit 0");
        merged.streams = Streams::Merged;
        assert!(matches!(
            reactor.start(merged),
            Err(StartError::InvalidRequest(_))
        ));
        let mut mixed = duplex_request("exit 0");
        mixed.stdin = Some(vec![]);
        assert!(matches!(
            reactor.start(mixed),
            Err(StartError::InvalidRequest(_))
        ));
        assert_eq!(reactor.pending(), 0);
        let mut fixed = Request::new(vec!["/bin/cat".into()]);
        fixed.stdin = Some(b"fixed\xff\n".to_vec());
        fixed.scope = Scope::Leader;
        let id = reactor.start(fixed).unwrap();
        assert_eq!(reactor.send(id, b"no"), Err(SendError::NotDuplex));
        let done = finish(&mut reactor, id);
        assert!(done.cleanup_complete() && done.output_complete());
        assert_eq!(done.stdout.bytes, b"fixed\xff\n");
        assert!(done.duplex.is_none());
    }
    #[test]
    fn duplex_live_reply_partial_writes_raw_fragments_and_half_close() {
        let mut reactor = Reactor::new();
        let mut request = duplex_request(
            "printf 'ready\n'; IFS= read -r request; printf 'reply:%s\n' \"$request\"; cat; printf '\\377final\\n'; printf 'diagnostic\\n' >&2",
        );
        request.capture.max_bytes = 128;
        let id = reactor.start(request).unwrap();
        let generation = reactor.snapshot(id).unwrap().duplex.unwrap().generation;
        let payload = [b"raw-".as_slice(), &[0xe2, 0x82], &vec![b'x'; 8192]].concat();
        let mut events = Vec::new();
        let mut sent = false;
        let mut done = loop {
            while let Some(event) = reactor.receive(id).unwrap() {
                collect_event(event, &mut events);
            }
            if !sent && stream_bytes(&events, OutputStream::Stdout) == b"ready\n" {
                assert!(
                    reactor.snapshot(id).unwrap().exit.is_none(),
                    "live event must precede child completion"
                );
                reactor.send(id, b"answer\n").unwrap();
                reactor.send(id, &payload).unwrap();
                reactor.close_input(id).unwrap();
                reactor.close_input(id).unwrap();
                assert_eq!(reactor.send(id, b"late"), Err(SendError::InputClosed));
                sent = true;
            }
            match reactor.poll(id, Duration::from_millis(1)).unwrap() {
                Progress::Complete(done) => break done,
                Progress::Pending(snapshot) => {
                    let state = snapshot.duplex.unwrap();
                    assert!(state.input_queued_bytes <= 65536);
                    assert!(state.output_queued_bytes <= 1024);
                    assert!(state.output_queued_events <= 8);
                    assert_eq!(state.generation, generation);
                    assert!(snapshot.elapsed < Duration::from_secs(5));
                }
            }
        };
        assert!(sent);
        assert!(
            done.cleanup_complete() && done.output_complete(),
            "{done:?}"
        );
        assert_eq!(done.stop, None);
        let terminal = done.duplex.take().unwrap();
        for event in terminal.output {
            collect_event(event, &mut events);
        }
        assert_eq!(events[0].generation, generation);
        let expected = [
            b"ready\nreply:answer\n".as_slice(),
            &payload,
            b"\xfffinal\n",
        ]
        .concat();
        assert_eq!(stream_bytes(&events, OutputStream::Stdout), expected);
        assert_eq!(stream_bytes(&events, OutputStream::Stderr), b"diagnostic\n");
        assert!(done.stdout.limited && done.stdout.bytes.len() <= 128);
        assert!(
            terminal.status.input_close_requested
                && terminal.status.input_closed
                && terminal.status.input_drained
        );
        assert_eq!(
            terminal.status.input_written_bytes,
            (payload.len() + 7) as u64
        );
        assert_eq!(terminal.status.input_abandoned_bytes, 0);
        assert!(!terminal.status.output_closed_early);
        assert_eq!(reactor.send(id, b"removed"), Err(SendError::UnknownJob));
    }
    #[test]
    fn duplex_terminal_queue_and_input_bounds_are_explicit() {
        let mut reactor = Reactor::new();
        let mut request = duplex_request("cat; printf final; printf err >&2");
        let options = request.duplex.as_mut().unwrap();
        options.max_frame_bytes = 4;
        options.max_input_bytes = 4;
        options.max_input_frames = 2;
        options.max_write_bytes_per_turn = 1;
        let id = reactor.start(request).unwrap();
        assert_eq!(reactor.send(id, b"large"), Err(SendError::FrameTooLarge));
        reactor.send(id, b"abcd").unwrap();
        assert_eq!(reactor.send(id, b"e"), Err(SendError::QueueFull));
        reactor.poll(id, Duration::ZERO).unwrap();
        let state = reactor.snapshot(id).unwrap().duplex.unwrap();
        assert_eq!(state.input_written_bytes, 1);
        assert_eq!(
            state.input_queued_bytes, 4,
            "partial frame retains its bounded allocation"
        );
        assert_eq!(reactor.send(id, b"e"), Err(SendError::QueueFull));
        reactor.close_input(id).unwrap();
        let mut done = finish(&mut reactor, id);
        assert!(done.output_complete() && done.cleanup_complete());
        let terminal = done.duplex.take().unwrap();
        let events: Vec<_> = terminal.output.into_iter().collect();
        assert_eq!(stream_bytes(&events, OutputStream::Stdout), b"abcdfinal");
        assert_eq!(stream_bytes(&events, OutputStream::Stderr), b"err");
        assert!(terminal.status.input_drained);
        assert!(reactor.receive(id).is_err());
        let id = reactor.start(duplex_request("cat")).unwrap();
        for _ in 0..4 {
            reactor.send(id, b"").unwrap();
        }
        assert_eq!(reactor.send(id, b""), Err(SendError::QueueFull));
        reactor.close_input(id).unwrap();
        let done = finish(&mut reactor, id);
        assert!(done.duplex.unwrap().status.input_drained);
    }
    #[test]
    fn duplex_full_queues_do_not_block_deadline_cancel_or_cleanup() {
        for cancel in [false, true] {
            let mut reactor = Reactor::new();
            let mut request = duplex_request(
                "while :; do printf 'stdout flood'; printf 'stderr flood' >&2; done",
            );
            request.scope = Scope::ProcessGroup;
            let call = PhysicalCall::new(if cancel {
                "duplex_cancel"
            } else {
                "duplex_deadline"
            })
            .unwrap();
            request.physical_call = Some(call.clone());
            let options = request.duplex.as_mut().unwrap();
            options.max_frame_bytes = 262144;
            options.max_input_bytes = 262144;
            options.max_write_bytes_per_turn = 262144;
            options.max_output_bytes = 32;
            options.max_output_events = 1;
            options.max_read_bytes_per_turn = 32;
            let id = reactor.start(request).unwrap();
            reactor.send(id, &vec![b'x'; 262144]).unwrap();
            loop {
                match reactor.poll(id, Duration::from_millis(1)).unwrap() {
                    Progress::Pending(snapshot)
                        if snapshot.duplex.as_ref().unwrap().output_queued_events == 1 =>
                    {
                        break;
                    }
                    Progress::Pending(snapshot) => {
                        assert!(snapshot.elapsed < Duration::from_secs(5))
                    }
                    Progress::Complete(done) => panic!("flood ended before control: {done:?}"),
                }
            }
            if cancel {
                reactor.stop(id, StopReason::Cancelled).unwrap();
            } else {
                reactor.jobs.get_mut(&id).unwrap().request.deadline = Some(Instant::now());
            }
            let done = finish(&mut reactor, id);
            assert_eq!(
                done.stop,
                Some(if cancel {
                    StopReason::Cancelled
                } else {
                    StopReason::Deadline
                })
            );
            assert!(done.cleanup_complete(), "{done:?}");
            assert!(
                done.owned_cleanup()
                    .unwrap()
                    .matches(&call, Scope::ProcessGroup)
            );
            assert!(!done.output_complete());
            let terminal = done.duplex.unwrap();
            assert_eq!(
                terminal.output.len(),
                1,
                "accepted protocol bytes must remain available"
            );
            assert!(terminal.status.output_queued_bytes <= 32);
            assert!(terminal.status.output_closed_early);
            assert!(terminal.status.input_abandoned_bytes > 0);
            assert!(!terminal.status.input_drained);
        }
    }
    #[test]
    fn duplex_stdout_eof_and_broken_input_do_not_hide_stderr() {
        let mut reactor = Reactor::new();
        let id = reactor
            .start(duplex_request(
                "exec 0<&-; exec 1>&-; printf closed >&2; while :; do :; done",
            ))
            .unwrap();
        let mut events = Vec::new();
        loop {
            match reactor.poll(id, Duration::from_millis(1)).unwrap() {
                Progress::Pending(snapshot) => assert!(snapshot.elapsed < Duration::from_secs(5)),
                Progress::Complete(done) => {
                    panic!("fixture completed before EPIPE probe: {done:?}")
                }
            }
            while let Some(event) = reactor.receive(id).unwrap() {
                collect_event(event, &mut events);
            }
            if stream_bytes(&events, OutputStream::Stderr) == b"closed"
                && reactor.snapshot(id).unwrap().stdout.eof
            {
                break;
            }
        }
        reactor.send(id, b"unread").unwrap();
        reactor.poll(id, Duration::ZERO).unwrap();
        let status = reactor.snapshot(id).unwrap().duplex.unwrap();
        assert!(status.input_broken_pipe && status.input_closed);
        assert_eq!(status.input_abandoned_bytes, 6);
        assert!(!status.input_drained);
        assert_eq!(reactor.send(id, b"late"), Err(SendError::InputClosed));
        reactor.stop(id, StopReason::Cancelled).unwrap();
        let done = finish(&mut reactor, id);
        assert!(done.stdout.eof && done.cleanup_complete());
        assert_eq!(stream_bytes(&events, OutputStream::Stderr), b"closed");
    }
    #[test]
    fn duplex_capture_limit_and_unobserved_cleanup_remain_failures() {
        let mut reactor = Reactor::new();
        let mut request = duplex_request("while :; do printf flood; done");
        request.capture.max_bytes = 8;
        request.capture.on_limit = crate::capture::LimitAction::Terminate;
        let call = PhysicalCall::new("duplex_output_limit").unwrap();
        request.physical_call = Some(call);
        let id = reactor.start(request).unwrap();
        let done = finish(&mut reactor, id);
        assert_eq!(done.stop, Some(StopReason::OutputLimit));
        assert!(done.cleanup_complete());
        assert!(!done.output_complete());
        assert!(done.duplex.as_ref().unwrap().status.output_closed_early);
        assert!(
            done.duplex
                .as_ref()
                .unwrap()
                .output
                .iter()
                .map(|e| e.bytes.len())
                .sum::<usize>()
                > 8
        );
        let id = reactor.start(duplex_request("exit 0")).unwrap();
        reactor.jobs.get_mut(&id).unwrap().error =
            Some("fixture: cleanup observation unknown".into());
        let done = finish(&mut reactor, id);
        assert!(!done.cleanup_complete());
        assert!(done.owned_cleanup().is_none());

        // No-fd poll fallback keeps the 50 ms floor rather than spinning at 2 ms.
        let mut no_fd = Request::new(vec![
            "/bin/sh".into(),
            "-c".into(),
            "exec 0<&- 1>&- 2>&-; while :; do sleep 1; done".into(),
        ]);
        no_fd.scope = Scope::ProcessGroup;
        no_fd.deadline = Some(Instant::now() + Duration::from_secs(5));
        let id = reactor.start(no_fd).unwrap();
        #[cfg(target_os = "linux")]
        {
            reactor.jobs.get_mut(&id).unwrap().pidfd = None;
        }
        let wait_eof = Instant::now() + Duration::from_secs(2);
        while {
            let snap = reactor.snapshot(id).unwrap();
            !snap.stdout.eof || snap.stderr.as_ref().is_some_and(|s| !s.eof)
        } {
            assert!(Instant::now() < wait_eof, "stdio did not close");
            reactor.poll(id, Duration::from_millis(10)).unwrap();
        }
        {
            let job = reactor.jobs.get(&id).unwrap();
            assert!(job.out.is_none() && job.err.is_none() && job.input.is_none());
        }
        let poll_started = Instant::now();
        assert!(matches!(
            reactor.poll(id, Duration::from_millis(50)).unwrap(),
            Progress::Pending(_)
        ));
        assert!(
            poll_started.elapsed() >= Duration::from_millis(35),
            "no-fd poll spun instead of keeping the 50 ms floor: {:?}",
            poll_started.elapsed()
        );
        reactor.stop(id, StopReason::Cancelled).unwrap();
        let done = finish(&mut reactor, id);
        assert!(done.cleanup_complete());

        // Full duplex output queue with no input/pidfd also keeps the 50 ms floor.
        let mut full_duplex = duplex_request("printf 'stdout flood'; while :; do sleep 1; done");
        full_duplex.scope = Scope::ProcessGroup;
        full_duplex.deadline = Some(Instant::now() + Duration::from_secs(5));
        let options = full_duplex.duplex.as_mut().unwrap();
        options.max_output_events = 1;
        options.max_output_bytes = 32;
        let id = reactor.start(full_duplex).unwrap();
        #[cfg(target_os = "linux")]
        {
            reactor.jobs.get_mut(&id).unwrap().pidfd = None;
        }
        let wait_full = Instant::now() + Duration::from_secs(2);
        while reactor
            .snapshot(id)
            .unwrap()
            .duplex
            .as_ref()
            .unwrap()
            .output_queued_events
            == 0
        {
            assert!(Instant::now() < wait_full, "duplex output did not fill");
            reactor.poll(id, Duration::from_millis(10)).unwrap();
        }
        let poll_started = Instant::now();
        assert!(matches!(
            reactor.poll(id, Duration::from_millis(50)).unwrap(),
            Progress::Pending(_)
        ));
        assert!(
            poll_started.elapsed() >= Duration::from_millis(35),
            "full-duplex no-fd poll spun instead of keeping the 50 ms floor: {:?}",
            poll_started.elapsed()
        );
        reactor.stop(id, StopReason::Cancelled).unwrap();
        let done = finish(&mut reactor, id);
        assert!(done.cleanup_complete());
    }
}
