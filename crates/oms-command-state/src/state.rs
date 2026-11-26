//! Read-only source collection. Failed collectors remain explicit degraded sources.
use crate::json::{number, object, string};
use crate::{
    Context, HostServices, Options, Output, Result, Value, epoch, error, failures, helpers, json,
    json_output, require, spine, storage, success, timestamp,
};
use oms_command_room::AttemptRoom;
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::AtomicBool;

#[path = "state_projection.rs"]
mod projection;

pub(crate) const HELP: &str = "Usage: oms state [--repo PATH] [--json] [--refresh-ci]\n       oms state gpu-status [--json]\n       oms state gpu-wait --min-free-mib N [--gpu IDX] [--timeout-secs S] [--poll-ms M] [--json]\n       oms state compact [--repo PATH] [--keep-recent N] [--dry-run] [--json]\n       oms state archive <list|show|search|stats> [--repo PATH] [--json]\n       oms state-verify [--repo PATH] [--json]\n       oms inbox [--repo PATH] [--json] [--fix-safe]\n       oms dashboard [--repo PATH] [--json [--panel-snapshot [--conversation-digest HEX32]]] [--watch [--interval N] [--count N]]\nState views never grant authority. Unsupported sources are reported as degraded.\n";

fn decoded(output: Output) -> Result<Value> {
    require(output.0 == 0, String::from_utf8_lossy(&output.2))?;
    json::parse_bounded(&output.1, storage::MAX_STREAM)
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

fn plan_action(
    repo: &Path,
    action: oms_command_plan::Action,
    host: &dyn HostServices,
    interrupted: &AtomicBool,
) -> Result<Value> {
    let req = oms_command_plan::Request::new(action).flag("json");
    let mut context = oms_command_plan::Context::from_environment(&req, repo)?;
    context.state_repo = repo.to_path_buf();
    context.plan_file = repo.join(".oms/plan/tasks.json");
    let plan = oms_command_plan::Plan::open(context, host.plan_services(), interrupted)?;
    let response = plan.execute(&req)?;
    require(
        response.code == 0,
        String::from_utf8_lossy(&response.stderr),
    )?;
    json::parse(&response.stdout)
}

fn plan(
    ctx: &Context<'_>,
    action: oms_command_plan::Action,
    host: &dyn HostServices,
) -> Result<Value> {
    plan_action(&ctx.repo, action, host, ctx.interrupted)
}

pub(crate) fn plan_status(
    repo: &Path,
    interrupted: &AtomicBool,
    host: &dyn HostServices,
) -> Result<Value> {
    crate::checkpoint(interrupted)?;
    let repo = repo.canonicalize().map_err(|e| error(2, e))?;
    plan_action(&repo, oms_command_plan::Action::Status, host, interrupted)
}

pub(crate) fn snapshot(ctx: &Context<'_>, host: &dyn HostServices) -> Result<Value> {
    collect_snapshot(ctx, host, None)
}

fn attempt_room(row: &Value) -> AttemptRoom<'_> {
    let Some(refs) = row.opt("refs") else {
        return AttemptRoom::Unspecified;
    };
    if refs.object().is_err() {
        return AttemptRoom::Invalid;
    }
    let tag = |key| refs.opt(key).map(|v| v.str().unwrap_or(""));
    AttemptRoom::from_tags(tag("panel_room_id"), tag("panel_room"))
}

/// A native Main's owner is also the Pi gateway's inference provider. Only
/// the exact attempt's producer metadata distinguishes the two harnesses.
fn conversation_harness(
    consumer: &str,
    binding: &oms_command_room::value::Value,
    rows: &[Value],
) -> Option<&'static str> {
    let main = &binding["conversationMain"];
    let participant = main.text("participant");
    let room = main.text("room");
    if binding.text("conversationBinding") != "matched"
        || main.text("provider") != "codex"
        || main.text("source") != "room-session"
        || main.text("state") != "joined"
        || room.is_empty()
        || participant.is_empty()
    {
        return None;
    }
    // As in native Main admission, two unfinished Main attempts in the same
    // Room cannot identify a single current harness. Other Rooms and old
    // terminal participants cannot supply this observation.
    let mut current = rows.iter().filter(|row| {
        row.text("tool") == "panel-main"
            && (row.get("refs").text("panel_room_id") == room
                || row.get("refs").text("panel_room") == room)
            && !matches!(row.get("terminal"), Value::Bool(true))
    });
    let row = current.next()?;
    let refs = row.get("refs");
    // Fresh launches join as their attempt ID; in-place swaps explicitly
    // record the participant they retain.
    let member = refs
        .opt("panel_room_participant")
        .map_or_else(|| row.get("attempt_id").str().ok(), |v| v.str().ok())?;
    if current.next().is_some()
        || refs.text("panel_room_id") != room
        || attempt_room(row) != AttemptRoom::Named(room)
        || member != participant
        || !matches!(row.get("terminal"), Value::Bool(false))
        || row.text("provider") != main.text("provider")
        || refs.text("panel_owner") != main.text("provider")
        || refs.text("panel_role") != "main"
        || refs
            .opt("panel_session_digest")
            .is_some_and(|v| v.str().ok() != Some(consumer))
    {
        return None;
    }
    match refs.text("panel_harness") {
        "codex" => Some("codex"),
        "pi" => Some("pi"),
        _ => None,
    }
}

// Keep Room evidence out of the repository-wide recency window. A worker without
// a Room field may still have an exact parent attempt within this Room.
fn panel_room_attempts(rows: &[Value], room: &str) -> (Vec<Value>, usize) {
    let lead = |row: &Value| {
        row.get("refs").text("panel_role") == "lead"
            || row.text("tool") == "panel-native-assignment"
    };
    let leads: std::collections::BTreeSet<_> = rows
        .iter()
        .filter(|row| attempt_room(row) == AttemptRoom::Named(room) && lead(row))
        .map(|row| row.text("attempt_id"))
        .filter(|id| !id.is_empty())
        .collect();
    let mut scoped: Vec<_> = rows
        .iter()
        .filter(|row| match attempt_room(row) {
            AttemptRoom::Named(id) => id == room,
            AttemptRoom::Unspecified => leads.contains(row.text("parent_attempt_id")),
            AttemptRoom::Invalid => false,
        })
        .collect();
    scoped.sort_by(|a, b| {
        let rank = |row: &Value| {
            let root = row.text("tool") == "panel-main" || lead(row);
            (
                row.get("terminal").truth(),
                oms_command_operations::activity_rank(row.text("state")),
                !root,
            )
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| b.text("updated_at").cmp(a.text("updated_at")))
    });
    let omitted = scoped.len().saturating_sub(24);
    (scoped.into_iter().take(24).cloned().collect(), omitted)
}

fn collect_snapshot(
    ctx: &Context<'_>,
    host: &dyn HostServices,
    consumer: Option<&str>,
) -> Result<Value> {
    let mut state = object([
        ("schema", number(1)),
        ("project", helpers::project(&ctx.repo)),
    ]);
    let task = decoded(
        host.task_status(&ctx.repo, ctx.interrupted)
            .unwrap_or_else(|| {
                oms_command_task::run(
                    &["status".into(), "--json".into()],
                    &ctx.repo,
                    ctx.interrupted,
                )
            }),
    );
    state.set(
        "task",
        collector(task, ctx.repo.join(".oms/task/current.md").exists()),
    )?;
    state.set(
        "plan",
        collector(
            plan(ctx, oms_command_plan::Action::Status, host),
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
    if let Some(consumer) = consumer
        && events.as_ref().is_ok_and(|v| v.array().is_ok())
    {
        // The panel and this exact-chat lookup share the full lifecycle collection.
        // 'recent' and 'active_latest' below can omit a terminal Main.
        let panel_mains = event_rows
            .iter()
            .filter(|row| row.text("tool") == "panel-main")
            .map(|row| {
                let refs = row.get("refs");
                oms_command_room::ConversationPanelAttempt {
                    room: refs.text("panel_room_id"),
                    participant: refs
                        .get("panel_room_participant")
                        .str()
                        .unwrap_or_else(|_| row.text("attempt_id")),
                    terminal: row.get("terminal").truth(),
                }
            });
        // Use only the chosen project. A process-scoped Room or previous chat
        // cannot supply the conversation identity for a per-request probe.
        if let Ok(room) = oms_command_room::Room::open(
            &ctx.repo,
            Default::default(),
            &oms_command_room::LocalServices,
            ctx.interrupted,
            false,
            false,
        ) && let Ok(mut binding) = room.conversation_main(consumer, panel_mains)
        {
            if let Some(harness) = conversation_harness(consumer, &binding, event_rows) {
                binding["conversationMain"].set("harness", harness);
            }
            if let Ok(encoded) = oms_command_room::value::encode(&binding, false, true)
                && let Ok(mut binding) = json::parse(&encoded)
            {
                let room = binding.get("conversationMain").text("room");
                if !room.is_empty() {
                    let (items, omitted) = panel_room_attempts(event_rows, room);
                    binding.set("attempts", Value::Array(items))?;
                    binding.set("attemptsOmitted", number(omitted))?;
                }
                state.set("conversation", binding)?;
            }
        }
    }
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
        .rev()
        .cloned()
        .collect();
    active_latest.sort_by(|a, b| {
        oms_command_operations::activity_rank(a.text("state"))
            .cmp(&oms_command_operations::activity_rank(b.text("state")))
            .then_with(|| b.text("updated_at").cmp(a.text("updated_at")))
    });
    active_latest.truncate(8);
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
            ("active_latest", Value::Array(active_latest)),
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
    let memory_review = match oms_command_memory::pending_review(&ctx.repo, ctx.interrupted) {
        Ok(summary) => object([
            ("healthy", Value::Bool(true)),
            ("pending", number(summary.pending)),
        ]),
        Err(_) => object([
            ("healthy", Value::Bool(false)),
            ("pending", Value::Null),
            ("error", string("status-unavailable")),
        ]),
    };
    state.set("memory_review", memory_review)?;
    let failure_rows = storage::rows(&ctx.repo, Path::new(".oms/failures.jsonl"));
    let repeat = failure_rows
        .as_ref()
        .ok()
        .and_then(|(rows, _)| failures::stall_findings(rows).into_iter().next())
        .map(|f| f.get("count").int().unwrap_or(0));
    let failure = failure_rows.and_then(|(rows, bad)| failures::project(&rows, bad, "", epoch()));
    let mut failure = collector(failure, ctx.repo.join(".oms/failures.jsonl").exists());
    if let Some(count) = repeat {
        failure.set("recent_repeat", object([("count", number(count))]))?;
    }
    state.set("failures", failure)?;
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
    let by_id = spine::group_runs(
        runs,
        |id, _| object([("run_id", string(id)), ("closed", Value::Bool(false))]),
        |run, row| {
            run.set("last", row.get("ts").clone())?;
            if row.text("tool") == "oms-run" && row.text("event") == "close" {
                run.set("closed", Value::Bool(true))?;
            }
            Ok(())
        },
    )?;
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
    projection::enrich(ctx, host, &mut state)?;
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

pub(crate) fn run(
    tool: &str,
    args: &[OsString],
    ctx: &Context<'_>,
    host: &dyn HostServices,
) -> Result<Output> {
    if tool == "state"
        && let Some(action @ ("gpu-status" | "gpu-wait")) = args.first().and_then(|v| v.to_str())
    {
        return crate::gpu::run(action, &args[1..], ctx);
    }
    if tool == "state"
        && let Some(action @ ("compact" | "archive")) = args.first().and_then(|v| v.to_str())
    {
        return crate::archive::run(action, &args[1..], ctx);
    }
    let (values, flags): (&[&str], &[&str]) = match tool {
        "state" => (&["repo"], &["json", "refresh-ci"]),
        "inbox" => (&["repo"], &["json", "fix-safe"]),
        "dashboard" => (
            &["repo", "interval", "count", "conversation-digest"],
            &["json", "watch", "panel-snapshot"],
        ),
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
    require(
        !opts.flag("panel-snapshot") || opts.flag("json"),
        "--panel-snapshot requires --json",
    )?;
    let digest = opts.get("conversation-digest");
    require(
        digest.is_empty()
            || (opts.flag("panel-snapshot")
                && digest.len() == 32
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
        "--conversation-digest requires a panel snapshot and 32 lowercase hex digits",
    )?;
    let consumer = (!digest.is_empty()).then_some(digest);
    let count = opts.number("count", if opts.flag("watch") { usize::MAX } else { 1 })?;
    let interval = opts.number("interval", 5)?;
    require(
        count > 0 && interval >= 2,
        "count must be positive and interval at least 2",
    )?;
    if opts.flag("refresh-ci") {
        let _ = projection::refresh_ci(&ctx);
    }
    let mut cached = if opts.flag("fix-safe") {
        Some(snapshot(&ctx, host)?)
    } else {
        None
    };
    let safe_actions = if let Some(state) = cached.as_ref() {
        projection::repairs(&ctx, host, state)?
    } else {
        vec![]
    };
    if safe_actions
        .iter()
        .any(|action| !action.str().unwrap_or("").ends_with("-failed"))
    {
        cached = None;
    }
    let mut out = Vec::new();
    let mut code = 0;
    for i in 0..count {
        crate::checkpoint(ctx.interrupted)?;
        let mut state = match cached.take() {
            Some(state) => state,
            None => collect_snapshot(&ctx, host, consumer)?,
        };
        if matches!(tool, "state" | "dashboard")
            && let Some(observed) = host.scratch_activity(&ctx.repo, ctx.interrupted)
        {
            state.set("managedScratch", observed)?;
        }
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
                projection::verify(&ctx, host, &state, &mut findings)?;
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
                let mut report = projection::dashboard(&ctx, &state)?;
                code = i32::from(!report.get("collection").get("ok").truth());
                let panel =
                    projection::panel_snapshot(&ctx.repo, &report, state.get("conversation"));
                if opts.flag("panel-snapshot") {
                    panel
                } else {
                    report.set(
                        "observation",
                        panel.get("dashboard").get("observation").clone(),
                    )?;
                    report
                }
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

#[cfg(test)]
mod panel_room_tests {
    use super::*;

    #[test]
    fn harness_requires_consistent_current_and_legacy_room_tags() {
        let binding = oms_command_room::value::parse(
            br#"{"conversationBinding":"matched","conversationMain":{"provider":"codex","source":"room-session","state":"joined","room":"work","participant":"owner"}}"#,
            1024,
        )
        .unwrap();
        let mut row = json::parse(br#"{"tool":"panel-main","attempt_id":"owner","terminal":false,"provider":"codex","refs":{"panel_owner":"codex","panel_harness":"pi","panel_role":"main","panel_room_id":"work"}}"#).unwrap();
        // Legacy-only rows cannot attest a native harness, but they can
        // invalidate uniqueness of another current Main in the same Room.
        let mut older = row.clone();
        let mut refs = older.get("refs").clone();
        refs.remove("panel_room_id");
        refs.set("panel_room", string("work")).unwrap();
        older.set("refs", refs).unwrap();
        older.set("attempt_id", string("earlier")).unwrap();
        assert_eq!(
            conversation_harness("test", &binding, std::slice::from_ref(&older)),
            None
        );
        assert_eq!(
            conversation_harness("test", &binding, &[row.clone(), older]),
            None
        );
        for (legacy, expected) in [
            (string("work"), Some("pi")),
            (string("other"), None),
            (Value::Null, None),
        ] {
            let mut refs = row.get("refs").clone();
            refs.set("panel_room", legacy).unwrap();
            row.set("refs", refs).unwrap();
            assert_eq!(
                conversation_harness("test", &binding, std::slice::from_ref(&row)),
                expected
            );
        }
    }

    #[test]
    fn attempt_window_keeps_the_room_and_only_explicit_unscoped_children() {
        let events = json::parse(
            br#"[
            {"attempt_id":"child","parent_attempt_id":"lead"},
            {"attempt_id":"foreign","parent_attempt_id":"lead","refs":{"panel_room_id":"outside"}},
            {"attempt_id":"other","parent_attempt_id":"foreign"},
            {"attempt_id":"legacy","refs":{"panel_room":"work","panel_role":"lead"}},
            {"attempt_id":"legacy-child","parent_attempt_id":"legacy","refs":{}},
            {"attempt_id":"both","refs":{"panel_room_id":"work","panel_room":"work"}},
            {"attempt_id":"legacy-foreign","parent_attempt_id":"lead","refs":{"panel_room":"outside"}},
            {"attempt_id":"conflict","parent_attempt_id":"lead","refs":{"panel_room_id":"work","panel_room":"outside"}},
            {"attempt_id":"bad","parent_attempt_id":"lead","refs":{"panel_room":null}},
            {"attempt_id":"invalid","parent_attempt_id":"lead","refs":{"panel_room_id":"../work"}},
            {"attempt_id":"bad-container","parent_attempt_id":"lead","refs":false},
            {"attempt_id":"empty","parent_attempt_id":"lead","refs":{"panel_room":""}},
            {"attempt_id":"lead","refs":{"panel_room_id":"work","panel_role":"lead"}},
            {"attempt_id":"main","tool":"panel-main","refs":{"panel_room_id":"work"}}
        ]"#,
        )
        .unwrap();
        let (rows, omitted) = panel_room_attempts(events.array().unwrap(), "work");
        let ids: Vec<_> = rows.iter().map(|row| row.text("attempt_id")).collect();
        assert_eq!(
            ids,
            ["legacy", "lead", "main", "child", "legacy-child", "both"]
        );
        assert_eq!(omitted, 0);

        let mut crowded: Vec<Value> = (0..24)
            .map(|i| {
                object([
                    ("attempt_id", string(format!("review-{i:02}"))),
                    ("state", string("review")),
                    ("terminal", Value::Bool(false)),
                    ("updated_at", string("2026-06-02T00:00:00Z")),
                    (
                        "refs",
                        object([
                            ("panel_room_id", string("work")),
                            ("panel_role", string("lead")),
                        ]),
                    ),
                ])
            })
            .collect();
        crowded.push(object([
            ("attempt_id", string("working-older")),
            ("state", string("working")),
            ("terminal", Value::Bool(false)),
            ("updated_at", string("2025-01-01T00:00:00Z")),
            ("refs", object([("panel_room_id", string("work"))])),
        ]));
        let (capped_rows, capped_omitted) = panel_room_attempts(&crowded, "work");
        assert_eq!(capped_rows.len(), 24);
        assert_eq!(capped_omitted, 1);
        assert_eq!(capped_rows[0].text("attempt_id"), "working-older");
    }
}
