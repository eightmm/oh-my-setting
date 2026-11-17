//! Bounded, untrusted discovery packets and candidate eligibility. A source
//! citation is checked against current tracked regular bytes, never executed.
use crate::contract::Contract;
use crate::planning::{invoke, strings};
use crate::storage::{Context, MAX_FILE};
use crate::{Error, Result, require, value};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

pub const BUDGETS: &[(&str, usize)] = &[
    ("failures", 16384),
    ("metrics", 4096),
    ("reviews", 65536),
    ("ci", 4096),
    ("tasks", 4096),
    ("rejected", 32768),
];
pub fn validate_packet(packet: &Value) -> Result<()> {
    require(
        packet["schema"].as_u64() == Some(1) && packet["sections"].is_object(),
        "discovery packet has invalid schema",
    )?;
    for (name, cap) in BUDGETS {
        let section = &packet["sections"][name];
        value::keys(section, &["status", "reason", "coverage", "items"], &[])?;
        require(
            ["available", "unavailable"].contains(&value::text(section, "status"))
                && section["items"].is_array()
                && section["coverage"].is_object(),
            "discovery section shape is invalid",
        )?;
        require(
            value::canonical(section)?.len() <= *cap,
            format!("discovery {name} exceeds its {cap}-byte budget"),
        )?;
    }
    require(
        value::canonical(packet)?.len() <= 131072,
        "discovery total exceeds 131072-byte budget",
    )
}
fn section(items: Vec<Value>, coverage: Value, reason: Option<&str>) -> Value {
    json!({"status": if reason.is_none() { "available" } else { "unavailable" }, "reason": reason, "coverage": coverage, "items": items})
}
pub(crate) fn collect(ctx: &Context<'_>) -> Result<Value> {
    let plan = ctx.plan_value()?;
    let tasks = plan["tasks"]
        .as_object()
        .ok_or_else(|| Error::new(3, "corrupt tasks evidence"))?;
    let task_items = tasks
        .values()
        .map(|task| {
            let mut selected = serde_json::Map::new();
            for key in ["id", "state", "title", "depends", "claim_expired", "reason"] {
                selected.insert(key.into(), task.get(key).cloned().unwrap_or(Value::Null));
            }
            Value::Object(selected)
        })
        .collect::<Vec<_>>();
    let mut sections = json!({});
    sections["tasks"] = section(
        task_items,
        json!({"count": tasks.len(), "open": tasks.values().filter(|t| !["done", "cancelled"].contains(&value::text(t, "state"))).count(), "terminal": tasks.values().filter(|t| ["done", "cancelled"].contains(&value::text(t, "state"))).count()}),
        None,
    );
    let memory = invoke(
        ctx,
        "agent-memory",
        vec![
            "--repo".into(),
            ctx.repo.as_os_str().to_owned(),
            "--full".into(),
            "show".into(),
        ],
        Duration::from_secs(30),
    )?;
    require(memory.code == 0, "corrupt rejected-hypothesis evidence")?;
    let memory = std::str::from_utf8(&memory.stdout)
        .map_err(|_| Error::new(3, "rejected evidence must be UTF-8"))?;
    let mut rejected = vec![];
    let mut verdicts = BTreeMap::<String, u64>::new();
    for (index, line) in memory.lines().enumerate() {
        if !line.starts_with("hypothesis:") {
            continue;
        }
        let parts = line.split('|').map(str::trim).collect::<Vec<_>>();
        let verdict = parts
            .iter()
            .skip(1)
            .find_map(|part| part.strip_prefix("verdict:"))
            .or_else(|| parts.get(3).copied())
            .unwrap_or("unknown")
            .split_whitespace()
            .next()
            .unwrap_or("unknown")
            .to_lowercase();
        *verdicts.entry(verdict.clone()).or_default() += 1;
        rejected.push(json!({"ref": format!("line-{}-{}", index + 1, &value::hash(line)[..16]), "line": index + 1, "text": line, "verdict": verdict}));
    }
    sections["rejected"] = section(
        rejected.clone(),
        json!({"complete": true, "hypotheses": rejected.len(), "verdict_counts": verdicts}),
        None,
    );
    match invoke(
        ctx,
        "fail-ledger",
        vec![
            "--repo".into(),
            ctx.repo.as_os_str().to_owned(),
            "list".into(),
            "--unresolved".into(),
            "--json".into(),
        ],
        Duration::from_secs(30),
    ) {
        Ok(output) if output.code == 0 => {
            let document = value::parse(&output.stdout)?;
            let rows = document
                .as_array()
                .or_else(|| document["failures"].as_array())
                .ok_or_else(|| Error::new(3, "failure ledger returned invalid JSON"))?;
            let mut selected = vec![];
            for row in rows.iter().take(16) {
                let summary = oms_contract_markdown::bounded_line(value::text(row, "summary"), 240);
                selected.push(json!({"fingerprint": row["fingerprint"], "count": row["count"], "kind": row["kind"], "attention": row["attention"], "summary": summary, "last_exit": row["exit"], "ref": row.get("fingerprint").cloned().unwrap_or_else(|| value::hash(value::canonical(row).unwrap_or_default()).into())}));
            }
            sections["failures"] = section(
                selected,
                json!({"count": rows.len(), "selected": rows.len().min(16), "complete": rows.len() <= 16, "land_scan_complete": false}),
                None,
            );
        }
        _ => {
            sections["failures"] = section(
                vec![],
                json!({"complete": false}),
                Some("native failure evidence unavailable"),
            )
        }
    }
    // These producers own their projections. Missing adapters are explicit
    // coverage gaps and cannot be treated as negative evidence by the planner.
    for name in ["metrics", "reviews", "ci"] {
        sections[name] = match ctx
            .services
            .discovery_section(&ctx.repo, name, ctx.interrupted)
        {
            Ok(section) => section,
            Err(_) => section(
                vec![],
                json!({"complete": false}),
                Some("native producer projection unavailable"),
            ),
        };
    }
    let mut packet = json!({"schema": 1, "sections": sections, "bytes": 0});
    loop {
        let size = value::canonical(&packet)?.len();
        if packet["bytes"].as_u64() == Some(size as u64) {
            break;
        }
        packet["bytes"] = size.into();
    }
    validate_packet(&packet)?;
    Ok(packet)
}
fn source_ref(ctx: &Context<'_>, reference: &str) -> bool {
    let Some(rest) = reference.strip_prefix("src:") else {
        return false;
    };
    let Some((path, line)) = rest.rsplit_once(':') else {
        return false;
    };
    let Ok(number) = line.parse::<usize>() else {
        return false;
    };
    if number == 0 || crate::contract::path(path).ok().as_deref() != Some(path) {
        return false;
    }
    let Ok(raw) = ctx.source.read(path, MAX_FILE) else {
        return false;
    };
    if raw.split(|b| *b == b'\n').count() < number {
        return false;
    }
    let Ok(tracked) = ctx.plan().and_then(|p| {
        p.git()
            .run(&["ls-files", "--stage", "-z", "--", path])
            .map_err(Into::into)
    }) else {
        return false;
    };
    let Some(at) = tracked.iter().position(|b| *b == b'\t') else {
        return false;
    };
    let (meta, name) = (&tracked[..at], &tracked[at + 1..]);
    name.strip_suffix(b"\0") == Some(path.as_bytes())
        && (meta.starts_with(b"100644 ") || meta.starts_with(b"100755 "))
}
pub(crate) fn normalize(
    ctx: &Context<'_>,
    answer: &Value,
    packet: &Value,
    spec: &Contract,
) -> Result<(Vec<Value>, Value)> {
    validate_packet(packet)?;
    let candidates = answer["candidates"]
        .as_array()
        .ok_or_else(|| Error::new(3, "discover answer requires candidates array"))?;
    let tasks = answer["tasks"]
        .as_array()
        .ok_or_else(|| Error::new(3, "discover answer requires tasks array"))?;
    let existing = packet["sections"]["tasks"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| value::text(r, "id"))
        .collect::<BTreeSet<_>>();
    let rejected = packet["sections"]["rejected"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| {
            r["line"]
                .as_u64()
                .map(|line| (line, value::text(r, "verdict")))
        })
        .collect::<BTreeMap<_, _>>();
    let refs = packet["sections"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|s| s["items"].as_array().unwrap())
        .filter_map(|r| r["ref"].as_str())
        .collect::<BTreeSet<_>>();
    let mut statuses = BTreeMap::new();
    let mut normalized = vec![];
    let line_re = regex::Regex::new(r"line\s+([0-9]+)").unwrap();
    for candidate in candidates {
        let id = value::text(candidate, "id");
        require(
            value::id(id, 160) && !statuses.contains_key(id),
            "candidate ids must be unique",
        )?;
        for key in [
            "claim",
            "lens",
            "counterevidence",
            "smallest_check",
            "scope",
            "expected_benefit",
            "expected_cost",
            "duplicate_disposition",
        ] {
            require(
                candidate[key]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty()),
                "candidate report field is missing or empty",
            )?;
        }
        require(
            ["imagination", "gap critique", "pruning", "spec gap"]
                .contains(&value::text(candidate, "lens")),
            "candidate lens is invalid",
        )?;
        let evidence = strings(&candidate["evidence_refs"])?;
        require(
            !evidence.is_empty(),
            "candidate evidence_refs must be nonempty",
        )?;
        let disposition = value::text(candidate, "duplicate_disposition")
            .trim()
            .to_lowercase();
        let mut status = if existing.contains(id) {
            "dropped_existing_task_id"
        } else if disposition == "new" {
            "eligible"
        } else if let Some(other) = disposition.strip_prefix("duplicates task ") {
            require(
                existing.contains(other),
                "candidate cites an unknown duplicate task",
            )?;
            "suppressed_duplicate_task"
        } else if disposition.starts_with("matches rejected hypothesis") {
            let line = line_re
                .captures(&disposition)
                .and_then(|m| m[1].parse::<u64>().ok())
                .ok_or_else(|| Error::new(3, "candidate cites unknown rejected-hypothesis line"))?;
            let verdict = rejected
                .get(&line)
                .ok_or_else(|| Error::new(3, "candidate cites unknown rejected-hypothesis line"))?;
            if *verdict == "rejected" {
                "suppressed_rejected_hypothesis"
            } else {
                "held_uncertain_match"
            }
        } else {
            "held_uncertain_match"
        };
        if status == "eligible" && candidate["lens"] == "spec gap" {
            if !spec
                .criteria
                .contains_key(value::text(candidate, "criterion"))
            {
                status = "unknown_criterion_report_only";
            } else if !evidence.iter().any(|r| source_ref(ctx, r)) {
                status = "unresolved_evidence_report_only";
            }
        }
        if status == "eligible" && evidence.iter().any(|r| r == "speculative") {
            status = "speculative_report_only";
        } else if status == "eligible"
            && evidence
                .iter()
                .any(|r| !refs.contains(r.as_str()) && !source_ref(ctx, r))
        {
            status = "unresolved_evidence_report_only";
        }
        statuses.insert(id.to_owned(), status);
        let mut row = candidate.clone();
        row["disposition_status"] = status.into();
        normalized.push(row);
    }
    let mut eligible = vec![];
    let mut used = BTreeSet::new();
    for task in tasks {
        let id = value::text(task, "candidate_id");
        let status = statuses
            .get(id)
            .ok_or_else(|| Error::new(3, "every task must map to a known candidate"))?;
        require(
            *status != "speculative_report_only",
            "task maps to ineligible speculative candidate",
        )?;
        if *status == "eligible" {
            let mut row = task.clone();
            row.as_object_mut()
                .ok_or_else(|| Error::new(3, "task must be an object"))?
                .remove("candidate_id");
            eligible.push(row);
            used.insert(id);
        }
    }
    for row in &mut normalized {
        if row["disposition_status"] == "eligible" && !used.contains(value::text(row, "id")) {
            row["disposition_status"] = "eligible_no_task".into();
        }
    }
    let report = json!({"schema": 1, "kind": "work-discovery-candidate-report", "packet_sha256": value::hash(value::canonical(packet)?), "candidates": normalized});
    Ok((eligible, report))
}
