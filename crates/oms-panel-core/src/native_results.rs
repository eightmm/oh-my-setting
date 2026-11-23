//! Parent-imported native reports retain evidence, never provider completion or acceptance.
use crate::native_contract::{CheckedAssignment, CheckedReport, MAX_ASSIGNMENT, MAX_REPORT};
use crate::results::{MAX_DRAFT, MAX_RESULT, PATCH_MAX};
use crate::value::text;
use crate::{Error, Result, Store, Transaction, Value, require};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeSet;
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
    verification_paths: Vec<String>,
    assignment: Option<CheckedAssignment>,
    checked_report: Option<CheckedReport>,
}

/// Read the bounded manifest before choosing its frozen assignment and checkout.
/// This phase never opens a source, patch or verification evidence path.
pub struct CaptureInput {
    input: Input,
    raw: Vec<u8>,
    sha256: String,
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

impl CaptureInput {
    pub fn read(repo: &Path, path: &Path) -> Result<Self> {
        let raw = crate::storage::input(repo, path, 64 * 1024)?;
        let decoded = crate::decode(&raw, 64 * 1024)?;
        crate::outbound(&crate::value::encoded(&decoded)?)?;
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
        Ok(Self {
            input,
            sha256: crate::digest(&raw),
            raw,
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
        &self.sha256
    }

    pub fn capture(self, repo: &Path, assignment: Option<&CheckedAssignment>) -> Result<Capture> {
        let input = self.input;
        let source = input_reference(
            repo,
            &input.source,
            if assignment.is_some() {
                MAX_REPORT
            } else {
                MAX_RESULT
            },
        )?;
        let checked_report = assignment
            .map(|a| CheckedReport::parse_bound(&source, a))
            .transpose()?;
        let (patch, verification, verification_paths) = if let (Some(assignment), Some(report)) =
            (assignment, &checked_report)
        {
            require(
                input.attempt_id == assignment.attempt_id()
                    && input.binding_sha256 == assignment.sha256()
                    && input.summary == report.summary()
                    && (input.kind == "candidate") == report.candidate_patch().is_some(),
                "native manifest differs from its typed return",
            )?;
            let evidence = report.value()["evidence"].as_array().unwrap();
            require(
                evidence.len() == input.verification.len() + usize::from(input.patch.is_some()),
                "every typed return evidence reference needs one immutable capture",
            )?;
            let mut captured = BTreeSet::new();
            let patch = input
                .patch
                .as_ref()
                .map(|reference| {
                    let path = report.candidate_patch().unwrap();
                    captured.insert(path);
                    typed_reference(repo, assignment, report, reference, path, PATCH_MAX)
                })
                .transpose()?;
            if let Some(patch) = &patch {
                let actual = crate::files::patch_paths(patch)?
                    .into_iter()
                    .collect::<BTreeSet<_>>();
                let declared = report.value()["changed_paths"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned())
                    .collect::<BTreeSet<_>>();
                require(
                    !actual.is_empty() && actual == declared,
                    "native candidate paths differ from its typed return",
                )?;
                validate_scope(assignment.packet(), patch)?;
            }
            let captured_verification = input
                .verification
                .iter()
                .map(|reference| {
                    let path = evidence
                        .iter()
                        .filter_map(|row| row["path"].as_str())
                        .find(|path| {
                            Some(*path) != report.candidate_patch()
                                && reference_matches(repo, assignment, &reference.path, path)
                        })
                        .ok_or_else(|| {
                            Error::new("native verification reference is outside its typed return")
                        })?;
                    require(captured.insert(path), "duplicate native evidence capture")?;
                    typed_reference(repo, assignment, report, reference, path, MAX_RESULT)
                        .map(|bytes| (bytes, path.to_owned()))
                })
                .collect::<Result<Vec<_>>>()?;
            let (verification, paths) = captured_verification.into_iter().unzip();
            (patch, verification, paths)
        } else {
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
            (patch, verification, Vec::new())
        };
        Ok(Capture {
            input,
            input_raw: self.raw,
            input_sha256: self.sha256,
            source,
            patch,
            verification,
            verification_paths,
            assignment: assignment.cloned(),
            checked_report,
        })
    }
}

fn reference_root<'a>(repo: &'a Path, assignment: &'a CheckedAssignment, path: &str) -> &'a Path {
    if path.starts_with(".oms/artifacts/") {
        repo
    } else {
        Path::new(assignment.checkout()["worktree"].as_str().unwrap())
    }
}

fn reference_matches(
    repo: &Path,
    assignment: &CheckedAssignment,
    supplied: &Path,
    path: &str,
) -> bool {
    supplied == Path::new(path) || supplied == reference_root(repo, assignment, path).join(path)
}

fn typed_reference(
    repo: &Path,
    assignment: &CheckedAssignment,
    report: &CheckedReport,
    reference: &InputReference,
    path: &str,
    limit: usize,
) -> Result<Vec<u8>> {
    let declared = report.value()["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["path"] == path)
        .ok_or_else(|| Error::new("typed native evidence is absent"))?;
    require(
        reference_matches(repo, assignment, &reference.path, path)
            && reference.sha256 == declared["sha256"],
        "native evidence path or digest differs from the frozen return",
    )?;
    let body =
        Store::open(reference_root(repo, assignment, path))?.read(Path::new(path), limit, false)?;
    crate::outbound(&body)?;
    require(
        !body.is_empty()
            && crate::digest(&body) == reference.sha256
            && declared["bytes"].as_u64() == Some(body.len() as u64),
        "native evidence bytes changed",
    )?;
    Ok(body)
}

fn typed_metadata(assignment: &CheckedAssignment, report: &CheckedReport) -> Value {
    json!({"schema":1,"outcome":report.outcome(),"revision":report.revision(),
        "source_sha256":report.sha256(),"task_sha256":assignment.task_sha256(),
        "lease_epoch":assignment.lease_epoch(),"checkout_sha256":assignment.checkout_sha256()})
}

fn retained_shape(value: &Value, path: &str, limit: usize) -> Result<()> {
    require(
        value.as_object().is_some_and(|object| {
            object.len()
                == if value.get("claimed_path").is_some() {
                    4
                } else {
                    3
                }
        }) && value["path"] == path
            && crate::hex(&text(value, "sha256"), 64)
            && value["bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0 && bytes <= limit as u64),
        "invalid retained native evidence reference",
    )
}

fn typed_snapshots(retained: &Value, report: &CheckedReport) -> Result<()> {
    let verification = retained["verification"]
        .as_array()
        .ok_or_else(|| Error::new("native verification references unavailable"))?;
    let evidence = report.value()["evidence"].as_array().unwrap();
    require(
        verification.len() <= 4
            && evidence.len()
                == verification.len() + usize::from(report.candidate_patch().is_some()),
        "typed native evidence inventory changed",
    )?;
    let mut paths = BTreeSet::new();
    let references = report
        .candidate_patch()
        .map(|_| &retained["patch"])
        .into_iter()
        .chain(verification.iter());
    for reference in references {
        let path = text(reference, "claimed_path");
        let declared = evidence
            .iter()
            .find(|row| row["path"] == path)
            .ok_or_else(|| Error::new("captured native evidence has no declared path"))?;
        require(
            paths.insert(path.clone())
                && reference["sha256"] == declared["sha256"]
                && reference["bytes"] == declared["bytes"],
            "captured native evidence differs from the typed return",
        )?;
    }
    if let Some(candidate) = report.candidate_patch() {
        require(
            retained["patch"]["claimed_path"] == candidate,
            "captured native candidate changed",
        )?;
    }
    Ok(())
}

/// Mutable provider output locations; immutable evidence remains owned by Capture.
pub fn input_paths(assignment: &CheckedAssignment) -> Value {
    let root = format!(
        ".oms/artifacts/native-inputs/{}/round-{}",
        assignment.attempt_id(),
        assignment.round()
    );
    json!({"source":format!("{root}/source.json"),"manifest":format!("{root}/manifest.json")})
}

/// No input byte is read. A no-follow descriptor walk supplies an advisory hint
/// only; import reopens and validates the complete bounded manifest and evidence.
fn input_presence(store: &Store, path: &Path) -> &'static str {
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    let observe = || -> std::result::Result<bool, rustix::io::Errno> {
        let flags = OFlags::RDONLY
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW
            | OFlags::DIRECTORY
            | OFlags::NONBLOCK;
        let mut directory = fs::open(store.repo(), flags, Mode::empty())?;
        let parts = path.components().map(|p| p.as_os_str()).collect::<Vec<_>>();
        for part in &parts[..parts.len() - 1] {
            directory = fs::openat(&directory, *part, flags, Mode::empty())?;
        }
        let leaf = fs::statat(
            &directory,
            parts[parts.len() - 1],
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
        Ok(
            FileType::from_raw_mode(leaf.st_mode) == FileType::RegularFile
                && leaf.st_uid == rustix::process::getuid().as_raw()
                && leaf.st_nlink == 1
                && leaf.st_size >= 0
                && leaf.st_size <= MAX_REPORT as i64,
        )
    };
    match observe() {
        Ok(true) => "present",
        Err(rustix::io::Errno::NOENT) => "absent",
        Ok(false) | Err(_) => "unknown",
    }
}

fn typed_view(
    projected: &mut Value,
    assignment: &CheckedAssignment,
    report: Option<&CheckedReport>,
    store: &Store,
) {
    let paths = input_paths(assignment);
    let source = input_presence(store, Path::new(paths["source"].as_str().unwrap()));
    let manifest = input_presence(store, Path::new(paths["manifest"].as_str().unwrap()));
    let mut template = assignment.report_template();
    if let Some(report) = report {
        if let Some(revision) = report.revision().checked_add(1) {
            template["revision"] = revision.into();
            template["previous_report_sha256"] = report.sha256().into();
        } else {
            template = Value::Null;
        }
    }
    projected["binding_sha256"] = assignment.sha256().into();
    projected["attempt_id"] = assignment.attempt_id().into();
    projected["parent_attempt_id"] = assignment.parent_attempt_id().into();
    projected["task_id"] = assignment.task_id().into();
    projected["plan_id"] = assignment.plan_id().into();
    projected["lease_id"] = assignment.lease_id().into();
    projected["lease_epoch"] = assignment.lease_epoch().into();
    projected["owner"] = assignment.owner().into();
    projected["room"] = assignment.room().into();
    projected["round"] = assignment.round().into();
    projected["execution_contract"] = assignment.packet()["result_contract"].clone();
    projected["return_state"] = report.map_or("absent", |report| report.outcome()).into();
    projected["return_claims"] = report.map_or(Value::Null, |report| report.value().clone());
    projected["source_template"] = template;
    projected["manifest_template"] = json!({"schema":1,"attempt_id":assignment.attempt_id(),
        "binding_sha256":assignment.sha256(),"previous_sha256":projected["artifact_sha256"].as_str().unwrap_or(""),
        "kind":"findings","summary":"<copy filled source.summary>",
        "source":{"path":paths["source"],"sha256":"<sha256 of exact source.json bytes>"},
        "patch":null,"verification":[]});
    projected["manifest_required_input"] = json!([
        "summary",
        "source.sha256",
        "kind/patch/verification matching the filled source"
    ]);
    projected["input_paths"] = paths;
    projected["input_presence"] = json!({"source":source,"manifest":manifest});
    projected["pending_input"] = (report.is_none() && manifest == "present").into();
}

/// Read at most one assignment, retained metadata and typed source (64 KiB each).
/// Callers bound row counts. Candidate and verification bytes are never read here.
pub fn metadata(store: &Store, attempt: &Value) -> Result<Value> {
    require(
        attempt["refs"]["native_return_schema"] == "1",
        "unsupported typed native return schema",
    )?;
    if let Some(report) = read_mode(store, attempt, false)? {
        return Ok(report);
    }
    let (_, _, assignment) = binding_for(store, attempt, true)?;
    let assignment =
        assignment.ok_or_else(|| Error::new("typed native assignment contract is unavailable"))?;
    let mut value = json!({"status":"awaiting-report","acceptance_eligible":false,
        "provider_consumption":"unconfirmed","task_completion":"unconfirmed","child_cleanup":"unsupported",
        "verification_state":"unreported","evidence_validation":"metadata-and-typed-source-only"});
    typed_view(&mut value, &assignment, None, store);
    Ok(value)
}

impl Capture {
    /// Legacy advisory capture. Typed assignments use CaptureInput after custody validation.
    pub fn read(repo: &Path, path: &Path) -> Result<Self> {
        CaptureInput::read(repo, path)?.capture(repo, None)
    }

    pub fn checked_report(&self) -> Option<&CheckedReport> {
        self.checked_report.as_ref()
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
        require(
            self.assignment
                .as_ref()
                .map_or(!binding["result_contract"].is_object(), |a| {
                    a.packet() == binding && a.sha256() == self.binding_digest()
                }),
            "typed native assignment requires its checked source capture",
        )?;
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
        let source = retain(
            "source.txt",
            &self.source,
            if self.checked_report.is_some() {
                MAX_REPORT
            } else {
                MAX_RESULT
            },
        )?;
        let patch = self
            .patch
            .as_ref()
            .map(|b| {
                let mut reference = retain("candidate.patch", b, PATCH_MAX)?;
                if let Some(report) = &self.checked_report {
                    reference["claimed_path"] = report.candidate_patch().unwrap().into();
                }
                Ok(reference)
            })
            .transpose()?;
        let verification = self
            .verification
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let mut reference = retain(&format!("verification-{i}.txt"), b, MAX_RESULT)?;
                if self.checked_report.is_some() {
                    reference["claimed_path"] = self.verification_paths[i].clone().into();
                }
                Ok(reference)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut report = json!({"schema":1,"kind":"oms-native-result", "origin":"native-codex",
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
        if let (Some(assignment), Some(checked)) = (&self.assignment, &self.checked_report) {
            report["return_contract"] = typed_metadata(assignment, checked);
        }
        let raw = serde_json::to_vec(&report).map_err(Error::new)?;
        let path = format!("{root}/result.json");
        tx.write(
            Path::new(&path),
            &raw,
            true,
            if self.checked_report.is_some() {
                MAX_REPORT
            } else {
                MAX_RESULT
            },
        )?;
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

fn snapshot_bytes(store: &Store, path: &str, limit: usize, observation: bool) -> Result<Vec<u8>> {
    store.read(Path::new(path), limit, false).map_err(|error| {
        if observation {
            Error { code: 75, ..error }
        } else {
            error
        }
    })
}

fn retained_reference(
    store: &Store,
    value: &Value,
    path: &str,
    limit: usize,
    observation: bool,
) -> Result<Vec<u8>> {
    require(value["path"] == path, "native report evidence path changed")?;
    let raw = snapshot_bytes(store, path, limit, observation)?;
    require(
        value["sha256"]
            .as_str()
            .is_some_and(|s| crate::hex(s, 64) && crate::digest(&raw) == s)
            && value["bytes"].as_u64() == Some(raw.len() as u64),
        "native report evidence digest changed",
    )?;
    Ok(raw)
}

fn result_absent(refs: &Value) -> bool {
    [
        "native_result_schema",
        "native_result_path",
        "native_result_sha256",
    ]
    .iter()
    .all(|key| {
        refs.get(*key)
            .is_none_or(|value| value.as_str() == Some(""))
    })
}

fn binding_for(
    store: &Store,
    attempt: &Value,
    observation: bool,
) -> Result<(String, Value, Option<CheckedAssignment>)> {
    let id = text(attempt, "attempt_id");
    crate::identifier(&id)?;
    let refs = &attempt["refs"];
    if observation
        && refs["native_return_schema"] == "1"
        && ["native_binding", "native_binding_sha256"]
            .iter()
            .all(|key| {
                refs.get(*key)
                    .is_none_or(|value| value.as_str() == Some(""))
            })
    {
        return Err(Error::busy(
            "native assignment binding is not yet published",
        ));
    }
    let fallback = format!(".oms/lifecycle/attempts/{id}/native-assignment.json");
    let path = refs["native_binding"]
        .as_str()
        .filter(|path| !path.is_empty())
        .unwrap_or(&fallback)
        .to_owned();
    let parent = format!(".oms/lifecycle/attempts/{id}");
    require(
        Path::new(&path).parent() == Some(Path::new(&parent))
            && (path == fallback
                || (path.starts_with(&format!("{parent}/native-round-"))
                    && path.ends_with(&format!("-{}.json", text(refs, "native_binding_sha256"))))),
        "native assignment binding path leaves its exact attempt",
    )?;
    let raw = snapshot_bytes(store, &path, MAX_ASSIGNMENT, observation)?;
    require(
        crate::hex(&text(refs, "native_binding_sha256"), 64)
            && crate::digest(&raw) == refs["native_binding_sha256"],
        "native assignment binding changed",
    )?;
    let binding = crate::decode(&raw, MAX_ASSIGNMENT)?;
    let checked = CheckedAssignment::parse(&raw)?;
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
        attempt["tool"] == "panel-native-assignment"
            && attempt["provider"] == "codex"
            && refs["native_assignment_schema"] == "1"
            && binding["schema"] == 1
            && binding["kind"] == "oms-native-assignment"
            && binding["provider"] == "codex"
            && binding["attempt_id"] == id
            && binding["parent_attempt_id"] == attempt["parent_attempt_id"]
            && binding["task"]["id"] == attempt["task_id"]
            && binding["task"]["plan_id"] == refs["plan_id"]
            && binding["task"]["lease_id"] == refs["lease_id"]
            && binding["room"] == refs["panel_room_id"],
        "native assignment lineage changed",
    )?;
    if let Some(assignment) = &checked {
        let expected = if assignment.round() == 1 {
            fallback
        } else {
            format!(
                "{parent}/native-round-{}-{}.json",
                assignment.round(),
                assignment.sha256()
            )
        };
        require(
            path == expected
                && assignment.owner() == text(refs, "panel_owner")
                && text(refs, "native_round") == assignment.round().to_string(),
            "typed native assignment owner or round changed",
        )?;
    } else {
        require(
            path == fallback && refs["native_return_schema"].is_null(),
            "typed native assignment contract is unavailable",
        )?;
    }
    Ok((path, binding, checked))
}

/// Deep verification belongs to explicit import/reconciliation or evidence inspection.
pub fn read(store: &Store, attempt: &Value) -> Result<Option<Value>> {
    read_mode(store, attempt, true)
}

fn read_mode(store: &Store, attempt: &Value, deep: bool) -> Result<Option<Value>> {
    let refs = &attempt["refs"];
    if result_absent(refs) {
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
    let limit = if deep { MAX_RESULT } else { MAX_REPORT };
    let raw = snapshot_bytes(store, &path, limit, !deep)?;
    require(
        crate::digest(&raw) == digest,
        "native result digest changed",
    )?;
    let report: Value = crate::decode(&raw, limit)?;
    let request_digest = text(&report, "input_sha256");
    let root = format!(".oms/artifacts/native-results/{id}/{request_digest}");
    let (binding_path, binding, checked_assignment) = binding_for(store, attempt, !deep)?;
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
    require(
        deep || checked_assignment.is_some(),
        "bounded native metadata requires a typed assignment",
    )?;
    retained_shape(&report["input"], &format!("{root}/input.json"), 64 * 1024)?;
    require(
        report["input"]["sha256"] == request_digest,
        "native result input digest changed",
    )?;
    let input = if deep {
        let raw = retained_reference(
            store,
            &report["input"],
            &format!("{root}/input.json"),
            64 * 1024,
            false,
        )?;
        let decoded = crate::decode(&raw, 64 * 1024)?;
        crate::outbound(&crate::value::encoded(&decoded)?)?;
        let input: Input = serde_json::from_slice(&raw)
            .map_err(|_| Error::new("invalid retained native result manifest"))?;
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
        Some(input)
    } else {
        None
    };
    let source = retained_reference(
        store,
        &report["source"],
        &format!("{root}/source.txt"),
        if checked_assignment.is_some() {
            MAX_REPORT
        } else {
            MAX_RESULT
        },
        !deep,
    )?;
    let checked_report = checked_assignment
        .as_ref()
        .map(|a| CheckedReport::parse_bound(&source, a))
        .transpose()?;
    if let (Some(assignment), Some(returned)) = (&checked_assignment, &checked_report) {
        require(
            report["return_contract"] == typed_metadata(assignment, returned)
                && report["summary"] == returned.summary()
                && (report["result_kind"] == "candidate") == returned.candidate_patch().is_some(),
            "retained native return contract changed",
        )?;
        typed_snapshots(&report, returned)?;
    } else {
        require(
            report["return_contract"].is_null(),
            "unbound typed native return",
        )?;
    }
    let kind = text(&report, "result_kind");
    require(
        matches!(kind.as_str(), "findings" | "candidate")
            && (kind == "candidate") == !report["patch"].is_null(),
        "invalid retained native result kind",
    )?;
    if kind == "candidate" {
        require(
            input.as_ref().is_none_or(|input| {
                input
                    .patch
                    .as_ref()
                    .is_some_and(|v| report["patch"]["sha256"] == v.sha256)
            }),
            "native patch reference changed",
        )?;
        retained_shape(
            &report["patch"],
            &format!("{root}/candidate.patch"),
            PATCH_MAX,
        )?;
        if deep {
            let patch = retained_reference(
                store,
                &report["patch"],
                &format!("{root}/candidate.patch"),
                PATCH_MAX,
                false,
            )?;
            validate_scope(&binding, &patch)?;
            if let Some(returned) = &checked_report {
                let actual = crate::files::patch_paths(&patch)?
                    .into_iter()
                    .collect::<BTreeSet<_>>();
                let declared = returned.value()["changed_paths"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned())
                    .collect::<BTreeSet<_>>();
                require(
                    actual == declared,
                    "retained native candidate paths changed",
                )?;
            }
        }
    } else {
        require(
            input.as_ref().is_none_or(|input| input.patch.is_none()),
            "unexpected native findings patch",
        )?;
    }
    let verification = report["verification"]
        .as_array()
        .ok_or_else(|| Error::new("native verification references unavailable"))?;
    require(
        verification.len() <= 4
            && input
                .as_ref()
                .is_none_or(|input| verification.len() == input.verification.len()),
        "native verification reference bound changed",
    )?;
    for (i, reference) in verification.iter().enumerate() {
        require(
            input
                .as_ref()
                .is_none_or(|input| reference["sha256"] == input.verification[i].sha256),
            "native verification reference changed",
        )?;
        retained_shape(
            reference,
            &format!("{root}/verification-{i}.txt"),
            MAX_RESULT,
        )?;
        if deep {
            retained_reference(
                store,
                reference,
                &format!("{root}/verification-{i}.txt"),
                MAX_RESULT,
                false,
            )?;
        }
        if let (Some(input), Some(assignment)) = (&input, &checked_assignment) {
            require(
                reference_matches(
                    store.repo(),
                    assignment,
                    &input.verification[i].path,
                    &text(reference, "claimed_path"),
                ),
                "retained native verification origin changed",
            )?;
        }
    }
    if let (Some(input), Some(assignment), Some(returned)) =
        (&input, &checked_assignment, &checked_report)
        && let Some(reference) = &input.patch
    {
        require(
            reference_matches(
                store.repo(),
                assignment,
                &reference.path,
                returned.candidate_patch().unwrap(),
            ),
            "retained native candidate origin changed",
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
    if let Some(assignment) = &checked_assignment {
        typed_view(&mut projected, assignment, checked_report.as_ref(), store);
        projected["evidence_validation"] = if deep {
            "retained-bytes"
        } else {
            "metadata-and-typed-source-only"
        }
        .into();
    }
    projected["missing_evidence"] = json!([
        "native completion and child cleanup are unconfirmed",
        "effective native permissions are unknown",
        "reported verification references are not passing review or acceptance receipts",
        "current task, lease, session, account and base must be revalidated at admission"
    ]);
    Ok(Some(projected))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;

    pub(crate) fn write(repo: &Path, path: &str, raw: &[u8]) {
        Store::open(repo)
            .unwrap()
            .transaction("native-fixture", Duration::from_secs(3))
            .unwrap()
            .write(Path::new(path), raw, false, MAX_RESULT)
            .unwrap();
    }

    pub(crate) fn fixture(
        repo: &Path,
        checkout: &Path,
        id: &str,
        task_id: &str,
    ) -> (Value, Value, CheckedAssignment) {
        let task = json!({"id":task_id,"title":"Inspect bounded source","role":"researcher",
            "plan_id":format!("plan_{}","a".repeat(32)),"lease_id":format!("lease_{}","b".repeat(32)),
            "lease_epoch":1,"provider":"codex","claimed_by_participant":"main-native",
            "state":"claimed","claim_expired":false,"contract_stale":false,
            "allowed_paths":["src"],"forbidden_paths":["src/private"],"verify":"cargo test -p fixture","covers":["C1"]});
        let mut packet = json!({"schema":1,"kind":"oms-native-assignment","attempt_id":id,
            "parent_attempt_id":"main-native","room":"room-native","provider":"codex",
            "task":task,"checkout":{"cwd":checkout,"worktree":checkout,"base_sha":"c".repeat(40)},
            "result_contract":crate::native_contract::execution_contract(&task).unwrap()});
        let raw = serde_json::to_vec(&packet).unwrap();
        let assignment = CheckedAssignment::parse(&raw).unwrap().unwrap();
        let path = format!(".oms/lifecycle/attempts/{id}/native-assignment.json");
        write(repo, &path, &raw);
        packet.as_object_mut().unwrap().remove("attempt_id");
        let operation = json!({"attempt_id":id,"parent_attempt_id":"main-native","tool":"panel-native-assignment",
            "provider":"codex","task_id":task_id,"state":"working","terminal":false,
            "refs":{"native_assignment_schema":"1","native_return_schema":"1","native_transport":"submitted",
                "plan_id":task["plan_id"],"lease_id":task["lease_id"],"panel_room_id":"room-native","panel_owner":"codex",
                "native_binding":path,"native_binding_sha256":assignment.sha256(),
                "native_intent_sha256":crate::digest(&serde_json::to_vec(&packet).unwrap()),
                "native_round":"1","native_resolution":"awaiting-report"}});
        (operation, task, assignment)
    }

    pub(crate) fn retain(
        repo: &Path,
        operation: &mut Value,
        assignment: &CheckedAssignment,
        source: &Value,
    ) -> Value {
        let raw = serde_json::to_vec(source).unwrap();
        let paths = input_paths(assignment);
        write(repo, paths["source"].as_str().unwrap(), &raw);
        let candidate = source["candidate_patch"].as_str();
        let evidence = source["evidence"].as_array().unwrap();
        let reference = |row: &Value| json!({"path":row["path"],"sha256":row["sha256"]});
        let manifest = json!({"schema":1,"attempt_id":assignment.attempt_id(),"binding_sha256":assignment.sha256(),
            "kind":if candidate.is_some(){"candidate"}else{"findings"},"summary":source["summary"],
            "source":{"path":paths["source"],"sha256":crate::digest(&raw)},
            "patch":candidate.map(|path|reference(evidence.iter().find(|row|row["path"]==path).unwrap())),
            "verification":evidence.iter().filter(|row|row["path"].as_str()!=candidate).map(reference).collect::<Vec<_>>()});
        let path = paths["manifest"].as_str().unwrap();
        write(repo, path, &serde_json::to_vec(&manifest).unwrap());
        let capture = CaptureInput::read(repo, Path::new(path))
            .unwrap()
            .capture(repo, Some(assignment))
            .unwrap();
        assert_eq!(capture.checked_report().unwrap().value(), source);
        let store = Store::open(repo).unwrap();
        let tx = store
            .transaction("native-fixture-retain", Duration::from_secs(3))
            .unwrap();
        let retained = capture
            .retain(
                &tx,
                assignment.packet(),
                &text(&operation["refs"], "native_binding"),
            )
            .unwrap();
        operation["refs"]["native_result_schema"] = "1".into();
        operation["refs"]["native_result_path"] = retained["path"].clone();
        operation["refs"]["native_result_sha256"] = retained["sha256"].clone();
        operation["refs"]["native_resolution"] = "pending".into();
        operation["refs"]["native_resolution_result_sha256"] = "".into();
        retained
    }

    #[test]
    fn typed_capture_snapshots_frozen_checkout_and_metadata_does_not_read_evidence() {
        let state = crate::git::Scratch::new().unwrap();
        let checkout = crate::git::Scratch::new().unwrap();
        let (mut operation, _, assignment) =
            fixture(&state.0, &checkout.0, "native-snapshot", "task-snapshot");
        write(&state.0, "src/check.txt", b"wrong checkout");
        write(&checkout.0, "src/check.txt", b"frozen checkout");
        let mut source = assignment.report_template();
        source["summary"] = "Claimed result for Main".into();
        source["evidence"] =
            json!([{"path":"src/check.txt","sha256":crate::digest(b"frozen checkout"),"bytes":15}]);
        source["checks"] = json!([{"name":"frozen verifier","status":"passed","note":"Lead claim only","evidence":["src/check.txt"]}]);
        retain(&state.0, &mut operation, &assignment, &source);
        let store = Store::open(&state.0).unwrap();
        let deep = read(&store, &operation).unwrap().unwrap();
        assert_eq!(
            metadata(&store, &operation).unwrap()["return_claims"],
            source
        );
        assert_eq!(deep["acceptance_eligible"], false);
        write(&checkout.0, "src/check.txt", b"edited after capture");
        assert!(
            read(&store, &operation).is_ok(),
            "original mutable evidence is no longer consulted"
        );
        let captured = deep["verification"][0]["path"].as_str().unwrap();
        write(&state.0, captured, b"tampered retained evidence");
        assert!(
            read(&store, &operation).is_err(),
            "explicit deep verification still checks captured bytes"
        );
        assert!(
            metadata(&store, &operation).is_ok(),
            "ordinary metadata never opens verification bodies"
        );
        write(
            &state.0,
            deep["source"]["path"].as_str().unwrap(),
            b"changed typed source",
        );
        assert!(
            metadata(&store, &operation).is_err(),
            "typed source itself is digest checked"
        );
    }

    #[test]
    fn typed_capture_refuses_legacy_bypass_and_incomplete_evidence_inventory() {
        let repo = crate::git::Scratch::new().unwrap();
        let (operation, _, assignment) =
            fixture(&repo.0, &repo.0, "native-inventory", "task-inventory");
        let paths = input_paths(&assignment);
        let mut source = assignment.report_template();
        source["summary"] = "Claim only".into();
        source["evidence"] = json!([{"path":"src/missing.txt","sha256":"a".repeat(64),"bytes":1}]);
        let raw = serde_json::to_vec(&source).unwrap();
        write(&repo.0, paths["source"].as_str().unwrap(), &raw);
        let manifest = json!({"schema":1,"attempt_id":assignment.attempt_id(),"binding_sha256":assignment.sha256(),
            "kind":"findings","summary":source["summary"],"source":{"path":paths["source"],"sha256":crate::digest(&raw)}});
        let path = Path::new(paths["manifest"].as_str().unwrap());
        write(
            &repo.0,
            path.to_str().unwrap(),
            &serde_json::to_vec(&manifest).unwrap(),
        );
        assert!(
            CaptureInput::read(&repo.0, path).is_ok(),
            "manifest inspection does not read its evidence"
        );
        assert!(
            CaptureInput::read(&repo.0, path)
                .unwrap()
                .capture(&repo.0, Some(&assignment))
                .is_err()
        );
        let legacy = Capture::read(&repo.0, path).unwrap();
        let store = Store::open(&repo.0).unwrap();
        let tx = store
            .transaction("native-bypass", Duration::from_secs(3))
            .unwrap();
        assert!(
            legacy
                .retain(
                    &tx,
                    assignment.packet(),
                    &text(&operation["refs"], "native_binding")
                )
                .is_err()
        );
        drop(tx);

        // Pre-contract assignments keep the original prose capture path. Its
        // candidate still has to fit the same directional frozen task scope.
        let mut binding = assignment.packet().clone();
        binding["result_contract"] = "Return findings for the owning Main".into();
        binding["task"]["allowed_paths"] = json!(["src/lib.rs"]);
        let raw = serde_json::to_vec(&binding).unwrap();
        let binding_path = text(&operation["refs"], "native_binding");
        write(&repo.0, &binding_path, &raw);
        let mut operation = operation;
        operation["refs"]
            .as_object_mut()
            .unwrap()
            .remove("native_return_schema");
        operation["refs"]["native_binding_sha256"] = crate::digest(&raw).into();
        let mut intent = binding.clone();
        intent.as_object_mut().unwrap().remove("attempt_id");
        operation["refs"]["native_intent_sha256"] =
            crate::digest(&serde_json::to_vec(&intent).unwrap()).into();
        let prose = b"Legacy candidate for Main review";
        let patch = b"diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        write(&repo.0, "legacy-source.txt", prose);
        write(&repo.0, "legacy-candidate.patch", patch);
        let mut manifest = json!({"schema":1,"attempt_id":assignment.attempt_id(),
            "binding_sha256":crate::digest(&raw),"kind":"candidate","summary":"Legacy candidate",
            "source":{"path":"legacy-source.txt","sha256":crate::digest(prose)},
            "patch":{"path":"legacy-candidate.patch","sha256":crate::digest(patch)}});
        write(
            &repo.0,
            "legacy-manifest.json",
            &serde_json::to_vec(&manifest).unwrap(),
        );
        let capture = Capture::read(&repo.0, Path::new("legacy-manifest.json")).unwrap();
        assert!(capture.checked_report().is_none());
        let tx = store
            .transaction("native-legacy", Duration::from_secs(3))
            .unwrap();
        let retained = capture.retain(&tx, &binding, &binding_path).unwrap();
        drop(tx);
        operation["refs"]["native_result_schema"] = "1".into();
        operation["refs"]["native_result_path"] = retained["path"].clone();
        operation["refs"]["native_result_sha256"] = retained["sha256"].clone();
        let observed = read(&store, &operation).unwrap().unwrap();
        assert_eq!(observed["status"], "reported");
        assert_eq!(observed["result_kind"], "candidate");
        assert_eq!(observed["source"]["sha256"], crate::digest(prose));
        assert_eq!(observed["acceptance_eligible"], false);
        assert!(observed.get("return_contract").is_none());

        let ancestor = b"diff --git a/src b/src\n--- a/src\n+++ b/src\n@@ -1 +1 @@\n-old\n+new\n";
        write(&repo.0, "legacy-candidate.patch", ancestor);
        manifest["patch"]["sha256"] = crate::digest(ancestor).into();
        write(
            &repo.0,
            "legacy-manifest.json",
            &serde_json::to_vec(&manifest).unwrap(),
        );
        let capture = Capture::read(&repo.0, Path::new("legacy-manifest.json")).unwrap();
        let tx = store
            .transaction("native-legacy-scope", Duration::from_secs(3))
            .unwrap();
        assert!(capture.retain(&tx, &binding, &binding_path).is_err());
    }

    #[test]
    fn typed_round_absence_partial_refs_and_input_presence_remain_distinct() {
        let repo = crate::git::Scratch::new().unwrap();
        let (mut operation, _, assignment) =
            fixture(&repo.0, &repo.0, "native-round", "task-round");
        let store = Store::open(&repo.0).unwrap();
        let paths = input_paths(&assignment);
        write(
            &repo.0,
            paths["manifest"].as_str().unwrap(),
            b"not yet valid JSON",
        );
        let before = metadata(&store, &operation).unwrap();
        assert_eq!(before["return_state"], "absent");
        assert_eq!(before["pending_input"], true);
        assert_eq!(before["acceptance_eligible"], false);
        let mut packet = assignment.packet().clone();
        packet["round"] = 2.into();
        packet["previous_report_sha256"] = "d".repeat(64).into();
        packet["previous_result_sha256"] = "e".repeat(64).into();
        let raw = serde_json::to_vec(&packet).unwrap();
        let sha = crate::digest(&raw);
        let path = format!(".oms/lifecycle/attempts/native-round/native-round-2-{sha}.json");
        write(&repo.0, &path, &raw);
        packet.as_object_mut().unwrap().remove("attempt_id");
        operation["refs"]["native_binding"] = path.into();
        operation["refs"]["native_binding_sha256"] = sha.into();
        operation["refs"]["native_intent_sha256"] =
            crate::digest(&serde_json::to_vec(&packet).unwrap()).into();
        operation["refs"]["native_round"] = "2".into();
        for key in [
            "native_result_schema",
            "native_result_path",
            "native_result_sha256",
        ] {
            operation["refs"][key] = "".into();
        }
        assert!(read(&store, &operation).unwrap().is_none());
        let next = metadata(&store, &operation).unwrap();
        assert_eq!(next["round"], 2);
        assert_eq!(next["pending_input"], false);
        assert!(text(&next["input_paths"], "manifest").contains("/round-2/"));
        operation["refs"]["native_result_schema"] = "1".into();
        assert!(
            metadata(&store, &operation).is_err(),
            "partial cleared result identity is invalid"
        );
    }
}
