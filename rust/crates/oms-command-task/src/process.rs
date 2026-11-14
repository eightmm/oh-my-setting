use crate::{Error, Result, checkpoint};
use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid},
};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    thread,
    time::{Duration, Instant},
};

struct OwnedChild {
    child: Child,
    pid: Pid,
    reaped: bool,
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            // An unreaped directly-owned child pins this process generation.
            // Never signal a bare PID observed after wait()/try_wait().
            let _ = kill_process_group(self.pid, Signal::KILL);
            let _ = self.child.wait();
        }
    }
}
impl OwnedChild {
    fn interrupt(&mut self, signal: Option<&AtomicI32>) -> Result<()> {
        let observed = match interrupted_code(signal) - 128 {
            1 => Signal::HUP,
            15 => Signal::TERM,
            _ => Signal::INT,
        };
        let _ = kill_process_group(self.pid, observed);
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if waitid(
                WaitId::Pid(self.pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )
            .map_err(|e| Error::io("observe interrupted command", e))?
            .is_some()
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = kill_process_group(self.pid, Signal::KILL);
        self.child
            .wait()
            .map_err(|e| Error::io("reap interrupted command", e))?;
        self.reaped = true;
        Ok(())
    }
}
pub(crate) struct Outcome {
    pub exit: i32,
    pub tail: Vec<u8>,
}
pub(crate) fn execute(
    command: &mut Command,
    merge_stderr: bool,
    stdin_inherit: bool,
    output: &mut dyn Write,
    tail_cap: usize,
    interrupted: &AtomicBool,
    signal: Option<&AtomicI32>,
) -> Result<Outcome> {
    checkpoint(interrupted, signal)?;
    let (read_fd, write_fd) =
        rustix::pipe::pipe().map_err(|e| Error::io("create command output pipe", e))?;
    fcntl_setfl(
        &read_fd,
        fcntl_getfl(&read_fd).map_err(|e| Error::io("read pipe flags", e))? | OFlags::NONBLOCK,
    )
    .map_err(|e| Error::io("set pipe flags", e))?;
    let stdout = File::from(write_fd);
    let stderr = if merge_stderr {
        Stdio::from(
            stdout
                .try_clone()
                .map_err(|e| Error::io("clone output pipe", e))?,
        )
    } else {
        Stdio::null()
    };
    command
        .process_group(0)
        .stdin(if stdin_inherit {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::from(stdout))
        .stderr(stderr);
    let child = command.spawn().map_err(|e| Error::io("start command", e))?;
    let pid = Pid::from_raw(child.id() as i32)
        .ok_or_else(|| Error::new(1, "invalid owned command identity"))?;
    let mut owned = OwnedChild {
        child,
        pid,
        reaped: false,
    };
    // Command retains configured Stdio handles, so drop them before reading EOF.
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut pipe = File::from(read_fd);
    let mut tail = Vec::new();
    let mut buf = [0; 16384];
    loop {
        if interrupted.load(Ordering::Relaxed) {
            owned.interrupt(signal)?;
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => output
                        .write_all(&buf[..n])
                        .map_err(|e| Error::io("write interrupted command output", e))?,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(Error::io("drain interrupted command output", e)),
                }
            }
            return Err(Error::new(interrupted_code(signal), "interrupted"));
        }
        loop {
            if interrupted.load(Ordering::Relaxed) {
                break;
            }
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    output
                        .write_all(&buf[..n])
                        .map_err(|e| Error::io("write command output", e))?;
                    if tail_cap > 0 {
                        tail.extend_from_slice(&buf[..n]);
                        if tail.len() > tail_cap {
                            tail.drain(..tail.len() - tail_cap);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::io("read command output", e)),
            }
        }
        let terminal = waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .map_err(|e| Error::io("observe command", e))?;
        if terminal.is_some() {
            let _ = kill_process_group(pid, Signal::KILL);
            let status = owned
                .child
                .wait()
                .map_err(|e| Error::io("reap command", e))?;
            owned.reaped = true;
            // Descendants no longer hold the pipe. Drain everything the command
            // wrote, including the final kernel-buffered chunk, before receipt.
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        output
                            .write_all(&buf[..n])
                            .map_err(|e| Error::io("write command output", e))?;
                        if tail_cap > 0 {
                            tail.extend_from_slice(&buf[..n]);
                            if tail.len() > tail_cap {
                                tail.drain(..tail.len() - tail_cap);
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(Error::io("drain command output", e)),
                }
            }
            output
                .flush()
                .map_err(|e| Error::io("flush command output", e))?;
            return Ok(Outcome {
                exit: status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
                tail,
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}
pub(crate) fn interrupted_code(signal: Option<&AtomicI32>) -> i32 {
    let signal = signal.map(|s| s.load(Ordering::Relaxed)).unwrap_or(2);
    128 + if [1, 2, 15].contains(&signal) {
        signal
    } else {
        2
    }
}
