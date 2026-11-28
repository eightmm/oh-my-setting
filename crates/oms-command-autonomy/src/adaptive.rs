//! Evidence-bound decisions over the existing Plan and autopilot owner.
//! Observations and model claims never grant task, verification or publication authority.
use crate::storage::{ACTIVE, Context, MAX_FILE, PROGRESS, RECEIPT};
use crate::{Error, Result, Services, planning, require, value};
use oms_command_plan::{Action, Request};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
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

fn string_list(value: &Value) -> Option<Vec<String>> {
    if value.is_null() {
        return Some(Vec::new());
    }
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn scope_root(normalized: &str) -> String {
    let mut path = normalized.to_owned();
    if let Some(at) = path.find(['*', '?', '[']) {
        path = path[..at]
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_owned())
            .unwrap_or_default();
    }
    path.trim_matches('/').to_owned()
}

fn bounded_allowed_roots(value: &Value) -> Option<Vec<String>> {
    let items = value.as_array()?;
    if items.is_empty() {
        return None;
    }
    let mut roots = Vec::with_capacity(items.len());
    for item in items {
        let raw = item.as_str()?;
        let normalized = oms_contract_markdown::scope::normalize(raw).ok()?;
        if normalized == "." {
            return None;
        }
        let root = scope_root(&normalized);
        if root.is_empty() {
            return None;
        }
        roots.push(root);
    }
    Some(roots)
}

fn paths_overlap(failed_roots: &[String], touched: &str) -> bool {
    failed_roots.iter().any(|root| {
        root.is_empty()
            || touched.is_empty()
            || root == touched
            || touched.starts_with(&format!("{root}/"))
            || root.starts_with(&format!("{touched}/"))
    })
}

fn diff_tree_paths(
    ctx: &Context<'_>,
    parent: &str,
    commit: &str,
) -> Result<Option<BTreeSet<String>>> {
    let raw = ctx.plan()?.git().run(&[
        "diff-tree",
        "--no-commit-id",
        "--no-renames",
        "--name-only",
        "-r",
        "-z",
        parent,
        commit,
    ])?;
    let mut paths = BTreeSet::new();
    for entry in raw.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let Ok(text) = std::str::from_utf8(entry) else {
            return Ok(None);
        };
        let Ok(normalized) = oms_contract_markdown::scope::normalize(text) else {
            return Ok(None);
        };
        if normalized == "." {
            return Ok(None);
        }
        paths.insert(normalized);
    }
    if paths.is_empty() {
        return Ok(None);
    }
    Ok(Some(paths))
}

fn linear_commit_chain(
    ctx: &Context<'_>,
    from_head: &str,
    to_head: &str,
) -> Result<Option<Vec<(String, String)>>> {
    let plan_handle = ctx.plan()?;
    let git = plan_handle.git();
    if git.ancestor(from_head, to_head).is_err() {
        return Ok(None);
    }
    let raw = git.text(&["rev-list", "--parents", &format!("{from_head}..{to_head}")])?;
    let mut parents = BTreeMap::new();
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let Some(commit) = parts.next() else {
            return Ok(None);
        };
        let Some(parent) = parts.next() else {
            return Ok(None);
        };
        if parts.next().is_some()
            || !value::commit(commit)
            || !value::commit(parent)
            || parents
                .insert(commit.to_owned(), parent.to_owned())
                .is_some()
        {
            return Ok(None);
        }
    }
    if parents.is_empty() {
        return Ok(None);
    }
    let mut chain = Vec::with_capacity(parents.len());
    let mut cursor = to_head;
    while cursor != from_head {
        let Some(parent) = parents.get(cursor) else {
            return Ok(None);
        };
        chain.push((cursor.to_owned(), parent.clone()));
        if chain.len() > parents.len() {
            return Ok(None);
        }
        cursor = parent;
    }
    if chain.len() != parents.len() {
        return Ok(None);
    }
    Ok(Some(chain))
}

/// Accept `from_head != to_head` only when every intervening commit is a linear
/// `commit-intent` (`landed` + `committed` joined by `intent_id`) linked to an
/// `autopilot-cycle` `run_id` of `owner_id`, and every touched path and committed
/// task resource is strictly disjoint from `task`'s bounded `allowed_paths`/`resources`.
fn head_matches_or_verified_progression(
    ctx: &Context<'_>,
    rows: &[Value],
    owner_id: &str,
    plan: &Value,
    task: &Value,
    from_head: &str,
    to_head: &str,
) -> Result<bool> {
    if !value::commit(from_head) || !value::commit(to_head) {
        return Ok(false);
    }
    if from_head == to_head {
        return Ok(true);
    }
    let Some(failed_roots) = bounded_allowed_roots(&task["allowed_paths"]) else {
        return Ok(false);
    };
    let Some(failed_resources) = string_list(&task["resources"]) else {
        return Ok(false);
    };
    let Some(chain) = linear_commit_chain(ctx, from_head, to_head)? else {
        return Ok(false);
    };
    let owner_cycle_runs = rows
        .iter()
        .filter(|r| r["kind"] == "autopilot-cycle" && r["contract_id"] == owner_id)
        .map(|r| value::text(r, "run_id"))
        .filter(|run_id| !run_id.is_empty())
        .collect::<BTreeSet<_>>();
    if owner_cycle_runs.is_empty() {
        return Ok(false);
    }
    let mut abandoned = BTreeSet::new();
    let mut landed_by_commit: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    let mut committed_by_intent: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for row in rows.iter().filter(|r| r["kind"] == "commit-intent") {
        let intent_id = value::text(row, "intent_id");
        if intent_id.is_empty() {
            continue;
        }
        match value::text(row, "phase") {
            "abandoned" => {
                abandoned.insert(intent_id);
            }
            "landed" => {
                let commit = value::text(row, "commit");
                if value::commit(commit) {
                    landed_by_commit.entry(commit).or_default().push(row);
                }
            }
            "committed" => {
                committed_by_intent.entry(intent_id).or_default().push(row);
            }
            _ => {}
        }
    }
    let repo_str = ctx.repo.to_string_lossy();
    let plan_handle = ctx.plan()?;
    let git = plan_handle.git();
    for (commit, parent) in chain {
        let Some([landed]) = landed_by_commit.get(commit.as_str()).map(Vec::as_slice) else {
            return Ok(false);
        };
        let intent_id = value::text(landed, "intent_id");
        if intent_id.is_empty() || abandoned.contains(intent_id) {
            return Ok(false);
        }
        let Some([committed]) = committed_by_intent.get(intent_id).map(Vec::as_slice) else {
            return Ok(false);
        };
        let expected_tree = value::text(landed, "expected_tree");
        if value::text(landed, "worktree") != repo_str.as_ref()
            || value::text(committed, "worktree") != repo_str.as_ref()
            || value::text(landed, "base_sha") != parent
            || value::text(committed, "base_sha") != parent
            || committed["run_id"] != landed["run_id"]
            || committed["task_id"] != landed["task_id"]
            || committed["paths"] != landed["paths"]
            || !value::commit(expected_tree)
            || git.text(&["rev-parse", &format!("{commit}^{{tree}}")])? != expected_tree
        {
            return Ok(false);
        }
        let run_id = value::text(committed, "run_id");
        if !owner_cycle_runs.contains(run_id) {
            return Ok(false);
        }
        let Some(raw_intent_paths) = string_list(&committed["paths"]) else {
            return Ok(false);
        };
        if raw_intent_paths.is_empty() {
            return Ok(false);
        }
        let mut intent_paths = BTreeSet::new();
        for raw in &raw_intent_paths {
            let Ok(normalized) = oms_contract_markdown::scope::normalize(raw) else {
                return Ok(false);
            };
            if normalized == "." {
                return Ok(false);
            }
            intent_paths.insert(normalized);
        }
        let Some(git_paths) = diff_tree_paths(ctx, &parent, &commit)? else {
            return Ok(false);
        };
        if git_paths != intent_paths
            || git_paths
                .iter()
                .any(|touched| paths_overlap(&failed_roots, touched))
        {
            return Ok(false);
        }
        let committed_task_id = value::text(committed, "task_id");
        let Some(committed_task) = plan["tasks"]
            .get(committed_task_id)
            .filter(|t| t.is_object())
        else {
            return Ok(false);
        };
        let Some(committed_resources) = string_list(&committed_task["resources"]) else {
            return Ok(false);
        };
        if committed_resources
            .iter()
            .any(|res| failed_resources.contains(res))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn parked_run_id<'a>(rows: &'a [Value], owner_id: &str) -> Option<&'a str> {
    rows.iter()
        .rev()
        .find(|r| {
            (r["kind"] == "terminal" && r["owner_id"] == owner_id)
                || (r["kind"] == "autopilot-cycle" && r["contract_id"] == owner_id)
                || (r["kind"] == "autopilot-worker-call" && r["owner_id"] == owner_id)
        })
        .map(|r| value::text(r, "run_id"))
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
    let owner_id = value::text(row, "owner_id");
    let rows = ctx.rows(PROGRESS)?;
    let Some(run_id) = parked_run_id(&rows, owner_id) else {
        return Ok(Value::Null);
    };
    let plan = ctx.plan_value()?;
    let current_head = ctx.git(&["rev-parse", "HEAD"])?;
    let receipt_sha = value::hash(raw);
    let intents = oms_command_telemetry::store::physical::load(&ctx.state, ctx.interrupted)
        .map_err(|e| Error::new(2, e.to_string()))?;
    let usage = oms_command_telemetry::store::usage::load(&ctx.state)
        .map_err(|e| Error::new(2, e.to_string()))?;

    for call in rows.iter().rev().filter(|r| {
        r["kind"] == "autopilot-worker-call"
            && r["owner_id"] == owner_id
            && r["run_id"] == run_id
            && r["phase"] == "returned"
    }) {
        if call["failure"]["retryable"] != true || call["exit"] == 0 || call["exit"] == 130 {
            continue;
        }
        let op = value::text(call, "operation_id");
        if op.is_empty()
            || rows
                .iter()
                .filter(|r| {
                    r["kind"] == "autopilot-worker-call"
                        && r["owner_id"] == owner_id
                        && r["operation_id"] == op
                        && r["phase"] == "returned"
                })
                .count()
                != 1
        {
            continue;
        }
        let consumed = rows.iter().any(|r| {
            r["kind"] == "autopilot-retry-reserved"
                && r["owner_id"] == owner_id
                && r["operation_id"] == op
                && r["receipt_sha256"] != receipt_sha
        });
        if consumed && !allow_reserved {
            continue;
        }
        let task_id = value::text(call, "task_id");
        let Some(task) = plan["tasks"].get(task_id).filter(|t| t.is_object()) else {
            continue;
        };
        let task_sha = worker_contract(task)?;
        if task["state"] != "ready"
            || !value::text(task, "lease_id").is_empty()
            || task["lease_epoch"] != call["lease_epoch"]
            || task["repair_count"].as_u64().unwrap_or(0) != 0
            || call["plan_id"] != plan["plan_id"]
            || call["task_sha256"] != task_sha
            || call["route"]["provider"] != row["providers"]["worker"]
            || !head_matches_or_verified_progression(
                ctx,
                &rows,
                owner_id,
                &plan,
                task,
                value::text(call, "head"),
                &current_head,
            )?
        {
            continue;
        }
        let op_intents = intents
            .iter()
            .filter(|i| i.identity.operation_id == op)
            .collect::<Vec<_>>();
        let op_calls = usage
            .calls
            .iter()
            .filter(|c| c.receipt.identity.operation_id == op)
            .collect::<Vec<_>>();
        // Empty physical coverage cannot prove a known completed provider attempt.
        if op_intents.is_empty() || op_intents.len() > 64 || op_intents.len() != op_calls.len() {
            continue;
        }
        let mut proofs = vec![];
        let mut valid_physical = true;
        for intent in op_intents {
            let Some(usage_entry) = op_calls
                .iter()
                .find(|c| c.receipt.identity == intent.identity)
            else {
                valid_physical = false;
                break;
            };
            let receipt = &usage_entry.receipt;
            if usage_entry.conflict
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
                valid_physical = false;
                break;
            }
            proofs.push(json!({"call_id":receipt.identity.call_id,"attempt_id":receipt.identity.attempt_id,"ordinal":receipt.ordinal,
                "revision":receipt.revision,"source_sha256":receipt.source_sha256,"cleanup_complete":true}));
        }
        if !valid_physical {
            continue;
        }
        return Ok(
            json!({"call":call,"physical_calls":proofs,"task_sha256":task_sha,"task_id":call["task_id"],"operation_id":op,
            "instruction":"One same-contract retry under the original retry-known authority; no scope, route or budget increase."}),
        );
    }
    Ok(Value::Null)
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
    let plan = ctx.plan()?;
    let snapshot = plan.load()?;
    let canonical = snapshot.ordered()?;
    let mut completed = BTreeSet::new();
    let mut local = Vec::new();
    for task in &canonical {
        if !snapshot.verified_done(task)
            || task.get("completion_verified") == &oms_command_plan::json::Value::Bool(false)
        {
            continue;
        }
        if task.get("completion_receipt").object().is_ok() || task.get("landing").object().is_ok() {
            completed.insert(task.text("id").to_owned());
        } else if task
            .get("covers")
            .strings()
            .is_ok_and(|ids| ids.iter().any(|id| spec.criteria.contains_key(*id)))
        {
            local.push(task.text("id"));
        }
    }
    // Ordinary patch-land Finish retains its proof in the landing ledger,
    // rather than the research/landed-commit completion objects on a task.
    if !local.is_empty() {
        let rows = plan.patch_landing_rows()?;
        let ledger = crate::goal::LandingLedger::new(&rows);
        for id in local {
            match crate::goal::completed_landing_from_snapshot(
                ctx, &plan, &snapshot, &ledger, id, None,
            ) {
                Ok(_) => {
                    completed.insert(id.to_owned());
                }
                Err(e) if e.code == 130 => return Err(e),
                Err(_) => {}
            }
        }
    }
    let tasks = canonical
        .iter()
        .map(value::from_plan)
        .collect::<Result<Vec<_>>>()?;
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
        let completed_tasks = linked
            .iter()
            .filter(|t| completed.contains(value::text(t, "id")))
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
        criteria.push(json!({"id":id,"criterion_digest":digest,"state":if verified{"verified"}else{"unverified"},"completed_tasks":completed_tasks,
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
        let task_id = value::text(&decision["retry"], "task_id");
        let task = &plan["tasks"][task_id];
        let rows = ctx.rows(PROGRESS)?;
        let current_head = ctx.git(&["rev-parse", "HEAD"])?;
        require(
            task.is_object()
                && task["state"] == "ready"
                && worker_contract(task)? == decision["retry"]["task_sha256"]
                && head_matches_or_verified_progression(
                    ctx,
                    &rows,
                    value::text(&row, "owner_id"),
                    &plan,
                    task,
                    value::text(&decision, "head"),
                    &current_head,
                )?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::RECEIPT;
    use oms_command_plan::services::{GroupMembership, GroupResolver};
    use oms_command_telemetry::store::{physical, usage};
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt;
            let path = std::env::temp_dir().join(value::random("oms-adaptive-").unwrap());
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(path.join(".oms/plan"))
                .unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
        fn plan(&self, name: &str) -> PathBuf {
            self.0.join(".oms/plan").join(name)
        }
        fn write_receipt(&self, row: &Value) {
            std::fs::write(self.plan("autopilot-run.json"), value::pretty(row).unwrap()).unwrap();
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn private(path: &Path, body: &[u8]) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(body).unwrap();
    }

    #[derive(Default)]
    struct TestHost;
    impl oms_command_plan::Services for TestHost {
        fn group_membership_resolver(&self) -> Option<GroupResolver> {
            fn resolve(
                gid: u32,
            ) -> oms_command_plan::Result<oms_command_plan::services::GroupMembership> {
                let uid = rustix::process::getuid().as_raw();
                Ok(GroupMembership {
                    gid,
                    explicit_member_uids: vec![uid],
                    primary_member_uids: vec![uid],
                })
            }
            Some(resolve)
        }
    }
    impl Services for TestHost {
        fn state_repository(&self, work: &Path, _: &AtomicBool) -> Result<PathBuf> {
            Ok(work.into())
        }
        fn discovery_section(&self, _: &Path, _: &str, _: &AtomicBool) -> Result<Value> {
            Ok(json!({"criteria":[]}))
        }
    }

    fn setup_parallel_retry(tasks: &[(&str, &[&str], &[&str])]) -> (Dir, Value, Context<'static>) {
        static HOST: TestHost = TestHost;
        static STOP: AtomicBool = AtomicBool::new(false);
        let dir = Dir::new();
        git(&dir.0, &["init", "--quiet", "-b", "main"]);
        private(&dir.0.join("tests/check.sh"), b"exit 1\n");
        private(&dir.0.join("a/seed.txt"), b"seed a\n");
        private(&dir.0.join("b/seed.txt"), b"seed b\n");
        private(&dir.0.join("c/seed.txt"), b"seed c\n");
        private(
            &dir.0.join("PROJECT.md"),
            b"# Project\n\n## Status\n- State: confirmed\n\n## Goal\n- Goal: Check parallel retry\n\n## Scope\n- Allowed: a, b, c\n\n## Acceptance\n- Behavior one is checked\n\n## Verification\n- Required checks: bash tests/check.sh\n- Required check files: tests/check.sh\n",
        );
        std::fs::write(dir.0.join(".git/info/exclude"), ".oms/\n").unwrap();
        git(&dir.0, &["add", "PROJECT.md", "a", "b", "c", "tests"]);
        git(&dir.0, &["commit", "--quiet", "-m", "initial"]);
        let spec = value::hash(std::fs::read(dir.0.join("PROJECT.md")).unwrap());
        let base = git(&dir.0, &["rev-parse", "HEAD"]);
        let branch = format!("oms/autopilot-{}", &spec[..12]);
        git(&dir.0, &["switch", "--quiet", "-c", &branch]);
        let proposal = value::pretty(&json!({
            "tasks": tasks.iter().map(|(id, ..)| json!({"id": id})).collect::<Vec<_>>()
        }))
        .unwrap();
        let proposal_sha = value::hash(&proposal);
        let proposal_path = dir.plan(&format!("approved-proposal-{proposal_sha}.json"));
        private(&proposal_path, &proposal);
        let mut row = json!({
            "schema": 1,
            "kind": "autopilot-run",
            "stage": "parked",
            "repo": dir.0,
            "spec_sha256": spec,
            "providers": {"planner": "claude", "worker": "codex", "reviewer": "claude"},
            "routing": {
                "provider_timeout": "5m",
                "planner": {"model": "", "fallback_model": "", "reasoning_effort": "auto", "timeout": "15m"},
                "worker": {"model": "gpt-6-sol", "fallback_model": "", "reasoning_effort": "auto", "timeout": "30m"},
                "reviewer": {"model": "", "fallback_model": "", "reasoning_effort": "auto", "timeout": "20m"}
            },
            "contract": {
                "allowed": "a,b,c",
                "base": "main",
                "base_sha": base,
                "remote": "origin",
                "max_cycles": 5,
                "max_writers": 2,
                "initial_tasks": 6,
                "replan_tasks": 2,
                "auto_repair": false,
                "retry_known": true,
                "review_mode": "off",
                "draft_pr": false,
                "allow_verifier_change": false,
                "allow_floor_change": "",
                "collaboration": "off"
            },
            "proposal": {"path": proposal_path, "sha256": proposal_sha},
            "branch": branch,
            "owner_id": format!("owner_{}", "1".repeat(32)),
            "updated": "2026-11-29T00:00:00Z"
        });
        state(&mut row);
        dir.write_receipt(&row);
        let mut ctx = Context::new(dir.0.clone(), dir.0.clone(), &HOST, &STOP).unwrap();
        ctx.owner_id = value::text(&row, "owner_id").into();
        assert_eq!(
            ctx.action(
                &Request::new(Action::Init)
                    .with("goal", "Check parallel retry")
                    .with("accept", "bash tests/check.sh"),
            )
            .unwrap()
            .code,
            0
        );
        private(&dir.plan("fixture-brief"), b"Implement scoped task.");
        for (id, allowed, resources) in tasks {
            let mut req = Request::new(Action::Add)
                .with("id", *id)
                .with("title", *id)
                .with("brief-file", dir.plan("fixture-brief"))
                .with("allowed", allowed.join(","))
                .with("verify", "bash tests/check.sh");
            if !resources.is_empty() {
                req = req.with("resources", resources.join(","));
            }
            assert_eq!(ctx.action(&req).unwrap().code, 0);
        }
        (dir, row, ctx)
    }

    fn append_worker_call(
        ctx: &Context<'_>,
        owner_id: &str,
        run_id: &str,
        task_id: &str,
        head: &str,
        exit: i32,
        retryable: bool,
    ) -> String {
        let plan = ctx.plan_value().unwrap();
        let task = &plan["tasks"][task_id];
        let op = value::random("op_").unwrap();
        let lease_epoch = task["lease_epoch"].clone();
        ctx.append(
            PROGRESS,
            &json!({
                "schema": 1,
                "kind": "autopilot-cycle",
                "contract_id": owner_id,
                "run_id": run_id,
                "cycle": 1,
                "task_id": task_id,
                "lease_id": ""
            }),
        )
        .unwrap();
        ctx.append(
            PROGRESS,
            &json!({
                "schema": 1,
                "kind": "autopilot-worker-call",
                "phase": "returned",
                "owner_id": owner_id,
                "run_id": run_id,
                "operation_id": op,
                "plan_id": plan["plan_id"],
                "task_id": task_id,
                "task_sha256": worker_contract(task).unwrap(),
                "lease_id": "",
                "lease_epoch": lease_epoch,
                "head": head,
                "route": {"provider": "codex", "model": "gpt-6-sol", "fallback_model": "", "reasoning_effort": "auto"},
                "exit": exit,
                "failure": {
                    "code": if retryable { "provider_capacity" } else { "none" },
                    "recovery": if retryable { "one_explicit_fallback" } else { "none" },
                    "retryable": retryable
                },
                "ts": value::now()
            }),
        )
        .unwrap();
        let identity = usage::Identity {
            call_id: value::random("call_").unwrap(),
            operation_id: op.clone(),
            provider: "codex".into(),
            selected_model: Some("gpt-6-sol".into()),
            task_id: Some(task_id.into()),
            attempt_id: Some(value::random("att_").unwrap()),
            ..Default::default()
        };
        let physical::Registration::New(mut receipt) = physical::begin(
            &ctx.state,
            identity,
            None,
            true,
            ctx.interrupted,
            std::time::Instant::now() + Duration::from_secs(10),
        )
        .unwrap() else {
            panic!("new physical telemetry");
        };
        receipt.revision += 1;
        receipt.phase = usage::Phase::Unknown;
        receipt.cleanup_complete = Some(true);
        receipt.source_sha256 = Some(value::hash(b"known-ended"));
        usage::record(&ctx.state, &receipt, ctx.interrupted).unwrap();
        op
    }

    fn commit_intent_step(
        ctx: &Context<'_>,
        run_id: &str,
        task_id: &str,
        file_rel: &str,
        content: &[u8],
        intent_paths_override: Option<Vec<&str>>,
        include_committed: bool,
    ) -> String {
        let base = git(&ctx.repo, &["rev-parse", "HEAD"]);
        private(&ctx.repo.join(file_rel), content);
        git(&ctx.repo, &["add", file_rel]);
        git(&ctx.repo, &["commit", "--quiet", "-m", task_id]);
        let commit = git(&ctx.repo, &["rev-parse", "HEAD"]);
        let tree = git(&ctx.repo, &["rev-parse", &format!("{commit}^{{tree}}")]);
        let intent_id = value::random("intent-").unwrap();
        let paths = intent_paths_override
            .map(|items| items.into_iter().map(str::to_owned).collect())
            .unwrap_or_else(|| vec![file_rel.to_owned()]);
        let intent = json!({
            "schema": 1,
            "kind": "commit-intent",
            "worktree": ctx.repo,
            "run_id": run_id,
            "cycle": 1,
            "intent_id": intent_id,
            "task_id": task_id,
            "base_sha": base,
            "head_ref": format!("refs/heads/{}", git(&ctx.repo, &["symbolic-ref", "--short", "HEAD"])),
            "paths": paths,
            "commit": commit,
            "expected_tree": tree,
            "phase": "landed",
            "ts": value::now()
        });
        ctx.append(PROGRESS, &intent).unwrap();
        if include_committed {
            let mut committed = intent;
            committed.as_object_mut().unwrap().remove("commit");
            committed.as_object_mut().unwrap().remove("expected_tree");
            committed["phase"] = "committed".into();
            ctx.append(PROGRESS, &committed).unwrap();
        }
        commit
    }

    fn approve_decision(ctx: &Context<'_>, row: &mut Value, decision: &CheckedDecision) -> Vec<u8> {
        let raw_before = std::fs::read(ctx.state.join(RECEIPT)).unwrap();
        let retry = &decision.value()["retry"];
        let recorded = retain(
            ctx,
            &json!({
                "schema": 1,
                "kind": "autopilot-parent-decision",
                "owner_id": row["owner_id"],
                "decision_sha256": decision.sha256(),
                "action": "resume",
                "reason": "test resume",
                "head": decision.value()["head"],
                "plan_sha256": decision.value()["plan_sha256"],
                "retry": retry
            }),
        )
        .unwrap();
        state(row)["last_decision"] = recorded;
        ctx.append(
            PROGRESS,
            &json!({
                "schema": 1,
                "kind": "autopilot-retry-reserved",
                "owner_id": row["owner_id"],
                "operation_id": retry["operation_id"],
                "receipt_sha256": value::hash(&raw_before),
                "decision_sha256": decision.sha256(),
                "evidence": row["adaptive"]["last_decision"]
            }),
        )
        .unwrap();
        row["stage"] = "approved".into();
        let next_raw = value::pretty(row).unwrap();
        ctx.store
            .mutate(RECEIPT, Some(&raw_before), &next_raw, MAX_EVIDENCE, true)
            .unwrap();
        next_raw
    }

    #[test]
    fn worker_retry_and_tranche_allow_disjoint_provenance_verified_sibling_commit() {
        let (_dir, mut row, ctx) = setup_parallel_retry(&[
            ("a", &["a/"], &["gpu-0"]),
            ("b", &["b/"], &["gpu-1"]),
            ("c", &["c/"], &["gpu-2"]),
        ]);
        let owner_id = value::text(&row, "owner_id").to_owned();
        let h0 = git(&ctx.repo, &["rev-parse", "HEAD"]);

        // Sibling `a` fails retryably, and sibling `b` finishes later (`exit == 0`)
        // so `b` is the last `autopilot-worker-call` row in the ledger.
        append_worker_call(&ctx, &owner_id, "goal-1", "a", &h0, 2, true);
        append_worker_call(&ctx, &owner_id, "goal-1", "b", &h0, 0, false);

        // Healthy sibling `b` commits disjoint changes (`H0 -> H1`).
        commit_intent_step(&ctx, "goal-1", "b", "b/out.txt", b"ok\n", None, true);
        let raw = std::fs::read(ctx.state.join(RECEIPT)).unwrap();
        let decision = decision(&ctx, &row, &raw).unwrap();
        assert_eq!(decision.value()["action"], "resume", "{}", decision.value());
        assert!(decision.allows("resume"));
        assert_eq!(decision.value()["retry"]["task_id"], "a");

        approve_decision(&ctx, &mut row, &decision);
        let t = tranche(&ctx).unwrap();
        assert_eq!(t["retry_task_id"], "a");

        // Another provenance-verified disjoint commit from `goal-1` (`H1 -> H2`)
        // keeps `tranche` aligned with `decision`.
        append_worker_call(&ctx, &owner_id, "goal-1", "c", &h0, 0, false);
        commit_intent_step(&ctx, "goal-1", "c", "c/out.txt", b"ok2\n", None, true);
        let t2 = tranche(&ctx).unwrap();
        assert_eq!(t2["retry_task_id"], "a");
    }

    #[test]
    fn worker_retry_and_tranche_fail_closed_on_overlap_foreign_commit_or_unbounded_scope() {
        // 1. Overlapping path (`a/conflict.txt` inside `a`'s `a/`) -> inspect.
        {
            let (_dir, row, ctx) =
                setup_parallel_retry(&[("a", &["a/"], &[]), ("b", &["b/"], &[])]);
            let owner_id = value::text(&row, "owner_id").to_owned();
            let h0 = git(&ctx.repo, &["rev-parse", "HEAD"]);
            append_worker_call(&ctx, &owner_id, "goal-1", "a", &h0, 2, true);
            append_worker_call(&ctx, &owner_id, "goal-1", "b", &h0, 0, false);
            commit_intent_step(&ctx, "goal-1", "b", "a/conflict.txt", b"x\n", None, true);
            let raw = std::fs::read(ctx.state.join(RECEIPT)).unwrap();
            let d = decision(&ctx, &row, &raw).unwrap();
            assert_eq!(d.value()["action"], "inspect", "{}", d.value());
            assert!(!d.allows("resume"));
        }

        // 2. Overlapping resource (`gpu-0` shared by `a` and `b`) -> inspect.
        {
            let (_dir, row, ctx) =
                setup_parallel_retry(&[("a", &["a/"], &["gpu-0"]), ("b", &["b/"], &["gpu-0"])]);
            let owner_id = value::text(&row, "owner_id").to_owned();
            let h0 = git(&ctx.repo, &["rev-parse", "HEAD"]);
            append_worker_call(&ctx, &owner_id, "goal-1", "a", &h0, 2, true);
            append_worker_call(&ctx, &owner_id, "goal-1", "b", &h0, 0, false);
            commit_intent_step(&ctx, "goal-1", "b", "b/out.txt", b"x\n", None, true);
            let raw = std::fs::read(ctx.state.join(RECEIPT)).unwrap();
            let d = decision(&ctx, &row, &raw).unwrap();
            assert_eq!(d.value()["action"], "inspect", "{}", d.value());
            assert!(!d.allows("resume"));
        }

        // 3. Missing `committed` row or foreign `run_id` -> inspect.
        {
            let (_dir, row, ctx) =
                setup_parallel_retry(&[("a", &["a/"], &[]), ("b", &["b/"], &[])]);
            let owner_id = value::text(&row, "owner_id").to_owned();
            let h0 = git(&ctx.repo, &["rev-parse", "HEAD"]);
            append_worker_call(&ctx, &owner_id, "goal-1", "a", &h0, 2, true);
            commit_intent_step(&ctx, "goal-1", "b", "b/out.txt", b"x\n", None, false);
            let raw = std::fs::read(ctx.state.join(RECEIPT)).unwrap();
            let d = decision(&ctx, &row, &raw).unwrap();
            assert_eq!(d.value()["action"], "inspect", "{}", d.value());
            assert!(!d.allows("resume"));
        }
        {
            let (_dir, row, ctx) =
                setup_parallel_retry(&[("a", &["a/"], &[]), ("b", &["b/"], &[])]);
            let owner_id = value::text(&row, "owner_id").to_owned();
            let h0 = git(&ctx.repo, &["rev-parse", "HEAD"]);
            append_worker_call(&ctx, &owner_id, "goal-1", "a", &h0, 2, true);
            // `goal-foreign` has no `autopilot-cycle` for `owner_id`.
            commit_intent_step(&ctx, "goal-foreign", "b", "b/out.txt", b"x\n", None, true);
            let raw = std::fs::read(ctx.state.join(RECEIPT)).unwrap();
            let d = decision(&ctx, &row, &raw).unwrap();
            assert_eq!(d.value()["action"], "inspect", "{}", d.value());
            assert!(!d.allows("resume"));
        }

        // 4. Forged `intent["paths"]` differing from `git diff-tree` -> inspect.
        {
            let (_dir, row, ctx) =
                setup_parallel_retry(&[("a", &["a/"], &[]), ("b", &["b/"], &[])]);
            let owner_id = value::text(&row, "owner_id").to_owned();
            let h0 = git(&ctx.repo, &["rev-parse", "HEAD"]);
            append_worker_call(&ctx, &owner_id, "goal-1", "a", &h0, 2, true);
            commit_intent_step(
                &ctx,
                "goal-1",
                "b",
                "b/out.txt",
                b"x\n",
                Some(vec!["b/other.txt"]),
                true,
            );
            let raw = std::fs::read(ctx.state.join(RECEIPT)).unwrap();
            let d = decision(&ctx, &row, &raw).unwrap();
            assert_eq!(d.value()["action"], "inspect", "{}", d.value());
            assert!(!d.allows("resume"));
        }

        // 5. Unbounded `allowed_paths` (`["."]` / `["*.rs"]`) when HEAD moved -> inspect.
        for unbounded in [&["."][..], &["*.rs"][..]] {
            let (_dir, row, ctx) =
                setup_parallel_retry(&[("a", unbounded, &[]), ("b", &["b/"], &[])]);
            let owner_id = value::text(&row, "owner_id").to_owned();
            let h0 = git(&ctx.repo, &["rev-parse", "HEAD"]);
            append_worker_call(&ctx, &owner_id, "goal-1", "a", &h0, 2, true);
            commit_intent_step(&ctx, "goal-1", "b", "b/out.txt", b"x\n", None, true);
            let raw = std::fs::read(ctx.state.join(RECEIPT)).unwrap();
            let d = decision(&ctx, &row, &raw).unwrap();
            assert_eq!(d.value()["action"], "inspect", "{}", d.value());
            assert!(!d.allows("resume"));
        }

        // 6. Foreign commit after `decision` fails closed in `tranche`.
        {
            let (_dir, mut row, ctx) =
                setup_parallel_retry(&[("a", &["a/"], &[]), ("b", &["b/"], &[])]);
            let owner_id = value::text(&row, "owner_id").to_owned();
            let h0 = git(&ctx.repo, &["rev-parse", "HEAD"]);
            append_worker_call(&ctx, &owner_id, "goal-1", "a", &h0, 2, true);
            append_worker_call(&ctx, &owner_id, "goal-1", "b", &h0, 0, false);
            commit_intent_step(&ctx, "goal-1", "b", "b/out.txt", b"x\n", None, true);
            let raw = std::fs::read(ctx.state.join(RECEIPT)).unwrap();
            let d = decision(&ctx, &row, &raw).unwrap();
            assert!(d.allows("resume"));
            approve_decision(&ctx, &mut row, &d);

            // Foreign manual commit advances HEAD without `commit-intent` provenance.
            private(&ctx.repo.join("b/manual.txt"), b"foreign\n");
            git(&ctx.repo, &["add", "b/manual.txt"]);
            git(&ctx.repo, &["commit", "--quiet", "-m", "foreign"]);
            let err = tranche(&ctx).unwrap_err();
            assert!(
                err.message.contains("reviewed worker retry source changed"),
                "{}",
                err.message
            );
        }
    }
}
