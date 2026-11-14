//! Callable compatibility surface for every landing-reference helper action.
//! argv excludes ACTION and REPO. The parent CLI owns routing and signals.
use crate::{
    Result,
    capture::{self, CaptureIndex},
    publication,
    scope::Protocol,
};
use std::{
    ffi::OsString, io::Write, os::unix::ffi::OsStrExt, path::Path, sync::atomic::AtomicBool,
};

/// `task-append` and `admission` are transaction legs: their callers retain
/// respectively the packet and artifact-index locks, as in the shell helper.
/// No missing monitor/identity producer is replaced with a successful result.
pub fn reference_action(
    action: &str,
    repo: &Path,
    args: &[OsString],
    protocol: Option<&Protocol>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    interrupted: &AtomicBool,
) -> i32 {
    let outcome = (|| {
        let repo = std::fs::canonicalize(repo).map_err(|e| e.to_string())?;
        let word = |position: usize| -> Result<&str> {
            args.get(position)
                .ok_or_else(|| "missing reference operation argument".into())
                .and_then(|v| {
                    v.to_str()
                        .ok_or_else(|| "invalid reference operation text".into())
                })
        };
        let path = |position: usize| -> Result<&Path> {
            args.get(position)
                .map(Path::new)
                .ok_or_else(|| "missing reference operation argument".into())
        };
        let arity = |minimum: usize, maximum: usize| -> Result<()> {
            if (minimum..=maximum).contains(&args.len()) {
                Ok(())
            } else {
                Err("invalid reference operation arguments".into())
            }
        };
        let protocol = || {
            protocol
                .ok_or_else(|| "native checked lifecycle/scope producer is unavailable".to_string())
        };
        match action {
            "begin" => {
                arity(1, 2)?;
                publication::begin(
                    &repo,
                    word(0)?,
                    args.get(1).map(Path::new),
                    stdout,
                    interrupted,
                )?;
            }
            "begin-owned" => {
                arity(3, 3)?;
                protocol()?.begin_owned(
                    &repo,
                    word(0)?,
                    path(1)?,
                    path(2)?,
                    stdout,
                    interrupted,
                )?;
            }
            "monitor" => {
                arity(2, 2)?;
                protocol()?.monitor(&repo, path(0)?, word(1)?, interrupted)?;
            }
            "await-monitor" => {
                arity(3, 3)?;
                let proof =
                    protocol()?.await_monitor(&repo, path(0)?, word(1)?, word(2)?, interrupted)?;
                writeln!(stdout, "{proof}")
                    .and_then(|()| stdout.flush())
                    .map_err(|e| e.to_string())?;
            }
            "stop-monitor" => {
                arity(2, 3)?;
                return protocol()?.stop_monitor(
                    &repo,
                    path(0)?,
                    word(1)?,
                    args.get(2).and_then(|v| v.to_str()).unwrap_or_default(),
                    stderr,
                    interrupted,
                );
            }
            "close-scope" => {
                arity(2, 2)?;
                protocol()?.close_scope(&repo, path(0)?, word(1)?, interrupted)?;
            }
            "finish" => {
                arity(2, 2)?;
                publication::finish(&repo, word(0)?, word(1)?, interrupted)?;
            }
            "task-append" => {
                arity(6, 6)?;
                let index = CaptureIndex::load(&repo)?;
                publication::append_deduplicated_unlocked(
                    &index,
                    word(0)?,
                    path(1)?,
                    word(2)?,
                    args[3].as_bytes(),
                    path(4)?,
                    args[5].as_bytes(),
                    std::env::var_os("OMS_AGENT_TASK_SOURCE_SESSION")
                        .as_deref()
                        .map(|v| v.as_bytes())
                        .unwrap_or_default(),
                    interrupted,
                )?;
            }
            "admission" => {
                arity(4, 4)?;
                capture::record_admission(
                    &CaptureIndex::load(&repo)?,
                    path(0)?,
                    word(1)?,
                    path(2)?,
                    word(3)?,
                )?;
            }
            "task" => {
                arity(1, usize::MAX)?;
                CaptureIndex::load(&repo)?.validate(
                    path(0)?,
                    &args[1..]
                        .iter()
                        .map(|v| v.as_bytes().to_vec())
                        .collect::<Vec<_>>(),
                )?;
            }
            "admit" => {
                arity(2, 2)?;
                let known = CaptureIndex::load(&repo)?.admit(path(0)?, path(1)?)?;
                writeln!(stdout, "{}", if known { 1 } else { 0 }).map_err(|e| e.to_string())?;
            }
            _ => return Err("unsupported reference operation".into()),
        }
        Ok(0)
    })();
    match outcome {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(stderr, "landing-reference: {error}");
            2
        }
    }
}
