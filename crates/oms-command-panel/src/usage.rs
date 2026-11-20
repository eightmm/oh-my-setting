//! Local quota and usage projection matching `provider_usage` schema 1.

use crate::{rows as array, text as vtext};
use oms_panel_core::{Value, metrics};
use serde_json::{Map, json};
use std::collections::{BTreeMap, HashMap};

pub const PROVIDERS: &[&str] = &["claude", "codex", "antigravity"];
const FIVE_HOUR_SECONDS: f64 = 5.0 * 3600.0;
const WEEK_SECONDS: f64 = 7.0 * 86400.0;
const TERMINAL_STATES: &[&str] = &[
    "done",
    "failed",
    "stopped",
    "cancelled",
    "canceled",
    "exited",
    "accepted",
    "rejected",
    "completed",
    "retired",
    "dead",
    "stale",
];

fn warn_threshold() -> Option<u8> {
    std::env::var("OMS_QUOTA_WARN_PCT")
        .ok()
        .and_then(|raw| raw.trim().parse::<u8>().ok())
        .filter(|&pct| pct <= 100)
}

pub(crate) fn format_duration(seconds: f64) -> String {
    let secs = seconds.max(0.0).floor() as u64;
    if secs >= 86400 {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        format!("{d}d{h}h")
    } else if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h{m:02}m")
    } else {
        let m = secs / 60;
        format!("{m}m")
    }
}

pub(crate) fn format_age(seconds: f64) -> String {
    let secs = seconds.max(0.0).floor() as u64;
    if secs >= 86400 {
        format!("{}d", secs / 86400)
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn window_length(name: &str) -> Option<f64> {
    match name {
        "5h" => Some(FIVE_HOUR_SECONDS),
        "weekly" => Some(WEEK_SECONDS),
        _ => None,
    }
}

/// Compute pace of quota usage relative to elapsed window duration.
pub fn pace(name: &str, used: u8, resets_at: Option<f64>, now: f64, stale: bool) -> Value {
    let Some(length) = window_length(name) else {
        return json!({"unreported": "window length not known"});
    };
    if stale {
        return json!({"unreported": "reading is stale"});
    }
    let Some(reset) = resets_at.filter(|r| r.is_finite() && *r > now) else {
        return json!({"unreported": "no usable reset time"});
    };
    let remaining = reset - now;
    if remaining > length {
        return json!({"unreported": "reset is further out than window length"});
    }
    let elapsed_pct = (((length - remaining) / length) * 100.0)
        .round()
        .clamp(0.0, 100.0) as i64;
    if elapsed_pct < 5 {
        return json!({"unreported": "window just started"});
    }
    let diff = i64::from(used) - elapsed_pct;
    let verdict = if diff > 5 {
        "ahead"
    } else if diff < -5 {
        "behind"
    } else {
        "even"
    };
    json!({
        "elapsed_pct": elapsed_pct,
        "verdict": verdict,
    })
}

fn window_level(used: u8, stale: bool, warn_pct: Option<u8>) -> Value {
    if stale {
        return Value::Null;
    }
    if 100u8.saturating_sub(used) <= 10 {
        return json!("critical");
    }
    if warn_pct.is_some_and(|w| used >= w) {
        return json!("warn");
    }
    Value::Null
}

#[allow(clippy::too_many_arguments)]
fn build_window(
    name: &str,
    used: u8,
    resets_at: Option<f64>,
    source: &str,
    stale: bool,
    age_seconds: Option<f64>,
    now: f64,
    warn_pct: Option<u8>,
) -> Value {
    let resets_in = resets_at
        .filter(|r| r.is_finite() && *r > now)
        .map(|r| Value::String(format_duration(r - now)))
        .unwrap_or(Value::Null);
    json!({
        "name": name,
        "used": used,
        "left": 100u8.saturating_sub(used),
        "resets_at": resets_at,
        "resets_in": resets_in,
        "source": source,
        "stale": stale,
        "age_seconds": age_seconds,
        "level": window_level(used, stale, warn_pct),
        "pace": pace(name, used, resets_at, now, stale),
    })
}

fn seats_by_provider(report: &Value) -> BTreeMap<String, (usize, usize)> {
    let mut attempt_states: HashMap<String, String> = HashMap::new();
    for key in ["active_recent", "recent"] {
        for attempt in array(&report["attempts"][key]) {
            let state = vtext(attempt, "state");
            if let Some(id) = attempt["attempt_id"].as_str().filter(|s| !s.is_empty()) {
                attempt_states.insert(id.to_owned(), state.to_owned());
            }
            if let Some(part) = attempt["panel"]["room_participant"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    attempt["refs"]["panel_room_participant"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                })
            {
                attempt_states
                    .entry(part.to_owned())
                    .or_insert_with(|| state.to_owned());
            }
        }
    }
    if let Some(map) = report["room_attempts"].as_object() {
        for (id, attempt) in map {
            let state = vtext(attempt, "state");
            attempt_states.insert(id.clone(), state.to_owned());
        }
    }

    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for member in array(&report["room"]["participants"]) {
        let joined = member["joined"].as_bool().unwrap_or(false);
        let role = vtext(member, "role");
        let participant = vtext(member, "participant");
        let provider = vtext(member, "provider");
        if provider.is_empty() || !joined {
            continue;
        }
        if role != "main" && role != "worker" {
            continue;
        }
        let state = attempt_states
            .get(participant)
            .map(String::as_str)
            .unwrap_or_else(|| member["state"].as_str().unwrap_or(""));
        if TERMINAL_STATES.contains(&state) {
            continue;
        }
        let entry = counts.entry(provider.to_owned()).or_insert((0, 0));
        if role == "main" {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    counts
}

fn held_items(report: &Value) -> Value {
    let mut items = Vec::new();
    for worker in array(&report["attempts"]["active_recent"]) {
        let state = vtext(worker, "state");
        let reason = vtext(worker, "reason");
        if state == "held" || reason.contains("quota") {
            items.push(json!({
                "attempt_id": worker["attempt_id"],
                "provider": worker["provider"],
                "reason": reason,
            }));
        }
    }
    Value::Array(items)
}

fn provider_snapshot(
    provider: &str,
    ps: &Value,
    seats: (usize, usize),
    now: f64,
    warn_pct: Option<u8>,
) -> Value {
    let native_weekly = metrics::stamp(&ps["weekly_native_observed_at"]);
    let lead_weekly = metrics::stamp(&ps["weekly_lead_observed_at"]);
    let observed_at = native_weekly
        .or(lead_weekly)
        .or_else(|| metrics::stamp(&ps["observed_at"]));
    let age_seconds = observed_at.map(|ts| (now - ts).max(0.0));
    let raw_source = if native_weekly.is_some() {
        if provider == "antigravity" {
            "AGY Gemini quota"
        } else {
            "Claude native usage"
        }
    } else if lead_weekly.is_some() {
        "Claude lead HUD"
    } else {
        ps["source"].as_str().unwrap_or("unavailable")
    };
    let stale = raw_source == "cache stale"
        || age_seconds.is_some_and(|age| age > metrics::READING_FRESH_SECONDS);
    let source_label = match provider {
        "claude" => {
            if raw_source == "cache stale" {
                "HUD cache"
            } else {
                raw_source
            }
        }
        "codex" => {
            if raw_source == "session log" {
                "Codex session log"
            } else {
                raw_source
            }
        }
        _ => raw_source,
    };

    let mut windows: Vec<Value> = Vec::new();
    let mut unreported = Map::new();

    match provider {
        "claude" => {
            if raw_source == "cache stale" {
                let reason = "HUD cache reading is stale; numbers not shown";
                unreported.insert("5h".into(), json!(reason));
                unreported.insert("weekly".into(), json!(reason));
            } else {
                if let (Some(used), Some(resets_at)) = (
                    metrics::percent(&ps["five_hour_used"]),
                    metrics::stamp(&ps["five_hour_resets_at"]).filter(|r| *r > now),
                ) {
                    windows.push(build_window(
                        "5h",
                        used,
                        Some(resets_at),
                        source_label,
                        stale,
                        age_seconds,
                        now,
                        warn_pct,
                    ));
                } else {
                    unreported.insert("5h".into(), json!("HUD cache has no 5-hour reading"));
                }
                if let Some(used) = metrics::percent(&ps["weekly_used"]) {
                    let resets_at = metrics::stamp(&ps["weekly_resets_at"]).filter(|r| *r > now);
                    windows.push(build_window(
                        "weekly",
                        used,
                        resets_at,
                        source_label,
                        stale,
                        age_seconds,
                        now,
                        warn_pct,
                    ));
                } else {
                    unreported.insert("weekly".into(), json!("HUD cache has no weekly reading"));
                }
            }
        }
        "codex" => {
            let mut has_5h = false;
            let mut shorts: Vec<(u64, u8, Option<f64>)> = Vec::new();
            for row in array(&ps["short_windows"]) {
                let Some(minutes) = metrics::number(&row["minutes"]) else {
                    continue;
                };
                if minutes <= 0.0 || minutes == 10080.0 || minutes.fract() != 0.0 {
                    continue;
                }
                let Some(used) = metrics::percent(&row["used"]) else {
                    continue;
                };
                let Some(resets_at) = metrics::stamp(&row["resets_at"]).filter(|r| *r > now) else {
                    continue;
                };
                shorts.push((minutes as u64, used, Some(resets_at)));
            }
            if shorts.is_empty()
                && let (Some(used), Some(resets_at)) = (
                    metrics::percent(&ps["five_hour_used"]),
                    metrics::stamp(&ps["five_hour_resets_at"]).filter(|r| *r > now),
                )
            {
                shorts.push((300, used, Some(resets_at)));
            }
            shorts.sort_by_key(|(m, _, _)| *m);
            for (minutes, used, resets_at) in shorts {
                let name = if minutes == 300 {
                    has_5h = true;
                    "5h".to_string()
                } else {
                    format!("{minutes}m")
                };
                windows.push(build_window(
                    &name,
                    used,
                    resets_at,
                    source_label,
                    stale,
                    age_seconds,
                    now,
                    warn_pct,
                ));
            }
            if !has_5h {
                unreported.insert(
                    "5h".into(),
                    json!("Codex session log has no 5-hour rate limit reading"),
                );
            }
            if let Some(used) = metrics::percent(&ps["weekly_used"]) {
                let resets_at = metrics::stamp(&ps["weekly_resets_at"]).filter(|r| *r > now);
                windows.push(build_window(
                    "weekly",
                    used,
                    resets_at,
                    source_label,
                    stale,
                    age_seconds,
                    now,
                    warn_pct,
                ));
            } else {
                unreported.insert(
                    "weekly".into(),
                    json!("Codex session log has no weekly rate limit reading"),
                );
            }
        }
        "antigravity" if native_weekly.is_some() => {
            if let Some(used) = metrics::percent(&ps["weekly_used"]) {
                windows.push(build_window(
                    "weekly",
                    used,
                    None,
                    source_label,
                    stale,
                    age_seconds,
                    now,
                    warn_pct,
                ));
            }
            unreported.insert("5h".into(), json!("not sampled"));
        }
        _ => {
            let label = if provider == "antigravity" {
                "agy"
            } else {
                provider
            };
            let msg = format!("{label} reports no quota window locally");
            unreported.insert("5h".into(), json!(msg));
            unreported.insert("weekly".into(), json!(msg));
        }
    }

    let tightest = windows
        .iter()
        .max_by_key(|w| (w["used"].as_u64().unwrap_or(0), w["name"] == "5h"))
        .and_then(|w| w["name"].as_str())
        .map(Value::from)
        .unwrap_or(Value::Null);

    let context_used = metrics::percent(&ps["context_left"])
        .map(|left| Value::from(100u8.saturating_sub(left)))
        .unwrap_or_else(|| json!({"unreported": "no context reading recorded locally"}));

    let cache_read_share = metrics::percent(&ps["cache_read_share"])
        .map(|pct| json!({"percent": pct}))
        .unwrap_or_else(|| json!({"unreported": "no cache read share recorded"}));

    let account_provider = oms_panel_core::routing::ACCOUNT_PROVIDERS.contains(&provider);
    let account = if account_provider
        && let Some(label) = ps["account_label"].as_str().filter(|s| !s.is_empty())
    {
        let mut obj = Map::new();
        obj.insert("label".into(), json!(label));
        if ps["accounts"].is_object() {
            obj.insert("accounts".into(), ps["accounts"].clone());
        }
        Value::Object(obj)
    } else if account_provider && ps["accounts"].is_object() {
        json!({"accounts": ps["accounts"]})
    } else if account_provider
        && let Some(err) = ps["accounts_error"].as_str().filter(|s| !s.is_empty())
    {
        json!({"unreported": format!("{provider} account registry invalid: {err}")})
    } else {
        json!({"unreported": format!("{provider} exposes no account label locally")})
    };

    let recent_7d = if provider == "antigravity" && ps["calls"].as_u64().is_some_and(|c| c > 0) {
        json!({
            "calls": ps["calls"],
            "input_tokens": ps["input_tokens"],
            "cache_read_tokens": ps["cache_read_tokens"],
            "output_tokens": ps["output_tokens"],
        })
    } else {
        json!({"unreported": format!("no 7d {provider} token aggregate recorded locally")})
    };

    json!({
        "windows": windows,
        "windows_unreported": Value::Object(unreported),
        "tightest": tightest,
        "reading": {
            "stale": stale,
            "age_seconds": age_seconds,
            "observed_at": observed_at,
            "source": source_label,
        },
        "context_used": context_used,
        "cache_read_share": cache_read_share,
        "account": account,
        "seats": {
            "live": seats.0 + seats.1,
            "main": seats.0,
            "workers": seats.1,
        },
        "tokens": {
            "today": {"unreported": "no per-provider daily token total exists locally"},
            "recent_7d": recent_7d,
        },
    })
}

/// Build the schema 1 usage snapshot from a collected panel board report.
pub fn snapshot(report: &Value, now: f64) -> Value {
    let seats = seats_by_provider(report);
    let warn_pct = warn_threshold();
    let mut providers = Map::new();
    let include_pi = report["provider_status"]["pi"].is_object() || seats.contains_key("pi");
    for provider in PROVIDERS.iter().copied().chain(include_pi.then_some("pi")) {
        let count = seats.get(provider).copied().unwrap_or((0, 0));
        providers.insert(
            provider.into(),
            provider_snapshot(
                provider,
                &report["provider_status"][provider],
                count,
                now,
                warn_pct,
            ),
        );
    }
    json!({
        "schema": 1,
        "now": now,
        "fresh_seconds": metrics::READING_FRESH_SECONDS as u64,
        "held": held_items(report),
        "projection": {
            "unreported": "two or fewer windows with reset and pace; no extrapolation"
        },
        "providers": Value::Object(providers),
    })
}

/// The explicit usage command may inspect private account receipts. Board snapshots remain pure.
pub(crate) fn explicit_snapshot(
    report: &Value,
    now: f64,
    host: &dyn crate::orchestrator::account_binding::AccountHost,
) -> Value {
    let mut snap = snapshot(report, now);
    let observations: Map<_, _> =
        crate::orchestrator::account_binding::inspect_claude_accounts(host, now)
            .into_iter()
            .map(|(alias, reading)| {
                (
                    alias,
                    json!({
                        "used_percentage": reading.used_percentage,
                        "callback_age_seconds": reading.callback_age_seconds,
                        "resets_at": reading.resets_at,
                        "source": "local Claude statusline receipt",
                        "acquired_at": Value::Null,
                        "ranking_eligible": false,
                    }),
                )
            })
            .collect();
    if !observations.is_empty() {
        snap["providers"]["claude"]["account"]["local_weekly_observations"] =
            Value::Object(observations);
    }
    snap
}

fn provider_title(name: &str) -> &str {
    match name {
        "claude" => "Claude",
        "codex" => "Codex",
        "antigravity" => "Antigravity",
        "pi" => "Pi",
        _ => name,
    }
}

/// Format a usage snapshot into plain human-readable lines for `oms panel --usage`.
pub fn text(snap: &Value) -> String {
    let fresh_mins = snap["fresh_seconds"].as_u64().unwrap_or(600) / 60;
    let mut lines = vec![format!(
        "Provider usage (fresh <= {fresh_mins}m; local readings only):"
    )];
    let include_pi = snap["providers"]["pi"].is_object();
    for name in PROVIDERS.iter().copied().chain(include_pi.then_some("pi")) {
        let p = &snap["providers"][name];
        let seats = &p["seats"];
        let mut header = format!(
            "{} · {} live ({} main, {} worker)",
            provider_title(name),
            seats["live"].as_u64().unwrap_or(0),
            seats["main"].as_u64().unwrap_or(0),
            seats["workers"].as_u64().unwrap_or(0)
        );
        if let Some(label) = p["account"]["label"].as_str().filter(|s| !s.is_empty()) {
            header.push_str(&format!(" · account {label}"));
        } else if let Some(accounts) = p["account"]["accounts"]
            .as_object()
            .filter(|a| !a.is_empty())
        {
            let names: Vec<&str> = accounts.keys().map(String::as_str).collect();
            header.push_str(&format!(" · accounts {}", names.join(", ")));
        }
        lines.push(header);
        if name == "claude"
            && let Some(observations) = p["account"]["local_weekly_observations"].as_object()
        {
            for (alias, reading) in observations {
                let (Some(used), Some(age), Some(reset), Some(now)) = (
                    reading["used_percentage"].as_f64(),
                    reading["callback_age_seconds"].as_f64(),
                    reading["resets_at"].as_f64(),
                    snap["now"].as_f64(),
                ) else {
                    continue;
                };
                lines.push(format!("  {alias} · local weekly observation · {used}% used · callback age {} · resets in {} · ranking unavailable",
                    format_age(age), format_duration(reset - now)));
            }
        }
        for w in array(&p["windows"]) {
            let wname = w["name"].as_str().unwrap_or("window");
            let used = w["used"].as_u64().unwrap_or(0);
            let mut parts = vec![format!("{used}% used")];
            if let Some(resets_in) = w["resets_in"].as_str() {
                parts.push(format!("resets in {resets_in}"));
            }
            if let Some(age) = w["age_seconds"].as_f64() {
                parts.push(format!("age {}", format_age(age)));
            }
            if let Some(src) = w["source"].as_str() {
                parts.push(format!("source {src}"));
            }
            if w["stale"].as_bool().unwrap_or(false) {
                parts.push("stale".into());
            }
            lines.push(format!("  {wname:<6}  {}", parts.join("; ")));

            let pval = &w["pace"];
            if let Some(reason) = pval["unreported"].as_str() {
                lines.push(format!("          pace: unreported ({reason})"));
            } else if let (Some(elapsed), Some(verdict)) =
                (pval["elapsed_pct"].as_i64(), pval["verdict"].as_str())
            {
                let phrase = match verdict {
                    "ahead" => "ahead of pace",
                    "behind" => "behind pace",
                    _ => "even with pace",
                };
                lines.push(format!(
                    "          pace: {used}% used · {elapsed}% of window elapsed · {phrase}"
                ));
            }
        }
        if let Some(unrep) = p["windows_unreported"].as_object() {
            for (k, v) in unrep {
                let reason = v.as_str().unwrap_or("unreported");
                lines.push(format!("  {k:<6}  unreported ({reason})"));
            }
        }
        if let Some(used) = p["context_used"].as_u64() {
            lines.push(format!("  context: {used}% used"));
        }
        if let Some(pct) = p["cache_read_share"]["percent"].as_u64() {
            lines.push(format!("  cache read share: {pct}%"));
        }
        if let Some(calls) = p["tokens"]["recent_7d"]["calls"].as_u64() {
            let inp = p["tokens"]["recent_7d"]["input_tokens"]
                .as_u64()
                .unwrap_or(0);
            let cache = p["tokens"]["recent_7d"]["cache_read_tokens"]
                .as_u64()
                .unwrap_or(0);
            let out = p["tokens"]["recent_7d"]["output_tokens"]
                .as_u64()
                .unwrap_or(0);
            lines.push(format!(
                "  recent 7d: {calls} calls ({inp} input, {cache} cache read, {out} output)"
            ));
        }
    }
    if let Some(reason) = snap["projection"]["unreported"].as_str() {
        lines.push(format!("exhaustion projection: unreported ({reason})"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::provider_snapshot;
    use serde_json::json;

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_claude_receipts_follow_current_alias_and_keep_ranking_separate() {
        use super::{explicit_snapshot, snapshot, text};
        use crate::claude_lead_usage::claude_observation_identity;
        use crate::orchestrator::account_binding::test_support::{
            FakeHost, TempDir, private_dir, sample_accounts,
        };
        use std::{fs, os::unix::fs::PermissionsExt, path::Path};

        const SLOT: &str = ".oms-claude-weekly-receipt.json";
        const NOW: f64 = 1_900_000_000.0;
        let tmp = TempDir::new("explicit-claude-usage");
        let backup = private_dir(&tmp.0.join("first-private"));
        let primary = private_dir(&tmp.0.join("second-private"));
        let idle = private_dir(&tmp.0.join("idle-private"));
        let mut host = FakeHost::new(tmp.0.join("state"));
        let registry = |a: &Path, b: &Path| {
            Some(sample_accounts(vec![(
                "claude",
                "manual",
                vec![
                    ("backup", a.into()),
                    ("primary", b.into()),
                    ("idle", idle.clone()),
                ],
            )]))
        };
        host.accounts = registry(&backup, &primary);
        let make_receipt = |alias: &str, home: &Path, used: f64| {
            let identity = claude_observation_identity(home, host.uid)?;
            let mut scope = format!("claude-weekly-account\0{alias}\0").into_bytes();
            scope.extend(home.as_os_str().as_encoded_bytes());
            Some(json!({
                "observation":{"schema":1,"home":identity,"process":"a".repeat(64),"session":"b".repeat(64),
                    "weekly_used":used,"callback_observed_at":NOW-65.0,"resets_at":NOW+1800.0},
                "evidence":"local_statusline","acquired_at":null,"ranking_eligible":false,
                "account_scope": oms_panel_core::digest(&scope),
            }))
        };
        let Some(pinned) = make_receipt("backup", &backup, 14.0) else {
            return;
        };
        let other = make_receipt("primary", &primary, 33.0).unwrap();
        let write = |home: &Path, row: &serde_json::Value| {
            fs::write(home.join(SLOT), serde_json::to_vec(row).unwrap()).unwrap();
            fs::set_permissions(home.join(SLOT), fs::Permissions::from_mode(0o600)).unwrap();
        };
        let report = json!({"provider_status":{"claude":{"source":"HUD cache","weekly_used":70,
            "account_label":"backup","accounts":{"backup":{"weekly_used":null},"primary":{"weekly_used":null},"idle":{"weekly_used":null}}}}});
        let old = snapshot(&report, NOW);
        let inspect = |host: &FakeHost| explicit_snapshot(&report, NOW, host);
        let measurements = |snap: &serde_json::Value| {
            snap["providers"]["claude"]["account"]["local_weekly_observations"].clone()
        };
        assert_eq!(
            inspect(&host),
            old,
            "absent receipts keep prior usage shape"
        );

        write(&backup, &pinned);
        write(&primary, &other);
        write(&idle, &pinned);
        let observed = inspect(&host);
        let projected = measurements(&observed);
        assert_eq!(
            projected["backup"],
            json!({"used_percentage":14.0,"callback_age_seconds":65.0,
            "resets_at":NOW+1800.0,"source":"local Claude statusline receipt","acquired_at":null,"ranking_eligible":false})
        );
        assert_eq!(projected["primary"]["used_percentage"], 33.0);
        assert!(projected.get("idle").is_none());
        assert_eq!(
            observed["providers"]["claude"]["account"]["accounts"],
            old["providers"]["claude"]["account"]["accounts"]
        );
        assert_eq!(
            observed["providers"]["claude"]["windows"],
            old["providers"]["claude"]["windows"]
        );
        let plain = text(&observed);
        assert!(plain.contains("backup · local weekly observation · 14% used · callback age 1m · resets in 30m · ranking unavailable"));
        for private in [
            backup.to_str().unwrap(),
            pinned["observation"]["home"].as_str().unwrap(),
            pinned["observation"]["process"].as_str().unwrap(),
            pinned["observation"]["session"].as_str().unwrap(),
            pinned["account_scope"].as_str().unwrap(),
        ] {
            assert!(!observed.to_string().contains(private) && !plain.contains(private));
        }

        // A remap cannot relabel another already valid home's receipt.
        host.accounts = registry(&primary, &backup);
        assert!(measurements(&inspect(&host)).is_null());
        host.accounts = registry(&backup, &primary);
        for (key, value) in [
            ("callback_observed_at", json!(NOW - 601.0)),
            ("callback_observed_at", json!(NOW + 1.0)),
            ("resets_at", json!(NOW)),
            ("resets_at", json!(NOW + 8.0 * 86400.0)),
            ("weekly_used", json!(101)),
            ("process", json!("not-a-digest")),
            ("session", json!("not-a-digest")),
        ] {
            let mut malformed = pinned.clone();
            malformed["observation"][key] = value;
            write(&backup, &malformed);
            assert!(
                measurements(&inspect(&host)).get("backup").is_none(),
                "rejected {key}"
            );
        }
        for removed in [
            "account_scope",
            "acquired_at",
            "ranking_eligible",
            "evidence",
        ] {
            let mut malformed = pinned.clone();
            malformed.as_object_mut().unwrap().remove(removed);
            write(&backup, &malformed);
            assert!(measurements(&inspect(&host)).get("backup").is_none());
        }
        for (key, value) in [
            ("ranking_eligible", json!(true)),
            ("acquired_at", json!(NOW - 66.0)),
            ("evidence", json!("legacy HUD")),
            ("evidence", json!("x".repeat(1100))),
        ] {
            let mut malformed = pinned.clone();
            malformed[key] = value;
            write(&backup, &malformed);
            assert!(measurements(&inspect(&host)).get("backup").is_none());
        }
        write(&backup, &pinned);
        fs::set_permissions(backup.join(SLOT), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(measurements(&inspect(&host)).get("backup").is_none());
        write(&backup, &pinned);
        let moved = tmp.0.join("moved-private");
        fs::rename(&backup, &moved).unwrap();
        assert_eq!(
            claude_observation_identity(&moved, host.uid).as_deref(),
            pinned["observation"]["home"].as_str()
        );
        host.accounts = registry(&moved, &primary);
        assert!(measurements(&inspect(&host)).get("backup").is_none());
        private_dir(&backup);
        write(&backup, &pinned);
        host.accounts = registry(&backup, &primary);
        assert!(measurements(&inspect(&host)).get("backup").is_none());
    }

    #[test]
    fn lead_hud_weekly_is_labelled_without_new_keys() {
        let now = 10_000.0;
        let keys = |snap: &serde_json::Value| {
            let mut keys: Vec<_> = snap.as_object().unwrap().keys().cloned().collect();
            keys.push(format!(
                "{:?}",
                snap["reading"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .collect::<Vec<_>>()
            ));
            keys
        };
        let native = provider_snapshot(
            "claude",
            &json!({"source": "unavailable", "weekly_used": 40, "weekly_native_observed_at": now - 30.0}),
            (0, 0),
            now,
            None,
        );
        let lead = |at: f64| {
            provider_snapshot(
                "claude",
                &json!({"source": "cache stale", "observed_at": now - 900.0, "weekly_used": 21,
                        "weekly_lead_observed_at": at, "weekly_resets_at": now + 3600.0}),
                (0, 0),
                now,
                None,
            )
        };
        let fresh = lead(now - 30.0);
        assert_eq!(keys(&fresh), keys(&native));
        assert_eq!(fresh["reading"]["source"], "Claude lead HUD");
        assert_eq!(fresh["reading"]["stale"], false);
        assert_eq!(fresh["windows"][0]["name"], "weekly");
        assert_eq!(fresh["windows"][0]["used"], 21);
        assert_eq!(fresh["windows"][0]["source"], "Claude lead HUD");
        let aged = lead(now - 700.0);
        assert_eq!(aged["reading"]["stale"], true);
        assert_eq!(aged["windows"][0]["level"], serde_json::Value::Null);
        let stale = provider_snapshot(
            "claude",
            &json!({"source": "cache stale", "observed_at": now - 700.0}),
            (0, 0),
            now,
            None,
        );
        assert_eq!(stale["windows"], json!([]));
        assert_eq!(stale["reading"]["stale"], true);
    }
}
