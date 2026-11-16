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
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::marker::PhantomData;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub(crate) const MAX_DROP_WAIT: Duration = Duration::from_secs(5);
const POST_EXIT_DRAIN: Duration = Duration::from_millis(250);

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
        let (input, child_input) = if request.stdin.is_some() {
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
            stdout,
            stderr,
            out: if detached { None } else { Some(out_read) },
            err: if detached { None } else { err_read },
            input: if detached { None } else { input },
            input_offset: 0,
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
            let mut wait = budget.min(Duration::from_millis(50));
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
            match os::test_kill_process_group(self.pid) {
                Err(Errno::SRCH) => return Some(true),
                Err(Errno::PERM | Errno::ACCESS) => return Some(false),
                Err(_) => return None,
                Ok(()) => {}
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
    if capture.should_stop() {
        fd.take();
        return Ok(());
    }
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
            Ok(n) => {
                capture.push(&buf[..n]);
                if capture.should_stop() {
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
    match os::test_kill_process_group(pid) {
        Err(Errno::SRCH) => return Some(true),
        Err(Errno::PERM | Errno::ACCESS) => return Some(false),
        Err(_) => return None,
        Ok(()) => {}
    }
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
        // Synchronous command-side observations share the bounded producer.
        // Job::observe_group instead retains it and advances without waiting.
        let mut reactor = Reactor::new();
        let id = reactor.start(mac_group_request()).ok()?;
        let until = Instant::now() + Duration::from_millis(750);
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
        "-ax".into(),
        "-o".into(),
        "pid=".into(),
        "-o".into(),
        "pgid=".into(),
        "-o".into(),
        "stat=".into(),
    ]);
    request.scope = Scope::Leader;
    request.environment = vec![("LC_ALL".into(), "C".into())];
    request.deadline = Some(Instant::now() + Duration::from_millis(500));
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
        let row_pid = fields.next()?.parse::<i32>().ok()?;
        let group = fields.next()?.parse::<i32>().ok()?;
        let state = fields.next()?;
        if fields.next().is_some() {
            return None;
        }
        if leader_exited && row_pid == target {
            continue;
        }
        if group == target && !state.starts_with(['Z', 'X']) {
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

        // 4) Malformed column count or non-zero exit returns None.
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
}
