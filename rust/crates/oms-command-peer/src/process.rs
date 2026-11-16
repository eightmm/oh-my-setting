//! Execution uses the supervisor reactor's owned child, cancellation and drainage.
use crate::{Error, Result};
use oms_supervisor::process::{Progress, Reactor, Request, Scope, StopReason, Streams};
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Invocation {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub input: Option<Vec<u8>>,
    pub environment: Vec<(OsString, OsString)>,
    pub remove_environment: Vec<OsString>,
    pub timeout: Duration,
    pub max_bytes: usize,
    pub merge: bool,
}
#[derive(Clone, Debug)]
pub struct Completed {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub complete: bool,
    pub elapsed: Duration,
}
pub trait Executor: Sync {
    fn execute(&self, request: Invocation, interrupted: &AtomicBool) -> Result<Completed>;
}
pub struct Native;
impl Executor for Native {
    fn execute(&self, input: Invocation, interrupted: &AtomicBool) -> Result<Completed> {
        crate::check(interrupted)?;
        let mut request = Request::new(input.argv);
        request.cwd = Some(input.cwd);
        request.stdin = input.input;
        request.environment = input.environment;
        request.remove_environment = input.remove_environment;
        request.streams = if input.merge {
            Streams::Merged
        } else {
            Streams::Separate
        };
        #[cfg(target_os = "linux")]
        let (scope, mut reactor) = match Reactor::dedicated() {
            Ok(dedicated) => (Scope::WholeTree, dedicated),
            Err(_) => (Scope::ProcessGroup, Reactor::new()),
        };
        #[cfg(not(target_os = "linux"))]
        let (scope, mut reactor) = (Scope::ProcessGroup, Reactor::new());
        request.scope = scope;
        request.deadline = Some(
            Instant::now()
                .checked_add(input.timeout)
                .ok_or_else(|| Error::new(2, "execution deadline overflow"))?,
        );
        request.capture.max_bytes = input.max_bytes;
        let job = reactor
            .start(request)
            .map_err(|e| Error::new(125, format!("process could not start: {e}")))?;
        let mut cancelled = false;
        loop {
            if interrupted.load(Ordering::Acquire) && !cancelled {
                reactor.stop(job, StopReason::Cancelled)?;
                cancelled = true;
            }
            match reactor.poll(job, Duration::from_millis(50))? {
                Progress::Pending(_) => {}
                Progress::Complete(result) => {
                    let code = match result.stop {
                        Some(StopReason::Deadline) => 124,
                        Some(StopReason::Cancelled) => 130,
                        Some(StopReason::OutputLimit | StopReason::SupervisorError) => 125,
                        _ => result.exit.map_or(125, |e| e.shell_code()),
                    };
                    let complete = result.stop.is_none()
                        && result.output_complete()
                        && result.cleanup_complete()
                        && result.supervision_error.is_none();
                    return Ok(Completed {
                        code,
                        stdout: result.stdout.bytes,
                        stderr: result.stderr.map_or_else(Vec::new, |s| s.bytes),
                        complete,
                        elapsed: result.duration,
                    });
                }
            }
        }
    }
}
pub(crate) fn invoke(
    context: &crate::Context<'_>,
    cwd: &std::path::Path,
    argv: Vec<OsString>,
    input: Option<Vec<u8>>,
    timeout: u64,
    merge: bool,
) -> Result<Completed> {
    invoke_env(context, cwd, argv, input, Vec::new(), timeout, merge)
}

pub(crate) fn invoke_env(
    context: &crate::Context<'_>,
    cwd: &std::path::Path,
    argv: Vec<OsString>,
    input: Option<Vec<u8>>,
    environment: Vec<(OsString, OsString)>,
    timeout: u64,
    merge: bool,
) -> Result<Completed> {
    context.executor.execute(
        Invocation {
            argv,
            cwd: cwd.to_owned(),
            input,
            environment,
            remove_environment: Vec::new(),
            timeout: Duration::from_secs(timeout),
            max_bytes: 64 * 1024 * 1024,
            merge,
        },
        context.interrupted,
    )
}
pub(crate) fn git(
    context: &crate::Context<'_>,
    cwd: &std::path::Path,
    args: &[&str],
) -> Result<Vec<u8>> {
    git_input(context, cwd, args, None)
}
pub(crate) fn git_input(
    context: &crate::Context<'_>,
    cwd: &std::path::Path,
    args: &[&str],
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut argv: Vec<OsString> = [
        "git",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "diff.external=",
        "-c",
        "core.attributesFile=/dev/null",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    argv.extend(args.iter().map(OsString::from));
    let remove_environment = std::env::vars_os()
        .filter_map(|(key, _)| {
            key.to_str()
                .is_some_and(|s| s.starts_with("GIT_"))
                .then_some(key)
        })
        .collect();
    let result = context.executor.execute(
        Invocation {
            argv,
            cwd: cwd.to_owned(),
            input,
            environment: vec![
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ],
            remove_environment,
            timeout: Duration::from_secs(30),
            max_bytes: 64 * 1024 * 1024,
            merge: false,
        },
        context.interrupted,
    )?;
    if result.code != 0 || !result.complete {
        return crate::fail("git operation failed or output was incomplete");
    }
    Ok(result.stdout)
}
