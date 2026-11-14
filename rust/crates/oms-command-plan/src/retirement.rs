//! Exact active-plan generation CAS, archives, proof ledger and crash recovery.
use crate::engine::{STATES, lineage, marker_alive, marker_typed, reason, valid_id};
use crate::json::{self, Value, list, number, object, string};
use crate::storage::{self, ACTIVE};
use crate::{Plan, Request, Response, Result, Snapshot, error, require};
use std::collections::{BTreeMap, BTreeSet};

pub struct PreparedRetirement {
    pub(crate) snapshot: Value,
    raw: Vec<u8>,
    plan: Value,
    disposition: String,
    reason: String,
    archive: String,
    intent_path: String,
    intent: Option<Value>,
    prior: Option<Value>,
    already_retired: Option<Value>,
}
impl PreparedRetirement {
    pub fn context(&self) -> &Value {
        &self.snapshot
    }
    pub fn resumes(&self) -> bool {
        self.intent.is_some() || self.prior.is_some()
    }
}
fn paths(hash: &str) -> (String, String) {
    (
        format!(".oms/plan/tasks.{hash}.archive.json"),
        format!(".oms/plan/tasks.{hash}.retire-intent.json"),
    )
}
fn temporary(path: &str) -> Result<String> {
    let (parent, name) = path
        .rsplit_once('/')
        .ok_or_else(|| error("invalid retirement authority path"))?;
    Ok(format!("{parent}/.{name}.tmp"))
}
fn recover(plan: &Plan<'_>, path: &str) -> Result<()> {
    let temporary = temporary(path)?;
    plan.store.recover_pair(path, &temporary)?;
    if let Some(raw) = plan.store.optional(&temporary, 1024 * 1024)? {
        plan.store.remove(&temporary, &raw, None)?;
    }
    Ok(())
}
fn payload(plan: &Plan<'_>, raw: &[u8]) -> Result<Value> {
    let value = json::parse_legacy(raw, 1024 * 1024)?;
    value.object()?;
    let tasks = value.get("tasks").object()?;
    let id = value.text("plan_id");
    require(
        id.is_empty() || lineage(id, "plan_"),
        "plan has invalid plan_id",
    )?;
    for (id, task) in tasks {
        let id = std::str::from_utf8(id).map_err(|_| error("plan task id is not UTF-8"))?;
        require(valid_id(id), "plan contains invalid task id")?;
        task.object()?;
        require(
            task.opt("state")
                .is_none_or(|s| s.str().is_ok_and(|s| STATES.contains(&s))),
            "plan task has invalid state",
        )?;
        crate::decision::validate(&plan.store, task)?;
    }
    Ok(value)
}
fn states(plan: &Value) -> Result<Value> {
    let mut by = BTreeMap::<String, u64>::new();
    for (_, t) in plan.get("tasks").object()? {
        *by.entry(
            t.opt("state")
                .map(Value::str)
                .transpose()?
                .unwrap_or("ready")
                .into(),
        )
        .or_default() += 1;
    }
    Ok(Value::Object(
        by.into_iter()
            .map(|(k, v)| (k.into_bytes(), number(v)))
            .collect(),
    ))
}
fn active(plan: &Plan<'_>, state: &Value) -> Result<Vec<String>> {
    let tasks = state.get("tasks");
    let mut active = tasks
        .object()?
        .iter()
        .filter(|(_, t)| {
            matches!(
                t.text("state"),
                "claimed" | "running" | "review" | "landing"
            )
        })
        .map(|(k, _)| String::from_utf8(k.clone()).map_err(|_| error("task id not UTF-8")))
        .collect::<Result<BTreeSet<_>>>()?;
    for marker in plan.worker_markers()? {
        require(
            marker_typed(&marker),
            "worker marker set malformed or unproven; refusing retirement",
        )?;
        if tasks.opt(marker.text("task_id")).is_some() && marker_alive(&marker) {
            active.insert(marker.text("task_id").into());
        }
    }
    Ok(active.into_iter().collect())
}
fn acceptance_proof(plan: &Plan<'_>, expected: &Value) -> Result<Value> {
    let rows = plan.store.rows(".oms/plan/progress.jsonl", true)?;
    let found = rows
        .iter()
        .filter(|r| {
            r.text("kind") == "acceptance" && r.get("retire_proof_id") == expected.get("proof_id")
        })
        .collect::<Vec<_>>();
    require(
        found.len() == 1,
        "completed-external requires fresh acceptance run from this retire call",
    )?;
    let row = found[0];
    require(
        row.get("schema").is_int(1)
            && !row.text("ts").is_empty()
            && row.text("status") == "pass"
            && row.get("exit").is_int(0)
            && row.text("reason").is_empty()
            && row.get("timed_out") == &Value::Bool(false)
            && row.get("output_limited") == &Value::Bool(false)
            && row.get("git_clean") == &Value::Bool(true)
            && row.get("plan_sha256") == expected.get("plan_sha256")
            && row.get("base_sha") == expected.get("head")
            && row.get("git_ref") == expected.get("git_ref")
            && row.get("plan_generation") == expected.get("plan_generation")
            && row.get("accept_sha256_full") == expected.get("accept_sha256")
            && json::hex(row.text("output_sha256_full"), 64)
            && json::hex(row.text("repo_snapshot_sha256"), 64)
            && row.text("accept_sha256") == &row.text("accept_sha256_full")[..16]
            && row.text("output_sha256") == &row.text("output_sha256_full")[..16],
        "completed-external acceptance proof stale or belongs to another retire call",
    )?;
    Ok(object([
        ("ts", row.get("ts").clone()),
        ("base_sha", row.get("base_sha").clone()),
        ("git_ref", row.get("git_ref").clone()),
        ("accept_sha256", row.get("accept_sha256_full").clone()),
        ("output_sha256", row.get("output_sha256_full").clone()),
        (
            "repo_snapshot_sha256",
            row.get("repo_snapshot_sha256").clone(),
        ),
        ("plan_sha256", row.get("plan_sha256").clone()),
        ("proof_id", row.get("retire_proof_id").clone()),
    ]))
}
pub fn validate_receipt(plan: &Plan<'_>, row: &Value) -> Result<()> {
    let hash = json::digest(row.get("plan_sha256"))?;
    let (archive, _) = paths(hash);
    require(
        row.text("archive") == archive
            && row.get("schema").is_int(1)
            && !row.text("ts").is_empty()
            && (row.text("plan_id").is_empty() || lineage(row.text("plan_id"), "plan_")),
        "plan retirement receipt has invalid core fields",
    )?;
    let archived = plan.store.read(&archive, 1024 * 1024)?;
    require(
        json::sha(&archived) == hash,
        "retirement receipt lacks exact content-addressed archive",
    )?;
    let state = payload(plan, &archived)?;
    require(
        state.text("plan_id") == row.text("plan_id"),
        "retirement receipt lineage contradicts archive",
    )?;
    if row.text("kind") == "plan-replacement" {
        json::keys(
            row,
            &[
                "schema",
                "kind",
                "ts",
                "replacement_id",
                "plan_sha256",
                "plan_id",
                "disposition",
                "archive",
                "non_terminal_task_ids",
            ],
            &[],
        )?;
        require(
            row.text("replacement_id") == format!("replace_{hash}")
                && row.text("disposition") == "replaced",
            "plan replacement receipt invalid core fields",
        )?;
        let mut actual = state
            .get("tasks")
            .object()?
            .iter()
            .filter(|(_, t)| !matches!(t.text("state"), "done" | "cancelled"))
            .map(|(id, _)| {
                std::str::from_utf8(id)
                    .map(str::to_owned)
                    .map_err(|_| error("task id not UTF-8"))
            })
            .collect::<Result<Vec<_>>>()?;
        actual.sort();
        require(
            row.get("non_terminal_task_ids") == &list(actual.iter().map(String::as_str)),
            "replacement receipt task ids contradict archive",
        )?;
        return Ok(());
    }
    let disposition = row.get("disposition").str()?;
    let mut required = vec![
        "schema",
        "kind",
        "ts",
        "retirement_id",
        "plan_sha256",
        "plan_id",
        "disposition",
        "reason",
        "head",
        "git_ref",
        "archive",
        "plan_generation",
        "proof_id",
        "acceptance_verified",
        "task_landings_claimed",
        "task_states",
    ];
    if disposition == "completed-external" {
        required.push("acceptance");
    }
    json::keys(row, &required, &[])?;
    require(
        row.text("kind") == "plan-retirement"
            && json::hex(row.text("proof_id"), 64)
            && row.text("retirement_id") == format!("retire_{}", row.text("proof_id"))
            && matches!(disposition, "completed-external" | "superseded")
            && !row.text("reason").is_empty()
            && row.text("reason").chars().count() <= 500
            && crate::repository::commit(row.text("head"))
            && !row.text("git_ref").is_empty()
            && storage::valid_generation(row.get("plan_generation"))
            && row.get("task_landings_claimed") == &Value::Bool(false),
        "plan retirement receipt invalid core fields",
    )?;
    let actual = states(&state)?;
    require(
        row.get("task_states") == &actual
            && actual.object()?.iter().all(|(s, _)| {
                ![b"claimed".as_slice(), b"running", b"review", b"landing"].contains(&s.as_slice())
            }),
        "retirement receipt states contradict archive or archive contains active authority",
    )?;
    if disposition == "completed-external" {
        let accept = state.get("accept").str()?;
        require(
            !accept.is_empty(),
            "completed-external archive lacks acceptance command",
        )?;
        let snapshot = object([
            ("plan_sha256", string(hash)),
            ("head", row.get("head").clone()),
            ("git_ref", row.get("git_ref").clone()),
            ("plan_generation", row.get("plan_generation").clone()),
            ("proof_id", row.get("proof_id").clone()),
            ("accept_sha256", string(json::sha(accept))),
        ]);
        let proof = acceptance_proof(plan, &snapshot)?;
        require(
            row.get("acceptance_verified") == &Value::Bool(true) && row.get("acceptance") == &proof,
            "completed-external retirement lacks exact fresh acceptance proof",
        )?;
    } else {
        require(
            row.get("acceptance_verified") == &Value::Bool(false)
                && actual
                    .object()?
                    .iter()
                    .all(|(s, _)| [b"done".as_slice(), b"cancelled"].contains(&s.as_slice())),
            "superseded retirement contradicts closed archive",
        )?;
    }
    Ok(())
}
pub fn validate_ledger(plan: &Plan<'_>) -> Result<Vec<Value>> {
    let rows = plan.store.rows(".oms/plan/retirements.jsonl", true)?;
    let mut hashes = BTreeSet::new();
    let mut proofs = BTreeSet::new();
    for row in &rows {
        validate_receipt(plan, row)?;
        require(
            hashes.insert(row.text("plan_sha256"))
                && proofs.insert(
                    row.opt("proof_id")
                        .unwrap_or(row.get("replacement_id"))
                        .str()?,
                ),
            "duplicate plan retirement receipt or proof",
        )?;
    }
    Ok(rows)
}
fn intent_from_receipt(row: &Value) -> Value {
    object([
        ("schema", number(1)),
        ("kind", string("plan-retirement-intent")),
        ("plan_sha256", row.get("plan_sha256").clone()),
        ("disposition", row.get("disposition").clone()),
        ("reason", row.get("reason").clone()),
        ("head", row.get("head").clone()),
        ("git_ref", row.get("git_ref").clone()),
        ("plan_generation", row.get("plan_generation").clone()),
        ("proof_id", row.get("proof_id").clone()),
        ("archive", row.get("archive").clone()),
    ])
}
pub(crate) fn cleanup(plan: &Plan<'_>) -> Result<()> {
    if !plan.context.canonical() {
        return Ok(());
    }
    let names = plan.store.names(".oms/plan", 65536, true)?;
    let mut matches = Vec::new();
    for name in names {
        if let Some(name) = name.to_str() {
            if let Some(hash) = name
                .strip_prefix("tasks.")
                .and_then(|s| s.strip_suffix(".retire-intent.json"))
            {
                if json::hex(hash, 64) {
                    matches.push((hash.to_owned(), format!(".oms/plan/{name}")));
                }
            }
        }
    }
    if matches.is_empty() {
        return Ok(());
    }
    require(
        matches.len() == 1,
        "multiple completed retirement intents require exact replay",
    )?;
    let (hash, path) = &matches[0];
    let rows = validate_ledger(plan)?;
    let prior = rows
        .iter()
        .find(|r| r.text("kind") == "plan-retirement" && r.text("plan_sha256") == hash)
        .ok_or_else(|| error("retirement intent lacks validated durable receipt"))?;
    let raw = plan.store.read(path, 64 * 1024)?;
    let rows = storage::rows(&raw)?;
    require(
        rows.len() == 1 && rows[0] == intent_from_receipt(prior),
        "completed retirement has conflicting residual intent",
    )?;
    if let Some(s) = plan.store.snapshot(ACTIVE, 1024 * 1024, true)? {
        require(
            s.generation != *prior.get("plan_generation"),
            "exact retired plan generation still requires retire replay",
        )?;
        let active = payload(plan, &s.raw)?;
        require(
            !active.text("plan_id").is_empty() && active.get("plan_id") != prior.get("plan_id"),
            "active plan lineage still requires exact retire replay",
        )?;
    }
    plan.store.remove(path, &raw, None)
}
pub(crate) fn cleanup_for_accept(
    plan: &Plan<'_>,
    retirement: Option<&PreparedRetirement>,
) -> Result<()> {
    // A resumed exact retirement still owns its intent; ordinary acceptance
    // cannot clean that live intent or borrow its historical proof.
    if retirement.is_none() {
        cleanup(plan)?;
    }
    Ok(())
}
pub fn validate_active_lineage(plan: &Plan<'_>) -> Result<Vec<Value>> {
    let rows = validate_ledger(plan)?;
    if let Some(raw) = plan.store.optional(ACTIVE, 1024 * 1024)? {
        let state = payload(plan, &raw)?;
        for row in &rows {
            if row.text("kind") == "plan-retirement" {
                require(
                    row.text("plan_sha256") != json::sha(&raw)
                        && row.get("plan_id") != state.get("plan_id"),
                    "active plan lineage conflicts with retirement receipt",
                )?;
            }
        }
    }
    Ok(rows)
}
pub(crate) fn archive_replacement(plan: &Plan<'_>, s: &Snapshot) -> Result<String> {
    plan.context.parent()?;
    plan.canonical_existing(s)?;
    let raw = s.raw.as_ref().ok_or_else(|| error("active plan missing"))?;
    let state = payload(plan, raw)?;
    let hash = json::sha(raw);
    let rows = validate_ledger(plan)?;
    let prior = rows.iter().find(|r| r.text("plan_sha256") == hash);
    if let Some(prior) = prior {
        require(
            prior.text("kind") == "plan-replacement",
            "active plan bytes already have retirement receipt",
        )?;
    }
    let (archive, _) = paths(&hash);
    recover(plan, &archive)?;
    plan.store.publish_once(&archive, raw, 1024 * 1024)?;
    let mut nonterminal = state
        .get("tasks")
        .object()?
        .iter()
        .filter(|(_, t)| !matches!(t.text("state"), "done" | "cancelled"))
        .map(|(k, _)| String::from_utf8(k.clone()).map_err(|_| error("task id not UTF-8")))
        .collect::<Result<Vec<_>>>()?;
    nonterminal.sort();
    let mut row = object([
        ("schema", number(1)),
        ("kind", string("plan-replacement")),
        ("ts", string(&plan.context.ts)),
        ("replacement_id", string(format!("replace_{hash}"))),
        ("plan_sha256", string(hash)),
        ("plan_id", string(state.text("plan_id"))),
        ("disposition", string("replaced")),
        ("archive", string(&archive)),
        (
            "non_terminal_task_ids",
            list(nonterminal.iter().map(String::as_str)),
        ),
    ]);
    if let Some(prior) = prior {
        row.set("ts", prior.get("ts").clone())?;
        require(
            &row == prior,
            "existing replacement receipt conflicts with operation",
        )?;
    } else {
        plan.store.append(".oms/plan/retirements.jsonl", &row)?;
    }
    Ok(archive)
}
pub fn execute(plan: &Plan<'_>, req: &Request) -> Result<Response> {
    plan.context.parent()?;
    require(
        plan.context.canonical() && plan.context.execution_repo == plan.context.state_repo,
        "retire valid only for canonical repo-local plan",
    )?;
    plan.git().guard()?;
    let disposition = req.text("disposition")?;
    require(
        matches!(disposition, "" | "completed-external" | "superseded"),
        "--disposition must be completed-external or superseded",
    )?;
    if !req.has("apply") {
        return plan.locked(true, None, || check(plan));
    }
    let prepared = plan.locked(true, None, || preflight(plan, req))?;
    if let Some(response) = &prepared.already_retired {
        return Response::json_line(response, true, false);
    }
    if prepared.disposition == "completed-external" && !prepared.resumes() {
        let acceptance = Request::new(crate::Action::Accept);
        let result = crate::acceptance::execute(plan, &acceptance, Some(&prepared))?;
        if result.code != 0 {
            return Ok(result);
        }
    }
    plan.locked(true, None, || finalize(plan, &prepared))
}
fn check(plan: &Plan<'_>) -> Result<Response> {
    let s = plan.load()?;
    let raw = s
        .raw
        .as_ref()
        .ok_or_else(|| error("no active plan to retire"))?;
    let blockers = active(plan, &s.plan)?;
    let git = if blockers.is_empty() {
        plan.git().snapshot()?
    } else {
        object([
            ("head", string("")),
            ("git_ref", string("")),
            ("clean", Value::Bool(false)),
        ])
    };
    let result = object([
        ("schema", number(1)),
        ("action", string("plan-retire-check")),
        ("plan_sha256", string(json::sha(raw))),
        ("head", git.get("head").clone()),
        ("git_ref", git.get("git_ref").clone()),
        ("clean", git.get("clean").clone()),
        (
            "active_task_blockers",
            list(blockers.iter().map(String::as_str)),
        ),
        ("would_write", Value::Bool(false)),
    ]);
    let mut response = Response::json_line(&result, true, false)?;
    if !blockers.is_empty() {
        response.code = 3;
    }
    Ok(response)
}
fn preflight(plan: &Plan<'_>, req: &Request) -> Result<PreparedRetirement> {
    let disposition = req.value("disposition")?;
    let reason = reason(req, "retirement")?;
    let expected = req.value("expected-plan-sha256")?;
    require(
        json::hex(expected, 64),
        "retirement expected plan SHA-256 invalid",
    )?;
    let (archive, intent_path) = paths(expected);
    recover(plan, &archive)?;
    recover(plan, &intent_path)?;
    let prior = validate_ledger(plan)?
        .into_iter()
        .find(|r| r.text("kind") == "plan-retirement" && r.text("plan_sha256") == expected);
    if let Some(row) = &prior {
        require(
            row.text("disposition") == disposition
                && row.text("reason") == reason
                && row.text("archive") == archive,
            "existing retirement receipt malformed or conflicts with operation",
        )?;
    }
    let intent = if let Some(raw) = plan.store.optional(&intent_path, 64 * 1024)? {
        let rows = storage::rows(&raw)?;
        require(
            rows.len() == 1,
            "retirement intent must contain exactly one object row",
        )?;
        Some(rows[0].clone())
    } else {
        None
    };
    let active_snap = plan.store.snapshot(ACTIVE, 1024 * 1024, true)?;
    if let Some(prior) = &prior {
        if active_snap
            .as_ref()
            .is_none_or(|s| s.generation != *prior.get("plan_generation"))
        {
            if let Some(active) = &active_snap {
                let state = payload(plan, &active.raw)?;
                require(
                    json::sha(&active.raw) != expected
                        && state.get("plan_id") != prior.get("plan_id"),
                    "active plan still belongs to retired lineage; exact replay required",
                )?;
            }
            if let Some(intent) = &intent {
                require(
                    intent == &intent_from_receipt(prior),
                    "completed retirement conflicting residual intent",
                )?;
                let raw = plan.store.read(&intent_path, 64 * 1024)?;
                plan.store.remove(&intent_path, &raw, None)?;
            }
            let response = object([
                ("schema", number(1)),
                ("action", string("plan-retire")),
                ("status", string("already-retired")),
                ("plan_sha256", string(expected)),
                ("archive", string(&archive)),
                ("disposition", string(disposition)),
                ("active_plan_untouched", Value::Bool(active_snap.is_some())),
            ]);
            return Ok(PreparedRetirement {
                snapshot: crate::json::empty(),
                raw: vec![],
                plan: crate::json::empty(),
                disposition: disposition.into(),
                reason: reason.into(),
                archive,
                intent_path,
                intent,
                prior: Some(prior.clone()),
                already_retired: Some(response),
            });
        }
    }
    let active_snap = active_snap
        .ok_or_else(|| error("retirement incomplete but original active plan missing"))?;
    require(
        json::sha(&active_snap.raw) == expected,
        "active plan changed after retirement CAS reviewed",
    )?;
    let s = plan.load()?;
    require(s.unverified.is_empty(), "research completion not verified")?;
    let state = payload(plan, &active_snap.raw)?;
    require(
        active(plan, &state)?.is_empty(),
        "active plan task authority prevents retirement",
    )?;
    let acceptance = state.text("accept");
    if disposition == "completed-external" {
        require(
            !acceptance.is_empty(),
            "completed-external requires nonempty plan acceptance command",
        )?;
    } else {
        require(
            state
                .get("tasks")
                .object()?
                .iter()
                .all(|(_, t)| matches!(t.text("state"), "done" | "cancelled")),
            "superseded retirement requires every old-plan task done or cancelled",
        )?;
    }
    let git = plan.git().snapshot()?;
    if disposition == "completed-external" {
        require(
            git.get("clean") == &Value::Bool(true),
            "plan retirement requires clean committed worktree",
        )?;
    }
    let proof = if let Some(row) = &prior {
        require(
            intent.is_some(),
            "receipt-before-unlink recovery requires exact retirement intent",
        )?;
        row.text("proof_id").to_owned()
    } else if let Some(i) = &intent {
        require(
            i.text("plan_sha256") == expected
                && i.text("disposition") == disposition
                && i.text("reason") == reason
                && i.get("head") == git.get("head")
                && i.get("git_ref") == git.get("git_ref")
                && i.get("plan_generation") == &active_snap.generation,
            "retirement intent does not match current plan generation",
        )?;
        i.text("proof_id").to_owned()
    } else {
        storage::random_hex(32)?
    };
    require(json::hex(&proof, 64), "retirement proof token malformed")?;
    let snapshot = object([
        ("plan_sha256", string(expected)),
        ("head", git.get("head").clone()),
        ("git_ref", git.get("git_ref").clone()),
        ("plan_generation", active_snap.generation),
        ("proof_id", string(proof)),
        ("accept_sha256", string(json::sha(acceptance))),
    ]);
    Ok(PreparedRetirement {
        snapshot,
        raw: active_snap.raw,
        plan: state,
        disposition: disposition.into(),
        reason: reason.into(),
        archive,
        intent_path,
        intent,
        prior,
        already_retired: None,
    })
}
pub(crate) fn accept_context(plan: &Plan<'_>, p: &PreparedRetirement) -> Result<()> {
    let active = plan
        .store
        .snapshot(ACTIVE, 1024 * 1024, false)?
        .ok_or_else(|| error("retirement acceptance plan missing"))?;
    require(
        active.generation == *p.snapshot.get("plan_generation")
            && json::sha(&active.raw) == p.snapshot.text("plan_sha256"),
        "acceptance-retire-context-changed: plan or generation changed",
    )?;
    let git = plan.git().snapshot()?;
    require(
        git.get("head") == p.snapshot.get("head")
            && git.get("git_ref") == p.snapshot.get("git_ref")
            && git.get("clean") == &Value::Bool(true),
        "acceptance-retire-context-changed: HEAD, ref or clean status changed",
    )
}
fn finalize(plan: &Plan<'_>, p: &PreparedRetirement) -> Result<Response> {
    let current = plan
        .store
        .snapshot(ACTIVE, 1024 * 1024, false)?
        .ok_or_else(|| error("active plan missing during retirement"))?;
    require(
        current.raw == p.raw && current.generation == *p.snapshot.get("plan_generation"),
        "plan or file generation changed during retirement",
    )?;
    require(
        active(plan, &p.plan)?.is_empty(),
        "active worker authority prevents retirement",
    )?;
    let git = plan.git().snapshot()?;
    require(
        git.get("head") == p.snapshot.get("head")
            && git.get("git_ref") == p.snapshot.get("git_ref")
            && (p.disposition != "completed-external" || git.get("clean") == &Value::Bool(true)),
        "HEAD, ref or clean status changed during retirement",
    )?;
    let proof = if p.disposition == "completed-external" {
        Some(acceptance_proof(plan, &p.snapshot)?)
    } else {
        None
    };
    let intent = object([
        ("schema", number(1)),
        ("kind", string("plan-retirement-intent")),
        ("plan_sha256", p.snapshot.get("plan_sha256").clone()),
        ("disposition", string(&p.disposition)),
        ("reason", string(&p.reason)),
        ("head", p.snapshot.get("head").clone()),
        ("git_ref", p.snapshot.get("git_ref").clone()),
        ("plan_generation", p.snapshot.get("plan_generation").clone()),
        ("proof_id", p.snapshot.get("proof_id").clone()),
        ("archive", string(&p.archive)),
    ]);
    if let Some(existing) = &p.intent {
        require(
            existing == &intent,
            "retirement intent differs from exact operation",
        )?;
    } else {
        plan.store
            .publish_once(&p.intent_path, &json::encoded(&intent)?, 64 * 1024)?;
    }
    plan.store.publish_once(&p.archive, &p.raw, 1024 * 1024)?;
    let mut receipt = object([
        ("schema", number(1)),
        ("kind", string("plan-retirement")),
        ("ts", string(&plan.context.ts)),
        (
            "retirement_id",
            string(format!("retire_{}", p.snapshot.text("proof_id"))),
        ),
        ("plan_sha256", p.snapshot.get("plan_sha256").clone()),
        ("plan_id", string(p.plan.text("plan_id"))),
        ("disposition", string(&p.disposition)),
        ("reason", string(&p.reason)),
        ("head", p.snapshot.get("head").clone()),
        ("git_ref", p.snapshot.get("git_ref").clone()),
        ("archive", string(&p.archive)),
        ("plan_generation", p.snapshot.get("plan_generation").clone()),
        ("proof_id", p.snapshot.get("proof_id").clone()),
        (
            "acceptance_verified",
            Value::Bool(p.disposition == "completed-external"),
        ),
        ("task_landings_claimed", Value::Bool(false)),
        ("task_states", states(&p.plan)?),
    ]);
    if let Some(proof) = proof {
        receipt.set("acceptance", proof)?;
    }
    if let Some(prior) = &p.prior {
        receipt.set("ts", prior.get("ts").clone())?;
        receipt.set("retirement_id", prior.get("retirement_id").clone())?;
        require(
            &receipt == prior,
            "existing retirement receipt conflicts with operation",
        )?;
    } else {
        plan.store.append(".oms/plan/retirements.jsonl", &receipt)?;
    }
    plan.store
        .remove(ACTIVE, &p.raw, Some(p.snapshot.get("plan_generation")))?;
    let raw = plan.store.read(&p.intent_path, 64 * 1024)?;
    require(
        json::parse(&raw)? == intent,
        "retirement intent changed after publication",
    )?;
    plan.store.remove(&p.intent_path, &raw, None)?;
    Response::json_line(
        &object([
            ("schema", number(1)),
            ("action", string("plan-retire")),
            ("status", string("retired")),
            ("plan_sha256", p.snapshot.get("plan_sha256").clone()),
            ("archive", string(&p.archive)),
            ("disposition", string(&p.disposition)),
            ("receipt", receipt.get("retirement_id").clone()),
        ]),
        true,
        false,
    )
}
