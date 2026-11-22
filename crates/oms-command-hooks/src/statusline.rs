use crate::{Output, common::*};
use serde_json::{Value, json};
use std::{ffi::OsString, path::Path};

fn number(v: &Value, min: f64, max: f64) -> Option<f64> {
    v.as_f64()
        .filter(|n| n.is_finite())
        .map(|n| n.clamp(min, max))
}
fn pct(v: &Value) -> Option<u32> {
    number(v, 0., 100.).map(|n| (n + 0.5) as u32)
}
fn count(v: &Value) -> Option<String> {
    let n = number(v, 0., 1e12)?;
    Some(if n >= 1e9 {
        format!("{}b", n / 1e9)
    } else if n >= 1e6 {
        format!("{}m", n / 1e6)
    } else if n >= 1e3 {
        format!("{}k", (n / 1e3 + 0.5) as u64)
    } else {
        format!("{}", (n + 0.5) as u64)
    })
}
fn clean(v: &Value, fallback: &str, limit: usize, ellipsis: bool) -> String {
    let Some(s) = v.as_str() else {
        return fallback.into();
    };
    let stripped = cached_regex!("\x1b\\[[0-?]*[ -/]*[@-~]").replace_all(s, "");
    let s = flat(&stripped);
    let chars = s.chars().count();
    let s = if chars > limit && ellipsis {
        format!(
            "{}…",
            s.chars()
                .take(limit.saturating_sub(1))
                .collect::<String>()
                .trim_end()
        )
    } else {
        s.chars().take(limit).collect()
    };
    if s.is_empty() { fallback.into() } else { s }
}
fn duration(s: f64, sub: bool) -> String {
    let s = s.max(0.) as u64;
    if s < 60 {
        return if sub { format!("{s}s") } else { "<1m".into() };
    }
    let m = s / 60;
    if m < 60 {
        return if sub {
            format!("{m}m{:02}s", s % 60)
        } else {
            format!("{m}m")
        };
    }
    let h = m / 60;
    if h < 48 {
        format!("{h}h{:02}m", m % 60)
    } else {
        format!("{}d{}h", h / 24, h % 24)
    }
}
fn clock() -> f64 {
    env("OMS_HUD_NOW")
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .unwrap_or_else(seconds)
}
fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.into()
    } else if width <= 1 {
        "…".chars().take(width).collect()
    } else {
        format!(
            "{}…",
            s.chars().take(width - 1).collect::<String>().trim_end()
        )
    }
}
fn color(s: String, p: u32, on: bool) -> String {
    if on {
        format!(
            "\x1b[{}m{s}\x1b[0m",
            if p >= 80 {
                31
            } else if p >= 60 {
                33
            } else {
                32
            }
        )
    } else {
        s
    }
}
fn subagent(v: &Value) -> Vec<u8> {
    let width = number(&v["columns"], 20., 500.).unwrap_or(100.) as usize;
    let now = clock();
    let mut result = Vec::new();
    let date_suffix_re = cached_regex!(r"-\d{8}$");
    for task in v["tasks"].as_array().into_iter().flatten().take(128) {
        let Some(id) = task["id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.chars().count() <= 256 && !s.chars().any(control))
        else {
            continue;
        };
        let mut label = clean(&task["label"], "", 24, true);
        if label.is_empty() {
            label = clean(&task["name"], "", 24, true);
        }
        if label.is_empty() {
            label = clean(&task["type"], "agent", 24, true);
        }
        let mut parts = vec![label];
        let mut model = clean(&task["model"], "", 40, true).to_lowercase();
        if let Some(s) = model.strip_prefix("claude-") {
            model = s.into();
        }
        model = date_suffix_re.replace(&model, "").into_owned();
        model = clean(&json!(model), "", 20, true);
        let effort = if ["low", "medium", "high", "xhigh", "max"].contains(&text(task, "effort")) {
            text(task, "effort").into()
        } else {
            count(&task["effort"])
                .map(|s| format!("{s} effort"))
                .unwrap_or_default()
        };
        let models = [model, effort]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        if !models.is_empty() {
            parts.push(models);
        }
        if let Some(used) = number(&task[concat!("to", "kenCount")], 0., 1e12) {
            let mut s = format!("ctx {}", count(&json!(used)).unwrap());
            if let Some(capacity) = number(&task["contextWindowSize"], 1., 1e12) {
                s.push_str(&format!(
                    "/{} {}%",
                    count(&json!(capacity)).unwrap(),
                    ((used * 100. / capacity + 0.5) as u64).min(100)
                ));
            }
            parts.push(s);
        }
        let started = number(&task["startTime"], 0., 1e13)
            .map(|n| if n > 1e10 { n / 1000. } else { n })
            .or_else(|| {
                task["startTime"]
                    .as_str()
                    .filter(|s| s.len() <= 80)
                    .and_then(epoch)
            });
        if let Some(s) = started
            && (0. ..=365. * 86400.).contains(&(now - s))
        {
            parts.push(duration(now - s, true));
        }
        let state = clean(&task["status"], "", 16, true)
            .to_lowercase()
            .replace('_', "-");
        let state = match state.as_str() {
            "completed" | "complete" => "done",
            "errored" | "failed" => "error",
            "interrupted" | "cancelled" | "canceled" => "stopped",
            s => s,
        };
        if !state.is_empty() {
            parts.push(state.into());
        }
        result.extend(json_line(
            &json!({"id":id,"content":truncate(&parts.join(" | "),width)}),
        ));
    }
    result
}
fn git_status(v: &Value, cwd: &str, ctx: &Context<'_>) -> Value {
    if cwd.trim().is_empty() {
        return json!({});
    }
    let cwd = absolute(ctx.cwd, std::path::Path::new(cwd));
    let physical = cwd.canonicalize().unwrap_or(cwd.clone());
    let id = hash(format!("{}\0{}", session(v), physical.to_string_lossy()));
    let cache = hud_dir().join(format!("git-{}.json", &id[..24]));
    if cache
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|a| a.as_secs_f64() <= 5.)
    {
        return load(&cache);
    }
    let mut result = json!({});
    if let Some(status) = git(
        &cwd,
        &[
            "status",
            "--porcelain=v1",
            "--branch",
            "--untracked-files=normal",
        ],
        ctx,
    ) {
        let mut lines = status.lines();
        if let Some(head) = lines.next().and_then(|s| s.strip_prefix("## ")) {
            let head = head
                .trim()
                .strip_prefix("No commits yet on ")
                .or_else(|| head.trim().strip_prefix("Initial commit on "))
                .unwrap_or(head.trim());
            let branch = if head == "HEAD" || head.starts_with("HEAD ") {
                "detached"
            } else {
                head.split("...")
                    .next()
                    .unwrap_or("")
                    .split(" [")
                    .next()
                    .unwrap_or("")
            };
            let branch = clean(&json!(branch), "", 24, false);
            if !branch.is_empty() {
                let (mut staged, mut modified, mut untracked) = (0, 0, 0);
                for line in lines {
                    let b = line.as_bytes();
                    if b.starts_with(b"??") {
                        untracked += 1;
                        continue;
                    }
                    if b.len() < 2 {
                        continue;
                    }
                    if !b" ?!".contains(&b[0]) {
                        staged += 1;
                    }
                    if !b" ?!".contains(&b[1]) {
                        modified += 1;
                    }
                }
                result = json!({"branch":branch,"staged":staged,"modified":modified,"untracked":untracked});
            }
        }
    }
    let _ = write(&cache, &result);
    result
}
fn context_cache(v: &Value) {
    if session(v).is_empty() {
        return;
    }
    let c = &v["context_window"];
    let Some(used) = pct(&c["used_percentage"]) else {
        return;
    };
    let mut row = json!({
        "schema":1,"used_percentage":used,"context_window_size":number(&c["context_window_size"],0.,1e12),
        "total_input_tokens":number(&c[concat!("total_input_to", "kens")],0.,1e12),"ts":seconds()
    });
    for key in ["five_hour", "seven_day"] {
        let w = &v["rate_limits"][key];
        if let Some(p) = pct(&w["used_percentage"]) {
            row[key] = json!({"used_percentage":p,"resets_at":number(&w["resets_at"],0.,4e9)});
        }
    }
    let usage = &c["current_usage"];
    let values: Vec<_> = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .filter_map(|k| number(&usage[*k], 0., 1e12))
    .collect();
    let total = values.iter().sum::<f64>();
    if values.len() == 3 && total > 0. {
        row["cache_read_share"] = json!((100. * values[2] / total).round_ties_even());
    }
    let _ = write(
        &hud_dir().join(format!("ctx-{}.json", &hash(session(v))[..24])),
        &row,
    );
}
fn main_line(v: &Value, ctx: &Context<'_>) -> String {
    let on = std::env::var_os("NO_COLOR").is_none() && env("TERM") != "dumb";
    let mut identity = Vec::new();
    let mut metrics = Vec::new();
    let model = &v["model"];
    identity.push(clean(
        &model["display_name"],
        &clean(
            &model["id"],
            model
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("Claude"),
            24,
            false,
        ),
        24,
        false,
    ));
    let session = clean(&v["session_name"], "", 16, false);
    if !session.is_empty() {
        identity.push(session);
    }
    let cwd = text(&v["workspace"], "current_dir");
    if !cwd.trim().is_empty() {
        let name = cwd
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("");
        let name = clean(&json!(name), "", 16, false);
        if !name.is_empty() {
            identity.push(name);
        }
    }
    if v["fast_mode"] == true {
        identity.push("fast".into());
    }
    let state = git_status(v, cwd, ctx);
    let branch = clean(&state["branch"], "", 24, false);
    if !branch.is_empty() {
        let mut s = format!("git {branch}");
        for (k, prefix) in [("staged", "+"), ("modified", "~"), ("untracked", "?")] {
            if let Some(n) = number(&state[k], 0., 9999.)
                && n > 0.
            {
                s.push_str(&format!(" {prefix}{}", n as u64));
            }
        }
        identity.push(s);
    }
    let c = &v["context_window"];
    if let Some(used) = pct(&c["used_percentage"]) {
        let filled = (used / 10) as usize;
        let mut s = format!(
            "ctx [{}{}] {used}%",
            "#".repeat(filled),
            "-".repeat(10 - filled)
        );
        let input = number(&c[concat!("total_input_to", "kens")], 0., 1e12).or_else(|| {
            let usage = &c["current_usage"];
            let values: Vec<_> = [
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
            ]
            .iter()
            .filter_map(|k| number(&usage[*k], 0., 1e12))
            .collect();
            (!values.is_empty()).then(|| values.iter().sum::<f64>())
        });
        if let (Some(input), Some(capacity)) = (
            input.and_then(|n| count(&json!(n))),
            count(&c["context_window_size"]),
        ) {
            s.push_str(&format!(" {input}/{capacity}"));
        }
        metrics.push(color(s, used, on));
    } else {
        metrics.push("ctx --".into());
    }
    for (key, label) in [("five_hour", "5h"), ("seven_day", "7d")] {
        let w = &v["rate_limits"][key];
        if let Some(p) = pct(&w["used_percentage"]) {
            let mut s = format!("{label} {p}%");
            if let Some(reset) = number(&w["resets_at"], 0., 4e9) {
                let left = reset - clock();
                if left > 0. && left <= 8. * 86400. {
                    s.push_str(&format!(" ({})", duration(left, false)));
                }
            }
            metrics.push(color(s, p, on));
        }
    }
    if !cwd.trim().is_empty() {
        let live = oms_live_strip(root(Path::new(cwd), ctx.cwd).as_deref());
        if !live.is_empty() {
            metrics.push(live);
        }
    }
    if let Some(cost) = number(&v["cost"]["total_cost_usd"], 0., 1e12) {
        identity.push(format!("${cost:.2}"));
    }
    let effort = clean(&v["effort"]["level"], "", 8, false);
    let mut effort = if ["low", "medium", "high", "xhigh", "max"].contains(&effort.as_str()) {
        effort
    } else {
        String::new()
    };
    if v["thinking"]["enabled"] == true {
        if !effort.is_empty() {
            effort.push('+');
        }
        effort.push_str("think");
    }
    if !effort.is_empty() {
        identity.push(effort);
    }
    let width = env("COLUMNS")
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .unwrap_or(80);
    let single = identity
        .iter()
        .chain(&metrics)
        .cloned()
        .collect::<Vec<_>>()
        .join(" | ");
    let plain = cached_regex!("\x1b\\[[0-9;]*m").replace_all(&single, "");
    if plain.chars().count() <= width {
        single
    } else {
        format!(
            "{}\n{}",
            truncate(&identity.join(" | "), width),
            metrics.join(" | ")
        )
    }
}
/// Lightweight local OMS state, read without spawning processes. Total I/O is
/// capped at `LIVE_BUDGET`; absent or unreadable state contributes nothing.
const LIVE_BUDGET: usize = 2 * 1024 * 1024;
#[derive(Default)]
struct Live {
    tasks: usize,
    title: String,
    failures: usize,
    agents: usize,
}
fn spend(path: &Path, cap: usize, budget: &mut usize) -> Option<Vec<u8>> {
    let raw = read(path, cap.min(*budget)).ok()?;
    *budget -= raw.len();
    Some(raw)
}
fn terminal(row: &Value) -> bool {
    row["terminal"] == true
        || [
            "resolved",
            "closed",
            "superseded",
            "done",
            "completed",
            "failed",
            "cancelled",
            "canceled",
            "stopped",
            "error",
        ]
        .contains(&first(row, &["status", "state"]).to_lowercase().as_str())
}
fn live(repo: &Path) -> Live {
    let mut budget = LIVE_BUDGET;
    let mut live = Live::default();
    let state = repo.join(".oms");
    if let Some(plan) = spend(&state.join("plan/tasks.json"), 1 << 20, &mut budget)
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
    {
        let rows: Vec<&Value> = match &plan["tasks"] {
            Value::Object(m) => m.values().collect(),
            Value::Array(a) => a.iter().collect(),
            _ => vec![],
        };
        for row in rows.into_iter().take(4096) {
            if ["running", "claimed", "in_progress", "active"]
                .contains(&text(row, "state").to_lowercase().as_str())
            {
                if live.tasks == 0 {
                    live.title = clean(&json!(first(row, &["title", "goal", "id"])), "", 32, true);
                }
                live.tasks += 1;
            }
        }
    }
    // Mirrors the ledger fold: a fail adds to its fingerprint, `resolved` clears
    // it, and an expired hook failure counts for nothing.
    if let Some(raw) = spend(&state.join("failures.jsonl"), 1 << 20, &mut budget) {
        let ttl = cap("OMS_HOOK_FAIL_TTL", 86400, 0, 365 * 86400) as f64;
        let mut open = std::collections::BTreeMap::<String, (usize, bool)>::new();
        for row in String::from_utf8_lossy(&raw)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        {
            let fp = text(&row, "fingerprint");
            if fp.is_empty() {
                continue;
            }
            match text(&row, "event") {
                "resolved" => {
                    open.remove(fp);
                }
                "fail" => {
                    let hook = text(&row, "kind") == "hook";
                    if hook && epoch(text(&row, "ts")).is_some_and(|t| seconds() - t >= ttl) {
                        continue;
                    }
                    let entry = open.entry(fp.to_owned()).or_default();
                    entry.0 += 1;
                    entry.1 = hook;
                }
                _ => {}
            }
        }
        live.failures = open.values().filter(|(n, hook)| !(*hook && *n < 2)).count();
    }
    // A marker with a pid is live while that process exists (as the state GC
    // decides); one without a pid is live until it records a terminal state.
    if let Ok(dir) = std::fs::read_dir(state.join("delegations")) {
        for entry in dir.flatten().take(256) {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Some(row) = spend(&path, 64 * 1024, &mut budget)
                    .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
                && row.is_object()
                && !terminal(&row)
            {
                let pid = row["pid"]
                    .as_i64()
                    .or_else(|| row["pid"].as_str().and_then(|s| s.parse().ok()))
                    .and_then(|n| i32::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .and_then(rustix::process::Pid::from_raw);
                if pid.is_none_or(|p| {
                    rustix::process::test_kill_process(p) != Err(rustix::io::Errno::SRCH)
                }) {
                    live.agents += 1;
                }
            }
        }
    }
    live
}
fn strip(l: &Live) -> String {
    let mut parts = Vec::new();
    if l.tasks > 0 {
        parts.push(format!("T{} {}", l.tasks, l.title).trim_end().to_owned());
    }
    if l.failures > 0 {
        parts.push(format!("F{}", l.failures));
    }
    if l.agents > 0 {
        parts.push(format!("A{}", l.agents));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("oms {}", parts.join(" | "))
    }
}
/// Empty when `repo` has no OMS state or nothing is live.
pub fn oms_live_strip(repo: Option<&Path>) -> String {
    repo.filter(|r| adopted(r))
        .map(|r| strip(&live(r)))
        .unwrap_or_default()
}
fn branch_label(repo: Option<&Path>) -> String {
    match repo.and_then(head_ref) {
        Some(HeadRef::Branch(b)) => clean(&json!(b), "", 24, true),
        Some(HeadRef::Detached(h)) => clean(&json!(h), "", 24, true),
        None => String::new(),
    }
}
fn agy_weekly_remaining(v: &Value, ctx: &Context<'_>) -> Option<String> {
    let bucket = v.get("quota")?.get("gemini-weekly")?;
    if !matches!(bucket.get("disabled"), None | Some(Value::Bool(false))) {
        return None;
    }
    let fraction = bucket["remaining_fraction"].as_f64()?;
    if !(0.0..=1.0).contains(&fraction) {
        return None;
    }
    let reset = bucket["reset_time"].as_str().filter(|stamp| {
        stamp.len() <= 64 && matches!(stamp.as_bytes().get(10), Some(b'T' | b't'))
    })?;
    let reset = chrono::DateTime::parse_from_rfc3339(reset).ok()?;
    let now = chrono::DateTime::<chrono::Utc>::from(ctx.snapshot.time());
    let until = reset.signed_duration_since(now);
    if until <= chrono::TimeDelta::zero() || until > chrono::TimeDelta::days(7) {
        return None;
    }
    Some(match fraction {
        0.0 => "0%".into(),
        1.0 => "100%".into(),
        n if n < 0.01 => "<1%".into(),
        n if n > 0.99 => ">99%".into(),
        n => format!("{}%", (100.0 * n + 0.5) as u32),
    })
}

fn agy(v: &Value, unambiguous: bool, ctx: &Context<'_>) -> String {
    let mut parts = Vec::new();
    let model = &v["model"];
    let model = if model.is_object() {
        &model["display_name"]
    } else if model.is_null() {
        &v["modelName"]
    } else {
        model
    };
    let mut label = clean(model, "agy", 24, false);
    let tier = clean(&v["plan_tier"], "", 12, false);
    if !tier.is_empty() {
        label.push_str(&format!(" ({tier})"));
    }
    parts.push(label);
    if let Some(p) = pct(&v["context_window"]["used_percentage"]) {
        parts.push(format!("ctx {p}%"));
    }
    if unambiguous && let Some(remaining) = agy_weekly_remaining(v, ctx) {
        parts.push(format!("Gemini weekly {remaining} left"));
    }
    let cwd = first(v, &["cwd"]);
    let repo = root(
        if cwd.trim().is_empty() {
            ctx.cwd
        } else {
            Path::new(cwd)
        },
        ctx.cwd,
    );
    let branch = branch_label(repo.as_deref());
    if !branch.is_empty() {
        parts.push(branch);
    }
    let mode = clean(&v["vim"]["mode"], "", 12, false);
    let state = match v.get("agent_state") {
        None => clean(&v["state"], "", 16, false),
        Some(Value::String(state))
            if unambiguous
                && matches!(
                    state.as_str(),
                    "idle"
                        | "working"
                        | "tool_use"
                        | "reviewing"
                        | "authenticating"
                        | "initializing"
                        | "error"
                ) =>
        {
            state.clone()
        }
        _ => String::new(),
    };
    let mode = [mode, state]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if !mode.is_empty() {
        parts.push(mode);
    }
    let live = oms_live_strip(repo.as_deref());
    if !live.is_empty() {
        parts.push(live);
    }
    parts.join(" | ")
}
/// Standalone/tmux status: `oms-hud [--repo PATH] [--tmux] [--json] [--compact]`.
pub fn hud(args: &[OsString], ctx: &Context<'_>) -> Output {
    let (mut tmux, mut json_out, mut compact, mut target) = (false, false, false, None);
    let mut i = 0;
    while i < args.len() {
        match args[i].to_str() {
            Some("--tmux") => tmux = true,
            Some("--json") => json_out = true,
            Some("--compact") => compact = true,
            Some("--repo") => {
                i += 1;
                match args.get(i) {
                    Some(p) => target = Some(absolute(ctx.cwd, Path::new(p))),
                    None => return fail("--repo requires a path"),
                }
            }
            _ => return fail("usage: oms-hud [--repo PATH] [--tmux] [--json] [--compact]"),
        }
        i += 1;
    }
    let repo = root(target.as_deref().unwrap_or(ctx.cwd), ctx.cwd);
    let branch = branch_label(repo.as_deref());
    let state = repo
        .as_deref()
        .filter(|r| adopted(r))
        .map(live)
        .unwrap_or_default();
    let name = repo
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| clean(&json!(n.to_string_lossy()), "", 24, true))
        .unwrap_or_default();
    if json_out {
        return out(format!(
            "{}\n",
            json!({"schema":1,"repo":name,"branch":branch,"tasks":state.tasks,"task":state.title,
                "failures":state.failures,"agents":state.agents})
        ));
    }
    let mut parts = Vec::new();
    if !compact && !name.is_empty() {
        parts.push(name);
    }
    if !branch.is_empty() {
        parts.push(branch);
    }
    let live = strip(&state);
    if !live.is_empty() {
        parts.push(live);
    }
    let mut line = parts.join(" | ");
    if tmux {
        line = line
            .chars()
            .filter(|c| !matches!(c, '#' | '%'))
            .map(|c| if control(c) { ' ' } else { c })
            .collect();
    }
    out(format!("{line}\n"))
}
pub fn run(name: &str, raw: &[u8], ctx: &Context<'_>) -> Output {
    let max = if name == "claude-statusline" {
        256 * 1024
    } else {
        1024 * 1024
    };
    if raw.len() > max {
        return empty();
    }
    if name == "agy-statusline" {
        // Only unambiguous JSON can supply the current AGY fields.
        let (value, unambiguous) = match strict_document(raw) {
            Ok(value) => (Some(value), true),
            Err(_) => (document(raw), false),
        };
        return value.map_or_else(empty, |value| {
            out(format!("{}\n", agy(&value, unambiguous, ctx)))
        });
    }
    let Some(v) = document(raw) else {
        return empty();
    };
    if name == "claude-subagent-statusline" {
        out(subagent(&v))
    } else {
        let line = main_line(&v, ctx);
        context_cache(&v);
        #[cfg(target_os = "linux")]
        if name == "claude-statusline" {
            let _ = local_observation::record(&v, raw, ctx.source);
        }
        out(format!("{line}\n"))
    }
}

#[cfg(target_os = "linux")]
mod local_observation {
    use super::{Value, hash, json};
    use oms_reference::process::{IdentitySource, NativeIdentity, Owner};
    use rustix::fd::OwnedFd;
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, StatxFlags};
    use rustix::process::{getegid, getuid};
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Component, Path, PathBuf};

    const SWITCH: &str = "OMS_PANEL_CLAUDE_OBSERVATION";
    const AUTO_SWITCH: &str = "OMS_PANEL_CLAUDE_AUTO_OBSERVATION";
    const SLOT: &str = ".oms-claude-weekly-candidate.json";
    const AUTO_SLOT: &str = ".oms-claude-auto-weekly.json";
    // Skip redundant fsync/rename when an active auto slot has identical quota
    // values within a short fraction of the 600s consumer freshness TTL.
    const SAME_VALUE_REFRESH_SECONDS: f64 = 60.0;
    const DIR: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const OVERRIDES: &[&str] = &[
        concat!("ANTHROPIC_API_", "KEY"),
        concat!("ANTHROPIC_AUTH_", "TOKEN"),
        concat!("CLAUDE_CODE_OAUTH_", "TOKEN"),
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_CUSTOM_HEADERS",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    ];

    pub(super) fn record(input: &Value, raw: &[u8], oms_source: &Path) -> Option<()> {
        let pinned = std::env::var_os(SWITCH);
        let auto = std::env::var_os(AUTO_SWITCH);
        let (marker, auto) = match (pinned.as_deref(), auto.as_deref()) {
            (Some(pinned), None) => (pinned.to_str()?, None),
            (None, Some(auto)) => {
                let (launch, home) = auto.to_str()?.split_once(':')?;
                if !hex64(launch) {
                    return None;
                }
                (home, Some(launch))
            }
            _ => return None,
        };
        if !hex64(marker)
            || OVERRIDES
                .iter()
                .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
        {
            return None;
        }
        let validated;
        let input = if auto.is_some() {
            validated = crate::common::strict_document(raw).ok()?;
            &validated
        } else {
            input
        };
        let home = PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR")?);
        let id = input["session_id"].as_str().filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })?;
        let now = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|t| t.as_secs_f64())
        };
        let week = &input["rate_limits"]["seven_day"];
        let used = week["used_percentage"]
            .as_f64()
            .filter(|n| (0.0..=100.0).contains(n))?;
        let reset = week["resets_at"].as_f64()?;
        let in_window = |at| reset > at && reset <= at + 7.0 * 86400.0;
        let callback_observed_at = now()?;
        if !in_window(callback_observed_at) {
            return None;
        }
        let dir = open_home(&home, marker)?;
        let parent = NativeIdentity.foreground_parent().ok()?;
        let auto_value = auto.map(|launch| format!("{launch}:{marker}"));
        let source = |dir: &OwnedFd| {
            if NativeIdentity.foreground_parent().ok()? != parent {
                return None;
            }
            let proves = |owner: &Owner| {
                proves_callback(owner, dir, id, auto.is_some())
                    && auto_value
                        .as_deref()
                        .is_none_or(|value| auto_parent_scope(owner, &home, value))
            };
            if proves(&parent) {
                return Some(parent.clone());
            }
            let native = shell_parent(&parent, oms_source)?;
            proves(&native).then_some(native)
        };
        let owner = source(&dir)?;
        let process = hash(format!("claude-process\0{}\0{}", owner.pid, owner.birth));
        let session = hash(format!("claude-session\0{id}"));
        let slot = if auto.is_some() { AUTO_SLOT } else { SLOT };
        fs::flock(&dir, fs::FlockOperation::NonBlockingLockExclusive).ok()?;
        match fs::statat(&dir, slot, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => {
                let old = read_slot(&dir, slot, 1024, false)?;
                let prior = if auto.is_some() {
                    crate::common::strict_document(&old).ok()?
                } else {
                    crate::common::document(&old)?
                };
                let blocked = if let Some(launch) = auto {
                    !allow_auto_replace(
                        &prior,
                        &AutoCandidate {
                            launch,
                            home: marker,
                            process: &process,
                            session: &session,
                            used,
                            reset,
                            observed_at: callback_observed_at,
                        },
                    )
                } else {
                    prior["schema"] != 1
                        || (prior["home"] == marker
                            && prior["resets_at"]
                                .as_f64()
                                .is_some_and(|reset| reset > callback_observed_at))
                };
                if blocked {
                    return None;
                }
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(_) => return None,
        }
        if source(&dir)? != owner || !in_window(now()?) {
            return None;
        }
        open_home(&home, marker)?;
        let observation = json!({"schema":1,"home":marker,"process":process,
            "session":session,"weekly_used":used,"resets_at":reset,
            "callback_observed_at":callback_observed_at});
        let record = match auto {
            Some(launch) => json!({"launch":launch, "observation":observation}),
            None => observation,
        };
        write_slot(&dir, slot, &serde_json::to_vec(&record).ok()?)
    }

    struct AutoCandidate<'a> {
        launch: &'a str,
        home: &'a str,
        process: &'a str,
        session: &'a str,
        used: f64,
        reset: f64,
        observed_at: f64,
    }

    fn allow_auto_replace(prior: &Value, next: &AutoCandidate<'_>) -> bool {
        let Some(root) = prior.as_object().filter(|m| m.len() == 2) else {
            return false;
        };
        let Some(previous_launch) = root
            .get("launch")
            .and_then(Value::as_str)
            .filter(|v| hex64(v))
        else {
            return false;
        };
        let Some(previous) = root
            .get("observation")
            .and_then(Value::as_object)
            .filter(|m| m.len() == 7)
        else {
            return false;
        };
        if previous.get("schema") != Some(&json!(1))
            || previous.get("home").and_then(Value::as_str) != Some(next.home)
        {
            return false;
        }
        let Some(previous_process) = previous
            .get("process")
            .and_then(Value::as_str)
            .filter(|v| hex64(v))
        else {
            return false;
        };
        let Some(previous_session) = previous
            .get("session")
            .and_then(Value::as_str)
            .filter(|v| hex64(v))
        else {
            return false;
        };
        let Some(previous_used) = previous
            .get("weekly_used")
            .and_then(Value::as_f64)
            .filter(|n| (0.0..=100.0).contains(n))
        else {
            return false;
        };
        let Some(previous_reset) = previous.get("resets_at").and_then(Value::as_f64) else {
            return false;
        };
        let Some(previous_observed_at) = previous
            .get("callback_observed_at")
            .and_then(Value::as_f64)
            .filter(|t| *t >= 0.0 && *t < previous_reset)
        else {
            return false;
        };
        if previous_reset <= next.observed_at {
            return true;
        }
        // Two live launches can share a home. Keep an active window protected
        // against foreign launches/processes/sessions and replays, while allowing
        // the exact same source to refresh before the 600s consumer TTL expires.
        if previous_launch != next.launch
            || previous_process != next.process
            || previous_session != next.session
            || next.observed_at <= previous_observed_at
        {
            return false;
        }
        let same_value = next.used == previous_used && next.reset == previous_reset;
        !(same_value && next.observed_at - previous_observed_at < SAME_VALUE_REFRESH_SECONDS)
    }

    fn hex64(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    fn open_home(home: &Path, expected: &str) -> Option<OwnedFd> {
        let mut dir = fs::open("/", DIR, Mode::empty()).ok()?;
        let mut parts = home.components();
        if parts.next()? != Component::RootDir {
            return None;
        }
        for part in parts {
            let Component::Normal(name) = part else {
                return None;
            };
            dir = fs::openat(&dir, name, DIR, Mode::empty()).ok()?;
        }
        (mounted_identity(&dir).as_deref() == Some(expected)).then_some(dir)
    }

    fn mounted_identity(dir: &OwnedFd) -> Option<String> {
        let mask = StatxFlags::BASIC_STATS | StatxFlags::BTIME | StatxFlags::MNT_ID;
        let stat = fs::statx(dir, "", AtFlags::EMPTY_PATH, mask).ok()?;
        if stat.stx_mask & mask.bits() != mask.bits()
            || stat.stx_uid != getuid().as_raw()
            || stat.stx_mode & 0o022 != 0
            || stat.stx_ino == 0
            || stat.stx_btime.tv_sec <= 0
            || stat.stx_mnt_id == 0
        {
            return None;
        }
        Some(hash(format!(
            "claude-home\0{}\0{}\0{}\0{}\0{}\0{}",
            stat.stx_dev_major,
            stat.stx_dev_minor,
            stat.stx_ino,
            stat.stx_btime.tv_sec,
            stat.stx_btime.tv_nsec,
            stat.stx_mnt_id
        )))
    }

    fn proc_bytes(pid: u32, leaf: &str, cap: u64) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(format!("/proc/{pid}/{leaf}"))
            .ok()?
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= cap).then_some(bytes)
    }

    fn auto_parent_scope(owner: &Owner, home: &Path, expected: &str) -> bool {
        let read = || {
            let env = proc_bytes(owner.pid, "environ", 256 * 1024)?;
            let fields = || env.split(|b| *b == 0);
            let exact = |prefix: &[u8], expected: &[u8]| {
                let mut values = fields().filter_map(|entry| entry.strip_prefix(prefix));
                values.next() == Some(expected) && values.next().is_none()
            };
            Some(
                exact(b"OMS_PANEL_CLAUDE_AUTO_OBSERVATION=", expected.as_bytes())
                    && exact(b"CLAUDE_CONFIG_DIR=", home.as_os_str().as_bytes())
                    && !fields().any(|entry| {
                        entry.starts_with(b"OMS_PANEL_CLAUDE_OBSERVATION=")
                            || entry.iter().position(|b| *b == b'=').is_some_and(|pos| {
                                pos + 1 < entry.len()
                                    && OVERRIDES.iter().any(|key| key.as_bytes() == &entry[..pos])
                            })
                    })
                    && NativeIdentity.owner_identity(owner.pid).ok()?.eq(owner),
            )
        };
        read() == Some(true)
    }

    fn shell_parent(shell: &Owner, oms_source: &Path) -> Option<Owner> {
        let image = std::fs::metadata(format!("/proc/{}/exe", shell.pid)).ok()?;
        if !["/bin/sh", "/bin/bash"].into_iter().any(|path| {
            std::fs::metadata(path).is_ok_and(|m| (m.dev(), m.ino()) == (image.dev(), image.ino()))
        }) {
            return None;
        }
        let args = proc_bytes(shell.pid, "cmdline", 4096)?;
        let args: Vec<_> = args.strip_suffix(&[0])?.split(|b| *b == 0).collect();
        let [_, b"-c", cmd] = args.as_slice() else {
            return None;
        };
        let words = shlex::split(std::str::from_utf8(cmd).ok()?)?;
        let [path, verb] = words.as_slice() else {
            return None;
        };
        if verb != "claude-statusline" || !Path::new(path).is_absolute() {
            return None;
        }
        let invoked = std::fs::metadata(path).ok()?;
        if ![
            PathBuf::from("/proc/self/exe"),
            oms_source.join("scripts/oms"),
        ]
        .iter()
        .any(|launcher| {
            std::fs::metadata(launcher).is_ok_and(|current| {
                (current.dev(), current.ino()) == (invoked.dev(), invoked.ino())
            })
        }) {
            return None;
        }
        let stat = proc_bytes(shell.pid, "stat", 4096)?;
        let stat = std::str::from_utf8(&stat).ok()?;
        let pid = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        let owner = NativeIdentity.owner_identity(pid).ok()?;
        (NativeIdentity.owner_identity(shell.pid).ok()? == *shell).then_some(owner)
    }

    fn proves_callback(owner: &Owner, dir: &OwnedFd, session: &str, strict: bool) -> bool {
        let proof = || {
            for leaf in ["root", "ns/mnt"] {
                let current = std::fs::metadata(format!("/proc/self/{leaf}")).ok()?;
                let native = std::fs::metadata(format!("/proc/{}/{leaf}", owner.pid)).ok()?;
                if (current.dev(), current.ino()) != (native.dev(), native.ino()) {
                    return None;
                }
            }
            // The switch filters callbacks. The Board separately checks the live process environment.
            let sessions = fs::openat(dir, "sessions", DIR, Mode::empty()).ok()?;
            let filename = format!("{}.json", owner.pid);
            let bytes = read_slot(&sessions, &filename, 16 * 1024, true)?;
            let record = if strict {
                crate::common::strict_document(&bytes).ok()?
            } else {
                crate::common::document(&bytes)?
            };
            (record["pid"].as_u64() == Some(u64::from(owner.pid))
                && record["procStart"].as_str() == Some(owner.birth.rsplit_once(':')?.1)
                && record["sessionId"].as_str() == Some(session)
                && NativeIdentity.owner_identity(owner.pid).ok()? == *owner)
                .then_some(())
        };
        proof().is_some()
    }

    fn read_slot(dir: &OwnedFd, name: &str, cap: u64, session: bool) -> Option<Vec<u8>> {
        let uid = getuid().as_raw();
        let parent = fs::fstat(dir).ok()?;
        if parent.st_uid != uid || parent.st_mode & 0o022 != 0 {
            return None;
        }
        let fd = fs::openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
            Mode::empty(),
        )
        .ok()?;
        let before = fs::fstat(&fd).ok()?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
            || before.st_uid != uid
            || before.st_nlink != 1
            || before.st_mode & 0o002 != 0
            || (before.st_mode & 0o020 != 0
                && !(session && parent.st_mode & 0o077 == 0 && before.st_gid == getegid().as_raw()))
            || (!session && before.st_mode & 0o777 != 0o600)
        {
            return None;
        }
        let file = File::from(fd);
        let mut bytes = Vec::new();
        (&file).take(cap + 1).read_to_end(&mut bytes).ok()?;
        let after = fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        let same = |a: &fs::Stat, b: &fs::Stat| {
            (
                a.st_dev,
                a.st_ino,
                a.st_mode,
                a.st_uid,
                a.st_nlink,
                a.st_size,
                a.st_ctime,
                a.st_ctime_nsec,
            ) == (
                b.st_dev,
                b.st_ino,
                b.st_mode,
                b.st_uid,
                b.st_nlink,
                b.st_size,
                b.st_ctime,
                b.st_ctime_nsec,
            )
        };
        (bytes.len() as u64 <= cap && bytes.len() as i64 == before.st_size && same(&before, &after))
            .then_some(bytes)
    }

    fn write_slot(dir: &OwnedFd, slot: &str, bytes: &[u8]) -> Option<()> {
        let suffix = hash(crate::common::random::<16>().ok()?);
        let tmp = format!(".oms-claude-observation-{}", &suffix[..24]);
        let mode = Mode::from_raw_mode(0o600);
        let fd = fs::openat(
            dir,
            tmp.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
        )
        .ok()?;
        let mut file = File::from(fd);
        let written = fs::fchmod(&file, mode).is_ok()
            && file.write_all(bytes).is_ok()
            && file.sync_all().is_ok()
            && fs::renameat(dir, tmp.as_str(), dir, slot).is_ok();
        if !written {
            let _ = fs::unlinkat(dir, tmp.as_str(), AtFlags::empty());
        }
        written.then_some(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        #[test]
        fn local_weekly_callback_checks_native_parent_and_keeps_first_observation() {
            const CASE: &str = "OMS_LOCAL_WEEKLY_FIXTURE";
            const RESET_ENV: &str = "OMS_LOCAL_WEEKLY_RESET";
            const SESSION: &str = "fixture-native-session";
            const TEST: &str = "statusline::local_observation::tests::local_weekly_callback_checks_native_parent_and_keeps_first_observation";
            let run = |case: &str, dir: &Path, marker: &str, gateway: bool, reset: Option<u64>| {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", TEST])
                    .env_clear()
                    .env(CASE, case)
                    .env(
                        if marker.contains(':') {
                            AUTO_SWITCH
                        } else {
                            SWITCH
                        },
                        marker,
                    )
                    .env("CLAUDE_CONFIG_DIR", dir);
                if let Some(reset) = reset {
                    command.env(RESET_ENV, reset.to_string());
                }
                if gateway {
                    command.env("CLAUDE_CODE_USE_BEDROCK", "1");
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "synthetic callback {case}: {}",
                    String::from_utf8_lossy(&output.stdout)
                );
            };
            if let Ok(case) = std::env::var(CASE) {
                if matches!(case.as_str(), "auto-owner" | "contending-owner") {
                    let dir = PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").unwrap());
                    let marker = std::env::var(AUTO_SWITCH).unwrap();
                    let pid = std::process::id();
                    let owner = NativeIdentity.owner_identity(pid).unwrap();
                    let start = owner.birth.rsplit_once(':').unwrap().1;
                    std::fs::write(
                        dir.join(format!("sessions/{pid}.json")),
                        json!({"pid":pid,"procStart":start,"sessionId":SESSION}).to_string(),
                    )
                    .unwrap();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    let active_reset = now + 3600;
                    if case == "contending-owner" {
                        run("contended", &dir, &marker, false, Some(active_reset));
                        return;
                    }
                    for case in ["duplicate-key", "elapsed", "future", "other-session"] {
                        run(case, &dir, &marker, false, Some(active_reset));
                    }
                    run("override", &dir, &marker, true, Some(active_reset));
                    assert!(!dir.join(AUTO_SLOT).exists());
                    run("valid", &dir, &marker, false, Some(active_reset));
                    let path = dir.join(AUTO_SLOT);
                    let first = std::fs::read(&path).unwrap();
                    let first_doc = crate::common::strict_document(&first).unwrap();
                    let first_ts = first_doc["observation"]["callback_observed_at"]
                        .as_f64()
                        .unwrap();
                    let inode = std::fs::metadata(&path).unwrap().ino();
                    // Same value within short bounded interval avoids redundant write/fsync.
                    run("duplicate", &dir, &marker, false, Some(active_reset));
                    assert_eq!(std::fs::read(&path).unwrap(), first);
                    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);

                    // Repeated same-source callback with new used refreshes before weekly reset.
                    run("refresh-used", &dir, &marker, false, Some(active_reset));
                    let refreshed = std::fs::read(&path).unwrap();
                    let refreshed_doc = crate::common::strict_document(&refreshed).unwrap();
                    let refreshed_ts = refreshed_doc["observation"]["callback_observed_at"]
                        .as_f64()
                        .unwrap();
                    assert_eq!(refreshed_doc["observation"]["weekly_used"], 28.0);
                    assert!(refreshed_ts > first_ts);
                    assert_ne!(std::fs::metadata(&path).unwrap().ino(), inode);

                    // Same value past bounded interval refreshes timestamp before consumer TTL.
                    let mut stale_same = refreshed_doc.clone();
                    let backdated = refreshed_ts - 65.0;
                    stale_same["observation"]["callback_observed_at"] = json!(backdated);
                    std::fs::write(&path, serde_json::to_vec(&stale_same).unwrap()).unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    run(
                        "refresh-same-stale",
                        &dir,
                        &marker,
                        false,
                        Some(active_reset),
                    );
                    let stale_refreshed =
                        crate::common::strict_document(&std::fs::read(&path).unwrap()).unwrap();
                    assert_eq!(stale_refreshed["observation"]["weekly_used"], 28.0);
                    assert!(
                        stale_refreshed["observation"]["callback_observed_at"]
                            .as_f64()
                            .unwrap()
                            > backdated
                    );

                    // Replay (non-newer timestamp) retains existing.
                    let mut replay = refreshed_doc.clone();
                    replay["observation"]["callback_observed_at"] = json!(now as f64 + 60.0);
                    let replay_bytes = serde_json::to_vec(&replay).unwrap();
                    std::fs::write(&path, &replay_bytes).unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    let replay_ino = std::fs::metadata(&path).unwrap().ino();
                    run("rejected-update", &dir, &marker, false, Some(active_reset));
                    assert_eq!(std::fs::read(&path).unwrap(), replay_bytes);
                    assert_eq!(std::fs::metadata(&path).unwrap().ino(), replay_ino);

                    // Wrong session retains existing.
                    let mut wrong_session = refreshed_doc.clone();
                    wrong_session["observation"]["session"] =
                        json!(hash("claude-session\0other-session"));
                    let wrong_session_bytes = serde_json::to_vec(&wrong_session).unwrap();
                    std::fs::write(&path, &wrong_session_bytes).unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    let wrong_session_ino = std::fs::metadata(&path).unwrap().ino();
                    run("rejected-update", &dir, &marker, false, Some(active_reset));
                    assert_eq!(std::fs::read(&path).unwrap(), wrong_session_bytes);
                    assert_eq!(std::fs::metadata(&path).unwrap().ino(), wrong_session_ino);

                    // Wrong process retains existing.
                    let mut wrong_process = refreshed_doc.clone();
                    wrong_process["observation"]["process"] =
                        json!(hash("claude-process\0999999\0other-boot:1"));
                    let wrong_process_bytes = serde_json::to_vec(&wrong_process).unwrap();
                    std::fs::write(&path, &wrong_process_bytes).unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    let wrong_process_ino = std::fs::metadata(&path).unwrap().ino();
                    run("rejected-update", &dir, &marker, false, Some(active_reset));
                    assert_eq!(std::fs::read(&path).unwrap(), wrong_process_bytes);
                    assert_eq!(std::fs::metadata(&path).unwrap().ino(), wrong_process_ino);

                    // Malformed prior (missing timestamp, extra field, invalid JSON) retains existing.
                    let mut missing_ts = refreshed_doc.clone();
                    missing_ts["observation"]
                        .as_object_mut()
                        .unwrap()
                        .remove("callback_observed_at");
                    let mut extra_field = refreshed_doc.clone();
                    extra_field["observation"]
                        .as_object_mut()
                        .unwrap()
                        .insert("unexpected".into(), json!(true));
                    for malformed_bytes in [
                        serde_json::to_vec(&missing_ts).unwrap(),
                        serde_json::to_vec(&extra_field).unwrap(),
                        b"{not-json".to_vec(),
                    ] {
                        std::fs::write(&path, &malformed_bytes).unwrap();
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                            .unwrap();
                        let malformed_ino = std::fs::metadata(&path).unwrap().ino();
                        run("rejected-update", &dir, &marker, false, Some(active_reset));
                        assert_eq!(std::fs::read(&path).unwrap(), malformed_bytes);
                        assert_eq!(std::fs::metadata(&path).unwrap().ino(), malformed_ino);
                    }

                    // Restore valid refreshed receipt for outer assertions & contending-owner check.
                    std::fs::write(&path, &refreshed).unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    return;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let fixed_reset = std::env::var(RESET_ENV)
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(now + 3600);
                let reset = match case.as_str() {
                    "elapsed" => now - 1,
                    "future" => now + 8 * 86400,
                    _ => fixed_reset,
                };
                let used = match case.as_str() {
                    "refresh-used" | "refresh-same-stale" => 28.0,
                    "rejected-update" | "pinned-refresh" => 42.0,
                    _ => 14.0,
                };
                let id = if case == "other-session" {
                    "different-session"
                } else {
                    SESSION
                };
                let raw = json!({"session_id":id,"rate_limits":{"seven_day":{"used_percentage":used,"resets_at":reset}}});
                let mut bytes = raw.to_string();
                if case == "duplicate-key" {
                    bytes.insert_str(1, &format!(r#""session_id":"{SESSION}","#));
                }
                assert_eq!(
                    record(&raw, bytes.as_bytes(), Path::new("/unused")).is_some(),
                    matches!(
                        case.as_str(),
                        "valid" | "refresh-used" | "refresh-same-stale"
                    )
                );
                return;
            }
            struct Temp(PathBuf);
            impl Drop for Temp {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let temp = Temp(std::env::temp_dir().canonicalize().unwrap().join(format!(
                "oms-weekly-{}",
                &hash(crate::common::random::<16>().unwrap())[..24]
            )));
            let pid = std::process::id();
            let parent = NativeIdentity.owner_identity(pid).unwrap();
            let start = parent.birth.rsplit_once(':').unwrap().1;
            let native = json!({"pid":pid,"procStart":start,"sessionId":SESSION});
            let fixture = |name| {
                let home = temp.0.join(name);
                let sessions = home.join("sessions");
                std::fs::create_dir_all(&sessions).unwrap();
                std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
                std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                std::fs::write(sessions.join(format!("{pid}.json")), native.to_string()).unwrap();
                let fd = fs::open(&home, DIR, Mode::empty()).unwrap();
                (home, mounted_identity(&fd))
            };
            let (home, Some(identity)) = fixture("one") else {
                return;
            };
            let (other, Some(other_identity)) = fixture("two") else {
                return;
            };
            for case in ["elapsed", "future", "other-session"] {
                run(case, &home, &identity, false, None);
            }
            run("override", &home, &identity, true, None);
            run("wrong-home", &other, &identity, false, None);
            assert!(!home.join(SLOT).exists() && !other.join(SLOT).exists());
            run("valid", &home, &identity, false, None);
            let stored = std::fs::read(home.join(SLOT)).unwrap();
            let inode = std::fs::metadata(home.join(SLOT)).unwrap().ino();
            run("duplicate", &home, &identity, false, None);
            run("pinned-refresh", &home, &identity, false, None);
            assert_eq!(std::fs::metadata(home.join(SLOT)).unwrap().ino(), inode);
            assert_eq!(std::fs::read(home.join(SLOT)).unwrap(), stored);
            assert_eq!(
                std::fs::metadata(home.join(SLOT)).unwrap().mode() & 0o777,
                0o600
            );
            assert!(!String::from_utf8(stored).unwrap().contains(SESSION));
            run("valid", &other, &other_identity, false, None);
            let second =
                crate::common::document(&std::fs::read(other.join(SLOT)).unwrap()).unwrap();
            assert_eq!(second["home"], other_identity);

            let auto = format!("{}:{identity}", "a".repeat(64));
            run("spoofed-child", &home, &auto, false, None);
            run("wrong-home", &other, &auto, false, None);
            let path = home.join(AUTO_SLOT);
            assert!(!path.exists() && !other.join(AUTO_SLOT).exists());
            run("auto-owner", &home, &auto, false, None);
            let first = std::fs::read(&path).unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            assert_eq!(meta.mode() & 0o777, 0o600);
            let receipt = crate::common::strict_document(&first).unwrap();
            assert_eq!(receipt["launch"], "a".repeat(64));
            assert_eq!(receipt["observation"]["weekly_used"], 28.0);
            assert!(!String::from_utf8_lossy(&first).contains(SESSION));
            run(
                "contending-owner",
                &home,
                &format!("{}:{identity}", "b".repeat(64)),
                false,
                None,
            );
            assert_eq!(std::fs::read(&path).unwrap(), first);
            assert_eq!(std::fs::metadata(&path).unwrap().ino(), meta.ino());
            assert_eq!(std::fs::metadata(home.join(SLOT)).unwrap().ino(), inode);
        }
    }
}
