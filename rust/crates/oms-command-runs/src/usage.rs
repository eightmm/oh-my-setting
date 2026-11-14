//! Arithmetic over recorded Slurm allocation rows. No scheduler transport.
use crate::{Result, store, value::*};
use chrono::NaiveDateTime;
use oms_context::Json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn seconds(text: &str) -> Option<i128> {
    let text = text.trim();
    let (days, clock) = text
        .split_once('-')
        .map_or(Some((0, text)), |(d, c)| Some((d.parse::<i128>().ok()?, c)))?;
    if days < 0 {
        return None;
    }
    let parts = clock.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut nums = vec![];
    for (i, p) in parts.iter().enumerate() {
        let p = if i == parts.len() - 1 {
            let (s, f) = p.split_once('.').map_or((*p, None), |(s, f)| (s, Some(f)));
            if f.is_some_and(|f| f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit())) {
                return None;
            }
            s
        } else {
            p
        };
        if p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        nums.push(p.parse::<i128>().ok()?);
    }
    let (h, m, s) = if nums.len() == 3 {
        (nums[0], nums[1], nums[2])
    } else {
        (0, nums[0], nums[1])
    };
    days.checked_mul(24)?
        .checked_add(h)?
        .checked_mul(60)?
        .checked_add(m)?
        .checked_mul(60)?
        .checked_add(s)
}
pub fn stamp(text: &str) -> Option<i64> {
    NaiveDateTime::parse_from_str(text.trim().get(..19)?, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|d| d.and_utc().timestamp())
}
pub fn tres_count(text: &str, key: &str) -> Option<i128> {
    if text.trim().is_empty() {
        return None;
    }
    let mut exact = None;
    let mut typed = 0i128;
    for part in text.split(',') {
        let Some((name, v)) = part.split_once('=') else {
            continue;
        };
        let Ok(n) = v.trim().parse::<i128>() else {
            continue;
        };
        let name = name.trim();
        if name == key {
            exact = Some(n);
        } else if name.starts_with(&format!("{key}:")) {
            typed = typed.checked_add(n)?;
        }
    }
    exact.or(Some(typed))
}
fn rounded(v: f64) -> Json {
    if v.is_finite() {
        num(format!("{:.4}", v))
    } else {
        Json::Null
    }
}
pub fn consumed(records: &[Json]) -> Json {
    let mut out = Json::Object(vec![]);
    let elapsed = records
        .iter()
        .map(|r| seconds(&s(r, "elapsed")))
        .collect::<Vec<_>>();
    if !elapsed.is_empty()
        && elapsed.iter().all(Option::is_some)
        && let Some(total) = elapsed
            .iter()
            .try_fold(0i128, |acc, x| acc.checked_add(x.unwrap()))
    {
        put(&mut out, "elapsed_seconds", num(total));
    }
    let waits = records
        .iter()
        .filter_map(|r| {
            let b = stamp(&s(r, "start"))?;
            let a = stamp(&s(r, "submit"))?;
            b.checked_sub(a).filter(|n| *n >= 0)
        })
        .collect::<Vec<_>>();
    if !waits.is_empty() {
        if let Some(total) = waits
            .iter()
            .try_fold(0i128, |a, v| a.checked_add(*v as i128))
        {
            put(&mut out, "queue_wait_seconds", num(total));
        }
        if waits.len() != records.len() {
            put(&mut out, "queue_wait_records", num(waits.len()));
        }
    }
    let gpu = records
        .iter()
        .zip(&elapsed)
        .try_fold(0i128, |total, (r, e)| {
            total.checked_add(tres_count(&s(r, "alloc_tres"), "gres/gpu")?.checked_mul((*e)?)?)
        });
    if !records.is_empty()
        && let Some(gpu) = gpu
    {
        put(&mut out, "gpu_hours", rounded(gpu as f64 / 3600.0));
    }
    let mut limits = 0.0;
    let mut spent = 0.0;
    let mut used = 0.0;
    let mut given = 0.0;
    let mut limits_ok = !records.is_empty();
    let mut cpus_ok = limits_ok;
    for (r, e) in records.iter().zip(elapsed) {
        let limit = seconds(&s(r, "timelimit"));
        if let (Some(l), Some(e)) = (limit, e)
            && l > 0
        {
            limits += l as f64;
            spent += e as f64;
        } else {
            limits_ok = false;
        }
        let cpu = seconds(&s(r, "total_cpu"));
        let cpus = s(r, "alloc_cpus").parse::<i128>().ok();
        if let (Some(cpu), Some(cpus), Some(e)) = (cpu, cpus, e)
            && cpus > 0
            && e > 0
        {
            used += cpu as f64;
            given += cpus as f64 * e as f64;
        } else {
            cpus_ok = false;
        }
    }
    if limits_ok && limits > 0.0 {
        put(&mut out, "time_efficiency", rounded(spent / limits));
    }
    if cpus_ok && given > 0.0 {
        put(&mut out, "cpu_efficiency", rounded(used / given));
    }
    out
}
pub fn experiment_jobs(ledger: &Path, experiment: &str) -> Result<BTreeSet<String>> {
    experiment_jobs_in(ledger, experiment, &std::env::current_dir()?)
}
pub(crate) fn experiment_jobs_in(
    ledger: &Path,
    experiment: &str,
    root: &Path,
) -> Result<BTreeSet<String>> {
    let mut jobs = BTreeSet::new();
    let wanted = format!("experiment={experiment}");
    store::rows_in(ledger, true, root, |row| {
        if !get(&row, "slurm_job_id").is_some_and(truth) {
            return Ok(());
        }
        if s(&row, "note").split_whitespace().any(|w| w == wanted) {
            let job = s(&row, "slurm_job_id");
            if !job.trim().is_empty() && job != "None" {
                jobs.insert(job.trim().into());
            }
        }
        Ok(())
    })?;
    Ok(jobs)
}
pub fn summary(reconcile: &Path, jobs: Option<&BTreeSet<String>>) -> Result<Json> {
    summary_in(reconcile, jobs, &std::env::current_dir()?)
}
pub(crate) fn summary_in(
    reconcile: &Path,
    jobs: Option<&BTreeSet<String>>,
    root: &Path,
) -> Result<Json> {
    let mut seen = BTreeSet::new();
    let mut values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut states: BTreeMap<String, usize> = BTreeMap::new();
    let mut count = 0;
    store::rows_in(reconcile, true, root, |row| {
        if !get(&row, "job_id").is_some_and(truth) {
            return Ok(());
        }
        let id = s(&row, "job_id");
        if id.is_empty() || id == "None" || jobs.is_some_and(|j| !j.contains(&id)) {
            return Ok(());
        }
        if !seen.insert((id, s(&row, "submit"))) {
            return Ok(());
        }
        count += 1;
        *states.entry(s(&row, "state")).or_default() += 1;
        if let Some(usage) = get(&row, "usage") {
            for key in [
                "gpu_hours",
                "queue_wait_seconds",
                "elapsed_seconds",
                "time_efficiency",
                "cpu_efficiency",
            ] {
                if let Some(n) = get(usage, key).and_then(number) {
                    values.entry(key.into()).or_default().push(n);
                }
            }
        }
        Ok(())
    })?;
    let mut out = object(vec![
        ("schema", num(1)),
        ("kind", string("oms-run-usage")),
        ("jobs", num(count)),
        (
            "states",
            Json::Object(
                states
                    .into_iter()
                    .map(|(k, n)| (k.into_bytes(), num(n)))
                    .collect(),
            ),
        ),
        ("gpu_utilization", string("unknown")),
        ("memory_efficiency", string("unknown")),
    ]);
    for (key, target, nkey, mean) in [
        ("gpu_hours", "gpu_hours", "gpu_hours_jobs", false),
        (
            "queue_wait_seconds",
            "queue_wait_seconds",
            "queue_wait_jobs",
            false,
        ),
        ("elapsed_seconds", "elapsed_seconds", "elapsed_jobs", false),
        (
            "time_efficiency",
            "time_efficiency_mean",
            "time_efficiency_jobs",
            true,
        ),
        (
            "cpu_efficiency",
            "cpu_efficiency_mean",
            "cpu_efficiency_jobs",
            true,
        ),
    ] {
        let n = values.get(key).map_or(0, Vec::len);
        let total = values.get(key).map(|v| {
            v.iter()
                .map(|n| *n / if mean { v.len() as f64 } else { 1.0 })
                .sum()
        });
        put(&mut out, target, total.map(rounded).unwrap_or(Json::Null));
        put(&mut out, nkey, num(n));
    }
    Ok(out)
}
fn clock(v: f64) -> String {
    let n = v as i128;
    format!("{:02}:{:02}:{:02}", n / 3600, (n % 3600) / 60, n % 60)
}
pub fn render(report: &Json, scope: &str) -> String {
    let jobs = get(report, "jobs").and_then(number).unwrap_or(0.0) as usize;
    let mut details = vec![];
    if let Some(Json::Object(states)) = get(report, "states") {
        for (k, v) in states {
            details.push(format!("{}={}", String::from_utf8_lossy(k), display(v)));
        }
    }
    let mut lines = vec![format!(
        "run usage{scope}: {jobs} recorded job(s){}",
        if jobs > 0 {
            format!(" ({})", details.join(", "))
        } else {
            String::new()
        }
    )];
    let missing = |key: &str| {
        let n = get(report, key).and_then(number).unwrap_or(0.0) as usize;
        if n < jobs {
            format!("; {} job(s) unknown", jobs - n)
        } else {
            String::new()
        }
    };
    if let Some(n) = get(report, "gpu_hours").and_then(number) {
        lines.push(format!(
            "gpu-hours: {} allocated over {} job(s){}",
            display(&rounded(n)),
            s(report, "gpu_hours_jobs"),
            missing("gpu_hours_jobs")
        ));
    } else {
        lines.push(
            "gpu-hours: unknown (no recorded job carries its allocation and elapsed time)".into(),
        );
    }
    if let Some(n) = get(report, "queue_wait_seconds").and_then(number) {
        lines.push(format!(
            "queue wait: {} total over {} job(s){}",
            clock(n),
            s(report, "queue_wait_jobs"),
            missing("queue_wait_jobs")
        ));
    } else {
        lines.push("queue wait: unknown".into());
    }
    if let Some(n) = get(report, "elapsed_seconds").and_then(number) {
        lines.push(format!(
            "elapsed: {} total over {} job(s){}",
            clock(n),
            s(report, "elapsed_jobs"),
            missing("elapsed_jobs")
        ));
    }
    for (label, key) in [
        (
            "time efficiency (elapsed / requested limit)",
            "time_efficiency",
        ),
        (
            "cpu efficiency (TotalCPU / allocated cpu time)",
            "cpu_efficiency",
        ),
    ] {
        let nkey = format!("{key}_jobs");
        let val = get(report, &format!("{key}_mean"))
            .and_then(number)
            .map(|n| {
                format!(
                    "{:.0}% mean over {} job(s){}",
                    n * 100.0,
                    s(report, &nkey),
                    missing(&nkey)
                )
            })
            .unwrap_or_else(|| "unknown".into());
        lines.push(format!("{label}: {val}"));
    }
    lines.push(
        "gpu utilisation: unknown (sacct reports the allocation, not how busy the GPU was)".into(),
    );
    lines.push(
        "memory efficiency: unknown (peak memory is a step field this query does not read)".into(),
    );
    lines.push(
        "allocated figures from recorded accounting rows only; nothing was queried or re-run"
            .into(),
    );
    lines.join("\n")
}
