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

pub const READING_FRESH_SECONDS: f64 = 600.0;
const CODEX_WEEK_SECONDS: f64 = 10080.0 * 60.0;
const CODEX_TAIL_BYTES: usize = 256 * 1024;
const MAX_SCAN_ENTRIES: usize = 4096;

/// Collect current local provider usage telemetry (Codex rollout logs, Claude
/// HUD cache, Antigravity OMS artifacts, and configured Codex accounts) for the
/// panel status and usage views without mutating repository state.
pub fn collect_local(
    repo: Option<&std::path::Path>,
    report: &Value,
    main_attempt: Option<&str>,
    now: f64,
) -> Value {
    let participants = crate::value::array(&report["room"]["participants"]);
    let mut output = serde_json::Map::new();

    for provider in ["codex", "claude"] {
        let mut mains: Vec<&Value> = participants
            .iter()
            .filter(|v| v["role"] == "main" && v["provider"] == provider)
            .collect();
        mains.sort_by_key(|row| {
            (
                main_attempt.is_some_and(|id| row["attempt_id"] == id),
                !crate::state::TERMINAL_STATES.contains(&text(row, "state").as_str()),
                text(row, "state") == "working",
                text(row, "attempt_id"),
            )
        });
        let selected = mains.last().copied();
        let mut row = serde_json::Map::new();
        row.insert(
            "source".into(),
            selected
                .filter(|m| m["consumer"].as_str().is_some_and(|c| !c.is_empty()))
                .map(|_| "unavailable".into())
                .unwrap_or_else(|| "no active consumer".into()),
        );

        let mut codex_log: Option<(Option<f64>, Option<f64>, Option<u8>, Vec<Value>)> = None;
        if provider == "codex" {
            let (weekly_used, resets_at, observed_at, shorts) = codex_readings(None, now, true);
            if let Some(u) = weekly_used {
                row.insert("weekly_used".into(), u.into());
                row.insert("source".into(), "session log".into());
            }
            if !shorts.is_empty() {
                if row.get("weekly_used").is_none() {
                    row.insert("source".into(), "session log".into());
                }
                row.insert("short_windows".into(), Value::Array(shorts.clone()));
                if let Some(first) = shorts.first() {
                    if let Some(obs) = first.get("observed_at").cloned() {
                        row.entry("observed_at").or_insert(obs);
                    }
                    if first["minutes"].as_u64() == Some(300) {
                        if let Some(u) = first["used"].as_u64() {
                            row.insert("five_hour_used".into(), u.into());
                        }
                        if let Some(r) = first.get("resets_at").cloned() {
                            row.insert("five_hour_resets_at".into(), r);
                        }
                    }
                }
            }
            if let Some(r) = resets_at {
                row.insert("weekly_resets_at".into(), r.into());
            }
            if let Some(o) = observed_at {
                row.insert("observed_at".into(), o.into());
            }
            codex_log = Some((resets_at, observed_at, weekly_used, shorts));

            match crate::routing::Accounts::configured() {
                Ok(Some(config)) => {
                    let mut accs = serde_json::Map::new();
                    let active_home = std::env::var_os("CODEX_HOME")
                        .map(std::path::PathBuf::from)
                        .or_else(|| {
                            std::env::var_os("HOME")
                                .map(|h| std::path::PathBuf::from(h).join(".codex"))
                        })
                        .and_then(|p| std::fs::canonicalize(p).ok());
                    let mut errs = Vec::new();
                    for entry in &config.accounts {
                        let resolved = std::fs::canonicalize(&entry.home);
                        if active_home
                            .as_ref()
                            .is_some_and(|h| resolved.as_ref().is_ok_and(|r| r == h))
                        {
                            row.insert("account_label".into(), entry.id.clone().into());
                        }
                        match resolved {
                            Ok(home_path) if home_path.is_dir() => {
                                let (w_used, r_at, o_at, _) =
                                    codex_readings(Some(&home_path), now, false);
                                let mut info = serde_json::Map::new();
                                info.insert(
                                    "weekly_used".into(),
                                    w_used.map(Value::from).unwrap_or(Value::Null),
                                );
                                if let Some(u) = w_used {
                                    info.insert("used".into(), u.into());
                                    info.insert("source".into(), "session log".into());
                                }
                                if let Some(r) = r_at {
                                    info.insert("resets_at".into(), r.into());
                                }
                                if let Some(o) = o_at {
                                    info.insert("observed_at".into(), o.into());
                                }
                                accs.insert(entry.id.clone(), Value::Object(info));
                            }
                            _ => {
                                let msg = "account home unavailable".to_string();
                                errs.push(format!("{}: {msg}", entry.id));
                                accs.insert(
                                    entry.id.clone(),
                                    serde_json::json!({"weekly_used": null, "error": msg}),
                                );
                            }
                        }
                    }
                    row.insert("accounts".into(), Value::Object(accs));
                    if !errs.is_empty() {
                        row.insert("accounts_error".into(), errs.join("; ").into());
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    row.insert("accounts_error".into(), e.message.into());
                }
            }
        }

        if let Some(main) = selected {
            row.insert("participant".into(), main["participant"].clone());
            if let Some(model) = main.get("model") {
                row.insert("model".into(), model.clone());
            }
            if provider == "claude"
                && let Some(consumer) = main["consumer"].as_str()
            {
                let hud = hud_cache_reading(consumer, now);
                if let Value::Object(map) = hud {
                    for (k, v) in map {
                        row.insert(k, v);
                    }
                }
            }
            if crate::state::TERMINAL_STATES.contains(&text(main, "state").as_str()) {
                row.insert("context_left".into(), Value::Null);
            }
        }
        let _ = codex_log;
        output.insert(provider.into(), Value::Object(row));
    }

    let mut agy = serde_json::Map::new();
    agy.insert("context_left".into(), Value::Null);
    agy.insert("weekly_used".into(), Value::Null);
    agy.insert("source".into(), "unavailable".into());
    if let Some(repo_path) = repo
        && let Ok(rows) = crate::results::index_rows(repo_path)
    {
        let cutoff = now - CODEX_WEEK_SECONDS;
        let mut calls = 0u64;
        let mut input = 0u64;
        let mut cache_read = 0u64;
        let mut out = 0u64;
        for entry in rows {
            let prov = entry["provider"].as_str().unwrap_or_default();
            if prov != "antigravity" && prov != "agy" {
                continue;
            }
            let Some(ts) = stamp(&entry["ts"]) else {
                continue;
            };
            if ts < cutoff {
                continue;
            }
            calls += 1;
            let u = &entry["provider_usage"];
            if u.is_object() {
                input += u["input_tokens"].as_u64().unwrap_or(0);
                cache_read += u["cache_read_tokens"]
                    .as_u64()
                    .or_else(|| u["cached_tokens"].as_u64())
                    .unwrap_or(0);
                out += u["output_tokens"].as_u64().unwrap_or(0);
            }
        }
        if calls > 0 {
            agy.insert("calls".into(), calls.into());
            agy.insert("input_tokens".into(), input.into());
            agy.insert("cache_read_tokens".into(), cache_read.into());
            agy.insert("output_tokens".into(), out.into());
            agy.insert("source".into(), "OMS artifacts".into());
        }
    }
    output.insert("antigravity".into(), Value::Object(agy));
    Value::Object(output)
}

fn hud_cache_reading(consumer: &str, now: f64) -> Value {
    if !crate::hex(consumer, 32) {
        return Value::Object(serde_json::Map::new());
    }
    let folder = std::env::var_os("OMS_HUD_CACHE_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("oh-my-setting-hud"));
    let Ok(dir_meta) = std::fs::symlink_metadata(&folder) else {
        return Value::Object(serde_json::Map::new());
    };
    if !dir_meta.is_dir() || dir_meta.file_type().is_symlink() {
        return Value::Object(serde_json::Map::new());
    }
    let path = folder.join(format!("ctx-{}.json", &consumer[..24]));
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    let Ok(fd) = rustix::fs::open(&path, flags, rustix::fs::Mode::empty()) else {
        return Value::Object(serde_json::Map::new());
    };
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let mut file = std::fs::File::from(fd);
    let Ok(meta) = file.metadata() else {
        return Value::Object(serde_json::Map::new());
    };
    if !meta.is_file() || meta.nlink() != 1 || meta.size() > 8192 {
        return Value::Object(serde_json::Map::new());
    }
    let mut buf = Vec::with_capacity(meta.size() as usize);
    if file.read_to_end(&mut buf).is_err() || buf.len() > 8192 {
        return Value::Object(serde_json::Map::new());
    }
    let Ok(data) = crate::decode(&buf, 8192) else {
        return Value::Object(serde_json::Map::new());
    };
    let Some(ts) = number(&data["ts"]) else {
        return Value::Object(serde_json::Map::new());
    };
    if ts > now {
        return Value::Object(serde_json::Map::new());
    }
    if now - ts > READING_FRESH_SECONDS {
        return serde_json::json!({"source": "cache stale", "observed_at": ts});
    }
    let mut out = serde_json::Map::new();
    out.insert("source".into(), "HUD cache".into());
    out.insert("observed_at".into(), ts.into());
    if let Some(used) = percent(&data["used_percentage"]) {
        out.insert("context_left".into(), (100 - used).into());
    }
    if let Some(fh) = data.get("five_hour") {
        if let Some(used) = percent(&fh["used_percentage"]) {
            out.insert("five_hour_used".into(), used.into());
        }
        if let Some(r) = stamp(&fh["resets_at"]) {
            out.insert("five_hour_resets_at".into(), r.into());
        }
    }
    if let Some(sd) = data.get("seven_day") {
        if let Some(used) = percent(&sd["used_percentage"]) {
            out.insert("weekly_used".into(), used.into());
        }
        if let Some(r) = stamp(&sd["resets_at"]) {
            out.insert("weekly_resets_at".into(), r.into());
        }
    }
    if let Some(usage) = data.get("current_usage").and_then(|v| v.as_object()) {
        let input_t = usage
            .get("input_tokens")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let cache_create = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let total = input_t + cache_create + cache_read;
        if total > 0.0 {
            let pct = ((cache_read * 100.0) / total).round().clamp(0.0, 100.0) as u8;
            out.insert("cache_read_share".into(), pct.into());
        }
    }
    Value::Object(out)
}

fn codex_readings(
    explicit_home: Option<&std::path::Path>,
    now: f64,
    fallback_older: bool,
) -> (Option<u8>, Option<f64>, Option<f64>, Vec<Value>) {
    let default_home = explicit_home.is_none();
    let home = explicit_home
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("CODEX_HOME").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex")));
    let Some(home) = home.and_then(|h| std::fs::canonicalize(h).ok()) else {
        return (None, None, None, vec![]);
    };
    let sessions_root = home.join("sessions");
    let mut remaining = MAX_SCAN_ENTRIES;
    let mut files: Vec<(i128, u64, u64, std::path::PathBuf)> = Vec::new();
    let years = scan_filtered(&sessions_root, 4, true, &mut remaining, |name| {
        name.len() == 4 && name.bytes().all(|b| b.is_ascii_digit())
    });
    'search: for year in years {
        let months = scan_filtered(&year, 2, true, &mut remaining, |name| {
            matches!(
                name,
                "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12"
            )
        });
        for month in months {
            let days = scan_filtered(&month, 2, true, &mut remaining, |name| {
                name.len() == 2 && matches!(name.parse::<u8>(), Ok(1..=31))
            });
            for day in days {
                let mut found = scan_filtered(&day, usize::MAX, false, &mut remaining, |name| {
                    name.starts_with("rollout-") && name.ends_with(".jsonl")
                });
                for path in found.drain(..) {
                    if let Ok(meta) = std::fs::symlink_metadata(&path)
                        && meta.is_file()
                    {
                        use std::os::unix::fs::MetadataExt;
                        let mtime =
                            meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128;
                        files.push((mtime, meta.ino(), meta.size(), path));
                    }
                }
                if !fallback_older && files.len() >= 16 {
                    break 'search;
                }
            }
        }
    }
    if files.is_empty() {
        return (None, None, None, vec![]);
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let mut best_weekly: Option<(u8, f64, Option<f64>)> = None;
    let mut newest_shorts: Vec<Value> = Vec::new();
    let limit = if fallback_older { 64 } else { 1 };
    for (idx, (mtime, ino, size, path)) in files.into_iter().take(limit).enumerate() {
        if default_home && idx == 0 && now - (mtime as f64 / 1e9) > CODEX_WEEK_SECONDS {
            break;
        }
        let Some((limits, observed)) = read_rate_limits(&path, ino, size, mtime) else {
            continue;
        };
        if idx == 0 {
            let mut by_min = BTreeMap::new();
            for key in ["primary", "secondary"] {
                let w = &limits[key];
                let Some(minutes) = number(&w["window_minutes"]) else {
                    continue;
                };
                if minutes <= 0.0 || minutes == 10080.0 || minutes.fract() != 0.0 {
                    continue;
                }
                if let (Some(u), Some(r)) = (percent(&w["used_percent"]), number(&w["resets_at"]))
                    && r > now
                {
                    let m = minutes as u64;
                    by_min.entry(m).or_insert_with(|| {
                        serde_json::json!({
                            "minutes": m,
                            "used": u,
                            "resets_at": r,
                            "observed_at": observed,
                        })
                    });
                }
            }
            newest_shorts = by_min.into_values().collect();
        }
        for key in ["primary", "secondary"] {
            let w = &limits[key];
            if number(&w["window_minutes"]) != Some(10080.0) {
                continue;
            }
            if let (Some(u), Some(r)) = (percent(&w["used_percent"]), number(&w["resets_at"]))
                && r > now
                && best_weekly.is_none_or(|(_, _, prev_obs)| {
                    observed.is_some_and(|o| prev_obs.is_none_or(|po| o > po))
                })
            {
                best_weekly = Some((u, r, observed));
            }
        }
        if best_weekly.is_some() && (!fallback_older || idx == 0) {
            break;
        }
    }
    match best_weekly {
        Some((u, r, o)) => (Some(u), Some(r), o, newest_shorts),
        None => (None, None, None, newest_shorts),
    }
}

fn scan_filtered(
    dir: &std::path::Path,
    expected_len: usize,
    want_dir: bool,
    remaining: &mut usize,
    pred: impl Fn(&str) -> bool,
) -> Vec<std::path::PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        if *remaining == 0 {
            return out;
        }
        *remaining -= 1;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if (expected_len != usize::MAX && name_str.len() != expected_len) || !pred(name_str) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if if want_dir {
            meta.is_dir()
        } else {
            meta.is_file()
        } {
            out.push(path);
        }
    }
    out.sort_by(|a, b| b.cmp(a));
    out
}

fn read_rate_limits(
    path: &std::path::Path,
    expected_ino: u64,
    expected_size: u64,
    expected_mtime: i128,
) -> Option<(Value, Option<f64>)> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    let fd = rustix::fs::open(path, flags, rustix::fs::Mode::empty()).ok()?;
    let mut file = std::fs::File::from(fd);
    let meta = file.metadata().ok()?;
    let actual_mtime = meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128;
    if !meta.is_file()
        || meta.ino() != expected_ino
        || meta.size() != expected_size
        || actual_mtime != expected_mtime
    {
        return None;
    }
    let start = expected_size.saturating_sub(CODEX_TAIL_BYTES as u64);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((expected_size - start) as usize);
    file.take(CODEX_TAIL_BYTES as u64)
        .read_to_end(&mut buf)
        .ok()?;
    let slice = if start > 0 {
        buf.iter()
            .position(|&b| b == b'\n')
            .map(|idx| &buf[idx + 1..])
            .unwrap_or(&[])
    } else {
        &buf[..]
    };
    for line in slice.rsplit(|&b| b == b'\n') {
        if line.is_empty() || !line.windows(13).any(|w| w == b"\"rate_limits\"") {
            continue;
        }
        let Ok(row) = crate::decode(line, CODEX_TAIL_BYTES) else {
            continue;
        };
        let limits = row
            .get("rate_limits")
            .or_else(|| row.get("payload").and_then(|p| p.get("rate_limits")));
        if let Some(lim) = limits
            && lim.is_object()
        {
            return Some((lim.clone(), stamp(&row["timestamp"])));
        }
    }
    None
}
