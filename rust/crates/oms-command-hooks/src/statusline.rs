use crate::{Output, common::*};
use serde_json::{Value, json};

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
        &clean(&model["id"], "Claude", 24, false),
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
    } else {
        let line = main_line(&v, ctx);
        context_cache(&v);
        out(format!("{line}\n"))
    }
}
