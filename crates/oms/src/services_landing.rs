//! Native landing authorities for `ControlServices`.
//!
//! Every mutation forwards to the owning producer under its own lock: plan
//! lease/receipt compare-and-set to `oms-command-plan`, approval reservation to
//! the approval inbox, ledgers and findings to their front doors. Inputs this
//! adapter cannot prove (panel acceptance prefixes, goal batches, dispatch
//! rework grants) fail closed instead of widening admission.
use oms_command_landing::{Error, Result, V, obj, services::LandingRequest};
use oms_command_plan::json::Value as PlanValue;
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn fail(message: impl Into<String>) -> Error {
    Error::new(2, message)
}
fn argv(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}
fn first_error(stderr: &[u8], fallback: &str) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .map(|l| l.trim().trim_start_matches("error: "))
        .find(|l| !l.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
fn interrupt(flag: &AtomicBool) -> Result<()> {
    if flag.load(std::sync::atomic::Ordering::Relaxed) {
        Err(Error::new(130, "interrupted"))
    } else {
        Ok(())
    }
}
fn harness_child() -> bool {
    ["OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty() && v != "0"))
}
fn text(v: &V, key: &str) -> String {
    v.get(key).text().unwrap_or("").to_owned()
}

// ---------------------------------------------------------------- plan ----

fn plan_run(
    services: &dyn oms_command_plan::Services,
    repo: &Path,
    state: &Path,
    mut request: oms_command_plan::Request,
    interrupted: &AtomicBool,
) -> Result<Vec<u8>> {
    request.repo = Some(repo.to_owned());
    let response = landing_plan(services, repo, state, &request, interrupted)?.execute(&request)?;
    if response.code != 0 {
        return Err(Error::new(
            response.code,
            first_error(&response.stderr, "plan transition refused"),
        ));
    }
    Ok(response.stdout)
}
fn landing_plan<'a>(
    services: &'a dyn oms_command_plan::Services,
    repo: &Path,
    state: &Path,
    request: &oms_command_plan::Request,
    interrupted: &'a AtomicBool,
) -> Result<oms_command_plan::Plan<'a>> {
    let mut context = oms_command_plan::Context::from_environment(request, repo)?;
    context.state_repo = state.to_owned();
    context.plan_file = state.join(".oms/plan/tasks.json");
    Ok(oms_command_plan::Plan::open(
        context,
        services,
        interrupted,
    )?)
}

fn reviewed_task_snapshot(
    services: &dyn oms_command_plan::Services,
    request: &LandingRequest<'_>,
    interrupted: &AtomicBool,
) -> Result<PlanValue> {
    let Some(expected) = &request.policy.expected_plan_review else {
        return task_snapshot(services, request, interrupted);
    };
    let mut query = oms_command_plan::Request::new(oms_command_plan::Action::EvidenceSnapshot)
        .with("id", request.task);
    query.repo = Some(request.execution_repo.to_owned());
    let plan = landing_plan(
        services,
        request.execution_repo,
        request.state_repo,
        &query,
        interrupted,
    )?;
    let task = plan.check_landing_review(expected)?;
    stored_task(task)
}

/// Current task as stored: the snapshot's added `plan_id` and contract
/// staleness are view fields that the stored record `finish` hashes lacks.
fn task_snapshot(
    services: &dyn oms_command_plan::Services,
    request: &LandingRequest<'_>,
    interrupted: &AtomicBool,
) -> Result<PlanValue> {
    let raw = plan_run(
        services,
        request.execution_repo,
        request.state_repo,
        oms_command_plan::Request::new(oms_command_plan::Action::EvidenceSnapshot)
            .with("id", request.task),
        interrupted,
    )?;
    stored_task(oms_command_plan::json::parse(&raw)?)
}

fn stored_task(mut task: PlanValue) -> Result<PlanValue> {
    // A view whose state was overridden is not the stored record finish hashes.
    if task.opt("stored_state").is_some() {
        return Err(fail("plan task completion is unverified; not landable"));
    }
    if task.get("contract_stale").truth() {
        return Err(fail(
            "plan task contracts are stale; refresh them before landing",
        ));
    }
    for view in ["plan_id", "contract_stale", "stale_contracts"] {
        task.remove(view);
    }
    Ok(task)
}
fn done_receipt(task: &PlanValue, patch: &Path) -> Result<String> {
    let mut done = task.clone();
    done.set(
        "patch",
        oms_command_plan::json::string(patch.to_string_lossy()),
    )?;
    Ok(oms_command_plan::json::receipt_digest(&done)?)
}

/// Controller salvage provenance keyed by the diff payload: a salvaged patch
/// lands only with owner consent, and encoding loss needs its own consent.
fn salvage_consent(request: &LandingRequest<'_>) -> Result<()> {
    let payload = oms_command_plan::json::sha(oms_panel_core::salvage::payload(request.patch));
    let path = request
        .state_repo
        .join(".oms/salvage-provenance")
        .join(format!("{payload}.json"));
    let raw = match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(m) if m.is_file() && m.len() <= 8 * 1024 * 1024 => std::fs::read(&path)?,
        _ => return Err(fail("invalid controller salvage provenance")),
    };
    let value = V::parse(&raw).map_err(|_| fail("invalid controller salvage provenance"))?;
    let records = value.get("records").array();
    if value.get("schema").py() != "1"
        || value.get("payload_sha256").text() != Some(payload.as_str())
        || records.is_empty()
        || records.iter().any(|r| {
            !matches!(r.get("salvaged_from"), V::Obj(_))
                || !matches!(r.get("encoding_warnings"), V::Arr(_))
        })
    {
        return Err(fail("invalid controller salvage provenance"));
    }
    if !request.policy.accept_salvaged {
        return Err(fail(
            "salvaged patch requires --accept-salvaged after owner review",
        ));
    }
    if records
        .iter()
        .any(|r| !r.get("encoding_warnings").array().is_empty())
        && !request.policy.accept_encoding_loss
    {
        return Err(fail(
            "salvaged patch has encoding loss warnings; requires --accept-encoding-loss",
        ));
    }
    Ok(())
}

pub(crate) fn landing_review(
    services: &dyn oms_command_plan::Services,
    request: &LandingRequest<'_>,
    interrupted: &AtomicBool,
) -> Result<V> {
    let admission = request.admission;
    if admission.get("kind").text() != Some("patch-admit")
        || admission.get("exit").py() != "0"
        || admission.get("patch_sha256").text() != Some(request.patch_sha256)
        || admission.get("base").text() != Some(request.base)
    {
        return Err(fail(
            "landing review requires this exact admitted patch on this base",
        ));
    }
    if request.policy.expected_plan_review.is_some()
        && (admission.get("verification").text() != Some("command")
            || !matches!(admission.get("verify_exit"), V::Num(exit) if exit == "0"))
    {
        return Err(fail(
            "bound Plan landing requires successful command verification",
        ));
    }
    if oms_command_plan::json::sha(request.patch) != request.patch_sha256 {
        return Err(fail("landing review requires the admitted patch bytes"));
    }
    salvage_consent(request)?;
    if request.task.is_empty() {
        return Ok(obj([
            ("plan_receipt_sha", "".into()),
            ("plan_done_receipt_sha", "".into()),
        ]));
    }
    let mut task = reviewed_task_snapshot(services, request, interrupted)?;
    if let Some(expected) = &request.policy.expected_plan_review {
        expected.stamp_landing_task(&mut task)?;
    }
    if task.text("state") != "review"
        || task.text("lease_id").is_empty()
        || task.text("review_lease_id") != task.text("lease_id")
    {
        return Err(fail(
            "plan task review is stale or not under its claim lease",
        ));
    }
    if request.frozen_patch.as_os_str().is_empty() {
        return Err(fail("landing review requires the frozen landing patch"));
    }
    Ok(obj([
        (
            "plan_receipt_sha",
            oms_command_plan::json::receipt_digest(&task)?.into(),
        ),
        (
            "plan_done_receipt_sha",
            done_receipt(&task, request.frozen_patch)?.into(),
        ),
    ]))
}

// ----------------------------------------------------------- approvals ----

fn approvals(state: &Path, args: &[&str], interrupted: &AtomicBool) -> Result<String> {
    let mut all = argv(&["--repo"]);
    all.push(state.as_os_str().to_owned());
    all.extend(argv(args));
    let (code, out, err) = oms_command_operations::run(
        oms_command_operations::App::Approvals,
        &all,
        state,
        interrupted,
    );
    if code != 0 {
        return Err(Error::new(
            code,
            first_error(&err, "approval inbox refused"),
        ));
    }
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}
fn approval_row(state: &Path, id: &str) -> Result<Option<V>> {
    Ok(oms_command_operations::load_approvals(state)?
        .into_iter()
        .find(|r| r.get("approval_id").text() == Some(id)))
}
fn profile() -> Result<String> {
    let value = std::env::var("OMS_EXECUTION_PROFILE").unwrap_or_default();
    let value = if value.is_empty() {
        "trusted-local".to_owned()
    } else {
        value
    };
    if !["trusted-local", "isolated", "remote"].contains(&value.as_str()) {
        return Err(fail(format!("invalid OMS_EXECUTION_PROFILE: {value}")));
    }
    Ok(value)
}
/// The legacy admission contract an approval binds: verifier digest and every
/// admission exception. A grant for a plain landing cannot widen admission.
fn parameters(request: &LandingRequest<'_>, verify: &str) -> V {
    let policy = request.policy;
    let mut value = obj([
        ("admission_contract_schema", 1usize.into()),
        ("allow_restructure", policy.allow_restructure.into()),
        ("allow_test_reduction", policy.allow_test_reduction.into()),
        ("allow_verifier_change", policy.allow_verifier_change.into()),
        ("accept_salvaged", policy.accept_salvaged.into()),
        ("accept_encoding_loss", policy.accept_encoding_loss.into()),
        ("ml", policy.ml.into()),
        ("verify_explicit", (!verify.is_empty()).into()),
        (
            "verify_sha256",
            oms_command_plan::json::sha(verify.as_bytes()).into(),
        ),
        ("executor_id", V::Null),
        ("executor_soul_sha256", V::Null),
    ]);
    if !policy.allow_floor_change.is_empty() {
        value.set(
            "allow_floor_change_reason",
            policy.allow_floor_change.as_str(),
        );
    }
    value
}

pub(crate) struct Grant {
    approval: String,
    proof: String,
    version: String,
}

pub(crate) fn landing_approval(
    services: &dyn oms_command_plan::Services,
    request: &LandingRequest<'_>,
    approval: &str,
    grant: &str,
    version: &str,
    request_only: bool,
    interrupted: &AtomicBool,
) -> Result<(V, Option<Grant>)> {
    let profile = profile()?;
    let (lease, verify) = if request.task.is_empty() {
        (String::new(), request.policy.verify.clone())
    } else {
        let task = task_snapshot(services, request, interrupted)?;
        let stored = task.text("verify").to_owned();
        (
            task.text("lease_id").to_owned(),
            if request.policy.verify.is_empty() {
                stored
            } else {
                request.policy.verify.clone()
            },
        )
    };
    let parameters = parameters(request, &verify);
    if request_only {
        if !approval.is_empty() || !grant.is_empty() || !version.is_empty() {
            return Err(fail(
                "--request-approval cannot be combined with approval consumption options",
            ));
        }
        let object = format!("patch:{}", request.patch_sha256);
        let encoded = String::from_utf8(parameters.json(false)?).map_err(|e| e.to_string())?;
        let attempt = std::env::var("OMS_ATTEMPT_ID").unwrap_or_default();
        let mut args = vec![
            "request",
            "--action",
            "patch-land",
            "--object-id",
            &object,
            "--summary",
            "Land reviewed patch",
            "--base-sha",
            request.base,
            "--patch-sha",
            request.patch_sha256,
            "--profile",
            &profile,
            "--parameters-json",
            &encoded,
        ];
        for (flag, value) in [
            ("--attempt", attempt.as_str()),
            ("--task-id", request.task),
            ("--lease-id", lease.as_str()),
        ] {
            if !value.is_empty() {
                args.extend([flag, value]);
            }
        }
        let id = approvals(request.state_repo, &args, interrupted)?;
        return Ok((
            obj([
                ("approval_id", id.into()),
                ("state", "requested".into()),
                ("action", "patch-land".into()),
                ("object_id", object.into()),
                ("patch_sha", request.patch_sha256.into()),
                ("base_sha", request.base.into()),
                ("task_id", request.task.into()),
                ("lease_id", lease.into()),
                ("profile", profile.into()),
            ]),
            None,
        ));
    }
    let row = approval_row(request.state_repo, approval)?
        .ok_or_else(|| fail("unknown landing approval"))?;
    let grant_id = grant.split_once('.').map_or("", |(id, _)| id);
    if row.get("profile").text() != Some(profile.as_str())
        || row.get("parameters") != &parameters
        || text(&row, "lease_id") != lease
        || grant_id.is_empty()
        || row.get("_grant_id").text() != Some(grant_id)
    {
        return Err(fail(
            "approval grant does not match this exact landing policy",
        ));
    }
    Ok((
        obj([("approval_id", approval.into())]),
        Some(Grant {
            approval: approval.to_owned(),
            proof: grant.to_owned(),
            version: version.to_owned(),
        }),
    ))
}

// ------------------------------------------------- landing transaction ----

pub(crate) fn fence_landing(
    services: &dyn oms_command_plan::Services,
    request: &LandingRequest<'_>,
    intent: &V,
    grant: Option<Grant>,
    interrupted: &AtomicBool,
) -> Result<()> {
    let approval = text(intent, "approval");
    let reservation = match (&grant, approval.is_empty()) {
        (None, true) => None,
        (Some(g), false)
            if g.approval == approval
                && oms_command_operations::value::add(&g.version, "1")
                    == text(intent, "approval_version") =>
        {
            Some(g)
        }
        _ => {
            return Err(fail(
                "landing approval reservation does not match the intent",
            ));
        }
    };
    if !request.task.is_empty() {
        let mut task = reviewed_task_snapshot(services, request, interrupted)?;
        if let Some(expected) = &request.policy.expected_plan_review {
            expected.stamp_landing_task(&mut task)?;
        }
        let lease = text(intent, "lease");
        if task.text("lease_id") != lease
            || oms_command_plan::json::receipt_digest(&task)? != text(intent, "plan_receipt_sha")
        {
            return Err(fail(
                "plan task lease or review receipt changed; not applied",
            ));
        }
        let mut land = oms_command_plan::Request::new(oms_command_plan::Action::Land)
            .with("id", request.task)
            .with("lease-id", lease.as_str())
            .with("expected-review-patch", task.text("patch"))
            .with("expected-review-patch-sha256", request.patch_sha256)
            .with("expected-review-verify", task.text("verify"))
            .with("expected-review-executor-id", "")
            .with("expected-review-executor-soul-sha256", "")
            .with("expected-review-lease-id", task.text("review_lease_id"));
        if let Some(expected) = &request.policy.expected_plan_review {
            let encoded = String::from_utf8(expected.encode()?)
                .map_err(|_| fail("Plan review binding encoding unavailable"))?;
            land = land.with("expected-review-binding", encoded);
        }
        plan_run(
            services,
            request.execution_repo,
            request.state_repo,
            land,
            interrupted,
        )?;
    }
    if let Some(g) = reservation {
        let reserved = approvals(
            request.state_repo,
            &[
                "begin-consume",
                "--approval",
                &g.approval,
                "--token",
                &g.proof,
                "--expected-version",
                &g.version,
                "--consumer",
                "patch-land",
            ],
            interrupted,
        );
        if let Err(error) = reserved {
            // The tree is untouched: release the fence entered above so the
            // task is not stranded in `landing` until a recovery pass.
            if !request.task.is_empty() {
                let _ = release_fenced(services, request, intent, interrupted);
            }
            return Err(error);
        }
    }
    Ok(())
}
/// Release only the exact claim and review receipt this intent fenced.
fn release_fenced(
    services: &dyn oms_command_plan::Services,
    request: &LandingRequest<'_>,
    intent: &V,
    interrupted: &AtomicBool,
) -> Result<()> {
    let task = task_snapshot(services, request, interrupted)?;
    let lease = text(intent, "lease");
    if task.text("state") != "landing"
        || task.text("lease_id") != lease
        || oms_command_plan::json::receipt_digest(&task)? != text(intent, "plan_receipt_sha")
    {
        return Ok(());
    }
    let mut release = oms_command_plan::Request::new(oms_command_plan::Action::Release)
        .with("id", request.task)
        .with(
            "expected-landing-receipt-sha256",
            text(intent, "plan_receipt_sha"),
        );
    if !lease.is_empty() {
        release = release.with("lease-id", lease.as_str());
    }
    if let Some(encoded) = intent_review_binding(intent)? {
        release = release.with("expected-review-binding", encoded);
    }
    plan_run(
        services,
        request.execution_repo,
        request.state_repo,
        release,
        interrupted,
    )
    .map(drop)
}

fn intent_review_binding(intent: &V) -> Result<Option<String>> {
    if intent.get("plan_review_binding") == &V::Null {
        return Ok(None);
    }
    let hash = oms_command_landing::landing::receipt_digest(intent)?;
    if text(intent, "receipt_sha") != hash {
        return Err(fail("landing receipt changed before Plan transition"));
    }
    let raw = intent.get("plan_review_binding").json(false)?;
    Ok(Some(String::from_utf8(raw).map_err(|_| {
        fail("Plan review binding encoding unavailable")
    })?))
}

fn finish_task(
    services: &dyn oms_command_plan::Services,
    request: &LandingRequest<'_>,
    intent: &V,
    refreeze: bool,
    interrupted: &AtomicBool,
) -> Result<()> {
    let mut finish = oms_command_plan::Request::new(oms_command_plan::Action::Finish)
        .with("id", request.task)
        .with("patch", text(intent, "patch"))
        .with(
            "expected-landing-receipt-sha256",
            text(intent, "plan_receipt_sha"),
        );
    if !text(intent, "lease").is_empty() {
        finish = finish.with("lease-id", text(intent, "lease"));
    }
    if let Some(encoded) = intent_review_binding(intent)? {
        finish = finish.with("expected-review-binding", encoded).with(
            "expected-landing-done-receipt-sha256",
            text(intent, "plan_done_receipt_sha"),
        );
    }
    if refreeze {
        finish = finish.flag("refreeze-acceptance");
    }
    plan_run(
        services,
        request.execution_repo,
        request.state_repo,
        finish,
        interrupted,
    )?;
    let done = task_snapshot(services, request, interrupted)?;
    if oms_command_plan::json::receipt_digest(&done)? != text(intent, "plan_done_receipt_sha") {
        return Err(fail(
            "finished plan task differs from the landing done receipt",
        ));
    }
    Ok(())
}
fn finish_approval(state: &Path, intent: &V, result: &str, interrupted: &AtomicBool) -> Result<()> {
    approvals(
        state,
        &[
            "finish-consume",
            "--approval",
            &text(intent, "approval"),
            "--expected-version",
            &text(intent, "approval_version"),
            "--result",
            result,
            "--consumer",
            "patch-land",
        ],
        interrupted,
    )
    .map(drop)
}

pub(crate) fn complete_landing(
    services: &dyn oms_command_plan::Services,
    request: &LandingRequest<'_>,
    intent: &V,
    interrupted: &AtomicBool,
) -> Result<()> {
    if !request.task.is_empty() {
        let refreeze =
            request.policy.allow_verifier_change || intent.get("verifier_change_consent").truth();
        finish_task(services, request, intent, refreeze, interrupted)?;
    }
    if !text(intent, "approval").is_empty() {
        finish_approval(request.state_repo, intent, "consumed", interrupted)?;
    }
    Ok(())
}

/// Idempotent: converged receipts are left alone. An applied landing whose
/// receipts cannot converge stays outstanding (`Err`); an unapplied one never
/// releases a task it cannot prove it fenced into `landing`.
pub(crate) fn recover_landing(
    services: &dyn oms_command_plan::Services,
    request: &LandingRequest<'_>,
    intent: &V,
    applied: bool,
    interrupted: &AtomicBool,
) -> Result<()> {
    if !request.task.is_empty() {
        let task = task_snapshot(services, request, interrupted)?;
        let lease = text(intent, "lease");
        let same = task.text("lease_id") == lease
            && oms_command_plan::json::receipt_digest(&task)? == text(intent, "plan_receipt_sha");
        match (applied, task.text("state")) {
            (true, "landing") if same => finish_task(
                services,
                request,
                intent,
                intent.get("verifier_change_consent").truth(),
                interrupted,
            )?,
            (true, "done")
                if oms_command_plan::json::receipt_digest(&task)?
                    == text(intent, "plan_done_receipt_sha") =>
            {
                let expected = intent_review_binding(intent)?
                    .map(|encoded| oms_command_plan::ReviewFence::parse(encoded.as_bytes()))
                    .transpose()?;
                let mut query =
                    oms_command_plan::Request::new(oms_command_plan::Action::EvidenceSnapshot)
                        .with("id", request.task);
                query.repo = Some(request.execution_repo.to_owned());
                let plan = landing_plan(
                    services,
                    request.execution_repo,
                    request.state_repo,
                    &query,
                    interrupted,
                )?;
                plan.check_landing_done(
                    &plan.load()?,
                    request.task,
                    expected.as_ref(),
                    request.patch_sha256,
                )?;
            }
            (true, _) => {
                return Err(fail(
                    "applied landing plan receipt did not converge; intent kept",
                ));
            }
            (false, "landing") if same => release_fenced(services, request, intent, interrupted)?,
            (false, _) => {}
        }
    }
    let approval = text(intent, "approval");
    if approval.is_empty() {
        return Ok(());
    }
    let row = approval_row(request.state_repo, &approval)?;
    let version = text(intent, "approval_version");
    let (state, current) = row.as_ref().map_or((String::new(), String::new()), |r| {
        (text(r, "state"), r.get("version").py())
    });
    let terminal = oms_command_operations::value::add(&version, "1");
    match (applied, state.as_str()) {
        (_, "consuming") if current == version => finish_approval(
            request.state_repo,
            intent,
            if applied { "consumed" } else { "failed" },
            interrupted,
        ),
        (true, "consumed") if current == terminal => Ok(()),
        (true, _) => Err(fail(
            "applied landing approval receipt did not converge; intent kept",
        )),
        (false, _) => Ok(()),
    }
}

// ------------------------------------------------- admission context ----

/// Allowed paths, panel acceptance and goal batch obligations for admission.
/// Only the plan's own scope is produced natively; anything that could add
/// paths or verifiers fails closed rather than being silently dropped.
pub(crate) struct AdmissionContext;

fn jsonl(path: &Path) -> std::result::Result<Vec<V>, String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.to_string()),
    };
    raw.split(|b| *b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .map(|l| V::parse(l).map_err(|_| format!("malformed record in {}", path.display())))
        .collect()
}
impl oms_reference::binding::Context for AdmissionContext {
    fn effective_allowed_paths(
        &self,
        repo: &Path,
        task: &V,
        _interrupted: &AtomicBool,
    ) -> oms_reference::Result<Vec<String>> {
        let mut allowed = task
            .get("allowed_paths")
            .array()
            .iter()
            .map(|v| v.text().map(str::to_owned).ok_or("invalid allowed path"))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        allowed.sort();
        allowed.dedup();
        let lease = task.get("lease_id").text().unwrap_or("");
        if allowed.is_empty() && lease.starts_with("lease_") {
            let (_, rows) = oms_command_operations::load_projection(repo)?;
            if rows
                .iter()
                .any(|r| r.get("refs").get("panel_lease_id").text() == Some(lease))
            {
                return Err("native dispatch rework grant producer unavailable".into());
            }
        }
        Ok(allowed)
    }
    fn accepted_prefix(
        &self,
        repo: &Path,
        execution: &Path,
        intents: &[oms_reference::binding::PanelIntent],
        _interrupted: &AtomicBool,
    ) -> oms_reference::Result<Vec<oms_reference::binding::AcceptedPrefix>> {
        let execution = std::fs::canonicalize(execution).map_err(|e| e.to_string())?;
        let local = intents.iter().any(|i| {
            !i.has_abandoned
                && i.intent
                    .get("worktree")
                    .text()
                    .is_some_and(|w| std::fs::canonicalize(w).ok().as_ref() == Some(&execution))
        });
        let plan = match std::fs::read(repo.join(".oms/plan/tasks.json")) {
            Ok(raw) => V::parse(&raw).map_err(|_| "panel acceptance plan is malformed")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => V::Null,
            Err(e) => return Err(e.to_string()),
        };
        let pending = match plan.get("tasks") {
            V::Obj(tasks) => tasks.values().any(|t| {
                t.get("panel_accept").truth()
                    && t.get("state").text() != Some("cancelled")
                    && !t.get("landing").get("sha").truth()
            }),
            _ => false,
        };
        if local || pending {
            return Err("native panel acceptance prefix producer unavailable".into());
        }
        Ok(vec![])
    }
    fn goal_batch_prefix(
        &self,
        repo: &Path,
        _execution: &Path,
        task: &str,
        _interrupted: &AtomicBool,
    ) -> oms_reference::Result<oms_reference::binding::BatchPrefix> {
        let rows = jsonl(&repo.join(".oms/plan/progress.jsonl"))?;
        if rows.iter().any(|r| {
            r.get("kind").text() == Some("goal-batch")
                && r.get("tasks")
                    .array()
                    .iter()
                    .any(|t| t.text() == Some(task) || t.get("id").text() == Some(task))
        }) {
            return Err("native goal batch prefix producer unavailable".into());
        }
        Ok(oms_reference::binding::BatchPrefix {
            batches: vec![],
            checks: vec![],
        })
    }
}

// -------------------------------------------------- coverage/reporting ----

fn criterion_id(id: &str) -> bool {
    (1..=160).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
pub(crate) fn coverage(
    runtime: &dyn oms_runtime::Services,
    source: &Path,
    repo: &Path,
    ids: &[String],
    evidence: Option<&V>,
    interrupted: &AtomicBool,
) -> Result<V> {
    if ids.is_empty() || ids.len() > 15 {
        return Err(fail("--covers accepts 1 to 15 criterion ids"));
    }
    if let Some(bad) = ids.iter().find(|id| !criterion_id(id)) {
        return Err(fail(format!(
            "covers id has an invalid shape: {}",
            bad.chars().take(80).collect::<String>()
        )));
    }
    let listed = V::Arr(ids.iter().map(|s| s.as_str().into()).collect());
    if let Some(row) = evidence {
        // Post-admission the ids were already proven against the projection;
        // the admission scope's monitor thread still runs, so no child is
        // spawned here. The record must carry exactly these covers.
        let status = row.get("status").text().unwrap_or("");
        if row.get("covers") != &listed
            || row.get("coverage").get("covers") != &listed
            || !["verified", "inconclusive", "failed"].contains(&status)
            || row.get("coverage").get("status").text() != Some(status)
        {
            return Err(fail(
                "admission record does not carry its criterion coverage",
            ));
        }
        return Ok(obj([
            ("covers", listed),
            ("status", status.into()),
            ("event_id", row.get("event_id").clone()),
        ]));
    }
    let mut args = argv(&["evidence", "show", "--repo"]);
    args.push(repo.as_os_str().to_owned());
    let response = oms_runtime::run(&args, repo, source, runtime, interrupted);
    if response.code != 0 {
        return Err(Error::new(
            3,
            format!(
                "covers validation needs the runtime projection: {}",
                first_error(&response.stderr, "projection failed")
            ),
        ));
    }
    let projected = V::parse(&response.stdout)?;
    let criteria = projected.get("criteria").array();
    let unknown: Vec<_> = ids
        .iter()
        .filter(|id| !criteria.iter().any(|c| c.get("id").text() == Some(id)))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(fail(format!(
            "unknown criterion id(s): {} ({} criteria in the current envelope)",
            unknown.join(" "),
            criteria.len()
        )));
    }
    let lineages: std::collections::BTreeSet<_> = criteria
        .iter()
        .filter(|c| ids.iter().any(|id| c.get("id").text() == Some(id)))
        .filter(|c| c.get("source").text() == Some("task"))
        .map(|c| text(c, "active_task_id"))
        .collect();
    if lineages.len() > 1 {
        return Err(Error::new(
            3,
            "task covers do not share one active task lineage",
        ));
    }
    Ok(obj([("covers", listed)]))
}

pub(crate) fn failure_report(
    public: &str,
    cwd: &Path,
    fingerprint: &str,
    interrupted: &AtomicBool,
) -> Result<V> {
    if !hex(fingerprint, 16) {
        return Err(fail("malformed failure fingerprint"));
    }
    let (code, out, _) = oms_command_state::run(
        "fail-ledger",
        &argv(&["show", "--fingerprint", fingerprint, "--json"]),
        cwd,
        interrupted,
    );
    if code != 0 {
        return Err(fail("failure record unavailable"));
    }
    let row = V::parse(&out)
        .map_err(|_| fail("failure record unavailable"))?
        .get("failure")
        .clone();
    if row.get("fingerprint").text() != Some(fingerprint) {
        return Err(fail("failure record unavailable"));
    }
    // Command arguments and free-form ledger text never enter the draft.
    let cmd = text(&row, "cmd");
    let verb = cmd
        .strip_prefix("oms ")
        .and_then(|rest| rest.split(char::is_whitespace).next())
        .filter(|v| {
            v.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && v.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && public.lines().any(|t| t == *v)
        })
        .map_or_else(|| "unavailable".to_owned(), |v| format!("oms {v}"));
    let (code, catalog, _) = oms_command_failure::run(&argv(&["catalog"]), cwd, interrupted);
    let catalog = if code == 0 {
        V::parse(&catalog).unwrap_or(V::Null)
    } else {
        V::Null
    };
    let failure_code = text(&row, "failure_code");
    // The catalog is one object keyed by failure code.
    let known = matches!(&catalog, V::Obj(map) if map.contains_key(failure_code.as_bytes()));
    let exit = match row.get("exit") {
        V::Num(n) if n.parse::<i64>().is_ok() => V::Num(n.clone()),
        _ => "unavailable".into(),
    };
    let count = match row.get("count") {
        V::Num(n) if n.parse::<u64>().is_ok() => V::Num(n.clone()),
        _ => return Err(fail("failure record unavailable")),
    };
    Ok(obj([
        ("fingerprint", fingerprint.into()),
        ("verb", verb.into()),
        ("exit", exit),
        (
            "failure_code",
            if known {
                failure_code.into()
            } else {
                "unknown_failure".into()
            },
        ),
        ("count", count),
    ]))
}

pub(crate) fn review_findings(repo: &Path, interrupted: &AtomicBool) -> Result<V> {
    let mut args = argv(&["findings", "list", "--json", "--repo"]);
    args.push(repo.as_os_str().to_owned());
    let (code, out, err) = oms_command_peer::run("peer-review", &args, repo, interrupted);
    if code != 0 {
        return Err(Error::new(
            code,
            first_error(&err, "review findings unavailable"),
        ));
    }
    let rows = V::parse(&out)?;
    if !matches!(rows, V::Arr(_)) {
        return Err(fail("review findings are not a list"));
    }
    Ok(rows)
}

// -------------------------------------------------------------- cleanup ----

const ALLOWED_CHILDREN: &[&str] = &[
    ".oh-my-setting-tmp",
    "worker-guard",
    "continuation.patch",
    "continuation-base",
    "graph-context.json",
    "graph-context.log",
    "worker-guard-snapshot.error",
    "worker-user-config.before",
    "worker-user-config.after",
];
const RESIDUE_KINDS: &[(&str, &str)] = &[
    ("delegate", "wt"),
    ("admit", "wt"),
    ("scratch", "wt"),
    ("land", "wt"),
    ("agy-read", "tree"),
];

struct Report {
    lines: String,
    removable: usize,
    kept: usize,
    apply: bool,
}
impl Report {
    fn remove(&mut self, path: &Path, reason: &str) {
        self.removable += 1;
        self.lines += &format!(
            "{}: {} ({reason})\n",
            if self.apply {
                "removed"
            } else {
                "would remove"
            },
            path.display()
        );
    }
    fn keep(&mut self, path: &Path, reason: &str) {
        self.kept += 1;
        self.lines += &format!("kept: {} ({reason})\n", path.display());
    }
}

/// A caller's repository-selecting Git variables must never redirect the
/// ownership and cleanliness proofs (or `worktree remove`) to another tree.
const GIT_REDIRECTS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];
fn git_command(repo: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    for key in GIT_REDIRECTS {
        command.env_remove(key);
    }
    command.arg("-C").arg(repo);
    command
}
fn git(
    repo: &Path,
    args: &[&str],
    interrupted: &AtomicBool,
) -> std::result::Result<Vec<u8>, String> {
    interrupt(interrupted).map_err(|e| e.message)?;
    // The residue worktree is untrusted: no fsmonitor hook or untracked cache.
    let out = git_command(repo)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(first_error(&out.stderr, "git failed"))
    }
}
fn git_dir(repo: &Path, query: &str, interrupted: &AtomicBool) -> Option<PathBuf> {
    let raw = git(repo, &["rev-parse", query], interrupted).ok()?;
    let raw = PathBuf::from(String::from_utf8(raw).ok()?.trim());
    std::fs::canonicalize(if raw.is_absolute() {
        raw
    } else {
        repo.join(raw)
    })
    .ok()
}
fn plain_dir(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return None;
    }
    std::fs::canonicalize(path).ok()
}
fn read_marker(dir: &Path) -> Option<(String, std::collections::BTreeMap<String, String>)> {
    let path = dir.join(".oh-my-setting-tmp");
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > 16 * 1024 {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let fields = raw
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect::<std::collections::BTreeMap<_, _>>();
    (fields.get("kind").map(String::as_str) == Some("oh-my-setting-temp")
        && fields.get("temporary").map(String::as_str) == Some("1"))
    .then_some((raw, fields))
}
/// `Ok(true)` live, `Ok(false)` proven dead, `Err` unproven.
fn owner_alive(
    fields: &std::collections::BTreeMap<String, String>,
) -> std::result::Result<bool, ()> {
    let pid = fields
        .get("pid")
        .and_then(|p| p.parse::<u32>().ok())
        .filter(|p| *p > 0)
        .ok_or(())?;
    if !oms_command_operations::lock::pid_alive(i64::from(pid)) {
        return Ok(false);
    }
    Ok(match fields.get("process-start") {
        Some(start) => oms_command_operations::lock::process_start(pid).is_none_or(|s| s == *start),
        None => true,
    })
}
/// A tracked file is proven unchanged only by its raw bytes: Git conversion
/// filters configured for the repository never run on the untrusted tree.
fn dirty_reason(worktree: &Path, interrupted: &AtomicBool) -> Option<String> {
    let unreadable = |e: String| Some(format!("status is unreadable ({e})"));
    let head = match git(
        worktree,
        &["ls-tree", "-r", "-z", "--full-tree", "HEAD"],
        interrupted,
    ) {
        Ok(v) => v,
        Err(e) => return unreadable(e),
    };
    let index = match git(worktree, &["ls-files", "-z", "--stage"], interrupted) {
        Ok(v) => v,
        Err(e) => return unreadable(e),
    };
    // ls-tree prints "mode type sha\tpath"; ls-files --stage "mode sha stage\tpath".
    let entries = |raw: &[u8], sha: usize| -> Vec<(String, String, String)> {
        raw.split(|b| *b == 0)
            .filter(|e| !e.is_empty())
            .filter_map(|e| {
                let e = String::from_utf8_lossy(e);
                let (meta, path) = e.split_once('\t')?;
                let parts: Vec<_> = meta.split(' ').collect();
                Some((
                    parts.first()?.to_string(),
                    parts.get(sha)?.to_string(),
                    path.to_owned(),
                ))
            })
            .collect()
    };
    let tree = entries(&head, 2);
    let index_entries = entries(&index, 1);
    if index_entries != tree {
        return Some("has uncommitted or untracked files".into());
    }
    let mut paths = String::new();
    for (mode, _, path) in &tree {
        if !matches!(mode.as_str(), "100644" | "100755") {
            return Some(format!("tracked {path} cannot be byte-proven"));
        }
        let full = worktree.join(path);
        match std::fs::symlink_metadata(&full) {
            Ok(m) if m.is_file() => {
                let exec = m.mode() & 0o111 != 0;
                if exec != (mode == "100755") {
                    return Some("has uncommitted or untracked files".into());
                }
            }
            _ => return Some("has uncommitted or untracked files".into()),
        }
        if path.contains('\n') {
            return Some(format!("tracked {path:?} cannot be byte-proven"));
        }
        paths += path;
        paths.push('\n');
    }
    if !paths.is_empty() {
        let mut child = match git_command(worktree)
            .args(["hash-object", "--no-filters", "--stdin-paths"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return unreadable(e.to_string()),
        };
        let mut stdin = child.stdin.take()?;
        let writer = std::thread::spawn(move || {
            use std::io::Write;
            stdin.write_all(paths.as_bytes())
        });
        let output = match child.wait_with_output() {
            Ok(o) if o.status.success() => o.stdout,
            _ => return unreadable("hash-object failed".into()),
        };
        if !writer.join().is_ok_and(|r| r.is_ok()) {
            return unreadable("hash-object input failed".into());
        }
        let hashes: Vec<_> = String::from_utf8_lossy(&output)
            .lines()
            .map(str::to_owned)
            .collect();
        if hashes.len() != tree.len() || hashes.iter().zip(&tree).any(|(h, (_, sha, _))| h != sha) {
            return Some("has uncommitted or untracked files".into());
        }
    }
    match git(
        worktree,
        &["ls-files", "-z", "-o", "--exclude-standard"],
        interrupted,
    ) {
        Ok(v) if v.is_empty() => None,
        Ok(_) => Some("has uncommitted or untracked files".into()),
        Err(e) => unreadable(e),
    }
}
fn keep_reason(
    kind: &str,
    fields: &std::collections::BTreeMap<String, String>,
    worktree: &Path,
    interrupted: &AtomicBool,
) -> Option<String> {
    if let Some(reason) = dirty_reason(worktree, interrupted) {
        return Some(reason);
    }
    let parent = worktree.parent()?;
    if kind == "delegate" {
        if fields.get("scratch").map(String::as_str) != Some("0") && parent.join("tmp").exists() {
            return Some("holds investigation scratch".into());
        }
        match git(
            worktree,
            &[
                "ls-files",
                "-z",
                "-o",
                "-i",
                "--exclude-standard",
                "--directory",
            ],
            interrupted,
        ) {
            Ok(v) if v.is_empty() => {}
            Ok(_) => return Some("holds ignored files".into()),
            Err(e) => return Some(format!("ignored files are unreadable ({e})")),
        }
    }
    let head = git(
        worktree,
        &["rev-parse", "-q", "--verify", "HEAD"],
        interrupted,
    )
    .ok()
    .map(|v| String::from_utf8_lossy(&v).trim().to_owned())
    .unwrap_or_default();
    if !head.is_empty()
        && git(
            worktree,
            &[
                "for-each-ref",
                "--count=1",
                "--contains",
                &head,
                "--format=x",
                "refs/heads",
                "refs/tags",
                "refs/remotes",
            ],
            interrupted,
        )
        .map_or(true, |v| v.is_empty())
    {
        return Some(format!(
            "HEAD {} is on no branch, tag or remote ref",
            &head[..head.len().min(12)]
        ));
    }
    let oms = worktree.join(".oms");
    if plain_dir(&oms).is_some() {
        for record in [
            "plan/tasks.json",
            "failures.jsonl",
            "landings.jsonl",
            "artifacts/index.jsonl",
        ] {
            if std::fs::metadata(oms.join(record)).is_ok_and(|m| m.len() > 0) {
                return Some(format!("holds .oms/{record}"));
            }
        }
    }
    None
}
/// Classify one dead residue directory. `Ok(Some(worktree))` is a validated
/// registered worktree, `Ok(None)` a directory without one; `Err` keeps it.
fn classify(
    repo: &Path,
    dir: &Path,
    kind: &str,
    child: &str,
    fields: &std::collections::BTreeMap<String, String>,
    interrupted: &AtomicBool,
) -> std::result::Result<Option<PathBuf>, String> {
    let suffix = dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.rsplit_once('.'))
        .map(|(_, s)| s.to_owned())
        .unwrap_or_default();
    let marked = fields
        .get("worktree")
        .map(PathBuf::from)
        .unwrap_or_default();
    let leaf = marked.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let expected = if leaf == child || leaf == format!("{child}-{suffix}") {
        dir.join(leaf)
    } else {
        return Err(
            "marker does not describe this residue; inspect it, then remove by hand".into(),
        );
    };
    let unexpected = |validated: Option<&Path>| -> Option<String> {
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if ALLOWED_CHILDREN.contains(&name.as_ref())
                || name.starts_with(".oms-panel-fixtures.")
                || validated.is_some_and(|w| w.file_name() == Some(entry.file_name().as_os_str()))
                || (name == "tmp" && validated.is_some() && kind == "delegate")
            {
                continue;
            }
            return Some(if validated.is_some() {
                "residue contains an additional or unrecognized child; inspect it, then remove by hand".into()
            } else {
                "unverifiable (repo moved, re-cloned or unregistered); inspect it, then remove by hand".into()
            });
        }
        None
    };
    if std::fs::symlink_metadata(&expected).is_err() {
        return match unexpected(None) {
            Some(reason) => Err(reason),
            None => Ok(None),
        };
    }
    let worktree = plain_dir(&expected)
        .filter(|w| *w == expected && plain_dir(&marked).as_ref() == Some(w))
        .ok_or("unsafe residue worktree path (symlink or moved)")?;
    let repo_common =
        git_dir(repo, "--git-common-dir", interrupted).ok_or("owning repository is unreadable")?;
    let common = git_dir(&worktree, "--git-common-dir", interrupted)
        .filter(|c| *c == repo_common)
        .ok_or("worktree belongs to another Git common directory")?;
    git_dir(&worktree, "--git-dir", interrupted)
        .filter(|d| d.parent() == Some(common.join("worktrees").as_path()))
        .ok_or("worktree is not a linked registration")?;
    let listing = git(repo, &["worktree", "list", "--porcelain"], interrupted)?;
    if !String::from_utf8_lossy(&listing).lines().any(|l| {
        l.strip_prefix("worktree ")
            .and_then(|p| std::fs::canonicalize(p).ok())
            .as_ref()
            == Some(&worktree)
    }) {
        return Err("worktree is not registered".into());
    }
    if worktree.starts_with(repo) || repo.starts_with(dir) {
        return Err("the owning repo is the worktree, around or inside it".into());
    }
    if let Some(reason) = keep_reason(kind, fields, &worktree, interrupted) {
        return Err(reason);
    }
    if let Some(reason) = unexpected(Some(&worktree)) {
        return Err(reason);
    }
    Ok(Some(worktree))
}
fn remove_residue(
    repo: &Path,
    dir: &Path,
    marker: &str,
    worktree: Option<&Path>,
    kind: &str,
    interrupted: &AtomicBool,
) -> std::result::Result<(), &'static str> {
    if let Some(worktree) = worktree {
        git(
            repo,
            &["worktree", "remove", "--force", &worktree.to_string_lossy()],
            interrupted,
        )
        .map_err(|_| "worktree removal failed")?;
    }
    for name in ALLOWED_CHILDREN
        .iter()
        .filter(|n| **n != ".oh-my-setting-tmp")
    {
        let path = dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
                let _ = std::fs::remove_dir_all(&path);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&path);
            }
            Err(_) => {}
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let is_fixture = name.to_string_lossy().starts_with(".oms-panel-fixtures.");
            let is_tmp = name == "tmp" && worktree.is_some() && kind == "delegate";
            if is_fixture || is_tmp {
                match entry.file_type() {
                    Ok(t) if t.is_dir() => drop(std::fs::remove_dir_all(entry.path())),
                    _ => drop(std::fs::remove_file(entry.path())),
                }
            }
        }
    }
    let _ = std::fs::remove_file(dir.join(".oh-my-setting-tmp"));
    if std::fs::remove_dir(dir).is_err() {
        // Restore the marker so a later pass still recognizes the residue.
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(".oh-my-setting-tmp"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, marker.as_bytes()));
        return Err("new entries appeared during cleanup");
    }
    Ok(())
}
fn temp_dirs(repo: &Path, report: &mut Report, interrupted: &AtomicBool) -> Result<()> {
    let tmp = std::env::var_os("TMPDIR")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let mut bases = vec![tmp];
    if let Ok(resolver) = oms_reference::repository::Resolver::from_env() {
        bases.push(resolver.managed_root);
    }
    let mut seen = std::collections::BTreeSet::new();
    let uid = users::get_current_uid();
    for base in bases {
        let Some(base) = plain_dir(&base) else {
            continue;
        };
        if !seen.insert(base.clone()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            interrupt(interrupted)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((kind, child)) = RESIDUE_KINDS
                .iter()
                .find(|(k, _)| name.starts_with(&format!("oh-my-setting-{k}.")))
            else {
                continue;
            };
            let dir = base.join(&name);
            let Ok(meta) = std::fs::symlink_metadata(&dir) else {
                continue;
            };
            if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid {
                continue;
            }
            let Some((marker, fields)) = read_marker(&dir) else {
                continue;
            };
            match owner_alive(&fields) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(()) => {
                    report.keep(&dir, "dead harness temp dir; owner pid is unproven");
                    continue;
                }
            }
            let owner = fields
                .get("repo")
                .and_then(|r| std::fs::canonicalize(r).ok());
            if owner.as_deref() != Some(repo) {
                report.keep(&dir, "harness temp dir owned by another repository");
                continue;
            }
            let classified = classify(repo, &dir, kind, child, &fields, interrupted);
            let worktree = match classified {
                Ok(w) => w,
                Err(reason) => {
                    report.keep(&dir, &format!("dead harness temp dir; worktree {reason}"));
                    continue;
                }
            };
            if !report.apply {
                report.remove(&dir, "dead harness temp dir");
                continue;
            }
            // Recheck immediately before removal: marker, owner and proof.
            let unchanged = read_marker(&dir).is_some_and(|(m, _)| m == marker)
                && owner_alive(&fields) == Ok(false)
                && classify(repo, &dir, kind, child, &fields, interrupted).ok()
                    == Some(worktree.clone());
            if !unchanged {
                report.keep(
                    &dir,
                    "dead harness temp dir; worktree changed during cleanup",
                );
                continue;
            }
            match remove_residue(repo, &dir, &marker, worktree.as_deref(), kind, interrupted) {
                Ok(()) => report.remove(&dir, "dead harness temp dir"),
                Err(reason) => report.keep(&dir, reason),
            }
        }
    }
    Ok(())
}
fn legacy_links(source: &Path, report: &mut Report) {
    let Some(home) = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
    else {
        return;
    };
    let pi = std::env::var_os("PI_CODING_AGENT_DIR")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".pi/agent"), PathBuf::from);
    let old = home.join(".oh-my-setting");
    let mut links = vec![
        (
            home.join(".codex/skills/oh-my-setting"),
            "custom-skills".to_owned(),
        ),
        (
            home.join(".claude/skills/oh-my-setting"),
            "custom-skills".to_owned(),
        ),
        (home.join(".gemini/GEMINI.md"), "AGENTS.md".to_owned()),
        (pi.join("AGENTS.md"), "AGENTS.md".to_owned()),
    ];
    let mut names: Vec<String> = std::fs::read_dir(source.join("custom-skills"))
        .map(|e| {
            e.flatten()
                .filter(|e| e.path().join("SKILL.md").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.extend(
        [
            "agent-harness",
            "oh-my-setting-ops",
            "spec-interview",
            "trace",
            "trust-boundary",
            "careful",
            "gpu-workstation",
            "slurm",
            "peer-ask",
            "peer-delegate",
            "peer-review",
        ]
        .map(str::to_owned),
    );
    names.sort();
    names.dedup();
    for root in [home.join(".agents/skills"), pi.join("skills")] {
        for name in &names {
            links.push((root.join(name), format!("custom-skills/{name}")));
        }
    }
    for (target, rel) in links {
        // Only a symbolic link pointing exactly at this install is ours.
        let Ok(meta) = std::fs::symlink_metadata(&target) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let Ok(current) = std::fs::read_link(&target) else {
            continue;
        };
        if current != source.join(&rel) && current != old.join(&rel) {
            continue;
        }
        if report.apply && std::fs::remove_file(&target).is_err() {
            report.keep(&target, "legacy oh-my-setting link could not be removed");
            continue;
        }
        report.remove(&target, "legacy oh-my-setting link");
    }
}
pub(crate) fn cleanup(
    source: &Path,
    repo: &Path,
    apply: bool,
    interrupted: &AtomicBool,
) -> Result<V> {
    if apply && harness_child() {
        return Err(fail(
            "a harness child cannot mutate OMS host lifecycle authority",
        ));
    }
    let mut report = Report {
        lines: String::new(),
        removable: 0,
        kept: 0,
        apply,
    };
    legacy_links(source, &mut report);
    if let Ok(repo) = std::fs::canonicalize(repo)
        && git(&repo, &["rev-parse", "--git-dir"], interrupted).is_ok()
    {
        temp_dirs(&repo, &mut report, interrupted)?;
    }
    report.lines += &if apply {
        format!(
            "\ncleanup: removed {} item(s), {} kept with work\n",
            report.removable, report.kept
        )
    } else {
        format!(
            "\ncleanup: {} removable item(s) found, {} kept with work\n",
            report.removable, report.kept
        )
    };
    Ok(obj([
        ("text", report.lines.into()),
        ("removable", report.removable.into()),
        ("kept", report.kept.into()),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services_control::ControlServices;
    use oms_command_plan::{Action, Request};
    use std::process::Command;
    use std::sync::Arc;

    const CASE: &str = "OMS_LANDING_UT_CASE";
    const ROOT: &str = "OMS_LANDING_UT_ROOT";

    /// Plan transitions read harness identity from the environment, so each
    /// case runs in a re-executed test process with a cleared environment.
    fn isolated(case: &str) {
        let root =
            std::env::temp_dir().join(format!("oms-landing-ut-{}-{case}", std::process::id()));
        std::fs::create_dir_all(root.join("home")).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        std::fs::write(
            root.join("gitconfig"),
            "[user]\n\tname = t\n\temail = t@example.invalid\n",
        )
        .unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "services_landing::tests::isolated_entry",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root.join("home"))
            .env("TMPDIR", &root)
            .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("OMS_HARNESS_CHILD", "0")
            .env("OMS_HARNESS_DELEGATE_DEPTH", "0")
            .env(CASE, case)
            .env(ROOT, &root)
            .output()
            .unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "{case}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    struct Fixture {
        repo: PathBuf,
        raw: String,
        patch_sha: String,
        base: String,
        frozen: PathBuf,
        admission: V,
        policy: oms_command_landing::admission::Policy,
    }
    fn git_ok(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
    fn plan(control: &ControlServices<'_>, f: &Fixture, request: Request) -> Result<Vec<u8>> {
        plan_run(control, &f.repo, &f.repo, request, &AtomicBool::new(false))
    }
    /// A committed repository whose plan task `t1` is in review under a lease.
    fn reviewed(control: &ControlServices<'_>, root: &Path, review: bool) -> Fixture {
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_ok(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git_ok(&repo, &["add", "a.txt"]);
        git_ok(&repo, &["commit", "-qm", "init"]);
        std::fs::write(repo.join(".git/info/exclude"), ".oms/\n").unwrap();
        let raw = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n a\n+b\n";
        std::fs::create_dir_all(repo.join(".oms/p")).unwrap();
        std::fs::write(repo.join(".oms/p/p.patch"), raw).unwrap();
        std::fs::write(repo.join(".oms/p/r.md"), "report\n").unwrap();
        std::fs::create_dir_all(repo.join(".oms/landing-patches")).unwrap();
        let frozen = repo.join(".oms/landing-patches/land-ut.patch");
        std::fs::write(&frozen, raw).unwrap();
        let patch_sha = oms_command_plan::json::sha(raw);
        let base = git_ok(&repo, &["rev-parse", "HEAD"]);
        let f = Fixture {
            admission: obj([
                ("kind", "patch-admit".into()),
                ("exit", 0usize.into()),
                ("patch_sha256", patch_sha.as_str().into()),
                ("base", base.as_str().into()),
            ]),
            policy: oms_command_landing::admission::Policy {
                task: "t1".into(),
                ..Default::default()
            },
            repo,
            raw: raw.to_owned(),
            patch_sha,
            base,
            frozen,
        };
        plan(control, &f, Request::new(Action::Init)).unwrap();
        plan(
            control,
            &f,
            Request::new(Action::Add)
                .with("id", "t1")
                .with("title", "T")
                .with("allowed", "a.txt")
                .with("verify", "true"),
        )
        .unwrap();
        plan(
            control,
            &f,
            Request::new(Action::Claim)
                .with("id", "t1")
                .with("provider", "codex"),
        )
        .unwrap();
        if review {
            let lease = state(control, &f).text("lease_id").to_owned();
            plan(
                control,
                &f,
                Request::new(Action::Review)
                    .with("id", "t1")
                    .with("lease-id", lease)
                    .with("artifact", ".oms/p/r.md")
                    .with("patch", ".oms/p/p.patch"),
            )
            .unwrap();
        }
        f
    }
    fn public_plan(control: &ControlServices<'_>, repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let (code, out, err) = oms_command_plan::run_with_services(
            &argv(args),
            repo,
            &AtomicBool::new(false),
            control,
        );
        if code == 0 {
            Ok(out)
        } else {
            Err(fail(String::from_utf8_lossy(&err)))
        }
    }
    fn request<'a>(f: &'a Fixture) -> LandingRequest<'a> {
        LandingRequest {
            state_repo: &f.repo,
            execution_repo: &f.repo,
            patch_sha256: &f.patch_sha,
            base: &f.base,
            task: &f.policy.task,
            admission: &f.admission,
            policy: &f.policy,
            frozen_patch: &f.frozen,
            patch: f.raw.as_bytes(),
        }
    }
    fn state(control: &ControlServices<'_>, f: &Fixture) -> PlanValue {
        task_snapshot(control, &request(f), &AtomicBool::new(false)).unwrap()
    }
    /// The intent `patch-land` records after a successful review.
    fn intent(control: &ControlServices<'_>, f: &Fixture) -> V {
        let review = landing_review(control, &request(f), &AtomicBool::new(false)).unwrap();
        obj([
            ("patch", f.frozen.to_string_lossy().into_owned().into()),
            ("lease", state(control, f).text("lease_id").into()),
            ("plan_receipt_sha", review.get("plan_receipt_sha").clone()),
            (
                "plan_done_receipt_sha",
                review.get("plan_done_receipt_sha").clone(),
            ),
            ("approval", "".into()),
            ("approval_version", "".into()),
        ])
    }

    fn run_case(case: &str, root: &Path) {
        let stop = Arc::new(AtomicBool::new(false));
        let control = ControlServices::new(&stop, root).unwrap();
        if case == "publication-review" {
            public_plan(
                &control,
                &root.join("repo"),
                &[
                    "review",
                    "--id",
                    "t1",
                    "--lease-id",
                    &std::env::var("OMS_LANDING_UT_LEASE").unwrap(),
                    "--artifact",
                    ".oms/p/r.md",
                    "--patch",
                    ".oms/p/p.patch",
                ],
            )
            .unwrap();
            return;
        }
        let f = reviewed(&control, root, case != "publication-product");
        if case == "publication-product" {
            use oms_command_plan::services::PublicationProduct;
            use std::os::unix::fs::PermissionsExt;
            git_ok(
                &f.repo,
                &[
                    "apply",
                    "--index",
                    f.repo.join(".oms/p/p.patch").to_str().unwrap(),
                ],
            );
            git_ok(&f.repo, &["commit", "-qm", "accepted product"]);
            let commit = git_ok(&f.repo, &["rev-parse", "HEAD"]);
            let tree = git_ok(&f.repo, &["rev-parse", "HEAD^{tree}"]);
            let all = oms_command_plan::json::parse(
                &std::fs::read(f.repo.join(".oms/plan/tasks.json")).unwrap(),
            )
            .unwrap();
            let task = all.get("tasks").get("t1");
            let lease = task.text("lease_id").to_owned();
            let record = serde_json::json!({"schema":1,"task_id":"t1","plan_id":all.text("plan_id"),"lease_id":lease,
                "candidate_event_id":"evt-native","candidate_attempt_id":"attempt-native","patch_sha256":f.patch_sha,
                "artifact":".oms/p/r.md","patch":".oms/p/p.patch","commit":commit,"tree":tree,"base":f.base,
                "review_gate":{"event_id":"review-native","patch_sha256":f.patch_sha,"exit":0,"gate_verify_exit":0,"seat_verdicts":["pass"]}});
            let review = |record: &serde_json::Value| {
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "services_landing::tests::isolated_entry",
                        "--exact",
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(CASE, "publication-review")
                    .env("OMS_LANDING_UT_LEASE", &lease)
                    .env(
                        "OMS_PANEL_ACCEPT_RECORD_JSON",
                        serde_json::to_string(record).unwrap(),
                    )
                    .output()
                    .unwrap()
            };
            for (key, value) in [
                ("task_id", "other".to_owned()),
                ("lease_id", format!("lease_{}", "0".repeat(32))),
                ("tree", "0".repeat(40)),
                ("commit", "0".repeat(40)),
                ("patch_sha256", "0".repeat(64)),
            ] {
                let mut bad = record.clone();
                bad[key] = value.into();
                assert!(!review(&bad).status.success(), "forged acceptance {key}");
                assert_eq!(state(&control, &f).text("state"), "claimed");
            }
            let out = review(&record);
            assert!(
                out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let finish = || {
                public_plan(
                    &control,
                    &f.repo,
                    &[
                        "finish",
                        "--id",
                        "t1",
                        "--lease-id",
                        &lease,
                        "--landed-commit",
                        &commit,
                    ],
                )
            };
            assert!(
                finish().is_err(),
                "missing publication cannot complete the task"
            );
            assert_eq!(state(&control, &f).text("state"), "review");
            let mut checksum = Command::new("cksum")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            std::io::Write::write_all(
                checksum.stdin.take().as_mut().unwrap(),
                f.repo.as_os_str().as_encoded_bytes(),
            )
            .unwrap();
            let checksum = checksum.wait_with_output().unwrap();
            assert!(checksum.status.success());
            let sum = String::from_utf8(checksum.stdout).unwrap();
            let mut words = sum.split_whitespace();
            let receipt_dir = root
                .join("home/.local/state/oh-my-setting/land")
                .join(format!(
                    "repo-{}-{}",
                    words.next().unwrap(),
                    words.next().unwrap()
                ));
            std::fs::create_dir_all(&receipt_dir).unwrap();
            std::fs::set_permissions(&receipt_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let receipt_path = receipt_dir.join(format!("{commit}-native.json"));
            let receipt = serde_json::to_vec(&serde_json::json!({"schema":1,"sha":commit,"state":"passed","remote":"fixture","target":"main","gate":{"rc":0},"push":{"rc":0,"moved":1,"base":f.base},"ci":{"conclusion":"success"}})).unwrap();
            std::fs::write(&receipt_path, &receipt).unwrap();
            std::fs::set_permissions(&receipt_path, std::fs::Permissions::from_mode(0o600))
                .unwrap();
            assert!(
                finish().is_err(),
                "receipt without actual pushed target is insufficient"
            );
            git_ok(
                &f.repo,
                &["update-ref", "refs/remotes/fixture/main", &commit],
            );
            let value =
                oms_command_plan::json::parse(&serde_json::to_vec(&record).unwrap()).unwrap();
            let product = |record: &oms_command_plan::json::Value| -> oms_command_plan::Result<PublicationProduct> {
                oms_command_plan::Services::publication_product(&control, &f.repo, &commit, record, f.raw.as_bytes())
            };
            for (key, text) in [
                ("tree", "0".repeat(40)),
                ("base", commit.clone()),
                ("commit", "0".repeat(40)),
                ("patch_sha256", "0".repeat(64)),
            ] {
                let mut bad = value.clone();
                bad.set(key, oms_command_plan::json::string(text)).unwrap();
                assert!(
                    product(&bad).is_err(),
                    "native content proof must refuse {key}"
                );
            }
            finish().unwrap();
            let captured = product(&value).unwrap();
            assert_eq!(
                (
                    captured.commit,
                    captured.accepted_tree,
                    captured.base,
                    captured.patch_sha256,
                    captured.raw_proof
                ),
                (
                    commit.clone(),
                    tree,
                    f.base.clone(),
                    f.patch_sha.clone(),
                    receipt.clone()
                )
            );
            let completed = state(&control, &f);
            assert_eq!(completed.text("state"), "done");
            assert_eq!(completed.get("landing").text("sha"), commit);
            assert_eq!(
                completed.get("landing").text("receipt_sha256"),
                oms_command_plan::json::sha(&receipt)
            );
            let before = std::fs::read(f.repo.join(".oms/plan/tasks.json")).unwrap();
            let base_tree = git_ok(&f.repo, &["rev-parse", &format!("{}^{{tree}}", f.base)]);
            let other_parent = git_ok(
                &f.repo,
                &[
                    "commit-tree",
                    &base_tree,
                    "-p",
                    &f.base,
                    "-m",
                    "other parent",
                ],
            );
            let merge = git_ok(
                &f.repo,
                &[
                    "commit-tree",
                    value.text("tree"),
                    "-p",
                    &f.base,
                    "-p",
                    &other_parent,
                    "-m",
                    "not the accepted parent",
                ],
            );
            let mut wrong_parent = value.clone();
            wrong_parent
                .set("commit", oms_command_plan::json::string(merge))
                .unwrap();
            assert!(
                product(&wrong_parent)
                    .err()
                    .unwrap()
                    .message
                    .contains("tree/base"),
                "native proof must reject extra accepted parents"
            );
            assert!(
                oms_command_plan::Services::publication_product(
                    &control,
                    &f.repo,
                    &commit,
                    &value,
                    b"changed patch"
                )
                .is_err()
            );
            let other_repo = root.join("other-repo");
            std::fs::create_dir(&other_repo).unwrap();
            git_ok(&other_repo, &["init", "-q"]);
            assert!(
                oms_command_plan::Services::publication_product(
                    &control,
                    &other_repo,
                    &commit,
                    &value,
                    f.raw.as_bytes()
                )
                .is_err(),
                "another repository cannot reuse publication evidence"
            );
            stop.store(true, std::sync::atomic::Ordering::Release);
            assert_eq!(product(&value).err().unwrap().code, 130);
            stop.store(false, std::sync::atomic::Ordering::Release);
            finish().unwrap();
            assert_eq!(
                std::fs::read(f.repo.join(".oms/plan/tasks.json")).unwrap(),
                before
            );
            let mut changed: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
            changed["gate"]["rc"] = 1.into();
            std::fs::write(&receipt_path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(
                finish().is_err(),
                "changed publication evidence must refuse reuse"
            );
            assert_eq!(
                std::fs::read(f.repo.join(".oms/plan/tasks.json")).unwrap(),
                before
            );
            return;
        }
        let off = AtomicBool::new(false);
        let req = request(&f);
        let intent = intent(&control, &f);
        match case {
            "complete" => {
                fence_landing(&control, &req, &intent, None, &off).unwrap();
                assert_eq!(state(&control, &f).text("state"), "landing");
                complete_landing(&control, &req, &intent, &off).unwrap();
                let done = state(&control, &f);
                assert_eq!(done.text("state"), "done");
                assert_eq!(
                    oms_command_plan::json::receipt_digest(&done).unwrap(),
                    text(&intent, "plan_done_receipt_sha"),
                    "predicted done receipt must equal the finished task"
                );
                // Converged receipts are idempotent; unapplied never reopens done.
                recover_landing(&control, &req, &intent, true, &off).unwrap();
                recover_landing(&control, &req, &intent, false, &off).unwrap();
                assert_eq!(state(&control, &f).text("state"), "done");
            }
            "recover-applied" => {
                fence_landing(&control, &req, &intent, None, &off).unwrap();
                let mut drifted = intent.clone();
                drifted.set("plan_receipt_sha", "0".repeat(64));
                let error = recover_landing(&control, &req, &drifted, true, &off).unwrap_err();
                assert!(
                    error.message.contains("did not converge"),
                    "{}",
                    error.message
                );
                assert_eq!(state(&control, &f).text("state"), "landing");
                recover_landing(&control, &req, &intent, true, &off).unwrap();
                assert_eq!(state(&control, &f).text("state"), "done");
                recover_landing(&control, &req, &intent, true, &off).unwrap();
            }
            "recover-unapplied" => {
                // Never fenced: a review the intent did not move is left alone.
                recover_landing(&control, &req, &intent, false, &off).unwrap();
                assert_eq!(state(&control, &f).text("state"), "review");
                fence_landing(&control, &req, &intent, None, &off).unwrap();
                let mut other = intent.clone();
                other.set("lease", "lease_".to_owned() + &"0".repeat(32));
                recover_landing(&control, &req, &other, false, &off).unwrap();
                assert_eq!(state(&control, &f).text("state"), "landing");
                recover_landing(&control, &req, &intent, false, &off).unwrap();
                let released = state(&control, &f);
                assert_eq!(released.text("state"), "ready");
                assert_eq!(released.text("lease_id"), "");
                recover_landing(&control, &req, &intent, false, &off).unwrap();
            }
            "fence-refuses" => {
                let mut stale = intent.clone();
                stale.set("plan_receipt_sha", "0".repeat(64));
                assert!(fence_landing(&control, &req, &stale, None, &off).is_err());
                let mut lease = intent.clone();
                lease.set("lease", "lease_".to_owned() + &"0".repeat(32));
                assert!(fence_landing(&control, &req, &lease, None, &off).is_err());
                let mut approval = intent.clone();
                approval.set("approval", "apr_0");
                approval.set("approval_version", "3");
                let error = fence_landing(&control, &req, &approval, None, &off).unwrap_err();
                assert!(error.message.contains("reservation"), "{}", error.message);
                assert_eq!(state(&control, &f).text("state"), "review");
                // A fenced task no longer passes review.
                fence_landing(&control, &req, &intent, None, &off).unwrap();
                assert!(landing_review(&control, &req, &off).is_err());
            }
            "review-refuses" => {
                for (key, value) in [
                    ("patch_sha256", "0".repeat(64)),
                    ("base", "0".repeat(40)),
                    ("exit", "1".into()),
                ] {
                    let mut admission = f.admission.clone();
                    admission.set(key, value.as_str());
                    let req = LandingRequest {
                        admission: &admission,
                        ..request(&f)
                    };
                    assert!(landing_review(&control, &req, &off).is_err(), "{key}");
                }
                let req = LandingRequest {
                    frozen_patch: Path::new(""),
                    ..request(&f)
                };
                assert!(landing_review(&control, &req, &off).is_err());
                let query = Request::new(Action::EvidenceSnapshot).with("id", "t1");
                let plan = landing_plan(&control, &f.repo, &f.repo, &query, &off).unwrap();
                let shown =
                    oms_command_plan::json::parse(&plan.execute(&query).unwrap().stdout).unwrap();
                let mut policy = f.policy.clone();
                policy.expected_plan_review =
                    Some(plan.landing_review_fence(&shown, &f.base).unwrap());
                let mut admission = f.admission.clone();
                let mut check = |kind: &str, exit: V| {
                    admission.set("verification", kind);
                    admission.set("verify_exit", exit);
                    landing_review(
                        &control,
                        &LandingRequest {
                            admission: &admission,
                            policy: &policy,
                            ..request(&f)
                        },
                        &off,
                    )
                };
                assert!(check("skipped", V::Null).is_err());
                assert!(check("command", 1usize.into()).is_err());
                assert!(check("command", "0".into()).is_err());
                check("command", 0usize.into()).unwrap();
                let plain = oms_command_landing::admission::Policy::default();
                let req = LandingRequest {
                    task: "",
                    policy: &plain,
                    ..request(&f)
                };
                let review = landing_review(&control, &req, &off).unwrap();
                assert_eq!(review.get("plan_receipt_sha").text(), Some(""));
            }
            "context" => {
                let task = state(&control, &f);
                let row = V::parse(&oms_command_plan::json::encoded(&task).unwrap()).unwrap();
                let allowed = oms_reference::binding::Context::effective_allowed_paths(
                    &AdmissionContext,
                    &f.repo,
                    &row,
                    &off,
                )
                .unwrap();
                assert_eq!(allowed, ["a.txt"]);
                std::fs::write(
                    f.repo.join(".oms/plan/progress.jsonl"),
                    "{\"kind\":\"goal-batch\",\"tasks\":[\"t1\"]}\n",
                )
                .unwrap();
                let context: &dyn oms_reference::binding::Context = &AdmissionContext;
                assert!(
                    context
                        .goal_batch_prefix(&f.repo, &f.repo, "t1", &off)
                        .is_err()
                );
                assert!(
                    context
                        .goal_batch_prefix(&f.repo, &f.repo, "t2", &off)
                        .is_ok()
                );
                std::fs::write(f.repo.join(".oms/plan/progress.jsonl"), "{bad\n").unwrap();
                assert!(
                    context
                        .goal_batch_prefix(&f.repo, &f.repo, "t2", &off)
                        .is_err()
                );
                assert!(
                    context
                        .accepted_prefix(&f.repo, &f.repo, &[], &off)
                        .unwrap()
                        .is_empty()
                );
            }
            "salvage" => {
                let payload =
                    oms_command_plan::json::sha(oms_panel_core::salvage::payload(f.raw.as_bytes()));
                let dir = f.repo.join(".oms/salvage-provenance");
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join(format!("{payload}.json"));
                let record = |warnings: &str| {
                    format!(
                        "{{\"schema\":1,\"payload_sha256\":\"{payload}\",\"records\":[{{\"salvaged_from\":{{}},\"encoding_warnings\":[{warnings}]}}]}}"
                    )
                };
                let review = |salvaged: bool, loss: bool| {
                    let policy = oms_command_landing::admission::Policy {
                        task: "t1".into(),
                        accept_salvaged: salvaged,
                        accept_encoding_loss: loss,
                        ..Default::default()
                    };
                    let req = LandingRequest {
                        policy: &policy,
                        ..request(&f)
                    };
                    landing_review(&control, &req, &off).map_err(|e| e.message)
                };
                std::fs::write(&path, record("")).unwrap();
                assert!(
                    review(false, false)
                        .unwrap_err()
                        .contains("--accept-salvaged")
                );
                review(true, false).unwrap();
                std::fs::write(&path, record("{}")).unwrap();
                assert!(
                    review(true, false)
                        .unwrap_err()
                        .contains("--accept-encoding-loss")
                );
                review(true, true).unwrap();
                for bad in [
                    "{}".to_owned(),
                    "{bad".to_owned(),
                    record("").replace("\"schema\":1", "\"schema\":2"),
                    format!("{{\"schema\":1,\"payload_sha256\":\"{payload}\",\"records\":[]}}"),
                ] {
                    std::fs::write(&path, bad).unwrap();
                    assert!(review(true, true).unwrap_err().contains("invalid"));
                }
                std::fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink(f.repo.join(".oms/p/r.md"), &path).unwrap();
                assert!(review(true, true).unwrap_err().contains("invalid"));
                std::fs::remove_file(&path).unwrap();
                review(false, false).unwrap();
                let req = LandingRequest {
                    patch: b"other",
                    ..request(&f)
                };
                assert!(landing_review(&control, &req, &off).is_err());
            }
            other => panic!("unknown case {other}"),
        }
    }

    #[test]
    #[ignore = "re-executed by the isolated landing cases"]
    fn isolated_entry() {
        let (Some(case), Some(root)) = (std::env::var_os(CASE), std::env::var_os(ROOT)) else {
            return;
        };
        run_case(&case.to_string_lossy(), Path::new(&root));
    }
    #[test]
    fn task_landing_finishes_with_the_predicted_done_receipt() {
        isolated("complete");
    }
    #[test]
    fn task_recover_applied_converges_and_keeps_drifted_intents() {
        isolated("recover-applied");
    }
    #[test]
    fn task_recover_unapplied_releases_only_the_fenced_claim() {
        isolated("recover-unapplied");
    }
    #[test]
    fn task_fence_refuses_stale_lease_receipt_or_reservation() {
        isolated("fence-refuses");
    }
    #[test]
    fn landing_review_binds_admission_base_and_frozen_patch() {
        isolated("review-refuses");
    }
    #[test]
    fn landing_review_requires_salvage_and_encoding_consent() {
        isolated("salvage");
    }
    #[test]
    fn admission_context_scopes_allowed_paths_and_fails_closed() {
        isolated("context");
    }
    #[test]
    fn panel_publication_product_finishes_through_the_native_plan_entry() {
        isolated("publication-product");
    }
}
