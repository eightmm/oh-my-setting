use super::{Json, bytes, field, is_text, py_string};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

struct IdentifiedContext {
    digest: Option<[u8; 32]>,
    bytes: u64,
    conflicting: bool,
}

/// A read-only projection of provider-recorded attachments. Neither hook stdout
/// nor an occurrence of the type name in ordinary conversation is delivery evidence.
#[derive(Default)]
pub(super) struct AsyncContext {
    rows: u64,
    context_rows: u64,
    recorded_bytes: u64,
    without_context: u64,
    invalid_context: u64,
    unidentified: u64,
    duplicates: u64,
    identity_limit_rows: u64,
    identities: BTreeMap<[u8; 32], IdentifiedContext>,
}

fn identity(value: &Json) -> Option<&[u8]> {
    bytes(value).filter(|s| {
        !s.is_empty()
            && s.len() <= 128
            && s.iter()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(b))
    })
}

fn digest(parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}

impl AsyncContext {
    pub(super) fn observe(&mut self, row: &Json) {
        let attachment = field(row, "attachment");
        if !is_text(field(row, "type"), "attachment")
            || !is_text(field(attachment, "type"), "async_hook_response")
        {
            return;
        }
        self.rows += 1;
        let output = field(field(attachment, "response"), "hookSpecificOutput");
        let value = field(output, "additionalContext");
        let context = match bytes(value) {
            Some(context) if !context.is_empty() => Some(context),
            Some(_) | None if matches!(value, Json::Null | Json::String(_)) => {
                self.without_context += 1;
                None
            }
            _ => {
                self.invalid_context += 1;
                None
            }
        };
        let size = context.map_or(0, |text| py_string(text).utf8_with_replacement_len() as u64);
        if context.is_some() {
            self.context_rows += 1;
            self.recorded_bytes += size;
        }
        let Some(uuid) = identity(field(row, "uuid")) else {
            self.unidentified += u64::from(context.is_some());
            return;
        };
        // Missing session metadata must not turn a resumed row into a second
        // identified observation alongside its fully identified original.
        let Some(session) = identity(field(row, "sessionId")) else {
            self.unidentified += u64::from(context.is_some());
            return;
        };
        let key = digest(&[session, uuid]);
        let content_digest = context.map(|text| {
            digest(&[
                text,
                bytes(field(attachment, "hookName")).unwrap_or_default(),
                bytes(field(output, "hookEventName")).unwrap_or_default(),
            ])
        });
        // Retain identified empty/malformed responses too: a later nonempty
        // response reusing that identity cannot become unambiguous evidence.
        if let Some(previous) = self.identities.get_mut(&key) {
            if previous.digest == content_digest {
                self.duplicates += u64::from(context.is_some());
            } else {
                previous.conflicting = true;
            }
        } else if self.identities.len() < super::MAX_KEYS {
            self.identities.insert(
                key,
                IdentifiedContext {
                    digest: content_digest,
                    bytes: size,
                    conflicting: false,
                },
            );
        } else {
            self.identity_limit_rows += u64::from(context.is_some());
        }
    }

    pub(super) fn observed_format(&self) -> bool {
        self.rows > 0
    }

    pub(super) fn report(&self) -> Value {
        let valid = self
            .identities
            .values()
            .filter(|v| !v.conflicting && v.digest.is_some());
        json!({
            "status": if self.context_rows > 0 { "observed" } else { "not_observed" },
            "source": "attachment.async_hook_response.response.hookSpecificOutput.additionalContext",
            "response_rows": self.rows,
            "context_rows": self.context_rows,
            "recorded_bytes": self.recorded_bytes,
            "identified_contexts": valid.clone().count(),
            "identified_bytes": valid.map(|v| v.bytes).sum::<u64>(),
            "duplicate_rows": self.duplicates,
            "conflicting_identities": self.identities.values().filter(|v| v.conflicting).count(),
            "unidentified_rows": self.unidentified,
            "identity_limit_rows": self.identity_limit_rows,
            "without_context_rows": self.without_context,
            "invalid_context_rows": self.invalid_context,
            "producer_attribution": "unknown",
            "model_use": "unknown",
            "note": "Provider-recorded asynchronous context, not proof of model use or OMS attribution. Recorded bytes include repeated rows; identified bytes count each unambiguous session/UUID once, requiring both identities. Missing identities and entries beyond the identity limit are not deduplicated. No observation does not establish non-delivery; synchronous hooks and other transcript formats are not counted."
        })
    }
}
