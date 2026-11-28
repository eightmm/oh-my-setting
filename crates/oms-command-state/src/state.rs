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
    collect_snapshot(ctx, host, None, false).map(|(state, _)| state)
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
    include_dashboard: bool,
) -> Result<(Value, Option<projection::DashboardArtifacts>)> {
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
    let mut conversation_room = None;
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
        ) {
            if let Ok(mut binding) = room.conversation_main(consumer, panel_mains) {
                if let Some(harness) = conversation_harness(consumer, &binding, event_rows) {
                    binding["conversationMain"].set("harness", harness);
                }
                if let Ok(encoded) = oms_command_room::value::encode(&binding, false, true)
                    && let Ok(mut binding) = json::parse(&encoded)
                {
                    let room_id = binding.get("conversationMain").text("room");
                    if !room_id.is_empty() {
                        let (items, omitted) = panel_room_attempts(event_rows, room_id);
                        binding.set("attempts", Value::Array(items))?;
                        binding.set("attemptsOmitted", number(omitted))?;
                    }
                    state.set("conversation", binding)?;
                }
            }
            conversation_room = Some(room);
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
        .collect();
    // Stable sorting preserves reverse traversal order for equal rank and timestamp.
    active_latest.sort_by(|a, b| {
        oms_command_operations::activity_rank(a.text("state"))
            .cmp(&oms_command_operations::activity_rank(b.text("state")))
            .then_with(|| b.text("updated_at").cmp(a.text("updated_at")))
    });
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
                Value::Array(active_latest.into_iter().take(8).cloned().collect()),
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
    let memory_review = match oms_command_memory::pending_review(&ctx.repo, ctx.interrupted) {
        Ok(summary) => object([
            ("healthy", Value::Bool(summary.skipped == 0)),
            (
                "status",
                string(if summary.skipped == 0 {
                    "ok"
                } else {
                    "partial"
                }),
            ),
            ("pending", number(summary.pending)),
            ("skipped", number(summary.skipped)),
        ]),
        Err(_) => object([
            ("healthy", Value::Bool(false)),
            ("status", string("unavailable")),
            ("pending", Value::Null),
            ("skipped", Value::Null),
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
    let artifacts = projection::enrich(
        ctx,
        host,
        &mut state,
        include_dashboard,
        conversation_room.as_ref(),
    )?;
    Ok((state, artifacts))
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
        let (mut state, artifacts) = match cached.take() {
            Some(state) => (state, None),
            None => {
                let _scope = if tool == "dashboard" && opts.flag("panel-snapshot") {
                    host.begin_panel_snapshot()
                } else {
                    None
                };
                collect_snapshot(&ctx, host, consumer, tool == "dashboard")?
            }
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
                let artifacts =
                    artifacts.ok_or_else(|| error(2, "dashboard artifact observation missing"))?;
                let mut report = projection::dashboard(&ctx, &state, artifacts)?;
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

    #[test]
    fn collect_snapshot_reuses_active_room_logs_and_falls_back_safely() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;

        let init_repo = |path: &Path| {
            std::fs::create_dir_all(path).unwrap();
            let path = path.canonicalize().unwrap();
            let ok = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&path)
                .status()
                .unwrap()
                .success();
            assert!(ok);
            path
        };
        let threads_without_age = |state: &Value| {
            let mut threads = state.get("threads").clone();
            let mut recent = threads.get("recent").clone();
            for thread in recent.array_mut().unwrap() {
                assert!(thread.remove("age_seconds").unwrap().int().is_ok());
            }
            threads.set("recent", recent).unwrap();
            threads
        };
        let enrich_state = || {
            object([
                ("schema", number(1)),
                ("task", json::empty()),
                ("plan", json::empty()),
                ("failures", json::empty()),
            ])
        };

        let root = std::env::temp_dir().join(crate::random_id("oms-state-room-reuse-").unwrap());
        std::fs::create_dir_all(&root).unwrap();
        let result = std::panic::catch_unwind(|| {
            let stop = AtomicBool::new(false);
            let repo = init_repo(&root.join("repo"));
            let foreign = init_repo(&root.join("foreign"));

            let writer = oms_command_room::Room::open(
                &repo,
                Default::default(),
                &oms_command_room::LocalServices,
                &stop,
                true,
                false,
            )
            .unwrap();
            let create_bound = |id: &str| {
                writer
                    .create(Some(id), &format!("Title {id}"), None, Some("owner"), None)
                    .unwrap();
                let mut join = oms_command_room::Join::new("owner", "codex");
                join.native_session = Some(id.to_owned());
                writer.join(id, join).unwrap();
                format!("{:x}", Sha256::digest(id.as_bytes()))[..32].to_owned()
            };
            let digest = create_bound("room-open-bound");
            create_bound("room-open-other");
            create_bound("room-closed");
            writer.close("room-closed", "owner").unwrap();

            // Sealed Room falls back to state read and is excluded as closed/successor.
            writer
                .create(Some("room-sealed"), "Sealed", None, Some("owner"), None)
                .unwrap();
            let sealed_path = repo.join(".oms/threads/room-sealed.jsonl");
            let mut sealed_bytes = std::fs::read(&sealed_path).unwrap();
            sealed_bytes.extend_from_slice(
                b"{\"schema\":1,\"ts\":\"2026-11-27T00:00:00Z\",\"thread\":\"room-sealed\",\"seq\":3,\"role\":\"note\",\"agent\":\"agent\",\"text\":\"Rolled over\",\"room_event\":{\"kind\":\"successor\",\"target\":\"room-open-bound\"}}\n",
            );
            std::fs::write(&sealed_path, sealed_bytes).unwrap();
            std::fs::set_permissions(&sealed_path, std::fs::Permissions::from_mode(0o600)).unwrap();

            // Legacy non-Room thread without trailing newline + stale timestamp + provider.
            let plain_path = repo.join(".oms/threads/plain-legacy.jsonl");
            std::fs::write(
                &plain_path,
                br#"{"schema":1,"ts":"2020-01-01T00:00:00Z","thread":"plain-legacy","seq":1,"role":"note","provider":"claude","text":"legacy"}"#,
            )
            .unwrap();
            std::fs::set_permissions(&plain_path, std::fs::Permissions::from_mode(0o600)).unwrap();

            // Malformed thread falls back and is skipped when bad > 0.
            let malformed_path = repo.join(".oms/threads/bad-thread.jsonl");
            std::fs::write(&malformed_path, b"{bad\n").unwrap();
            std::fs::set_permissions(&malformed_path, std::fs::Permissions::from_mode(0o600))
                .unwrap();

            let current_path = repo.join(".oms/threads/CURRENT");
            std::fs::write(&current_path, b"room-open-bound\n").unwrap();
            std::fs::set_permissions(&current_path, std::fs::Permissions::from_mode(0o600))
                .unwrap();

            let ctx = Context {
                runtime: None,
                cwd: repo.clone(),
                repo: repo.clone(),
                state_repo: repo.clone(),
                interrupted: &stop,
            };
            projection::reset_test_thread_io_counters();
            let (with_reuse, _) =
                collect_snapshot(&ctx, &crate::NoHostServices, Some(&digest), true).unwrap();
            assert_eq!(projection::test_thread_io_counts(), (3, 3));

            projection::reset_test_thread_io_counters();
            let (without_reuse, _) =
                collect_snapshot(&ctx, &crate::NoHostServices, None, true).unwrap();
            assert_eq!(projection::test_thread_io_counts(), (0, 6));
            assert_eq!(
                threads_without_age(&with_reuse),
                threads_without_age(&without_reuse)
            );
            // The generic thread summary excludes the legacy closed role, while
            // a Room close is recorded as a Room event on a note row.
            assert_eq!(with_reuse.get("threads").get("open").int().unwrap(), 4);

            // Rejection & fallback checks with a pre-warmed Room between conversation_main and enrich.
            let cached_room = oms_command_room::Room::open(
                &repo,
                Default::default(),
                &oms_command_room::LocalServices,
                &stop,
                false,
                false,
            )
            .unwrap();
            cached_room.conversation_main(&digest, []).unwrap();

            // Append to room-open-bound and atomically replace room-open-other.
            writer
                .send(
                    "room-open-bound",
                    oms_command_room::Send::note("owner", "all", "updated"),
                )
                .unwrap();
            let other_path = repo.join(".oms/threads/room-open-other.jsonl");
            let mut other_bytes = std::fs::read(&other_path).unwrap();
            other_bytes.extend_from_slice(
                format!(
                    "{{\"schema\":1,\"ts\":\"{}\",\"thread\":\"room-open-other\",\"seq\":4,\"role\":\"note\",\"provider\":\"gemini\",\"text\":\"replaced\"}}\n",
                    timestamp(epoch()).unwrap()
                )
                .as_bytes(),
            );
            let tmp_other = repo.join(".oms/threads/.tmp-room-open-other");
            std::fs::write(&tmp_other, &other_bytes).unwrap();
            std::fs::set_permissions(&tmp_other, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::rename(&tmp_other, &other_path).unwrap();

            let mut mutated_state = enrich_state();
            projection::reset_test_thread_io_counters();
            projection::enrich(
                &ctx,
                &crate::NoHostServices,
                &mut mutated_state,
                false,
                Some(&cached_room),
            )
            .unwrap();
            // Only room-closed stays reusable; room-open-bound + room-open-other + 3 non-active fall back.
            assert_eq!(projection::test_thread_io_counts(), (1, 5));
            let other_turns = mutated_state
                .get("threads")
                .get("recent")
                .array()
                .unwrap()
                .iter()
                .find(|r| r.text("id") == "room-open-other")
                .unwrap()
                .get("turns")
                .int()
                .unwrap();
            assert_eq!(other_turns, 4);

            // Foreign Room repository is rejected by canonical anchor check.
            let foreign_writer = oms_command_room::Room::open(
                &foreign,
                Default::default(),
                &oms_command_room::LocalServices,
                &stop,
                true,
                false,
            )
            .unwrap();
            foreign_writer
                .create(
                    Some("room-open-bound"),
                    "Foreign",
                    None,
                    Some("owner"),
                    None,
                )
                .unwrap();
            foreign_writer.discover().unwrap();
            let mut foreign_state = enrich_state();
            projection::reset_test_thread_io_counters();
            projection::enrich(
                &ctx,
                &crate::NoHostServices,
                &mut foreign_state,
                false,
                Some(&foreign_writer),
            )
            .unwrap();
            assert_eq!(projection::test_thread_io_counts(), (0, 6));

            // Hardlinking a cached active Room forces fallback and preserves single-link refusal.
            let fresh_room = oms_command_room::Room::open(
                &repo,
                Default::default(),
                &oms_command_room::LocalServices,
                &stop,
                false,
                false,
            )
            .unwrap();
            fresh_room.conversation_main(&digest, []).unwrap();
            let hard = repo.join(".oms/threads/zzz-hard-link.jsonl");
            std::fs::hard_link(repo.join(".oms/threads/room-open-bound.jsonl"), &hard).unwrap();
            let mut linked_state = enrich_state();
            projection::reset_test_thread_io_counters();
            assert!(
                projection::enrich(
                    &ctx,
                    &crate::NoHostServices,
                    &mut linked_state,
                    false,
                    Some(&fresh_room),
                )
                .is_err()
            );
            // bad-thread & plain-legacy & room-closed run before room-open-bound, which
            // must reject reuse due to st_nlink == 2 and fail in fallback storage::rows.
            assert_eq!(projection::test_thread_io_counts(), (1, 3));
            std::fs::remove_file(&hard).unwrap();

            // Incomplete MAX_ROOMS + CURRENT fallback: 65 rooms are all cached and reused.
            let crowded_repo = init_repo(&root.join("crowded"));
            let crowded_writer = oms_command_room::Room::open(
                &crowded_repo,
                Default::default(),
                &oms_command_room::LocalServices,
                &stop,
                true,
                false,
            )
            .unwrap();
            let mut all_ids = Vec::new();
            for i in 0..=oms_command_room::MAX_ROOMS {
                let id = format!("room-{i:03}");
                crowded_writer
                    .create(
                        Some(&id),
                        &format!("Crowded {i}"),
                        None,
                        Some("owner"),
                        None,
                    )
                    .unwrap();
                let mut join = oms_command_room::Join::new("owner", "codex");
                join.native_session = Some(id.clone());
                crowded_writer.join(&id, join).unwrap();
                all_ids.push(id);
            }
            let crowded_current = crowded_repo.join(".oms/threads/CURRENT");
            assert!(crowded_current.is_file());
            let (discovered, incomplete) = crowded_writer.discover().unwrap();
            assert!(incomplete);
            let omitted = all_ids
                .into_iter()
                .find(|id| !discovered.iter().any(|r| r.text("id") == id))
                .expect("at least one room omitted past MAX_ROOMS");
            let target_digest =
                format!("{:x}", Sha256::digest(omitted.as_bytes()))[..32].to_owned();
            std::fs::write(&crowded_current, format!("{omitted}\n")).unwrap();
            std::fs::set_permissions(&crowded_current, std::fs::Permissions::from_mode(0o600))
                .unwrap();

            let crowded_ctx = Context {
                runtime: None,
                cwd: crowded_repo.clone(),
                repo: crowded_repo.clone(),
                state_repo: crowded_repo.clone(),
                interrupted: &stop,
            };
            projection::reset_test_thread_io_counters();
            let (crowded_reuse, _) = collect_snapshot(
                &crowded_ctx,
                &crate::NoHostServices,
                Some(&target_digest),
                true,
            )
            .unwrap();
            let binding = crowded_reuse.get("conversation");
            assert_eq!(binding.text("conversationBinding"), "matched");
            assert_eq!(binding.get("conversationMain").text("room"), omitted);
            assert!(binding.get("conversationMain").get("incomplete").truth());
            assert_eq!(
                projection::test_thread_io_counts(),
                (oms_command_room::MAX_ROOMS + 1, 0)
            );
            projection::reset_test_thread_io_counters();
            let (crowded_plain, _) =
                collect_snapshot(&crowded_ctx, &crate::NoHostServices, None, true).unwrap();
            assert_eq!(
                projection::test_thread_io_counts(),
                (0, oms_command_room::MAX_ROOMS + 1)
            );
            assert_eq!(
                threads_without_age(&crowded_reuse),
                threads_without_age(&crowded_plain)
            );

            // Concurrent council appends and CURRENT swaps neither fail state/verify
            // nor count an active torn trailing line as a bad row.
            let race_repo = init_repo(&root.join("race"));
            let race_threads = race_repo.join(".oms/threads");
            std::fs::create_dir_all(&race_threads).unwrap();
            std::fs::set_permissions(&race_threads, std::fs::Permissions::from_mode(0o700))
                .unwrap();
            std::fs::write(race_repo.join(".oms/.gitignore"), b"*\n").unwrap();
            let council_id = "panel-council-0123456789abcdef01234567";
            let council_path = race_threads.join(format!("{council_id}.jsonl"));
            let now_ts = timestamp(epoch()).unwrap();
            std::fs::write(
                &council_path,
                format!(
                    "{{\"schema\":1,\"ts\":\"{now_ts}\",\"thread\":\"{council_id}\",\"seq\":1,\"role\":\"answer\",\"provider\":\"claude\",\"text\":\"pos-0\"}}\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&council_path, std::fs::Permissions::from_mode(0o600))
                .unwrap();
            let other_log = race_threads.join("room-stable.jsonl");
            std::fs::write(
                &other_log,
                format!(
                    "{{\"schema\":1,\"ts\":\"{now_ts}\",\"thread\":\"room-stable\",\"seq\":1,\"role\":\"note\",\"provider\":\"codex\",\"text\":\"stable\"}}\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&other_log, std::fs::Permissions::from_mode(0o600)).unwrap();
            let race_current = race_threads.join("CURRENT");
            std::fs::write(&race_current, format!("{council_id}\n")).unwrap();
            std::fs::set_permissions(&race_current, std::fs::Permissions::from_mode(0o600))
                .unwrap();
            let race_ctx = Context {
                runtime: None,
                cwd: race_repo.clone(),
                repo: race_repo.clone(),
                state_repo: race_repo.clone(),
                interrupted: &stop,
            };
            let done = AtomicBool::new(false);
            let barrier = std::sync::Barrier::new(3);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&council_path)
                        .unwrap();
                    barrier.wait();
                    for seq in 2..=25 {
                        file.write_all(
                            format!(
                                "{{\"schema\":1,\"ts\":\"{now_ts}\",\"thread\":\"{council_id}\",\"seq\":{seq},\"role\":\"answer\",\"provider\":\"codex\",\"text\":\""
                            )
                            .as_bytes(),
                        )
                        .unwrap();
                        file.flush().unwrap();
                        if seq == 2 {
                            std::thread::sleep(std::time::Duration::from_millis(12));
                        }
                        file.write_all(format!("pos-{seq}\"}}\n").as_bytes())
                            .unwrap();
                        file.flush().unwrap();
                    }
                    done.store(true, std::sync::atomic::Ordering::Release);
                });
                scope.spawn(|| {
                    barrier.wait();
                    let mut step = 0usize;
                    while !done.load(std::sync::atomic::Ordering::Acquire) {
                        let target = if step.is_multiple_of(2) {
                            council_id
                        } else {
                            "room-stable"
                        };
                        storage::replace_locked(
                            &race_repo,
                            Path::new(".oms/threads/CURRENT"),
                            format!("{target}\n").as_bytes(),
                        )
                        .unwrap();
                        step += 1;
                    }
                });
                scope.spawn(|| {
                    barrier.wait();
                    let mut prev_turns = 1i128;
                    let mut reads = 0usize;
                    while !done.load(std::sync::atomic::Ordering::Acquire) || reads < 16 {
                        let mut snap = enrich_state();
                        projection::enrich(
                            &race_ctx,
                            &crate::NoHostServices,
                            &mut snap,
                            false,
                            None,
                        )
                        .unwrap();
                        assert_eq!(snap.get("threads").get("open").int().unwrap(), 2);
                        let turns = snap
                            .get("threads")
                            .get("recent")
                            .array()
                            .unwrap()
                            .iter()
                            .find(|r| r.text("id") == council_id)
                            .expect("council thread stays visible during append")
                            .get("turns")
                            .int()
                            .unwrap();
                        assert!(turns >= prev_turns, "{turns} < {prev_turns}");
                        prev_turns = turns;
                        let mut findings = Vec::new();
                        projection::verify(&race_ctx, &crate::NoHostServices, &snap, &mut findings)
                            .unwrap();
                        assert!(findings.is_empty(), "{findings:?}");
                        reads += 1;
                    }
                    assert_eq!(prev_turns, 25);
                });
            });
            {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&council_path)
                    .unwrap();
                file.write_all(format!("{{\"schema\":1,\"thread\":\"{council_id}\"").as_bytes())
                    .unwrap();
                file.flush().unwrap();
            }
            let mut crashed_state = enrich_state();
            projection::enrich(
                &race_ctx,
                &crate::NoHostServices,
                &mut crashed_state,
                false,
                None,
            )
            .unwrap();
            assert_eq!(crashed_state.get("threads").get("open").int().unwrap(), 1);
        });
        let _ = std::fs::remove_dir_all(root);
        result.unwrap();
    }
}
