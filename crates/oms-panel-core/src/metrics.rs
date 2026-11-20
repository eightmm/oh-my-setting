//! Recorded evidence only. Missing samples never become zero-valued measurements.
use crate::value::text;
use crate::{Error, Result, Value, require};
use oms_command_telemetry::store::{
    MAX_INPUT_BYTES, StoreError,
    usage::{self, Projection},
};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// One strict receipt projection, reused only at the same repository generation.
#[derive(Default)]
pub struct ReceiptTokensCache {
    entry: Option<(ReceiptGeneration, BTreeMap<String, ReceiptAttempt>)>,
}

struct ReceiptGeneration {
    repo: PathBuf,
    chain: Vec<Stat>,
}

impl ReceiptGeneration {
    fn read(repo: &Path) -> Option<Self> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut fd = fs::open(repo, flags, Mode::empty()).ok()?;
        let mut chain = vec![fs::fstat(&fd).ok()?];
        let receipt = Path::new(usage::RECEIPTS);
        for component in receipt.parent()?.components() {
            let std::path::Component::Normal(name) = component else {
                return None;
            };
            fd = fs::openat(&fd, name, flags, Mode::empty()).ok()?;
            chain.push(fs::fstat(&fd).ok()?);
        }
        let leaf = fs::statat(&fd, receipt.file_name()?, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        if FileType::from_raw_mode(leaf.st_mode) != FileType::RegularFile
            || leaf.st_nlink != 1
            || leaf.st_uid != rustix::process::getuid().as_raw()
            || usize::try_from(leaf.st_size).ok()? > MAX_INPUT_BYTES
        {
            return None;
        }
        chain.push(leaf);
        Some(Self {
            repo: repo.to_owned(),
            chain,
        })
    }
}

impl PartialEq for ReceiptGeneration {
    fn eq(&self, other: &Self) -> bool {
        self.repo == other.repo
            && self.chain.len() == other.chain.len()
            && self.chain.iter().zip(&other.chain).all(|(before, after)| {
                before.st_dev == after.st_dev
                    && before.st_ino == after.st_ino
                    && before.st_size == after.st_size
                    && before.st_mtime == after.st_mtime
                    && before.st_mtime_nsec == after.st_mtime_nsec
                    && before.st_ctime == after.st_ctime
                    && before.st_ctime_nsec == after.st_ctime_nsec
                    && before.st_nlink == after.st_nlink
                    && before.st_mode == after.st_mode
                    && before.st_uid == after.st_uid
            })
    }
}

struct ReceiptSum {
    reported: usize,
    sum: Option<u64>,
}

impl Default for ReceiptSum {
    fn default() -> Self {
        Self {
            reported: 0,
            sum: Some(0),
        }
    }
}

impl ReceiptSum {
    fn observe(&mut self, next: Option<u64>) {
        if let Some(next) = next {
            self.reported += 1;
            self.sum = self.sum.and_then(|sum| sum.checked_add(next));
        }
    }

    fn complete(&self, calls: usize) -> Option<u64> {
        (self.reported == calls).then_some(self.sum).flatten()
    }
}

#[derive(Default)]
struct ReceiptProvider {
    calls: usize,
    input_basis: Option<bool>,
    components: [ReceiptSum; 4],
}

#[derive(Default)]
struct ReceiptAttempt {
    calls: usize,
    total: ReceiptSum,
    providers: BTreeMap<String, ReceiptProvider>,
}

impl ReceiptAttempt {
    fn snapshot(&self) -> Value {
        let providers: serde_json::Map<_, _> = self
            .providers
            .iter()
            .map(|(name, provider)| {
                let [input, output, read, write] = &provider.components;
                let complete = |sum: &ReceiptSum| sum.complete(provider.calls);
                let comparable = provider.calls == 1 || provider.input_basis.is_some();
                let input_reported = if comparable { input.reported } else { 0 };
                let input = comparable.then(|| complete(input)).flatten();
                (
                    name.clone(),
                    json!({"input":input,"output":complete(output),
                        "cache_read":complete(read),"cache_write":complete(write),
                        "calls":provider.calls,
                        "reported":{"input":input_reported,"output":output.reported,
                            "cache_read":read.reported,"cache_write":write.reported}}),
                )
            })
            .collect();
        json!({"total":self.total.complete(self.calls),"reported":self.total.reported,
            "calls":self.calls,"providers":providers})
    }
}

impl ReceiptTokensCache {
    /// Select only requested attempt IDs for a private Board snapshot. The
    /// repository must be the collector's canonical state root.
    pub fn for_attempts<'a>(
        &mut self,
        repo: &Path,
        attempts: impl IntoIterator<Item = &'a str>,
        stop: &AtomicBool,
    ) -> Option<serde_json::Map<String, Value>> {
        let by_attempt = self.refresh(repo, stop, usage::load)?;
        Some(
            attempts
                .into_iter()
                .filter_map(|id| Some((id.to_owned(), by_attempt.get(id)?.snapshot())))
                .collect(),
        )
    }

    fn refresh(
        &mut self,
        repo: &Path,
        stop: &AtomicBool,
        load: impl FnOnce(&Path) -> std::result::Result<Projection, StoreError>,
    ) -> Option<&BTreeMap<String, ReceiptAttempt>> {
        let prior = self.entry.take();
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let before = ReceiptGeneration::read(repo)?;
        let by_attempt = match prior {
            Some((stamp, by_attempt)) if stamp == before => by_attempt,
            _ => {
                let mut by_attempt = BTreeMap::<String, ReceiptAttempt>::new();
                let projection = load(repo).ok()?;
                let mut bound = projection
                    .calls
                    .iter()
                    .map(|call| &call.receipt)
                    .filter(|receipt| receipt.identity.attempt_id.is_some());
                for call in projection.attributed_calls() {
                    let receipt = bound.next()?;
                    if receipt.identity.call_id != call.call_id {
                        return None;
                    }
                    let attempt = by_attempt.entry(call.attempt_id.to_owned()).or_default();
                    attempt.calls += 1;
                    attempt.total.observe(call.usage.total_tokens);
                    let provider = attempt
                        .providers
                        .entry(call.provider.to_owned())
                        .or_default();
                    let basis = receipt.usage.cache_in_input;
                    provider.input_basis = if provider.calls == 0 || provider.input_basis == basis {
                        basis
                    } else {
                        None
                    };
                    provider.calls += 1;
                    for (sum, tokens) in provider.components.iter_mut().zip([
                        call.usage.input_tokens,
                        call.usage.output_tokens,
                        call.usage.cache_read_tokens,
                        call.usage.cache_write_tokens,
                    ]) {
                        sum.observe(tokens);
                    }
                }
                if bound.next().is_some() {
                    return None;
                }
                by_attempt
            }
        };
        if stop.load(Ordering::Relaxed) || ReceiptGeneration::read(repo)? != before {
            return None;
        }
        Some(&self.entry.insert((before, by_attempt)).1)
    }
}

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
    let rows = repo.and_then(|r| crate::results::index_rows(r).ok());
    collect_local_with(report, main_attempt, now, rows.as_deref())
}
/// `collect_local` over artifact-index rows the caller already read.
pub fn collect_local_with(
    report: &Value,
    main_attempt: Option<&str>,
    now: f64,
    index: Option<&[Value]>,
) -> Value {
    collect_local_with_sources(report, main_attempt, now, index).0
}

/// In-memory evidence for the home that supplied each Codex quota reading.
#[derive(Clone, Default)]
pub struct CodexQuotaSources {
    ambient: Option<String>,
    accounts: BTreeMap<String, String>,
}

impl CodexQuotaSources {
    pub fn ambient_matches(&self, digest: &str) -> bool {
        self.ambient.as_deref() == Some(digest)
    }

    pub fn account_matches(&self, alias: &str, digest: &str) -> bool {
        self.accounts.get(alias).is_some_and(|home| home == digest)
    }
}

/// Collect telemetry and its non-serializable Codex source evidence together.
pub fn collect_local_with_sources(
    report: &Value,
    main_attempt: Option<&str>,
    now: f64,
    index: Option<&[Value]>,
) -> (Value, CodexQuotaSources) {
    let participants = crate::value::array(&report["room"]["participants"]);
    let mut output = serde_json::Map::new();
    let mut sources = CodexQuotaSources::default();

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

        if provider == "codex" {
            let CodexReadings {
                weekly_used,
                resets_at,
                observed_at,
                shorts,
                home,
            } = codex_readings(None, now, true);
            if let Some(u) = weekly_used {
                row.insert("weekly_used".into(), u.into());
                row.insert("source".into(), "session log".into());
            }
            if !shorts.is_empty() {
                if row.get("weekly_used").is_none() {
                    row.insert("source".into(), "session log".into());
                }
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
                row.insert("short_windows".into(), Value::Array(shorts));
            }
            if let Some(r) = resets_at {
                row.insert("weekly_resets_at".into(), r.into());
            }
            if let Some(o) = observed_at {
                row.insert("observed_at".into(), o.into());
            }
            sources.ambient = home;
        }
        account_status(provider, now, &mut row, &mut sources);

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
        output.insert(provider.into(), Value::Object(row));
    }
    let mut pi = serde_json::Map::new();
    account_status("pi", now, &mut pi, &mut sources);
    if !pi.is_empty() {
        output.insert("pi".into(), Value::Object(pi));
    }

    let mut agy = serde_json::Map::new();
    agy.insert("context_left".into(), Value::Null);
    agy.insert("weekly_used".into(), Value::Null);
    agy.insert("source".into(), "unavailable".into());
    if let Some(rows) = index {
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
    (Value::Object(output), sources)
}

/// Per-account readings and the active account label for an account-isolated
/// provider. Only Codex session logs carry per-home usage; other providers
/// report availability with `weekly_used: null`.
fn account_status(
    provider: &str,
    now: f64,
    row: &mut serde_json::Map<String, Value>,
    sources: &mut CodexQuotaSources,
) {
    let config = match crate::routing::Accounts::configured() {
        Ok(Some(config)) => config,
        Ok(None) => return,
        Err(e) => {
            row.insert("accounts_error".into(), e.message.into());
            return;
        }
    };
    let Some(config) = config.provider(provider) else {
        return;
    };
    let mut accs = serde_json::Map::new();
    let active_home =
        crate::routing::active_account_home(provider).and_then(|p| std::fs::canonicalize(p).ok());
    if provider == "codex"
        && active_home
            .as_deref()
            .map(|path| crate::digest(path.as_os_str().as_encoded_bytes()))
            != sources.ambient
    {
        sources.ambient = None;
    }
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
            Ok(home_path) if crate::routing::private_home(&home_path) => {
                let CodexReadings {
                    weekly_used,
                    resets_at,
                    observed_at,
                    home,
                    ..
                } = if provider == "codex" {
                    codex_readings(Some(&home_path), now, false)
                } else {
                    CodexReadings::default()
                };
                let mut info = serde_json::Map::new();
                info.insert(
                    "weekly_used".into(),
                    weekly_used.map(Value::from).unwrap_or(Value::Null),
                );
                if let Some(u) = weekly_used {
                    info.insert("used".into(), u.into());
                    info.insert("source".into(), "session log".into());
                    if let Some(home) = home {
                        sources.accounts.insert(entry.id.clone(), home);
                    }
                }
                if let Some(r) = resets_at {
                    info.insert("resets_at".into(), r.into());
                }
                if let Some(o) = observed_at {
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

#[derive(Default)]
struct CodexReadings {
    weekly_used: Option<u8>,
    resets_at: Option<f64>,
    observed_at: Option<f64>,
    shorts: Vec<Value>,
    home: Option<String>,
}

fn codex_readings(
    explicit_home: Option<&std::path::Path>,
    now: f64,
    fallback_older: bool,
) -> CodexReadings {
    let default_home = explicit_home.is_none();
    let home = explicit_home
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("CODEX_HOME").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex")));
    let Some(home) = home.and_then(|h| std::fs::canonicalize(h).ok()) else {
        return CodexReadings::default();
    };
    let sessions_root = home.join("sessions");
    let mut remaining = MAX_SCAN_ENTRIES;
    let mut files: Vec<(i128, u64, u64, std::path::PathBuf)> = Vec::new();
    let years = scan_filtered(&sessions_root, 4, true, &mut remaining, |name| {
        name.len() == 4 && name.bytes().all(|b| b.is_ascii_digit())
    });
    'search: for (year, _) in years {
        let months = scan_filtered(&year, 2, true, &mut remaining, |name| {
            matches!(
                name,
                "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12"
            )
        });
        for (month, _) in months {
            let days = scan_filtered(&month, 2, true, &mut remaining, |name| {
                name.len() == 2 && matches!(name.parse::<u8>(), Ok(1..=31))
            });
            for (day, _) in days {
                let mut found = scan_filtered(&day, usize::MAX, false, &mut remaining, |name| {
                    name.starts_with("rollout-") && name.ends_with(".jsonl")
                });
                for (path, meta) in found.drain(..) {
                    use std::os::unix::fs::MetadataExt;
                    let mtime = meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128;
                    files.push((mtime, meta.ino(), meta.size(), path));
                }
                if files.len() >= 16 && (!fallback_older || has_week_reading(&files, now)) {
                    break 'search;
                }
            }
        }
    }
    if files.is_empty() {
        return CodexReadings::default();
    }
    files.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut best_weekly: Option<(u8, f64, Option<f64>)> = None;
    let mut newest_shorts: Vec<Value> = Vec::new();
    let mut source_proven = crate::routing::private_home(&home);
    let limit = if fallback_older { 64 } else { 1 };
    for (idx, (mtime, ino, size, path)) in files.into_iter().take(limit).enumerate() {
        if default_home && idx == 0 && now - (mtime as f64 / 1e9) > CODEX_WEEK_SECONDS {
            break;
        }
        let route_matches = || path.canonicalize().is_ok_and(|resolved| resolved == path);
        let before = source_proven && route_matches();
        let Some((limits, observed)) = read_rate_limits(&path, ino, size, mtime) else {
            continue;
        };
        source_proven = before && route_matches();
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
    let (weekly_used, resets_at, observed_at) = best_weekly
        .map_or((None, None, None), |(used, reset, observed)| {
            (Some(used), Some(reset), observed)
        });
    let home = (source_proven && (weekly_used.is_some() || !newest_shorts.is_empty()))
        .then(|| crate::digest(home.as_os_str().as_encoded_bytes()));
    CodexReadings {
        weekly_used,
        resets_at,
        observed_at,
        shorts: newest_shorts,
        home,
    }
}

/// True when the newest files already yield a valid weekly reading, so older
/// day directories cannot change the result.
fn has_week_reading(files: &[(i128, u64, u64, std::path::PathBuf)], now: f64) -> bool {
    let mut newest: Vec<_> = files.iter().collect();
    newest.sort_by_key(|f| std::cmp::Reverse(f.0));
    newest.into_iter().take(16).any(|(mtime, ino, size, path)| {
        let Some((limits, _)) = read_rate_limits(path, *ino, *size, *mtime) else {
            return false;
        };
        ["primary", "secondary"].iter().any(|key| {
            let w = &limits[*key];
            number(&w["window_minutes"]) == Some(10080.0)
                && percent(&w["used_percent"]).is_some()
                && number(&w["resets_at"]).is_some_and(|r| r > now)
        })
    })
}

fn scan_filtered(
    dir: &std::path::Path,
    expected_len: usize,
    want_dir: bool,
    remaining: &mut usize,
    pred: impl Fn(&str) -> bool,
) -> Vec<(std::path::PathBuf, std::fs::Metadata)> {
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
            out.push((path, meta));
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// Locate a Codex rollout by the same native-session digest the Room stores.
pub fn codex_rollout(consumer: &str) -> Option<std::path::PathBuf> {
    if !crate::hex(consumer, 32) {
        return None;
    }
    let home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex")))?;
    let sessions = std::fs::canonicalize(home).ok()?.join("sessions");
    if !std::fs::symlink_metadata(&sessions).ok()?.is_dir() {
        return None;
    }
    let digits = |name: &str| name.bytes().all(|b| b.is_ascii_digit());
    let mut remaining = MAX_SCAN_ENTRIES;
    for (year, _) in scan_filtered(&sessions, 4, true, &mut remaining, digits) {
        for (month, _) in scan_filtered(&year, 2, true, &mut remaining, digits) {
            for (day, _) in scan_filtered(&month, 2, true, &mut remaining, digits) {
                let found = scan_filtered(&day, usize::MAX, false, &mut remaining, |name| {
                    let Some(stem) = name
                        .strip_suffix(".jsonl")
                        .filter(|_| name.starts_with("rollout-"))
                    else {
                        return false;
                    };
                    stem.len()
                        .checked_sub(36)
                        .and_then(|start| stem.get(start..))
                        .is_some_and(|id| &crate::digest(id.as_bytes())[..32] == consumer)
                });
                if let Some((path, _)) = found.into_iter().next() {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Read only the latest complete turn model from a bound Codex rollout.
pub fn codex_turn_model(path: &std::path::Path, consumer: &str) -> Option<String> {
    codex_turn_observations(path, consumer)?.0
}

pub fn codex_turn_observations(
    path: &std::path::Path,
    consumer: &str,
) -> Option<(Option<String>, Option<u64>)> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;
    const MODEL_TAIL: u64 = 1024 * 1024;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    let fd = rustix::fs::open(path, flags, rustix::fs::Mode::empty()).ok()?;
    let mut file = std::fs::File::from(fd);
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.nlink() != 1 {
        return None;
    }
    let mut first = Vec::new();
    use std::io::BufRead;
    std::io::BufReader::new((&mut file).take(CODEX_TAIL_BYTES as u64))
        .read_until(b'\n', &mut first)
        .ok()?;
    if first.last() != Some(&b'\n') {
        return None;
    }
    let meta_row = crate::decode(&first, CODEX_TAIL_BYTES).ok()?;
    let id = meta_row["payload"]["id"].as_str()?;
    if meta_row["type"] != "session_meta" || &crate::digest(id.as_bytes())[..32] != consumer {
        return None;
    }
    let start = meta.len().saturating_sub(MODEL_TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.take(MODEL_TAIL).read_to_end(&mut tail).ok()?;
    let first = if start == 0 {
        Some(0)
    } else {
        tail.iter().position(|&b| b == b'\n').map(|at| at + 1)
    };
    let last = tail.iter().rposition(|&b| b == b'\n');
    let Some(complete) = first
        .zip(last)
        .and_then(|(start, end)| tail.get(start..end))
    else {
        return Some((None, None));
    };
    let mut model = None;
    let mut total_tokens = None;
    for line in complete.rsplit(|&b| b == b'\n') {
        let is_turn = model.is_none() && line.windows(14).any(|s| s == b"\"turn_context\"");
        let is_usage =
            total_tokens.is_none() && line.windows(20).any(|s| s == b"\"token_usage_record\"");
        if !is_turn && !is_usage {
            continue;
        }
        let Ok(row) = crate::decode(line, MODEL_TAIL as usize) else {
            continue;
        };
        if is_turn
            && row["type"] == "turn_context"
            && let Some(value) = row["payload"]["model"].as_str().filter(|value| {
                !value.is_empty()
                    && value.len() <= 120
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            model = Some(value.to_owned());
        }
        if is_usage
            && row["type"] == "token_usage_record"
            && row["payload"]["thread_id"].as_str() == Some(id)
            && row["payload"]["session_id"].as_str() == Some(id)
        {
            total_tokens = row["payload"]["thread_token_usage"]["total_tokens"].as_u64();
        }
        if model.is_some() && total_tokens.is_some() {
            break;
        }
    }
    Some((model, total_tokens))
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

#[cfg(test)]
mod receipt_token_tests {
    use super::*;
    use oms_command_telemetry::store::usage::{Cumulative, Identity, Phase, Usage};

    struct Repo(PathBuf);

    impl Repo {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "oms-receipt-tokens-{}",
                oms_command_plan::storage::random_hex(8).unwrap()
            ));
            fs::mkdir(&path, Mode::from_raw_mode(0o700)).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(
        repo: &Path,
        call: &str,
        attempt: &str,
        provider: &str,
        session: Option<(&str, bool)>,
        usage: Option<Usage>,
    ) {
        let stop = AtomicBool::new(false);
        let mut receipt = usage::begin(
            repo,
            Identity {
                call_id: call.into(),
                operation_id: format!("op-{call}"),
                provider: provider.into(),
                account: Some("seat".into()),
                attempt_id: Some(attempt.into()),
                ..Identity::default()
            },
            session.map(|(id, baseline_unknown)| Cumulative {
                session_id: id.into(),
                baseline_unknown,
            }),
            &stop,
        )
        .unwrap();
        receipt.revision = 1;
        if let Some(usage) = usage {
            receipt.phase = Phase::Final;
            receipt.usage = usage;
        } else {
            receipt.phase = Phase::Unknown;
        }
        usage::record(repo, &receipt, &stop).unwrap();
    }

    #[test]
    fn receipt_tokens_cover_exact_calls_without_mixing_provider_components() {
        let repo = Repo::new();
        let path = &repo.0;
        let codex = |input, output, read| Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_read_tokens: Some(read),
            cache_in_input: Some(true),
            ..Usage::default()
        };
        let claude = Usage {
            input_tokens: Some(6),
            output_tokens: Some(2),
            cache_read_tokens: Some(60),
            cache_write_tokens: Some(3),
            cache_in_input: Some(false),
            ..Usage::default()
        };
        let explicit = |total| Usage {
            total_tokens: Some(total),
            ..Usage::default()
        };
        for (call, attempt, provider, session, usage) in [
            (
                "fresh",
                "mixed",
                "codex",
                Some(("session", false)),
                Some(codex(50, 10, 20)),
            ),
            (
                "resume",
                "mixed",
                "codex",
                Some(("session", true)),
                Some(codex(70, 15, 28)),
            ),
            ("fallback", "mixed", "claude", None, Some(claude.clone())),
            (
                "agy",
                "mixed",
                "antigravity",
                None,
                Some(Usage {
                    input_tokens: Some(4),
                    output_tokens: Some(3),
                    cache_read_tokens: Some(1),
                    total_tokens: Some(10),
                    ..Usage::default()
                }),
            ),
            ("gap", "incomplete", "codex", Some(("session", true)), None),
            (
                "after-gap",
                "incomplete",
                "codex",
                Some(("session", true)),
                Some(codex(80, 20, 30)),
            ),
            (
                "later",
                "later",
                "codex",
                Some(("session", true)),
                Some(codex(87, 22, 31)),
            ),
            (
                "large",
                "overflow",
                "antigravity",
                None,
                Some(explicit(u64::MAX)),
            ),
            (
                "addition",
                "overflow",
                "antigravity",
                None,
                Some(explicit(1)),
            ),
            ("zero", "zero", "antigravity", None, Some(explicit(0))),
            ("inclusive", "basis", "codex", None, Some(codex(4, 2, 1))),
            ("exclusive", "basis", "codex", None, Some(claude)),
            ("whole", "partial", "codex", None, Some(codex(4, 2, 1))),
            (
                "sparse",
                "partial",
                "codex",
                None,
                Some(Usage {
                    output_tokens: Some(5),
                    cache_in_input: Some(true),
                    ..Usage::default()
                }),
            ),
        ] {
            write(path, call, attempt, provider, session, usage);
        }
        let attempts = [
            "mixed",
            "incomplete",
            "later",
            "overflow",
            "zero",
            "basis",
            "partial",
            "foreign",
        ];
        let observed = ReceiptTokensCache::default()
            .for_attempts(path, attempts, &AtomicBool::new(false))
            .unwrap();
        assert!(!observed.contains_key("foreign"));
        assert_eq!(
            observed["mixed"],
            json!({
                "total":166,"reported":4,"calls":4,
                "providers": {
                    "codex":{"input":70,"output":15,"cache_read":28,"cache_write":null,"calls":2,
                        "reported":{"input":2,"output":2,"cache_read":2,"cache_write":0}},
                    "claude":{"input":6,"output":2,"cache_read":60,"cache_write":3,"calls":1,
                        "reported":{"input":1,"output":1,"cache_read":1,"cache_write":1}},
                    "antigravity":{"input":4,"output":3,"cache_read":1,"cache_write":null,"calls":1,
                        "reported":{"input":1,"output":1,"cache_read":1,"cache_write":0}}
                }
            })
        );
        assert_eq!(observed["incomplete"]["reported"], 0);
        assert_eq!(observed["incomplete"]["calls"], 2);
        assert!(observed["incomplete"]["total"].is_null());
        assert_eq!(observed["later"]["total"], 9);
        assert_eq!(observed["overflow"]["reported"], 2);
        assert!(observed["overflow"]["total"].is_null());
        assert_eq!(observed["zero"]["total"], 0);
        assert_eq!(observed["basis"]["total"], 77);
        assert!(observed["basis"]["providers"]["codex"]["input"].is_null());
        assert_eq!(observed["basis"]["providers"]["codex"]["output"], 4);
        let sparse = &observed["partial"]["providers"]["codex"];
        assert_eq!(sparse["calls"], 2);
        assert_eq!(sparse["output"], 7);
        assert!(sparse["input"].is_null() && sparse["cache_read"].is_null());
        assert_eq!(
            sparse["reported"],
            json!({"input":1,"output":2,"cache_read":1,"cache_write":0})
        );
        assert_eq!(observed["basis"]["providers"]["codex"]["reported"]["input"], 0);
    }

    #[test]
    fn receipt_cache_reuses_only_the_proved_generation_and_drops_invalid_data() {
        use std::os::unix::fs::symlink;
        let repo = Repo::new();
        let stop = AtomicBool::new(false);
        let mut cache = ReceiptTokensCache::default();
        let path = repo.0.join(usage::RECEIPTS);
        let attempt = "attempt-one";
        let load = |cache: &mut ReceiptTokensCache| cache.for_attempts(&repo.0, [attempt], &stop);
        assert!(load(&mut cache).is_none());
        let record = |call, total| {
            write(
                &repo.0,
                call,
                attempt,
                "antigravity",
                None,
                Some(Usage {
                    total_tokens: Some(total),
                    ..Usage::default()
                }),
            );
        };
        record("one", 1);
        assert_eq!(load(&mut cache).unwrap()[attempt]["total"], 1);
        let warm = cache.refresh(&repo.0, &stop, |_| panic!("warm cache reparsed receipts"));
        assert_eq!(warm.unwrap()[attempt].snapshot()["total"], 1);

        record("two", 2); // Normal publication replaces the leaf.
        assert_eq!(load(&mut cache).unwrap()[attempt]["total"], 3);
        record("three", 3);
        assert!(
            cache
                .refresh(&repo.0, &stop, |repo| {
                    let snapshot = usage::load(repo);
                    record("four", 4);
                    snapshot
                })
                .is_none()
        );
        assert_eq!(load(&mut cache).unwrap()[attempt]["total"], 10);

        let other = Repo::new();
        assert!(cache.for_attempts(&other.0, [attempt], &stop).is_none());
        assert_eq!(load(&mut cache).unwrap()[attempt]["total"], 10);
        let displaced = path.with_file_name("saved-receipts");
        std::fs::rename(&path, &displaced).unwrap();
        assert!(load(&mut cache).is_none());
        symlink(&displaced, &path).unwrap();
        assert!(load(&mut cache).is_none());
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&displaced, &path).unwrap();
        assert!(load(&mut cache).is_none());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"{bad receipt}").unwrap();
        assert!(load(&mut cache).is_none());
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(u64::try_from(MAX_INPUT_BYTES).unwrap() + 1)
            .unwrap();
        assert!(
            cache
                .refresh(&repo.0, &stop, |_| panic!("oversized ledger was parsed"))
                .is_none()
        );
        assert!(cache.entry.is_none());
    }
}
