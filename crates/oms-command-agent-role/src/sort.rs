use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use std::{
    io::{self, Read, Write},
    os::unix::process::ExitStatusExt,
    path::Path,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(crate) const MAX_BYTES: usize = 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);

pub(crate) struct Failure {
    pub code: i32,
    pub message: Vec<u8>,
}
impl Failure {
    fn new(code: i32, message: impl Into<Vec<u8>>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub(crate) fn interrupted() -> Self {
        Self::new(130, Vec::new())
    }
    pub(crate) fn input_limit() -> Self {
        Self::new(1, b"agent-role list input exceeds 1048576 bytes".to_vec())
    }
}
impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::new(1, format!("agent-role list sort: {error}").into_bytes())
    }
}

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn nonblocking(fd: &impl std::os::fd::AsFd) -> Result<(), Failure> {
    let flags = fcntl_getfl(fd).map_err(io::Error::from)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(io::Error::from)?;
    Ok(())
}
fn receive<R: Read>(
    pipe: &mut Option<R>,
    bytes: &mut Vec<u8>,
    limit: usize,
    stream: &str,
) -> Result<bool, Failure> {
    let Some(reader) = pipe.as_mut() else {
        return Ok(false);
    };
    let mut buffer = [0; 8192];
    match reader.read(&mut buffer) {
        Ok(0) => {
            pipe.take();
            Ok(true)
        }
        Ok(count) => {
            if count > limit.saturating_sub(bytes.len()) {
                return Err(Failure::new(
                    1,
                    format!("agent-role list sort {stream} exceeds {limit} bytes").into_bytes(),
                ));
            }
            bytes.extend_from_slice(&buffer[..count]);
            Ok(true)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn collate(
    input: &[u8],
    cwd: &Path,
    interrupted: &AtomicBool,
) -> Result<(Vec<u8>, Vec<u8>), Failure> {
    if input.len() > MAX_BYTES {
        return Err(Failure::input_limit());
    }
    if interrupted.load(Ordering::Relaxed) {
        return Err(Failure::interrupted());
    }
    let deadline = Instant::now() + DEADLINE;
    // Fixed argv, stdin-only names, and inherited locale match the canonical
    // sort -u without an interpreter, shell expansion, or environment mutation.
    let mut child = Reap(
        Command::new("sort")
            .arg("-u")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut stdin = child.0.stdin.take();
    let mut stdout = child.0.stdout.take();
    let mut stderr = child.0.stderr.take();
    nonblocking(stdin.as_ref().unwrap())?;
    nonblocking(stdout.as_ref().unwrap())?;
    nonblocking(stderr.as_ref().unwrap())?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut written = 0;
    let mut status = None;
    loop {
        if interrupted.load(Ordering::Relaxed) {
            return Err(Failure::interrupted());
        }
        if Instant::now() >= deadline {
            return Err(Failure::new(
                124,
                b"agent-role list sort deadline exceeded (30 seconds)".to_vec(),
            ));
        }
        let mut progress = receive(&mut stdout, &mut out, MAX_BYTES, "stdout")?;
        progress |= receive(&mut stderr, &mut err, MAX_STDERR, "stderr")?;
        if written == input.len() {
            stdin.take();
        }
        if let Some(writer) = stdin.as_mut() {
            match writer.write(&input[written..input.len().min(written + 8192)]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "sort stdin write returned zero",
                    )
                    .into());
                }
                Ok(count) => {
                    written += count;
                    progress = true;
                }
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                    stdin.take();
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if let Some(status) = status
            && stdout.is_none()
            && stderr.is_none()
        {
            if !status.success() {
                let code = status
                    .code()
                    .unwrap_or_else(|| status.signal().map_or(1, |signal| 128 + signal));
                let mut message = format!("agent-role list sort failed (exit {code})").into_bytes();
                if !err.is_empty() {
                    message.extend_from_slice(b": ");
                    message.extend_from_slice(&err);
                }
                return Err(Failure::new(code, message));
            }
            if written != input.len()
                || out.contains(&0)
                || (!out.is_empty() && out.last() != Some(&b'\n'))
                || (!input.is_empty() && out.is_empty())
            {
                return Err(Failure::new(
                    1,
                    b"agent-role list sort returned an incomplete or malformed result".to_vec(),
                ));
            }
            return Ok((out, err));
        }
        if !progress {
            std::thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}
