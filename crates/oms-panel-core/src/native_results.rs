//! Parent-imported native reports retain evidence, never provider completion or acceptance.
use crate::results::{MAX_DRAFT, MAX_RESULT, PATCH_MAX};
use crate::value::text;
use crate::{Error, Result, Store, Transaction, Value, require};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputReference {
    path: PathBuf,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema: u32,
    attempt_id: String,
    binding_sha256: String,
    #[serde(default)]
    previous_sha256: String,
    kind: String,
    summary: String,
    source: InputReference,
    #[serde(default)]
    patch: Option<InputReference>,
    #[serde(default)]
    verification: Vec<InputReference>,
}

pub struct Capture {
    input: Input,
    input_raw: Vec<u8>,
    input_sha256: String,
    source: Vec<u8>,
    patch: Option<Vec<u8>>,
    verification: Vec<Vec<u8>>,
}

fn input_reference(repo: &Path, reference: &InputReference, limit: usize) -> Result<Vec<u8>> {
    require(
        crate::hex(&reference.sha256, 64),
        "invalid native evidence digest",
    )?;
    let body = crate::storage::input(repo, &reference.path, limit)?;
    require(
        !body.is_empty() && crate::digest(&body) == reference.sha256,
        "native evidence digest changed or evidence is empty",
    )?;
    Ok(body)
}

impl Capture {
    pub fn read(repo: &Path, path: &Path) -> Result<Self> {
        let raw = crate::storage::input(repo, path, 64 * 1024)?;
        let input: Input = serde_json::from_slice(&raw)
            .map_err(|_| Error::new("invalid native result manifest"))?;
        crate::identifier(&input.attempt_id)?;
        require(
            input.schema == 1
                && crate::hex(&input.binding_sha256, 64)
                && (input.previous_sha256.is_empty() || crate::hex(&input.previous_sha256, 64))
                && matches!(input.kind.as_str(), "findings" | "candidate")
                && (input.kind == "candidate") == input.patch.is_some()
                && !input.summary.trim().is_empty()
                && input.summary.len() <= MAX_DRAFT
                && input.verification.len() <= 4,
            "invalid native result contract",
        )?;
        // Scan the decoded value as well: JSON escapes must not bypass the
        // outbound check before Summary or JSON export renders the text.
        crate::outbound(input.summary.as_bytes())?;
        let source = input_reference(repo, &input.source, MAX_RESULT)?;
        let patch = input
            .patch
            .as_ref()
            .map(|r| input_reference(repo, r, PATCH_MAX))
            .transpose()?;
        let verification = input
            .verification
            .iter()
            .map(|r| input_reference(repo, r, MAX_RESULT))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            input,
            input_sha256: crate::digest(&raw),
            input_raw: raw,
            source,
            patch,
            verification,
        })
    }

    pub fn attempt(&self) -> &str {
        &self.input.attempt_id
    }
    pub fn binding_digest(&self) -> &str {
        &self.input.binding_sha256
    }
    pub fn previous_digest(&self) -> &str {
        &self.input.previous_sha256
    }
    pub fn input_digest(&self) -> &str {
        &self.input_sha256
    }

    pub fn validate_scope(&self, binding: &Value) -> Result<()> {
        let Some(patch) = &self.patch else {
            return Ok(());
        };
        validate_scope(binding, patch)
    }

    /// Writes immutable snapshots first; only the owning lifecycle writer publishes their reference.
    pub fn retain(
        &self,
        tx: &Transaction<'_>,
        binding: &Value,
        binding_path: &str,
    ) -> Result<Value> {
        self.validate_scope(binding)?;
        let root = format!(
            ".oms/artifacts/native-results/{}/{}",
            self.attempt(),
            self.input_sha256
        );
        let retain = |name: &str, body: &[u8], limit| -> Result<Value> {
            let path = format!("{root}/{name}");
            tx.write(Path::new(&path), body, true, limit)?;
            Ok(json!({"path":path,"sha256":crate::digest(body),"bytes":body.len()}))
        };
        let input = retain("input.json", &self.input_raw, 64 * 1024)?;
        let source = retain("source.txt", &self.source, MAX_RESULT)?;
        let patch = self
            .patch
            .as_ref()
            .map(|b| retain("candidate.patch", b, PATCH_MAX))
            .transpose()?;
        let verification = self
            .verification
            .iter()
            .enumerate()
            .map(|(i, b)| retain(&format!("verification-{i}.txt"), b, MAX_RESULT))
            .collect::<Result<Vec<_>>>()?;
        let report = json!({"schema":1,"kind":"oms-native-result", "origin":"native-codex",
            "input":input,"input_sha256":self.input_sha256,"previous_sha256":self.input.previous_sha256,
            "binding":{"path":binding_path,"sha256":self.input.binding_sha256},
            "attempt_id":self.attempt(),"parent_attempt_id":binding["parent_attempt_id"],
            "task_id":binding["task"]["id"],"plan_id":binding["task"]["plan_id"],
            "lease_id":binding["task"]["lease_id"],"room":binding["room"],
            "custody":{"session":binding["session"],"main_pane":binding["main_pane"],
                "lead_pane":binding["lead_pane"],"account":binding["account"],
                "account_verification":binding["account_verification"],"checkout":binding["checkout"]},
            "result_kind":self.input.kind,"summary":self.input.summary,
            "source":source,"patch":patch,"verification":verification,
            "producer":"owner-import","provider_consumption":"unconfirmed",
            "task_completion":"unconfirmed","write_isolation":"unmanaged",
            "effective_permissions":"unknown","child_cleanup":"unsupported"});
        let raw = serde_json::to_vec(&report).map_err(Error::new)?;
        let path = format!("{root}/result.json");
        tx.write(Path::new(&path), &raw, true, MAX_RESULT)?;
        Ok(json!({"path":path,"sha256":crate::digest(&raw),"kind":self.input.kind}))
    }
}

fn validate_scope(binding: &Value, patch: &[u8]) -> Result<()> {
    let scopes = |key: &str| -> Result<Vec<&str>> {
        binding["task"][key]
            .as_array()
            .ok_or_else(|| Error::new("native assignment path scope unavailable"))?
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| Error::new("invalid native assignment scope"))
            })
            .collect()
    };
    let allowed = scopes("allowed_paths")?;
    let forbidden = scopes("forbidden_paths")?;
    let paths = crate::files::patch_paths(patch)?;
    require(
        !paths.is_empty() && !allowed.is_empty(),
        "native candidate has no bounded patch scope",
    )?;
    let classifier =
        oms_contract_markdown::scope::Classifier::new(&allowed, &forbidden).map_err(Error::new)?;
    for path in paths {
        require(
            classifier.classify(&path) == oms_contract_markdown::scope::Classification::Allowed,
            "native candidate patch leaves the assigned scope",
        )?;
    }
    Ok(())
}

fn retained_reference(store: &Store, value: &Value, path: &str, limit: usize) -> Result<Vec<u8>> {
    require(value["path"] == path, "native report evidence path changed")?;
    let raw = store.read(Path::new(path), limit, false)?;
    require(
        value["sha256"]
            .as_str()
            .is_some_and(|s| crate::hex(s, 64) && crate::digest(&raw) == s)
            && value["bytes"].as_u64() == Some(raw.len() as u64),
        "native report evidence digest changed",
    )?;
    Ok(raw)
}

/// A reader checks retained bytes and lineage again. It does not adopt a report as a managed call.
pub fn read(store: &Store, attempt: &Value) -> Result<Option<Value>> {
    let refs = &attempt["refs"];
    if [
        "native_result_schema",
        "native_result_path",
        "native_result_sha256",
    ]
    .iter()
    .all(|key| refs.get(*key).is_none())
    {
        return Ok(None);
    }
    let id = text(attempt, "attempt_id");
    crate::identifier(&id)?;
    require(
        attempt["tool"] == "panel-native-assignment"
            && attempt["provider"] == "codex"
            && refs["native_assignment_schema"] == "1"
            && refs["native_result_schema"] == "1"
            && matches!(
                refs["native_transport"].as_str(),
                Some("submitted" | "sending" | "unknown")
            ),
        "unsupported native result origin",
    )?;
    let path = text(refs, "native_result_path");
    let digest = text(refs, "native_result_sha256");
    require(crate::hex(&digest, 64), "invalid native result digest")?;
    require(
        path.strip_prefix(&format!(".oms/artifacts/native-results/{id}/"))
            .and_then(|p| p.strip_suffix("/result.json"))
            .is_some_and(|p| crate::hex(p, 64)),
        "native result path leaves its attempt",
    )?;
    let raw = store.read(Path::new(&path), MAX_RESULT, false)?;
    require(
        crate::digest(&raw) == digest,
        "native result digest changed",
    )?;
    let report: Value = crate::decode(&raw, MAX_RESULT)?;
    let request_digest = text(&report, "input_sha256");
    let root = format!(".oms/artifacts/native-results/{id}/{request_digest}");
    let binding_path = format!(".oms/lifecycle/attempts/{id}/native-assignment.json");
    require(
        crate::hex(&request_digest, 64)
            && path == format!("{root}/result.json")
            && report["schema"] == 1
            && report["kind"] == "oms-native-result"
            && report["origin"] == "native-codex"
            && report["producer"] == "owner-import"
            && report["attempt_id"] == id
            && report["parent_attempt_id"] == attempt["parent_attempt_id"]
            && report["task_id"] == attempt["task_id"]
            && report["plan_id"] == refs["plan_id"]
            && report["lease_id"] == refs["lease_id"]
            && report["room"] == refs["panel_room_id"]
            && report["binding"]["path"] == binding_path
            && report["binding"]["sha256"] == refs["native_binding_sha256"],
        "native result lineage changed",
    )?;
    let binding_raw = store.read(Path::new(&binding_path), 64 * 1024, false)?;
    require(
        crate::digest(&binding_raw) == report["binding"]["sha256"],
        "native assignment binding changed",
    )?;
    let binding = crate::decode(&binding_raw, 64 * 1024)?;
    let mut intent = binding.clone();
    intent
        .as_object_mut()
        .ok_or_else(|| Error::new("invalid native assignment binding"))?
        .remove("attempt_id");
    require(
        refs["native_intent_sha256"]
            == crate::digest(&serde_json::to_vec(&intent).map_err(Error::new)?),
        "native assignment intent changed",
    )?;
    require(
        binding["schema"] == 1
            && binding["kind"] == "oms-native-assignment"
            && binding["provider"] == "codex"
            && binding["attempt_id"] == id
            && binding["parent_attempt_id"] == report["parent_attempt_id"]
            && binding["task"]["id"] == report["task_id"]
            && binding["task"]["plan_id"] == report["plan_id"]
            && binding["task"]["lease_id"] == report["lease_id"]
            && binding["room"] == report["room"]
            && [
                "session",
                "main_pane",
                "lead_pane",
                "account",
                "account_verification",
                "checkout",
            ]
            .iter()
            .all(|k| report["custody"][k] == binding[k])
            && report["provider_consumption"] == "unconfirmed"
            && report["task_completion"] == "unconfirmed"
            && report["write_isolation"] == "unmanaged"
            && report["effective_permissions"] == "unknown"
            && report["child_cleanup"] == "unsupported",
        "native result custody or certainty changed",
    )?;
    let input_raw = retained_reference(
        store,
        &report["input"],
        &format!("{root}/input.json"),
        64 * 1024,
    )?;
    require(
        crate::digest(&input_raw) == request_digest,
        "native result input digest changed",
    )?;
    let input: Input = serde_json::from_slice(&input_raw)
        .map_err(|_| Error::new("invalid retained native result manifest"))?;
    crate::outbound(input.summary.as_bytes())?;
    require(
        input.schema == 1
            && input.attempt_id == id
            && report["binding"]["sha256"] == input.binding_sha256
            && report["previous_sha256"] == input.previous_sha256
            && report["result_kind"] == input.kind
            && report["summary"] == input.summary
            && !input.summary.trim().is_empty()
            && input.summary.len() <= MAX_DRAFT
            && report["source"]["sha256"] == input.source.sha256,
        "native report does not match its retained input",
    )?;
    retained_reference(
        store,
        &report["source"],
        &format!("{root}/source.txt"),
        MAX_RESULT,
    )?;
    let kind = text(&report, "result_kind");
    require(
        matches!(kind.as_str(), "findings" | "candidate")
            && (kind == "candidate") == !report["patch"].is_null(),
        "invalid retained native result kind",
    )?;
    if kind == "candidate" {
        require(
            input
                .patch
                .as_ref()
                .is_some_and(|v| report["patch"]["sha256"] == v.sha256),
            "native patch reference changed",
        )?;
        let patch = retained_reference(
            store,
            &report["patch"],
            &format!("{root}/candidate.patch"),
            PATCH_MAX,
        )?;
        validate_scope(&binding, &patch)?;
    } else {
        require(input.patch.is_none(), "unexpected native findings patch")?;
    }
    let verification = report["verification"]
        .as_array()
        .ok_or_else(|| Error::new("native verification references unavailable"))?;
    require(
        verification.len() <= 4 && verification.len() == input.verification.len(),
        "native verification reference bound changed",
    )?;
    for (i, reference) in verification.iter().enumerate() {
        require(
            reference["sha256"] == input.verification[i].sha256,
            "native verification reference changed",
        )?;
        retained_reference(
            store,
            reference,
            &format!("{root}/verification-{i}.txt"),
            MAX_RESULT,
        )?;
    }
    let verification_state = if verification.is_empty() {
        "unreported"
    } else {
        "reported"
    };
    let mut projected = report;
    projected["artifact"] = path.into();
    projected["artifact_sha256"] = digest.into();
    projected["status"] = "reported".into();
    projected["verification_state"] = verification_state.into();
    projected["custody_revalidation"] = "registration_only".into();
    projected["acceptance_eligible"] = false.into();
    projected["missing_evidence"] = json!([
        "native completion and child cleanup are unconfirmed",
        "effective native permissions are unknown",
        "reported verification references are not passing review or acceptance receipts",
        "current task, lease, session, account and base must be revalidated at admission"
    ]);
    Ok(Some(projected))
}
