//! Read-only source collection. Failed collectors remain explicit degraded sources.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, epoch, error, failures, helpers, json, json_output,
    require, spine, storage, success, timestamp,
};
use std::ffi::OsString;
use std::path::Path;

#[path = "state_projection.rs"]
mod projection;

pub(crate) const HELP: &str = "Usage: oms state [--repo PATH] [--json] [--refresh-ci]\n       oms state-verify [--repo PATH] [--json]\n       oms inbox [--repo PATH] [--json] [--fix-safe]\n       oms dashboard [--repo PATH] [--json] [--watch [--interval N] [--count N]]\nState views never grant authority. Unsupported sources are reported as degraded.\n";

fn decoded(output: Output) -> Result<Value> {
    require(output.0 == 0, String::from_utf8_lossy(&output.2))?;
    json::parse(&output.1)
}

fn collector(result: Result<Value>, present: bool) -> Value {
    match result {
        Ok(mut row) if row.object().is_ok() => {
            let _ = row.set("healthy", Value::Bool(true));
            row
        }
        Ok(rows) if rows.array().is_ok() => {
            object([("healthy", Value::Bool(true)), ("rows", rows)])
        }
        _ => object([
            ("healthy", Value::Bool(false)),
            ("present", Value::Bool(present)),
            ("error", string("status-unavailable")),
        ]),
    }
}

fn plan(ctx: &Context<'_>, action: oms_command_plan::Action) -> Result<Value> {
    let req = oms_command_plan::Request::new(action).flag("json");
    let mut context = oms_command_plan::Context::from_environment(&req, &ctx.repo)?;
    context.state_repo = ctx.repo.clone();
    context.plan_file = ctx.repo.join(".oms/plan/tasks.json");
    let services = oms_command_plan::UnavailableServices;
    let plan = oms_command_plan::Plan::open(context, &services, ctx.interrupted)?;
    let response = plan.execute(&req)?;
    require(
        response.code == 0,
        String::from_utf8_lossy(&response.stderr),
    )?;
    json::parse(&response.stdout)
}

pub(crate) fn snapshot(ctx: &Context<'_>) -> Result<Value> {
    let mut state = object([
        ("schema", number(1)),
        ("project", helpers::project(&ctx.repo)),
    ]);
    let task = decoded(oms_command_task::run(
        &["status".into(), "--json".into()],
        &ctx.repo,
        ctx.interrupted,
    ));
    state.set(
        "task",
        collector(task, ctx.repo.join(".oms/task/current.md").exists()),
    )?;
    state.set(
        "plan",
        collector(
            plan(ctx, oms_command_plan::Action::Status),
            ctx.repo.join(".oms/plan/tasks.json").exists(),
        ),
    )?;
    let events = decoded(oms_command_operations::run(
        oms_command_operations::App::Events,
        &["list".into(), "--json".into()],
        &ctx.repo,
        ctx.interrupted,
    ));
    let event_rows = events
        .as_ref()
        .ok()
        .and_then(|v| v.array().ok())
        .unwrap_or(&[]);
    let mut counts = json::empty();
    let mut active = 0;
    let mut executing = 0;
    let mut attention = Vec::new();
    let mut aged_reviews = 0;
    for row in event_rows {
        let name = row.text("state");
        counts.set(name, number(counts.get(name).int().unwrap_or(0) + 1))?;
        active += usize::from(!row.get("terminal").truth());
        executing += usize::from(
            !row.get("terminal").truth()
                && (["starting", "working", "verifying"].contains(&name)
                    || name.starts_with("waiting_")),
        );
        if ["waiting_input", "waiting_approval", "blocked"].contains(&name) {
            attention.push(row.clone());
        }
        if name == "review"
            && helpers::now_age(row.text("review_entered_at")).is_some_and(|age| age > 3 * 86400)
        {
            aged_reviews += 1;
        }
    }
    let mut active_latest: Vec<_> = event_rows
        .iter()
        .filter(|r| !r.get("terminal").truth())
        .cloned()
        .collect();
    active_latest.sort_by(|a, b| a.text("updated_at").cmp(b.text("updated_at")));
    state.set(
        "agent_operations",
        object([
            ("healthy", Value::Bool(events.is_ok())),
            ("total", number(event_rows.len())),
            ("active", number(active)),
            ("executing", number(executing)),
            ("review", counts.get("review").clone()),
            ("review_aged", number(aged_reviews)),
            ("by_state", counts),
            ("needs_attention", Value::Array(attention)),
            (
                "latest",
                Value::Array(event_rows.iter().rev().take(5).rev().cloned().collect()),
            ),
            (
                "recent",
                Value::Array(event_rows.iter().rev().take(8).rev().cloned().collect()),
            ),
            (
                "active_latest",
                Value::Array(active_latest.into_iter().rev().take(8).collect()),
            ),
        ]),
    )?;
    let approvals = decoded(oms_command_operations::run(
        oms_command_operations::App::Approvals,
        &["list".into(), "--json".into()],
        &ctx.repo,
        ctx.interrupted,
    ));
    let approval_rows = approvals
        .as_ref()
        .ok()
        .and_then(|v| v.array().ok())
        .unwrap_or(&[]);
    let pending: Vec<_> = approval_rows
        .iter()
        .filter(|r| ["requested", "approved", "consuming"].contains(&r.text("effective_state")))
        .cloned()
        .collect();
    state.set(
        "approvals",
        object([
            ("healthy", Value::Bool(approvals.is_ok())),
            ("total", number(approval_rows.len())),
            ("pending", number(pending.len())),
            ("latest_pending", Value::Array(pending)),
        ]),
    )?;
    let failure_rows = storage::rows(&ctx.repo, Path::new(".oms/failures.jsonl"));
    let failure = failure_rows.and_then(|(rows, bad)| failures::project(&rows, bad, "", epoch()));
    state.set(
        "failures",
        collector(failure, ctx.repo.join(".oms/failures.jsonl").exists()),
    )?;
    let journal = decoded(oms_command_journal::run(
        &["status".into(), "--json".into()],
        &ctx.repo,
        ctx.interrupted,
    ));
    state.set(
        "work_journal",
        collector(journal, ctx.repo.join(".oms/work-journal").exists()),
    )?;
    let (runs, bad) = storage::rows(&ctx.repo, &spine::index(ctx))?;
    let mut by_id: Vec<Value> = Vec::new();
    for row in runs {
        let id = row.text("run_id");
        if id.is_empty() {
            continue;
        }
        let i = by_id
            .iter()
            .position(|r| r.text("run_id") == id)
            .unwrap_or_else(|| {
                by_id.push(object([
                    ("run_id", string(id)),
                    ("closed", Value::Bool(false)),
                ]));
                by_id.len() - 1
            });
        by_id[i].set("last", row.get("ts").clone())?;
        if row.text("tool") == "oms-run" && row.text("event") == "close" {
            by_id[i].set("closed", Value::Bool(true))?;
        }
    }
    state.set(
        "runs",
        object([
            ("healthy", Value::Bool(bad == 0)),
            (
                "current",
                spine::current(ctx).map(string).unwrap_or(Value::Null),
            ),
            (
                "open",
                Value::Array(
                    by_id
                        .iter()
                        .filter(|r| !r.get("closed").truth())
                        .cloned()
                        .collect(),
                ),
            ),
            ("recent", Value::Array(by_id)),
        ]),
    )?;
    let room_id = std::env::var("OMS_ROOM_ID").unwrap_or_default();
    let participant = std::env::var("OMS_ROOM_PARTICIPANT").unwrap_or_default();
    let bound = !room_id.is_empty() || !participant.is_empty();
    let mut mail = object([
        ("bound", Value::Bool(bound)),
        ("healthy", Value::Bool(!bound)),
        ("pending", number(0)),
        ("room_id", string(&room_id)),
        ("participant", string(&participant)),
        ("oldest_age_seconds", Value::Null),
        ("command", string("")),
    ]);
    if bound
        && std::env::var_os("OMS_ROOM_REPO")
            .is_none_or(|p| std::fs::canonicalize(p).ok().as_ref() == Some(&ctx.repo))
    {
        let args = [
            "updates".into(),
            "--id".into(),
            room_id.clone().into(),
            "--participant".into(),
            participant.clone().into(),
            "--json".into(),
        ];
        if let Ok(value) = decoded(oms_command_room::run(&args, &ctx.repo, ctx.interrupted)) {
            let turns = value.get("turns").array().unwrap_or(&[]);
            mail.set("pending", number(turns.len()))?;
            mail.set("healthy", Value::Bool(true))?;
            mail.set(
                "oldest_age_seconds",
                turns
                    .iter()
                    .filter_map(|r| helpers::now_age(r.text("ts")))
                    .max()
                    .map(number)
                    .unwrap_or(Value::Null),
            )?;
            mail.set(
                "command",
                string(format!(
                    "oms room updates --repo {} --id {} --participant {}",
                    quote(&ctx.repo.to_string_lossy()),
                    quote(&room_id),
                    quote(&participant)
                )),
            )?;
        }
    }
    state.set("room_mail", mail)?;
    state.set("repo", string(ctx.repo.to_string_lossy()))?;
    projection::enrich(ctx, &mut state)?;
    Ok(state)
}

fn quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}

pub(crate) fn inbox(state: &Value, repo: &Path) -> Result<Value> {
    let mut items = Vec::new();
    let anchor = quote(&repo.to_string_lossy());
    for (key, command) in [
        ("task", "agent-task status"),
        ("plan", "agent-plan status"),
        ("agent_operations", "agent-events validate"),
        ("approvals", "approval-inbox validate"),
        ("failures", "fail-ledger list"),
        ("runtime", "runtime envelope show"),
    ] {
        if state.get(key).get("healthy") == &Value::Bool(false) {
            let (verb, tail) = command.split_once(' ').unwrap_or((command, ""));
            items.push(object([
                ("priority", string("P1")),
                ("code", string(format!("{key}-unavailable"))),
                ("count", number(1)),
                ("summary", string(format!("{key} source is unavailable"))),
                (
                    "command",
                    string(format!("oms {verb} --repo {anchor} {tail}")),
                ),
            ]));
        }
    }
    for (key, rows, command) in [
        (
            "pending-room-mail",
            state.get("room_mail").get("pending").int().unwrap_or(0),
            format!(
                "OMS_ROOM_REPO={} {}",
                anchor,
                state.get("room_mail").text("command")
            ),
        ),
        (
            "pending-approvals",
            state.get("approvals").get("pending").int().unwrap_or(0),
            format!("oms approval-inbox --repo {anchor} list --json"),
        ),
        (
            "attempts-needing-attention",
            state
                .get("agent_operations")
                .get("needs_attention")
                .array()
                .unwrap_or(&[])
                .len() as i128,
            format!("oms agent-events --repo {anchor} list --json"),
        ),
        (
            "stale-plan-claims",
            state.get("plan").get("stale").array().unwrap_or(&[]).len() as i128,
            format!("oms agent-plan --repo {anchor} reclaim"),
        ),
    ] {
        if rows > 0 {
            items.push(object([
                ("priority", string("P1")),
                ("code", string(key)),
                ("count", number(rows)),
                ("summary", string(format!("{rows} {key}"))),
                ("command", string(command)),
            ]));
        }
    }
    projection::extra_attention(state, repo, &mut items)?;
    items.sort_by(|a, b| a.text("priority").cmp(b.text("priority")));
    Ok(object([
        ("schema", number(1)),
        ("actionable", number(items.len())),
        ("safe_actions", Value::Array(vec![])),
        ("items", Value::Array(items)),
        (
            "recommended_actions",
            state.get("runtime").get("next_actions").clone(),
        ),
        ("room_mail", state.get("room_mail").clone()),
    ]))
}

fn render(report: &Value, tool: &str) -> Result<String> {
    Ok(projection::render(report, tool)?
        .chars()
        .filter(|c| !c.is_control() || ['\n', '\t'].contains(c))
        .collect())
}

pub(crate) fn run(tool: &str, args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let (values, flags): (&[&str], &[&str]) = match tool {
        "state" => (&["repo"], &["json", "refresh-ci"]),
        "inbox" => (&["repo"], &["json", "fix-safe"]),
        "dashboard" => (&["repo", "interval", "count"], &["json", "watch"]),
        _ => (&["repo"], &["json"]),
    };
    let opts = Options::parse(args, values, flags)?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    require(opts.pos.is_empty(), "unexpected positional argument")?;
    let repo = if opts.get("repo").is_empty() && ["state", "inbox"].contains(&tool) {
        std::env::var_os("OMS_ROOM_REPO")
            .map(|p| {
                oms_command_operations::storage::repo_root(Path::new(&p), &ctx.cwd)
                    .map_err(|e| error(2, e))
            })
            .transpose()?
            .unwrap_or_else(|| ctx.repo.clone())
    } else {
        opts.repo(ctx)?
    };
    if tool == "state-verify" && !repo.join(".oms").exists() {
        let value = object([
            ("schema", number(1)),
            ("repo", string(repo.to_string_lossy())),
            ("adopted", Value::Bool(false)),
            ("findings", Value::Array(vec![])),
        ]);
        return if opts.flag("json") {
            json_output(0, &value)
        } else {
            Ok(success(format!(
                "state-verify: {} is not harness-adopted (no .oms); nothing to verify\n",
                repo.display()
            )))
        };
    }
    let ctx = Context {
        runtime: ctx.runtime,
        cwd: repo.clone(),
        repo: repo.clone(),
        state_repo: repo.clone(),
        interrupted: ctx.interrupted,
    };
    require(
        tool == "dashboard"
            || !opts.flag("watch")
                && opts.get("count").is_empty()
                && opts.get("interval").is_empty(),
        "watch options require dashboard",
    )?;
    require(
        !opts.flag("watch") || !opts.flag("json"),
        "--json is a single snapshot",
    )?;
    require(
        opts.flag("watch") || opts.get("count").is_empty() && opts.get("interval").is_empty(),
        "--count and --interval require --watch",
    )?;
    let count = opts.number("count", if opts.flag("watch") { usize::MAX } else { 1 })?;
    let interval = opts.number("interval", 5)?;
    require(
        count > 0 && interval >= 2,
        "count must be positive and interval at least 2",
    )?;
    if opts.flag("refresh-ci") {
        let _ = projection::refresh_ci(&ctx);
    }
    let safe_actions = if opts.flag("fix-safe") {
        projection::repairs(&ctx)?
    } else {
        vec![]
    };
    let mut out = Vec::new();
    let mut code = 0;
    for i in 0..count {
        crate::checkpoint(ctx.interrupted)?;
        let state = snapshot(&ctx)?;
        let report = match tool {
            "state" => state,
            "inbox" => {
                let mut report = inbox(&state, &repo)?;
                report.set("safe_actions", Value::Array(safe_actions.clone()))?;
                report
            }
            "state-verify" => {
                let (mut findings, _) = spine::validate(&ctx, &repo.join(".oms"))?;
                for key in ["task", "plan", "agent_operations", "approvals", "failures"] {
                    if state.get(key).get("healthy") == &Value::Bool(false) {
                        findings.push(object([
                            ("family", string(key)),
                            ("message", string("source validation failed")),
                            (
                                "remedy",
                                string(format!(
                                    "oms state --repo {} --json",
                                    quote(&repo.to_string_lossy())
                                )),
                            ),
                        ]));
                    }
                }
                projection::verify(&ctx, &state, &mut findings)?;
                let fails = findings
                    .iter()
                    .filter(|r| r.text("level") == "fail")
                    .count();
                let warns = findings.len() - fails;
                code = i32::from(!findings.is_empty());
                object([
                    ("schema", number(1)),
                    ("repo", string(repo.to_string_lossy())),
                    ("adopted", Value::Bool(repo.join(".oms").is_dir())),
                    (
                        "summary",
                        object([("fail", number(fails)), ("warn", number(warns))]),
                    ),
                    ("findings", Value::Array(findings)),
                ])
            }
            "dashboard" => {
                let report = projection::dashboard(&ctx, &state)?;
                code = i32::from(!report.get("collection").get("ok").truth());
                report
            }
            _ => return Err(error(2, "unknown state view")),
        };
        if opts.flag("json") {
            return json_output(code, &report);
        }
        let text = render(&report, tool)?;
        if opts.flag("watch") {
            use std::io::{IsTerminal, Write};
            let mut stdout = std::io::stdout().lock();
            if stdout.is_terminal() {
                stdout
                    .write_all(b"\x1b[H\x1b[2J")
                    .map_err(|e| error(1, e))?;
            } else if i > 0 {
                stdout.write_all(b"----\n").map_err(|e| error(1, e))?;
            }
            stdout
                .write_all(text.as_bytes())
                .and_then(|_| stdout.flush())
                .map_err(|e| error(1, e))?;
        } else {
            out.extend_from_slice(text.as_bytes());
        }
        if !opts.flag("watch") {
            break;
        }
        if i + 1 < count {
            for _ in 0..interval.saturating_mul(10) {
                crate::checkpoint(ctx.interrupted)?;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    Ok((code, out, vec![]))
}
