//! Owned native processes and the durable agent-supervisor command adapter.
#![forbid(unsafe_code)]
pub mod capture;
mod cli;
mod command;
mod lifecycle;
pub mod process;
mod storage;
pub use command::{CommandHost, run, run_streaming};
pub type Result<T> = std::result::Result<T, String>;

/// Parse-only classification of the internal runner command.
pub fn is_runner_invocation(args: &[std::ffi::OsString]) -> bool {
    matches!(cli::parse(args), Ok(cli::Parsed::Args(parsed)) if parsed.command == "_run-one")
}
