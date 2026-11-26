//! Bounded request receipts in the canonical Team record. Inputs are data;
//! sender authentication belongs to the owning native channel, never this API.
use crate::json::{self, Value, number, object, string};
use crate::team::AdmissionRequest;
use crate::{Plan, Result, require};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const MAX_REQUEST_BYTES: usize = 8192;
pub const MAX_RECEIPTS: usize = 64;
pub const MAX_RECEIPT_BYTES: usize = 512 * 1024;
pub const MAX_PROPOSED_TASKS: usize = 8;

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
        matches!(record.get("request_schema").int()?, 2 | 3),
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
            record.get("request_schema").is_int(3)
                || receipt.get("request").text("action") == "report_blocker",
            "task proposals require Team request record version 3",
        )?;
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
            receipt.get("outcome")
                == &outcome(
                    receipt.get("request").text("action"),
                    receipt.text("request_sha256"),
                ),
            "invalid Team request outcome",
        )?;
    }
    require(
        json::canonical(record.get("requests"))?.len() <= MAX_RECEIPT_BYTES,
        "Team request bytes limit",
    )
}
fn outcome(action: &str, digest: &str) -> Value {
    object([
        ("schema", number(1)),
        (
            "status",
            string(if action == "propose_tasks" {
                "proposed"
            } else {
                "recorded"
            }),
        ),
        ("request_sha256", string(digest)),
        ("action", string(action)),
    ])
}
pub fn validate_request(request: &Value) -> Result<()> {
    let raw = json::canonical(request)?;
    crate::engine::screen(&raw)?;
    require(
        raw.len() <= MAX_REQUEST_BYTES,
        "Team request exceeds frame bound",
    )?;
    match (request.get("schema").int()?, request.text("action")) {
        (1, "report_blocker") => {
            json::keys(
                request,
                &["schema", "request_id", "action", "text", "expires_at"],
                &[],
            )?;
            bounded_text(request.get("text").str()?, 4000)?;
        }
        (2, "propose_tasks") => {
            json::keys(
                request,
                &[
                    "schema",
                    "request_id",
                    "action",
                    "summary",
                    "expected_task_sha256",
                    "tasks",
                    "expires_at",
                ],
                &[],
            )?;
            json::digest(request.get("expected_task_sha256"))?;
            bounded_text(request.get("summary").str()?, 1000)?;
            let tasks = request.get("tasks").array()?;
            require(
                !tasks.is_empty() && tasks.len() <= MAX_PROPOSED_TASKS,
                "task proposal needs 1..8 tasks",
            )?;
            let mut ids = std::collections::BTreeSet::new();
            for task in tasks {
                json::keys(task, &["id", "title", "allowed", "depends"], &[])?;
                let id = task.get("id").str()?;
                require(
                    crate::engine::valid_id(id) && id.len() <= 160 && ids.insert(id),
                    "invalid or duplicate proposed task id",
                )?;
                bounded_text(task.get("title").str()?, 300)?;
                crate::engine::plain(task.text("title"), "proposed task title")?;
                let paths = task
                    .get("allowed")
                    .strings()?
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>();
                require(
                    !paths.is_empty()
                        && paths.len() <= 32
                        && paths.iter().all(|p| p.len() <= 240)
                        && crate::engine::clean_paths(&paths)? == paths
                        && paths
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == paths.len(),
                    "proposed task paths must be bounded, normalized and unique",
                )?;
            }
            for task in tasks {
                let depends = task.get("depends").strings()?;
                require(
                    depends.len() <= MAX_PROPOSED_TASKS
                        && depends
                            .iter()
                            .all(|id| ids.contains(id) && *id != task.text("id"))
                        && depends
                            .iter()
                            .copied()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == depends.len(),
                    "proposal dependencies must name unique other proposed tasks",
                )?;
            }
            let mut resolved = std::collections::BTreeSet::new();
            loop {
                let before = resolved.len();
                for task in tasks {
                    if task
                        .get("depends")
                        .strings()?
                        .iter()
                        .all(|id| resolved.contains(id))
                    {
                        resolved.insert(task.text("id"));
                    }
                }
                if resolved.len() == tasks.len() {
                    break;
                }
                require(resolved.len() > before, "task proposal dependency cycle")?;
            }
        }
        _ => return Err(crate::error("unsupported Team request schema/action")),
    }
    let id = request.text("request_id");
    require(
        !id.is_empty()
            && id.len() <= 160
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)),
        "invalid Team request identity",
    )?;
    require(
        request.get("expires_at").int()? > 0,
        "invalid Team request expiry",
    )
}
fn bounded_text(text: &str, maximum: usize) -> Result<()> {
    require(
        !text.trim().is_empty()
            && text.len() <= maximum
            && !text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "invalid bounded Team request text",
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
        require(
            request.text("action") == "report_blocker",
            "report writer accepts only report_blocker",
        )?;
        self.record_team_request_before(admission, expected_binding, request, source, deadline)
    }
    /// Records an authenticated consumer's bounded proposal/report as data.
    /// Proposed tasks do not enter the task graph or authorize their execution.
    pub fn record_team_request_before(
        &self,
        admission: AdmissionRequest,
        expected_binding: &str,
        request: &Value,
        source: &RequestEnrollment<'_>,
        deadline: Instant,
    ) -> Result<Value> {
        validate_request(request)?;
        let guard = self.team_admission_before(admission.clone(), deadline)?;
        self.record_team_request_with_guard(&guard, &admission, expected_binding, request, source)
    }
    pub fn record_team_request_bounded(
        &self,
        admission: AdmissionRequest,
        expected_binding: &str,
        request: &Value,
        source: &RequestEnrollment<'_>,
        deadline: Instant,
        bounds: crate::team::AdmissionBounds,
    ) -> Result<Value> {
        validate_request(request)?;
        let guard = self.team_admission_bounded(admission.clone(), deadline, bounds)?;
        self.record_team_request_with_guard(&guard, &admission, expected_binding, request, source)
    }
    fn record_team_request_with_guard(
        &self,
        guard: &crate::team::AdmissionGuard<'_, '_>,
        admission: &AdmissionRequest,
        expected_binding: &str,
        request: &Value,
        source: &RequestEnrollment<'_>,
    ) -> Result<Value> {
        let deadline = guard.deadline();
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
        if request.text("action") == "propose_tasks" {
            require(
                request.text("expected_task_sha256") == guard.binding().text("task_sha256"),
                "task proposal parent task changed",
            )?;
            let declared = guard.binding().get("allowed_paths").strings()?;
            let envelope = if declared.is_empty() {
                vec!["."]
            } else {
                declared
            };
            let forbidden = guard.binding().get("forbidden_paths").strings()?;
            for task in request.get("tasks").array()? {
                require(
                    !snapshot
                        .tasks()?
                        .iter()
                        .any(|(id, _)| id == task.text("id").as_bytes()),
                    "proposed task id already exists in the canonical plan",
                )?;
                let paths = task.get("allowed").strings()?;
                for path in &paths {
                    require(
                        oms_contract_markdown::scope::pattern_within_envelope(path, &envelope)
                            .map_err(crate::error)?,
                        "task proposal widens Team scope",
                    )?;
                }
                require(
                    forbidden.is_empty()
                        || !oms_contract_markdown::scope::scopes_overlap(
                            Some(&paths),
                            Some(&forbidden),
                        ),
                    "task proposal intersects or cannot prove separation from forbidden scope",
                )?;
            }
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
        let result = outcome(request.text("action"), &digest);
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
        let version = if request.text("action") == "propose_tasks"
            || record.get("request_schema").is_int(3)
        {
            3
        } else {
            2
        };
        record.set("request_schema", number(version))?;
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
                "Team request may already be recorded; reconcile retained receipt before retry: {}",
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
            ("outcome", outcome("report_blocker", &digest)),
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
        record.set("request_schema", number(4)).unwrap();
        assert!(validate_record(&record).is_err());
        let proposal = object([
            ("schema", number(2)),
            ("request_id", string("proposal-one")),
            ("action", string("propose_tasks")),
            ("summary", string("Review a bounded decomposition")),
            ("expected_task_sha256", string("d".repeat(64))),
            ("expires_at", number(100)),
            (
                "tasks",
                Value::Array(vec![object([
                    ("id", string("proposed-a")),
                    ("title", string("Inspect the assigned source")),
                    ("allowed", Value::Array(vec![string("src/lib.rs")])),
                    ("depends", Value::Array(vec![])),
                ])]),
            ),
        ]);
        validate_request(&proposal).unwrap();
        let mut bad = proposal.clone();
        bad.set("dispatch", Value::Bool(true)).unwrap();
        assert!(validate_request(&bad).is_err());
        let mut task = proposal.get("tasks").array().unwrap()[0].clone();
        task.set("depends", Value::Array(vec![string("proposed-a")]))
            .unwrap();
        bad = proposal.clone();
        bad.set("tasks", Value::Array(vec![task])).unwrap();
        assert!(validate_request(&bad).is_err());
        bad.set(
            "tasks",
            Value::Array(vec![proposal.get("tasks").array().unwrap()[0].clone(); 9]),
        )
        .unwrap();
        assert!(validate_request(&bad).is_err());
        let digest = json::hash(&proposal).unwrap();
        let receipt = object([
            (
                "key",
                string(
                    json::hash(&object([
                        ("binding", string(&binding)),
                        ("request_id", proposal.get("request_id").clone()),
                    ]))
                    .unwrap(),
                ),
            ),
            ("binding_sha256", string(&binding)),
            ("request_sha256", string(&digest)),
            ("source_sha256", string("b".repeat(64))),
            ("request", proposal),
            ("outcome", outcome("propose_tasks", &digest)),
        ]);
        record.set("requests", Value::Array(vec![receipt])).unwrap();
        record.set("request_schema", number(2)).unwrap();
        assert!(
            validate_record(&record).is_err(),
            "version 2 cannot carry task proposals"
        );
        record.set("request_schema", number(3)).unwrap();
        validate_record(&record).unwrap();
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
            ("outcome", outcome("report_blocker", &digest)),
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
