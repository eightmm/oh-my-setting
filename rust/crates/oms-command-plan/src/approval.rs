use crate::completion::{self, reference, text, validate_verification};
use crate::json::{self, Value, empty, number, object, string};
use crate::provenance;
use crate::services::MainReviewerRequest;
use crate::storage::MAX_JSON;
use crate::{Plan, Request, Response, Result, error, require};
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;

const POLICY: &str = "main-read-completion-v1";
pub(crate) fn neutral(snapshot: &Value) -> Result<Value> {
    let mut result = snapshot.clone();
    let mut evidence = result.get("evidence").clone();
    evidence.set("reviewer", empty())?;
    result.set("evidence", evidence)?;
    Ok(result)
}
pub(crate) fn validate_controller(plan: &Plan<'_>, manifest: &Value) -> Result<()> {
    let rows = manifest.array()?;
    require(
        !rows.is_empty() && rows.len() <= 4096,
        "controller source manifest exceeds bound or is empty",
    )?;
    let mut names = BTreeSet::new();
    let mut total = 0;
    for row in rows {
        json::keys(row, &["path", "sha256", "mode"], &[])?;
        let path = completion::relative(row.get("path").str()?)?;
        require(names.insert(path), "duplicate controller source path")?;
        let snap = plan
            .source
            .snapshot(path, 8 * 1024 * 1024, false)?
            .ok_or_else(|| error("controller source missing"))?;
        total += snap.raw.len();
        require(
            total <= 64 * 1024 * 1024
                && json::sha(&snap.raw) == json::digest(row.get("sha256"))?
                && crate::repository::source_mode(snap.mode) == row.text("mode"),
            "executing controller source bytes or mode changed",
        )?;
    }
    Ok(())
}
fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b))
    {
        s.into()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}
fn argv(plan: &Plan<'_>, bundle: &Value, sha: &str, path: &str) -> Result<Vec<String>> {
    let state = plan
        .context
        .state_repo
        .to_str()
        .ok_or_else(|| error("approval state repository is not UTF-8"))?;
    let repo = plan
        .context
        .execution_repo
        .to_str()
        .ok_or_else(|| error("approval source repository is not UTF-8"))?;
    let mut args = Vec::new();
    if state != repo {
        args.extend(["env".into(), format!("OMS_STATE_REPO={state}")]);
    }
    args.extend([
        "scripts/oms".into(),
        "agent-plan".into(),
        "approve-main-research".into(),
        "--repo".into(),
        repo.into(),
        "--id".into(),
        bundle.get("task_id").str()?.into(),
        "--lease-id".into(),
        bundle.get("lease_id").str()?.into(),
        "--expected-state".into(),
        bundle.get("state").str()?.into(),
        "--expected-plan-sha256".into(),
        bundle.get("plan_sha256").str()?.into(),
        "--expected-task-sha256".into(),
        bundle.get("task_sha256").str()?.into(),
        "--completion-bundle".into(),
        path.into(),
        "--expected-completion-bundle-sha256".into(),
        sha.into(),
        "--approve-reviewed-bundle".into(),
    ]);
    Ok(args)
}
fn invocation(plan: &Plan<'_>, bundle: &Value, sha: &str, path: &str) -> Result<String> {
    Ok(argv(plan, bundle, sha, path)?
        .iter()
        .map(|s| quote(s))
        .collect::<Vec<_>>()
        .join(" "))
}
pub(crate) fn record(
    plan: &Plan<'_>,
    r: &Value,
    bundle: &Value,
    sha: &str,
    current: bool,
) -> Result<Value> {
    json::keys(r, &["path", "sha256"], &[])?;
    require(
        r.text("path")
            == format!(
                ".oms/plan/completion-captures/{}.json",
                json::digest(r.get("sha256"))?
            ),
        "main approval must be an immutable canonical capture",
    )?;
    let raw = reference(plan, r)?;
    let marker = json::parse(&raw)?;
    json::keys(
        &marker,
        &[
            "schema",
            "kind",
            "authorized_action",
            "nonce",
            "task_id",
            "bundle_sha256",
            "room_id",
            "participant",
            "lease_id",
            "state",
            "plan_sha256",
            "task_sha256",
            "snapshot_sha256",
            "code_manifest_sha256",
            "state_repo_sha256",
            "execution_repo_sha256",
            "git_common_sha256",
            "snapshot",
            "verification",
            "bundle_path",
            "approve_invocation_sha256",
        ],
        &[],
    )?;
    require(
        json::encoded(&marker)? == raw && marker.get("schema").is_int(1),
        "noncanonical main approval capture",
    )?;
    let expected = object([
        ("kind", string("oms-main-read-approval-v1")),
        ("authorized_action", string("accept-main-research")),
        ("task_id", bundle.get("task_id").clone()),
        ("bundle_sha256", string(sha)),
        ("room_id", bundle.get("reviewer").get("room_id").clone()),
        (
            "participant",
            bundle.get("owner").get("participant").clone(),
        ),
        ("lease_id", bundle.get("lease_id").clone()),
        ("state", bundle.get("state").clone()),
        ("plan_sha256", bundle.get("plan_sha256").clone()),
        ("task_sha256", bundle.get("task_sha256").clone()),
        (
            "state_repo_sha256",
            string(json::sha(plan.context.state_repo.as_os_str().as_bytes())),
        ),
        ("git_common_sha256", string(plan.state_git().namespace()?)),
    ]);
    for (k, v) in expected.object()? {
        require(
            marker.get(std::str::from_utf8(k).map_err(|_| error("invalid expected field"))?) == v,
            "main approval differs from exact original bundle/CAS/namespace",
        )?;
    }
    for name in [
        "nonce",
        "execution_repo_sha256",
        "approve_invocation_sha256",
        "snapshot_sha256",
        "code_manifest_sha256",
    ] {
        json::digest(marker.get(name))?;
    }
    completion::relative(marker.get("bundle_path").str()?)?;
    let snapshot = marker.get("snapshot");
    json::keys(
        snapshot,
        &[
            "head",
            "base_commit",
            "manifest_sha256",
            "source_identities",
            "verifier_command_sha256",
            "repository_sha256",
            "evidence",
            "evidence_identities",
            "provenance",
            "controller_manifest",
        ],
        &[],
    )?;
    require(
        neutral(snapshot)? == *snapshot
            && json::encoded_hash(snapshot)? == marker.text("snapshot_sha256")
            && json::encoded_hash(snapshot.get("controller_manifest"))?
                == marker.text("code_manifest_sha256")
            && snapshot.text("repository_sha256") == plan.state_git().namespace()?
            && snapshot.text("manifest_sha256")
                == json::encoded_hash(bundle.get("source_manifest"))?
            && snapshot.get("base_commit") == bundle.get("base_commit")
            && snapshot.text("verifier_command_sha256")
                == json::sha(bundle.get("verifier").text("command"))
            && snapshot.get("provenance").get("room_id") == marker.get("room_id"),
        "main approval changed frozen report/source/verifier snapshot",
    )?;
    provenance::load(plan, snapshot.get("provenance"), false)?;
    validate_verification(marker.get("verification"), snapshot)?;
    if current {
        let controller = plan
            .services
            .controller_manifest(&plan.context.execution_repo)?;
        validate_controller(plan, &controller)?;
        require(
            marker.text("execution_repo_sha256")
                == json::sha(plan.context.execution_repo.as_os_str().as_bytes())
                && snapshot.get("controller_manifest") == &controller
                && marker.text("approve_invocation_sha256")
                    == json::sha(invocation(plan, bundle, sha, marker.text("bundle_path"))?),
            "main approval controller source or invocation changed",
        )?;
    }
    Ok(marker)
}
pub(crate) fn runtime_record(
    plan: &Plan<'_>,
    req: &Request,
    bundle: &Value,
    sha: &str,
) -> Result<(Value, Value)> {
    let r = object([
        ("path", string(req.value("main-review-approval")?)),
        (
            "sha256",
            string(req.value("expected-main-review-approval-sha256")?),
        ),
    ]);
    let marker = record(plan, &r, bundle, sha, true)?;
    require(
        marker.text("bundle_path") == req.value("completion-bundle")?,
        "main approval bundle path changed",
    )?;
    Ok((r, marker))
}
fn desktop_member<'a>(state: &'a Value, who: &str) -> Result<&'a Value> {
    require(
        state.get("closed") == &Value::Bool(false) && !state.get("successor").truth(),
        "declared main review room closed or continues as a successor; name current room",
    )?;
    let member = state
        .get("participants")
        .array()?
        .iter()
        .find(|m| m.text("participant") == who)
        .ok_or_else(|| error("declared main reviewer missing"))?;
    require(
        member.get("joined") == &Value::Bool(true)
            && member.text("role") == "main"
            && !member.get("parent").truth()
            && member.text("provider") == "codex"
            && json::hex(member.text("consumer"), 32),
        "declared main reviewer is not a current bound Desktop room main",
    )?;
    Ok(member)
}
pub(crate) fn completion_owner(
    plan: &Plan<'_>,
    req: &Request,
    bundle: &Value,
    task: &Value,
    sha: &str,
    _frozen: Option<&Value>,
    approval_only: bool,
) -> Result<Value> {
    if !matches!(
        req.action,
        crate::Action::AcceptMainResearch | crate::Action::ApproveMainResearch
    ) {
        let history = provenance::live_history(plan, &plan.context.room_id)?;
        return provenance::owner(
            plan,
            bundle,
            task,
            &history,
            false,
            &plan.context.panel_main_attempt,
        );
    }
    let descriptor = bundle.get("reviewer");
    let mut original = task.clone();
    let declared = bundle.get("owner").text("participant");
    if task.text("claimed_by_participant").is_empty() {
        let b = bundle.get("legacy_reviewer_binding");
        json::keys(
            b,
            &["participant", "original_contract", "review_summary"],
            &[],
        )?;
        require(
            b.text("participant") == declared
                && *b.get("original_contract") == json::projection(task)?,
            "unbound legacy review needs exact explicit current reviewer binding",
        )?;
        text(
            b.get("review_summary").str()?,
            "current legacy reviewer binding",
            1000,
        )?;
        original.set("claimed_by_participant", string(declared))?;
    } else {
        require(
            task.text("claimed_by_participant") == declared
                && bundle.opt("legacy_reviewer_binding").is_none(),
            "foreign native claim cannot be reassigned by main research",
        )?;
    }
    if descriptor.text("kind") == "native-panel-main" {
        json::keys(descriptor, &["kind"], &[])?;
        require(
            !approval_only && !req.has("main-review-approval"),
            "native panel main retains direct acceptance contract",
        )?;
        let history = provenance::live_history(plan, &plan.context.room_id)?;
        let mut owner = provenance::owner(
            plan,
            bundle,
            &original,
            &history,
            false,
            &plan.context.panel_main_attempt,
        )?;
        let mut reviewer = owner.clone();
        reviewer.set("schema", number(1))?;
        reviewer.set("kind", string("native-panel-main"))?;
        reviewer.set("attempt_id", string(&plan.context.panel_main_attempt))?;
        reviewer.set("bundle_sha256", string(sha))?;
        owner.set("reviewer", reviewer)?;
        return Ok(owner);
    }
    json::keys(descriptor, &["kind", "room_id", "approval"], &[])?;
    plan.context.parent()?;
    require(
        plan.context.attempt_id.is_empty()
            && plan.context.harness_call_id.is_empty()
            && plan.context.panel_main_attempt.is_empty()
            && descriptor.text("kind") == "codex-desktop-main"
            && bundle.get("owner").text("parent_provider") == "codex",
        "Desktop main approval unavailable to child, delegated or panel caller",
    )?;
    let history = provenance::live_history(plan, descriptor.get("room_id").str()?)?;
    desktop_member(&history.room, declared)?;
    let reviewer = if approval_only {
        require(
            req.has("approve-reviewed-bundle"),
            "explicit reviewed bundle approval flag required",
        )?;
        empty()
    } else {
        let (reference, marker) = runtime_record(plan, req, bundle, sha)?;
        let request = object([
            ("policy", string(POLICY)),
            (
                "execution_repo",
                json::os(plan.context.execution_repo.as_os_str()),
            ),
            (
                "approve_argv",
                crate::json::list(
                    argv(plan, bundle, sha, marker.text("bundle_path"))?
                        .iter()
                        .map(String::as_str),
                ),
            ),
            ("approval", descriptor.get("approval").clone()),
            ("approved_marker", marker.clone()),
            (
                "expected_snapshot_sha256",
                marker.get("snapshot_sha256").clone(),
            ),
            (
                "code_manifest_sha256",
                marker.get("code_manifest_sha256").clone(),
            ),
        ]);
        let captured = plan.services.capture_main_reviewer(&MainReviewerRequest {
            state_repo: &plan.context.state_repo,
            execution_repo: &plan.context.execution_repo,
            task_id: task.text("id"),
            bundle_sha256: sha,
            room_id: descriptor.text("room_id"),
            participant: declared,
            approval_ref: &reference,
            request: &request,
        })?;
        let proof = json::parse(&captured.raw)?;
        validate_reviewer(
            plan,
            bundle,
            &proof,
            sha,
            marker.get("snapshot").get("provenance"),
            true,
        )?;
        proof
    };
    Ok(object([
        ("participant", string(declared)),
        ("parent_provider", string("codex")),
        ("room_id", descriptor.get("room_id").clone()),
        ("reviewer", reviewer),
    ]))
}
pub(crate) fn validate_reviewer(
    plan: &Plan<'_>,
    bundle: &Value,
    reviewer: &Value,
    sha: &str,
    provenance: &Value,
    _current: bool,
) -> Result<()> {
    let history = provenance::load(plan, provenance, false)?;
    if reviewer.text("kind") == "native-panel-main" {
        require(
            bundle.get("reviewer").text("kind") == "native-panel-main",
            "native reviewer cannot replace approved Desktop contract",
        )?;
        json::keys(
            reviewer,
            &[
                "schema",
                "kind",
                "participant",
                "parent_provider",
                "room_id",
                "attempt_id",
                "bundle_sha256",
            ],
            &[],
        )?;
        require(
            reviewer.get("schema").is_int(1)
                && reviewer.text("bundle_sha256") == sha
                && reviewer.get("participant") == bundle.get("owner").get("participant")
                && reviewer.get("parent_provider") == bundle.get("owner").get("parent_provider")
                && reviewer.get("room_id") == provenance.get("room_id"),
            "immutable main reviewer binding mismatch",
        )?;
        let mut original = bundle.get("original_contract").clone();
        original.set(
            "claimed_by_participant",
            bundle.get("owner").get("participant").clone(),
        )?;
        provenance::owner(
            plan,
            bundle,
            &original,
            &history,
            true,
            reviewer.text("attempt_id"),
        )?;
        return Ok(());
    }
    require(
        bundle.get("reviewer").text("kind") == "codex-desktop-main",
        "Desktop reviewer cannot replace native panel contract",
    )?;
    let member = desktop_member(&history.room, bundle.get("owner").text("participant"))?;
    json::keys(
        reviewer,
        &[
            "schema",
            "policy",
            "action",
            "authorized_action",
            "task_id",
            "bundle_sha256",
            "room_id",
            "participant",
            "approval_ref",
            "marker_nonce_sha256",
            "snapshot_sha256",
            "code_manifest_sha256",
            "state_repo_sha256",
            "execution_repo_sha256",
            "state_git_namespace_sha256",
            "source_git_namespace_sha256",
            "git_common_sha256",
            "native_consumer",
            "session_id_sha256",
            "header_sha256",
            "room_binding_sha256",
            "approve_invocation_sha256",
            "native_approval_execution",
            "policy_approval",
            "proof_sha256",
        ],
        &[],
    )?;
    let mut body = reviewer.clone();
    body.remove("proof_sha256");
    require(
        json::hash(&body)? == json::digest(reviewer.get("proof_sha256"))?,
        "invalid captured main reviewer proof digest",
    )?;
    let marker = record(plan, reviewer.get("approval_ref"), bundle, sha, false)?;
    let common = plan.state_git().namespace()?;
    let expected = object([
        ("schema", number(2)),
        ("policy", string(POLICY)),
        ("action", string("approve-main-research")),
        ("authorized_action", string("accept-main-research")),
        ("task_id", bundle.get("task_id").clone()),
        ("bundle_sha256", string(sha)),
        ("room_id", provenance.get("room_id").clone()),
        (
            "participant",
            bundle.get("owner").get("participant").clone(),
        ),
        ("native_consumer", member.get("consumer").clone()),
        (
            "state_repo_sha256",
            string(json::sha(plan.context.state_repo.as_os_str().as_bytes())),
        ),
        ("state_git_namespace_sha256", string(&common)),
        ("source_git_namespace_sha256", string(&common)),
        ("git_common_sha256", string(&common)),
        (
            "marker_nonce_sha256",
            string(json::sha(marker.text("nonce"))),
        ),
        ("snapshot_sha256", marker.get("snapshot_sha256").clone()),
        (
            "code_manifest_sha256",
            marker.get("code_manifest_sha256").clone(),
        ),
        (
            "execution_repo_sha256",
            marker.get("execution_repo_sha256").clone(),
        ),
        (
            "approve_invocation_sha256",
            marker.get("approve_invocation_sha256").clone(),
        ),
    ]);
    for (k, v) in expected.object()? {
        require(
            reviewer.get(std::str::from_utf8(k).map_err(|_| error("invalid reviewer field"))?) == v,
            "immutable Desktop completed approval binding mismatch",
        )?;
    }
    for name in [
        "bundle_sha256",
        "marker_nonce_sha256",
        "snapshot_sha256",
        "code_manifest_sha256",
        "state_repo_sha256",
        "execution_repo_sha256",
        "state_git_namespace_sha256",
        "source_git_namespace_sha256",
        "git_common_sha256",
        "session_id_sha256",
        "header_sha256",
        "room_binding_sha256",
        "approve_invocation_sha256",
    ] {
        json::digest(reviewer.get(name))?;
    }
    require(
        json::hex(reviewer.text("native_consumer"), 32)
            && reviewer.text("native_consumer") == &reviewer.text("session_id_sha256")[..32],
        "native reviewer consumer differs from producer session",
    )?;
    let binding = object([
        ("room", provenance.get("room_id").clone()),
        (
            "participant",
            bundle.get("owner").get("participant").clone(),
        ),
        ("consumer", member.get("consumer").clone()),
        ("role", string("main")),
        ("provider", string("codex")),
    ]);
    require(
        json::hash(&binding)? == reviewer.text("room_binding_sha256"),
        "native room binding digest mismatch",
    )?;
    let native = reviewer.get("native_approval_execution");
    json::keys(
        native,
        &[
            "call_record_sha256",
            "call_input_sha256",
            "call_id_sha256",
            "completion_record_sha256",
            "backend_item_id_sha256",
            "backend_process_id_sha256",
            "output_record_sha256",
            "envelope_sha256",
            "workdir_sha256",
        ],
        &[],
    )?;
    for (_, v) in native.object()? {
        json::digest(v)?;
    }
    let envelope = object([
        ("kind", string("oms-main-read-approved")),
        ("approval", reviewer.get("approval_ref").clone()),
    ]);
    require(
        native.text("envelope_sha256") == json::encoded_hash(&envelope)?
            && native.get("workdir_sha256") == reviewer.get("execution_repo_sha256"),
        "native completed approval output/workdir differs",
    )?;
    let approved = reviewer.get("policy_approval");
    json::keys(
        approved,
        &[
            "message_id",
            "question_item_id",
            "question_sha256",
            "answer_sha256",
            "record_sha256",
            "prompt_record_sha256",
            "prompt_call_id_sha256",
            "policy_adapter",
        ],
        &[],
    )?;
    let declared = bundle.get("reviewer").get("approval");
    require(
        declared.text("policy") == POLICY && !approved.text("message_id").is_empty(),
        "unsupported native main review policy approval",
    )?;
    for name in [
        "message_id",
        "question_item_id",
        "question_sha256",
        "answer_sha256",
    ] {
        require(
            approved.get(name) == declared.get(name),
            "captured policy approval differs from reviewed declaration",
        )?;
    }
    for name in [
        "question_sha256",
        "answer_sha256",
        "record_sha256",
        "prompt_record_sha256",
        "prompt_call_id_sha256",
    ] {
        json::digest(approved.get(name))?;
    }
    let prompt = json::parse(approved.get("question_item_id").str()?.as_bytes())?;
    let args = prompt.array()?;
    require(
        args.len() == 3
            && args[0].str() == Ok("request_user_input_async")
            && args[1].str().is_ok_and(|s| {
                s.starts_with("call_")
                    && s.len() <= 133
                    && s[5..]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
            && args[2].int().is_ok_and(|n| (0..3).contains(&n))
            && approved.text("prompt_call_id_sha256") == json::sha(args[1].str()?),
        "invalid native question reference",
    )?;
    require(
        match approved.text("policy_adapter") {
            "canonical-v1" => approved.text("answer_sha256") == json::sha("approve"),
            "legacy-korean-v1" => {
                approved.text("question_sha256")
                    == "5c16741f07d2e3a1ebdff52109fe1a1019674ba3f70229b9e021e5a68b510003"
                    && approved.text("answer_sha256") == json::sha("검증된 메인 READ도 지원")
            }
            _ => false,
        },
        "native approval did not affirm main READ completion policy",
    )
}
pub(crate) fn publish(
    plan: &Plan<'_>,
    req: &Request,
    bundle: &Value,
    sha: &str,
    snapshot: &Value,
    verification: &Value,
) -> Result<Response> {
    require(
        req.has("approve-reviewed-bundle"),
        "explicit reviewed bundle approval flag required",
    )?;
    let snapshot = neutral(snapshot)?;
    validate_verification(verification, &snapshot)?;
    let marker = object([
        ("schema", number(1)),
        ("kind", string("oms-main-read-approval-v1")),
        ("authorized_action", string("accept-main-research")),
        ("nonce", string(crate::storage::random_hex(32)?)),
        ("task_id", bundle.get("task_id").clone()),
        ("bundle_sha256", string(sha)),
        ("room_id", bundle.get("reviewer").get("room_id").clone()),
        (
            "participant",
            bundle.get("owner").get("participant").clone(),
        ),
        ("lease_id", bundle.get("lease_id").clone()),
        ("state", bundle.get("state").clone()),
        ("plan_sha256", bundle.get("plan_sha256").clone()),
        ("task_sha256", bundle.get("task_sha256").clone()),
        ("snapshot_sha256", string(json::encoded_hash(&snapshot)?)),
        (
            "code_manifest_sha256",
            string(json::encoded_hash(snapshot.get("controller_manifest"))?),
        ),
        (
            "state_repo_sha256",
            string(json::sha(plan.context.state_repo.as_os_str().as_bytes())),
        ),
        (
            "execution_repo_sha256",
            string(json::sha(
                plan.context.execution_repo.as_os_str().as_bytes(),
            )),
        ),
        ("git_common_sha256", string(plan.git().namespace()?)),
        ("snapshot", snapshot),
        ("verification", verification.clone()),
        ("bundle_path", string(req.value("completion-bundle")?)),
        (
            "approve_invocation_sha256",
            string(json::sha(invocation(
                plan,
                bundle,
                sha,
                req.value("completion-bundle")?,
            )?)),
        ),
    ]);
    let (reference, _) = plan.store.immutable(
        ".oms/plan/completion-captures",
        &json::encoded(&marker)?,
        ".json",
        MAX_JSON,
    )?;
    let out = object([
        ("kind", string("oms-main-read-approved")),
        ("approval", reference),
    ]);
    Ok(Response {
        stdout: json::encoded(&out)?,
        ..Default::default()
    })
}
pub(crate) fn landing_authority(plan: &Plan<'_>, intent: &Value, _completed: &Value) -> Result<()> {
    let id = intent.text("approval");
    if id.is_empty() {
        require(
            intent.text("approval_version").is_empty(),
            "landing approval version without approval",
        )?;
        return Ok(());
    }
    let proof = plan
        .services
        .landing_approval(&plan.context.state_repo, id)?;
    let public = json::parse(&proof.public_record)?;
    let rows = crate::storage::rows(&proof.private_rows)?;
    let mut relevant = rows
        .iter()
        .filter(|r| r.text("approval_id") == id)
        .collect::<Vec<_>>();
    require(
        !relevant.is_empty(),
        "private landing approval history missing",
    )?;
    relevant.sort_by_key(|r| r.get("version").int().unwrap_or(-1));
    let request = relevant[0];
    require(
        request.get("schema").is_int(1)
            && request.text("event_type") == "approval.requested"
            && request.text("repo_hash")
                == json::sha(plan.context.state_repo.as_os_str().as_bytes())
            && request.text("action") == "patch-land"
            && request.text("object_id") == format!("patch:{}", intent.text("patch_sha")),
        "landing approval request/action/namespace mismatch",
    )?;
    for (stored, bound) in [
        ("base_sha", "base_sha"),
        ("patch_sha", "patch_sha"),
        ("task_id", "task"),
        ("lease_id", "lease"),
    ] {
        require(
            request.text(stored) == intent.text(bound) && public.text(stored) == intent.text(bound),
            "landing approval differs from exact patch/base/task/lease",
        )?;
    }
    let fields = [
        "attempt_id",
        "action",
        "object_id",
        "summary",
        "base_sha",
        "patch_sha",
        "task_id",
        "lease_id",
        "profile",
        "parameters",
    ];
    let mut action = object([("repo_hash", request.get("repo_hash").clone())]);
    for field in fields {
        let value = request.opt(field).cloned().unwrap_or_else(|| {
            if field == "parameters" {
                empty()
            } else if field == "profile" {
                string("trusted-local")
            } else {
                string("")
            }
        });
        action.set(field, value)?;
    }
    require(
        json::hash(&action)? == request.text("action_digest")
            && public.get("action_digest") == request.get("action_digest"),
        "landing approval action digest mismatch",
    )?;
    let version = intent
        .text("approval_version")
        .parse::<i128>()
        .map_err(|_| error("invalid landing approval version"))?;
    let mut state = "requested";
    let mut events = BTreeSet::new();
    let mut approved = false;
    for (index, row) in relevant.iter().enumerate() {
        require(
            row.get("schema").is_int(1)
                && row.get("version").is_int((index + 1) as i128)
                && events.insert(row.text("event_id"))
                && crate::engine::parse_ts(row.text("ts")).is_some(),
            "malformed private landing approval history",
        )?;
        if index == 0 {
            continue;
        }
        require(
            row.get("expected_version").is_int(index as i128),
            "private approval CAS version mismatch",
        )?;
        let target = match (state, row.text("event_type")) {
            ("requested", "approval.approved") => {
                require(
                    !row.text("grant_id").is_empty() && json::hex(row.text("grant_hash"), 64),
                    "private approved grant binding missing",
                )?;
                approved = true;
                "approved"
            }
            ("approved", "approval.consuming") => "consuming",
            ("consuming", "approval.consumed") => "consumed",
            _ => {
                return Err(error(
                    "private landing approval state transition is not successful",
                ));
            }
        };
        require(
            row.text("state") == target,
            "private landing approval state mismatch",
        )?;
        state = target;
    }
    require(
        approved
            && state == "consumed"
            && public.text("approval_id") == id
            && public.text("state") == "consumed"
            && public.get("version").is_int(version + 1)
            && relevant.last().unwrap().get("version") == public.get("version"),
        "public/private landing approval terminal binding mismatch",
    )
}
