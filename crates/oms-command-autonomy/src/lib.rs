//! Native intake, planning and bounded goal orchestration.
//!
//! A reviewed proposal is the only route from generated tasks to plan authority.
//! Provider and publication integrations are explicit [`Services`] callbacks;
//! missing integrations refuse instead of invoking a legacy implementation.
#![forbid(unsafe_code)]

pub mod adaptive;
pub mod advisory;
pub mod campaign;
mod campaign_usage;
mod cli;
pub mod conflicts;
pub mod contract;
pub mod discovery;
mod github;
pub mod goal;
pub mod handoff;
mod install;
pub mod intent;
mod parallel;
pub mod planning;
pub mod receipt;
pub mod runner;
pub mod selectors;
pub mod services;
mod storage;
pub mod value;
pub mod verify;

use cli::Args;
pub use conflicts::{CombinedOutcome, verify_combined};
pub use oms_command_plan::{completion, decision, retirement};
pub use oms_command_task::{
    consumers as task_consumers, observe as verify_observe, packet as handoff_packet,
    state_fingerprint,
};
pub use oms_work_inspect as work_inspect;
pub use services::{Invocation, JournalEvent, LocalServices, ModelRoute, Services};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
pub use value::{Error, Result};

pub type CommandResult = (i32, Vec<u8>, Vec<u8>);

pub fn owns(tool: &str, _args: &[OsString]) -> bool {
    matches!(
        tool,
        "intent" | "plan-from-spec" | "plan-run" | "autopilot" | "goal-drive" | "install-plan"
    )
}

pub fn run(tool: &str, args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> CommandResult {
    run_with_services(
        tool,
        args,
        cwd,
        interrupted,
        &LocalServices::new(interrupted),
    )
}

/// `args` excludes the public verb. The host must also route native child OMS
/// invocations used by its service implementation; there is no script fallback.
pub fn run_with_services(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    services: &dyn Services,
) -> CommandResult {
    let result = (|| {
        require(owns(tool, args), "unknown autonomy command")?;
        let mut args = Args::parse(tool, args)?;
        if args.flag("help") {
            return Ok(Output::text(cli::usage(tool)));
        }
        for name in [
            "repo",
            "apply",
            "restamp",
            "validate-spec",
            "proposal",
            "context-pack",
            "campaign",
            "wait-binding",
        ] {
            if let Some(raw) = args.values.get_mut(name)
                && !Path::new(raw).is_absolute()
            {
                *raw = cwd.join(&*raw).into_os_string();
            }
        }
        checkpoint(interrupted)?;
        let selected = args.path("repo", cwd);
        let repo = if tool == "intent" {
            std::fs::canonicalize(selected)
                .map_err(|_| Error::new(2, "cannot resolve intent repository"))?
        } else {
            services.work_repository(&selected, interrupted)?
        };
        let state = services.state_repository(&repo, interrupted)?;
        let context = storage::Context::new(repo, state, services, interrupted)?;
        match tool {
            "intent" => intent::execute(&context, &args),
            "plan-from-spec" => planning::execute(&context, &args),
            "plan-run" if args.flag("verify-combined") => conflicts::execute_cli(&context, &args),
            "plan-run" if args.flag("parallel") => {
                require(
                    !args.has("base-ref")
                        && !args.has("base-sha")
                        && !args.has("candidates")
                        && !args.has("verify-cmd"),
                    "--base-ref, --base-sha, --candidates and --verify-cmd require --verify-combined",
                )?;
                parallel::execute(&context, &args)
            }
            "plan-run" => {
                require(
                    !args.has("max-writers") && !args.has("max-review-backlog"),
                    "--max-writers and --max-review-backlog require --parallel",
                )?;
                require(
                    !args.has("base-ref")
                        && !args.has("base-sha")
                        && !args.has("candidates")
                        && !args.has("verify-cmd"),
                    "--base-ref, --base-sha, --candidates and --verify-cmd require --verify-combined",
                )?;
                runner::execute(&context, &args)
            }
            "autopilot" => {
                let wake = if matches!(args.action.as_str(), "run" | "reenter") {
                    adaptive::start_wake(&context)?
                } else {
                    None
                };
                let result = receipt::execute(&context, &args);
                if let Some(wake) = wake {
                    adaptive::finish_wake(
                        &context,
                        wake,
                        result.as_ref().map(|r| r.code).unwrap_or_else(|e| e.code),
                    )?;
                }
                result
            }
            "goal-drive" => goal::execute(&context, &args),
            "install-plan" => install::execute(&context, &args),
            _ => unreachable!(),
        }
    })();
    match result {
        Ok(out) => (out.code, out.stdout, out.stderr),
        Err(e) => (
            e.code,
            Vec::new(),
            format!("error: {}\n", e.message).into_bytes(),
        ),
    }
}

#[derive(Default)]
pub(crate) struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
impl Output {
    pub fn text(s: impl AsRef<str>) -> Self {
        Self {
            stdout: format!("{}\n", s.as_ref().trim_end()).into_bytes(),
            ..Self::default()
        }
    }
    pub fn json(v: &serde_json::Value) -> Result<Self> {
        Ok(Self {
            stdout: value::pretty(v)?,
            ..Self::default()
        })
    }
    pub fn append(&mut self, other: Self) {
        self.stdout.extend(other.stdout);
        self.stderr.extend(other.stderr);
        self.code = other.code;
    }
}
pub(crate) fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::new(2, message))
    }
}
pub(crate) fn checkpoint(interrupted: &AtomicBool) -> Result<()> {
    if interrupted.load(Ordering::Acquire) {
        Err(Error::new(130, "autonomy command interrupted"))
    } else {
        Ok(())
    }
}
