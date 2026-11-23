//! Native installation, host inspection, and reversible lifecycle transactions.
#![forbid(unsafe_code)]

mod account_cleanup;
mod agy_hook;
mod agy_surface;
mod cli;
mod config;
mod doctor;
mod effects;
mod fingerprint;
mod install;
mod integrations;
mod lifecycle_lock;
mod mcp_probe;
mod payload;
mod plugins;
mod prepared;
mod provider_check;
mod receipt;
mod runtime;
mod scheduler;
mod services;
mod settings;
mod snapshot;
mod supply;
mod tick;
mod tools;
mod transaction;
mod update;

pub use services::{
    IncomingDiagnostic, IncomingPhase, IncomingSourceLease, IncomingTool, Services,
    UnavailableServices,
};
use std::{ffi::OsString, path::Path, sync::atomic::AtomicBool};
pub type Output = (i32, Vec<u8>, Vec<u8>);

/// Tracking policy for an already resolved release commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseChannel {
    Stable,
    Edge,
}

/// Pure routing, including malformed arguments for commands owned here.
pub fn owns(tool: &str, _args: &[OsString]) -> bool {
    matches!(
        tool,
        "doctor"
            | "agy-hook"
            | "install"
            | "link"
            | "install-profile"
            | "install-tools"
            | "install-hooks"
            | "install-claude-hooks"
            | "install-mcp"
            | "install-codex-plugin"
            | "install-agy-plugin"
            | "install-skills"
            | "install-autoupdate"
            | "update"
            | "auto-update"
            | "uninstall"
            | "uninstall-autoupdate"
            | "unlink"
            | "status"
            | "snapshot"
            | "write-machine-snapshot"
            | "tick"
    )
}

/// No process-global cwd, environment, signal handler, or interpreter changes.
/// The caller supplies the reviewed source root; persisted state grants no
/// authority to execute programs from a different checkout.
pub fn run_with_source(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
) -> Output {
    run_with_services(tool, args, cwd, source, interrupted, &UnavailableServices)
}

/// Inject cross-domain native tick producers without changing process state.
pub fn run_with_services(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
    services: &dyn Services,
) -> Output {
    output(cli::execute(tool, args, cwd, source, interrupted, services))
}

/// Apply exactly the reviewed release commit with the normal update transaction.
/// Edge must still match its fetched ref and remains subscribed to that ref;
/// stable retains an explicit commit pin. The reviewed target is bound without
/// changing process-global state or adding a public CLI override.
pub fn apply_release_with_services(
    channel: ReleaseChannel,
    target: &str,
    selected_ref: &str,
    source: &Path,
    no_tools: bool,
    interrupted: &AtomicBool,
    services: &dyn Services,
) -> Output {
    output((|| {
        runtime::check(interrupted)?;
        runtime::native_host()?;
        let paths = receipt::Paths::new(source, source)?;
        update::release(
            &paths,
            channel,
            target,
            selected_ref,
            no_tools,
            interrupted,
            services,
        )
    })())
}

fn output(result: runtime::Result<Output>) -> Output {
    match result {
        Ok(output) => output,
        Err(runtime::Error(code, message)) => {
            (code, Vec::new(), format!("error: {message}\n").into_bytes())
        }
    }
}
