//! Streaming for the authorized scheduler and fixed read-only metadata queries.
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group};
use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Default)]
struct JobOutput {
    line: Vec<u8>,
    discarded: bool,
    plain_suffix: bool,
    job_id: OsString,
    cluster_name: OsString,
}
impl JobOutput {
    fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.end_line();
                continue;
            }
            if self.discarded {
                continue;
            }
            self.line.push(byte);
            // Arbitrary diagnostic lines consume no unbounded capture memory.
            let verbose = b"Submitted batch job ";
            if !self.line[0].is_ascii_digit()
                && !verbose.starts_with(&self.line)
                && !self.line.starts_with(verbose)
            {
                self.line.clear();
                self.discarded = true;
            } else if self.line[0].is_ascii_digit() {
                if !self.plain_suffix && byte == b';' {
                    self.plain_suffix = true;
                } else if !self.plain_suffix && !byte.is_ascii_digit() {
                    self.line.clear();
                    self.discarded = true;
                }
            }
        }
    }
    fn end_line(&mut self) {
        if !self.discarded {
            let verbose = self.line.strip_prefix(b"Submitted batch job ");
            let value = verbose.unwrap_or(&self.line);
            let end = value.iter().take_while(|b| b.is_ascii_digit()).count();
            if end > 0 {
                let tail = &value[end..];
                let where_on = if tail.is_empty() {
                    Some(&[][..])
                } else if verbose.is_some() {
                    let marker = b" on cluster ";
                    tail.windows(marker.len())
                        .rposition(|s| s == marker)
                        .map(|p| &tail[p + marker.len()..])
                } else {
                    tail.strip_prefix(b";")
                };
                if let Some(where_on) = where_on {
                    self.job_id = OsString::from_vec(value[..end].to_vec());
                    self.cluster_name = OsString::from_vec(where_on.to_vec());
                }
            }
        }
        self.line.clear();
        self.discarded = false;
        self.plain_suffix = false;
    }
}

// The handle is the sole reaper. NOWAIT preserves the leader PID through every
// group signal; cleanup disarms before any subsequent pipe handling can fail.
struct Running {
    child: Child,
    group: Pid,
    owned: bool,
}
impl Running {
    fn observe(&mut self) -> io::Result<Option<i32>> {
        match rustix::process::waitid(
            WaitId::Pid(self.group),
            WaitIdOptions::EXITED
                | WaitIdOptions::STOPPED
                | WaitIdOptions::NOHANG
                | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) if status.exited() || status.killed() || status.dumped() => {
                Ok(Some(status.exit_status().unwrap_or_else(|| {
                    128 + status.terminating_signal().unwrap_or(1)
                })))
            }
            Ok(Some(status))
                if status.stopping_signal() == Some(Signal::TTIN.as_raw())
                    || status.stopping_signal() == Some(Signal::TTOU.as_raw()) =>
            {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "client attempted background terminal I/O",
                ))
            }
            Ok(_) | Err(rustix::io::Errno::INTR) => Ok(None),
            Err(rustix::io::Errno::CHILD) => {
                self.owned = false;
                Err(io::Error::other("child ownership was lost"))
            }
            Err(e) => Err(e.into()),
        }
    }
    fn stop(&mut self) {
        if !self.owned {
            return;
        }
        // A parent integration must not independently reap this owned Child.
        // ECHILD is nevertheless handled without signalling a recycled number.
        loop {
            match rustix::process::waitid(
                WaitId::Pid(self.group),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::CHILD) => {
                    self.owned = false;
                    return;
                }
                _ => break,
            }
        }
        let _ = kill_process_group(self.group, Signal::KILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.owned = false;
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) enum Input {
    Caller,
    Closed,
}
pub(crate) enum Execution {
    Answered {
        code: i32,
        job_id: OsString,
        cluster_name: OsString,
        output_complete: bool,
    },
    Interrupted,
    Unknown,
}

enum Drained {
    Eof,
    Pending,
    More,
}
fn drain(
    reader: &mut impl Read,
    out: &mut dyn Write,
    mut jobs: Option<&mut JobOutput>,
) -> io::Result<Drained> {
    let mut bytes = [0u8; 16384];
    for _ in 0..16 {
        match reader.read(&mut bytes) {
            Ok(0) => return Ok(Drained::Eof),
            Ok(n) => {
                if let Some(jobs) = jobs.as_deref_mut() {
                    jobs.bytes(&bytes[..n]);
                }
                out.write_all(&bytes[..n])?;
                out.flush()?;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Drained::Pending),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(Drained::More)
}

// Reopen the *same* terminal with an independent nonblocking description. This
// avoids changing the caller's flags, raw/canonical settings or foreground group.
fn terminal_source() -> io::Result<Option<std::fs::File>> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Ok(None);
    }
    if rustix::termios::tcgetpgrp(&stdin)? != rustix::process::getpgrp() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "stdin is not a foreground terminal",
        ));
    }
    let name = rustix::termios::ttyname(&stdin, Vec::new())?;
    let fd = rustix::fs::open(
        &name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let original = rustix::fs::fstat(&stdin)?;
    let reopened = rustix::fs::fstat(&fd)?;
    if original.st_dev != reopened.st_dev
        || original.st_ino != reopened.st_ino
        || original.st_rdev != reopened.st_rdev
    {
        return Err(io::Error::other("terminal identity changed"));
    }
    Ok(Some(std::fs::File::from(fd)))
}
struct TerminalRelay {
    source: std::fs::File,
    sink: Option<std::process::ChildStdin>,
    bytes: [u8; 16384],
    start: usize,
    end: usize,
}
impl TerminalRelay {
    fn pump(&mut self) -> io::Result<()> {
        let Some(sink) = self.sink.as_mut() else {
            return Ok(());
        };
        if self.start == self.end {
            if rustix::termios::tcgetpgrp(&self.source)? != rustix::process::getpgrp() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "terminal foreground ownership changed",
                ));
            }
            match self.source.read(&mut self.bytes) {
                Ok(0) => {
                    self.sink.take();
                    return Ok(());
                }
                Ok(n) => {
                    self.start = 0;
                    self.end = n;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        match sink.write(&self.bytes[self.start..self.end]) {
            Ok(0) => {
                self.sink.take();
            }
            Ok(n) => self.start += n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                self.sink.take();
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }
}

pub(crate) fn run(
    executable: &OsStr,
    args: &[OsString],
    cwd: &Path,
    input: Input,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Execution {
    if interrupted.load(Ordering::Relaxed) {
        return Execution::Interrupted;
    }
    let terminal = match input {
        Input::Closed => None,
        Input::Caller => match terminal_source() {
            Ok(source) => source,
            Err(_) => {
                let _ = err.write_all(b"sbatch: cannot safely forward terminal stdin\n");
                return Execution::Answered {
                    code: 126,
                    job_id: OsString::new(),
                    cluster_name: OsString::new(),
                    output_complete: true,
                };
            }
        },
    };
    let stdin = if terminal.is_some() {
        Stdio::piped()
    } else {
        match input {
            Input::Caller => Stdio::inherit(),
            Input::Closed => Stdio::null(),
        }
    };
    let executable = if executable.as_bytes().contains(&b'/') {
        cwd.join(executable).into_os_string()
    } else {
        executable.to_owned()
    };
    let child = Command::new(&executable)
        .args(args)
        .current_dir(cwd)
        .process_group(0)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(e) => {
            let code = if e.kind() == io::ErrorKind::NotFound {
                127
            } else {
                126
            };
            let _ = err.write_all(b"sbatch: ");
            let _ = err.write_all(e.to_string().as_bytes());
            let _ = err.write_all(b"\n");
            return Execution::Answered {
                code,
                job_id: OsString::new(),
                cluster_name: OsString::new(),
                output_complete: true,
            };
        }
    };
    let group = Pid::from_raw(child.id() as i32).expect("spawned child pid is positive");
    let mut running = Running {
        child,
        group,
        owned: true,
    };
    let mut stdout = running.child.stdout.take().unwrap();
    let mut stderr = running.child.stderr.take().unwrap();
    let mut terminal = terminal.map(|source| TerminalRelay {
        source,
        sink: running.child.stdin.take(),
        bytes: [0; 16384],
        start: 0,
        end: 0,
    });
    let nonblocking = || -> io::Result<()> {
        fcntl_setfl(&stdout, fcntl_getfl(&stdout)? | OFlags::NONBLOCK)?;
        fcntl_setfl(&stderr, fcntl_getfl(&stderr)? | OFlags::NONBLOCK)?;
        if let Some(sink) = terminal.as_ref().and_then(|relay| relay.sink.as_ref()) {
            fcntl_setfl(sink, fcntl_getfl(sink)? | OFlags::NONBLOCK)?;
        }
        Ok(())
    };
    if nonblocking().is_err() {
        return Execution::Unknown;
    }
    let (mut out_eof, mut err_eof, mut status) = (false, false, None);
    let mut jobs = JobOutput::default();
    let mut output_complete = true;
    let mut stopping = false;
    loop {
        if status.is_none() {
            match running.observe() {
                Ok(Some(code)) => status = Some(code),
                Ok(None) => {}
                Err(e) => {
                    let _ = writeln!(err, "error: {e}");
                    return Execution::Unknown;
                }
            }
        }
        if interrupted.load(Ordering::Relaxed) {
            if status.is_none() {
                return Execution::Interrupted;
            }
            // A completed answer survives cancellation. Retain PID ownership
            // through group cleanup, then drain only bytes already available.
            if !stopping {
                terminal.take();
                running.stop();
                stopping = true;
            }
            output_complete = false;
        }
        let mut more = false;
        let mut forward_error = terminal.as_mut().is_some_and(|relay| relay.pump().is_err());
        if !out_eof {
            match drain(&mut stdout, out, Some(&mut jobs)) {
                Ok(Drained::Eof) => out_eof = true,
                Ok(Drained::More) => more = true,
                Ok(Drained::Pending) => {}
                Err(_) => forward_error = true,
            }
        }
        if !err_eof {
            match drain(&mut stderr, err, None) {
                Ok(Drained::Eof) => err_eof = true,
                Ok(Drained::More) => more = true,
                Ok(Drained::Pending) => {}
                Err(_) => forward_error = true,
            }
        }
        if forward_error {
            if status.is_none() {
                match running.observe() {
                    Ok(Some(code)) => status = Some(code),
                    _ => return Execution::Unknown,
                }
            }
            output_complete = false;
            break;
        }
        if status.is_some() && (out_eof && err_eof || stopping && !more) {
            break;
        }
        if !stopping {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // Normal completion also cleans up descendants that closed their capture
    // pipes. The leader is still unreaped here unless cancellation cleaned up.
    running.stop();
    terminal.take();
    if output_complete {
        jobs.end_line();
    }
    Execution::Answered {
        code: status.unwrap(),
        job_id: jobs.job_id,
        cluster_name: jobs.cluster_name,
        output_complete,
    }
}
