//! Evidence-bound decisions over the existing Plan and autopilot owner.
//! Observations and model claims never grant task, verification or publication authority.
use crate::storage::{ACTIVE, Context, MAX_FILE, PROGRESS, RECEIPT};
use crate::{Error, Result, Services, planning, require, value};
use oms_command_plan::{Action, Request};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub const MAX_EVIDENCE: usize = 64 * 1024;

pub(crate) struct WakeLaunch {
    record: Value,
}
pub(crate) fn start_wake(ctx: &Context<'_>) -> Result<Option<WakeLaunch>> {
    let names = [
        "OMS_AUTOPILOT_WAKE_LAUNCH_ID",
        "OMS_AUTOPILOT_WAKE_OWNER_ID",
        "OMS_AUTOPILOT_WAKE_RECEIPT_SHA256",
    ];
    let values = names.map(|n| std::env::var(n).unwrap_or_default());
    if values.iter().all(String::is_empty) {
        return Ok(None);
    }
    ctx.parent()?;
    require(
        value::id(&values[0], 160) && value::id(&values[1], 160) && value::digest(&values[2]),
        "incomplete wake launch identity",
    )?;
    let raw = ctx.store.read(RECEIPT, MAX_EVIDENCE)?;
    let row = value::parse(&raw)?;
    crate::receipt::validate(&row)?;
    require(
        row["owner_id"] == values[1] && value::hash(&raw) == values[2],
        "wake receipt owner or bytes changed",
    )?;
    let _lock = ctx.lock(PROGRESS)?;
    require(
        !ctx.rows(PROGRESS)?
            .iter()
            .any(|r| r["kind"] == "autopilot-wake-launch" && r["launch_id"] == values[0]),
        "wake launch already entered; reconcile its exact observation instead of replaying",
    )?;
    let record = json!({"schema":1,"kind":"autopilot-wake-launch","launch_id":values[0],"owner_id":values[1],
        "input_receipt_sha256":values[2],"phase":"started","ts":value::now()});
    ctx.append(PROGRESS, &record)?;
    Ok(Some(WakeLaunch { record }))
}
pub(crate) fn finish_wake(ctx: &Context<'_>, mut launch: WakeLaunch, exit: i32) -> Result<()> {
    launch.record["phase"] = "finished".into();
    launch.record["exit"] = exit.into();
    launch.record["ts"] = value::now().into();
    let _lock = ctx.lock(PROGRESS)?;
    ctx.append(PROGRESS, &launch.record)
}

/// Narrow presentation normalization only. The exact source digest is retained
/// separately; a signature cannot prove result identity or authorize execution.
pub fn failure_signature(text: &str) -> String {
    let timestamp = regex::Regex::new(
        r"\b\d{4}-\d{2}-\d{2}[T ][0-2]\d:[0-5]\d:[0-5]\d(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?\b",
    )
    .unwrap();
    let temporary =
        regex::Regex::new(r"(?:/private)?/tmp/(?:oms|pytest|tmp)[A-Za-z0-9_.-]+(?:/)?").unwrap();
    let text = timestamp.replace_all(text, "<timestamp>");
    let text = temporary.replace_all(&text, "<temporary>/");
    value::hash(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub(crate) fn classification(ctx: &Context<'_>, text: &str, exit: i32) -> Value {
    let result = ctx
        .services
        .classify_autonomy_failure(text, exit)
        .unwrap_or(Value::Null);
    let code = value::text(&result, "code");
    let recovery = value::text(&result, "recovery");
    let excerpt = oms_scrub::redact(text.as_bytes(), &[])
        .ok()
        .map(|r| {
            String::from_utf8_lossy(&r.bytes)
                .chars()
                .take(4096)
                .collect::<String>()
        })
        .filter(|s| planning::scan(s.as_bytes()).is_ok());
    json!({"code": if value::id(code, 80) { code } else { "unknown_failure" },
        "recovery": if value::id(recovery, 80) { recovery } else { "preserve_and_escalate" },
        "retryable":result["retryable"] == true,
        "fallback_allowed":result["fallback_allowed"] == true,
        "context_escalation_allowed":result["context_escalation_allowed"] == true,
        "exit":exit,"source_sha256":value::hash(text),"signature_sha256":failure_signature(text),"diagnostic":excerpt})
}

pub(crate) fn record_failure(
    ctx: &Context<'_>,
    phase: &str,
    text: &str,
    exit: i32,
) -> Result<String> {
    let failure = classification(ctx, text, exit);
    let code = value::text(&failure, "code").to_owned();
    let _lock = ctx.lock(PROGRESS)?;
    ctx.append(
        PROGRESS,
        &json!({"schema":1,"kind":"autopilot-failure","owner_id":ctx.owner_id,
        "run_id":ctx.run_id,"cycle":ctx.cycle,"phase":phase,"head":ctx.git(&["rev-parse","HEAD"])?,
        "failure":failure,"ts":value::now()}),
    )?;
    Ok(code)
}

/// Immutable references are read through the descriptor-checked canonical store.
pub(crate) fn evidence(ctx: &Context<'_>, reference: &Value) -> Result<Value> {
    value::keys(reference, &["path", "sha256"], &[])?;
    let path = value::text(reference, "path");
    require(
        path.starts_with(".oms/plan/adaptive/")
            && !path.contains("..")
            && !path.contains('\\')
            && value::digest(value::text(reference, "sha256")),
        "invalid adaptive evidence reference",
    )?;
    let raw = ctx.store.read(path, MAX_EVIDENCE)?;
    require(
        value::hash(&raw) == reference["sha256"],
        "adaptive evidence changed",
    )?;
    value::parse(&raw)
}

pub(crate) fn retain(ctx: &Context<'_>, packet: &Value) -> Result<Value> {
    let raw = value::pretty(packet)?;
    require(
        raw.len() <= MAX_EVIDENCE,
        "adaptive evidence exceeds 64 KiB",
    )?;
    planning::scan(&raw)?;
    let sha = value::hash(&raw);
    let path = format!(".oms/plan/adaptive/{sha}.json");
    ctx.store.publish_once(&path, &raw, MAX_EVIDENCE)?;
    Ok(json!({"path":path,"sha256":sha}))
}

/// Capture only this invocation's canonical review artifacts, never global latest.
/// A provider error, unverified quotation or mechanical verifier failure is not
/// a source-backed semantic finding.
pub(crate) fn review_evidence(
    ctx: &Context<'_>,
    row: &Value,
    directory: &str,
    exit: i32,
    command: &str,
) -> Result<Value> {
    ctx.clean()?;
    require(
        directory.starts_with(".oms/artifacts/autopilot-review-") && !directory.contains(".."),
        "invalid owned review directory",
    )?;
    let names = ctx.source.names(directory, 24, false)?;
    let verdicts = names
        .iter()
        .filter_map(|s| s.to_str())
        .filter(|s| s.starts_with("_verdicts-") && s.ends_with(".json"))
        .collect::<Vec<_>>();
    require(
        verdicts.len() == 1,
        "review completion is unknown or ambiguous; do not resend",
    )?;
    let raw = ctx
        .source
        .read(Path::new(directory).join(verdicts[0]), MAX_EVIDENCE)?;
    let verdict = value::parse(&raw)?;
    let run = value::text(&verdict, "run");
    require(
        value::id(run, 160) && verdict["overall"].as_i64() == Some(i64::from(exit)),
        "review verdict does not match this invocation",
    )?;
    let scope_path = format!("{directory}/_scope-{run}.json");
    let scope_raw = ctx.source.read(&scope_path, MAX_EVIDENCE)?;
    let scope = value::parse(&scope_raw)?;
    let head = ctx.git(&["rev-parse", "HEAD"])?;
    require(
        scope["kind"] == "review-scope"
            && scope["head"] == head
            && scope["base"] == row["contract"]["base_sha"]
            && scope["tracked_clean"] == true
            && scope["incremental"] == false
            && scope["diff_included"] == true,
        "review scope no longer matches the frozen whole change",
    )?;
    let index_raw = ctx
        .source
        .read(".oms/artifacts/index.jsonl", crate::storage::MAX_LEDGER)?;
    require(index_raw.ends_with(b"\n"), "incomplete review index")?;
    let index = index_raw
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
        .map(value::parse)
        .collect::<Result<Vec<_>>>()?;
    let outcomes = index
        .iter()
        .filter(|r| r["kind"] == "review-outcome" && r["operation_id"] == run)
        .collect::<Vec<_>>();
    require(
        outcomes.len() == 1
            && outcomes[0]["exit"].as_i64() == Some(i64::from(exit))
            && outcomes[0]["review_scope"]["diff_sha256"] == scope["diff_sha256"]
            && outcomes[0]["review_scope"]["head"] == head
            && outcomes[0]["review_scope"]["gate"] == true,
        "canonical review outcome is missing or changed",
    )?;
    let checks = index
        .iter()
        .filter(|r| r["kind"] == "review-verify" && r["operation_id"] == run)
        .collect::<Vec<_>>();
    require(
        checks.len() == 1,
        "review verifier receipt is missing or ambiguous",
    )?;
    let check = checks[0];
    let command_criterion = format!("criterion-plan-acceptance-{}", &value::hash(command)[..10]);
    require(
        check["covers"]
            .as_array()
            .is_some_and(|a| a.iter().any(|c| c == &command_criterion)),
        "review verifier does not match the frozen command",
    )?;
    let verify_scope = ctx
        .source
        .read(value::text(check, "source"), MAX_EVIDENCE)?;
    require(
        value::hash(&verify_scope) == check["source_sha256"] && check["verified_head"] == head,
        "review verifier evidence changed",
    )?;
    let verified_scope = value::parse(&verify_scope)?;
    require(
        verified_scope["diff_sha256"] == scope["diff_sha256"] && verified_scope["head"] == head,
        "review verifier scope differs",
    )?;
    let finding_path = format!("{directory}/_findings-{run}.jsonl");
    let finding_raw = ctx
        .source
        .optional(&finding_path, MAX_EVIDENCE)?
        .unwrap_or_default();
    require(
        finding_raw.is_empty() || finding_raw.ends_with(b"\n"),
        "incomplete review findings",
    )?;
    let mut findings = vec![];
    let allowed = planning::envelope(value::text(&row["contract"], "allowed"))?;
    for line in finding_raw.split(|b| *b == b'\n').filter(|r| !r.is_empty()) {
        require(findings.len() < 32, "review findings exceed bound")?;
        let finding = value::parse(line)?;
        let path = value::text(&finding, "file");
        if finding["run"] != run
            || finding["severity"] != "blocking"
            || finding["quote_found"] != true
            || !matches!(
                value::text(&finding, "line_anchor_status"),
                "verified" | "relocated"
            )
            || finding["diff_sha256"] != scope["diff_sha256"]
        {
            continue;
        }
        let path = crate::contract::path(path)?;
        require(
            !path.starts_with(".oms/")
                && !path.starts_with(".git/")
                && allowed
                    .iter()
                    .any(|p| p == "." || path == *p || path.starts_with(&format!("{p}/"))),
            "review finding escapes the authorized envelope",
        )?;
        let source = ctx.source.read(&path, MAX_FILE)?;
        require(
            value::hash(&source) == finding["source_sha256"],
            "review finding source changed",
        )?;
        findings.push(finding);
    }
    let complete = verdict["seats"].as_array().is_some_and(|seats| {
        !seats.is_empty()
            && seats
                .iter()
                .all(|s| matches!(value::text(s, "verdict"), "pass" | "fail"))
    });
    let corrective = exit == 1 && complete && check["exit"] == 0 && !findings.is_empty();
    ctx.clean()?;
    require(
        ctx.git(&["rev-parse", "HEAD"])? == head,
        "review source changed during capture",
    )?;
    retain(
        ctx,
        &json!({"schema":1,"kind":"autopilot-review-evidence","owner_id":row["owner_id"],
        "spec_sha256":row["spec_sha256"],"head":head,"tree":ctx.git(&["rev-parse","HEAD^{tree}"])?,
        "plan_sha256":value::hash(ctx.store.read(ACTIVE,8*MAX_FILE)?),"run":run,
        "scope":scope,"scope_sha256":value::hash(scope_raw),"verdict":verdict,
        "verdict_sha256":value::hash(raw),"findings_sha256":value::hash(finding_raw),
        "verification":check,"outcome_sha256":value::hash(value::canonical(outcomes[0])?),
        "verify_sha256":value::hash(command),"findings":findings,"corrective":corrective,"directory":directory}),
    )
}

/// A retained interpretation never substitutes for the native source artifacts.
/// Historical source contents remain bound even after the branch advances.
pub(crate) fn check_review_sources(ctx: &Context<'_>, packet: &Value) -> Result<()> {
    if packet["kind"] != "autopilot-review-evidence" {
        return Ok(());
    }
    let directory = value::text(packet, "directory");
    let run = value::text(packet, "run");
    require(
        directory.starts_with(".oms/artifacts/autopilot-review-")
            && !directory.contains("..")
            && value::id(run, 160),
        "invalid retained review source identity",
    )?;
    for (name, field) in [
        ("scope", "scope_sha256"),
        ("verdicts", "verdict_sha256"),
        ("findings", "findings_sha256"),
    ] {
        let extension = if name == "findings" { "jsonl" } else { "json" };
        let path = format!("{directory}/_{name}-{run}.{extension}");
        let raw = if name == "findings" {
            ctx.source
                .optional(&path, MAX_EVIDENCE)?
                .unwrap_or_default()
        } else {
            ctx.source.read(&path, MAX_EVIDENCE)?
        };
        require(
            value::hash(raw) == packet[field],
            "retained review source changed",
        )?;
    }
    let check = &packet["verification"];
    require(
        value::hash(
            ctx.source
                .read(value::text(check, "source"), MAX_EVIDENCE)?,
        ) == check["source_sha256"],
        "retained verifier source changed",
    )?;
    let raw = ctx
        .source
        .read(".oms/artifacts/index.jsonl", crate::storage::MAX_LEDGER)?;
    require(raw.ends_with(b"\n"), "incomplete review index")?;
    let rows = raw
        .split(|b| *b == b'\n')
        .filter(|r| !r.is_empty())
        .map(value::parse)
        .collect::<Result<Vec<_>>>()?;
    let outcomes = rows
        .iter()
        .filter(|r| r["kind"] == "review-outcome" && r["operation_id"] == run)
        .collect::<Vec<_>>();
    let checks = rows
        .iter()
        .filter(|r| r["kind"] == "review-verify" && r["operation_id"] == run)
        .collect::<Vec<_>>();
    require(
        outcomes.len() == 1
            && checks.len() == 1
            && checks[0] == check
            && value::hash(value::canonical(outcomes[0])?) == packet["outcome_sha256"],
        "retained canonical review receipt changed",
    )?;
    if packet["head"] == ctx.git(&["rev-parse", "HEAD"])? {
        for finding in packet["findings"].as_array().into_iter().flatten() {
            require(
                value::hash(ctx.source.read(value::text(finding, "file"), MAX_FILE)?)
                    == finding["source_sha256"],
                "retained finding source changed",
            )?;
        }
    }
    Ok(())
}

pub(crate) fn state(row: &mut Value) -> &mut Value {
    if row.get("adaptive").is_none() {
        row["adaptive"] = json!({"schema":1,"milestone_index":0,"completed_milestones":[],
            "review":null,"recovery":null,"wait":null,"last_decision":null});
    }
    &mut row["adaptive"]
}

pub(crate) fn validate_state(row: &Value) -> Result<()> {
    if let Some(s) = row.get("adaptive") {
        value::keys(
            s,
            &[
                "schema",
                "milestone_index",
                "completed_milestones",
                "review",
                "recovery",
                "wait",
                "last_decision",
            ],
            &[],
        )?;
        require(
            s["schema"] == 1
                && s["milestone_index"].as_u64().is_some_and(|i| i <= 40)
                && s["completed_milestones"]
                    .as_array()
                    .is_some_and(|v| v.len() <= 40),
            "invalid adaptive state",
        )?;
        for key in ["review", "recovery", "last_decision"] {
            if !s[key].is_null() {
                value::keys(&s[key], &["path", "sha256"], &[])?;
                require(
                    value::digest(value::text(&s[key], "sha256")),
                    "invalid adaptive evidence digest",
                )?;
            }
        }
        if !s["wait"].is_null() {
            value::keys(
                &s["wait"],
                &["binding", "binding_sha256", "consumed_result_sha256"],
                &[],
            )?;
            require(
                value::hash(value::canonical(&s["wait"]["binding"])?)
                    == s["wait"]["binding_sha256"],
                "wait binding changed",
            )?;
            let consumed = value::text(&s["wait"], "consumed_result_sha256");
            require(
                consumed.is_empty() || value::digest(consumed),
                "invalid consumed wait result",
            )?;
        }
    }
    Ok(())
}

pub(crate) fn budget(ctx: &Context<'_>, row: &Value) -> Result<Value> {
    let cycles = ctx
        .rows(PROGRESS)?
        .iter()
        .filter(|r| r["kind"] == "autopilot-cycle" && r["contract_id"] == row["owner_id"])
        .count() as u64;
    let replans = ctx
        .rows(".oms/plan/autopilot-replans.jsonl")?
        .iter()
        .filter(|r| r["owner_id"] == row["owner_id"])
        .count() as u64;
    require(replans <= 1, "ambiguous lifetime replan reservation")?;
    Ok(
        json!({"cycles_spent":cycles,"cycles_remaining":row["contract"]["max_cycles"].as_u64().unwrap_or(0).saturating_sub(cycles),
        "replans_spent":replans,"replans_remaining":1-replans}),
    )
}

pub(crate) fn reserved_recovery(
    ctx: &Context<'_>,
    row: &Value,
    raw: &[u8],
) -> Result<Option<Value>> {
    let rows = ctx.rows(".oms/plan/autopilot-replans.jsonl")?;
    let matching = rows
        .iter()
        .filter(|r| {
            r["owner_id"] == row["owner_id"]
                && r["receipt_sha256"] == value::hash(raw)
                && r["proposal_sha256"] == row["proposal"]["sha256"]
        })
        .collect::<Vec<_>>();
    require(matching.len() <= 1, "ambiguous corrective reservation")?;
    let Some(record) = matching.first() else {
        return Ok(None);
    };
    let packet = evidence(ctx, &record["evidence"])?;
    require(
        packet["owner_id"] == row["owner_id"]
            && packet["spec_sha256"] == row["spec_sha256"]
            && packet["head"] == ctx.git(&["rev-parse", "HEAD"])?
            && packet["plan_sha256"] == value::hash(ctx.store.read(ACTIVE, 8 * MAX_FILE)?),
        "reserved corrective evidence no longer matches this snapshot",
    )?;
    Ok(Some(packet))
}

pub(crate) fn selection_reason(ctx: &Context<'_>) -> Result<&'static str> {
    let plan = ctx.plan_value()?;
    let tasks = plan["tasks"]
        .as_object()
        .ok_or_else(|| Error::new(2, "plan tasks missing"))?;
    if tasks.values().all(|t| t["state"] == "done") {
        return Ok("tasks-exhausted");
    }
    let status = ctx.action(&Request::new(Action::Ready).flag("json"))?;
    require(status.code == 0, "ready observation unavailable")?;
    let ready = value::parse(&status.stdout)?;
    Ok(if ready["capacity_full"] == true {
        "capacity-full"
    } else if ready["backpressure"] == true {
        "review-backpressure"
    } else if tasks
        .values()
        .any(|t| matches!(value::text(t, "state"), "claimed" | "running" | "landing"))
    {
        "active-task-wait"
    } else if tasks.values().any(|t| t["state"] == "blocked") {
        "blocked-tasks"
    } else {
        "plan-not-actionable"
    })
}

pub(crate) fn failure_can_inform_revision(cause: &Value) -> bool {
    cause["phase"] == "acceptance"
        && matches!(
            value::text(&cause["failure"], "code"),
            "verifier_failed" | "unknown_failure"
        )
}

pub(crate) fn worker_contract(task: &Value) -> Result<String> {
    let mut fields = serde_json::Map::new();
    for key in [
        "id",
        "title",
        "brief",
        "depends",
        "allowed_paths",
        "forbidden_paths",
        "verify",
        "role",
        "assignment",
        "contracts",
        "project_contract",
        "covers",
        "requirements",
        "criteria",
    ] {
        fields.insert(key.into(), task.get(key).cloned().unwrap_or(Value::Null));
    }
    Ok(value::hash(value::canonical(&Value::Object(fields))?))
}

/// A returned native call is still insufficient without every registered
/// physical process being drained. Unknown usage/cost is separate from custody;
/// no returned call record, a pending intent or unknown cleanup refuses retry.
fn worker_retry(ctx: &Context<'_>, row: &Value, raw: &[u8], allow_reserved: bool) -> Result<Value> {
    if row["contract"]["retry_known"] != true
        || !row["adaptive"]["wait"].is_null()
        || value::text(&row["diagnostic"], "reason_code") == "parent-stopped"
    {
        return Ok(Value::Null);
    }
    let rows = ctx.rows(PROGRESS)?;
    let Some(call) = rows
        .iter()
        .rev()
        .find(|r| r["kind"] == "autopilot-worker-call" && r["owner_id"] == row["owner_id"])
    else {
        return Ok(Value::Null);
    };
    if call["phase"] != "returned"
        || call["failure"]["retryable"] != true
        || call["exit"] == 0
        || call["exit"] == 130
    {
        return Ok(Value::Null);
    }
    let op = value::text(call, "operation_id");
    let consumed = rows.iter().any(|r| {
        r["kind"] == "autopilot-retry-reserved"
            && r["owner_id"] == row["owner_id"]
            && r["operation_id"] == op
            && r["receipt_sha256"] != value::hash(raw)
    });
    if consumed && !allow_reserved {
        return Ok(Value::Null);
    }
    let plan = ctx.plan_value()?;
    let task = &plan["tasks"][value::text(call, "task_id")];
    if task["state"] != "ready"
        || !value::text(task, "lease_id").is_empty()
        || task["lease_epoch"] != call["lease_epoch"]
        || task["repair_count"].as_u64().unwrap_or(0) != 0
        || call["plan_id"] != plan["plan_id"]
        || call["head"] != ctx.git(&["rev-parse", "HEAD"])?
        || call["task_sha256"] != worker_contract(task)?
        || call["route"]["provider"] != row["providers"]["worker"]
    {
        return Ok(Value::Null);
    }
    let intents = oms_command_telemetry::store::physical::load(&ctx.state, ctx.interrupted)
        .map_err(|e| Error::new(2, e.to_string()))?;
    let usage = oms_command_telemetry::store::usage::load(&ctx.state)
        .map_err(|e| Error::new(2, e.to_string()))?;
    let intents = intents
        .iter()
        .filter(|i| i.identity.operation_id == op)
        .collect::<Vec<_>>();
    let calls = usage
        .calls
        .iter()
        .filter(|c| c.receipt.identity.operation_id == op)
        .collect::<Vec<_>>();
    // Empty physical coverage cannot prove a known completed provider attempt.
    if intents.is_empty() || intents.len() > 64 || intents.len() != calls.len() {
        return Ok(Value::Null);
    }
    let mut proofs = vec![];
    for intent in intents {
        let Some(usage) = calls.iter().find(|c| c.receipt.identity == intent.identity) else {
            return Ok(Value::Null);
        };
        let receipt = &usage.receipt;
        if usage.conflict
            || intent.phase != oms_command_telemetry::store::physical::Phase::Ready
            || intent.usage_ordinal != Some(receipt.ordinal)
            || receipt.cleanup_complete != Some(true)
            || matches!(
                receipt.phase,
                oms_command_telemetry::store::usage::Phase::Pending
                    | oms_command_telemetry::store::usage::Phase::Partial
            )
            || receipt.identity.task_id.as_deref() != call["task_id"].as_str()
            || receipt.identity.provider != value::text(&call["route"], "provider")
        {
            return Ok(Value::Null);
        }
        proofs.push(json!({"call_id":receipt.identity.call_id,"attempt_id":receipt.identity.attempt_id,"ordinal":receipt.ordinal,
            "revision":receipt.revision,"source_sha256":receipt.source_sha256,"cleanup_complete":true}));
    }
    Ok(
        json!({"call":call,"physical_calls":proofs,"task_sha256":worker_contract(task)?,"task_id":call["task_id"],"operation_id":op,
        "instruction":"One same-contract retry under the original retry-known authority; no scope, route or budget increase."}),
    )
}

/// Checked in-memory observation. Only the producer may construct it, and the
/// mutation re-observes it under the same foreground owner before acting.
pub struct CheckedDecision {
    value: Value,
}
impl CheckedDecision {
    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn sha256(&self) -> &str {
        value::text(&self.value, "decision_sha256")
    }
    pub fn allows(&self, action: &str) -> bool {
        self.value["allowed_actions"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == action))
    }
}

pub(crate) fn decision(ctx: &Context<'_>, row: &Value, raw: &[u8]) -> Result<CheckedDecision> {
    crate::receipt::validate(row)?;
    crate::receipt::live(ctx, row)?;
    let plan = ctx.plan_value()?;
    let plan_raw = ctx
        .store
        .optional(ACTIVE, 8 * MAX_FILE)?
        .unwrap_or_default();
    let head = ctx.git(&["rev-parse", "HEAD"])?;
    let tree = ctx.git(&["rev-parse", "HEAD^{tree}"])?;
    let budget = budget(ctx, row)?;
    let mut review = Value::Null;
    if !row["adaptive"]["review"].is_null() {
        review = evidence(ctx, &row["adaptive"]["review"])?;
        check_review_sources(ctx, &review)?;
        review["stale"] = (review["head"] != head).into();
    }
    let mut wait = Value::Null;
    if !row["adaptive"]["wait"].is_null() {
        wait = ctx
            .services
            .external_observation(
                &ctx.repo,
                &ctx.state,
                &row["adaptive"]["wait"]["binding"],
                false,
                ctx.interrupted,
            )
            .unwrap_or_else(|e| json!({"schema":1,"status":"unknown","reason":e.message}));
    }
    let progress = goal_progress(ctx, row)?;
    let cause = ctx
        .rows(PROGRESS)?
        .into_iter()
        .rev()
        .find(|r| r["kind"] == "autopilot-failure" && r["owner_id"] == row["owner_id"])
        .unwrap_or(Value::Null);
    let stage = value::text(row, "stage");
    let reason = value::text(&row["diagnostic"], "reason_code");
    let reserved = reserved_recovery(ctx, row, raw)?;
    let experiment = experiment_planning(row, &wait);
    let retry = if stage == "parked" && budget["cycles_remaining"].as_u64().unwrap_or(0) > 0 {
        worker_retry(ctx, row, raw, false)?
    } else {
        Value::Null
    };
    let enough = budget["cycles_remaining"].as_u64().unwrap_or(0) > 0
        && (budget["replans_remaining"] == 1 || reserved.is_some());
    let can_revise = stage == "parked"
        && enough
        && (review["corrective"] == true
            && review["head"] == head
            && review["plan_sha256"] == value::hash(&plan_raw)
            || !experiment.is_null()
            || reason == "acceptance-gap"
            || ["tasks-exhausted", "stuck"].contains(&reason)
                && failure_can_inform_revision(&cause));
    let can_bind_wait = row["adaptive"]["wait"].is_null()
        || value::digest(value::text(
            &row["adaptive"]["wait"],
            "consumed_result_sha256",
        ));
    let mut allowed = vec!["inspect"];
    let (action, why) = match stage {
        "parked" if reason == "parent-stopped" => (
            "stop",
            "The Parent stopped this owner. Retained waits and results cannot restart it; inspect the preserved handoff",
        ),
        "proposal-review" => (
            "review-proposal",
            "Parent reviews the retained exact proposal and runs its digest-bound continuation",
        ),
        "proposing" => (
            "inspect",
            "Inspect the retained planning intent; an unknown dispatched proposal is never resent",
        ),
        "done" => (
            "stop",
            "Fresh goal acceptance and configured review completed",
        ),
        "parked"
            if !wait.is_null()
                && wait["identity_valid"] == true
                && wait["status"] == "terminal"
                && wait["binding_sha256"] == row["adaptive"]["wait"]["binding_sha256"]
                && value::digest(value::text(&wait, "result_sha256"))
                && row["adaptive"]["wait"]["consumed_result_sha256"] != wait["result_sha256"] =>
        {
            allowed.push("resume");
            (
                "resume",
                "A new exact terminal external result is available; consume it before fresh acceptance",
            )
        }
        "parked" if !retry.is_null() => {
            allowed.push("resume");
            (
                "resume",
                "The retry-known contract permits one exact known-ended worker retry; physical cleanup is proven and the original cycle budget applies",
            )
        }
        _ if reserved.is_some() => {
            allowed.push("propose-revision");
            (
                "propose-revision",
                "Resume the exact reserved corrective intent; its budget remains spent",
            )
        }
        "parked" if can_revise => {
            allowed.push("propose-revision");
            (
                "propose-revision",
                "Review the preserved failure evidence and propose a bounded corrective tranche",
            )
        }
        "parked" => {
            if can_bind_wait {
                allowed.push("wait");
            }
            (
                "inspect",
                "Inspect the preserved cause or bind a canonical external wait; unchanged failures are not retried",
            )
        }
        _ => ("continue", "Continue only the existing approved contract"),
    };
    if stage != "done" {
        allowed.push("stop");
    }
    if stage == "parked"
        && reason != "parent-stopped"
        && can_bind_wait
        && !allowed.contains(&"wait")
    {
        allowed.push("wait");
    }
    let mut commands = vec![];
    if action == "review-proposal" || action == "continue" {
        if let Some(argv) = crate::receipt::approved_continuation(row) {
            commands.push(json!({"argv":argv}));
        }
    } else if allowed.contains(&action) {
        commands.push(json!({"argv":["oms","autopilot","decide","--expected-decision-sha256","<decision-sha256>","--action",action]}));
    }
    let mut value = json!({"schema":1,"kind":"autopilot-next-action","owner_id":row["owner_id"],
        "receipt_sha256":value::hash(raw),"plan_sha256":value::hash(plan_raw),"head":head,"tree":tree,
        "spec_sha256":row["spec_sha256"],"stage":stage,"action":action,"allowed_actions":allowed,
        "reason_code":if reason.is_empty(){stage}else{reason},"reason":why,"budget":budget,
        "review":review,"wait":wait,"experiment":experiment,"retry":retry,"progress":progress,"cause":cause,"proposal":row["proposal"],"plan_id":plan["plan_id"],
        "commands":commands});
    require(
        ctx.git(&["rev-parse", "HEAD"])? == value["head"]
            && ctx.git(&["rev-parse", "HEAD^{tree}"])? == value["tree"]
            && value::hash(
                ctx.store
                    .optional(ACTIVE, 8 * MAX_FILE)?
                    .unwrap_or_default(),
            ) == value["plan_sha256"]
            && ctx.store.read(RECEIPT, MAX_EVIDENCE)? == raw,
        "autopilot decision sources changed during observation",
    )?;
    let hash = value::hash(value::canonical(&value)?);
    value["decision_sha256"] = hash.clone().into();
    if let Some(commands) = value["commands"].as_array_mut() {
        for command in commands {
            if let Some(argv) = command["argv"].as_array_mut() {
                for arg in argv {
                    if arg == "<decision-sha256>" {
                        *arg = hash.clone().into();
                    }
                }
            }
        }
    }
    Ok(CheckedDecision { value })
}

/// Only a consumed, still current native experimental result may propose work.
/// Its missing command is planning input for the existing Parent, not a human
/// permission requirement and not permission to replay another cell's argv.
fn experiment_planning(row: &Value, observation: &Value) -> Value {
    let next = &observation["experiment"]["next_action"];
    if observation["source"] != "experiment"
        || observation["identity_valid"] != true
        || observation["status"] != "terminal"
        || !value::digest(value::text(observation, "result_sha256"))
        || observation["binding_sha256"] != row["adaptive"]["wait"]["binding_sha256"]
        || observation["result_sha256"] != row["adaptive"]["wait"]["consumed_result_sha256"]
        || !matches!(
            value::text(next, "kind"),
            "input-required" | "review-hypothesis"
        )
    {
        return Value::Null;
    }
    json!({"binding":row["adaptive"]["wait"]["binding"],"binding_sha256":observation["binding_sha256"],
        "result_sha256":observation["result_sha256"],"next_action":next,
        "instruction":"Plan only within the frozen project envelope, original experiment budget and preregistration. Propose the missing command/check or a separately reviewed next hypothesis. Never mutate the registered experiment or infer another arm/seed command from prior argv."})
}
pub fn observe(
    repo: &Path,
    state: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    let ctx = Context::new(repo.into(), state.into(), services, stop)?;
    let raw = ctx.store.read(RECEIPT, MAX_EVIDENCE)?;
    let row = value::parse(&raw)?;
    Ok(decision(&ctx, &row, &raw)?.value)
}

/// Coverage declarations select evidence to inspect; they are never themselves
/// proofs. The host's canonical criterion projection is independently checked.
pub(crate) fn goal_progress(ctx: &Context<'_>, row: &Value) -> Result<Value> {
    let spec = planning::contract(ctx)?;
    require(
        spec.criteria.len() <= 128,
        "goal criterion observation exceeds 128 criteria",
    )?;
    let listed = ctx.action(&Request::new(Action::List).flag("json"))?;
    require(
        listed.code == 0,
        "canonical completion observation unavailable",
    )?;
    let listed = value::parse(&listed.stdout)?;
    let tasks = listed["tasks"]
        .as_array()
        .ok_or_else(|| Error::new(2, "canonical tasks unavailable"))?;
    let observed = ctx
        .services
        .discovery_section(&ctx.repo, "acceptance", ctx.interrupted)
        .unwrap_or(Value::Null);
    require(
        value::canonical(&observed)?.len() <= MAX_FILE,
        "criterion observation exceeds bound",
    )?;
    let mut criteria = vec![];
    for (id, statement) in &spec.criteria {
        let linked = tasks
            .iter()
            .filter(|t| {
                t["covers"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|c| c == id))
            })
            .collect::<Vec<_>>();
        let completed = linked
            .iter()
            .filter(|t| {
                t["state"] == "done"
                    && t["completion_verified"] != false
                    && (t["completion_receipt"].is_object() || t["landing"].is_object())
            })
            .map(|t| t["id"].clone())
            .collect::<Vec<_>>();
        let digest = value::hash(format!("project\0{statement}"));
        let canonical = observed["criteria"].as_array().and_then(|rows| {
            rows.iter().find(|c| {
                c["id"] == *id && c["source"] == "project" && c["criterion_digest"] == digest
            })
        });
        let proof = canonical
            .and_then(|c| c["evidence"].as_array())
            .map(|rows| {
                rows.iter()
                    .filter(|e| {
                        e["status"] == "verified"
                            && e["stale"] == false
                            && e["signal"] == "execution_feedback"
                    })
                    .take(20)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let verified = canonical.is_some_and(|c| c["status"] == "verified") && !proof.is_empty();
        criteria.push(json!({"id":id,"criterion_digest":digest,"state":if verified{"verified"}else{"unverified"},"completed_tasks":completed,
            "claimed_tasks":linked.iter().map(|t|t["id"].clone()).collect::<Vec<_>>(),"evidence":proof}));
    }
    let results = ctx
        .rows(PROGRESS)?
        .into_iter()
        .filter(|r| {
            r["kind"] == "autopilot-external-result"
                && r["owner_id"] == row["owner_id"]
                && r["progress"] == true
        })
        .filter_map(|r| {
            r["observation"]["progress_sha256"]
                .as_str()
                .filter(|s| value::digest(s))
                .map(str::to_owned)
        })
        .collect::<BTreeSet<_>>();
    Ok(json!({"criteria":criteria,"verified_results":results,"authority":"observation-only"}))
}

/// Persisted, owner-bound progress includes exact sources alongside a separate
/// transient-normalized signal. Git churn and cancellation never reset it.
pub(crate) fn cycle(ctx: &Context<'_>, plan: &Value) -> Result<Value> {
    let rows = ctx.rows(PROGRESS)?;
    let acceptance = rows
        .iter()
        .rev()
        .find(|r| r["kind"] == "acceptance" && r["run_id"] == ctx.run_id)
        .ok_or_else(|| Error::new(2, "acceptance receipt missing for adaptive progress"))?;
    let source = value::text(acceptance, "output_sha256_full");
    require(
        value::digest(source),
        "acceptance source identity unavailable",
    )?;
    let log = ctx.store.optional(
        format!(".oms/plan/acceptance/{}.log", &source[..16]),
        MAX_FILE,
    )?;
    let signature = log
        .as_ref()
        .map(|r| failure_signature(&String::from_utf8_lossy(r)))
        .unwrap_or_else(|| source.to_owned());
    let receipt = ctx
        .store
        .optional(RECEIPT, MAX_EVIDENCE)?
        .map(|r| value::parse(&r))
        .transpose()?;
    let facts = if let Some(row) = receipt.as_ref().filter(|r| r["owner_id"] == ctx.owner_id) {
        goal_progress(ctx, row)?
    } else {
        json!({"criteria":[],"verified_results":[]})
    };
    let signals = meaningful_signals(&facts);
    let progress_key = value::hash(value::canonical(&json!(signals))?);
    let owner = if ctx.owner_id.is_empty() {
        ctx.run_id.as_str()
    } else {
        ctx.owner_id.as_str()
    };
    let previous = rows.iter().rev().find(|r| {
        r["kind"] == "autopilot-goal-progress"
            && r["owner_id"] == owner
            && r["plan_id"] == plan["plan_id"]
            && r["accept_sha256"] == acceptance["accept_sha256_full"]
    });
    let mut seen = previous
        .and_then(|p| p["seen_signals"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut seen_failures = previous
        .and_then(|p| p["seen_failure_signatures"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if let Some(previous) = previous {
        let old = value::text(previous, "signature_sha256");
        if value::digest(old) {
            seen_failures.insert(old.to_owned());
        }
    }
    let stalled = previous
        .map(|p| {
            if seen_failures.contains(&signature) && signals.is_subset(&seen) {
                p["stalled_cycles"].as_u64().unwrap_or(0).saturating_add(1)
            } else {
                0
            }
        })
        .unwrap_or(0);
    seen.extend(signals);
    seen_failures.insert(signature.clone());
    require(
        seen_failures.len() <= 256,
        "goal failure signature bound exceeded",
    )?;
    require(seen.len() <= 2048, "goal progress signal bound exceeded")?;
    let observation = json!({"schema":1,"kind":"autopilot-goal-progress","owner_id":owner,
        "run_id":ctx.run_id,"cycle":ctx.cycle,"plan_id":plan["plan_id"],"head":ctx.git(&["rev-parse","HEAD"])?,
        "accept_sha256":acceptance["accept_sha256_full"],"source_sha256":source,
        "observed_log_sha256":log.as_ref().map(value::hash),"signature_sha256":signature,
        "progress_sha256":progress_key,"progress":facts,"seen_signals":seen,"seen_failure_signatures":seen_failures,"stalled_cycles":stalled,"ts":value::now()});
    let _lock = ctx.lock(PROGRESS)?;
    ctx.append(PROGRESS, &observation)?;
    Ok(observation)
}

fn meaningful_signals(facts: &Value) -> BTreeSet<String> {
    let mut signals = BTreeSet::new();
    if let Some(criteria) = facts["criteria"].as_array() {
        for c in criteria {
            let id = value::text(c, "id");
            if c["state"] == "verified" {
                signals.insert(format!("criterion:{id}"));
            }
            if let Some(tasks) = c["completed_tasks"].as_array() {
                for task in tasks {
                    if let Some(task) = task.as_str() {
                        signals.insert(format!("task:{id}:{task}"));
                    }
                }
            }
        }
    }
    if let Some(results) = facts["verified_results"].as_array() {
        for result in results {
            if let Some(result) = result.as_str() {
                signals.insert(format!("result:{result}"));
            }
        }
    }
    signals
}

pub(crate) fn milestone_ready(ctx: &Context<'_>, row: &Value) -> Result<bool> {
    let Some(milestones) = row["contract"]["milestones"].as_array() else {
        return Ok(false);
    };
    let index = row["adaptive"]["milestone_index"].as_u64().unwrap_or(0) as usize;
    let milestone = milestones
        .get(index)
        .ok_or_else(|| Error::new(2, "milestone cursor changed"))?;
    let progress = goal_progress(ctx, row)?;
    let criteria = progress["criteria"]
        .as_array()
        .ok_or_else(|| Error::new(2, "criterion evidence unavailable"))?;
    let ready = milestone["criteria"].as_array().is_some_and(|wanted| {
        wanted.iter().all(|id| {
            criteria.iter().any(|c| {
                c["id"] == *id
                    && c["state"] == "verified"
                    && c["completed_tasks"]
                        .as_array()
                        .is_some_and(|a| !a.is_empty())
                    && c["claimed_tasks"].as_array().is_some_and(|a| {
                        a.len() == c["completed_tasks"].as_array().map_or(0, Vec::len)
                    })
            })
        })
    });
    Ok(ready)
}

/// An admitted corrective/milestone tranche is required work even if the old
/// global check passes. Its first attempt spends the same lifetime budget;
/// admitting it does not erase prior stalls or failures.
pub(crate) fn tranche(ctx: &Context<'_>) -> Result<Value> {
    if ctx.owner_id.is_empty() {
        return Ok(Value::Null);
    }
    let raw = ctx.store.read(RECEIPT, MAX_EVIDENCE)?;
    let row = value::parse(&raw)?;
    crate::receipt::validate(&row)?;
    require(
        row["owner_id"] == ctx.owner_id,
        "goal tranche owner changed",
    )?;
    let mandatory =
        row["contract"]["milestones"].is_array() || !row["adaptive"]["recovery"].is_null();
    let decision = if !row["adaptive"]["last_decision"].is_null() {
        evidence(ctx, &row["adaptive"]["last_decision"])?
    } else {
        Value::Null
    };
    let retry_sha = value::text(&row["adaptive"]["last_decision"], "sha256");
    let retry_unspent = !decision["retry"].is_null()
        && !ctx.rows(PROGRESS)?.iter().any(|r| {
            r["kind"] == "autopilot-cycle"
                && r["contract_id"] == ctx.owner_id
                && r["retry_sha256"] == retry_sha
        });
    if retry_unspent {
        let plan = ctx.plan_value()?;
        let task = &plan["tasks"][value::text(&decision["retry"], "task_id")];
        require(
            task["state"] == "ready"
                && worker_contract(task)? == decision["retry"]["task_sha256"]
                && ctx.git(&["rev-parse", "HEAD"])? == decision["head"],
            "reviewed worker retry source changed",
        )?;
        let fresh = worker_retry(ctx, &row, &raw, true)?;
        require(
            !fresh.is_null()
                && fresh["call"] == decision["retry"]["call"]
                && fresh["physical_calls"].as_array().map(Vec::len)
                    == decision["retry"]["physical_calls"].as_array().map(Vec::len)
                && fresh["physical_calls"].as_array().is_some_and(|rows| {
                    rows.iter().all(|p| {
                        decision["retry"]["physical_calls"]
                            .as_array()
                            .is_some_and(|old| {
                                old.iter().any(|o| {
                                    o["call_id"] == p["call_id"]
                                        && o["attempt_id"] == p["attempt_id"]
                                        && o["ordinal"] == p["ordinal"]
                                })
                            })
                    })
                }),
            "reviewed worker retry cleanup or physical call set changed",
        )?;
    }
    if !mandatory && !retry_unspent {
        return Ok(Value::Null);
    }
    let reference = &row["proposal"];
    let proposal = ctx.bytes(Path::new(value::text(reference, "path")), MAX_FILE)?;
    require(
        value::hash(&proposal) == reference["sha256"],
        "required tranche proposal changed",
    )?;
    let proposal = value::parse(&proposal)?;
    let tasks = proposal["tasks"]
        .as_array()
        .ok_or_else(|| Error::new(2, "required tranche lacks tasks"))?;
    require(
        !tasks.is_empty() && tasks.len() <= 12,
        "invalid required tranche",
    )?;
    let listed = ctx.action(&Request::new(Action::List).flag("json"))?;
    require(listed.code == 0, "required tranche completion unavailable")?;
    let listed = value::parse(&listed.stdout)?;
    let canonical = listed["tasks"]
        .as_array()
        .ok_or_else(|| Error::new(2, "canonical tasks unavailable"))?;
    let pending = tasks.iter().any(|t| {
        !canonical.iter().any(|c| {
            c["id"] == t["id"] && c["state"] == "done" && c["completion_verified"] != false
        })
    });
    let spent = ctx.rows(PROGRESS)?.iter().any(|r| {
        r["kind"] == "autopilot-cycle"
            && r["contract_id"] == ctx.owner_id
            && r["proposal_sha256"] == reference["sha256"]
    });
    let next_milestone = row["contract"]["milestones"].as_array().is_some_and(|m| {
        row["adaptive"]["milestone_index"].as_u64().unwrap_or(0) + 1 < m.len() as u64
    });
    Ok(
        json!({"mandatory":true,"pending":pending,"next_milestone":next_milestone,
        "first_attempt":!spent || retry_unspent,"proposal_sha256":reference["sha256"],
        "retry_task_id":if retry_unspent {decision["retry"]["task_id"].clone()}else{Value::Null},"retry_sha256":if retry_unspent{retry_sha}else{""},
        "retry_route":if retry_unspent{decision["retry"]["call"]["route"].clone()}else{Value::Null}}),
    )
}
