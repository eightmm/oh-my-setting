use crate::{
    json::{copy, field, object, string, text},
    packet,
};
use oms_context::Json;
use regex::bytes::Regex;
use std::{collections::BTreeSet, sync::OnceLock};

/// Pure normalized goal-lifecycle adapter. Plan and progress inputs have already
/// passed bounded no-follow reads. Malformed rows are ignored as in plan_retro;
/// callers must not turn filesystem uncertainty into proof of absence.
pub fn lifecycle(task: &[u8], plan: Option<&Json>, rows: &[Json], verification: &str) -> Json {
    let status = packet::effective_status(task);
    static EXIT: OnceLock<Regex> = OnceLock::new();
    let re = EXIT.get_or_init(|| Regex::new(r"\bexit: ([0-9]+)\b").unwrap());
    let mut last = Json::Null;
    for line in packet::lines(&packet::section(task, "Verification")) {
        if let Some(m) = re.captures(line) {
            let raw = &m[1];
            let start = raw.iter().position(|b| *b != b'0').unwrap_or(raw.len() - 1);
            last = Json::Number(String::from_utf8(raw[start..].to_vec()).unwrap());
        }
    }
    let mut evidence = vec![
        ("record", string("task-packet")),
        ("task_status", string(status)),
        ("verification", string(verification)),
        ("last_verify_exit", last),
    ];
    let (mut state, rule) = if status == b"verified" && verification == "fresh" {
        ("achieved", "verified-fresh")
    } else if ![b"active".as_slice(), b"verified"].contains(&status) {
        ("unknown", "unmapped-status")
    } else {
        ("pursuing", "not-verified-fresh")
    };
    evidence.push(("rule", string(rule)));
    if state == "pursuing"
        && let Some(park) = overlay(plan, rows)
    {
        let reason = text(field(&park, "reason")).unwrap();
        state = match reason {
            b"cycles-exhausted" | b"tasks-exhausted" | b"replan-budget-used" => "budget-limited",
            b"stuck"
            | b"acceptance-vacuous"
            | b"acceptance-command-error"
            | b"acceptance-command-changed" => "unmet",
            _ => "paused",
        };
        evidence[0].1 = string("goal-drive");
        evidence[4].1 = string([b"park:".as_slice(), reason].concat());
        let budget = if state == "budget-limited" {
            string(reason)
        } else {
            Json::Null
        };
        evidence.push(("park", park));
        evidence.push(("budget_limit", budget));
    }
    object([("state", string(state)), ("evidence", object(evidence))])
}
fn clean(raw: &[u8], limit: usize) -> Vec<u8> {
    oms_contract_markdown::bounded_line(
        &String::from_utf8_lossy(raw)
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect::<String>(),
        limit,
    )
    .into_bytes()
}
fn overlay(plan: Option<&Json>, rows: &[Json]) -> Option<Json> {
    let Json::Object(tasks) = field(plan?, "tasks") else {
        return None;
    };
    let ids: BTreeSet<_> = tasks
        .iter()
        .filter_map(|(_, task)| text(field(task, "id")))
        .collect();
    let runs: BTreeSet<_> = rows
        .iter()
        .filter(|row| {
            [Some(b"goal-cycle".as_slice()), Some(b"autopilot-cycle")]
                .contains(&text(field(row, "kind")))
                && text(field(row, "task_id")).is_some_and(|id| ids.contains(id))
        })
        .filter_map(|row| text(field(row, "run_id")))
        .collect();
    let (index, last) = rows.iter().enumerate().rfind(|(_, row)| {
        text(field(row, "kind")) == Some(b"terminal")
            && text(field(row, "run_id")).is_some_and(|run| runs.contains(run))
    })?;
    if text(field(last, "status")) != Some(b"park")
        || rows[index + 1..].iter().any(|row| {
            text(field(row, "kind")) == Some(b"acceptance")
                && text(field(row, "status")) == Some(b"pass")
        })
    {
        return None;
    }
    let reason = clean(text(field(last, "reason")).unwrap_or_default(), 80);
    let cycle = match field(last, "cycle") {
        Json::Bool(_) => copy(field(last, "cycle")),
        Json::Number(v) if !v.contains(['.', 'e', 'E']) => copy(field(last, "cycle")),
        _ => Json::Null,
    };
    Some(object([
        (
            "run_id",
            string(clean(text(field(last, "run_id")).unwrap_or_default(), 64)),
        ),
        (
            "reason",
            string(if reason.is_empty() {
                b"unstated".to_vec()
            } else {
                reason
            }),
        ),
        ("cycle", cycle),
    ]))
}
