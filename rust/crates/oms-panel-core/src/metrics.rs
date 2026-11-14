//! Recorded evidence only. Missing samples never become zero-valued measurements.
use crate::value::text;
use crate::{Error, Result, Value, require};
use std::collections::{BTreeMap, BTreeSet};
pub fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite() && *v >= 0.0)
}
pub fn percent(value: &Value) -> Option<u8> {
    let n = number(value)?;
    if n > 100.0 {
        None
    } else {
        Some(n.round_ties_even() as u8)
    }
}
pub fn stamp(value: &Value) -> Option<f64> {
    if let Some(v) = number(value) {
        return Some(v);
    }
    let raw = value.as_str()?;
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|v| v.timestamp_millis() as f64 / 1000.0)
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|v| v.and_utc().timestamp() as f64)
        })
}
pub fn duration(raw: &str) -> Result<f64> {
    let found = regex::Regex::new(r"^([0-9]+(?:\.[0-9]+)?)(s|m|h|d|w)$").map_err(Error::new)?;
    let found = found.captures(raw).ok_or_else(|| {
        Error::new("--since must be a positive finite duration such as 30m, 2h or 7d")
    })?;
    let amount = found[1].parse::<f64>().map_err(Error::new)?;
    let scale = match &found[2] {
        "s" => 1.,
        "m" => 60.,
        "h" => 3600.,
        "d" => 86400.,
        _ => 604800.,
    };
    let seconds = amount * scale;
    require(
        seconds.is_finite() && seconds > 0.,
        "--since duration is outside the supported time range",
    )?;
    Ok(seconds)
}
pub fn five_hour(row: &Value, now: f64) -> Option<(u8, f64)> {
    let used = percent(&row["five_hour_used"])?;
    let reset = number(&row["five_hour_resets_at"])?;
    if reset <= now {
        None
    } else {
        Some((100 - used, reset - now))
    }
}
pub fn quota_bar(left: u8, width: usize) -> String {
    let count = ((left.min(100) as usize * width + 50) / 100).min(width);
    format!("{}{}", "#".repeat(count), "-".repeat(width - count))
}
pub fn percentile(values: &[f64], percent: u8) -> Option<f64> {
    if values.is_empty() || percent > 100 || values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = (sorted.len() - 1) as f64 * f64::from(percent) / 100.;
    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;
    Some(sorted[lower] + (sorted[upper] - sorted[lower]) * (index - lower as f64))
}
#[derive(Clone, Debug)]
pub struct MetricInputs<'a> {
    pub attempts: &'a [Value],
    pub artifacts: &'a [Value],
    pub tasks: &'a [Value],
    pub since: Option<&'a str>,
    pub now: f64,
    pub min_samples: usize,
    pub incomplete_reasons: Vec<String>,
}
pub fn aggregate(input: &MetricInputs<'_>) -> Result<Value> {
    require(
        input.now.is_finite() && input.now >= 0. && input.min_samples > 0,
        "invalid metrics clock/sample bound",
    )?;
    let cutoff = input
        .since
        .map(duration)
        .transpose()?
        .map(|v| input.now - v);
    require(
        cutoff.is_none_or(f64::is_finite),
        "--since duration is outside supported range",
    )?;
    let in_window = |v: &Value| cutoff.is_none_or(|c| stamp(v).is_some_and(|v| v >= c));
    let calls: Vec<_> = input
        .attempts
        .iter()
        .filter(|v| {
            !text(v, "task_id").starts_with("preflight-")
                && v["refs"]["panel_role"] == "worker"
                && v["refs"]["panel_access"] == "write"
                && in_window(&v["created_at"])
        })
        .collect();
    let mut by_task: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let mut by_provider: BTreeMap<String, usize> = BTreeMap::new();
    for call in &calls {
        by_task.entry(text(call, "task_id")).or_default().push(call);
        *by_provider.entry(text(call, "provider")).or_default() += 1;
    }
    let done = calls.iter().filter(|v| v["state"] == "done").count();
    let failed = calls
        .iter()
        .filter(|v| ["failed", "timed_out", "abandoned"].contains(&text(v, "state").as_str()))
        .count();
    let terminal = done + failed;
    let durations: Vec<_> = calls
        .iter()
        .filter_map(|v| {
            if v["terminal"] != true {
                return None;
            }
            let elapsed = stamp(&v["updated_at"])? - stamp(&v["created_at"])?;
            (elapsed >= 0.0).then_some(elapsed)
        })
        .collect();
    let rounds = by_task
        .values()
        .map(|v| v.len().saturating_sub(1))
        .sum::<usize>();
    let reviewed: BTreeSet<_> = input
        .artifacts
        .iter()
        .filter(|v| {
            ["peer-review", "review"].contains(&text(v, "kind").as_str())
                && in_window(&v["timestamp"])
        })
        .filter_map(|v| v["task_id"].as_str())
        .collect();
    let tasks_done = input
        .tasks
        .iter()
        .filter(|v| v["state"] == "done" && in_window(&v["updated_at"]))
        .count();
    let mut incomplete = input.incomplete_reasons.clone();
    if input.attempts.len() >= 1000 {
        incomplete.push("lifecycle history reached bounded acquisition limit".into());
    }
    let rate = (terminal >= input.min_samples).then(|| done as f64 / terminal as f64);
    Ok(
        serde_json::json!({"schema":1,"kind":"oms-panel-metrics","generated_at":crate::now(),"since":input.since,
        "incomplete":!incomplete.is_empty(),"incomplete_reasons":incomplete,"min_samples":input.min_samples,
        "implementation":{"calls":calls.len(),"tasks":by_task.len(),"completed":done,"failed":failed,
            "rework_rounds":rounds,"success_rate":rate,"samples":terminal,"by_provider":by_provider},
        "review":{"tasks_with_recorded_review":reviewed.len()},"plan":{"completed":tasks_done},
        "duration":{"samples":durations.len(),"p50":percentile(&durations,50),"p90":percentile(&durations,90)}}),
    )
}
pub fn metrics_text(report: &Value) -> String {
    format!(
        "Implementation: {} calls, {} completed, {} failed; {} additional rounds{}",
        report["implementation"]["calls"],
        report["implementation"]["completed"],
        report["implementation"]["failed"],
        report["implementation"]["rework_rounds"],
        if report["incomplete"] == true {
            " (partial evidence)"
        } else {
            ""
        }
    )
}
pub fn collect(report: &Value) -> Value {
    let mut rows = serde_json::Map::new();
    for provider in ["codex", "claude", "antigravity", "pi"] {
        let row = &report["provider_status"][provider];
        rows.insert(provider.into(),serde_json::json!({"weekly_used":percent(&row["weekly_used"]),
            "context_left":percent(&row["context_left"]),"source":row.get("source").cloned().unwrap_or("unavailable".into())}));
    }
    Value::Object(rows)
}
