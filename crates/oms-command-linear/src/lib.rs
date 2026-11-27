//! Linear agent session webhook bridge and retry-safe activity reply adapter over OMS Rooms.
//!
//! No task ledger or execution engine lives here: inbound `AgentSessionEvent` requests land as
//! untrusted `question` messages in an existing OMS Room for Main, and outbound replies pass
//! `oms-scrub` before leaving the machine and record idempotent Room `status` receipts.

#![forbid(unsafe_code)]
#[cfg(not(unix))]
compile_error!("oms-command-linear supports Linux, WSL2 and macOS, not native Windows");

pub mod cli;
pub mod crypto;
pub mod model;
pub mod reply;
pub mod serve;
pub mod transport;

pub use cli::{ENV_ACCESS_CRED, ENV_WEBHOOK_CRED, HELP, run_streaming_with_services};
pub use crypto::{SecretValue, hmac_sha256, hmac_sha256_hex, verify_linear_signature};
pub use model::{
    ActivityKind, CREATED_THOUGHT_CALLER_KEY, DeliveryState, FIRST_THOUGHT_BODY,
    REFUSED_CONTENT_ERROR_BODY, ROOM_TEXT_LIMIT, UNTRUSTED_HEADER, created_message_id,
    derive_activity_id, format_created_room_text, format_prompted_room_text, format_receipt_text,
    prompted_message_id, receipt_message_id, reply_caller_key, unknown_receipt_message_id,
};
pub use reply::{ReplyConfig, ReplyOutcome, execute_reply};
pub use serve::{
    DEFAULT_LISTEN_ADDR, MAX_BODY_BYTES, MAX_HEADER_BYTES, PreparedServer, ServeConfig,
    ServerHandle, TIMESTAMP_WINDOW_MS, WEBHOOK_PATH, parse_loopback_listen,
};
pub use transport::{DEFAULT_LINEAR_API_URL, ENV_LINEAR_API_URL, EndpointUrl, LinearClient};

use oms_command_room::{LocalServices, Services as RoomServices};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub fn owns(_args: &[OsString]) -> bool {
    true
}

pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_services(args, default_repo, interrupted, &LocalServices)
}

pub fn run_with_services(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    room_services: &dyn RoomServices,
) -> (i32, Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_streaming_with_services(
        args,
        default_repo,
        interrupted,
        room_services,
        &mut out,
        &mut err,
    );
    (code, out, err)
}

pub fn run_streaming(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    run_streaming_with_services(
        args,
        default_repo,
        interrupted,
        &LocalServices,
        stdout,
        stderr,
    )
}
