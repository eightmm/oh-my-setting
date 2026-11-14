//! Read-only advisory projections. None grants completion, retry, publication,
//! or permission to replace a contract.
use crate::{Error, Result, require, value};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn normalize_stall_key(text: &str) -> String {
    let substitutions = [
        (
            r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?",
            "<ts>",
        ),
        (r"\b\d{1,2}:\d{2}:\d{2}(?:\.\d+)?\b", "<time>"),
        (
            r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
            "<uuid>",
        ),
        (r"\b0x[0-9a-fA-F]+\b", "<addr>"),
        (
            r#"(?:/private)?/(?:tmp|var/folders|var/tmp)/[^\s'\"]+"#,
            "<tmp>",
        ),
        (r"\btmp\.[A-Za-z0-9]{4,}\b", "<tmp>"),
        (
            r"\b(pid|PID|process|thread|port)([ =:#]+)\d+",
            "${1}${2}<n>",
        ),
        (r"\d{4,}", "<n>"),
        (r"\s+", " "),
    ];
    let mut text = text.to_owned();
    for (pattern, replacement) in substitutions {
        text = regex::Regex::new(pattern)
            .unwrap()
            .replace_all(&text, replacement)
            .into_owned();
    }
    text.trim().into()
}
#[derive(Clone)]
pub struct StallConfig {
    pub window: usize,
    pub repeat: usize,
    pub error_repeat: usize,
    pub alternate: usize,
    pub no_evidence: usize,
}
impl Default for StallConfig {
    fn default() -> Self {
        Self {
            window: 20,
            repeat: 4,
            error_repeat: 3,
            alternate: 6,
            no_evidence: 3,
        }
    }
}
pub fn detect_stalls(events: &[Value], config: &StallConfig) -> Vec<Value> {
    let mut streams = BTreeMap::<String, Vec<(String, String, bool)>>::new();
    for row in events {
        streams
            .entry(if value::text(row, "stream").is_empty() {
                "cmd".into()
            } else {
                value::text(row, "stream").into()
            })
            .or_default()
            .push((
                normalize_stall_key(value::text(row, "action")),
                normalize_stall_key(value::text(row, "result")),
                row["error"] == true,
            ));
    }
    let mut findings = vec![];
    for (stream, rows) in streams {
        let rows = if config.window == 0 {
            rows.as_slice()
        } else {
            &rows[rows.len().saturating_sub(config.window)..]
        };
        let Some(last) = rows.last() else {
            continue;
        };
        let run = rows.iter().rev().take_while(|row| *row == last).count();
        let (limit, rule, advice) = if stream == "cycle" {
            (
                config.no_evidence,
                "no-new-evidence",
                "another cycle would learn nothing new: re-plan, raise effort or tier, or ask the human before more cycles",
            )
        } else if last.2 {
            (
                config.error_repeat,
                "error-repeat",
                "the same command keeps failing: change the approach or get an outside read",
            )
        } else {
            (
                config.repeat,
                "repeat",
                "the same step keeps giving the same result: change the approach or ask the human",
            )
        };
        if limit > 0 && run >= limit {
            findings.push(json!({"rule": rule, "count": run, "stream": stream, "key": last.0, "result": last.1, "next": advice}));
            continue;
        }
        if stream != "cycle" && config.alternate > 0 && rows.len() >= 2 {
            let pair = |row: &(String, String, bool)| (row.0.clone(), row.1.clone());
            if pair(last) != pair(&rows[rows.len() - 2]) {
                let mut length = 2;
                while length < rows.len()
                    && pair(&rows[rows.len() - 1 - length]) == pair(&rows[rows.len() + 1 - length])
                {
                    length += 1;
                }
                if length >= config.alternate {
                    findings.push(json!({"rule": "alternation", "count": length, "stream": stream, "key": format!("{} <-> {}", last.0, rows[rows.len() - 2].0), "result": last.1, "next": "two steps are undoing each other: choose another approach or ask the human"}));
                }
            }
        }
    }
    findings
}
pub fn valid_repair_trace(trace: &Value) -> bool {
    if value::keys(
        trace,
        &["schema", "cohort", "budget", "policy", "samples", "steps"],
        &[],
    )
    .is_err()
        || trace["schema"].as_u64() != Some(1)
        || !value::digest(value::text(trace, "cohort"))
        || !["budget", "stop-repeat"].contains(&value::text(trace, "policy"))
    {
        return false;
    }
    let Some(budget) = trace["budget"].as_u64().filter(|n| (1..=3).contains(n)) else {
        return false;
    };
    if trace["samples"].as_u64().is_none_or(|n| n > 1000) {
        return false;
    }
    let Some(steps) = trace["steps"].as_array() else {
        return false;
    };
    !steps.is_empty()
        && steps.len() as u64 <= budget + 1
        && steps.iter().all(|step| {
            value::keys(step, &["candidate", "failure", "worker", "verify"], &[]).is_ok()
                && value::digest(value::text(step, "candidate"))
                && value::digest(value::text(step, "failure"))
                && step["worker"].as_u64().is_some_and(|n| n <= 255)
                && step["verify"].as_u64().is_some_and(|n| n <= 255)
        })
}
fn repair_passed(step: &Value) -> bool {
    step["worker"].as_u64() == Some(0) && step["verify"].as_u64() == Some(0)
}
fn repair_repeated(steps: &[Value]) -> bool {
    steps.len() >= 2
        && !repair_passed(steps.last().unwrap())
        && steps[steps.len() - 1] == steps[steps.len() - 2]
}
pub fn replay_repair(steps: &[Value], policy: &str) -> (bool, usize) {
    for (index, step) in steps.iter().enumerate() {
        if repair_passed(step) || policy == "stop-repeat" && repair_repeated(&steps[..=index]) {
            return (repair_passed(step), index + 1);
        }
    }
    (steps.last().is_some_and(repair_passed), steps.len())
}
pub fn select_repair_policy(rows: &[Value], cohort: &str, budget: u64) -> (&'static str, usize) {
    let mut episodes: Vec<(String, Vec<Value>)> = vec![];
    let mut lost_success = false;
    let mut challengers = 0;
    for row in rows {
        let trace = &row["repair_replay"];
        if row["kind"] != "delegate"
            || !valid_repair_trace(trace)
            || row["fallback_used"] == true
            || row["model_attribution"] == "ambiguous"
            || value::text(trace, "cohort") != cohort
            || trace["budget"].as_u64() != Some(budget)
        {
            continue;
        }
        let steps = trace["steps"].as_array().unwrap();
        if steps.iter().any(|s| {
            s["worker"]
                .as_u64()
                .is_some_and(|n| [125, 126, 127].contains(&n))
                || s["verify"].as_u64() == Some(125)
        }) || steps[..steps.len() - 1].iter().any(repair_passed)
            || row["exit"].as_u64()
                != Some(if repair_passed(steps.last().unwrap()) {
                    0
                } else {
                    1
                })
            || row["verify_exit"] != steps.last().unwrap()["verify"]
            || !value::digest(value::text(row, "prompt_hash"))
        {
            continue;
        }
        if trace["policy"] == "stop-repeat" {
            if repair_passed(steps.last().unwrap())
                || repair_repeated(steps)
                || steps.len() as u64 == budget + 1
            {
                challengers += 1;
            }
            continue;
        }
        if repair_passed(&steps[0])
            || !repair_passed(steps.last().unwrap()) && steps.len() as u64 != budget + 1
        {
            continue;
        }
        challengers = 0;
        lost_success |=
            repair_passed(steps.last().unwrap()) && !replay_repair(steps, "stop-repeat").0;
        let id = value::text(row, "prompt_hash");
        episodes.retain(|(name, _)| name != id);
        episodes.push((id.into(), steps.clone()));
    }
    if episodes.len() < 12 || lost_success || challengers >= 4 {
        return ("budget", episodes.len());
    }
    let split = episodes.len() * 2 / 3;
    for cohort in [&episodes[..split], &episodes[split..]] {
        let saved = cohort
            .iter()
            .map(|(_, steps)| {
                replay_repair(steps, "budget")
                    .1
                    .saturating_sub(replay_repair(steps, "stop-repeat").1)
            })
            .sum::<usize>();
        if saved == 0 {
            return ("budget", episodes.len());
        }
    }
    ("stop-repeat", episodes.len())
}
pub fn drive_overlay(plan: &Value, rows: &[Value]) -> Option<Value> {
    let ids = plan["tasks"]
        .as_object()?
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let runs = rows
        .iter()
        .filter(|r| {
            ["goal-cycle", "autopilot-cycle"].contains(&value::text(r, "kind"))
                && ids.contains(value::text(r, "task_id"))
        })
        .map(|r| value::text(r, "run_id"))
        .collect::<BTreeSet<_>>();
    let (index, last) = rows.iter().enumerate().rev().find(|(_, r)| {
        value::text(r, "kind") == "terminal" && runs.contains(value::text(r, "run_id"))
    })?;
    if last["status"] != "park"
        || rows[index + 1..]
            .iter()
            .any(|r| r["kind"] == "acceptance" && r["status"] == "pass")
    {
        return None;
    }
    Some(
        json!({"run_id": value::text(last, "run_id"), "reason": value::text(last, "reason"), "cycle": last.get("cycle").cloned().unwrap_or(Value::Null)}),
    )
}
pub fn handoff_decisions(packet: &[u8], max_entries: usize, max_bytes: usize) -> Vec<String> {
    if oms_command_task::packet::metadata(packet, "status") == Some(b"closed".as_slice())
        || packet.len() > 65536
    {
        return vec![];
    }
    let body = oms_command_task::packet::section(packet, "Decisions");
    let pattern = regex::Regex::new(r"^- (\d{4}-\d{2}-\d{2})T\S*Z \[[^\]]*\] (.+)$").unwrap();
    let mut rows = vec![];
    for line in String::from_utf8_lossy(&body).lines() {
        if let Some(found) = pattern.captures(line.trim()) {
            rows.push(format!("- {} {}", &found[1], found[2].trim()));
        }
    }
    let mut used = 0;
    let mut selected = vec![];
    for row in rows.into_iter().rev() {
        if selected.len() >= max_entries || used + row.len() + 1 > max_bytes {
            break;
        }
        used += row.len() + 1;
        selected.push(row);
    }
    selected.reverse();
    selected
}
pub fn handoff_pickups(rows: &[Value]) -> Value {
    let mut seen = BTreeMap::<String, BTreeSet<String>>::new();
    let mut found = json!({});
    for row in rows {
        let (Some(name), Some(session), Some(ts)) = (
            row["handoff"].as_str(),
            row["session"].as_str(),
            row["ts"].as_str(),
        ) else {
            continue;
        };
        let sessions = seen.entry(name.into()).or_default();
        if !sessions.insert(session.into()) {
            continue;
        }
        if found.get(name).is_none() {
            found[name] = json!({"session": oms_contract_markdown::bounded_line(session, 120), "ts": oms_contract_markdown::bounded_line(ts, 40), "count": 0});
        }
        found[name]["count"] = sessions.len().into();
    }
    found
}
pub fn team_baseline(
    want: &Value,
    fingerprint: &Value,
    capabilities: Option<&[String]>,
) -> Result<Value> {
    value::keys(
        want,
        &["schema"],
        &["min_oms_version", "capabilities", "digests"],
    )?;
    require(
        want["schema"].as_u64() == Some(1),
        "team baseline schema must be 1",
    )?;
    let version = |raw: &str| -> Result<Vec<u64>> {
        require(
            regex::Regex::new(r"^[0-9]+(?:\.[0-9]+){0,3}$")
                .unwrap()
                .is_match(raw.trim()),
            "version must look like 1.2.3",
        )?;
        raw.trim()
            .split('.')
            .map(|s| {
                s.parse()
                    .map_err(|_| Error::new(2, "version component exceeds integer bound"))
            })
            .collect()
    };
    let mut unmet = vec![];
    if let Some(minimum) = want.get("min_oms_version") {
        let min = version(
            minimum
                .as_str()
                .ok_or_else(|| Error::new(2, "min_oms_version must be text"))?,
        )?;
        if version(value::text(fingerprint, "oms_version")).map_or(true, |have| have < min) {
            unmet.push("min_oms_version".to_owned());
        }
    }
    if let Some(caps) = want.get("capabilities") {
        for cap in crate::planning::strings(caps)? {
            require(!cap.is_empty(), "capability name cannot be empty")?;
            if capabilities.is_none_or(|have| !have.iter().any(|s| s == &cap || s == "full")) {
                unmet.push(format!("capability:{cap}"));
            }
        }
    }
    if let Some(digests) = want.get("digests") {
        for (key, expected) in digests
            .as_object()
            .ok_or_else(|| Error::new(2, "digests must map names to strings"))?
        {
            require(expected.is_string(), "digest must be text")?;
            if fingerprint["digests"].get(key) != Some(expected) {
                unmet.push(format!("digest:{key}"));
            }
        }
    }
    Ok(json!({"schema": 1, "kind": "team-baseline", "ok": unmet.is_empty(), "unmet": unmet}))
}
pub fn slurm_seconds(raw: &str) -> Option<u64> {
    let matched =
        regex::Regex::new(r"^(?:([0-9]+)-)?(?:([0-9]+):)?([0-9]+):([0-9]+)(?:\.[0-9]+)?$").unwrap();
    let matched = matched.captures(raw.trim())?;
    let n = |i| {
        matched
            .get(i)
            .map_or(Some(0u64), |m| m.as_str().parse::<u64>().ok())
    };
    n(1)?
        .checked_mul(24)?
        .checked_add(n(2)?)?
        .checked_mul(60)?
        .checked_add(n(3)?)?
        .checked_mul(60)?
        .checked_add(n(4)?)
}
pub fn slurm_consumed(records: &[Value]) -> Value {
    let mut out = json!({});
    if records.is_empty() {
        return out;
    }
    let elapsed = records
        .iter()
        .map(|r| slurm_seconds(value::text(r, "elapsed")))
        .collect::<Vec<_>>();
    let total = elapsed
        .iter()
        .copied()
        .collect::<Option<Vec<_>>>()
        .and_then(|rows| rows.into_iter().try_fold(0u64, u64::checked_add));
    if let Some(total) = total {
        out["elapsed_seconds"] = total.into();
    }
    let round = |v: f64| (v * 10000.0).round() / 10000.0;
    let mut wait = 0i64;
    let mut wait_count = 0;
    let mut gpu = Some(0u64);
    let mut limits = Some((0u64, 0u64));
    let mut cpu = Some((0u64, 0u64));
    for (row, elapsed) in records.iter().zip(elapsed) {
        let stamp = |key| {
            chrono::NaiveDateTime::parse_from_str(
                &value::text(row, key).chars().take(19).collect::<String>(),
                "%Y-%m-%dT%H:%M:%S",
            )
            .ok()
            .map(|t| t.and_utc().timestamp())
        };
        if let (Some(start), Some(submit)) = (stamp("start"), stamp("submit")) {
            if start >= submit {
                if let Some(next) = wait.checked_add(start - submit) {
                    wait = next;
                    wait_count += 1;
                }
            }
        }
        let tres = value::text(row, "alloc_tres");
        let mut exact = None;
        let mut typed = 0u64;
        for field in tres.split(',') {
            if let Some((key, raw)) = field.split_once('=') {
                if let Ok(n) = raw.trim().parse::<u64>() {
                    if key.trim() == "gres/gpu" {
                        exact = Some(n);
                    } else if key.trim().starts_with("gres/gpu:") {
                        typed = typed.saturating_add(n);
                    }
                }
            }
        }
        let gpus = if tres.trim().is_empty() {
            None
        } else {
            Some(exact.unwrap_or(typed))
        };
        gpu = gpu.zip(gpus).zip(elapsed).and_then(|((sum, n), seconds)| {
            n.checked_mul(seconds)
                .and_then(|spent| sum.checked_add(spent))
        });
        limits = limits
            .zip(slurm_seconds(value::text(row, "timelimit")))
            .zip(elapsed)
            .filter(|((_, limit), _)| *limit > 0)
            .and_then(|(((limit_sum, elapsed_sum), limit), elapsed)| {
                Some((
                    limit_sum.checked_add(limit)?,
                    elapsed_sum.checked_add(elapsed)?,
                ))
            });
        let cpus = row["alloc_cpus"]
            .as_u64()
            .or_else(|| value::text(row, "alloc_cpus").parse::<u64>().ok());
        cpu = cpu
            .zip(slurm_seconds(value::text(row, "total_cpu")))
            .zip(cpus)
            .zip(elapsed)
            .filter(|(((_, _), count), spent)| *count > 0 && *spent > 0)
            .and_then(|((((used, given), current), cpus), elapsed)| {
                Some((
                    used.checked_add(current)?,
                    given.checked_add(cpus.checked_mul(elapsed)?)?,
                ))
            });
    }
    if wait_count > 0 {
        out["queue_wait_seconds"] = wait.into();
        if wait_count != records.len() {
            out["queue_wait_records"] = wait_count.into();
        }
    }
    if let Some(gpu) = gpu {
        out["gpu_hours"] = json!(round(gpu as f64 / 3600.0));
    }
    if let Some((limit, elapsed)) = limits {
        if limit > 0 {
            out["time_efficiency"] = json!(round(elapsed as f64 / limit as f64));
        }
    }
    if let Some((used, given)) = cpu {
        if given > 0 {
            out["cpu_efficiency"] = json!(round(used as f64 / given as f64));
        }
    }
    out
}

pub fn slurm_summary(records: &[Value], jobs: Option<&BTreeSet<String>>) -> Value {
    let mut seen = BTreeSet::new();
    let mut states = BTreeMap::<String, u64>::new();
    let mut usage = vec![];
    for row in records {
        let job = row
            .get("job_id")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .unwrap_or_default();
        if job.is_empty()
            || jobs.is_some_and(|set| !set.contains(&job))
            || !seen.insert((job, value::text(row, "submit").to_owned()))
        {
            continue;
        }
        *states
            .entry(value::text(row, "state").to_owned())
            .or_default() += 1;
        usage.push(&row["usage"]);
    }
    let aggregate = |key: &str, mean: bool| -> (Value, usize) {
        let numbers = usage
            .iter()
            .filter_map(|row| row[key].as_f64())
            .filter(|n| n.is_finite())
            .collect::<Vec<_>>();
        if numbers.is_empty() {
            return (Value::Null, 0);
        }
        let sum = numbers.iter().sum::<f64>();
        let result = if mean {
            sum / numbers.len() as f64
        } else {
            sum
        };
        (
            json!((result * 10000.0).round_ties_even() / 10000.0),
            numbers.len(),
        )
    };
    let (gpu, gpu_n) = aggregate("gpu_hours", false);
    let (wait, wait_n) = aggregate("queue_wait_seconds", false);
    let (elapsed, elapsed_n) = aggregate("elapsed_seconds", false);
    let (time, time_n) = aggregate("time_efficiency", true);
    let (cpu, cpu_n) = aggregate("cpu_efficiency", true);
    json!({"schema": 1, "kind": "oms-run-usage", "jobs": usage.len(), "states": states, "gpu_hours": gpu, "gpu_hours_jobs": gpu_n, "queue_wait_seconds": wait, "queue_wait_jobs": wait_n, "elapsed_seconds": elapsed, "elapsed_jobs": elapsed_n, "time_efficiency_mean": time, "time_efficiency_jobs": time_n, "cpu_efficiency_mean": cpu, "cpu_efficiency_jobs": cpu_n, "gpu_utilization": "unknown", "memory_efficiency": "unknown"})
}
