//! A single-threaded reactor is the sole reaper of every child it launches.
//!
//! `poll` never waits for process exit. A deadline stops execution, not kernel
//! teardown. Pending jobs remain in this reactor; callers must keep driving it.
//! Dropping a reactor with children deliberately drains cleanup without a time
//! bound. Bounded-return services must retain the reactor instead of dropping it.
use crate::capture::{Capture, CaptureOptions, Captured};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::{self as os, Pid, Signal, WaitId, WaitIdOptions};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::marker::PhantomData;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Only the unreaped direct child is signal authority. No containment claim.
    Leader,
    /// A POSIX group is not whole-tree containment; session escapes are unknown.
    ProcessGroup,
    /// Requires the exclusive, pre-spawn Linux subreaper reactor.
    WholeTree,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Streams {
    Merged,
    Separate,
}
#[derive(Clone, Debug)]
pub struct Request {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<Vec<u8>>,
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
}
impl Request {
    pub fn new(argv: Vec<OsString>) -> Self {
        Self {
            argv,
            cwd: None,
            stdin: None,
            clear_environment: false,
            environment: vec![],
            remove_environment: vec![],
            streams: Streams::Merged,
            capture: CaptureOptions::default(),
            stderr_capture: None,
            deadline: None,
            scope: Scope::ProcessGroup,
            terminate_grace: Duration::from_secs(2),
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
}
impl Completion {
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
}
#[derive(Debug)]
pub enum Progress {
    Pending(Snapshot),
    Complete(Completion),
}
struct Job {
    _child: Child,
    pid: Pid,
    request: Request,
    out: Option<OwnedFd>,
    err: Option<OwnedFd>,
    input: Option<OwnedFd>,
    input_offset: usize,
    stdout: Capture,
    stderr: Option<Capture>,
    started: Instant,
    exit: Option<Exit>,
    stop: Option<StopReason>,
    stopping: Option<Instant>,
    kill_sent: bool,
    reaped: bool,
    all_reaped: bool,
    children_exhausted: bool,
    ownership_lost: bool,
    adopted_signals: BTreeMap<i32, bool>,
    group_quiescent: Option<bool>,
    signals: Vec<SignalRecord>,
    error: Option<String>,
    finished: bool,
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
        let (out_read, out_write) =
            rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(|e| e.to_string())?;
        nonblocking(&out_read)?;
        let (err_read, err_write) = if request.streams == Streams::Separate {
            let (r, w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
                .map_err(|e| e.to_string())?;
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
        let (input, child_input) = if request.stdin.is_some() {
            let (r, w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
                .map_err(|e| e.to_string())?;
            nonblocking(&w)?;
            (Some(w), Stdio::from(r))
        } else {
            (None, Stdio::null())
        };
        let mut command = Command::new(&request.argv[0]);
        command
            .args(&request.argv[1..])
            .stdin(child_input)
            .stdout(Stdio::from(out_write))
            .stderr(Stdio::from(err_write));
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
        let stderr = err_read.as_ref().map(|_| {
            let policy = request.stderr_capture.clone().unwrap_or_else(|| {
                let mut policy = request.capture.clone();
                policy.lines = None;
                policy
            });
            Capture::new(policy)
        });
        let job = Job {
            _child: child,
            pid,
            stdout: Capture::new(request.capture.clone()),
            stderr,
            out: Some(out_read),
            err: err_read,
            input,
            input_offset: 0,
            request,
            started,
            exit: None,
            stop: None,
            stopping: None,
            kill_sent: false,
            reaped: false,
            all_reaped: false,
            children_exhausted: false,
            ownership_lost: false,
            adopted_signals: BTreeMap::new(),
            group_quiescent: None,
            signals: vec![],
            error: None,
            finished: false,
        };
        let id = JobId(self.next);
        self.next = next;
        self.jobs.insert(id, job);
        Ok(id)
    }
    /// Numeric provenance is retained; callers supply the real received signal.
    pub fn stop(&mut self, id: JobId, reason: StopReason) -> crate::Result<()> {
        let job = self.jobs.get_mut(&id).ok_or("unknown owned process")?;
        if let StopReason::Signal(number) = reason {
            if Signal::from_named_raw(number).is_none() {
                return Err("unsupported signal number".into());
            }
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
            let mut wait = budget.min(Duration::from_millis(50));
            if job.stopping.is_none()
                && let Some(deadline) = job.request.deadline
            {
                wait = wait.min(deadline.saturating_duration_since(now));
            }
            if let Some(stopping) = job.stopping
                && !job.kill_sent
            {
                wait = wait
                    .min((stopping + job.request.terminate_grace).saturating_duration_since(now));
            }
            let mut fds = Vec::new();
            if let Some(fd) = &job.out {
                fds.push(PollFd::new(fd, PollFlags::IN));
            }
            if let Some(fd) = &job.err {
                fds.push(PollFd::new(fd, PollFlags::IN));
            }
            if let Some(fd) = &job.input {
                fds.push(PollFd::new(fd, PollFlags::OUT));
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
}
impl Drop for Reactor {
    fn drop(&mut self) {
        for job in self.jobs.values_mut() {
            job.begin_stop(StopReason::SupervisorError);
        }
        // No background reaper or blocking wait syscall. This can still take
        // forever for kernel D-state: only retaining the reactor permits a
        // bounded caller return with honest ownership.
        while !self.jobs.is_empty() {
            let ids: Vec<_> = self.jobs.keys().copied().collect();
            for id in ids {
                let _ = self.poll(id, Duration::from_millis(50));
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
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            pid: self.pid.as_raw_nonzero().get() as u32,
            exit: self.exit,
            stop: self.stop,
            stdout: self.stdout.snapshot(),
            stderr: self.stderr.as_ref().map(Capture::snapshot),
            elapsed: self.started.elapsed(),
            supervision_error: self.error.clone(),
        }
    }
    fn completion(self) -> Completion {
        Completion {
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
        }
    }
    fn fail(&mut self, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
        }
        self.begin_stop(StopReason::SupervisorError);
    }
    fn begin_stop(&mut self, reason: StopReason) {
        if self.finished || self.stop.is_some() || self.stopping.is_some() {
            return;
        }
        self.stop = Some(reason);
        self.begin_cleanup();
    }
    fn begin_cleanup(&mut self) {
        if self.stopping.is_some() {
            return;
        }
        self.stopping = Some(Instant::now());
        self.input.take();
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
        if let Err(e) = result
            && e != Errno::SRCH
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
                self.exit = Some(Exit {
                    code: status.exit_status(),
                    signal: status.terminating_signal(),
                })
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
        if let Err(e) = drain(&mut self.out, &mut self.stdout) {
            self.out.take();
            self.fail(e);
        }
        if let Some(capture) = &mut self.stderr
            && let Err(e) = drain(&mut self.err, capture)
        {
            self.err.take();
            self.fail(e);
        }
        self.write_input();
        let eof = self.out.is_none() && self.err.is_none();
        // Freeze a valid completion decision before considering the deadline.
        // Cleanup latency must not turn a completed command into a timeout.
        if self.stopping.is_none() {
            if self.stdout.should_stop() || self.stderr.as_ref().is_some_and(Capture::should_stop) {
                let cutoff = self.stdout.snapshot().cutoff
                    || self.stderr.as_ref().is_some_and(|s| s.snapshot().cutoff);
                self.begin_stop(if cutoff {
                    StopReason::ConsumerCutoff
                } else {
                    StopReason::OutputLimit
                });
            } else if self.exit.is_some() && eof {
                self.begin_cleanup();
            } else if self.request.deadline.is_some_and(|d| Instant::now() >= d) {
                self.begin_stop(StopReason::Deadline);
            }
        }
        if self.stopping.is_none() {
            return;
        }
        self.group_quiescent = if self.request.scope == Scope::Leader {
            Some(self.exit.is_some())
        } else {
            group_quiescent(self.pid)
        };
        if !self.kill_sent
            && self.group_quiescent != Some(true)
            && self
                .stopping
                .is_some_and(|s| s.elapsed() >= self.request.terminate_grace)
        {
            self.signal(Signal::KILL);
            self.kill_sent = true;
        }
        self.observe_exit();
        // A kill is asynchronous. Only observed exit allows a nonblocking reap.
        // Final group signaling must happen BEFORE consuming the leader token.
        let signals_done = self.group_quiescent == Some(true)
            || self.kill_sent && self.group_quiescent.is_none()
            || self.request.scope == Scope::Leader;
        if !self.reaped && self.exit.is_some() && signals_done {
            match os::waitid(
                WaitId::Pid(self.pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
            ) {
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
        // Requested cancellation/cutoff is the consumer's authority to stop
        // drainage. Ordinary leader exit alone never closes a live pipe.
        if self.stop.is_some() && (self.kill_sent || self.group_quiescent == Some(true)) {
            self.out.take();
            self.err.take();
        }
        if self.reaped && self.out.is_none() && self.err.is_none() {
            if self.request.scope == Scope::WholeTree && !self.children_exhausted {
                return;
            }
            if self.request.scope == Scope::ProcessGroup {
                self.group_quiescent = group_quiescent(self.pid);
                if self.group_quiescent == Some(false) {
                    return;
                }
                if self.group_quiescent.is_none() && self.error.is_none() {
                    self.error =
                        Some("process group cleanup could not be observed on this platform".into());
                }
            }
            self.finished = true;
        }
    }
    fn write_input(&mut self) {
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
fn drain(fd: &mut Option<OwnedFd>, capture: &mut Capture) -> crate::Result<()> {
    let Some(opened) = fd.as_ref() else {
        return Ok(());
    };
    let mut buf = [0u8; 65536];
    // Bound per-turn work so an unbounded writer cannot starve cancellation.
    for _ in 0..4 {
        match rustix::io::read(opened, &mut buf) {
            Ok(0) => {
                capture.eof = true;
                fd.take();
                return Ok(());
            }
            Ok(n) => capture.push(&buf[..n]),
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
/// A proc snapshot can prove group quiescence while the leader is pinned.
/// Platforms without that observation return unknown, never kill(0) as proof.
pub(crate) fn group_quiescent(pid: Pid) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
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
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
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
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}
