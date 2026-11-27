//! Source-neutral external tracker intake receiver and retry-safe reply delivery over OMS Rooms.
//!
//! No task ledger or execution engine lives here: inbound signed requests land as untrusted
//! `question` messages in an existing OMS Room for Main, and outbound replies pass `oms-scrub`
//! before leaving the machine and record idempotent Room `status` receipts.

#![forbid(unsafe_code)]
#[cfg(not(unix))]
compile_error!("oms-command-intake supports Linux, WSL2 and macOS, not native Windows");

pub mod cli;
pub mod crypto;
pub mod linear;
pub mod model;
pub mod reply;
pub mod serve;
pub mod source;
pub mod transport;

pub use cli::{help_text, run_streaming_with_services};
pub use crypto::{
    SecretStore, SecretValue, TIMESTAMP_WINDOW_MS, ct_eq_32, current_unix_ms, hex_lower,
    hmac_sha256, hmac_sha256_hex, verify_hmac_sha256_hex, within_timestamp_window_ms,
};
pub use linear::{
    CREATED_THOUGHT_CALLER_KEY, DEFAULT_LINEAR_API_URL, ENV_ACCESS_CRED, ENV_LINEAR_API_URL,
    ENV_WEBHOOK_CRED, FIRST_THOUGHT_BODY, LINEAR_INTAKE_PATH, LinearSource,
    REFUSED_CONTENT_ERROR_BODY, SOURCE_LINEAR, verify_linear_signature,
};
pub use model::{
    DeliveryState, ROOM_TEXT_LIMIT, UNTRUSTED_HEADER, WorkRequest, WorkRequestKind,
    created_message_id, derive_delivery_id, format_receipt_text, format_work_request_room_text,
    intake_path, prompted_message_id, receipt_message_id, reply_caller_key, safe_message_id,
    unknown_receipt_message_id,
};
pub use reply::{ReplyConfig, ReplyOutcome, execute_reply};
pub use serve::{
    DEFAULT_LISTEN_ADDR, MAX_BODY_BYTES, MAX_HEADER_BYTES, PreparedServer, ServeConfig,
    ServerHandle, parse_loopback_listen,
};
pub use source::{
    BindingDecision, BindingFlagSpec, CreateDeliveryOutcome, EnvVarSpec, OutboundAck,
    OutboundReply, PayloadDecision, RequestHeaders, RoomDelivery, SignatureInput, Source,
    TransportError, compiled_sources, find_source_arc,
};
pub use transport::{EndpointUrl, post_json_bearer};

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
