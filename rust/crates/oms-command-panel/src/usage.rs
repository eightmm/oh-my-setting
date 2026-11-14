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

fn format_duration(seconds: f64) -> String {
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

fn format_age(seconds: f64) -> String {
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
            let state = vtext(&attempt, "state");
            if let Some(id) = attempt["attempt_id"].as_str().filter(|s| !s.is_empty()) {
                attempt_states.insert(id.to_owned(), state.to_owned());
            }
            if let Some(part) = attempt["panel"]["room_participant"]
                .as_str()
                .filter(|s| !s.is_empty())
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
        let role = vtext(&member, "role");
        let participant = vtext(&member, "participant");
        let provider = vtext(&member, "provider");
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
        let state = vtext(&worker, "state");
        let reason = vtext(&worker, "reason");
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
    let observed_at = metrics::stamp(&ps["observed_at"]);
    let age_seconds = observed_at.map(|ts| (now - ts).max(0.0));
    let raw_source = ps["source"].as_str().unwrap_or("unavailable");
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
        _ => {
            unreported.insert("5h".into(), json!("agy reports no quota window locally"));
            unreported.insert(
                "weekly".into(),
                json!("agy reports no quota window locally"),
            );
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

    let account = if provider == "codex"
        && let Some(label) = ps["account_label"].as_str().filter(|s| !s.is_empty())
    {
        let mut obj = Map::new();
        obj.insert("label".into(), json!(label));
        if ps["accounts"].is_object() {
            obj.insert("accounts".into(), ps["accounts"].clone());
        }
        Value::Object(obj)
    } else if provider == "codex" && ps["accounts"].is_object() {
        json!({"accounts": ps["accounts"]})
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
    for provider in PROVIDERS {
        let count = seats.get(*provider).copied().unwrap_or((0, 0));
        providers.insert(
            (*provider).into(),
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

fn provider_title(name: &str) -> &str {
    match name {
        "claude" => "Claude",
        "codex" => "Codex",
        "antigravity" => "Antigravity",
        _ => name,
    }
}

/// Format a usage snapshot into plain human-readable lines for `oms panel --usage`.
pub fn text(snap: &Value) -> String {
    let fresh_mins = snap["fresh_seconds"].as_u64().unwrap_or(600) / 60;
    let mut lines = vec![format!(
        "Provider usage (fresh <= {fresh_mins}m; local readings only):"
    )];
    for name in PROVIDERS {
        let p = &snap["providers"][name];
        let seats = &p["seats"];
        lines.push(format!(
            "{} · {} live ({} main, {} worker)",
            provider_title(name),
            seats["live"].as_u64().unwrap_or(0),
            seats["main"].as_u64().unwrap_or(0),
            seats["workers"].as_u64().unwrap_or(0)
        ));
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
