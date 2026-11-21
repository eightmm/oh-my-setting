//! Durable intent -> apply -> lineage -> completion. Recovery applies no new
//! patch and leaves ambiguous or changed inputs outstanding for their owner.
use crate::{
    Context, Error, Output, Result, V,
    admission::{self, Policy},
    cli::Options,
    digest,
    git::Git,
    now, obj, parent, require,
    services::LandingRequest,
    storage, string,
};
use oms_reference::capture::CaptureIndex;
use std::path::Path;
const LOG: &str = ".oms/landings.jsonl";
fn append(repo: &Path, row: &V, _interrupted: &std::sync::atomic::AtomicBool) -> Result<()> {
    storage::append_under_lock(repo, Path::new(LOG), row)
}
pub fn receipt_digest(row: &V) -> Result<String> {
    let keys = [
        "schema",
        "landing_id",
        "patch",
        "patch_sha",
        "base_sha",
        "task",
        "lease",
        "plan_receipt_sha",
        "plan_done_receipt_sha",
        "approval",
        "approval_version",
    ];
    let mut canonical = V::Obj(std::collections::BTreeMap::new());
    for k in keys {
        canonical.set(k, row.get(k).clone());
    }
    Ok(digest(&canonical.json(false)?))
}
fn terminal(intent: &V, event: &str, reason: &str) -> V {
    let mut row = intent.clone();
    row.set("event", event);
    row.set("ts", now());
    if !reason.is_empty() {
        row.set("reason", reason);
    }
    row
}
pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    parent()?;
    require(o.words.is_empty(), "unexpected patch-land argument")?;
    let repo = c.repo(o)?;
    let state = c.state(&repo)?;
    storage::ensure_ignore(&state, c.interrupted)?;
    let _publication = c.services.publication_guard(&state, c.interrupted)?;
    // The capture producer owns the landing writer lock while allocating or
    // publishing. This outer transaction uses a distinct execution fence.
    let _execution = storage::lock(&state.join(".oms/patch-land-execution"), c.interrupted)?;
    if o.has("recover") {
        return recover(c, &repo, &state, o.get("plan-task"));
    }
    let git = Git::new(&repo, c.interrupted);
    git.safe()?;
    git.clean()?;
    let before = git.snapshot()?;
    let plan = if o.has("plan-task") {
        Some(admission::task(c, &repo, o.get("plan-task"))?)
    } else {
        None
    };
    let source = o
        .path("patch", c.cwd)
        .or_else(|| {
            plan.as_ref()
                .and_then(|p| p.get("patch").text())
                .filter(|s| !s.is_empty())
                .map(|s| state.join(s))
        })
        .ok_or_else(|| {
            Error::new(
                2,
                "--patch is required unless the plan task has a stored patch",
            )
        })?;
    let raw = admission::capture_bytes(&CaptureIndex::load(&state)?, &source)?;
    let sha = digest(&raw);
    if let Some(plan) = &plan {
        require(
            string(plan, "state") == "review",
            "plan task must be in review",
        )?;
        require(plan.get("patch").truth(), "plan task has no reviewed patch")?;
        let reviewed = admission::capture_bytes(
            &CaptureIndex::load(&state)?,
            &state.join(string(plan, "patch")),
        )?;
        require(
            reviewed == raw,
            "patch bytes differ from the plan task review",
        )?;
    }
    let policy = Policy::options(o);
    let prelim = LandingRequest {
        state_repo: &state,
        execution_repo: &repo,
        patch_sha256: &sha,
        base: &before.head,
        task: &policy.task,
        admission: &V::Null,
        policy: &policy,
        frozen_patch: Path::new(""),
        patch: &raw,
    };
    let panel_accept = o
        .has("panel-accept")
        .then(|| V::parse(o.get("panel-accept").as_bytes()))
        .transpose()?;
    if let Some(fence) = &panel_accept {
        c.services.panel_accept(&prelim, fence)?;
    }
    let approval_key = concat!("approval-", "to", "ken");
    let grants = [
        o.has("approval"),
        o.has(approval_key),
        o.has("approval-version"),
    ];
    require(
        grants.iter().all(|v| *v) || grants.iter().all(|v| !*v),
        "all approval grant fields must be supplied together",
    )?;
    if o.has("approval-version") {
        require(
            o.get("approval-version")
                .parse::<u64>()
                .is_ok_and(|n| n > 0),
            "--approval-version must be positive",
        )?;
    }
    if o.has("request-approval") {
        let result = c.services.landing_approval(&prelim, "", "", "", true)?;
        return Output::json(&result);
    }
    if o.has("approval") {
        let approvals = oms_command_operations::load_approvals(&state)?;
        let approved = approvals
            .iter()
            .find(|r| r.get("approval_id").text() == Some(o.get("approval")))
            .ok_or_else(|| Error::new(2, "unknown landing approval"))?;
        require(
            oms_command_operations::approval_effective_state(approved, None)? == "approved"
                && approved.get("version").py() == o.get("approval-version")
                && approved.get("action").text() == Some("patch-land")
                && string(approved, "object_id") == format!("patch:{sha}")
                && approved.get("patch_sha").text() == Some(&sha)
                && approved.get("base_sha").text() == Some(&before.head)
                && approved.get("task_id").text().unwrap_or("") == policy.task,
            "approval does not match this exact landing action",
        )?;
        let expected_lease = plan
            .as_ref()
            .map(|p| string(p, "lease_id"))
            .unwrap_or_default();
        require(
            approved.get("lease_id").text().unwrap_or("") == expected_lease,
            "approval belongs to another task lease",
        )?;
        if let Some(attempt) = approved.get("attempt_id").text().filter(|v| !v.is_empty()) {
            require(
                std::env::var("OMS_ATTEMPT_ID").ok().as_deref() == Some(attempt),
                "approval belongs to another native attempt",
            )?;
        }
        c.services.landing_approval(
            &prelim,
            o.get("approval"),
            o.get(approval_key),
            o.get("approval-version"),
            false,
        )?;
    } else {
        // Isolated and remote profiles always land through an approval.
        let profile = std::env::var("OMS_EXECUTION_PROFILE").unwrap_or_default();
        require(
            ["", "trusted-local", "isolated", "remote"].contains(&profile.as_str()),
            format!("invalid OMS_EXECUTION_PROFILE: {profile}"),
        )?;
        require(
            !std::env::var("OMS_REQUIRE_LANDING_APPROVAL")
                .is_ok_and(|v| ["1", "true", "yes", "on"].contains(&v.as_str()))
                && !["isolated", "remote"].contains(&profile.as_str()),
            "this execution policy requires an approved exact landing request",
        )?;
    }
    let landing = format!(
        "land-{}-{}",
        now().replace(['-', ':'], ""),
        oms_command_plan::storage::random_hex(8)?
    );
    let captured = oms_reference::producer::Producer::open(&state, c.interrupted)?
        .allocate(&source, &landing, &sha, &repo, c.interrupted)
        .map_err(|e| Error::new(e.exit_code(), format!("{e:?}")))?;
    require(git.snapshot()? == before, "tree moved before admission")?;
    git.clean()?;
    let admitted = admission::admit(c, &repo, &captured, None, &policy, false)?;
    if !admitted.verdict {
        return Ok(Output {
            code: 1,
            stdout: vec![],
            stderr: format!(
                "patch-land: rejected; not applied ({})\n",
                admitted.report.display()
            )
            .into_bytes(),
        });
    }
    require(
        admitted.bytes == raw && admitted.snapshot == before,
        "frozen patch or source tree changed during admission",
    )?;
    let relative_frozen = Path::new(".oms/landing-patches").join(format!("{landing}.patch"));
    let frozen = state.join(&relative_frozen);
    let request = LandingRequest {
        state_repo: &state,
        execution_repo: &repo,
        patch_sha256: &sha,
        base: &before.head,
        task: &policy.task,
        admission: &admitted.record,
        policy: &policy,
        frozen_patch: &frozen,
        patch: &raw,
    };
    let review = c.services.landing_review(&request)?;
    oms_reference::producer::Producer::open(&state, c.interrupted)?.publish(
        &captured,
        &frozen,
        c.interrupted,
    )?;
    let canonical_landing = storage::lock(&state.join(LOG), c.interrupted)?;
    require(
        git.snapshot()? == before,
        "tree moved before the canonical landing transaction",
    )?;
    git.clean()?;
    let mut intent = obj([
        ("schema", 1usize.into()),
        ("ts", now().into()),
        ("landing_id", landing.clone().into()),
        ("event", "intent".into()),
        ("patch", frozen.to_string_lossy().into_owned().into()),
        ("patch_sha", sha.clone().into()),
        ("base_sha", before.head.clone().into()),
        ("task", policy.task.clone().into()),
        (
            "lease",
            plan.as_ref()
                .map(|p| p.get("lease_id").clone())
                .unwrap_or_else(|| "".into()),
        ),
        (
            "plan_receipt_sha",
            review.get("plan_receipt_sha").clone().or(""),
        ),
        (
            "plan_done_receipt_sha",
            review.get("plan_done_receipt_sha").clone().or(""),
        ),
        ("approval", o.get("approval").into()),
        (
            "approval_version",
            if o.has("approval-version") {
                oms_command_operations::value::add(o.get("approval-version"), "1").into()
            } else {
                "".into()
            },
        ),
        ("receipt_sha", "".into()),
        ("worktree", repo.to_string_lossy().into_owned().into()),
    ]);
    if let Some(plan) = &plan {
        intent.set("plan_id", plan.get("plan_id").clone());
    }
    // Outside the receipt hash: recovery replays the finish with the consent.
    if policy.allow_verifier_change {
        intent.set("verifier_change_consent", true);
    }
    intent.set("receipt_sha", receipt_digest(&intent)?);
    append(&state, &intent, c.interrupted)?;
    require(
        git.snapshot()? == before,
        "HEAD, tracked tree or index changed after admission; intent left outstanding",
    )?;
    git.clean()?;
    let fresh = admission::capture_bytes(&CaptureIndex::load(&state)?, &captured)?;
    require(
        fresh == raw,
        "frozen patch changed after admission; intent left outstanding",
    )?;
    c.services.fence_landing(&request, &intent)?;
    let mut composition_binding = None;
    if let Some(fence) = &panel_accept {
        c.services.panel_accept(&request, fence)?;
        let snapshot = fence.get("snapshot");
        let manifest = snapshot.get("composition");
        if manifest.truth() {
            let manifest_sha = string(snapshot.get("candidate"), "artifact_sha256");
            let participant = string(snapshot.get("task"), "claimed_by_participant");
            require(
                oms_command_plan::json::hex(&manifest_sha, 64)
                    && !participant.is_empty()
                    && !policy.verify.is_empty()
                    && admitted.record.get("verify_exit").py() == "0"
                    && string(&admitted.record, "verification") == "command",
                "composed landing lacks successful native integration verification",
            )?;
            composition_binding = Some(obj([
                ("manifest_sha256", manifest_sha.into()),
                ("participant", participant.into()),
                ("tree", manifest.get("tree").clone()),
                ("verify_sha256", digest(policy.verify.as_bytes()).into()),
                (
                    "admission_event_id",
                    admitted.record.get("event_id").clone(),
                ),
            ]));
        }
    }
    require(
        git.snapshot()? == before,
        "tree moved during final authority fence",
    )?;
    git.clean()?;
    git.apply(&raw, true, false, false)?;
    git.apply(&raw, false, false, false)?;
    let mut lineage = obj([
        ("schema", 1usize.into()),
        (
            "event_id",
            format!("art_{}", oms_command_plan::storage::random_hex(16)?).into(),
        ),
        ("ts", now().into()),
        ("kind", "patch-land".into()),
        ("exit", 0usize.into()),
        ("landing_id", landing.clone().into()),
        ("artifact", "".into()),
        (
            "patch",
            relative_frozen.to_string_lossy().into_owned().into(),
        ),
        ("patch_sha256", sha.clone().into()),
        ("task_id", policy.task.clone().into()),
        (
            "plan_id",
            plan.as_ref()
                .map(|p| p.get("plan_id").clone())
                .unwrap_or_else(|| "".into()),
        ),
        ("base", before.head.clone().into()),
    ]);
    if let Some(binding) = composition_binding {
        lineage.set("panel_composition", binding);
    }
    storage::append(
        &state,
        Path::new(".oms/artifacts/index.jsonl"),
        &lineage,
        c.interrupted,
    )?;
    if !policy.task.is_empty() || o.has("approval") {
        c.services.complete_landing(&request, &intent)?;
    }
    append(&state, &terminal(&intent, "complete", ""), c.interrupted)?;
    drop(canonical_landing);
    let attribution = crate::attribution::record_landing(
        &state,
        &repo,
        &landing,
        &before.head,
        &raw,
        &policy.task,
        c.interrupted,
    );
    oms_reference::producer::Producer::open(&state, c.interrupted)?.seal(
        &landing,
        "complete",
        c.interrupted,
    )?;
    let mut result = Output::text(format!("patch-land: applied {}\n", source.display()));
    if attribution.is_err() {
        result.stderr =
            b"warning: attribution: landing observation could not be recorded\n".to_vec();
    }
    Ok(result)
}
fn recover(c: &Context<'_>, repo: &Path, state: &Path, task: &str) -> Result<Output> {
    let rows = storage::rows(state, Path::new(LOG))?;
    let mut outstanding = vec![];
    for row in rows.iter().filter(|r| string(r, "event") == "intent") {
        if !task.is_empty() && string(row, "task") != task {
            continue;
        }
        if !rows.iter().any(|r| {
            r.get("landing_id") == row.get("landing_id")
                && ["complete", "abandoned"].contains(&string(r, "event").as_str())
        }) {
            outstanding.push(row);
        }
    }
    let git = Git::new(repo, c.interrupted);
    git.safe()?;
    let mut out = String::new();
    let mut failed = false;
    for intent in outstanding {
        let canonical_landing = storage::lock(&state.join(LOG), c.interrupted)?;
        if storage::rows(state, Path::new(LOG))?.iter().any(|r| {
            r.get("landing_id") == intent.get("landing_id")
                && ["complete", "abandoned"].contains(&string(r, "event").as_str())
        }) {
            continue;
        }
        crate::check(c.interrupted)?;
        if intent
            .get("worktree")
            .text()
            .is_some_and(|p| Path::new(p) != repo)
        {
            continue;
        }
        let path = state.join(string(intent, "patch"));
        let bytes = match admission::capture_bytes(&CaptureIndex::load(state)?, &path) {
            Ok(b) if digest(&b) == string(intent, "patch_sha") => b,
            _ => {
                failed = true;
                out += "patch-land: changed or missing recovery patch; intent kept\n";
                continue;
            }
        };
        let applied = git.apply(&bytes, true, true, false).is_ok();
        let absent = git.apply(&bytes, true, false, false).is_ok();
        if applied == absent {
            failed = true;
            out += "patch-land: ambiguous recovery; intent kept for manual inspection\n";
            continue;
        }
        let patch_sha = string(intent, "patch_sha");
        let base = string(intent, "base_sha");
        let task = string(intent, "task");
        let request = LandingRequest {
            state_repo: state,
            execution_repo: repo,
            patch_sha256: &patch_sha,
            base: &base,
            task: &task,
            admission: &V::Null,
            policy: &Policy::default(),
            frozen_patch: &path,
            patch: &[],
        };
        if applied {
            c.services.recover_landing(&request, intent, true)?;
            append(
                state,
                &terminal(intent, "complete", "recovered"),
                c.interrupted,
            )?;
            out += "patch-land: recovered applied landing\n";
        } else {
            c.services.recover_landing(&request, intent, false)?;
            append(
                state,
                &terminal(intent, "abandoned", "not-applied"),
                c.interrupted,
            )?;
            out += "patch-land: abandoned unapplied landing\n";
        }
        drop(canonical_landing);
        oms_reference::producer::Producer::open(state, c.interrupted)?.seal(
            &string(intent, "landing_id"),
            if applied { "complete" } else { "abandoned" },
            c.interrupted,
        )?;
    }
    Ok(Output {
        code: i32::from(failed),
        stdout: out.into_bytes(),
        stderr: vec![],
    })
}
