//! Content-free effectiveness measurements; observational cohorts grant no authority.
use crate::evidence::outcome;
use crate::storage::{Runtime, safe_id};
use crate::value::*;
use crate::{Result, error, require};
use std::collections::BTreeMap;
use std::path::Path;
pub const METRICS: &[&str] = &[
    "human_corrections",
    "escaped_defects",
    "reverted_lines",
    "false_refusals",
    "duplicate_work",
];
pub fn first_number(value: &Value, keys: &[&str]) -> Option<f64> {
    match value {
        Value::Object(items) => {
            for (k, v) in items {
                if keys.iter().any(|s| s.as_bytes() == k)
                    && let Some(n) = finite(v).filter(|n| *n >= 0.0)
                {
                    return Some(n);
                }
                if let Some(n) = first_number(v, keys) {
                    return Some(n);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|v| first_number(v, keys)),
        _ => None,
    }
}
pub fn call_units(row: &Value) -> Option<f64> {
    if let Some(n) = first_number(row, &[concat!("total_to", "kens")]) {
        return Some(n);
    }
    let usage = row.get("provider_usage");
    if usage.object().is_ok() {
        let mut keys = vec![concat!("input_to", "kens"), concat!("output_to", "kens")];
        if usage.get("cache_in_input") == &b(false) {
            keys.extend([
                concat!("cache_read_to", "kens"),
                concat!("cache_write_to", "kens"),
            ]);
        } else if usage.get("cache_in_input") != &b(true) {
            return None;
        }
        return keys
            .iter()
            .map(|key| first_number(usage, &[key]))
            .collect::<Option<Vec<_>>>()
            .map(|v| v.iter().sum());
    }
    let input = first_number(
        row,
        &[concat!("input_to", "kens"), concat!("prompt_to", "kens")],
    );
    let output = first_number(
        row,
        &[
            concat!("output_to", "kens"),
            concat!("completion_to", "kens"),
        ],
    );
    if input.is_none() && output.is_none() {
        first_number(row, &[concat!("to", "kens")])
    } else {
        input.zip(output).map(|(l, r)| l + r)
    }
}
fn identity(row: &Value) -> Option<(&str, &str)> {
    if row.get("fallback_used") == &b(true) || row.text("model_attribution") == "ambiguous" {
        return None;
    }
    for (key, source) in [
        ("served_model", "transport"),
        ("configured_model", "configured-default"),
        ("selected_model", "selected"),
        ("requested_model", "selected"),
    ] {
        let name = row.text(key);
        if !name.is_empty() && name != "provider-default" {
            return Some((name, source));
        }
    }
    None
}
fn provider_call(row: &Value) -> bool {
    matches!(row.text("kind"), "call" | "ask" | "review" | "delegate")
        || row.text("kind") == "review-synthesis" && !matches!(row.text("provider"), "" | "local")
}
fn verification(row: &Value) -> Option<&'static str> {
    if row.text("kind") == "delegate" {
        if row.text("context_verification") == "command" {
            outcome(row)
        } else {
            None
        }
    } else if matches!(
        row.text("kind"),
        "review-verify" | "acceptance" | "task-verification"
    ) {
        outcome(row)
    } else {
        None
    }
}
fn completion(row: &Value) -> Option<&'static str> {
    if row.text("context_verification") == "dry-run" {
        None
    } else {
        outcome(&pick(row, &["status", "exit", "exit_code"]))
    }
}
fn counts(values: &[Option<&str>]) -> Value {
    let passed = values.iter().filter(|v| **v == Some("verified")).count();
    let failed = values.iter().filter(|v| **v == Some("failed")).count();
    object([
        ("count", number(values.len())),
        ("passed", number(passed)),
        ("failed", number(failed)),
        ("unknown", number(values.len() - passed - failed)),
        (
            "success_rate",
            if passed + failed == 0 {
                Value::Null
            } else {
                number(passed as f64 / (passed + failed) as f64)
            },
        ),
    ])
}
fn total(values: &[f64]) -> Value {
    if values.is_empty() {
        Value::Null
    } else {
        number(values.iter().sum::<f64>())
    }
}
fn average(values: &[f64]) -> Value {
    if values.is_empty() {
        Value::Null
    } else {
        number(values.iter().map(|v| v / values.len() as f64).sum::<f64>())
    }
}
fn counter<'a>(names: impl IntoIterator<Item = &'a str>) -> Value {
    let mut counts = BTreeMap::<String, usize>::new();
    for n in names {
        *counts.entry(n.into()).or_default() += 1
    }
    Value::Object(
        counts
            .into_iter()
            .map(|(k, v)| (k.into_bytes(), number(v)))
            .collect(),
    )
}
pub fn model_table(artifacts: &[Value]) -> Result<Value> {
    let mut models = BTreeMap::<String, Vec<(&Value, &str)>>::new();
    for row in artifacts.iter().filter(|r| provider_call(r)) {
        if let Some((name, source)) = identity(row) {
            let provider = if row.text("provider").is_empty() {
                "unknown"
            } else {
                row.text("provider")
            };
            models
                .entry(format!("{provider}/{name}"))
                .or_default()
                .push((row, source));
        }
    }
    let mut table = empty();
    for (key, rows) in models {
        let verification = rows
            .iter()
            .map(|(r, _)| verification(r))
            .collect::<Vec<_>>();
        let verified = verification
            .iter()
            .filter(|v| **v == Some("verified"))
            .count();
        let failed = verification
            .iter()
            .filter(|v| **v == Some("failed"))
            .count();
        let units = rows
            .iter()
            .filter_map(|(r, _)| call_units(r))
            .collect::<Vec<_>>();
        let costs = rows
            .iter()
            .filter_map(|(r, _)| first_number(r, &["cost_usd", "cost"]))
            .collect::<Vec<_>>();
        let durations = rows
            .iter()
            .filter_map(|(r, _)| first_number(r, &["duration_seconds", "duration_s"]))
            .collect::<Vec<_>>();
        let mut entry = object([
            ("calls", number(rows.len())),
            ("verified", number(verified)),
            ("failed", number(failed)),
            ("cost_usd", total(&costs)),
            ("cost_usd_count", number(costs.len())),
            ("sources", counter(rows.iter().map(|(_, s)| *s))),
            (
                "success_rate",
                if verified + failed == 0 {
                    Value::Null
                } else {
                    number(verified as f64 / (verified + failed) as f64)
                },
            ),
            (
                "completion",
                counts(&rows.iter().map(|(r, _)| completion(r)).collect::<Vec<_>>()),
            ),
            ("duration_seconds_mean", average(&durations)),
            ("duration_seconds_count", number(durations.len())),
        ]);
        entry.set(concat!("to", "kens"), total(&units))?;
        entry.set(concat!("to", "kens_count"), number(units.len()))?;
        table.set(&key, entry)?;
    }
    Ok(table)
}
fn repr_text(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        if c == quote || c == '\\' {
            out.push('\\');
            out.push(c)
        } else {
            match c {
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
                c => out.push(c),
            }
        }
    }
    out.push(quote);
    out
}
fn repr(v: &Value) -> Result<String> {
    match v {
        Value::Null => Ok("None".into()),
        Value::Bool(v) => Ok(if *v { "True" } else { "False" }.into()),
        Value::String(_) => Ok(repr_text(v.str()?)),
        _ => String::from_utf8(canonical(v)?).map_err(error),
    }
}
fn context_group(rows: &[&Value]) -> Result<Value> {
    let checks = rows
        .iter()
        .filter(|r| {
            r.text("context_verification") == "command"
                && r.get("verify_exit")
                    .int()
                    .is_ok_and(|n| (0..125).contains(&n))
        })
        .collect::<Vec<_>>();
    let verified = checks
        .iter()
        .filter(|r| outcome(r) == Some("verified"))
        .count();
    let mut out = object([
        ("calls", number(rows.len())),
        ("verified", number(verified)),
        ("checked", number(checks.len())),
        (
            "verification_rate",
            if checks.is_empty() {
                Value::Null
            } else {
                number(verified as f64 / checks.len() as f64)
            },
        ),
    ]);
    for field in [
        "context_prepare_seconds",
        "context_orientation_bytes",
        "prompt_bytes",
        "duration_seconds",
        concat!("to", "kens"),
        "cost_usd",
    ] {
        let samples = rows
            .iter()
            .filter_map(|row| {
                if field == concat!("to", "kens") {
                    call_units(row)
                } else if field == "duration_seconds" {
                    first_number(row, &["duration_seconds", "duration_s"])
                } else {
                    first_number(row, &[field])
                }
            })
            .collect::<Vec<_>>();
        out.set(
            field,
            object([
                ("count", number(samples.len())),
                ("mean", average(&samples)),
            ]),
        )?;
    }
    Ok(out)
}
pub fn context_modes(artifacts: &[Value]) -> Result<Value> {
    let mut modes = BTreeMap::<String, Vec<&Value>>::new();
    let mut cohorts = BTreeMap::<String, BTreeMap<String, Vec<&Value>>>::new();
    for row in artifacts {
        let mode = row.text("context_mode");
        if row.text("kind") != "delegate"
            || ![
                "direct",
                "pack",
                "graph",
                "graph-fallback",
                "bundle",
                "pack+bundle",
                "graph+bundle",
                "graph-fallback+bundle",
            ]
            .contains(&mode)
        {
            continue;
        }
        modes.entry(mode.into()).or_default().push(row);
        let Some((model, source)) = identity(row) else {
            continue;
        };
        let task = row.text("context_task_sha256");
        if row.text("context_verification") != "command"
            || !hex(task, 64)
            || !row.get("base_sha").truth()
        {
            continue;
        }
        let parts = [
            row.get("base_sha").clone(),
            row.get("provider").clone(),
            string(model),
            string(source),
            row.opt("selected_reasoning_effort")
                .unwrap_or(row.get("reasoning_effort"))
                .clone(),
            string(task),
        ];
        let key = sha(format!(
            "({})",
            parts
                .iter()
                .map(repr)
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ));
        cohorts
            .entry(key)
            .or_default()
            .entry(mode.into())
            .or_default()
            .push(row);
    }
    let mut mode_table = empty();
    for (mode, rows) in modes {
        mode_table.set(&mode, context_group(&rows)?)?;
    }
    let mut matched = empty();
    for (key, groups) in cohorts {
        if groups.len() < 2 {
            continue;
        }
        let mut modes = empty();
        for (mode, rows) in groups {
            modes.set(&mode, context_group(&rows)?)?;
        }
        matched.set(&key, modes)?;
    }
    Ok(object([
        ("modes", mode_table),
        ("matched_cohorts", matched),
        (
            "comparison",
            string("observational; matched cohorts do not prove causal savings"),
        ),
    ]))
}
fn skill_summary(rows: &[Value]) -> Result<Value> {
    let mut out = object([
        ("count", number(0)),
        ("task_pass_delta_sum", number(0)),
        ("treatment_task_passed", number(0)),
        ("trigger_false_negatives", number(0)),
        ("trigger_false_positives", number(0)),
        ("trigger_true_positives", number(0)),
        ("candidate_count", number(0)),
        ("candidate_task_pass_delta_sum", number(0)),
    ]);
    for (index, row) in rows.iter().enumerate() {
        require(
            row.get("schema").is_int(1) && row.get("skill").str().is_ok(),
            format!("skill evaluation row {} has an invalid schema", index + 1),
        )?;
        let task = row.get("task");
        let treatment = row.get("trigger").get("treatment");
        require(
            task.object().is_ok() && treatment.object().is_ok(),
            format!("skill evaluation row {} has invalid metrics", index + 1),
        )?;
        let values = [
            ("task_pass_delta_sum", task.get("pass_delta")),
            ("treatment_task_passed", task.get("treatment_passed")),
            ("trigger_false_negatives", treatment.get("false_negative")),
            ("trigger_false_positives", treatment.get("false_positive")),
            ("trigger_true_positives", treatment.get("true_positive")),
        ];
        for (_, v) in values {
            v.int()?;
        }
        let increment = |out: &mut Value, key: &str, value: i128| -> Result<()> {
            out.set(
                key,
                number(
                    out.get(key)
                        .int()?
                        .checked_add(value)
                        .ok_or_else(|| error("benchmark integer overflow"))?,
                ),
            )
        };
        if row.get("comparison").text("mode") == "candidate" {
            increment(&mut out, "candidate_count", 1)?;
            increment(
                &mut out,
                "candidate_task_pass_delta_sum",
                task.get("pass_delta").int()?,
            )?;
        } else {
            increment(&mut out, "count", 1)?;
            for (k, v) in values {
                increment(&mut out, k, v.int()?)?;
            }
        }
    }
    Ok(out)
}
pub fn record(
    rt: &Runtime<'_>,
    task: &str,
    status: &str,
    metrics: &Value,
    note: &str,
) -> Result<Value> {
    safe_id(task, "task id")?;
    require(
        ["verified", "failed", "partial", "blocked"].contains(&status),
        "unsupported effectiveness outcome status",
    )?;
    for key in METRICS {
        require(
            metrics.get(key).int().is_ok_and(|n| n >= 0),
            format!("{key} must be a non-negative integer"),
        )?;
    }
    let row = object([
        ("schema", number(1)),
        ("created_at", string(&rt.now)),
        ("task_id", string(task)),
        ("status", string(status)),
        ("metrics", metrics.clone()),
        (
            "note_digest",
            string(if note.is_empty() {
                String::new()
            } else {
                sha(bounded(note, 300))
            }),
        ),
    ]);
    rt.append(Path::new(".oms/runtime/outcomes.jsonl"), &row)?;
    Ok(row)
}
pub fn snapshot(rt: &Runtime<'_>) -> Result<Value> {
    let artifacts = rt.rows(".oms/artifacts/index.jsonl")?;
    let events = rt.rows(".oms/lifecycle/events.jsonl")?;
    let manual = rt.rows(".oms/runtime/outcomes.jsonl")?;
    let skill = rt.rows(".oms/runtime/skill-evals.jsonl")?;
    let mut contexts = vec![];
    for name in rt.names(".oms/runtime/context")? {
        if !name.to_string_lossy().ends_with(".json") {
            continue;
        }
        let path = Path::new(".oms/runtime/context").join(&name);
        let row = rt.json(&path, Value::Null)?;
        if row.object().is_err() {
            continue;
        }
        let stamp = chrono::DateTime::parse_from_rfc3339(row.text("generated_at")).ok();
        contexts.push((stamp, name, row));
    }
    contexts.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    contexts.truncate(1000);
    let envelope = crate::projection::envelope(rt)?;
    let calls = artifacts
        .iter()
        .filter(|r| provider_call(r))
        .collect::<Vec<_>>();
    let decided = artifacts
        .iter()
        .filter_map(verification)
        .filter(|s| ["verified", "failed"].contains(s))
        .collect::<Vec<_>>();
    let verified = decided.iter().filter(|s| **s == "verified").count();
    let mut durations = calls
        .iter()
        .filter_map(|r| first_number(r, &["duration_seconds", "duration_s"]))
        .collect::<Vec<_>>();
    if durations.is_empty() {
        durations = events
            .iter()
            .filter_map(|r| first_number(r, &["duration_seconds", "duration_s"]))
            .collect();
    }
    let units = calls
        .iter()
        .filter_map(|r| call_units(r))
        .collect::<Vec<_>>();
    let costs = calls
        .iter()
        .filter_map(|r| first_number(r, &["cost_usd", "cost"]))
        .collect::<Vec<_>>();
    let selected = contexts
        .iter()
        .filter_map(|(_, _, r)| finite(r.get("selected_bytes")))
        .collect::<Vec<_>>();
    let debt = contexts
        .iter()
        .filter_map(|(_, _, r)| finite(r.get("context_debt")))
        .collect::<Vec<_>>();
    let verified_weight = rows(envelope.get("criteria"))
        .iter()
        .filter(|c| c.text("status") == "verified")
        .map(|c| finite(c.get("weight")).unwrap_or(1.0))
        .sum::<f64>();
    let mut metrics = empty();
    for key in METRICS {
        let sum = manual
            .iter()
            .filter_map(|r| r.get("metrics").get(key).int().ok())
            .filter(|n| *n >= 0)
            .try_fold(0i128, |sum, n| {
                sum.checked_add(n)
                    .ok_or_else(|| error("benchmark integer overflow"))
            })?;
        metrics.set(key, number(sum))?;
    }
    let mut acceptance = pick(
        envelope.get("evidence"),
        &["coverage", "risk_score", "complete"],
    );
    acceptance.set("verified_weight", number(verified_weight))?;
    let mut result = object([
        ("schema", number(1)),
        (
            "measurement_basis",
            string("provider-calls/explicit-verification-v1"),
        ),
        ("generated_at", string(&rt.now)),
        ("state_digest", envelope.get("state_digest").clone()),
        ("acceptance", acceptance),
        ("artifacts", number(artifacts.len())),
        ("lifecycle_events", number(events.len())),
        ("decided_outcomes", number(decided.len())),
        ("verified_outcomes", number(verified)),
        (
            "success_rate",
            if decided.is_empty() {
                Value::Null
            } else {
                number(verified as f64 / decided.len() as f64)
            },
        ),
        (
            "call_outcomes",
            counts(&calls.iter().map(|r| completion(r)).collect::<Vec<_>>()),
        ),
        (
            "review_outcomes",
            counts(
                &artifacts
                    .iter()
                    .filter(|r| r.text("kind") == "review-outcome")
                    .map(outcome)
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "providers",
            counter(
                artifacts
                    .iter()
                    .map(|r| r.text("provider"))
                    .filter(|s| !s.is_empty()),
            ),
        ),
        ("models", model_table(&artifacts)?),
        ("delegation_context", context_modes(&artifacts)?),
        (
            "artifact_kinds",
            counter(artifacts.iter().map(|r| {
                if r.opt("kind").is_none() {
                    "unknown"
                } else {
                    r.text("kind")
                }
            })),
        ),
        (
            "duration_seconds",
            object([
                ("count", number(durations.len())),
                ("sum", total(&durations)),
                ("mean", average(&durations)),
            ]),
        ),
        (
            "cost_usd",
            object([("count", number(costs.len())), ("sum", total(&costs))]),
        ),
        (
            "context",
            object([
                ("manifests", number(contexts.len())),
                ("sample_limit", number(1000)),
                ("selection", string("generated_at_desc_undated_last")),
                ("selected_bytes_sum", total(&selected)),
                ("selected_bytes_mean", average(&selected)),
                ("debt_sum", total(&debt)),
            ]),
        ),
        ("useful_work_efficiency", Value::Null),
        (
            "manual_outcomes",
            object([("count", number(manual.len())), ("totals", metrics)]),
        ),
        ("skill_evals", skill_summary(&skill)?),
        (
            "unknown_metrics",
            if manual.is_empty() {
                list(METRICS.iter().copied())
            } else {
                a([])
            },
        ),
    ]);
    result.set(
        concat!("to", "kens"),
        object([("count", number(units.len())), ("sum", total(&units))]),
    )?;
    Ok(result)
}
pub fn compare(left: &Value, right: &Value) -> Result<Value> {
    let mut fields = vec![
        "success_rate",
        "acceptance.coverage",
        "acceptance.risk_score",
        "context.selected_bytes_mean",
        concat!("to", "kens.sum"),
        "cost_usd.sum",
        "duration_seconds.sum",
        "skill_evals.task_pass_delta_sum",
    ];
    let extras = METRICS
        .iter()
        .map(|s| format!("manual_outcomes.totals.{s}"))
        .collect::<Vec<_>>();
    fields.extend(extras.iter().map(String::as_str));
    let mut changes = vec![];
    for field in fields {
        let get = |value: &Value| {
            field
                .split('.')
                .fold(value.clone(), |v, k| v.get(k).clone())
        };
        let l = get(left);
        let r = get(right);
        let delta = finite(&l)
            .zip(finite(&r))
            .map(|(l, r)| number(r - l))
            .unwrap_or(Value::Null);
        let mut item = object([
            ("field", string(field)),
            ("left", l),
            ("right", r),
            ("delta", delta),
        ]);
        if [
            "success_rate",
            concat!("to", "kens"),
            "cost_usd",
            "duration_seconds",
        ]
        .contains(&field.split('.').next().unwrap_or(""))
            && left.get("measurement_basis") != right.get("measurement_basis")
        {
            item.set("delta", Value::Null)?;
            item.set(
                "reason",
                string("measurement basis differs; not comparable"),
            )?;
        }
        changes.push(item)
    }
    Ok(object([("schema", number(1)), ("changes", a(changes))]))
}
