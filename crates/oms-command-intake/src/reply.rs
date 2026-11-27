//! Source-neutral outbound scan, deterministic delivery ID, query-before-create retry state
//! machine, and Room `status` receipt recorder.

use crate::crypto::SecretStore;
use crate::model::{
    DeliveryState, derive_delivery_id, format_receipt_text, receipt_message_id, reply_caller_key,
    unknown_receipt_message_id, validate_external_id, validate_source_name,
};
use crate::source::{CreateDeliveryOutcome, OutboundReply, Source, TransportError};
use oms_command_room::{Room, Send as RoomSend, Services as RoomServices, Session as RoomSession};
use rustix::fs::{self as rfs, FileType, Mode, OFlags};
use serde_json::json;
use std::fs::File;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub const MAX_REPLY_TEXT_BYTES: usize = 64 * 1024;
const REPLY_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct ReplyConfig {
    pub source: Arc<dyn Source>,
    pub repo: PathBuf,
    pub room: String,
    pub participant: String,
    pub session: String,
    pub kind: String,
    pub text_file: PathBuf,
    pub reply_to: Option<String>,
    pub json: bool,
    pub secrets: SecretStore,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplyOutcome {
    pub exit_code: i32,
    pub delivery_id: String,
    pub receipt_id: String,
    pub state: DeliveryState,
}

fn read_text_file_bounded(path: &Path) -> Result<String, String> {
    if path.as_os_str().is_empty() {
        return Err("--text-file must not be empty".into());
    }
    let fd = rfs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "reply --text-file is missing, unreadable, or a symbolic link".to_owned())?;
    let st = rfs::fstat(&fd).map_err(|_| "failed inspecting reply --text-file".to_owned())?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return Err("reply --text-file must be a regular file".into());
    }
    if st.st_size < 0 || (st.st_size as usize) > MAX_REPLY_TEXT_BYTES {
        return Err(format!(
            "reply --text-file must be at most {MAX_REPLY_TEXT_BYTES} bytes"
        ));
    }
    let mut raw = Vec::with_capacity(st.st_size as usize);
    File::from(fd)
        .take((MAX_REPLY_TEXT_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| "failed reading reply --text-file".to_owned())?;
    if raw.len() > MAX_REPLY_TEXT_BYTES {
        return Err(format!(
            "reply --text-file must be at most {MAX_REPLY_TEXT_BYTES} bytes"
        ));
    }
    let text = String::from_utf8(raw).map_err(|_| "reply --text-file must be valid UTF-8")?;
    if text.trim().is_empty() {
        return Err("reply --text-file must not be empty".into());
    }
    Ok(text)
}

enum OutboundScanError {
    BodyRefused,
    EnvironmentTooLarge,
    ScanFailed,
}

fn check_outbound_reply(body: &[u8], secrets: &SecretStore) -> Result<(), OutboundScanError> {
    for (_, secret) in secrets.entries() {
        let raw = secret.as_bytes();
        if !raw.is_empty() && body.windows(raw.len()).any(|w| w == raw) {
            return Err(OutboundScanError::BodyRefused);
        }
    }
    let mut pairs = Vec::new();
    let mut total = 0usize;
    for (key, value) in std::env::vars_os() {
        total = total
            .saturating_add(key.as_bytes().len())
            .saturating_add(value.as_bytes().len());
        if pairs.len() + 1 > oms_scrub::MAX_ENV_ENTRIES || total > oms_scrub::MAX_ENV_BYTES {
            return Err(OutboundScanError::EnvironmentTooLarge);
        }
        pairs.push((key, value));
    }
    let extra_count = secrets.entries().count();
    if pairs.len().saturating_add(extra_count) > oms_scrub::MAX_ENV_ENTRIES {
        return Err(OutboundScanError::EnvironmentTooLarge);
    }
    let mut refs: Vec<(&[u8], &[u8])> = pairs
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect();
    for (name, secret) in secrets.entries() {
        total = total
            .saturating_add(name.len())
            .saturating_add(secret.as_bytes().len());
        if total > oms_scrub::MAX_ENV_BYTES {
            return Err(OutboundScanError::EnvironmentTooLarge);
        }
        refs.push((name.as_bytes(), secret.as_bytes()));
    }
    match oms_scrub::scan(body, &refs) {
        Ok(hits) if hits.is_empty() => Ok(()),
        Ok(_) | Err(oms_scrub::Error::TooManyHits) => Err(OutboundScanError::BodyRefused),
        Err(oms_scrub::Error::EnvironmentTooLarge) => Err(OutboundScanError::EnvironmentTooLarge),
        Err(_) => Err(OutboundScanError::ScanFailed),
    }
}

fn record_status_receipt(
    room: &Room<'_>,
    cfg: &ReplyConfig,
    delivery_id: &str,
    state: DeliveryState,
) -> Result<String, String> {
    let source_name = cfg.source.name();
    let room_id = cfg.room.as_str();
    let participant = cfg.participant.as_str();
    let session_id = cfg.session.as_str();
    let kind = cfg.kind.as_str();
    let mid = match state {
        DeliveryState::Confirmed | DeliveryState::ConfirmedReplayed => {
            receipt_message_id(source_name, delivery_id)?
        }
        DeliveryState::OutcomeUnknown => unknown_receipt_message_id(source_name, delivery_id)?,
    };
    let existing = room
        .messages(room_id)
        .map_err(|e| format!("failed reading room messages for receipt: {}", e.message))?;
    let expected_prefix = format!(
        "intake delivery receipt: source={source_name} session={session_id} kind={kind} delivery={delivery_id} state="
    );

    let text_to_send = if let Some(prior) = existing.iter().find(|m| m.text("id") == mid) {
        let prior_text = prior.text("text");
        if prior.text("sender") == participant
            && prior.text("message_kind") == "status"
            && prior_text.starts_with(&expected_prefix)
        {
            prior_text.to_owned()
        } else {
            format_receipt_text(source_name, session_id, kind, delivery_id, state)
        }
    } else {
        format_receipt_text(source_name, session_id, kind, delivery_id, state)
    };

    room.send(
        room_id,
        RoomSend {
            sender: participant,
            recipient: "all",
            text: &text_to_send,
            kind: "status",
            reply_to: None,
            message_id: Some(&mid),
            linked: false,
            quiet: false,
        },
    )
    .map_err(|e| format!("failed recording intake receipt in room: {}", e.message))?;

    Ok(mid)
}

fn render_outcome(
    cfg: &ReplyConfig,
    delivery_id: &str,
    receipt_id: &str,
    state: DeliveryState,
) -> Vec<u8> {
    let source_name = cfg.source.name();
    if cfg.json {
        let payload = json!({
            "source": source_name,
            "room": cfg.room,
            "participant": cfg.participant,
            "session": cfg.session,
            "kind": cfg.kind,
            "delivery_id": delivery_id,
            "receipt_message_id": receipt_id,
            "state": state.as_str(),
        });
        let mut out = serde_json::to_vec(&payload).unwrap_or_default();
        out.push(b'\n');
        out
    } else {
        format!(
            "intake reply: source={source_name} session={} kind={} delivery={delivery_id} receipt={receipt_id} state={}\n",
            cfg.session,
            cfg.kind,
            state.as_str()
        )
        .into_bytes()
    }
}

pub fn execute_reply(
    cfg: &ReplyConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    let source_name = cfg.source.name();
    if let Err(msg) = validate_source_name(source_name) {
        return (2, vec![], format!("error: intake: {msg}\n").into_bytes());
    }
    if let Err(e) = oms_command_room::identifier(&cfg.room, "room") {
        return (
            2,
            vec![],
            format!("error: intake: {}\n", e.message).into_bytes(),
        );
    }
    if let Err(e) = oms_command_room::identifier(&cfg.participant, "participant") {
        return (
            2,
            vec![],
            format!("error: intake: {}\n", e.message).into_bytes(),
        );
    }
    if let Err(msg) = validate_external_id(&cfg.session, "session") {
        return (2, vec![], format!("error: intake: {msg}\n").into_bytes());
    }
    let allowed_kinds = cfg.source.reply_kinds();
    if !allowed_kinds.contains(&cfg.kind.as_str()) {
        return (
            2,
            vec![],
            format!(
                "error: intake: invalid --kind '{}' for source '{}' (expected {})\n",
                cfg.kind,
                source_name,
                allowed_kinds.join(", ")
            )
            .into_bytes(),
        );
    }
    if let Some(reply_to) = &cfg.reply_to
        && let Err(e) = oms_command_room::identifier(reply_to, "reply-to")
    {
        return (
            2,
            vec![],
            format!("error: intake: {}\n", e.message).into_bytes(),
        );
    }
    for spec in cfg.source.reply_env() {
        if spec.sensitive && spec.required && cfg.secrets.get(spec.name).is_none() {
            return (
                2,
                vec![],
                format!(
                    "error: intake: missing required environment variable {}\n",
                    spec.name
                )
                .into_bytes(),
            );
        }
    }
    if let Err(msg) = cfg.source.validate_endpoint() {
        return (2, vec![], format!("error: intake: {msg}\n").into_bytes());
    }

    let body = match read_text_file_bounded(&cfg.text_file) {
        Ok(t) => t,
        Err(msg) => return (2, vec![], format!("error: intake: {msg}\n").into_bytes()),
    };

    if let Err(scan_err) = check_outbound_reply(body.as_bytes(), &cfg.secrets) {
        let msg: &[u8] = match scan_err {
            OutboundScanError::BodyRefused => {
                b"error: intake: reply body refused by outbound scan; nothing sent\n"
            }
            OutboundScanError::EnvironmentTooLarge => {
                b"error: intake: process environment exceeds outbound scan limits; nothing sent\n"
            }
            OutboundScanError::ScanFailed => {
                b"error: intake: outbound scan failed to verify reply; nothing sent\n"
            }
        };
        return (3, vec![], msg.to_vec());
    }

    let room = match Room::open(
        &cfg.repo,
        RoomSession::from_env(),
        room_services,
        interrupted,
        false,
        false,
    ) {
        Ok(r) => r,
        Err(e) => {
            return (
                2,
                vec![],
                format!("error: intake: room unavailable: {}\n", e.message).into_bytes(),
            );
        }
    };
    let state_proj = match room.status(&cfg.room, false) {
        Ok(s) => s,
        Err(e) => {
            return (
                2,
                vec![],
                format!(
                    "error: intake: room '{}' unavailable: {}\n",
                    cfg.room, e.message
                )
                .into_bytes(),
            );
        }
    };
    if state_proj.flag("closed") {
        return (
            2,
            vec![],
            format!("error: intake: room '{}' is closed\n", cfg.room).into_bytes(),
        );
    }
    if let Err(e) = oms_command_room::participant(&state_proj, &cfg.participant) {
        return (
            2,
            vec![],
            format!(
                "error: intake: --participant '{}': {}\n",
                cfg.participant, e.message
            )
            .into_bytes(),
        );
    }

    let caller_key = reply_caller_key(&cfg.kind, cfg.reply_to.as_deref(), body.as_bytes());
    let delivery_id = derive_delivery_id(source_name, &cfg.session, &cfg.room, &caller_key);

    let delivery_res = match cfg.source.query_delivery(
        &cfg.secrets,
        &delivery_id,
        &cfg.session,
        REPLY_HTTP_TIMEOUT,
        interrupted,
    ) {
        Ok(true) => Ok(DeliveryState::ConfirmedReplayed),
        Ok(false) => {
            let outbound = OutboundReply {
                delivery_id: delivery_id.clone(),
                session_ref: cfg.session.clone(),
                kind: cfg.kind.clone(),
                body,
                reply_to: cfg.reply_to.clone(),
            };
            match cfg.source.create_delivery(
                &cfg.secrets,
                &outbound,
                REPLY_HTTP_TIMEOUT,
                interrupted,
            ) {
                Ok(CreateDeliveryOutcome::Created) => Ok(DeliveryState::Confirmed),
                Ok(CreateDeliveryOutcome::AlreadyExists) => Ok(DeliveryState::ConfirmedReplayed),
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    };

    match delivery_res {
        Ok(state) => match record_status_receipt(&room, cfg, &delivery_id, state) {
            Ok(receipt_id) => (
                0,
                render_outcome(cfg, &delivery_id, &receipt_id, state),
                vec![],
            ),
            Err(msg) => (1, vec![], format!("error: intake: {msg}\n").into_bytes()),
        },
        Err(TransportError::OutcomeUnknown(reason)) => {
            let receipt_id =
                record_status_receipt(&room, cfg, &delivery_id, DeliveryState::OutcomeUnknown)
                    .unwrap_or_else(|_| {
                        unknown_receipt_message_id(source_name, &delivery_id).unwrap_or_else(|_| {
                            format!("{source_name}-delivery-{delivery_id}-unknown")
                        })
                    });
            let out = render_outcome(
                cfg,
                &delivery_id,
                &receipt_id,
                DeliveryState::OutcomeUnknown,
            );
            let err = format!(
                "error: intake: delivery outcome unknown ({reason}); safe to retry with identical inputs\n"
            )
            .into_bytes();
            (75, out, err)
        }
        Err(TransportError::Rejected(reason)) => {
            (1, vec![], format!("error: intake: {reason}\n").into_bytes())
        }
    }
}
