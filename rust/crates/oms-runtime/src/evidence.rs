//! Receipt strength, exact lineage and statement/worktree-bound attestations.
use crate::storage::{Runtime, random_id, safe_id};
use crate::value::*;
use crate::{Result, error, require};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
const INDEX: &str = ".oms/artifacts/index.jsonl";
const BINDINGS: &str = ".oms/evidence/bindings.jsonl";
const ADVISORY: &[&str] = &[
    "call",
    "ask",
    "review",
    "ask-synthesis",
    "review-synthesis",
    "review-outcome",
    "call-export",
    "ask-export",
    "review-export",
    "call-import",
    "ask-import",
    "review-import",
    "delegate-import",
];
fn exit(v: &Value) -> Option<i128> {
    v.int()
        .ok()
        .or_else(|| v.str().ok().and_then(|s| s.parse().ok()))
}
fn bad_status(s: &str) -> bool {
    matches!(
        s,
        "fail" | "failed" | "error" | "rejected" | "reject" | "blocked" | "denied"
    )
}
pub fn outcome(row: &Value) -> Option<&'static str> {
    let kind = row.text("kind").to_lowercase();
    let st = text(
        row.opt("status")
            .or(row.opt("state"))
            .unwrap_or(row.get("verdict")),
    )
    .to_lowercase();
    if ADVISORY.contains(&kind.as_str()) {
        return Some(
            if bad_status(&st)
                || ["exit", "exit_code", "verify_exit"]
                    .iter()
                    .any(|k| exit(row.get(k)).is_some_and(|n| n != 0))
            {
                "failed"
            } else if matches!(st.as_str(), "skipped" | "skipped_with_reason") {
                "skipped_with_reason"
            } else {
                "inconclusive"
            },
        );
    }
    if kind == "review-verify" {
        return Some(
            if bad_status(&st)
                || ["exit", "verify_exit"]
                    .iter()
                    .any(|k| exit(row.get(k)).is_some_and(|n| n != 0))
            {
                "failed"
            } else if exit(row.get("exit")) == Some(0) {
                "verified"
            } else {
                "inconclusive"
            },
        );
    }
    if kind == "patch-admit" {
        return Some(
            if bad_status(&st) || exit(row.get("exit")).is_some_and(|n| n != 0) {
                "failed"
            } else if matches!(row.text("context_verification"), "none" | "dry-run") {
                "inconclusive"
            } else if exit(row.get("exit")) == Some(0)
                && st == "verified"
                && row.get("verify_exit").is_int(0)
            {
                "verified"
            } else {
                "inconclusive"
            },
        );
    }
    if kind == "acceptance" {
        return Some(if bad_status(&st) {
            "failed"
        } else {
            match exit(row.get("exit")) {
                Some(0) => "verified",
                Some(_) => "failed",
                None => "inconclusive",
            }
        });
    }
    if kind == "delegate" {
        let worker = row.get("exit").int().ok();
        let verifier = row.get("verify_exit").int().ok();
        let mode = row.text("context_verification");
        return Some(if worker.is_some_and(|n| n != 0) {
            "failed"
        } else if mode == "dry-run" {
            "skipped_with_reason"
        } else if mode == "none" {
            "inconclusive"
        } else if mode == "command" {
            if verifier.is_none_or(|n| !(0..125).contains(&n)) {
                "inconclusive"
            } else if verifier != Some(0) {
                "failed"
            } else if worker == Some(0) {
                "verified"
            } else {
                "inconclusive"
            }
        } else if bad_status(&st) || verifier.is_some_and(|n| n != 0) {
            "failed"
        } else if worker.is_some() && verifier.is_some() {
            "verified"
        } else {
            "inconclusive"
        });
    }
    match st.as_str() {
        "pass" | "passed" | "success" | "succeeded" | "verified" | "done" | "approved"
        | "admit" | "admitted" => return Some("verified"),
        "fail" | "failed" | "error" | "rejected" | "reject" | "blocked" | "denied" => {
            return Some("failed");
        }
        "inconclusive" | "unknown" | "partial" => return Some("inconclusive"),
        "skipped" | "skipped_with_reason" => return Some("skipped_with_reason"),
        _ => {}
    }
    if row.get("review").object().is_ok() {
        if let Some(v) = outcome(row.get("review")) {
            return Some(v);
        }
    }
    for k in ["verify_exit", "exit", "exit_code"] {
        if let Some(n) = exit(row.get(k)) {
            return Some(if n == 0 { "verified" } else { "failed" });
        }
    }
    None
}
pub fn evidence_ref(row: &Value) -> String {
    for key in [
        "event_id",
        "id",
        "operation_id",
        "attempt_id",
        "run_id",
        "delegation_id",
    ] {
        if row.get(key).truth() {
            return bounded(row.text(key), 160);
        }
    }
    String::new()
}
fn refs(row: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for key in ["covers", "criteria", "criterion_ids", "acceptance_criteria"] {
        let v = row.get(key);
        if let Ok(s) = v.str() {
            out.extend(s.replace(',', " ").split_whitespace().map(str::to_owned))
        } else {
            out.extend(
                rows(v)
                    .iter()
                    .map(|v| text(v).trim().to_owned())
                    .filter(|s| !s.is_empty()),
            )
        }
    }
    if row.get("evidence").object().is_ok() {
        out.extend(refs(row.get("evidence")))
    }
    out
}
pub fn artifacts(rt: &Runtime<'_>) -> Result<Vec<Value>> {
    let mut out = vec![];
    for path in [
        INDEX,
        ".oms/plan/progress.jsonl",
        ".oms/lifecycle/events.jsonl",
    ] {
        for mut row in rt.rows(path)? {
            row.set("_source", string(path))?;
            row.set("_ordinal", number(out.len() + 1))?;
            out.push(row)
        }
    }
    Ok(out)
}
pub fn resolved_ids(rows: &[Value]) -> BTreeSet<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for r in rows.iter().filter(|r| r.text("_source") == INDEX) {
        if let Ok(id) = r.get("event_id").str() {
            *counts.entry(id).or_default() += 1
        }
    }
    let by_id = rows
        .iter()
        .filter(|r| r.text("_source") == INDEX && counts.get(r.text("event_id")) == Some(&1))
        .map(|r| (r.text("event_id"), r))
        .collect::<BTreeMap<_, _>>();
    let mut out = BTreeSet::new();
    for r in by_id.values() {
        if r.text("kind") != "artifact-resolution" {
            continue;
        }
        let id = r.text("resolves_event_id");
        let Some(t) = by_id.get(id) else { continue };
        if r.get("schema").is_int(1)
            && r.text("parent_event_id") == id
            && r.text("resolution") == "resolved"
            && r.get("exit").is_int(0)
            && t.get("exit").int().is_ok_and(|n| n > 0)
            && t.get("_ordinal").int().ok() < r.get("_ordinal").int().ok()
            && t.get("schema").is_int(1)
            && t.text("kind") != "artifact-resolution"
            && r.get("operation_id") == t.get("operation_id")
            && r.get("artifact_id") == t.get("artifact_id")
        {
            out.insert(id.into());
        }
    }
    out
}
fn admission_digest(rt: &Runtime<'_>, row: &Value) -> Result<String> {
    let raw = row.text("artifact");
    let p = Path::new(raw);
    if raw.is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Ok(String::new());
    }
    let data = match rt.bytes(p, crate::storage::MAX_STATE) {
        Ok(Some(v)) => v,
        _ => return Ok(String::new()),
    };
    if sha(data.as_slice()) != row.text("artifact_sha256") {
        return Ok(String::new());
    }
    let Ok(s) = std::str::from_utf8(&data) else {
        return Ok(String::new());
    };
    let header = s.split("\n## Ladder").next().unwrap_or("");
    let lines = header.lines().collect::<Vec<_>>();
    if lines.first() != Some(&"# Patch admission: ADMIT")
        || !lines.contains(&"- verification: command")
    {
        return Ok(String::new());
    }
    let values = lines
        .iter()
        .filter_map(|s| s.strip_prefix("- verify_sha256: "))
        .collect::<Vec<_>>();
    Ok(if values.len() == 1 && hex(values[0], 64) {
        values[0].into()
    } else {
        String::new()
    })
}
fn dependencies(row: &Value) -> &Value {
    row.opt("dependency_digests")
        .unwrap_or(row.get("dependencies"))
}
fn worktree_head(state: &str) -> Option<&str> {
    let p = state.split(':').collect::<Vec<_>>();
    if p.len() != 3
        || !(p[0] == "unborn" || hex(p[0], 40) || hex(p[0], 64))
        || !hex(p[1], 64)
        || !hex(p[2], 64)
    {
        return None;
    }
    Some(if p[0] == "unborn" { "" } else { p[0] })
}
fn stale(rt: &Runtime<'_>, row: &Value) -> bool {
    let d = dependencies(row);
    if let Ok(v) = d.object() {
        if !v.is_empty() {
            return v.iter().any(|(k, w)| {
                let path = Value::String(k.clone());
                let Ok(path) = crate::value::path(&path) else {
                    return true;
                };
                rt.bytes(Path::new(&path), crate::storage::MAX_STATE)
                    .ok()
                    .flatten()
                    .is_none_or(|raw| sha(raw.as_slice()) != text(w))
            });
        }
    }
    let head = row
        .opt("verified_head")
        .or(row.opt("evidence_head"))
        .unwrap_or(if row.text("kind").eq_ignore_ascii_case("acceptance") {
            row.get("base_sha")
        } else {
            &NULL
        });
    !text(head).is_empty() && !rt.repository.head.is_empty() && text(head) != rt.repository.head
}
fn item(
    rt: &Runtime<'_>,
    row: &Value,
    support: &str,
    resolved: &BTreeSet<String>,
    criterion_digest: &str,
) -> Result<Value> {
    let explicit = support == "explicit-binding";
    let mut st = if explicit {
        row.text("status")
    } else {
        outcome(row).unwrap_or(if row.text("status").is_empty() {
            "inconclusive"
        } else {
            row.text("status")
        })
    };
    let mut is_stale = stale(rt, row);
    if explicit {
        if !hex(row.text("criterion_digest"), 64) || !hex(criterion_digest, 64) {
            if st == "verified" {
                st = "inconclusive"
            }
        } else {
            is_stale |= row.text("criterion_digest") != criterion_digest
        }
        if !dependencies(row).object().is_ok_and(|v| !v.is_empty()) {
            let stored = row.text("verified_state");
            let stored_head = worktree_head(stored);
            if stored_head.is_none()
                || stored_head != Some(row.text("verified_head"))
                || worktree_head(&rt.repository.worktree_state) != Some(rt.repository.head.as_str())
            {
                if st == "verified" {
                    st = "inconclusive"
                }
            } else {
                is_stale |= stored != rt.repository.worktree_state
            }
        }
    }
    if st == "failed" && row.text("_source") == INDEX && resolved.contains(row.text("event_id")) {
        is_stale = true
    }
    if is_stale {
        st = "stale"
    }
    let kind = text(row.opt("kind").unwrap_or(row.get("evidence_type"))).to_lowercase();
    let signal = if explicit {
        "parent_attestation"
    } else if ADVISORY.contains(&kind.as_str())
        || kind == "delegate" && row.text("context_verification") == "none"
    {
        "advisory"
    } else if kind == "delegate" {
        if (row.opt("context_verification").is_none()
            || row.text("context_verification") == "command")
            && row.get("exit").int().is_ok()
            && row.get("verify_exit").int().is_ok()
        {
            "execution_feedback"
        } else {
            "unknown"
        }
    } else if matches!(kind.as_str(), "review-verify" | "acceptance") {
        if exit(row.get("exit")).is_some() {
            "execution_feedback"
        } else {
            "unknown"
        }
    } else if kind == "patch-admit" {
        if matches!(outcome(row), Some("verified" | "failed")) {
            "execution_feedback"
        } else {
            "unknown"
        }
    } else if matches!(
        support,
        "patch-admission-receipt" | "fresh-task-gate" | "plan-acceptance-receipt"
    ) {
        "execution_feedback"
    } else {
        "unknown"
    };
    let reference = evidence_ref(row);
    Ok(object([
        (
            "evidence_ref",
            string(if reference.is_empty() {
                bounded(row.text("binding_id"), 160)
            } else {
                reference
            }),
        ),
        ("binding_id", string(bounded(row.text("binding_id"), 160))),
        (
            "kind",
            string(bounded(
                text(
                    row.opt("kind")
                        .or(row.opt("evidence_type"))
                        .unwrap_or(&string("artifact")),
                ),
                80,
            )),
        ),
        ("provider", string(bounded(row.text("provider"), 80))),
        ("status", string(st)),
        ("signal", string(signal)),
        ("stale", b(is_stale)),
        (
            "ts",
            string(bounded(
                text(row.opt("ts").unwrap_or(row.get("created_at"))),
                40,
            )),
        ),
        (
            "scope_digest",
            string(bounded(
                text(
                    row.opt("scope_digest")
                        .or(row.opt("reviewed_diff_sha256"))
                        .unwrap_or(row.get("patch_sha256")),
                ),
                80,
            )),
        ),
        ("support", string(support)),
        ("source", string(bounded(row.text("_source"), 200))),
        ("_ordinal", number(row.get("_ordinal").int().unwrap_or(0))),
    ]))
}
fn active(rt: &Runtime<'_>) -> Result<Vec<Value>> {
    let rows = rt.rows(BINDINGS)?;
    let revoked = rows
        .iter()
        .filter(|r| r.text("action") == "revoke")
        .map(|r| r.text("revokes"))
        .collect::<BTreeSet<_>>();
    Ok(rows
        .iter()
        .filter(|r| {
            matches!(r.text("action"), "" | "bind")
                && !r.text("binding_id").is_empty()
                && !revoked.contains(r.text("binding_id"))
        })
        .cloned()
        .collect())
}
fn lineage(criterion: &Value, row: &Value) -> bool {
    match criterion.text("source") {
        "plan-task" => {
            let p = criterion.text("plan_id");
            p.len() == 37 && p.starts_with("plan_") && hex(&p[5..], 32) && row.text("plan_id") == p
        }
        "task" => {
            let t = criterion.text("active_task_id");
            !t.is_empty()
                && t.len() <= 160
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
                && row.text("active_task_id") == t
        }
        _ => true,
    }
}
pub fn coverage(rt: &Runtime<'_>, base: &Value) -> Result<Value> {
    let criteria = rows(base.get("criteria"));
    let by_id = criteria
        .iter()
        .map(|c| (c.text("id"), c))
        .collect::<BTreeMap<_, _>>();
    let artifacts = artifacts(rt)?;
    let resolved = resolved_ids(&artifacts);
    let by_ref = artifacts
        .iter()
        .map(|r| (evidence_ref(r), r))
        .collect::<BTreeMap<_, _>>();
    let verifiers = rows(base.get("plan").get("tasks"))
        .iter()
        .map(|r| (r.text("id"), r.text("verify_digest")))
        .collect::<BTreeMap<_, _>>();
    let mut links = BTreeMap::<String, Vec<Value>>::new();
    let mut unbound = vec![];
    for original in &artifacts {
        let mut row = original.clone();
        if row.text("kind").eq_ignore_ascii_case("patch-admit") && outcome(&row) == Some("verified")
        {
            let digest = admission_digest(rt, &row)?;
            if digest.is_empty()
                || !row.text("task_id").is_empty()
                    && Some(&digest.as_str()) != verifiers.get(row.text("task_id"))
            {
                row.set("status", string("inconclusive"))?
            }
        }
        let mut matched = refs(&row)
            .into_iter()
            .filter(|id| by_id.get(id.as_str()).is_some_and(|c| lineage(c, &row)))
            .collect::<Vec<_>>();
        if row.text("kind").eq_ignore_ascii_case("patch-admit") && outcome(&row) == Some("verified")
        {
            let digest = admission_digest(rt, &row)?;
            if matched
                .iter()
                .filter_map(|id| by_id.get(id.as_str()))
                .any(|c| {
                    c.text("source") == "plan-task"
                        && verifiers.get(c.text("plan_task_id")).copied() != Some(digest.as_str())
                })
            {
                row.set("status", string("inconclusive"))?;
            }
        }
        let mut support = "explicit-artifact";
        if matched.is_empty()
            && row.text("kind") == "acceptance"
            && !row.text("accept_sha256").is_empty()
        {
            matched = criteria
                .iter()
                .filter(|c| {
                    c.text("source") == "plan"
                        && c.text("command_digest")
                            .starts_with(row.text("accept_sha256"))
                })
                .map(|c| c.text("id").into())
                .collect();
            if !matched.is_empty() {
                support = "plan-acceptance-receipt"
            }
        }
        if matched.is_empty()
            && row.text("kind") == "patch-admit"
            && !row.text("task_id").is_empty()
        {
            matched = criteria
                .iter()
                .filter(|c| {
                    c.text("source") == "plan-task"
                        && c.text("plan_task_id") == row.text("task_id")
                        && lineage(c, &row)
                })
                .map(|c| c.text("id").into())
                .collect();
            if !matched.is_empty() {
                support = "patch-admission-receipt"
            }
        }
        if matched.is_empty() {
            if outcome(&row).is_some() {
                unbound.push(item(rt, &row, support, &resolved, "")?)
            }
        } else {
            let ev = item(rt, &row, support, &resolved, "")?;
            for id in matched {
                links.entry(id).or_default().push(ev.clone())
            }
        }
    }
    let bindings = active(rt)?;
    for (index, binding) in bindings.iter().enumerate() {
        let Some(criterion) = by_id.get(binding.text("criterion_id")) else {
            continue;
        };
        if !lineage(criterion, binding) {
            continue;
        }
        let source = by_ref
            .get(binding.text("evidence_ref"))
            .copied()
            .cloned()
            .unwrap_or_else(empty);
        let mut merged = merge(&source, binding)?;
        merged.set("verified_state", binding.get("verified_state").clone())?;
        merged.set("criterion_digest", binding.get("criterion_digest").clone())?;
        merged.set("_ordinal", number(artifacts.len() + index + 1))?;
        links
            .entry(binding.text("criterion_id").into())
            .or_default()
            .push(item(
                rt,
                &merged,
                "explicit-binding",
                &resolved,
                criterion.text("criterion_digest"),
            )?);
    }
    let task = base.get("task");
    if task.text("verification") == "fresh" {
        for c in criteria.iter().filter(|c| c.text("source") == "task") {
            let id = c.text("id");
            if links.get(id).is_none_or(Vec::is_empty) {
                links.entry(id.into()).or_default().push(object([
                    (
                        "evidence_ref",
                        string(format!("task-verification:{}", task.text("task_id"))),
                    ),
                    ("kind", string("task-verification")),
                    ("provider", string("")),
                    ("status", string("verified")),
                    ("signal", string("execution_feedback")),
                    ("stale", b(false)),
                    (
                        "ts",
                        string(bounded(
                            text(
                                task.get("metadata")
                                    .opt("updated")
                                    .unwrap_or(task.get("metadata").get("last_activity")),
                            ),
                            40,
                        )),
                    ),
                    ("scope_digest", task.get("verify_digest").clone()),
                    ("support", string("fresh-task-gate")),
                    ("source", string(".oms/task/current.md")),
                    ("_ordinal", number(artifacts.len() + bindings.len() + 1)),
                ]));
            }
        }
    }
    let mut counts = BTreeMap::<String, usize>::new();
    let mut projected = vec![];
    let (mut total, mut verified, mut risk) = (0.0, 0.0, 0.0);
    for criterion in criteria {
        let mut items = links.remove(criterion.text("id")).unwrap_or_default();
        items.sort_by(|a, b| {
            (a.text("ts"), a.get("_ordinal").int().unwrap_or(0))
                .cmp(&(b.text("ts"), b.get("_ordinal").int().unwrap_or(0)))
        });
        if items.len() > 20 {
            items.drain(..items.len() - 20);
        }
        let native = items
            .iter()
            .map(|v| oms_context::parse_json(&canonical(v)?).map_err(error))
            .collect::<Result<Vec<_>>>()?;
        let state = oms_work_inspect::criterion_evidence_status(&native).map_err(error)?;
        *counts.entry(state.into()).or_default() += 1;
        let weight = finite(criterion.get("weight")).unwrap_or(1.0);
        require(
            weight >= 0.0,
            "criterion weight must be finite and non-negative",
        )?;
        total += weight;
        if state == "verified" {
            verified += weight
        }
        risk += weight
            * match state {
                "failed" => 1.0,
                "missing" => 0.8,
                "stale" => 0.7,
                "inconclusive" => 0.5,
                "skipped_with_reason" => 0.4,
                _ => 0.0,
            };
        let mut c = criterion.clone();
        c.set("status", string(state))?;
        c.set(
            "evidence",
            a(items.iter().map(|v| without(v, &["_ordinal"]))),
        )?;
        projected.push(c)
    }
    let complete =
        !projected.is_empty() && projected.iter().all(|c| c.text("status") == "verified");
    if unbound.len() > 50 {
        unbound.drain(..unbound.len() - 50);
    }
    Ok(object([
        ("schema", number(1)),
        ("criteria", a(projected)),
        (
            "counts",
            Value::Object(
                counts
                    .into_iter()
                    .map(|(k, v)| (k.into_bytes(), number(v)))
                    .collect(),
            ),
        ),
        (
            "coverage",
            number(if total > 0.0 { verified / total } else { 0.0 }),
        ),
        (
            "risk_score",
            number(if total > 0.0 { risk / total } else { 1.0 }),
        ),
        ("complete", b(complete)),
        ("unbound_evidence", a(unbound)),
        ("artifact_rows_seen", number(artifacts.len())),
    ]))
}
pub struct Binding<'a> {
    pub criterion: &'a str,
    pub reference: &'a str,
    pub status: &'a str,
    pub evidence_type: &'a str,
    pub note: &'a str,
    pub dependencies: &'a [String],
}
pub fn bind(rt: &Runtime<'_>, request: Binding<'_>) -> Result<Value> {
    rt.parent_write()?;
    safe_id(request.criterion, "criterion id")?;
    safe_id(request.reference, "evidence ref")?;
    require(
        [
            "verified",
            "failed",
            "inconclusive",
            "skipped_with_reason",
            "stale",
        ]
        .contains(&request.status),
        "unsupported evidence status",
    )?;
    let mut base = crate::projection::base(rt)?;
    let mut criterion = rows(base.get("criteria"))
        .iter()
        .find(|c| c.text("id") == request.criterion)
        .cloned()
        .ok_or_else(|| error("unknown current acceptance criterion"))?;
    if criterion.text("source") == "plan-task" && !lineage(&criterion, &criterion) {
        rt.services.ensure_plan_lineage(&rt.repo, &rt.state_repo)?;
        rt.invalidate(Path::new(".oms/plan/tasks.json"));
        base = crate::projection::base(rt)?;
        criterion = rows(base.get("criteria"))
            .iter()
            .find(|c| c.text("id") == request.criterion)
            .cloned()
            .ok_or_else(|| error("plan changed while establishing evidence lineage"))?;
        require(
            lineage(&criterion, &criterion),
            "plan changed while establishing evidence lineage",
        )?;
    }
    require(
        criterion.text("source") != "task" || lineage(&criterion, &criterion),
        "current task criterion has no valid active task lineage",
    )?;
    require(
        hex(criterion.text("criterion_digest"), 64),
        "current acceptance criterion has no valid statement fingerprint",
    )?;
    let task_ref = format!("task-verification:{}", base.get("task").text("task_id"));
    let artifacts = artifacts(rt)?;
    require(
        artifacts
            .iter()
            .any(|r| evidence_ref(r) == request.reference)
            || request.reference == task_ref,
        "evidence reference does not exist in current OMS receipts",
    )?;
    if request.reference == task_ref {
        require(
            base.get("task").text("verification") == "fresh",
            "task verification is not fresh and cannot be bound",
        )?;
    }
    let mut deps = empty();
    for name in oms_contract_markdown::parse_paths(request.dependencies.iter().map(String::as_str))
    {
        let raw = rt
            .bytes(&name, crate::storage::MAX_STATE)?
            .ok_or_else(|| error("evidence dependency is not a regular file"))?;
        deps.set(&name, string(sha(raw.as_slice())))?;
    }
    let current = worktree_head(&rt.repository.worktree_state) == Some(rt.repository.head.as_str());
    require(
        deps.truth() || current || request.status != "verified",
        "cannot bind verified evidence without a readable, current worktree fingerprint",
    )?;
    let mut row = object([
        ("schema", number(1)),
        ("binding_id", string(random_id("binding-")?)),
        ("action", string("bind")),
        ("created_at", string(&rt.now)),
        ("criterion_id", string(request.criterion)),
        (
            "criterion_digest",
            criterion.get("criterion_digest").clone(),
        ),
        ("evidence_ref", string(request.reference)),
        ("status", string(request.status)),
        ("evidence_type", string(bounded(request.evidence_type, 80))),
        ("note", string(bounded(request.note, 300))),
        (
            "verified_head",
            if deps.truth() {
                Value::Null
            } else if current {
                optional(&rt.repository.head)
            } else {
                Value::Null
            },
        ),
        ("dependency_digests", deps.clone()),
    ]);
    if !deps.truth() {
        row.set(
            "verified_state",
            if current {
                string(&rt.repository.worktree_state)
            } else {
                Value::Null
            },
        )?;
    }
    for key in ["plan_id", "active_task_id"] {
        if criterion.get(key).truth() {
            row.set(key, criterion.get(key).clone())?;
        }
    }
    rt.append(Path::new(BINDINGS), &row)?;
    Ok(row)
}
pub fn revoke(rt: &Runtime<'_>, id: &str, reason: &str) -> Result<Value> {
    safe_id(id, "binding id")?;
    let _guard = rt.lock(Path::new(BINDINGS))?;
    rt.invalidate(Path::new(BINDINGS));
    let current = active(rt)?
        .into_iter()
        .find(|r| r.text("binding_id") == id)
        .ok_or_else(|| error(format!("active evidence binding not found: {id}")))?;
    let row = object([
        ("schema", number(1)),
        ("binding_id", string(random_id("revocation-")?)),
        ("action", string("revoke")),
        ("created_at", string(&rt.now)),
        ("revokes", string(id)),
        ("criterion_id", current.get("criterion_id").clone()),
        ("evidence_ref", current.get("evidence_ref").clone()),
        (
            "reason",
            string(bounded(
                if reason.is_empty() {
                    "binding no longer applies"
                } else {
                    reason
                },
                300,
            )),
        ),
    ]);
    rt.append_locked(Path::new(BINDINGS), &row)?;
    Ok(row)
}
