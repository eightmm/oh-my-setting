//! Explicit Main custody transfer. Frozen task, lease, report and budget identity
//! remain unchanged; this module never accepts a result or sends provider input.
use super::*;
use oms_panel_core::native_contract::CheckedAssignment;
use oms_panel_core::native_succession::CheckedSuccession;

pub(super) fn assignment(panel: &Panel<'_>, operation: &Value) -> Result<CheckedAssignment> {
    let checked = CheckedAssignment::parse(&native_result::packet(panel, operation)?)?
        .ok_or_else(|| Error::new("native succession requires a typed assignment"))?;
    require(
        operation["tool"] == "panel-native-assignment"
            && operation["provider"] == "codex"
            && operation["attempt_id"] == checked.attempt_id()
            && operation["parent_attempt_id"] == checked.parent_attempt_id()
            && operation["task_id"] == checked.task_id()
            && operation["refs"]["plan_id"] == checked.plan_id()
            && operation["refs"]["lease_id"] == checked.lease_id()
            && operation["refs"]["panel_room_id"] == checked.room()
            && checked.room() == panel.room,
        "native assignment identity differs from its lifecycle owner",
    )?;
    Ok(checked)
}

fn prove(panel: &Panel<'_>, checked: &CheckedAssignment, previous: &str) -> Result<()> {
    let history = AttemptCache::default().attempts(panel.repo, panel.services, panel.stop)?;
    oms_panel_core::native_succession::prove_successor(
        panel.repo,
        crate::rows(&history),
        checked.parent_attempt_id(),
        previous,
        &panel.attempt,
        panel.room,
        &panel.participant,
        panel.services,
        panel.stop,
    )
}

/// Contract ownership and process custody are distinct. A saved record alone is
/// never enough: the canonical swap and absence proofs are rechecked each time.
pub(super) fn authorize(
    panel: &Panel<'_>,
    operation: &Value,
    checked: &CheckedAssignment,
) -> Result<Option<CheckedSuccession>> {
    let record = oms_panel_core::native_succession::read_bound(
        &Store::open(panel.repo)?,
        operation,
        checked,
    )?;
    if let Some(record) = &record {
        require(
            record.main_attempt_id() == panel.attempt
                && record.main_owner() == panel.owner
                && record.participant() == panel.participant
                && checked.parent_participant() == panel.participant,
            "native assignment belongs to another successor Main; inspect Results for explicit adoption",
        )?;
        prove(panel, checked, record.previous_main())?;
        if record.retirement_only() {
            require(
                native_result::settled(&native_assignment::task(panel, checked.task_id())?),
                "retirement-only succession cannot reopen native execution",
            )?;
        }
    } else {
        require(
            checked.parent_attempt_id() == panel.attempt
                && checked.parent_participant() == panel.participant
                && checked.owner() == panel.owner,
            "native assignment belongs to a previous Main; inspect Results for explicit adoption",
        )?;
    }
    Ok(record)
}

pub(super) fn claimed(panel: &Panel<'_>, task: &Value, operation: &Value) -> Result<()> {
    let checked = assignment(panel, operation)?;
    authorize(panel, operation, &checked)?;
    require(
        checked.matches_task(task)?,
        "native assignment frozen criteria changed",
    )?;
    native_assignment::claimed_by(task, checked.parent_participant(), checked.owner())
}

/// Read-only exact identity predicate, used only to select an existing attempt.
/// The selected attempt must subsequently pass authorize and fresh host custody.
pub(super) fn selected(panel: &Panel<'_>, operation: &Value) -> bool {
    let successor = text(&operation["refs"], "native_custodian_attempt");
    if successor.is_empty() {
        operation["parent_attempt_id"] == panel.attempt
    } else {
        successor == panel.attempt
    }
}

pub(super) fn custody(panel: &Panel<'_>, packet: &Value) -> Result<Value> {
    let operation = native_assignment::current_operation(panel, text(packet, "attempt_id"))?;
    let raw = serde_json::to_vec(packet).map_err(Error::new)?;
    // The exact-byte reader owns the digest; parsing this value is only for
    // legacy discrimination, never for trusting a reencoded assignment digest.
    if CheckedAssignment::parse(&raw)?.is_none() {
        require(
            operation["parent_attempt_id"] == panel.attempt
                && packet["parent_attempt_id"] == panel.attempt,
            "legacy native assignment belongs to another Main",
        )?;
        return Ok(packet.clone());
    }
    let checked = assignment(panel, &operation)?;
    let record = authorize(panel, &operation, &checked)?;
    Ok(record.map_or_else(|| packet.clone(), |record| record.custody().clone()))
}

pub(super) fn account_origin(
    panel: &Panel<'_>,
    main: &PaneIdentity,
    lead: &PaneIdentity,
) -> Result<Option<Value>> {
    let history = AttemptCache::default().attempts(panel.repo, panel.services, panel.stop)?;
    let mut origin = None;
    for operation in crate::rows(&history).iter().filter(|row| {
        row["tool"] == "panel-native-assignment"
            && row["refs"]["panel_room_id"] == panel.room
            && row["refs"]["native_custodian_attempt"] == panel.attempt
    }) {
        let checked = assignment(panel, operation)?;
        let record = authorize(panel, operation, &checked)?
            .ok_or_else(|| Error::new("native custody record missing"))?;
        let custody = record.custody();
        let pane =
            |p: &PaneIdentity| json!({"id":p.id,"window":p.window,"pid":p.pid,"birth":p.birth});
        if custody["session"] != json!(panel.session_proof)
            || custody["main_pane"] != pane(main)
            || custody["lead_pane"] != pane(lead)
            || panel
                .inherited_codex_parent
                .as_deref()
                .is_some_and(|parent| {
                    checked
                        .packet()
                        .get("lead_parent_attempt")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| checked.parent_attempt_id())
                        != parent
                })
        {
            continue;
        }
        let account = &custody["account"];
        require(
            origin.as_ref().is_none_or(|prior| prior == account),
            "native successor has conflicting retained account custody",
        )?;
        origin = Some(account.clone());
    }
    Ok(origin)
}

fn observed_custody(
    panel: &Panel<'_>,
    host: &dyn AccountHost,
    assignment: &CheckedAssignment,
    retirement: bool,
) -> Result<Value> {
    let fresh = native_result::fresh_main(panel, &AttemptCache::default())?;
    if retirement {
        let main = PaneIdentity::sample_with_host(&fresh.main, host, true)?;
        return Ok(
            json!({"main_attempt_id":fresh.attempt,"main_owner":fresh.owner,
            "participant":fresh.participant,"session":fresh.session_proof,
            "main_pane":{"id":main.id,"window":main.window,"pid":main.pid,"birth":main.birth},
            "lead_pane":assignment.packet()["lead_pane"],"account":assignment.packet()["account"],
            "account_verification":assignment.packet()["account_verification"],"checkout":assignment.checkout()}),
        );
    }
    require(
        fresh
            .inherited_codex_parent
            .as_deref()
            .unwrap_or(&fresh.attempt)
            == assignment
                .packet()
                .get("lead_parent_attempt")
                .and_then(Value::as_str)
                .unwrap_or_else(|| assignment.parent_attempt_id()),
        "native Lead parent custody changed",
    )?;
    let custody =
        fresh.codex_assignment_custody_from(host, Some(&assignment.packet()["account"]))?;
    let (_, _, verification) = binding_summary(custody.binding.as_ref());
    let pane = |p: &PaneIdentity| json!({"id":p.id,"window":p.window,"pid":p.pid,"birth":p.birth});
    Ok(
        json!({"main_attempt_id":fresh.attempt,"main_owner":fresh.owner,
        "participant":fresh.participant,"session":fresh.session_proof,
        "main_pane":pane(&custody.main),"lead_pane":pane(&custody.lead),
        "account":custody.binding,"account_verification":verification,
        "checkout":native_assignment::checkout(&fresh,&fresh.leads["codex"])?}),
    )
}

pub(super) fn adopt(
    panel: &Panel<'_>,
    options: &Options,
    host: &dyn AccountHost,
    tx: &Transaction<'_>,
) -> Result<Value> {
    adopt_before_append(panel, options, host, tx, || ())
}

pub(super) fn adopt_before_append(
    panel: &Panel<'_>,
    options: &Options,
    host: &dyn AccountHost,
    tx: &Transaction<'_>,
    before_append: impl FnOnce(),
) -> Result<Value> {
    let attempt = options.required("native-adopt")?;
    let operation = native_assignment::current_operation(panel, &attempt)?;
    let checked = assignment(panel, &operation)?;
    let task = native_assignment::task(panel, checked.task_id())?;
    require(
        checked.matches_task(&task)?
            && (operation["terminal"] == false || native_result::settled(&task))
            && (native_result::settled(&task)
                || (matches!(
                    task["state"].as_str(),
                    Some("claimed" | "running" | "review")
                ) && task["claim_expired"] == false)),
        "native succession task, lease or execution criteria changed",
    )?;
    native_result::fresh_main(panel, &AttemptCache::default())?;
    let prior = oms_panel_core::native_succession::read_bound(
        &Store::open(panel.repo)?,
        &operation,
        &checked,
    )?;
    let requested = options.required("assignment-sha256")?;
    if let Some(prior) = &prior
        && prior.main_attempt_id() == panel.attempt
    {
        require(
            prior.value()["assignment"]["sha256"] == requested
                && prior.value()["result_sha256"].as_str() == options.get("result-sha256"),
            "native adoption replay differs from its immutable transition",
        )?;
        authorize(panel, &operation, &checked)?;
        if !native_result::settled(&task) {
            native_result::check_custody(panel, checked.packet(), host, &AttemptCache::default())?;
        }
        return Ok(json!({"schema":1,"status":"adopted","attempt_id":attempt,
            "succession_sha256":prior.sha256(),"main_attempt_id":panel.attempt,
            "mode":prior.value()["mode"],
            "reused":true,"task_claim":"preserved","native_input":"not-sent",
            "next_action":adoption_guidance(prior.retirement_only())}));
    }
    require(
        checked.sha256() == requested,
        "native assignment changed; inspect its current exact digest before adoption",
    )?;
    let previous = prior
        .as_ref()
        .map_or(checked.parent_attempt_id(), |r| r.main_attempt_id());
    prove(panel, &checked, previous)?;
    let retained = oms_panel_core::native_results::read(&Store::open(panel.repo)?, &operation)?;
    require(
        retained
            .as_ref()
            .and_then(|r| r["artifact_sha256"].as_str())
            == options.get("result-sha256"),
        "native result changed; adoption requires its exact retained digest or exact absence",
    )?;
    let retirement = native_result::settled(&task);
    let custody = observed_custody(panel, host, &checked, retirement)?;
    require(
        custody["participant"] == checked.parent_participant()
            && custody["lead_pane"] == checked.packet()["lead_pane"]
            && custody["account"] == checked.packet()["account"]
            && custody["account_verification"] == checked.packet()["account_verification"]
            && custody["checkout"] == *checked.checkout(),
        "native successor cannot replace the original Lead, account or checkout",
    )?;
    let value = json!({"schema":1,"kind":"oms-native-main-succession",
        "mode":if retirement {"retirement"} else {"continuation"},
        "attempt_id":checked.attempt_id(),"parent_attempt_id":checked.parent_attempt_id(),
        "room":checked.room(),"task_sha256":checked.task_sha256(),
        "checkout_sha256":checked.checkout_sha256(),
        "assignment":{"path":operation["refs"]["native_binding"],"sha256":checked.sha256(),"round":checked.round()},
        "result_sha256":retained.as_ref().map(|r|&r["artifact_sha256"]),
        "previous":prior.as_ref().map(|r|json!({"sha256":r.sha256(),"main_attempt_id":r.main_attempt_id()})),
        "custody":custody});
    let bytes = serde_json::to_vec(&value).map_err(Error::new)?;
    let record = CheckedSuccession::parse_bound(&bytes, &checked)?;
    let path = format!(
        ".oms/lifecycle/attempts/{attempt}/native-succession-{}.json",
        record.sha256()
    );
    tx.write(
        Path::new(&path),
        &bytes,
        true,
        oms_panel_core::native_succession::MAX_SUCCESSION,
    )?;
    before_append();
    // Recheck after the immutable write. A crash may leave an unreferenced file;
    // identical retry reuses it and cannot allocate another claim or send input.
    require(
        native_assignment::current_operation(panel, &attempt)? == operation
            && native_assignment::task(panel, checked.task_id())? == task
            && observed_custody(panel, host, &checked, retirement)? == custody,
        "native assignment or successor custody changed before adoption",
    )?;
    prove(panel, &checked, previous)?;
    tx.check()?;
    native_result::annotate_checked(
        panel,
        &operation,
        &format!("native-succession-{}", record.sha256()),
        json!({"native_succession_schema":"1","native_succession_path":path,
            "native_succession_sha256":record.sha256(),"native_custodian_attempt":panel.attempt}),
        native_result::settled(&task),
    )?;
    let current = native_assignment::current_operation(panel, &attempt)?;
    authorize(panel, &current, &checked).map_err(|error| Error {
        publication_uncertain: true,
        ..error
    })?;
    Ok(json!({"schema":1,"status":"adopted","attempt_id":attempt,
        "succession_sha256":record.sha256(),"main_attempt_id":panel.attempt,
        "mode":record.value()["mode"],
        "reused":false,"task_claim":"preserved","native_input":"not-sent",
        "assignment_sha256":checked.sha256(),"result_sha256":value["result_sha256"],
        "next_action":adoption_guidance(retirement)}))
}

fn adoption_guidance(retirement: bool) -> &'static str {
    if retirement {
        "inspect Results and retire the settled assignment before selecting ready work"
    } else {
        "inspect Results and continue the existing frozen assignment"
    }
}
