//! Native installation, host inspection, and reversible lifecycle transactions.
#![forbid(unsafe_code)]

mod cli;
mod config;
mod doctor;
mod effects;
mod fingerprint;
mod install;
mod integrations;
mod mcp_probe;
mod payload;
mod plugins;
mod provider_check;
mod receipt;
mod runtime;
mod scheduler;
mod services;
mod settings;
mod snapshot;
mod tick;
mod tools;
mod transaction;
mod update;

pub use services::{Services, UnavailableServices};
use std::{ffi::OsString, path::Path, sync::atomic::AtomicBool};
pub type Output = (i32, Vec<u8>, Vec<u8>);

/// Pure routing, including malformed arguments for commands owned here.
pub fn owns(tool: &str, _args: &[OsString]) -> bool {
    matches!(
        tool,
        "doctor"
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
    match cli::execute(tool, args, cwd, source, interrupted, services) {
        Ok(output) => output,
        Err(runtime::Error(code, message)) => {
            (code, Vec::new(), format!("error: {message}\n").into_bytes())
        }
    }
}
