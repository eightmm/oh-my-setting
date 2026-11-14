use crate::{Context, Error, Result};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Output {
    pub code: i32,
    pub bytes: Vec<u8>,
    pub stderr: Vec<u8>,
    pub hash: String,
    pub submitted: String,
}
struct Lifetime(Child);
impl Drop for Lifetime {
    fn drop(&mut self) {
        let pid = nix::unistd::Pid::from_raw(self.0.id() as i32);
        let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL);
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn nonblock(f: &File) -> Result<()> {
    let flags = fcntl_getfl(f).map_err(|_| Error::io("pipe flags"))?;
    fcntl_setfl(f, flags | OFlags::NONBLOCK).map_err(|_| Error::io("pipe flags"))?;
    Ok(())
}
/// Drains both streams without retaining user output. Capture and line parsing are bounded.
pub fn execute(
    ctx: &Context<'_>,
    argv: &[OsString],
    query: bool,
    capture: usize,
    timeout: Option<Duration>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<Output> {
    execute_mode(ctx, argv, query, capture, timeout, out, err, false)
}
pub fn merged(
    ctx: &Context<'_>,
    argv: &[OsString],
    timeout: Option<Duration>,
    out: &mut dyn Write,
) -> Result<Output> {
    execute_mode(
        ctx,
        argv,
        false,
        0,
        timeout,
        out,
        &mut std::io::sink(),
        true,
    )
}
fn execute_mode(
    ctx: &Context<'_>,
    argv: &[OsString],
    query: bool,
    capture: usize,
    timeout: Option<Duration>,
    out: &mut dyn Write,
    err: &mut dyn Write,
    merged: bool,
) -> Result<Output> {
    if argv.is_empty() {
        return Err(Error::usage("empty command"));
    }
    ctx.check()?;
    let (or, ow) = nix::unistd::pipe().map_err(|_| Error::io("pipe"))?;
    let (er, ew) = if merged {
        (None, ow.try_clone().map_err(|_| Error::io("pipe clone"))?)
    } else {
        let (r, w) = nix::unistd::pipe().map_err(|_| Error::io("pipe"))?;
        (Some(File::from(r)), w)
    };
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&ctx.cwd)
        .envs(ctx.env.iter())
        .stdout(Stdio::from(ow))
        .stderr(Stdio::from(ew))
        .process_group(0);
    if query {
        command.stdin(Stdio::null());
    } else {
        command.stdin(Stdio::inherit());
    }
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(Output {
                code: if e.kind() == std::io::ErrorKind::NotFound {
                    127
                } else {
                    126
                },
                bytes: vec![],
                stderr: e.to_string().into_bytes(),
                hash: crate::value::hash(b""),
                submitted: String::new(),
            });
        }
    };
    drop(command);
    let child = Lifetime(child);
    let mut streams = [Some(File::from(or)), er];
    for f in streams.iter().flatten() {
        nonblock(f)?;
    }
    let started = Instant::now();
    let mut status = None;
    let mut bytes = vec![];
    let mut diagnostics = vec![];
    let mut sha = Sha256::new();
    let mut line = vec![];
    let mut line_large = false;
    let mut submitted = String::new();
    let mut stop = None;
    let mut stop_at = None;
    let mut write_error = false;
    loop {
        if stop.is_none() {
            if ctx.interrupted() {
                stop = Some(ctx.signal_code());
            } else if timeout.is_some_and(|t| started.elapsed() >= t) {
                stop = Some(124);
            }
            if stop.is_some() {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(child.0.id() as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
                stop_at = Some(Instant::now());
            }
        }
        if stop_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(250)) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(child.0.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let mut progress = false;
        for (index, slot) in streams.iter_mut().enumerate() {
            let Some(f) = slot else { continue };
            let mut block = [0; 16384];
            // One block per stream keeps an unbounded stdout producer from starving stderr/deadlines.
            match f.read(&mut block) {
                Ok(0) => {
                    *slot = None;
                }
                Ok(n) => {
                    progress = true;
                    let data = &block[..n];
                    if index == 0 {
                        sha.update(data);
                        if bytes.len().saturating_add(n) > capture && capture > 0 {
                            stop = Some(2);
                            stop_at.get_or_insert_with(Instant::now);
                        }
                        if capture > 0 {
                            let take = (capture - bytes.len()).min(n);
                            bytes.extend_from_slice(&data[..take]);
                        }
                        if !query {
                            for &b in data {
                                if b == b'\n' {
                                    if !line_large {
                                        parse_submission(&line, &mut submitted);
                                    }
                                    line.clear();
                                    line_large = false;
                                } else if line.len() < 65536 {
                                    line.push(b);
                                } else {
                                    line_large = true;
                                }
                            }
                        }
                    } else if query {
                        if diagnostics.len().saturating_add(n) > 65536 {
                            stop = Some(2);
                            stop_at.get_or_insert_with(Instant::now);
                        }
                        let take = (65536 - diagnostics.len()).min(n);
                        diagnostics.extend_from_slice(&data[..take]);
                    }
                    if !query && !write_error {
                        let write = if index == 0 {
                            out.write_all(data)
                        } else {
                            err.write_all(data)
                        };
                        if write.is_err() {
                            write_error = true;
                            stop = Some(141);
                            stop_at.get_or_insert_with(Instant::now);
                        }
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::Interrupted =>
                {
                    ()
                }
                Err(_) => return Err(Error::io("reading child output")),
            }
        }
        if status.is_none() {
            use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
            let pid = Pid::from_raw(child.0.id() as i32)
                .ok_or_else(|| Error::io("invalid child identity"))?;
            // Retain the zombie until pipe draining and group cleanup finish, preventing PID reuse.
            status = waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )
            .map_err(|_| Error::io("waiting for child"))?;
        }
        if status.is_some()
            && (streams.iter().all(Option::is_none)
                || stop_at.is_some_and(|t| t.elapsed() >= Duration::from_secs(1)))
        {
            break;
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if !line_large {
        parse_submission(&line, &mut submitted);
    }
    if stop == Some(2) {
        return Err(Error::usage(
            "scheduler or metadata output exceeds its byte limit",
        ));
    }
    let code = stop.unwrap_or_else(|| {
        let s = status.unwrap();
        s.exit_status()
            .unwrap_or_else(|| 128 + s.terminating_signal().unwrap_or(1))
    });
    Ok(Output {
        code,
        bytes,
        stderr: diagnostics,
        hash: format!("{:x}", sha.finalize()),
        submitted,
    })
}
fn parse_submission(line: &[u8], out: &mut String) {
    let line = line.strip_prefix(b"Submitted batch job ").unwrap_or(line);
    if !line.is_empty() && line.iter().all(u8::is_ascii_digit) {
        *out = String::from_utf8(line.to_vec()).unwrap();
    }
}
pub fn query(ctx: &Context<'_>, argv: &[OsString], limit: usize) -> Result<Output> {
    execute(
        ctx,
        argv,
        true,
        limit,
        Some(ctx.query_timeout),
        &mut std::io::sink(),
        &mut std::io::sink(),
    )
}
pub fn git(ctx: &Context<'_>, args: &[&str], limit: usize) -> Result<Output> {
    let argv = std::iter::once(OsString::from("git"))
        .chain(args.iter().map(OsString::from))
        .collect::<Vec<_>>();
    query(ctx, &argv, limit)
}
pub fn git_hash(ctx: &Context<'_>, args: &[&str]) -> Result<String> {
    let argv = std::iter::once(OsString::from("git"))
        .chain(args.iter().map(OsString::from))
        .collect::<Vec<_>>();
    let r = execute(
        ctx,
        &argv,
        true,
        0,
        Some(ctx.query_timeout),
        &mut std::io::sink(),
        &mut std::io::sink(),
    )?;
    if r.code != 0 {
        return Err(Error::io("git diff query failed"));
    }
    Ok(r.hash)
}
