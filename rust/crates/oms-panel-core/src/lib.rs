//! Native non-TUI panel control plane. Producer authority is supplied through Services.
#![forbid(unsafe_code)]
#[cfg(not(unix))]
compile_error!("OMS panel supports Linux, WSL2 and macOS");

mod acceptance;
pub mod activity;
pub mod cache;
mod control;
pub mod dispatch;
pub mod eventlog;
pub mod export;
pub mod files;
mod git;
pub mod health;
pub mod history;
pub mod host;
pub mod jobs;
pub mod metrics;
pub mod release;
pub mod results;
pub mod review;
pub mod routing;
pub mod salvage;
pub mod services;
pub mod since;
pub mod state;
mod storage;
pub mod terminals;
pub mod tick;
mod value;
pub mod workspaces;

pub use serde_json::Value;
pub use services::{CommandOutput, NativeCommand, PeerExecutor, Services, SubcommandServices};
pub use storage::{Store, Transaction};
pub use value::{decode, digest, now, outbound, redact, timestamp};

/// Native producer types remain available to an in-process command adapter.
pub mod native {
    pub use oms_command_attempt_bundle as attempt_bundle;
    pub use oms_command_failure as failure;
    pub use oms_command_operations as operations;
    pub use oms_command_plan as plan;
    pub use oms_command_room as room;
    pub use oms_command_task as task;
    pub use oms_command_telemetry as telemetry;
    pub use oms_context as context;
    pub use oms_json as json;
    pub use oms_reference as reference;
    pub use oms_supervisor as supervisor;
    pub use oms_work_inspect as work;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: i32,
    pub message: String,
    /// A mutation may already be visible. Never automatically repeat it.
    pub publication_uncertain: bool,
}
impl Error {
    pub fn new(message: impl ToString) -> Self {
        Self {
            code: 2,
            message: message.to_string(),
            publication_uncertain: false,
        }
    }
    pub fn busy(message: impl ToString) -> Self {
        Self {
            code: 75,
            ..Self::new(message)
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
pub(crate) fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::new(message))
    }
}
pub(crate) fn identifier(value: &str) -> Result<()> {
    require(
        !value.is_empty()
            && value.len() <= 160
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)),
        "invalid panel identifier",
    )
}
pub(crate) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub(crate) fn parent_only() -> Result<()> {
    require(
        std::env::var_os("OMS_HARNESS_CHILD").is_none_or(|v| v != "1")
            && std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH")
                .is_none_or(|v| v == "0" || v.is_empty()),
        "panel mutations require the owning parent",
    )
}
pub(crate) fn checkpoint(stop: &std::sync::atomic::AtomicBool) -> Result<()> {
    if stop.load(std::sync::atomic::Ordering::Acquire) {
        Err(Error {
            code: 130,
            ..Error::new("panel operation interrupted")
        })
    } else {
        Ok(())
    }
}
