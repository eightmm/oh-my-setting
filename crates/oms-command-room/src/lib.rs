//! Native task-scoped room coordination over the canonical OMS thread logs.
//! Membership and consumption confer no admission, approval or model authority.
//!
//! `Room` is the shared in-process core; `run_with_services` is the public
//! command adapter. Python module functions become methods on `Room`; pure
//! projection and scope helpers remain exports. `selected` returns both the
//! binding classification and retained projection, replacing `_selected` and
//! `selected` without a second acquisition of the same source.
//!
//! The parent implements `Services` for managed repository resolution, current
//! native identity, consistent operations projections and app delivery. A
//! producer read guard must fence concurrent lifecycle writes until dropped;
//! room writers retain it through publication, with room-before-producer lock
//! ordering. Missing integrations refuse; no provider process is started here.
//!
//! Durable effects are limited to existing thread logs, their advisory CURRENT
//! pointer, legacy thread/rollover locks and the private relay queue protocol.
//! A partial append is withheld, never truncated. A sealed unfinished rollover
//! accepts only reconciliation before its carried marker. An uncertain delivery
//! keeps its pending intent and requires explicit retry. Queue tickets identify
//! default messages across restart; timeouts still require reconciliation.
#![forbid(unsafe_code)]
#[cfg(not(unix))]
compile_error!("OMS rooms support Linux, WSL2 and macOS, not native Windows");

mod carry;
mod cli;
mod core;
mod cursor;
mod model;
mod relay;
mod services;
mod storage;
mod validate;
pub mod value;

pub use core::{Join, LineageEntry, Room, Selection, Send};
pub use model::{
    ACK_AFTER, ASKER_AFTER, KINDS, MAIN_OWNERS, MAX_REMINDERS, OVERDUE_AFTER, PERSON_AFTER,
    REPEAT_AFTER, ROLES, SECOND_REMINDER_AFTER, age, member as participant, safe_room_path,
};
pub use model::{
    CALL_RESERVE, CARRY_CALLS, CARRY_MESSAGES, MAIN_RESERVE, MAX_CALLS, MAX_HOPS, MAX_MESSAGES,
    MAX_METADATA_BYTES, MAX_PARTICIPANTS, MAX_ROOMS, MAX_SCAN_BYTES, MAX_SCAN_ENTRIES,
    MESSAGE_RESERVE, ROLLOVER_CALLS, ROLLOVER_MESSAGES,
};
pub use model::{
    declared_scopes, event_shape, identifier, main_owner_matches, open_questions, overlaps,
    project, prompt_message, reminder_id, review_ids, unread_messages, visible_to,
};
pub use oms_contract_markdown::scope::normalize as normalize_scope;
pub use relay::{Relay, relay_request};
pub use services::{
    App, Attempt, DeliveryReceipt, DeliveryRequest, LocalServices, OperationsRead, Services,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
pub use storage::{MAX_FILE, MAX_ROW, Snapshot};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub message: String,
}
impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
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
pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn now() -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(unix_seconds() as i64, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
pub(crate) fn unique() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|_| Error::new("native random source unavailable"))?;
    b[6] = (b[6] & 15) | 64;
    b[8] = (b[8] & 63) | 128;
    Ok(b.iter().map(|b| format!("{b:02x}")).collect())
}
pub(crate) fn checkpoint(stop: &AtomicBool) -> Result<()> {
    if stop.load(Ordering::Acquire) {
        Err(Error::new("room operation interrupted"))
    } else {
        Ok(())
    }
}

pub(crate) fn outbound_clean(bytes: &[u8]) -> Result<bool> {
    value::outbound_clean(bytes)
}

/// In-process derived data only. Poisoning discards the entire projection;
/// neither this mutex nor its contents are a filesystem authority.
pub(crate) struct Shared<T: Default>(std::sync::Mutex<T>);
impl<T: Default> Shared<T> {
    pub fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }
    pub fn borrow(&self) -> std::sync::MutexGuard<'_, T> {
        match self.0.lock() {
            Ok(value) => value,
            Err(error) => {
                let mut value = error.into_inner();
                *value = T::default();
                self.0.clear_poison();
                value
            }
        }
    }
    pub fn borrow_mut(&self) -> std::sync::MutexGuard<'_, T> {
        self.borrow()
    }
}
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub child: bool,
    pub room: Option<String>,
    pub participant: Option<String>,
    pub repository: Option<PathBuf>,
    pub admitted_participant: Option<String>,
    pub relay_allowed: bool,
    pub relay_queue: Option<PathBuf>,
    pub agent: String,
}
impl Session {
    pub fn from_env() -> Self {
        let text = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            child: std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1")
                || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH").is_some_and(|v| v != "0"),
            room: text("OMS_ROOM_ID"),
            participant: text("OMS_ROOM_PARTICIPANT"),
            repository: std::env::var_os("OMS_ROOM_REPO")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            admitted_participant: text("OMS_ROOM_ADMITTED_PARTICIPANT"),
            relay_allowed: std::env::var_os("OMS_ROOM_RELAY_ALLOWED").is_some_and(|v| v == "1"),
            relay_queue: std::env::var_os("OMS_ROOM_RELAY_QUEUE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            agent: text("OMS_AGENT").unwrap_or_else(|| {
                if text("CLAUDECODE").is_some() || text("CLAUDE_CODE_ENTRYPOINT").is_some() {
                    "claude".to_owned()
                } else if text("CODEX_SANDBOX").is_some() {
                    "codex".to_owned()
                } else {
                    "agent".to_owned()
                }
            }),
        }
    }
}
/// Public adapter; the parent supplies concrete native producers through
/// `run_with_services`. No provider process or Python/Bash bridge is spawned.
pub fn run(
    args: &[std::ffi::OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_services(args, default_repo, interrupted, &LocalServices)
}
pub fn run_with_services(
    args: &[std::ffi::OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    services: &dyn Services,
) -> (i32, Vec<u8>, Vec<u8>) {
    cli::run(args, default_repo, interrupted, services)
}
pub fn owns(_args: &[std::ffi::OsString]) -> bool {
    true
}
pub use carry::{CarryPlan, CarryWrite, carry_plan, successor_id};
pub use relay::{
    RELAY_MAX_PENDING, RELAY_MAX_RATE, RELAY_MAX_REQUEST, RELAY_MAX_SCAN_ENTRIES, RELAY_RATE_WINDOW,
};
