//! Canonical lifecycle/approval operations, with separate command adapters.
//! See README.md for routing, streaming, child guards, and parent integration.
#![forbid(unsafe_code)]
mod approvals;
mod cli;
mod diagnostic;
mod events;
mod help;
mod incremental;
pub mod lock;
pub mod projection;
pub mod storage;
pub mod value;

pub use approvals::{
    approval_action_digest, approval_effective_state, approval_projection, load_approvals,
    parse_scalar_map, public_approval,
};
pub use events::{
    StartRequest, append_lifecycle, append_lifecycle_checked,
    append_lifecycle_checked_with_projection, attempt_spec_from_created, attempt_spec_path,
    create_attempt, effective_run_id, existing_idempotent, lifecycle_semantics, load_projection,
    new_event, project_attempts, validate_event_row,
};
pub use incremental::EventsProjectionCache;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
pub use storage::{approval_path, event_path, read_rows};
use value::{V, obj};
pub type Result<T> = std::result::Result<T, String>;
pub type CommandResult = (i32, Vec<u8>, Vec<u8>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum App {
    Events,
    Approvals,
}
impl App {
    pub fn from_namespace(namespace: &str) -> Option<Self> {
        match namespace {
            "events" | "agent-events" => Some(Self::Events),
            "approvals" | "approval-inbox" => Some(Self::Approvals),
            _ => None,
        }
    }
}

/// Both complete command families are owned, including parser errors and help.
/// Classification performs no filesystem access. Bundle selectors remain owned
/// by oms-command-attempt-bundle and are offered there before this parser.
pub fn owns_events(_args: &[std::ffi::OsString]) -> bool {
    true
}
pub fn owns_approvals(_args: &[std::ffi::OsString]) -> bool {
    true
}

/// Buffered convenience adapter. Use run_streaming for follow's flushed rows.
pub fn run(
    app: App,
    args: &[std::ffi::OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> CommandResult {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_streaming(app, args, default_repo, interrupted, &mut out, &mut err);
    (code, out, err)
}

/// Full router contract: argv excludes the verb/namespace; --repo is a global
/// option before the subcommand, just as the canonical parser. Each streamed
/// follow row is flushed before waiting. Errors never invoke a legacy backend.
pub fn run_streaming(
    app: App,
    args: &[std::ffi::OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    stdout: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> i32 {
    if app == App::Events && oms_command_attempt_bundle::owns(args) {
        let (code, out, err) = oms_command_attempt_bundle::run(args, default_repo, interrupted);
        if stdout.write_all(&out).is_err() || stderr.write_all(&err).is_err() {
            return 2;
        }
        return code;
    }
    let parsed = match cli::parse(app, args) {
        Ok(parsed) => parsed,
        Err(exit) => {
            let _ = stdout.write_all(&exit.1);
            let _ = stderr.write_all(&exit.2);
            return exit.0;
        }
    };
    let result = (|| {
        checkpoint(interrupted)?;
        guard_child(app, &parsed.command, parsed.flag("apply"))?;
        let repo = storage::repo_root(&parsed.repo, default_repo)?;
        match app {
            App::Events => events::execute(&repo, &parsed, interrupted, stdout),
            App::Approvals => approvals::execute(&repo, &parsed, interrupted, stdout),
        }
    })();
    match result {
        Ok(()) => 0,
        Err(message) => {
            let _ = writeln!(stderr, "error: {message}");
            2
        }
    }
}

pub(crate) fn checkpoint(interrupted: &AtomicBool) -> Result<()> {
    if interrupted.load(Ordering::Relaxed) {
        Err("operation interrupted".into())
    } else {
        Ok(())
    }
}
fn guard_child(app: App, command: &str, apply: bool) -> Result<()> {
    let child = std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|s| s == "1")
        || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH")
            .is_some_and(|s| s != "0" && !s.is_empty());
    let read = matches!(command, "show" | "list" | "validate" | "path" | "follow")
        || matches!(command, "expire" | "reconcile" | "compact") && !apply;
    if child && (app == App::Approvals && !read || app == App::Events && !read) {
        Err("a harness child may only use read-only runtime actions".into())
    } else {
        Ok(())
    }
}
pub(crate) fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub(crate) fn now() -> String {
    stamp_at(now_seconds() as i64)
}
pub(crate) fn stamp_at(seconds: i64) -> String {
    use chrono::Datelike;
    chrono::DateTime::from_timestamp(seconds, 0)
        .filter(|t| (1..=9999).contains(&t.year()))
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn random_bytes(count: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0; count];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}
pub(crate) fn random_hex(count: usize) -> Result<String> {
    Ok(random_bytes(count)?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
pub(crate) fn uuid(prefix: &str) -> Result<String> {
    let mut bytes = random_bytes(16)?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    Ok(format!(
        "{prefix}{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}
pub(crate) fn safe(value: &str, label: &str, optional: bool) -> Result<String> {
    if optional && value.is_empty() || projection::id(value) {
        Ok(value.into())
    } else {
        Err(format!(
            "{label} must match [A-Za-z0-9][A-Za-z0-9._:-]{{0,159}}"
        ))
    }
}
pub(crate) fn reason(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() <= 64
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._:-".contains(&b))
            && value
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        Ok(value.into())
    } else {
        Err("reason_code must match [a-z0-9][a-z0-9._:-]{0,63}".into())
    }
}
pub(crate) fn print_json(out: &mut dyn std::io::Write, value: &V) -> Result<()> {
    out.write_all(&value.json(true)?)
        .and_then(|_| out.write_all(b"\n"))
        .map_err(|e| e.to_string())
}
pub(crate) fn line(out: &mut dyn std::io::Write, value: impl std::fmt::Display) -> Result<()> {
    writeln!(out, "{value}").map_err(|e| e.to_string())
}
pub(crate) fn actor(kind: &str, name: &str) -> Result<V> {
    Ok(obj([
        ("kind", safe(kind, "actor kind", false)?.into()),
        ("name", safe(name, "actor", false)?.into()),
    ]))
}
#[cfg(test)]
mod tests;

pub const TERMINAL_STATES: &[&str] = &["done", "failed", "cancelled", "timed_out", "abandoned"];
pub const NONTERMINAL_STATES: &[&str] = &[
    "queued",
    "starting",
    "working",
    "waiting_input",
    "waiting_approval",
    "verifying",
    "review",
    "blocked",
];
pub fn parse_ts(raw: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    projection::stamp(&V::from(raw))
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .ok_or_else(|| format!("invalid UTC timestamp: {raw}"))
}
