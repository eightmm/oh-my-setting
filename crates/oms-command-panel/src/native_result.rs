//! Main-owned result reconciliation and reservations; reports never prove cleanup.
use super::*;
use oms_command_operations as events;
use oms_command_operations::value::{V, obj};
use oms_panel_core::native_contract::{CheckedAssignment, CheckedReport};

pub(super) const UNSENT_RETIREMENT: &str = "native_intent_unsent";

pub(super) struct Current {
    pub assignment: CheckedAssignment,
    pub operation: Value,
    pub task: Value,
}

fn settled(task: &Value) -> bool {
    matches!(task["state"].as_str(), Some("done" | "cancelled"))
        && task["completion_verified"] != false
}

pub(super) fn guard_unbound(panel: &Panel<'_>, options: &Options) -> Result<()> {
    if !options.has("task") || options.has("task-id") || options.has("unbound") {
        return Ok(());
    }
    if Store::open(panel.repo)?
        .read(Path::new(".oms/plan/tasks.json"), 4 * 1024 * 1024, true)?
        .is_empty()
    {
        return Ok(());
    }
    let plan = commands::query(
        panel.services,
        NativeCommand::Plan,
        panel.repo,
        &["list", "--json"],
        panel.stop,
    )?;
    require(
        plan["tasks"].is_array(),
        "canonical Plan inventory is unavailable",
    )?;
    require(
        !crate::rows(&plan["tasks"])
            .iter()
            .any(|task| !matches!(task["state"].as_str(), Some("done" | "cancelled"))),
        "canonical work is pending: use Codex --task-id, managed --dispatch --task-id, or explicitly --unbound for conversation without task credit",
    )
}

fn operation_value(value: &Value) -> Result<V> {
    V::parse(&serde_json::to_vec(value).map_err(Error::new)?).map_err(Error::new)
}

/// A reusable Lead has one unresolved canonical assignment. Its report or an
/// owner acknowledgement cannot release that reservation; the Plan must finish.
pub(super) fn guard_lead(
    panel: &Panel<'_>,
    task: &Value,
    history: &Value,
    tx: &Transaction<'_>,
) -> Result<()> {
    let room = commands::query(
        panel.services,
        NativeCommand::Room,
        panel.repo,
        &["show", "--id", panel.room, "--exact", "--json"],
        panel.stop,
    )?;
    require(
        room["id"] == panel.room && room["participants"].is_array(),
        "native reservation Room membership is unavailable",
    )?;
    for operation in crate::rows(history).iter().filter(|row| {
        row["tool"] == "panel-native-assignment" && row["refs"]["panel_room_id"] == panel.room
    }) {
        // A marker alone cannot retire a reservation. A crash after the
        // lifecycle transition must still retry the Room leave.
        if operation["terminal"] == true
            && operation["refs"]["native_resolution"] == "retired"
            && !crate::rows(&room["participants"])
                .iter()
                .any(|p| p["participant"] == operation["attempt_id"] && p["joined"] == true)
        {
            continue;
        }
        // Replacing a pane/account cannot discard the ordinary Lead slot's
        // unresolved reservation. A same-task retry separately checks lineage
        // and custody, and may recover a binding its crashed send never recorded.
        if operation["task_id"] == task["id"]
            && operation["refs"]["plan_id"] == task["plan_id"]
            && operation["refs"]["lease_id"] == task["lease_id"]
            && operation["parent_attempt_id"] == panel.attempt
        {
            continue;
        }
        let unresolved = format!(
            "native Lead still owns unresolved task {}; reconcile its exact report and finish the canonical Plan before assigning another task",
            text(operation, "task_id")
        );
        let bytes = packet(panel, operation)
            .map_err(|error| Error::new(format!("{unresolved} ({})", error.message)))?;
        let binding = oms_panel_core::decode(&bytes, native_assignment::MAX_BINDING)?;
        require(
            binding["kind"] == "oms-native-assignment"
                && binding["provider"] == "codex"
                && binding["attempt_id"] == operation["attempt_id"]
                && binding["parent_attempt_id"] == operation["parent_attempt_id"]
                && binding["room"] == panel.room
                && binding["task"]["id"] == operation["task_id"],
            "native reservation lineage changed",
        )?;
        let old = native_assignment::task(panel, text(operation, "task_id"))?;
        require(
            old["plan_id"] == operation["refs"]["plan_id"]
                && old["lease_id"] == operation["refs"]["lease_id"]
                && old["lease_epoch"] == binding["task"]["lease_epoch"]
                && old["provider"] == binding["task"]["provider"]
                && old["claimed_by_participant"] == operation["parent_attempt_id"]
                && settled(&old)
                && operation["parent_attempt_id"] == panel.attempt,
            &unresolved,
        )?;
        fresh_main(panel, &AttemptCache::default())?;
        tx.check()?;
        retire(panel, operation, text(&old, "state"))?;
    }
    Ok(())
}

fn retire(panel: &Panel<'_>, operation: &Value, plan_state: &str) -> Result<()> {
    let attempt = text(operation, "attempt_id");
    let room = commands::query(
        panel.services,
        NativeCommand::Room,
        panel.repo,
        &["show", "--id", panel.room, "--exact", "--json"],
        panel.stop,
    )?;
    require(
        room["id"] == panel.room && room["participants"].is_array(),
        "native retirement membership is unavailable",
    )?;
    let member = crate::rows(&room["participants"])
        .iter()
        .find(|p| {
            p["participant"] == attempt
                && p["parent"] == panel.attempt
                && p["provider"] == "codex"
                && p["role"] == "worker"
                && p["joined"].is_boolean()
        })
        .ok_or_else(|| Error::new("native retirement participant lineage changed"))?;
    let joined = member["joined"] == true;
    if !joined {
        require(
            operation["terminal"] == true
                && operation["refs"]["native_retired_plan_state"] == plan_state
                && matches!(
                    operation["refs"]["native_resolution"].as_str(),
                    Some("retiring" | "retired")
                ),
            "native participant left without its exact retirement intent",
        )?;
        if operation["refs"]["native_resolution"] == "retired" {
            return Ok(());
        }
    }
    // Persist the intent even if an independent lifecycle cancellation already
    // made this assignment terminal. Room leave and the final marker are separate
    // writes, so every prefix must retain the exact canonical settlement.
    if operation["refs"]["native_resolution"] != "retiring"
        || operation["refs"]["native_retired_plan_state"] != plan_state
    {
        annotate_checked(
            panel,
            operation,
            &format!("native-retire-{plan_state}"),
            json!({
                "native_resolution":"retiring","native_retired_plan_state":plan_state,
                "native_child_cleanup":"unsupported",
            }),
            true,
        )?;
    }
    if operation["terminal"] != true {
        // These are administrative assignment states. The persistent native
        // session is never a target of lifecycle process control.
        let states: &[&str] = if plan_state == "cancelled" {
            &["cancelled"]
        } else {
            match text(operation, "state") {
                "queued" => &["starting", "working", "review", "done"],
                "starting" => &["working", "review", "done"],
                "working" | "verifying" => &["review", "done"],
                "waiting_input" | "waiting_approval" | "blocked" => &["working", "review", "done"],
                "review" => &["done"],
                _ => return Err(Error::new("native assignment state cannot be retired")),
            }
        };
        let mut args = commands::strings(&[
            "transition",
            "--attempt",
            attempt,
            "--state",
            states[0],
            "--actor",
            "panel-native-result",
            "--reason-code",
            "canonical_plan_settled",
            "--idempotency-key",
            &format!("native-plan-{plan_state}"),
        ]);
        for state in &states[1..] {
            args.extend(commands::strings(&["--then", state]));
        }
        panel
            .services
            .command(NativeCommand::Events, panel.repo, &args, panel.stop)?
            .success()?;
    }
    // The canonical Room leave producer requires a joined member. A replay
    // after that write observes the exact left membership instead of sending a
    // second leave or mistaking a missing/foreign participant for completion.
    if joined {
        panel
            .services
            .command(
                NativeCommand::Room,
                panel.repo,
                &commands::strings(&["leave", "--id", panel.room, "--participant", attempt]),
                panel.stop,
            )?
            .success()?;
    }
    let current = native_assignment::current_operation(panel, attempt)?;
    require(
        current["terminal"] == true,
        "native retirement has not settled its assignment",
    )?;
    annotate_checked(
        panel,
        &current,
        &format!("native-retired-{plan_state}"),
        json!({
            "native_resolution":"retired","native_retired_plan_state":plan_state,
            "native_child_cleanup":"unsupported",
        }),
        true,
    )
}

pub(super) fn annotate(panel: &Panel<'_>, operation: &Value, key: &str, refs: Value) -> Result<()> {
    annotate_checked(panel, operation, key, refs, false)
}

fn annotate_checked(
    panel: &Panel<'_>,
    operation: &Value,
    key: &str,
    refs: Value,
    allow_terminal: bool,
) -> Result<()> {
    let candidate = events::new_event(
        text(operation, "attempt_id"),
        0usize.into(),
        "attempt.usage",
        obj([
            (
                "actor",
                obj([
                    ("kind", "owner".into()),
                    ("name", "panel-native-result".into()),
                ]),
            ),
            ("idempotency_key", key.into()),
            ("refs", operation_value(&refs)?),
        ]),
    )
    .map_err(Error::new)?;
    let sequence = operation["sequence"].to_string();
    // Room membership, Plan lease, tmux pane identity, and account custody are
    // sampled across separate stores before/after the lifecycle lock; only
    // exact Main liveness/currentness and assignment state are checked
    // atomically under the lifecycle ledger lock (not atomic cross-store custody).
    events::append_lifecycle_checked_with_projection(
        panel.repo,
        candidate,
        panel.stop,
        |_, projected, current| {
            native_assignment::require_live_main_in_locked_ledger(panel, projected)?;
            let current = current.ok_or_else(|| "native assignment disappeared".to_owned())?;
            if current.get("sequence").py() != sequence
                || (!allow_terminal && current.get("terminal").truth())
            {
                return Err("native assignment changed; reread before recording its result".into());
            }
            Ok(())
        },
    )
    .map_err(Error::new)?;
    Ok(())
}

pub(super) fn packet(panel: &Panel<'_>, operation: &Value) -> Result<Vec<u8>> {
    let attempt = text(operation, "attempt_id");
    crate::identifier(attempt)?;
    let fallback = format!(".oms/lifecycle/attempts/{attempt}/native-assignment.json");
    let path = operation["refs"]["native_binding"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&fallback);
    require(
        Path::new(path).parent() == Some(Path::new(&format!(".oms/lifecycle/attempts/{attempt}"))),
        "native assignment binding leaves its attempt directory",
    )?;
    let bytes =
        Store::open(panel.repo)?.read(Path::new(path), native_assignment::MAX_BINDING, false)?;
    if let Some(digest) = operation["refs"]["native_binding_sha256"].as_str() {
        require(
            oms_panel_core::digest(&bytes) == digest,
            "native assignment binding digest changed",
        )?;
    }
    Ok(bytes)
}

fn fresh_main<'a>(panel: &Panel<'a>, attempts: &AttemptCache) -> Result<Panel<'a>> {
    let fresh = panel.reload()?;
    require(
        fresh.room == panel.room
            && fresh.main.id == panel.main.id
            && fresh.main.window == panel.main.window
            && fresh.main.pid == panel.main.pid
            && fresh.session_proof == panel.session_proof
            && fresh.attempt == panel.attempt
            && fresh.participant == panel.participant
            && fresh.owner == panel.owner,
        "native result Main or session changed",
    )?;
    fresh.require_codex_assignment_main(fresh.repo, attempts)?;
    Ok(fresh)
}

pub(super) fn check_custody(
    panel: &Panel<'_>,
    assignment: &Value,
    host: &dyn AccountHost,
    attempts: &AttemptCache,
) -> Result<()> {
    let fresh = fresh_main(panel, attempts)?;
    let lead = panel
        .leads
        .get("codex")
        .ok_or_else(|| Error::new("native Lead unavailable"))?;
    require(
        fresh.leads.get("codex").is_some_and(|current| {
            current.id == lead.id && current.window == lead.window && current.pid == lead.pid
        }),
        "native result Main, Codex lead, or session changed",
    )?;
    let custody = fresh.codex_assignment_custody(host)?;
    let (_, _, verification) = binding_summary(custody.binding.as_ref());
    require(
        assignment["session"] == json!(fresh.session_proof)
            && assignment["main_pane"]
                == json!({"id":custody.main.id,"window":custody.main.window,"pid":custody.main.pid,"birth":custody.main.birth})
            && assignment["lead_pane"]
                == json!({"id":custody.lead.id,"window":custody.lead.window,"pid":custody.lead.pid,"birth":custody.lead.birth})
            && assignment["account"]
                == serde_json::to_value(&custody.binding).map_err(Error::new)?
            && assignment["account_verification"]
                == serde_json::to_value(verification).map_err(Error::new)?,
        "native result Main, Lead or account custody changed",
    )?;
    let lead = fresh
        .leads
        .get("codex")
        .ok_or_else(|| Error::new("native Lead unavailable"))?;
    require(
        native_assignment::checkout(&fresh, lead)? == assignment["checkout"],
        "native result checkout or base changed",
    )
}

pub(super) fn load(
    panel: &Panel<'_>,
    attempt: &str,
    host: &dyn AccountHost,
    attempts: &AttemptCache,
) -> Result<Current> {
    crate::identifier(attempt)?;
    let operation = native_assignment::current_operation(panel, attempt)?;
    require(
        operation["tool"] == "panel-native-assignment"
            && operation["parent_attempt_id"] == panel.attempt
            && operation["refs"]["panel_room_id"] == panel.room,
        "native result is not owned by this Main and Room",
    )?;
    let assignment = CheckedAssignment::parse(&packet(panel, &operation)?)?
        .ok_or_else(|| Error::new("legacy native assignment has no typed return contract; reconcile it with its existing owner"))?;
    require(
        assignment.attempt_id() == attempt
            && assignment.parent_attempt_id() == panel.attempt
            && assignment.room() == panel.room
            && assignment.owner() == panel.owner
            && assignment.task_id() == text(&operation, "task_id")
            && assignment.plan_id() == text(&operation["refs"], "plan_id")
            && assignment.lease_id() == text(&operation["refs"], "lease_id"),
        "native return assignment identity differs from the lifecycle owner",
    )?;
    let task = native_assignment::task(panel, assignment.task_id())?;
    require(
        task["plan_id"] == assignment.plan_id()
            && task["lease_id"] == assignment.lease_id()
            && task["lease_epoch"] == assignment.lease_epoch()
            && task["claimed_by_participant"] == panel.attempt
            && oms_panel_core::native_contract::stable_task_sha256(&task)?
                == assignment.task_sha256(),
        "native return task, lease or frozen criteria changed",
    )?;
    if settled(&task) {
        // Landing may have advanced the Lead checkout. Only an exact canonical
        // completion permits administrative retirement without reusing that old
        // input custody as current process or cleanup evidence.
        fresh_main(panel, &AttemptCache::default())?;
    } else {
        check_custody(panel, assignment.packet(), host, attempts)?;
    }
    let room = commands::query(
        panel.services,
        NativeCommand::Room,
        panel.repo,
        &["show", "--id", panel.room, "--exact", "--json"],
        panel.stop,
    )?;
    require(
        room["id"] == panel.room && room["participants"].is_array(),
        "native result Room membership is unavailable",
    )?;
    let joined = crate::rows(&room["participants"]).iter().any(|p| {
        p["participant"] == attempt
            && p["joined"] == true
            && p["parent"] == panel.attempt
            && p["provider"] == "codex"
            && p["role"] == "worker"
    });
    let left = crate::rows(&room["participants"]).iter().any(|p| {
        p["participant"] == attempt
            && p["joined"] == false
            && p["parent"] == panel.attempt
            && p["provider"] == "codex"
            && p["role"] == "worker"
    });
    let retired = settled(&task)
        && operation["terminal"] == true
        && left
        && operation["refs"]["native_retired_plan_state"] == task["state"]
        && matches!(
            operation["refs"]["native_resolution"].as_str(),
            Some("retiring" | "retired")
        );
    require(
        joined || retired,
        "native assignment Room participant changed",
    )?;
    Ok(Current {
        assignment,
        operation,
        task,
    })
}

pub(super) fn report(panel: &Panel<'_>, current: &Current) -> Result<Option<Value>> {
    oms_panel_core::native_results::read(&Store::open(panel.repo)?, &current.operation)
}

pub(super) fn source_report(
    panel: &Panel<'_>,
    assignment: &CheckedAssignment,
    retained: &Value,
) -> Result<CheckedReport> {
    let source = &retained["source"];
    let raw = Store::open(panel.repo)?.read(
        Path::new(text(source, "path")),
        oms_panel_core::native_contract::MAX_REPORT,
        false,
    )?;
    require(
        source["sha256"] == oms_panel_core::digest(&raw)
            && source["bytes"].as_u64() == Some(raw.len() as u64),
        "retained native return source changed",
    )?;
    CheckedReport::parse_bound(&raw, assignment)
}

fn notice(
    panel: &Panel<'_>,
    current: &Current,
    report: &CheckedReport,
    result_sha: &str,
    decision: Option<&str>,
) -> Result<()> {
    let id = format!(
        "native-{}-{}",
        decision.unwrap_or("report"),
        &result_sha[..24]
    );
    let body = format!(
        "Native return for task {} / attempt {}: {}; report {}. {} Verification and acceptance remain separate; native child cleanup is unconfirmed.",
        current.assignment.task_id(),
        current.assignment.attempt_id(),
        report.outcome(),
        result_sha,
        decision
            .map(|d| format!("Main decision: {d}."))
            .unwrap_or_else(|| "Main reconciliation required.".into()),
    );
    panel
        .services
        .command(
            NativeCommand::Room,
            panel.repo,
            &commands::strings(&[
                "send",
                "--id",
                panel.room,
                "--participant",
                current.assignment.attempt_id(),
                "--to",
                &panel.attempt,
                "--kind",
                "status",
                "--message-id",
                &id,
                "--text",
                &body,
            ]),
            panel.stop,
        )?
        .success()
}

fn reviewed_binding(current: &Current, retained: &Value) -> Result<()> {
    require(
        matches!(current.task["state"].as_str(), Some("review" | "done"))
            && current.task["review_lease_id"] == current.assignment.lease_id()
            && current.task["artifact"] == retained["artifact"]
            && (current.task["state"] == "done"
                || current.task["patch"] == retained["patch"]["path"])
            && current.task["review_evidence"]["schema"] == 1
            && current.task["review_evidence"]["artifact"]["path"] == retained["artifact"]
            && current.task["review_evidence"]["artifact"]["sha256"] == retained["artifact_sha256"]
            && current.task["review_evidence"]["patch"]["path"] == retained["patch"]["path"]
            && current.task["review_evidence"]["patch"]["sha256"] == retained["patch"]["sha256"],
        "Plan review lacks the exact native evidence binding; reconciliation cannot overwrite it",
    )
}

/// The first lifecycle sequence is the only pre-binding state. The sender
/// records its binding and sending boundary before joining the native input path.
fn retire_unsent(
    panel: &Panel<'_>,
    attempt: &str,
    intent: &str,
    tx: &Transaction<'_>,
) -> Result<Value> {
    let operation = native_assignment::current_operation(panel, attempt)?;
    let refs = &operation["refs"];
    require(
        operation["tool"] == "panel-native-assignment"
            && operation["provider"] == "codex"
            && operation["parent_attempt_id"] == panel.attempt
            && refs["panel_room_id"] == panel.room
            && refs["panel_owner"] == panel.owner
            && refs["native_assignment_schema"] == "1"
            && refs["native_return_schema"] == "1"
            && refs["native_intent_sha256"] == intent,
        "native intent is not the exact typed assignment owned by this Main and Room",
    )?;
    let replay = operation["state"] == "cancelled"
        && operation["sequence"] == 2
        && operation["reason_code"] == UNSENT_RETIREMENT
        && refs["native_resolution"] == "retired";
    require(
        native_assignment::original_unbound_intent(&operation)
            || (replay && native_assignment::unbound_intent(&operation)),
        "only the original pre-binding native intent can be retired as unsent",
    )?;
    if !replay {
        let task = native_assignment::task(panel, text(&operation, "task_id"))?;
        require(
            matches!(task["state"].as_str(), Some("claimed" | "running"))
                && task["plan_id"] == refs["plan_id"]
                && task["lease_id"] == refs["lease_id"]
                && task["provider"] == panel.owner
                && task["claimed_by_participant"] == panel.attempt,
            "native intent no longer owns its exact canonical Plan claim",
        )?;
    }
    fresh_main(panel, &AttemptCache::default())?;
    let repo = panel
        .repo
        .to_str()
        .ok_or_else(|| Error::new("repository cannot be represented in a Room locator"))?;
    let room = commands::query(
        panel.services,
        NativeCommand::Room,
        panel.repo,
        &[
            "show", "--repo", repo, "--id", panel.room, "--exact", "--json",
        ],
        panel.stop,
    )?;
    require(
        panel.has_live_main_membership(&room)
            && !crate::rows(&room["participants"])
                .iter()
                .any(|member| member["participant"] == attempt),
        "native intent retirement requires an exact Room with no assignment member",
    )?;
    let artifact = format!(".oms/lifecycle/attempts/{attempt}/native-assignment.json");
    let candidate = events::new_event(
        attempt,
        0usize.into(),
        "attempt.state_changed",
        obj([
            ("from_state", V::Null),
            ("to_state", "cancelled".into()),
            ("expected_state", "queued".into()),
            ("expected_seq", 1usize.into()),
            ("reason_code", UNSENT_RETIREMENT.into()),
            (
                "actor",
                obj([
                    ("kind", "owner".into()),
                    ("name", "panel-native-result".into()),
                ]),
            ),
            (
                "idempotency_key",
                format!("native-retire-unsent-{intent}").into(),
            ),
            ("refs", obj([("native_resolution", "retired".into())])),
        ]),
    )
    .map_err(Error::new)?;
    tx.check()?;
    let sequence = if replay { "2" } else { "1" };
    events::append_lifecycle_checked_with_projection(
        panel.repo,
        candidate,
        panel.stop,
        |_, projected, current| {
            native_assignment::require_live_main_in_locked_ledger(panel, projected)?;
            let current = current.ok_or_else(|| "native intent disappeared".to_owned())?;
            if current.get("sequence").py() != sequence
                || current.get("refs").get("native_intent_sha256").py() != intent
            {
                return Err("native intent changed before retirement".into());
            }
            match native_assignment::initial_packet_missing(panel.repo, Path::new(&artifact)) {
                Ok(true) => Ok(()),
                _ => Err("native intent retirement requires a missing binding path".into()),
            }
        },
    )
    .map_err(Error::new)?;
    Ok(
        json!({"schema":1,"status":"retired-unsent","attempt_id":attempt,
        "intent_sha256":intent,"reused":replay,"plan_changed":false}),
    )
}

pub(super) fn reconcile(
    panel: &Panel<'_>,
    options: &Options,
    host: &dyn AccountHost,
    attempts: &AttemptCache,
    tx: &Transaction<'_>,
) -> Result<Value> {
    let attempt = options.required("native-reconcile")?;
    let decision = options.required("decision")?;
    if decision == "retire-unsent" {
        return retire_unsent(panel, &attempt, &options.required("intent-sha256")?, tx);
    }
    let current = load(panel, &attempt, host, attempts)?;
    require(
        current.operation["terminal"] == false || settled(&current.task),
        "native assignment is already terminal",
    )?;
    let retained = report(panel, &current)?
        .ok_or_else(|| Error::new("native assignment has no retained result"))?;
    let result_sha = text(&retained, "artifact_sha256");
    require(
        result_sha == options.required("result-sha256")?,
        "native result changed; review its current retained digest",
    )?;
    let source = source_report(panel, &current.assignment, &retained)?;
    let candidate = retained["patch"].is_object();
    let artifact = text(&retained, "artifact");
    let patch_path = text(&retained["patch"], "path");
    let replayed = current.operation["refs"]["native_resolution"] == decision
        && current.operation["refs"]["native_resolution_result_sha256"] == result_sha;
    if decision == "reviewed"
        && candidate
        && current.task["state"] != "cancelled"
        && (replayed || matches!(current.task["state"].as_str(), Some("review" | "done")))
    {
        reviewed_binding(&current, &retained)?;
    }
    if settled(&current.task) {
        require(
            decision == "reviewed",
            "a settled native task only permits exact-result retirement",
        )?;
        tx.check()?;
        let reused = current.operation["refs"]["native_resolution"] == "retired";
        retire(panel, &current.operation, text(&current.task, "state"))?;
        return Ok(json!({"schema":1,"status":"retired","attempt_id":attempt,
            "result_sha256":result_sha,"reused":reused,"plan_state":current.task["state"],
            "child_cleanup":"unsupported","native_session":"preserved",
            "next_action":"select the next canonical ready task"}));
    }
    // Capture already froze these exact bytes before publishing its reference.
    // The independent Plan review never consumes a report-supplied verifier.
    if !replayed {
        require(
            current.operation["refs"]["native_resolution"] != "reviewed",
            "reviewed native result cannot be silently reopened",
        )?;
        if decision == "reviewed" && candidate {
            if current.task["state"] != "review" {
                native_assignment::claimed(panel, &current.task)?;
                tx.check()?;
                panel
                    .services
                    .command(
                        NativeCommand::Plan,
                        panel.repo,
                        &commands::strings(&[
                            "review",
                            "--id",
                            current.assignment.task_id(),
                            "--lease-id",
                            current.assignment.lease_id(),
                            "--artifact",
                            artifact,
                            "--patch",
                            patch_path,
                            "--expected-review-artifact-sha256",
                            result_sha,
                            "--expected-review-patch-sha256",
                            text(&retained["patch"], "sha256"),
                        ]),
                        panel.stop,
                    )?
                    .success()?;
            }
        } else {
            native_assignment::claimed(panel, &current.task)?;
        }
        let fresh = load(panel, &attempt, host, &AttemptCache::default())?;
        require(
            fresh.operation == current.operation,
            "native assignment changed before reconciliation",
        )?;
        if decision == "reviewed" && candidate {
            reviewed_binding(&fresh, &retained)?;
        }
        tx.check()?;
        annotate(
            panel,
            &current.operation,
            &format!(
                "native-resolve-{result_sha}-{}-{decision}",
                current.operation["sequence"]
            ),
            json!({"native_resolution":decision,"native_resolution_result_sha256":result_sha}),
        )?;
    }
    notice(panel, &current, &source, result_sha, Some(&decision))?;
    let next = match decision.as_str() {
        "request-revision" => {
            "continue the same task with --after-native-result and this exact retained result digest"
        }
        "reviewed" if candidate => {
            "run patch-admit or patch-land --plan-task from a clean integration checkout; acceptance remains pending"
        }
        "reviewed" => "retain findings; complete through the existing verified Plan owner path",
        _ => "hold this assignment and resolve its blocker with Main",
    };
    Ok(
        json!({"schema":1,"status":decision,"attempt_id":attempt,"result_sha256":result_sha,
        "reused":replayed,"verification":"unverified","acceptance":"pending","next_action":next,
        "plan_task_id":current.assignment.task_id(),"child_cleanup":"unsupported"}),
    )
}
