//! Bounded report receipts in the canonical Team record. Inputs are data;
//! sender authentication belongs to the owning native channel, never this API.
use crate::json::{self, Value, number, object, string};
use crate::team::AdmissionRequest;
use crate::{Plan, Result, require};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const MAX_REQUEST_BYTES: usize = 8192;
pub const MAX_RECEIPTS: usize = 64;
pub const MAX_RECEIPT_BYTES: usize = 512 * 1024;

/// Correlation facts supplied by the authenticated native consumer, not a
/// credential. This writer independently checks current lifecycle and Room.
pub struct RequestEnrollment<'a> {
    pub attempt: &'a str,
    pub created: &'a str,
    pub participant: &'a str,
    pub room_sequence: i128,
    pub private_binding_sha256: &'a str,
}
pub(crate) fn validate_aggregate(teams: &Value) -> Result<()> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for (_, record) in teams.object()? {
        validate_record(record)?;
        if let Some(rows) = record.opt("requests") {
            count += rows.array()?.len();
            bytes += json::canonical(rows)?.len();
        }
    }
    require(
        count <= MAX_RECEIPTS && bytes <= MAX_RECEIPT_BYTES,
        "aggregate Team request history exceeds bound",
    )
}
pub fn validate_record(record: &Value) -> Result<()> {
    if record.opt("request_schema").is_none() {
        json::keys(record, &["versions", "phase", "reason"], &[])?;
        return Ok(());
    }
    json::keys(
        record,
        &["versions", "phase", "reason", "request_schema", "requests"],
        &[],
    )?;
    require(
        record.get("request_schema").is_int(2),
        "unsupported Team request record version",
    )?;
    let receipts = record.get("requests").array()?;
    require(receipts.len() <= MAX_RECEIPTS, "Team request receipt limit")?;
    let mut keys = std::collections::BTreeSet::new();
    for receipt in receipts {
        json::keys(
            receipt,
            &[
                "key",
                "request_sha256",
                "binding_sha256",
                "request",
                "outcome",
                "source_sha256",
            ],
            &[],
        )?;
        for field in ["key", "request_sha256", "binding_sha256", "source_sha256"] {
            json::digest(receipt.get(field))?;
        }
        require(
            keys.insert(receipt.text("key")),
            "duplicate Team request key",
        )?;
        validate_request(receipt.get("request"))?;
        require(
            receipt.text("key")
                == json::hash(&object([
                    ("binding", receipt.get("binding_sha256").clone()),
                    (
                        "request_id",
                        receipt.get("request").get("request_id").clone(),
                    ),
                ]))?,
            "Team request key changed",
        )?;
        require(
            json::hash(receipt.get("request"))? == receipt.text("request_sha256"),
            "Team request receipt digest changed",
        )?;
        require(
            receipt.get("outcome") == &outcome(receipt.text("request_sha256")),
            "invalid Team request outcome",
        )?;
    }
    require(
        json::canonical(record.get("requests"))?.len() <= MAX_RECEIPT_BYTES,
        "Team request bytes limit",
    )
}
fn outcome(digest: &str) -> Value {
    object([
        ("schema", number(1)),
        ("status", string("recorded")),
        ("request_sha256", string(digest)),
        ("action", string("report_blocker")),
    ])
}
pub fn validate_request(request: &Value) -> Result<()> {
    crate::engine::screen(&json::canonical(request)?)?;
    require(
        json::canonical(request)?.len() <= MAX_REQUEST_BYTES,
        "Team request exceeds frame bound",
    )?;
    json::keys(
        request,
        &["schema", "request_id", "action", "text", "expires_at"],
        &[],
    )?;
    require(
        request.get("schema").is_int(1) && request.text("action") == "report_blocker",
        "unsupported Team request schema/action",
    )?;
    let id = request.text("request_id");
    require(
        !id.is_empty()
            && id.len() <= 160
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)),
        "invalid Team request identity",
    )?;
    let text = request.get("text").str()?;
    require(
        !text.trim().is_empty()
            && text.len() <= 4000
            && !text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "invalid Team blocker text",
    )?;
    require(
        request.get("expires_at").int()? > 0,
        "invalid Team request expiry",
    )
}
impl Plan<'_> {
    /// Parent-only report metadata, not sender authentication or launch authority.
    /// The native consumer must authenticate its actual owned channel first.
    pub fn record_team_report_before(
        &self,
        admission: AdmissionRequest,
        expected_binding: &str,
        request: &Value,
        source: &RequestEnrollment<'_>,
        deadline: Instant,
    ) -> Result<Value> {
        validate_request(request)?;
        let guard = self.team_admission_before(admission.clone(), deadline)?;
        require(
            guard.binding().get("member").text("role") == "lead" && !admission.write,
            "Team requests require a read-only lead",
        )?;
        require(
            json::hash(guard.binding())? == expected_binding,
            "Team request contract changed",
        )?;
        let writer = guard.writer_before()?;
        let raw = writer
            .store
            .read(".oms/lifecycle/events.jsonl", crate::storage::MAX_STREAM)?;
        let projected = writer.services.project_lifecycle(&raw)?;
        let attempt = projected.attempts.get(source.attempt);
        require(
            attempt.text("tool") == "peer-ask"
                && attempt.text("state") == "working"
                && attempt.get("terminal") == &Value::Bool(false)
                && attempt.text("parent_attempt_id") == guard.binding().text("root_owner")
                && attempt.text("task_id") == guard.binding().text("task_id"),
            "Team request lifecycle changed",
        )?;
        require(
            crate::storage::rows(&raw)?.iter().any(|row| {
                row.text("attempt_id") == source.attempt
                    && row.text("event_type") == "attempt.created"
                    && row.text("event_id") == source.created
            }),
            "Team request creation generation changed",
        )?;
        let room_id = guard.binding().text("room_id");
        let room = writer
            .services
            .project_room(
                room_id,
                &writer.store.read(
                    format!(".oms/threads/{room_id}.jsonl"),
                    crate::storage::MAX_STREAM,
                )?,
            )?
            .state;
        require(
            room.get("participants").array()?.iter().any(|p| {
                p.text("participant") == source.participant
                    && p.get("seq").is_int(source.room_sequence)
                    && p.get("joined") == &Value::Bool(true)
                    && p.text("role") == "advisor"
                    && p.text("parent") == guard.binding().text("root_owner")
                    && p.text("provider") == admission.provider
            }),
            "Team request Room enrollment changed",
        )?;
        require(
            json::hex(source.private_binding_sha256, 64),
            "invalid private Team operation reference",
        )?;
        let mut snapshot = writer.load()?;
        let mut teams = snapshot.plan.get("teams").clone();
        let mut count = 0usize;
        let mut bytes = 0usize;
        for (_, record) in teams.object()? {
            validate_record(record)?;
            if let Some(receipts) = record.opt("requests") {
                count += receipts.array()?.len();
                bytes += json::canonical(receipts)?.len();
            }
        }
        let mut record = teams.get(&admission.team_id).clone();
        let mut receipts = record
            .opt("requests")
            .map(Value::array)
            .transpose()?
            .unwrap_or(&[])
            .to_vec();
        let key = json::hash(&object([
            ("binding", string(expected_binding)),
            ("request_id", request.get("request_id").clone()),
        ]))?;
        let digest = json::hash(request)?;
        if let Some(prior) = receipts.iter().find(|row| row.text("key") == key) {
            require(
                prior.text("request_sha256") == digest,
                "Team request identity conflicts with previous body",
            )?;
            guard.check()?;
            return Ok(prior.get("outcome").clone());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| crate::error("Team request clock unavailable"))?
            .as_secs() as i128;
        require(
            request.get("expires_at").int()? > now
                && request.get("expires_at").int()?
                    <= guard.expires_at_unix()?.unwrap_or(0) as i128,
            "Team request expired or exceeds contract expiry",
        )?;
        let result = outcome(&digest);
        let receipt = object([
            ("key", string(&key)),
            ("request_sha256", string(&digest)),
            ("binding_sha256", string(expected_binding)),
            ("request", request.clone()),
            ("outcome", result.clone()),
            ("source_sha256", string(source.private_binding_sha256)),
        ]);
        require(
            count < MAX_RECEIPTS
                && bytes.saturating_add(json::canonical(&receipt)?.len()) <= MAX_RECEIPT_BYTES,
            "Team request history full; existing dedup receipts retained",
        )?;
        receipts.push(receipt);
        record.set("request_schema", number(2))?;
        record.set("requests", Value::Array(receipts))?;
        teams.set(&admission.team_id, record)?;
        validate_aggregate(&teams)?;
        snapshot.plan.set("teams", teams)?;
        guard.check()?;
        if let Some(groups) = &writer.group_snapshot {
            for proof in groups.proofs()? {
                require(
                    writer.services.group_membership_before(
                        proof.gid,
                        deadline,
                        writer.interrupted,
                    )? == proof,
                    "Team request native group proof changed",
                )?;
            }
        }
        guard.check()?;
        writer.save(&mut snapshot)?;
        guard.check().map_err(|e| crate::Error {
            code: e.code,
            message: format!(
                "Team report may already be recorded; reconcile retained receipt before retry: {}",
                e.message
            ),
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_keys_are_reconstructed_and_versions_are_explicit() {
        let legacy = object([
            ("versions", Value::Array(vec![])),
            ("phase", string("planned")),
            ("reason", string("")),
        ]);
        validate_record(&legacy).unwrap();
        let request = object([
            ("schema", number(1)),
            ("request_id", string("same-id")),
            ("action", string("report_blocker")),
            ("text", string("bounded blocker")),
            ("expires_at", number(100)),
        ]);
        let binding = "a".repeat(64);
        let key = json::hash(&object([
            ("binding", string(&binding)),
            ("request_id", string("same-id")),
        ]))
        .unwrap();
        let digest = json::hash(&request).unwrap();
        let mut receipt = object([
            ("key", string(&key)),
            ("binding_sha256", string(&binding)),
            ("request_sha256", string(&digest)),
            ("source_sha256", string("b".repeat(64))),
            ("request", request),
            ("outcome", outcome(&digest)),
        ]);
        let mut record = legacy;
        record.set("request_schema", number(2)).unwrap();
        record
            .set("requests", Value::Array(vec![receipt.clone()]))
            .unwrap();
        validate_record(&record).unwrap();
        let mut sensitive = receipt.get("request").clone();
        sensitive
            .set(
                "text",
                string(["api_", "key=example-value-for-content-screen"].concat()),
            )
            .unwrap();
        assert!(
            validate_request(&sensitive).is_err(),
            "blocker text must pass the persistent-content screen"
        );
        receipt.set("key", string("c".repeat(64))).unwrap();
        record.set("requests", Value::Array(vec![receipt])).unwrap();
        assert!(
            validate_record(&record)
                .unwrap_err()
                .message
                .contains("key changed")
        );
        record.set("request_schema", number(3)).unwrap();
        assert!(validate_record(&record).is_err());
    }
    fn padded_receipt(index: usize, text: &str) -> Value {
        let binding = "a".repeat(64);
        let request = object([
            ("schema", number(1)),
            ("request_id", string(format!("report-{index}"))),
            ("action", string("report_blocker")),
            ("text", string(text)),
            ("expires_at", number(100)),
        ]);
        let digest = json::hash(&request).unwrap();
        object([
            (
                "key",
                string(
                    json::hash(&object([
                        ("binding", string(&binding)),
                        ("request_id", request.get("request_id").clone()),
                    ]))
                    .unwrap(),
                ),
            ),
            ("binding_sha256", string(&binding)),
            ("request_sha256", string(&digest)),
            ("source_sha256", string("b".repeat(64))),
            ("request", request),
            ("outcome", outcome(&digest)),
        ])
    }
    #[test]
    fn aggregate_bound_includes_final_array_separator_before_publication() {
        let next = padded_receipt(63, "b");
        let next_bytes = json::canonical(&next).unwrap().len();
        let target = MAX_RECEIPT_BYTES - next_bytes;
        let mut rows = (0..63)
            .map(|i| padded_receipt(i, &format!("b{}", "\n".repeat(3999))))
            .collect::<Vec<_>>();
        for i in (0..63).rev() {
            let length = json::canonical(&Value::Array(rows.clone())).unwrap().len();
            if length <= target {
                break;
            }
            let diff = length - target;
            let remove = (diff / 2).min(3999);
            let mut text = format!("b{}", "\n".repeat(3999 - remove));
            if remove == diff / 2 && diff % 2 == 1 {
                text.pop();
                text.push('x');
            }
            rows[i] = padded_receipt(i, &text);
        }
        let before = json::canonical(&Value::Array(rows.clone())).unwrap().len();
        assert_eq!(before + next_bytes, MAX_RECEIPT_BYTES);
        let mut record = object([
            ("versions", Value::Array(vec![])),
            ("phase", string("planned")),
            ("reason", string("")),
            ("request_schema", number(2)),
            ("requests", Value::Array(rows.clone())),
        ]);
        validate_aggregate(&object([("team", record.clone())])).unwrap();
        rows.push(next);
        record.set("requests", Value::Array(rows)).unwrap();
        assert!(
            validate_aggregate(&object([("team", record)])).is_err(),
            "the final comma must refuse before CAS"
        );
    }
}
