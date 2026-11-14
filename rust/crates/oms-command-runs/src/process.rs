use crate::{Context, Error, Result};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Child-local execution controls. Defaults preserve `execute` behavior.
#[derive(Clone, Debug, Default)]
pub struct ProcessPolicy {
    pub env_clear: bool,
    pub env: BTreeMap<OsString, OsString>,
    pub null_stdin: bool,
    /// Inherited output is neither captured nor hashed; the empty hash is returned.
    pub inherit_stdio: bool,
    pub rlimit_as: Option<u64>,
    /// Cancellation is enabled only by an explicit directory, never by child env.
    pub cancel_dir: Option<PathBuf>,
    /// Optional `Services::rlimit_helper()` result, using `LIMIT COMMAND...` argv.
    /// Context deliberately carries no service reference or process-global state.
    pub rlimit_helper: Option<PathBuf>,
}
impl ProcessPolicy {
    fn variable(&self, ctx: &Context<'_>, name: &str) -> Option<OsString> {
        self.env
            .get(OsStr::new(name))
            .or_else(|| ctx.env.get(OsStr::new(name)))
            .cloned()
            .or_else(|| (!self.env_clear).then(|| std::env::var_os(name)).flatten())
    }
    fn helper(&self, ctx: &Context<'_>) -> Result<PathBuf> {
        fn executable(path: PathBuf) -> Option<PathBuf> {
            let path = path.canonicalize().ok()?;
            if !path.is_file() || rustix::fs::access(&path, rustix::fs::Access::EXEC_OK).is_err() {
                return None;
            }
            Some(path)
        }
        if let Some(path) = &self.rlimit_helper {
            return executable(ctx.cwd.join(path))
                .ok_or_else(|| Error::io("configured RLIMIT_AS helper is not executable"));
        }
        let sibling = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("oms-run-bounded-helper")));
        let paths = self.variable(ctx, "PATH");
        sibling
            .into_iter()
            .chain(
                paths
                    .iter()
                    .flat_map(std::env::split_paths)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join("oms-run-bounded-helper")),
            )
            .find_map(executable)
            .ok_or_else(|| Error::io("RLIMIT_AS requires an executable oms-run-bounded-helper"))
    }
}

fn validate_rlimit(limit: u64) -> Result<()> {
    let hard = rustix::process::getrlimit(rustix::process::Resource::As).maximum;
    if limit == 0 || hard.is_some_and(|hard| limit > hard) {
        return Err(Error::usage(
            "RLIMIT_AS bytes must be positive and not exceed the hard limit",
        ));
    }
    Ok(())
}

/// Permanently lowers this process's address-space limit and replaces it with
/// the target. Call only from a dedicated helper, never an embedded CLI thread.
pub fn run_rlimit_helper_cli() -> ! {
    fn run() -> Result<()> {
        let mut args = std::env::args_os().skip(1);
        let raw = args
            .next()
            .ok_or_else(|| Error::usage("usage: oms-run-bounded-helper BYTES COMMAND..."))?;
        let raw = raw
            .to_str()
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| Error::usage("invalid RLIMIT_AS bytes"))?;
        let limit = raw
            .parse::<u64>()
            .map_err(|_| Error::usage("invalid RLIMIT_AS bytes"))?;
        validate_rlimit(limit)?;
        let target = args
            .next()
            .ok_or_else(|| Error::usage("usage: oms-run-bounded-helper BYTES COMMAND..."))?;
        rustix::process::setrlimit(
            rustix::process::Resource::As,
            rustix::process::Rlimit {
                current: Some(limit),
                maximum: Some(limit),
            },
        )
        .map_err(|_| Error::io("cannot apply child RLIMIT_AS"))?;
        let error = Command::new(target).args(args).exec();
        Err(Error {
            code: if error.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            },
            message: format!("cannot execute bounded command: {error}"),
        })
    }
    let error = run().expect_err("exec never returns on success");
    eprintln!("error: {error}");
    std::process::exit(error.code);
}

fn cancel_requested(directory: Option<&Path>) -> bool {
    use crate::value::{get, number, parse, text};
    let Some(directory) = directory else {
        return false;
    };
    let Some(task) = directory.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    oms_safe_fs::Root::open(directory)
        .ok()
        .and_then(|root| {
            root.read_manifest(Path::new("cancel-request.json"), 4096)
                .ok()
        })
        .and_then(|bytes| parse(&bytes).ok())
        .is_some_and(|row| {
            get(&row, "schema").and_then(number) == Some(1.0)
                && get(&row, "task_id").and_then(text) == Some(task)
                && get(&row, "kind").and_then(text) == Some("mcp-task-cancel-request")
        })
}
fn acknowledge_cancel(directory: &Path) -> Result<()> {
    use crate::value::{encode, num, object, string};
    use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
    let task = directory
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| Error::io("invalid cancellation directory"))?;
    let root = Root::open(directory).map_err(|_| Error::io("opening cancellation directory"))?;
    // Preserve the shared Python/MCP observation receipt as well as completion.
    for name in ["cancel-observed", "cancelled"] {
        let row = object(vec![
            ("schema", num(1)),
            ("task_id", string(task)),
            ("kind", string(format!("mcp-task-{name}"))),
        ]);
        let mut body = encode(&row, false, false)?;
        body.push(b'\n');
        root.atomic_mutate(
            Path::new(&format!("{name}.json")),
            MutationOptions {
                read: ByteReadOptions {
                    missing_ok: true,
                    max_bytes: 4096,
                },
                max_output: 4096,
                ..MutationOptions::default()
            },
            |_| -> Result<Vec<u8>> { Ok(body) },
        )
        .map_err(|e| {
            Error::io(format!(
                "cancellation handled but receipt publication failed: {e:?}"
            ))
        })?;
    }
    Ok(())
}

fn child_status(child: &Child) -> Result<Option<rustix::process::WaitIdStatus>> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    let pid =
        Pid::from_raw(child.id() as i32).ok_or_else(|| Error::io("invalid child identity"))?;
    // Retain the zombie until group cleanup finishes, preventing PID reuse.
    waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .map_err(|_| Error::io("waiting for child"))
}

pub struct Output {
    pub code: i32,
    pub bytes: Vec<u8>,
    /// Query diagnostics or spawn errors. Helper-backed non-query exits 126/127
    /// also retain up to 64 KiB while streaming to the supplied stderr writer.
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
    execute_with_policy(
        ctx,
        argv,
        &ProcessPolicy::default(),
        query,
        capture,
        timeout,
        out,
        err,
    )
}
/// Executes with child-local policy using the same group lifetime and bounded
/// stream handling as `execute`. Cancellation receipts return exit 130.
pub fn execute_with_policy(
    ctx: &Context<'_>,
    argv: &[OsString],
    policy: &ProcessPolicy,
    query: bool,
    capture: usize,
    timeout: Option<Duration>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<Output> {
    execute_mode(
        ctx, argv, policy, query, capture, timeout, out, err, false, None,
    )
}
/// Executes with child-local policy and invokes `on_spawn` with the OS PID
/// as soon as the child process is spawned and armed under `Lifetime`.
pub fn execute_with_hooks(
    ctx: &Context<'_>,
    argv: &[OsString],
    policy: &ProcessPolicy,
    query: bool,
    capture: usize,
    timeout: Option<Duration>,
    out: &mut dyn Write,
    err: &mut dyn Write,
    on_spawn: Option<&mut dyn FnMut(u32)>,
) -> Result<Output> {
    execute_mode(
        ctx, argv, policy, query, capture, timeout, out, err, false, on_spawn,
    )
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
        &ProcessPolicy::default(),
        false,
        0,
        timeout,
        out,
        &mut std::io::sink(),
        true,
        None,
    )
}
fn execute_mode(
    ctx: &Context<'_>,
    argv: &[OsString],
    policy: &ProcessPolicy,
    query: bool,
    capture: usize,
    timeout: Option<Duration>,
    out: &mut dyn Write,
    err: &mut dyn Write,
    merged: bool,
    on_spawn: Option<&mut dyn FnMut(u32)>,
) -> Result<Output> {
    if argv.is_empty() {
        return Err(Error::usage("empty command"));
    }
    if policy
        .cancel_dir
        .as_ref()
        .is_some_and(|p| p.as_os_str().is_empty())
    {
        return Err(Error::usage("empty cancel directory"));
    }
    ctx.check()?;
    let mut command = if let Some(limit) = policy.rlimit_as {
        validate_rlimit(limit)?;
        let mut command = Command::new(policy.helper(ctx)?);
        command.arg(limit.to_string()).arg(&argv[0]);
        command
    } else {
        Command::new(&argv[0])
    };
    let cancel_dir = policy.cancel_dir.as_ref().map(|p| ctx.cwd.join(p));
    if cancel_requested(cancel_dir.as_deref()) {
        acknowledge_cancel(cancel_dir.as_deref().unwrap())?;
        return Ok(Output {
            code: 130,
            bytes: vec![],
            stderr: vec![],
            hash: crate::value::hash(b""),
            submitted: String::new(),
        });
    }
    command
        .args(&argv[1..])
        .current_dir(&ctx.cwd)
        .process_group(0);
    if policy.env_clear {
        command.env_clear();
    }
    command.envs(&ctx.env).envs(&policy.env);
    let mut streams = if policy.inherit_stdio {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        [None, None]
    } else {
        let (or, ow) = nix::unistd::pipe().map_err(|_| Error::io("pipe"))?;
        let (er, ew) = if merged {
            (None, ow.try_clone().map_err(|_| Error::io("pipe clone"))?)
        } else {
            let (r, w) = nix::unistd::pipe().map_err(|_| Error::io("pipe"))?;
            (Some(File::from(r)), w)
        };
        command.stdout(Stdio::from(ow)).stderr(Stdio::from(ew));
        [Some(File::from(or)), er]
    };
    command.stdin(if query || policy.null_stdin {
        Stdio::null()
    } else {
        Stdio::inherit()
    });
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
    if let Some(callback) = on_spawn {
        callback(child.0.id());
    }
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
    let mut cancelled = false;
    let mut stop_at = None;
    let mut write_error = false;
    loop {
        if status.is_none() {
            status = child_status(&child.0)?;
        }
        if status.is_some() && streams.iter().all(Option::is_none) {
            break;
        }
        if status.is_none() && stop.is_none() {
            if cancel_requested(cancel_dir.as_deref()) {
                // A completed leader wins a cancellation that arrived after exit.
                status = child_status(&child.0)?;
                if status.is_none() {
                    stop = Some(130);
                    cancelled = true;
                }
            }
            if status.is_none() && stop.is_none() {
                if ctx.interrupted() {
                    stop = Some(ctx.signal_code());
                } else if timeout.is_some_and(|t| started.elapsed() >= t) {
                    stop = Some(124);
                }
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
                        if bytes.len().saturating_add(n) > capture && capture > 0 && !cancelled {
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
                    } else if query || policy.rlimit_as.is_some() {
                        if query && diagnostics.len().saturating_add(n) > 65536 && !cancelled {
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
                            if !cancelled {
                                stop = Some(141);
                                stop_at.get_or_insert_with(Instant::now);
                            }
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
        if status.is_none() && streams.iter().all(Option::is_none) {
            status = child_status(&child.0)?;
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
    if !query && !matches!(code, 126 | 127) {
        diagnostics.clear();
    }
    // Stop/reap before publishing cancellation completion, including descendants
    // that closed the pipes or ignored TERM in inherited-stdio mode.
    drop(child);
    if cancelled {
        acknowledge_cancel(cancel_dir.as_deref().unwrap())?;
    }
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
