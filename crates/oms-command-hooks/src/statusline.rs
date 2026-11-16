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
fn agy(v: &Value, ctx: &Context<'_>) -> String {
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
    let state = clean(&v["state"], "", 16, false);
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
    let Some(v) = document(raw) else {
        return empty();
    };
    if name == "claude-subagent-statusline" {
        out(subagent(&v))
    } else if name == "agy-statusline" {
        out(format!("{}\n", agy(&v, ctx)))
    } else {
        let line = main_line(&v, ctx);
        context_cache(&v);
        out(format!("{line}\n"))
    }
}
