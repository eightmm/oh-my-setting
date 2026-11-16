//! Managed terminal, workspace and persistent job subcommands.
use crate::{Error, Output, Result, Services, cli::Options, json, parent_only, require};
use oms_panel_core::{Value, jobs, state, terminals, workspaces};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Dispatch `oms panel terminal ...` and `oms panel job ...` actions through panel-core.
pub fn run(
    options: &Options,
    cwd: &Path,
    repo: &Path,
    source: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let action = options.action.as_deref().unwrap_or("");
    let is_job = options.command.as_deref() == Some("job");
    let ident = || -> Result<&str> {
        options
            .ident
            .as_deref()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::new("managed record ID required"))
    };
    let job_ident = || -> Result<&str> {
        options
            .get("id")
            .or(options.ident.as_deref())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::new("managed job ID required"))
    };
    let session = || -> Result<String> {
        options
            .get("session")
            .map(str::to_owned)
            .or_else(|| {
                std::env::var("OMS_PANEL_SESSION")
                    .ok()
                    .filter(|v| !v.is_empty())
            })
            .ok_or_else(|| Error::new("managed panel session required"))
    };
    let generation_required = || -> Result<u64> {
        require(options.has("generation"), "--generation is required")?;
        Ok(options.integer("generation", 1, 1, usize::MAX)? as u64)
    };
    let generation_optional = || -> Result<Option<u64>> {
        if options.has("generation") {
            Ok(Some(options.integer("generation", 1, 1, usize::MAX)? as u64))
        } else {
            Ok(None)
        }
    };
    let maximum = options.integer("maximum", 32768, 1, 32768)?;

    if is_job && action == "_supervise" {
        let g = generation_required()?;
        jobs::supervise_job(repo, job_ident()?, g)?;
        return Ok((0, vec![], vec![]));
    }

    let result = if is_job {
        match action {
            "list" => Value::Array(jobs::list_public_jobs(repo, options.has("archived"))?),
            "logs" => jobs::read_job_logs(repo, ident()?, generation_optional()?, maximum)?,
            "run" => {
                parent_only()?;
                let working = resolve_cwd(cwd, repo, options)?;
                let executable = job_executable(source)?;
                jobs::spawn_job(
                    repo,
                    &session()?,
                    options.argv.clone(),
                    &options.text("label", "Job"),
                    working.as_deref(),
                    services,
                    stop,
                    &executable,
                )?
            }
            "restart" => {
                parent_only()?;
                let g = generation_required()?;
                let executable = job_executable(source)?;
                jobs::spawn_restart_job(
                    repo,
                    &session()?,
                    ident()?,
                    g,
                    services,
                    stop,
                    &executable,
                )?
            }
            "interrupt" | "stop" => {
                parent_only()?;
                let g = generation_required()?;
                jobs::request_job_control(repo, ident()?, g, action)?
            }
            "forget" => {
                parent_only()?;
                jobs::forget_job(repo, ident()?, generation_required()?)?
            }
            "archive" => {
                parent_only()?;
                jobs::archive_job(repo, ident()?, generation_optional()?)?
            }
            "restore" => {
                parent_only()?;
                jobs::restore_job(repo, ident()?, generation_optional()?)?
            }
            _ => return Err(Error::new("unsupported managed job action")),
        }
    } else {
        match action {
            "list" => {
                parent_only()?;
                Value::Array(terminals::list_terminals(
                    repo,
                    options.has("archived"),
                    services,
                    stop,
                )?)
            }
            "workspaces" => Value::Array(workspaces::list_workspaces(
                repo,
                options.has("archived"),
                services,
                stop,
            )?),
            "open" => {
                parent_only()?;
                let _ = resolve_cwd(cwd, repo, options)?;
                let row = terminals::open_terminal(
                    repo,
                    &session()?,
                    &options.text("label", "Terminal"),
                    services,
                    stop,
                )?;
                terminals::public_terminal(repo, &row, services, stop)
            }
            "controls" => {
                let row = workspaces::open_workspace(repo, &session()?, services, stop)?;
                workspaces::public_workspace(repo, &row, services, stop)
            }
            "read" => terminals::read_terminal(repo, ident()?, maximum, services, stop)?,
            "focus" => terminals::focus_terminal(repo, ident()?, services, stop)?,
            "stop" => terminals::stop_terminal(repo, ident()?, services, stop)?,
            "workspace-focus" => workspaces::focus(repo, ident()?, services, stop)?,
            "forget" => {
                parent_only()?;
                terminals::forget_terminal(repo, ident()?, services, stop)?
            }
            "archive" => {
                parent_only()?;
                terminals::archive_terminal(repo, ident()?, services, stop)?
            }
            "restore" => {
                parent_only()?;
                terminals::restore_terminal(repo, ident()?, services, stop)?
            }
            "workspace-forget" => {
                parent_only()?;
                workspaces::forget_workspace(repo, ident()?, services, stop)?
            }
            "workspace-archive" => {
                parent_only()?;
                workspaces::archive_workspace(repo, ident()?, services, stop)?
            }
            "workspace-restore" => {
                parent_only()?;
                workspaces::restore_workspace(repo, ident()?, services, stop)?
            }
            _ => return Err(Error::new("unsupported managed terminal action")),
        }
    };
    Ok((0, json(&result)?, vec![]))
}

fn resolve_cwd(cwd: &Path, repo: &Path, options: &Options) -> Result<Option<PathBuf>> {
    let Some(raw) = options.get("cwd") else {
        return Ok(None);
    };
    let raw_path = Path::new(raw);
    let resolved = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        cwd.join(raw_path)
    };
    let canonical = std::fs::canonicalize(&resolved).map_err(Error::new)?;
    let working_repo = state::repository(&canonical)?;
    require(
        working_repo == repo,
        "managed working directory differs from repository",
    )?;
    Ok(Some(canonical))
}

fn job_executable(source: &Path) -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("OMS_PANEL_JOB_EXECUTABLE")
        && !explicit.is_empty()
    {
        let path = PathBuf::from(explicit);
        require(
            crate::executable::job_override(&path),
            "oms supervisor executable unavailable",
        )?;
        return Ok(path);
    }
    crate::executable::resolve_for_job(source)
        .map(|entry| entry.path)
        .ok_or_else(|| Error::new("oms supervisor executable unavailable"))
}

#[cfg(test)]
pub(crate) fn test_job_executable(source: &Path) -> Result<PathBuf> {
    job_executable(source)
}
