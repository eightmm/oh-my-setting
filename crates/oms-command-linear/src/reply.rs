//! Outbound-scanned, retry-safe Linear activity replies with Room status receipts.

use crate::crypto::SecretValue;
use crate::model::{
    ActivityKind, DeliveryState, derive_activity_id, format_receipt_text, receipt_message_id,
    reply_caller_key, unknown_receipt_message_id, validate_external_id,
};
use crate::transport::{ApiError, EndpointUrl, LinearClient};
use oms_command_room::{Room, Send as RoomSend, Services as RoomServices, Session as RoomSession};
use rustix::fs::{self as rfs, FileType, Mode, OFlags};
use serde_json::json;
use std::fs::File;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub const MAX_REPLY_TEXT_BYTES: usize = 64 * 1024;
const REPLY_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct ReplyConfig {
    pub repo: PathBuf,
    pub room: String,
    pub participant: String,
    pub session: String,
    pub kind: ActivityKind,
    pub text_file: PathBuf,
    pub reply_to: Option<String>,
    pub json: bool,
    pub access_cred: SecretValue,
    pub api_url: EndpointUrl,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplyOutcome {
    pub exit_code: i32,
    pub activity_id: String,
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

fn check_outbound_reply(body: &[u8], access_cred: &SecretValue) -> Result<(), ()> {
    let mut pairs = Vec::new();
    let mut total = 0usize;
    for (key, value) in std::env::vars_os() {
        total = total
            .saturating_add(key.as_bytes().len())
            .saturating_add(value.as_bytes().len());
        if pairs.len() + 1 >= oms_scrub::MAX_ENV_ENTRIES || total > oms_scrub::MAX_ENV_BYTES {
            return Err(());
        }
        pairs.push((key, value));
    }
    let explicit_name = concat!("OMS_LINEAR_ACCESS_", "TOKEN");
    let mut refs: Vec<(&[u8], &[u8])> = pairs
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect();
    refs.push((explicit_name.as_bytes(), access_cred.as_bytes()));
    oms_scrub::check_outbound_with_env(body, &refs).map_err(|_| ())
}

fn record_status_receipt(
    room: &Room<'_>,
    room_id: &str,
    participant: &str,
    session_id: &str,
    kind: ActivityKind,
    activity_id: &str,
    state: DeliveryState,
) -> Result<String, String> {
    let mid = match state {
        DeliveryState::Confirmed | DeliveryState::ConfirmedReplayed => {
            receipt_message_id(activity_id)?
        }
        DeliveryState::OutcomeUnknown => unknown_receipt_message_id(activity_id)?,
    };
    let existing = room
        .messages(room_id)
        .map_err(|e| format!("failed reading room messages for receipt: {}", e.message))?;
    let expected_prefix = format!(
        "linear activity receipt: session={session_id} kind={} activity={activity_id} state=",
        kind.as_str()
    );

    let text_to_send = if let Some(prior) = existing.iter().find(|m| m.text("id") == mid) {
        let prior_text = prior.text("text");
        if prior.text("sender") == participant
            && prior.text("message_kind") == "status"
            && prior_text.starts_with(&expected_prefix)
        {
            prior_text.to_owned()
        } else {
            format_receipt_text(session_id, kind, activity_id, state)
        }
    } else {
        format_receipt_text(session_id, kind, activity_id, state)
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
    .map_err(|e| format!("failed recording Linear receipt in room: {}", e.message))?;

    Ok(mid)
}

fn render_outcome(
    cfg: &ReplyConfig,
    activity_id: &str,
    receipt_id: &str,
    state: DeliveryState,
) -> Vec<u8> {
    if cfg.json {
        let payload = json!({
            "room": cfg.room,
            "participant": cfg.participant,
            "session": cfg.session,
            "kind": cfg.kind.as_str(),
            "activity_id": activity_id,
            "receipt_message_id": receipt_id,
            "state": state.as_str(),
        });
        let mut out = serde_json::to_vec(&payload).unwrap_or_default();
        out.push(b'\n');
        out
    } else {
        format!(
            "linear reply: session={} kind={} activity={activity_id} receipt={receipt_id} state={}\n",
            cfg.session,
            cfg.kind.as_str(),
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
    if let Err(e) = oms_command_room::identifier(&cfg.room, "room") {
        return (
            2,
            vec![],
            format!("error: linear: {}\n", e.message).into_bytes(),
        );
    }
    if let Err(e) = oms_command_room::identifier(&cfg.participant, "participant") {
        return (
            2,
            vec![],
            format!("error: linear: {}\n", e.message).into_bytes(),
        );
    }
    if let Err(msg) = validate_external_id(&cfg.session, "session") {
        return (2, vec![], format!("error: linear: {msg}\n").into_bytes());
    }
    if let Some(reply_to) = &cfg.reply_to
        && let Err(e) = oms_command_room::identifier(reply_to, "reply-to")
    {
        return (
            2,
            vec![],
            format!("error: linear: {}\n", e.message).into_bytes(),
        );
    }

    let body = match read_text_file_bounded(&cfg.text_file) {
        Ok(t) => t,
        Err(msg) => return (2, vec![], format!("error: linear: {msg}\n").into_bytes()),
    };

    if check_outbound_reply(body.as_bytes(), &cfg.access_cred).is_err() {
        return (
            3,
            vec![],
            b"error: linear: reply body refused by outbound scan; nothing sent\n".to_vec(),
        );
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
                format!("error: linear: room unavailable: {}\n", e.message).into_bytes(),
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
                    "error: linear: room '{}' unavailable: {}\n",
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
            format!("error: linear: room '{}' is closed\n", cfg.room).into_bytes(),
        );
    }
    if let Err(e) = oms_command_room::participant(&state_proj, &cfg.participant) {
        return (
            2,
            vec![],
            format!(
                "error: linear: --participant '{}': {}\n",
                cfg.participant, e.message
            )
            .into_bytes(),
        );
    }

    let caller_key = reply_caller_key(cfg.kind, cfg.reply_to.as_deref(), body.as_bytes());
    let activity_id = derive_activity_id(&cfg.session, &cfg.room, &caller_key);
    let client = LinearClient::new(cfg.api_url.clone(), cfg.access_cred.clone());

    let delivery_res = match client.query_activity(&activity_id, REPLY_HTTP_TIMEOUT, interrupted) {
        Ok(true) => Ok(DeliveryState::ConfirmedReplayed),
        Ok(false) => client.create_activity(
            &activity_id,
            &cfg.session,
            cfg.kind,
            &body,
            REPLY_HTTP_TIMEOUT,
            interrupted,
        ),
        Err(e) => Err(e),
    };

    match delivery_res {
        Ok(state) => {
            match record_status_receipt(
                &room,
                &cfg.room,
                &cfg.participant,
                &cfg.session,
                cfg.kind,
                &activity_id,
                state,
            ) {
                Ok(receipt_id) => (
                    0,
                    render_outcome(cfg, &activity_id, &receipt_id, state),
                    vec![],
                ),
                Err(msg) => (1, vec![], format!("error: linear: {msg}\n").into_bytes()),
            }
        }
        Err(ApiError::OutcomeUnknown(reason)) => {
            let receipt_id = record_status_receipt(
                &room,
                &cfg.room,
                &cfg.participant,
                &cfg.session,
                cfg.kind,
                &activity_id,
                DeliveryState::OutcomeUnknown,
            )
            .unwrap_or_else(|_| {
                unknown_receipt_message_id(&activity_id)
                    .unwrap_or_else(|_| format!("linear-activity-{activity_id}-unknown"))
            });
            let out = render_outcome(
                cfg,
                &activity_id,
                &receipt_id,
                DeliveryState::OutcomeUnknown,
            );
            let err = format!(
                "error: linear: delivery outcome unknown ({reason}); safe to retry with identical inputs\n"
            )
            .into_bytes();
            (75, out, err)
        }
        Err(ApiError::Rejected(reason)) => {
            (1, vec![], format!("error: linear: {reason}\n").into_bytes())
        }
    }
}
