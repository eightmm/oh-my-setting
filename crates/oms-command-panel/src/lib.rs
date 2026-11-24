//! Native terminal panel consuming the OMS panel control plane.
#![forbid(unsafe_code)]

mod agy_child_chat;
mod agy_child_records;
mod agy_lead_custody;
pub mod attention;
mod board_refresh;
mod claude_child_chat;
mod claude_lead_usage;
pub mod cli;
pub mod commands;
mod desktop_board;
mod desktop_detail;
mod desktop_evidence;
mod desktop_history;
mod executable;
pub mod input;
pub mod layout;
pub mod managed;
mod native_children;
pub mod native_panel;
pub mod navigation;
mod orchestrator;
mod provider_activity;
pub mod reader;
pub mod recommend;
pub mod render;
pub mod usage;
pub mod wakeup;
pub mod watch;

pub use oms_panel_core::{Error, Result, Services};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
pub use wakeup::notify_wakeup;

pub type Output = (i32, Vec<u8>, Vec<u8>);

pub fn owns(tool: &str, _args: &[OsString]) -> bool {
    tool == "panel"
}

pub fn run_with_source(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
) -> Output {
    match oms_panel_core::SubcommandServices::sibling() {
        Ok(services) => run_with_services(tool, args, cwd, source, interrupted, &services),
        Err(e) => failure(e),
    }
}

/// Hosts inject their existing native producers; display state confers no authority.
pub fn run_with_services(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
    services: &dyn Services,
) -> Output {
    if !owns(tool, args) {
        return (2, vec![], b"oms panel: unsupported tool\n".to_vec());
    }
    let options = match cli::Options::parse(args) {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    if options.has("help") {
        return (0, cli::usage().into_bytes(), vec![]);
    }
    if interrupted.load(Ordering::Acquire) {
        return (130, vec![], vec![]);
    }
    let default_main = options.selects_main()
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && parent_only().is_ok();
    match commands::run_mode(&options, default_main, cwd, source, services, interrupted) {
        Ok(v) => v,
        Err(e) => failure(e),
    }
}

pub(crate) fn require(value: bool, message: &str) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(Error::new(message))
    }
}
pub(crate) fn parent_only() -> Result<()> {
    require(
        std::env::var_os("OMS_HARNESS_CHILD").is_none_or(|v| v != "1")
            && std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH")
                .is_none_or(|v| v == "0" || v.is_empty()),
        "panel mutations require the owning parent",
    )
}
pub(crate) fn identifier(value: &str) -> Result<()> {
    require(
        !value.is_empty()
            && value.len() <= 160
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || b"._-".contains(&v)),
        "invalid panel identifier",
    )
}
pub(crate) fn failure(error: Error) -> Output {
    let uncertain = if error.publication_uncertain {
        "; outcome may already be visible; reconcile before retrying"
    } else {
        ""
    };
    (
        error.code,
        vec![],
        format!(
            "oms panel: {}{uncertain}\n",
            render::clean(&error.message, 1000)
        )
        .into_bytes(),
    )
}
pub(crate) fn json(value: &serde_json::Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(Error::new)?;
    bytes.push(b'\n');
    Ok(bytes)
}
pub(crate) fn text<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
pub(crate) fn rows(value: &serde_json::Value) -> &[serde_json::Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

#[cfg(test)]
#[path = "../tests/support/executable_resolution.rs"]
mod executable_resolution;
