//! Frozen native assignment and untrusted return contracts.
//!
//! Validation proves shape and exact binding, not provider authorship, task
//! satisfaction, child cleanup, permission, acceptance or publication. Producers
//! retain their owner/lease guards and verify evidence bytes under those guards.
use crate::{Error, Result, Value, decode, digest, require};
use std::collections::BTreeSet;
use std::path::{Component, Path};

pub const MAX_ASSIGNMENT: usize = 64 * 1024;
pub const MAX_REPORT: usize = 64 * 1024;
pub const MAX_EVIDENCE_BYTES: u64 = 1024 * 1024;
pub const MAX_PATCH_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REQUIREMENTS: usize = 64;
const MAX_EVIDENCE: usize = 32;

fn fields(value: &Value, required: &[&str], optional: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::new("native contract must be an object"))?;
    require(
        required.iter().all(|key| object.contains_key(*key))
            && object
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str())),
        "native contract has missing or unknown fields",
    )
}

fn text(value: &Value, maximum: usize, empty: bool) -> Result<&str> {
    let text = value
        .as_str()
        .ok_or_else(|| Error::new("native contract needs text"))?;
    require(
        text.len() <= maximum
            && (empty || !text.trim().is_empty())
            && !text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "native contract text is empty, oversized or contains controls",
    )?;
    Ok(text)
}

fn ident(value: &Value) -> Result<&str> {
    let id = text(value, 160, false)?;
    require(
        id.as_bytes()[0].is_ascii_alphanumeric()
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c)),
        "invalid native contract identifier",
    )?;
    Ok(id)
}

fn criterion_id(value: &Value) -> Result<&str> {
    let id = text(value, 160, false)?;
    require(
        id.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c)),
        "invalid native criterion identifier",
    )?;
    Ok(id)
}

fn sha(value: &Value) -> Result<&str> {
    let value = text(value, 64, false)?;
    require(crate::hex(value, 64), "invalid native contract digest")?;
    Ok(value)
}

fn hash(value: &Value) -> Result<String> {
    Ok(digest(&crate::value::encoded(value)?))
}

fn lineage(value: &Value, prefix: &str) -> Result<()> {
    require(
        value
            .as_str()
            .and_then(|s| s.strip_prefix(prefix))
            .is_some_and(|s| crate::hex(s, 32)),
        "invalid native plan or lease identity",
    )
}

fn list(value: &Value, maximum: usize) -> Result<&[Value]> {
    let values = value
        .as_array()
        .ok_or_else(|| Error::new("native contract needs an array"))?;
    require(
        values.len() <= maximum,
        "native contract array exceeds limit",
    )?;
    Ok(values)
}

fn scopes(task: &Value, key: &str, required: bool) -> Result<Value> {
    let absent = serde_json::json!([]);
    let values = list(task.get(key).unwrap_or(&absent), 32)?;
    require(
        !required || !values.is_empty(),
        "native assignment needs explicit scope",
    )?;
    let mut seen = BTreeSet::new();
    for value in values {
        let path = text(value, 4096, false)?;
        require(
            oms_contract_markdown::scope::normalize(path).map_err(Error::new)? == path
                && seen.insert(path),
            "native scope must be normalized and unique",
        )?;
        oms_contract_markdown::scope::validate_pattern(path).map_err(Error::new)?;
    }
    Ok(Value::Array(values.to_vec()))
}

/// Lifecycle progress is checked separately by the producer under its locks.
/// Contract, lease, owner and dependency fields remain exact across review and
/// genuine completion; changing a verifier or re-claiming never reuses a report.
fn stable_task(task: &Value) -> Result<Value> {
    let mut task = task.clone();
    let object = task
        .as_object_mut()
        .ok_or_else(|| Error::new("native assignment needs a task object"))?;
    for field in [
        "claim_age_s",
        "task_sha256",
        "created",
        "updated",
        "claimed_at",
        "state",
        "stored_state",
        "claim_expired",
        "artifact",
        "patch",
        "reason",
        "review_lease_id",
        "repair_count",
        "repair_artifact",
        "history",
        "landing",
        "landed_commit",
        "panel_accept",
        "review_evidence",
        "completion_kind",
        "completion_receipt",
        "completion_verified",
        "cancellation",
        "pr",
        "issue_cache",
    ] {
        object.remove(field);
    }
    Ok(task)
}

pub fn stable_task_sha256(task: &Value) -> Result<String> {
    hash(&stable_task(task)?)
}

/// Validate this opt-in path without changing generic Plan creation. A missing
/// role freezes the declared worker default and confers no capability.
pub fn execution_contract(task: &Value) -> Result<Value> {
    let task = stable_task(task)?;
    require(
        crate::value::encoded(&task)?.len() <= MAX_ASSIGNMENT,
        "native task snapshot exceeds byte limit",
    )?;
    let id = ident(&task["id"])?;
    let title = text(&task["title"], 2048, false)?;
    let verify = text(&task["verify"], 16 * 1024, false)?;
    for value in [task.get("role"), task["assignment"].get("role")]
        .into_iter()
        .flatten()
    {
        text(value, 160, true)?;
    }
    let role = match task.get("role").and_then(Value::as_str) {
        Some(role) if !role.trim().is_empty() => role,
        _ => match task["assignment"].get("role").and_then(Value::as_str) {
            Some(role) if !role.trim().is_empty() => role,
            _ => "worker",
        },
    };
    require(
        role.len() <= 160 && !role.chars().any(char::is_control),
        "native assignment role must be bounded declared intent",
    )?;
    let allowed = scopes(&task, "allowed_paths", true)?;
    let forbidden = scopes(&task, "forbidden_paths", false)?;
    let mut requirements = Vec::new();
    let mut sources = Vec::new();
    for key in ["covers", "requirements"] {
        if let Some(value) = task.get(key) {
            let values = list(value, MAX_REQUIREMENTS)?;
            require(
                !values.is_empty(),
                "native task criterion inventory is empty",
            )?;
            sources.push(key);
            let mut seen = BTreeSet::new();
            for item in values {
                let id = criterion_id(if key == "requirements" && item.is_object() {
                    &item["id"]
                } else {
                    item
                })?;
                require(seen.insert(id), "native task has duplicate criterion ids")?;
                if !requirements.iter().any(|known| known == id) {
                    requirements.push(id.to_owned());
                }
            }
        }
    }
    if requirements.is_empty() {
        requirements.push(id.to_owned());
        sources.push("task-title");
    }
    require(
        requirements.len() <= MAX_REQUIREMENTS,
        "native task has too many criteria",
    )?;
    Ok(serde_json::json!({
        "schema":1, "kind":"oms-native-execution-contract",
        "task_sha256":hash(&task)?, "role":role, "deliverable":title,
        "allowed_paths":allowed, "forbidden_paths":forbidden, "verify":verify,
        "requirements":requirements, "requirements_source":sources.join("+")
    }))
}

/// Private checked fields prevent bypassing the frozen task/checkout validator.
#[derive(Clone, Debug)]
pub struct CheckedAssignment {
    packet: Value,
    sha256: String,
    checkout_sha256: String,
}

impl CheckedAssignment {
    /// Legacy prose contracts remain unknown; malformed typed contracts fail
    /// closed. The digest covers exact stored bytes, not a reencoding.
    pub fn parse(raw: &[u8]) -> Result<Option<Self>> {
        let packet = decode(raw, MAX_ASSIGNMENT)?;
        require(
            packet["schema"].as_u64() == Some(1) && packet["kind"] == "oms-native-assignment",
            "unsupported native assignment packet",
        )?;
        let contract = &packet["result_contract"];
        if contract.is_null() || contract.is_string() {
            return Ok(None);
        }
        require(
            *contract == execution_contract(&packet["task"])?,
            "native assignment execution contract differs from its frozen task",
        )?;
        let task = &packet["task"];
        lineage(&task["plan_id"], "plan_")?;
        lineage(&task["lease_id"], "lease_")?;
        require(
            task["lease_epoch"].as_u64().is_some_and(|v| v > 0)
                && matches!(task["state"].as_str(), Some("claimed" | "running"))
                && task["claim_expired"] == false
                && task["contract_stale"] == false,
            "native assignment needs a current claimed task contract",
        )?;
        ident(&packet["attempt_id"])?;
        ident(&packet["parent_attempt_id"])?;
        ident(&packet["room"])?;
        ident(&packet["provider"])?;
        ident(&task["provider"])?;
        require(
            packet["parent_attempt_id"] == task["claimed_by_participant"],
            "native assignment claim owner differs from its parent",
        )?;
        let checkout = &packet["checkout"];
        for field in ["cwd", "worktree"] {
            let path = text(&checkout[field], 4096, false)?;
            require(
                Path::new(path).is_absolute()
                    && !path.chars().any(char::is_control)
                    && !path.ends_with('/')
                    && !path.contains("//")
                    && !path.split('/').any(|part| matches!(part, "." | ".."))
                    && Path::new(path)
                        .components()
                        .all(|p| matches!(p, Component::RootDir | Component::Normal(_))),
                "native checkout must be an absolute normalized path",
            )?;
        }
        let base = text(&checkout["base_sha"], 64, false)?;
        require(
            crate::hex(base, 40) || crate::hex(base, 64),
            "native checkout needs an exact base commit",
        )?;
        let round = packet.get("round").map_or(Some(1), Value::as_u64);
        require(
            round.is_some_and(|r| r > 0),
            "invalid native assignment round",
        )?;
        if round == Some(1) {
            require(
                packet["previous_report_sha256"].is_null(),
                "initial native assignment cannot name a previous report",
            )?;
        } else {
            sha(&packet["previous_report_sha256"])?;
        }
        Ok(Some(Self {
            checkout_sha256: hash(checkout)?,
            packet,
            sha256: digest(raw),
        }))
    }

    pub fn packet(&self) -> &Value {
        &self.packet
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn task(&self) -> &Value {
        &self.packet["task"]
    }
    pub fn verify(&self) -> &str {
        self.packet["result_contract"]["verify"].as_str().unwrap()
    }
    pub fn role(&self) -> &str {
        self.packet["result_contract"]["role"].as_str().unwrap()
    }
    pub fn requirement_ids(&self) -> Vec<&str> {
        self.packet["result_contract"]["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect()
    }
    pub fn attempt_id(&self) -> &str {
        self.packet["attempt_id"].as_str().unwrap()
    }
    pub fn parent_attempt_id(&self) -> &str {
        self.packet["parent_attempt_id"].as_str().unwrap()
    }
    pub fn room(&self) -> &str {
        self.packet["room"].as_str().unwrap()
    }
    pub fn plan_id(&self) -> &str {
        self.task()["plan_id"].as_str().unwrap()
    }
    pub fn task_id(&self) -> &str {
        self.task()["id"].as_str().unwrap()
    }
    pub fn lease_id(&self) -> &str {
        self.task()["lease_id"].as_str().unwrap()
    }
    pub fn lease_epoch(&self) -> u64 {
        self.task()["lease_epoch"].as_u64().unwrap()
    }
    pub fn task_sha256(&self) -> &str {
        self.packet["result_contract"]["task_sha256"]
            .as_str()
            .unwrap()
    }
    pub fn owner(&self) -> &str {
        self.task()["provider"].as_str().unwrap()
    }
    pub fn checkout(&self) -> &Value {
        &self.packet["checkout"]
    }
    pub fn checkout_sha256(&self) -> &str {
        &self.checkout_sha256
    }
    pub fn base_sha(&self) -> &str {
        self.checkout()["base_sha"].as_str().unwrap()
    }
    pub fn round(&self) -> u64 {
        self.packet["round"].as_u64().unwrap_or(1)
    }
    pub fn matches_task(&self, current: &Value) -> Result<bool> {
        Ok(stable_task_sha256(current)? == self.task_sha256())
    }

    fn identity(&self) -> Value {
        serde_json::json!({
            "assignment_sha256":self.sha256(), "attempt_id":self.attempt_id(),
            "plan_id":self.plan_id(), "task_id":self.task_id(),
            "task_sha256":self.task_sha256(), "lease_id":self.lease_id(),
            "lease_epoch":self.lease_epoch(), "parent_attempt_id":self.parent_attempt_id(),
            "owner":self.owner(), "room":self.room(),
            "checkout_sha256":self.checkout_sha256(), "base_sha":self.base_sha()
        })
    }

    /// A fillable inventory, not a completed report. No absolute checkout path
    /// or executable report command crosses into the return template.
    pub fn report_template(&self) -> Value {
        let mut report = self.identity();
        let fields = serde_json::json!({
            "schema":1, "kind":"oms-native-return", "revision":1,
            "previous_report_sha256":null, "outcome":"reported", "summary":"",
            "changed_paths":[], "evidence":[], "checks":[], "candidate_patch":null,
            "requirements":self.requirement_ids().iter().map(|id|
                serde_json::json!({"id":id,"status":"unverified","evidence":[],"note":""})
            ).collect::<Vec<_>>()
        });
        report
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        report
    }

    fn path_allowed(&self, path: &str, artifact: bool) -> Result<()> {
        require(
            path != "."
                && path.len() <= 4096
                && oms_contract_markdown::scope::normalize(path).map_err(Error::new)? == path
                && !oms_contract_markdown::scope::has_glob(path),
            "native report path must be a normalized literal repo-relative file",
        )?;
        require(
            !matches!(path.split('/').next(), Some(".git"))
                && (!path.starts_with(".oms/") || artifact && path.starts_with(".oms/artifacts/"))
                && path != ".oms",
            "native report cannot name private control state",
        )?;
        let contract = &self.packet["result_contract"];
        let rules = |key: &str| -> Vec<&str> {
            contract[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|rule| rule.as_str().unwrap())
                .collect()
        };
        use oms_contract_markdown::scope::{Classification, Classifier};
        let classification = Classifier::new(&rules("allowed_paths"), &rules("forbidden_paths"))
            .map_err(Error::new)?
            .classify(path);
        require(
            classification != Classification::Forbidden
                && (classification == Classification::Allowed
                    || artifact && path.starts_with(".oms/artifacts/")),
            "native report path is outside its frozen scope",
        )
    }
}

#[derive(Clone, Debug)]
pub struct CheckedReport {
    value: Value,
    raw: Vec<u8>,
    sha256: String,
}

impl CheckedReport {
    /// Read-side validation does not approve a revision. The ingest producer
    /// additionally calls check_report against the latest stored receipt.
    pub fn parse_bound(raw: &[u8], assignment: &CheckedAssignment) -> Result<Self> {
        let value = decode(raw, MAX_REPORT)?;
        let identity = assignment.identity();
        let mut required: Vec<_> = identity
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        required.extend([
            "schema",
            "kind",
            "revision",
            "previous_report_sha256",
            "outcome",
            "summary",
            "requirements",
            "checks",
            "evidence",
            "changed_paths",
        ]);
        fields(&value, &required, &["candidate_patch"])?;
        require(
            value["schema"].as_u64() == Some(1) && value["kind"] == "oms-native-return",
            "unsupported native return envelope",
        )?;
        require(
            identity
                .as_object()
                .unwrap()
                .iter()
                .all(|(key, expected)| value[key] == *expected),
            "native return differs from its exact assignment binding",
        )?;
        let revision = value["revision"]
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or_else(|| Error::new("invalid native report revision"))?;
        if revision == 1 {
            require(
                value["previous_report_sha256"].is_null(),
                "initial report names a prior revision",
            )?;
        } else {
            sha(&value["previous_report_sha256"])?;
        }
        require(
            matches!(
                value["outcome"].as_str(),
                Some("reported" | "blocked" | "needs-decision")
            ),
            "native report outcome grants no completion or acceptance",
        )?;
        text(&value["summary"], 4096, false)?;
        let mut changed = BTreeSet::new();
        for item in list(&value["changed_paths"], 128)? {
            let path = text(item, 4096, false)?;
            assignment.path_allowed(path, false)?;
            require(changed.insert(path), "duplicate native changed path")?;
        }
        let candidate = if value["candidate_patch"].is_null() {
            None
        } else {
            let path = text(&value["candidate_patch"], 4096, false)?;
            require(
                path.ends_with(".patch"),
                "native candidate must name patch evidence",
            )?;
            Some(path)
        };
        let mut evidence = BTreeSet::new();
        for item in list(&value["evidence"], MAX_EVIDENCE)? {
            fields(item, &["path", "sha256", "bytes"], &[])?;
            let path = text(&item["path"], 4096, false)?;
            assignment.path_allowed(path, true)?;
            sha(&item["sha256"])?;
            let maximum = if candidate == Some(path) {
                MAX_PATCH_BYTES
            } else {
                MAX_EVIDENCE_BYTES
            };
            require(
                item["bytes"].as_u64().is_some_and(|n| n <= maximum) && evidence.insert(path),
                "invalid or duplicate native evidence reference",
            )?;
        }
        require(
            candidate.is_none_or(|path| evidence.contains(path)),
            "native candidate patch is missing its exact evidence reference",
        )?;
        let references = |value: &Value| -> Result<()> {
            let mut seen = BTreeSet::new();
            for reference in list(value, MAX_EVIDENCE)? {
                let path = text(reference, 4096, false)?;
                require(
                    evidence.contains(path) && seen.insert(path),
                    "native claim references missing or duplicate evidence",
                )?;
            }
            Ok(())
        };
        let mut requirements = BTreeSet::new();
        for row in list(&value["requirements"], MAX_REQUIREMENTS)? {
            fields(row, &["id", "status", "evidence", "note"], &[])?;
            require(
                requirements.insert(criterion_id(&row["id"])?),
                "duplicate native requirement",
            )?;
            require(
                matches!(
                    row["status"].as_str(),
                    Some("addressed" | "blocked" | "unverified")
                ),
                "invalid native requirement claim",
            )?;
            references(&row["evidence"])?;
            text(&row["note"], 2048, true)?;
        }
        require(
            requirements == assignment.requirement_ids().into_iter().collect(),
            "native report must inventory every frozen requirement exactly",
        )?;
        let mut checks = BTreeSet::new();
        for row in list(&value["checks"], 32)? {
            fields(row, &["name", "status", "evidence", "note"], &[])?;
            require(
                checks.insert(text(&row["name"], 240, false)?),
                "duplicate native check",
            )?;
            require(
                matches!(
                    row["status"].as_str(),
                    Some("passed" | "failed" | "not-run" | "unknown")
                ),
                "invalid native check claim",
            )?;
            references(&row["evidence"])?;
            text(&row["note"], 2048, true)?;
        }
        crate::outbound(raw)?;
        crate::outbound(&crate::value::encoded(&value)?)?;
        Ok(Self {
            value,
            raw: raw.to_vec(),
            sha256: digest(raw),
        })
    }

    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn outcome(&self) -> &str {
        self.value["outcome"].as_str().unwrap()
    }
    pub fn summary(&self) -> &str {
        self.value["summary"].as_str().unwrap()
    }
    pub fn assignment_sha256(&self) -> &str {
        self.value["assignment_sha256"].as_str().unwrap()
    }
    pub fn revision(&self) -> u64 {
        self.value["revision"].as_u64().unwrap()
    }
    pub fn candidate_patch(&self) -> Option<&str> {
        self.value["candidate_patch"].as_str()
    }
}

#[derive(Clone, Debug)]
pub enum ReportChange {
    New(CheckedReport),
    Replayed(CheckedReport),
    Revised(CheckedReport),
}
impl ReportChange {
    pub fn report(&self) -> &CheckedReport {
        match self {
            Self::New(r) | Self::Replayed(r) | Self::Revised(r) => r,
        }
    }
}

/// expected_previous comes from Main's reviewed operation, never from the
/// report itself. Publication still requires a producer-side CAS.
pub fn check_report(
    raw: &[u8],
    assignment: &CheckedAssignment,
    prior: Option<&CheckedReport>,
    expected_previous: Option<&str>,
) -> Result<ReportChange> {
    let report = CheckedReport::parse_bound(raw, assignment)?;
    let Some(prior) = prior else {
        require(
            report.revision() == 1 && expected_previous.is_none(),
            "first native report cannot skip revision or previous receipt",
        )?;
        return Ok(ReportChange::New(report));
    };
    require(
        prior.assignment_sha256() == assignment.sha256(),
        "prior report belongs to another assignment round",
    )?;
    if report.sha256() == prior.sha256() {
        require(
            expected_previous.is_none_or(|v| v == prior.sha256()),
            "native report replay expected another receipt",
        )?;
        return Ok(ReportChange::Replayed(report));
    }
    require(
        expected_previous == Some(prior.sha256())
            && report.value["previous_report_sha256"] == prior.sha256()
            && prior.revision().checked_add(1) == Some(report.revision()),
        "changed native report needs an explicitly reviewed next revision",
    )?;
    Ok(ReportChange::Revised(report))
}

/// Read-only guidance for one assignment, never a scheduler or execution
/// authority. Every returned report leaves the next decision with Main.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextAction {
    Send,
    Reconcile,
    Wait,
    Review,
    ResolveBlocker,
    Decide,
}
impl NextAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Reconcile => "reconcile",
            Self::Wait => "wait",
            Self::Review => "review",
            Self::ResolveBlocker => "resolve-blocker",
            Self::Decide => "decide",
        }
    }
    pub fn reason(self) -> &'static str {
        match self {
            Self::Send => "native assignment has a durable unsent intent",
            Self::Reconcile => "native input submission is unconfirmed; do not resend",
            Self::Wait => "native assignment awaits a bound report",
            Self::Review => "Main must review the returned claims and run the frozen verifier",
            Self::ResolveBlocker => "Main must resolve the reported blocker before another round",
            Self::Decide => "Main must decide the reported scope or authority question",
        }
    }
}
pub fn next_action(transport: &str, report: Option<&CheckedReport>) -> NextAction {
    if let Some(report) = report {
        return match report.outcome() {
            "blocked" => NextAction::ResolveBlocker,
            "needs-decision" => NextAction::Decide,
            _ => NextAction::Review,
        };
    }
    match transport {
        "intent" => NextAction::Send,
        "submitted" => NextAction::Wait,
        _ => NextAction::Reconcile,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn packet() -> Value {
        let task = json!({
            "id":"task-one", "title":"Implement the bounded parser", "role":"builder",
            "plan_id":format!("plan_{}", "a".repeat(32)),
            "lease_id":format!("lease_{}", "b".repeat(32)), "lease_epoch":1,
            "provider":"codex", "claimed_by_participant":"main-one",
            "state":"claimed", "claim_expired":false, "contract_stale":false,
            "allowed_paths":["src"], "forbidden_paths":["src/private"],
            "verify":"cargo test -p parser", "covers":["criterion-one", "_criterion-two"]
        });
        json!({
            "schema":1, "kind":"oms-native-assignment", "attempt_id":"native-one",
            "parent_attempt_id":"main-one", "room":"room-one", "provider":"codex",
            "checkout":{"cwd":"/work/parser", "worktree":"/work/parser",
                "base_sha":"c".repeat(40), "write_isolation":"unmanaged"},
            "result_contract":execution_contract(&task).unwrap(), "task":task
        })
    }
    fn checked(value: &Value) -> CheckedAssignment {
        CheckedAssignment::parse(&serde_json::to_vec(value).unwrap())
            .unwrap()
            .unwrap()
    }
    fn report(assignment: &CheckedAssignment) -> Value {
        let mut value = assignment.report_template();
        value["summary"] = json!("Parser implementation returned for Main review");
        value["changed_paths"] = json!(["src/parser.rs"]);
        value["evidence"] = json!([{"path":"src/parser.rs","sha256":digest(b"code"),"bytes":4}]);
        value["requirements"][0]["status"] = json!("addressed");
        value["requirements"][0]["evidence"] = json!(["src/parser.rs"]);
        value["checks"] = json!([{"name":"frozen verifier","status":"passed",
            "evidence":["src/parser.rs"],"note":"Claimed by Lead; Main has not verified"}]);
        value
    }
    fn bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }

    #[test]
    fn frozen_execution_contract_preserves_requirements_and_legacy_unknown() {
        let mut value = packet();
        let assignment = checked(&value);
        assert_eq!(
            assignment.requirement_ids(),
            vec!["criterion-one", "_criterion-two"]
        );
        assert_eq!(assignment.verify(), "cargo test -p parser");
        assert_eq!(assignment.role(), "builder");
        let mut task = assignment.task().clone();
        for (name, value) in [
            ("claimed_at", json!("changed heartbeat")),
            ("claim_age_s", json!(100)),
            ("state", json!("review")),
            ("artifact", json!(".oms/artifacts/review.json")),
            ("patch", json!(".oms/artifacts/candidate.patch")),
            ("review_lease_id", task["lease_id"].clone()),
            ("review_evidence", json!({"schema":1})),
            (
                "cancellation",
                json!({"reason":"owner cancelled the registered task"}),
            ),
        ] {
            task[name] = value;
        }
        assert!(assignment.matches_task(&task).unwrap());
        task["verify"] = json!("different verifier");
        assert!(!assignment.matches_task(&task).unwrap());
        value["task"].as_object_mut().unwrap().remove("role");
        value["task"].as_object_mut().unwrap().remove("covers");
        value["result_contract"] = execution_contract(&value["task"]).unwrap();
        let fallback = checked(&value);
        assert_eq!(fallback.role(), "worker");
        assert_eq!(fallback.requirement_ids(), vec!["task-one"]);
        assert_eq!(
            value["result_contract"]["requirements_source"],
            "task-title"
        );
        value["result_contract"] = json!("legacy prose");
        assert!(CheckedAssignment::parse(&bytes(&value)).unwrap().is_none());
        value["result_contract"] = json!({"schema":2});
        assert!(CheckedAssignment::parse(&bytes(&value)).is_err());
    }

    #[test]
    fn reports_use_directional_scope_and_scan_decoded_text() {
        let mut packet = packet();
        packet["task"]["allowed_paths"] = json!(["src/parser.rs"]);
        packet["result_contract"] = execution_contract(&packet["task"]).unwrap();
        let assignment = checked(&packet);
        let mut value = report(&assignment);
        value["changed_paths"] = json!(["src"]);
        assert!(
            CheckedReport::parse_bound(&bytes(&value), &assignment).is_err(),
            "ancestor of a narrowly allowed file is outside"
        );
        let mut value = report(&assignment);
        value["summary"] = "ESCAPED_SECRET".into();
        let raw = String::from_utf8(bytes(&value)).unwrap().replace(
            "ESCAPED_SECRET",
            r"\u0067\u0068\u0070_12345678901234567890abcd",
        );
        assert!(
            crate::outbound(raw.as_bytes()).is_ok(),
            "the raw JSON obscures the decoded fixture token"
        );
        assert!(CheckedReport::parse_bound(raw.as_bytes(), &assignment).is_err());
        packet["task"]["forbidden_paths"] = json!([".oms/artifacts/private"]);
        packet["result_contract"] = execution_contract(&packet["task"]).unwrap();
        let assignment = checked(&packet);
        let mut value = assignment.report_template();
        value["summary"] = "Claim only".into();
        value["evidence"] =
            json!([{"path":".oms/artifacts/private/check.txt","sha256":"a".repeat(64),"bytes":1}]);
        assert!(
            CheckedReport::parse_bound(&bytes(&value), &assignment).is_err(),
            "artifact exception must not bypass forbidden scope"
        );
    }

    #[test]
    fn new_assignments_refuse_incomplete_or_rebound_execution_contracts() {
        let original = packet();
        for (key, replacement) in [
            ("verify", json!("")),
            ("allowed_paths", json!([])),
            ("allowed_paths", json!(["../elsewhere"])),
            ("covers", json!(["criterion-one", "criterion-one"])),
            ("role", json!(true)),
        ] {
            let mut task = original["task"].clone();
            task[key] = replacement;
            assert!(execution_contract(&task).is_err(), "{key}");
        }
        for (key, replacement) in [
            ("parent_attempt_id", json!("another-main")),
            ("attempt_id", json!("")),
            ("round", json!(2)),
        ] {
            let mut value = original.clone();
            value[key] = replacement;
            assert!(CheckedAssignment::parse(&bytes(&value)).is_err(), "{key}");
        }
        let assignment = checked(&original);
        for key in [
            "id",
            "plan_id",
            "lease_id",
            "lease_epoch",
            "provider",
            "claimed_by_participant",
            "title",
            "allowed_paths",
            "forbidden_paths",
            "verify",
            "covers",
            "depends",
            "contracts",
        ] {
            let mut task = original["task"].clone();
            task[key] = json!("changed");
            assert!(!assignment.matches_task(&task).unwrap(), "{key}");
        }
        let mut value = original.clone();
        value["task"]["verify"] = json!("changed");
        assert!(CheckedAssignment::parse(&bytes(&value)).is_err());
        value = original;
        value["checkout"]["base_sha"] = json!("HEAD");
        assert!(CheckedAssignment::parse(&bytes(&value)).is_err());
    }

    #[test]
    fn reports_bind_all_identity_and_inventory_but_grant_no_acceptance() {
        let assignment = checked(&packet());
        let value = report(&assignment);
        let parsed = CheckedReport::parse_bound(&bytes(&value), &assignment).unwrap();
        assert_eq!(parsed.outcome(), "reported");
        assert_eq!(next_action("submitted", Some(&parsed)), NextAction::Review);
        assert!(
            !bytes(&assignment.report_template())
                .windows(5)
                .any(|v| v == b"/work")
        );
        for key in assignment.identity().as_object().unwrap().keys() {
            let mut wrong = value.clone();
            wrong[key] = if key == "lease_epoch" {
                json!(2)
            } else {
                json!("other")
            };
            assert!(
                CheckedReport::parse_bound(&bytes(&wrong), &assignment).is_err(),
                "{key}"
            );
        }
        for (key, replacement) in [
            ("outcome", json!("accepted")),
            ("outcome", json!("completed")),
            ("requirements", json!([])),
            ("changed_paths", json!(["../escape"])),
            ("changed_paths", json!(["src/private/secret"])),
            ("changed_paths", json!(["other/file"])),
            ("changed_paths", json!([".git/config"])),
        ] {
            let mut wrong = value.clone();
            wrong[key] = replacement;
            assert!(
                CheckedReport::parse_bound(&bytes(&wrong), &assignment).is_err(),
                "{key}"
            );
        }
        let mut wrong = value.clone();
        wrong["checks"][0]["command"] = json!("unreviewed command");
        assert!(CheckedReport::parse_bound(&bytes(&wrong), &assignment).is_err());
        wrong = value;
        wrong["requirements"][0]["evidence"] = json!(["missing"]);
        assert!(CheckedReport::parse_bound(&bytes(&wrong), &assignment).is_err());
        let mut broad = packet();
        broad["task"]["allowed_paths"] = json!(["."]);
        broad["result_contract"] = execution_contract(&broad["task"]).unwrap();
        let broad = checked(&broad);
        for path in [".git/config", ".oms/plan/tasks.json"] {
            let mut wrong = report(&broad);
            wrong["evidence"][0]["path"] = json!(path);
            wrong["requirements"][0]["evidence"] = json!([path]);
            wrong["checks"][0]["evidence"] = json!([path]);
            assert!(
                CheckedReport::parse_bound(&bytes(&wrong), &broad)
                    .unwrap_err()
                    .message
                    .contains("private control state")
            );
        }
    }

    #[test]
    fn report_replays_and_revisions_require_exact_previous_receipt() {
        let assignment = checked(&packet());
        let mut value = report(&assignment);
        let first = check_report(&bytes(&value), &assignment, None, None).unwrap();
        assert!(matches!(first, ReportChange::New(_)));
        let prior = first.report();
        assert!(matches!(
            check_report(&bytes(&value), &assignment, Some(prior), None).unwrap(),
            ReportChange::Replayed(_)
        ));
        value["summary"] = json!("Updated findings");
        assert!(check_report(&bytes(&value), &assignment, Some(prior), None).is_err());
        value["revision"] = json!(2);
        value["previous_report_sha256"] = json!(prior.sha256());
        assert!(check_report(&bytes(&value), &assignment, Some(prior), None).is_err());
        assert!(
            check_report(
                &bytes(&value),
                &assignment,
                Some(prior),
                Some(&"0".repeat(64))
            )
            .is_err()
        );
        let revised = check_report(
            &bytes(&value),
            &assignment,
            Some(prior),
            Some(prior.sha256()),
        )
        .unwrap();
        assert!(matches!(revised, ReportChange::Revised(_)));
        assert!(CheckedReport::parse_bound(&bytes(&value), &assignment).is_ok());
        let mut next = packet();
        next["round"] = json!(2);
        next["previous_report_sha256"] = json!(revised.report().sha256());
        let next = checked(&next);
        assert!(CheckedReport::parse_bound(&bytes(&value), &next).is_err());
        assert!(check_report(&bytes(&report(&next)), &next, Some(prior), None).is_err());
    }

    #[test]
    fn candidate_patch_is_evidence_without_managed_provenance_or_acceptance() {
        let assignment = checked(&packet());
        let mut value = report(&assignment);
        let path = ".oms/artifacts/native-results/candidate.patch";
        value["candidate_patch"] = json!(path);
        assert!(CheckedReport::parse_bound(&bytes(&value), &assignment).is_err());
        value["evidence"]
            .as_array_mut()
            .unwrap()
            .push(json!({"path":path,"sha256":digest(b"patch"),"bytes":MAX_PATCH_BYTES}));
        let parsed = CheckedReport::parse_bound(&bytes(&value), &assignment).unwrap();
        assert_eq!(parsed.candidate_patch(), Some(path));
        assert_eq!(next_action("submitted", Some(&parsed)), NextAction::Review);
        value["evidence"][1]["bytes"] = json!(MAX_PATCH_BYTES + 1);
        assert!(CheckedReport::parse_bound(&bytes(&value), &assignment).is_err());
    }

    #[test]
    fn bounded_reports_refuse_duplicate_fields_and_keep_all_outcomes_advisory() {
        let assignment = checked(&packet());
        let mut value = report(&assignment);
        for (outcome, action) in [
            ("reported", NextAction::Review),
            ("blocked", NextAction::ResolveBlocker),
            ("needs-decision", NextAction::Decide),
        ] {
            value["outcome"] = json!(outcome);
            let report = CheckedReport::parse_bound(&bytes(&value), &assignment).unwrap();
            assert_eq!(next_action("unknown", Some(&report)), action);
            assert!(!action.reason().is_empty());
        }
        assert_eq!(next_action("intent", None), NextAction::Send);
        assert_eq!(next_action("submitted", None), NextAction::Wait);
        assert_eq!(next_action("unknown", None), NextAction::Reconcile);
        let mut duplicate = bytes(&value);
        duplicate.pop();
        duplicate.extend_from_slice(br#","outcome":"reported"}"#);
        assert!(CheckedReport::parse_bound(&duplicate, &assignment).is_err());
        value["evidence"][0]["bytes"] = json!(MAX_EVIDENCE_BYTES + 1);
        assert!(CheckedReport::parse_bound(&bytes(&value), &assignment).is_err());
        value = report(&assignment);
        value["summary"] = json!("x".repeat(MAX_REPORT));
        assert!(CheckedReport::parse_bound(&bytes(&value), &assignment).is_err());
    }
}
