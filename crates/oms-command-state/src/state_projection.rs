//! Cross-family read projections and the explicitly bounded mechanical repair set.
use super::*;
use crate::fleet;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

fn rows(ctx: &Context<'_>, path: &str) -> (Vec<Value>, usize) {
    storage::rows(&ctx.repo, Path::new(path)).unwrap_or_else(|_| (vec![], 1))
}
fn file(ctx: &Context<'_>, path: &str) -> Result<Value> {
    let raw = storage::read(&ctx.repo, Path::new(path), storage::MAX_FILE, true)?;
    if raw.is_empty() {
        Ok(json::empty())
    } else {
        json::parse(&raw)
    }
}
fn query(ctx: &Context<'_>, argv: &[&str]) -> String {
    helpers::query(ctx, argv)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
        .unwrap_or_default()
}
pub(super) fn alive(row: &Value) -> Option<bool> {
    let raw = row.get("pid");
    let pid = raw
        .int()
        .ok()
        .or_else(|| raw.str().ok().and_then(|s| s.parse().ok()))?;
    let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
    let pid = rustix::process::Pid::from_raw(pid)?;
    match rustix::process::test_kill_process(pid) {
        Ok(()) => Some(true),
        Err(rustix::io::Errno::SRCH) => Some(false),
        Err(_) => None,
    }
}
fn json_files(ctx: &Context<'_>, folder: &str) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for path in helpers::shallow_files(ctx, &ctx.repo.join(folder), ".json", 50000)? {
        result.push(json::parse(&storage::read(
            &ctx.repo,
            &path,
            storage::MAX_FILE,
            false,
        )?)?);
    }
    Ok(result)
}
fn artifact_rows(ctx: &Context<'_>) -> Result<(Vec<Value>, BTreeSet<String>, usize)> {
    use oms_command_telemetry::store::resolution::{Exit, Row, project};
    let (rows, mut bad) = rows(ctx, ".oms/artifacts/index.jsonl");
    let mut counts = HashMap::new();
    for row in &rows {
        *counts.entry(row.text("event_id")).or_insert(0) += 1;
    }
    let valid = |row: &Value| {
        !row.get("schema").is_int(1) && row.text("kind") != "artifact-resolution"
            || row.get("schema").is_int(1)
                && ["event_id", "operation_id", "artifact_id", "ts", "kind"]
                    .iter()
                    .all(|k| !row.text(k).is_empty())
                && row.get("provider").str().is_ok()
                && row.get("exit").int().is_ok_and(|n| n >= 0)
                && counts.get(row.text("event_id")) == Some(&1)
    };
    let views = rows
        .iter()
        .map(|row| Row {
            event_id: row.get("event_id").str().ok().map(str::as_bytes),
            schema_one: row.get("schema").is_int(1) && valid(row),
            is_resolution: row.text("kind") == "artifact-resolution",
            exit: match row.get("exit").int() {
                Ok(0) => Exit::Success,
                Ok(n) if n > 0 => Exit::Failure,
                _ => Exit::Invalid,
            },
            resolves_event_id: row.get("resolves_event_id").str().ok().map(str::as_bytes),
            parent_event_id: row.get("parent_event_id").str().ok().map(str::as_bytes),
            resolved: row.text("resolution") == "resolved",
        })
        .collect::<Vec<_>>();
    let projection = project(&views, |target, resolver| {
        rows[target].text("operation_id") == rows[resolver].text("operation_id")
            && rows[target].text("artifact_id") == rows[resolver].text("artifact_id")
    });
    bad += rows
        .iter()
        .enumerate()
        .filter(|(pos, row)| {
            !valid(row)
                || row.text("kind") == "artifact-resolution"
                    && !projection.accepted_resolution_rows.contains(pos)
        })
        .count();
    let resolved = projection
        .resolved_event_ids
        .into_iter()
        .filter_map(|id| std::str::from_utf8(id).ok().map(str::to_owned))
        .collect();
    Ok((rows, resolved, bad))
}
pub(super) struct DashboardArtifacts {
    operations: Value,
    reviews: Value,
}

fn dashboard_artifacts(
    rows: &[Value],
    outcomes_total: usize,
    bad: usize,
) -> Result<DashboardArtifacts> {
    let mut reviews = object([
        ("outcomes", number(0)),
        ("passed", number(0)),
        ("failed", number(0)),
        ("unknown", number(0)),
        ("seat_answers", number(0)),
    ]);
    for row in rows {
        let key = if row.text("kind") == "review" {
            "seat_answers"
        } else if row.text("kind") == "review-outcome" {
            "outcomes"
        } else {
            continue;
        };
        reviews.set(key, number(reviews.get(key).int()? + 1))?;
        if key == "outcomes" {
            let key = match row.get("exit").int() {
                Ok(0) => "passed",
                Ok(n) if n > 0 => "failed",
                _ => "unknown",
            };
            reviews.set(key, number(reviews.get(key).int()? + 1))?;
        }
    }
    let operations = object([
        ("available", Value::Bool(bad == 0)),
        (
            "recent",
            Value::Array(
                rows.iter()
                    .rev()
                    .filter(|r| r.text("kind") != "artifact-resolution")
                    .take(8)
                    .cloned()
                    .collect(),
            ),
        ),
        ("omitted", number(outcomes_total.saturating_sub(8))),
    ]);
    Ok(DashboardArtifacts {
        operations,
        reviews,
    })
}

fn project_artifacts(
    ctx: &Context<'_>,
    state: &mut Value,
    include_dashboard: bool,
) -> Result<Option<DashboardArtifacts>> {
    let (rows, resolved, bad) = artifact_rows(ctx)?;
    let outcomes = rows
        .iter()
        .filter(|r| r.text("kind") != "artifact-resolution");
    let status = |row: &Value| {
        if row.get("exit").is_int(0) {
            "success"
        } else if resolved.contains(row.text("event_id")) {
            "resolved"
        } else {
            "unresolved"
        }
    };
    let mut counts = object([
        ("success", number(0)),
        ("resolved", number(0)),
        ("unresolved", number(0)),
    ]);
    let mut outcomes_total = 0usize;
    for row in outcomes.clone() {
        let key = status(row);
        counts.set(key, number(counts.get(key).int()? + 1))?;
        outcomes_total += 1;
    }
    let latest = outcomes
        .rev()
        .take(5)
        .map(|row| {
            let mut projected = row.clone();
            projected.set("status", string(status(row)))?;
            Ok(projected)
        })
        .collect::<Result<Vec<_>>>()?;
    state.set(
        "artifacts",
        object([
            ("healthy", Value::Bool(bad == 0)),
            ("invalid_rows", number(bad)),
            ("total", number(rows.len())),
            ("outcomes_total", number(outcomes_total)),
            ("resolution_events", number(resolved.len())),
            ("counts", counts),
            ("latest", Value::Array(latest)),
        ]),
    )?;
    include_dashboard
        .then(|| dashboard_artifacts(&rows, outcomes_total, bad))
        .transpose()
}

const STATE_ACTIONABLE_MAX: usize = 5;

fn format_actionable(ids: &[&str]) -> String {
    if ids.len() <= STATE_ACTIONABLE_MAX {
        format!("{} ({})", ids.len(), ids.join(", "))
    } else {
        format!(
            "{} ({} +{} more)",
            ids.len(),
            ids[..STATE_ACTIONABLE_MAX].join(", "),
            ids.len() - STATE_ACTIONABLE_MAX
        )
    }
}

fn plan_updated_at(plan: &Value, plan_path: &Path) -> Option<String> {
    fn consider<'a>(best: &mut Option<(Option<i64>, &'a str)>, ts: &'a str) {
        let ts = ts.trim();
        if ts.is_empty() {
            return;
        }
        let key = (crate::parse_timestamp(ts), ts);
        if best.is_none_or(|cur| key > cur) {
            *best = Some(key);
        }
    }
    let mut best: Option<(Option<i64>, &str)> = None;
    consider(&mut best, plan.text("updated_at"));
    consider(&mut best, plan.text("updated"));
    if let Ok(entries) = plan.get("tasks").object() {
        for (_, t) in entries {
            let updated = t.text("updated");
            if !updated.is_empty() {
                consider(&mut best, updated);
            } else {
                consider(&mut best, t.text("created"));
            }
        }
    } else if let Ok(entries) = plan.get("tasks").array() {
        for t in entries {
            let updated = t.text("updated");
            if !updated.is_empty() {
                consider(&mut best, updated);
            } else {
                consider(&mut best, t.text("created"));
            }
        }
    }
    if let Some((_, ts)) = best {
        return Some(ts.to_owned());
    }
    let mtime = std::fs::metadata(plan_path)
        .and_then(|m| m.modified())
        .ok()?;
    Some(
        chrono::DateTime::<chrono::Utc>::from(mtime)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    )
}

/// Join only current Room assignments to the same Plan bytes validated by status.
fn conversation_task_titles(state: &Value, raw: &[u8], plan: &Value) -> Option<Value> {
    let conversation = state.get("conversation");
    let main = conversation.get("conversationMain");
    let room = main.text("room");
    let owner = main.text("participant");
    let status = state.get("plan");
    let plan_id = plan.text("plan_id");
    if conversation.text("conversationBinding") != "matched"
        || main.text("state") != "joined"
        || main.text("provider").is_empty()
        || room.is_empty()
        || owner.is_empty()
        || plan_id.is_empty()
        || !matches!(status.get("healthy"), Value::Bool(true))
    {
        return None;
    }
    let rows = conversation.get("attempts").array().ok()?;
    let members = main.get("participants").array().ok()?;
    let mut owners = rows.iter().filter(|row| {
        let refs = row.get("refs");
        let id = row.text("attempt_id");
        row.text("tool") == "panel-main"
            && !id.is_empty()
            && matches!(row.get("terminal"), Value::Bool(false))
            && row.text("provider") == main.text("provider")
            && attempt_room(row) == AttemptRoom::Named(room)
            && refs.text("panel_role") == "main"
            && refs
                .opt("panel_room_participant")
                .map_or(id, |v| v.str().unwrap_or(""))
                == owner
    });
    let main_attempt = owners.next()?;
    let main_id = main_attempt.text("attempt_id");
    if owners.next().is_some() {
        return None;
    }
    let mut titles = json::empty();
    for row in rows {
        let refs = row.get("refs");
        let id = row.text("attempt_id");
        let task_id = row.text("task_id");
        let task = plan.get("tasks").get(task_id);
        let lease = refs.text("lease_id");
        if id.is_empty()
            || task_id.is_empty()
            || task.text("id") != task_id
            || refs.text("plan_id") != plan_id
            || lease.is_empty()
            || task.text("lease_id") != lease
            || attempt_room(row) != AttemptRoom::Named(room)
            || [row.text("state"), task.text("state")]
                .iter()
                .any(|s| matches!(*s, "cancelled" | "canceled"))
        {
            continue;
        }
        let participant = if std::ptr::eq(row, main_attempt) {
            owner
        } else {
            let participant = if row.text("tool") == "panel-native-assignment" {
                if row.text("provider") != "codex"
                    || refs.text("native_assignment_schema") != "1"
                    || refs
                        .opt("panel_room_participant")
                        .is_some_and(|v| v.str().ok() != Some(id))
                    || refs
                        .opt("panel_role")
                        .is_some_and(|v| v.str().ok() != Some("lead"))
                {
                    continue;
                }
                id
            } else if refs.text("panel_role") == "lead" {
                refs.text("panel_room_participant")
            } else {
                continue;
            };
            if participant.is_empty()
                || participant == owner
                || row.text("parent_attempt_id") != main_id
                || !members.iter().any(|m| {
                    m.text("room") == room
                        && m.text("parent") == owner
                        && m.text("participant") == participant
                        && m.text("provider") == row.text("provider")
                        && matches!(m.get("joined"), Value::Bool(true))
                        && matches!(m.text("role"), "worker" | "lead")
                })
            {
                continue;
            }
            participant
        };
        let title = task.text("title");
        if ![owner, participant].contains(&task.text("claimed_by_participant"))
            || title.len() > 640
            || title.trim().is_empty()
            || title == task_id
            || title.chars().count() > 160
            || title.chars().any(|c| {
                c.is_control() || matches!(c, '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
            || !oms_command_room::outbound_clean(title.as_bytes()).unwrap_or(false)
            || !json::encoded(task.get("title"))
                .ok()
                .is_some_and(|b| oms_command_room::outbound_clean(&b).unwrap_or(false))
        {
            continue;
        }
        titles.set(id, string(title)).ok()?;
    }
    (!titles.object().ok()?.is_empty() && status.text("plan_sha256") == json::sha(raw))
        .then_some(titles)
}

#[cfg(test)]
std::thread_local! {
    static TEST_THREAD_REUSE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TEST_THREAD_FALLBACK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_test_thread_io_counters() {
    TEST_THREAD_REUSE.with(|c| c.set(0));
    TEST_THREAD_FALLBACK.with(|c| c.set(0));
}

#[cfg(test)]
pub(super) fn test_thread_io_counts() -> (usize, usize) {
    (
        TEST_THREAD_REUSE.with(std::cell::Cell::get),
        TEST_THREAD_FALLBACK.with(std::cell::Cell::get),
    )
}

fn thread_rows(
    repo: &Path,
    threads_dir: &Path,
    path: &Path,
    room: Option<&oms_command_room::Room<'_>>,
) -> Result<(Vec<Value>, usize)> {
    if let Some(room) = room
        && path.parent() == Some(threads_dir)
        && let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".jsonl"))
        && let Some(snapshot) = room.reusable_snapshot(id)
        && snapshot.bytes.len() <= storage::MAX_STREAM
    {
        #[cfg(test)]
        TEST_THREAD_REUSE.with(|c| c.set(c.get() + 1));
        return Ok(storage::parse_rows(&snapshot.bytes));
    }
    #[cfg(test)]
    TEST_THREAD_FALLBACK.with(|c| c.set(c.get() + 1));
    storage::thread_rows(repo, path)
}

pub(super) fn enrich(
    ctx: &Context<'_>,
    host: &dyn HostServices,
    state: &mut Value,
    include_dashboard: bool,
    room: Option<&oms_command_room::Room<'_>>,
) -> Result<Option<DashboardArtifacts>> {
    let task_read = storage::read(
        &ctx.repo,
        Path::new(".oms/task/current.md"),
        storage::MAX_FILE,
        true,
    );
    let packet_ok = task_read.is_ok();
    let task_text = task_read.unwrap_or_default();
    let mut section = "";
    let mut goal = Vec::new();
    let mut next = Vec::new();
    for line in std::str::from_utf8(&task_text).unwrap_or("").lines() {
        if line.starts_with("## ") {
            section = line;
            continue;
        }
        if !line.trim().is_empty() {
            match section {
                "## Goal" => goal.push(line.trim()),
                "## Next Step" => next.push(line.trim()),
                _ => {}
            }
        }
    }
    let mut task = state.get("task").clone();
    let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
    let open = task.get("present").truth() && task.text("status") != "closed";
    let (is_stale, is_dormant, idle_secs) = if !task_text.is_empty() {
        let f = oms_command_task::packet::freshness_from_env(&task_text, now);
        (open && f.stale, open && f.dormant, f.idle_seconds)
    } else {
        let idle = helpers::now_age(task.text("last_activity")).and_then(|s| u64::try_from(s).ok());
        let stale = open && task.get("stale").truth();
        let dormant = open
            && !stale
            && (task.get("dormant").truth()
                || idle
                    .is_some_and(|age| age >= oms_command_task::packet::DORMANT_THRESHOLD_SECONDS));
        (stale, dormant, idle)
    };
    if task.get("present").truth() {
        let eligible = open
            && packet_ok
            && matches!(task.get("current_goal_eligible"), Value::Bool(true))
            && oms_command_task::packet::current_goal_eligible(
                &task_text,
                oms_command_task::packet::PacketFreshness {
                    idle_seconds: idle_secs,
                    stale: is_stale,
                    dormant: is_dormant,
                },
            )
            && oms_command_task::packet::metadata(&task_text, "task_id").unwrap_or(b"legacy")
                == task.text("task_id").as_bytes();
        task.set("current_goal_eligible", Value::Bool(eligible))?;
        task.set("stale", Value::Bool(is_stale))?;
        task.set("dormant", Value::Bool(is_dormant))?;
        if let Some(secs) = idle_secs {
            task.set("idle_seconds", number(secs))?;
        }
    }
    task.set(
        "goal",
        string(goal.join(" ").chars().take(200).collect::<String>()),
    )?;
    task.set(
        "next",
        string(next.join(" ").chars().take(200).collect::<String>()),
    )?;
    state.set("task", task)?;
    let plan_path = Path::new(".oms/plan/tasks.json");
    let mut projected = state.get("plan").clone();
    if let Ok(raw) = storage::read(&ctx.repo, plan_path, storage::MAX_FILE, true)
        && let Ok(plan) = if raw.is_empty() {
            Ok(json::empty())
        } else {
            json::parse(&raw)
        }
    {
        if let Some(titles) = conversation_task_titles(state, &raw, &plan)
            && let Some(mut conversation) = state.remove("conversation")
        {
            conversation.set("taskTitles", titles)?;
            state.set("conversation", conversation)?;
        }
        projected.set("goal", plan.get("goal").clone())?;
        if let Some(updated_at) = plan_updated_at(&plan, &ctx.repo.join(plan_path)) {
            projected.set("updated_at", string(updated_at))?;
        }
    }
    let (retirements, _) = rows(ctx, ".oms/plan/retirements.jsonl");
    if let Some(last) = retirements.last() {
        projected.set("latest_retirement", last.clone())?;
    }
    state.set("plan", projected)?;
    let (experiments, bad) = rows(ctx, ".oms/experiments.jsonl");
    let mut current: BTreeMap<String, Value> = BTreeMap::new();
    for row in &experiments {
        let id = row.text("id");
        if id.is_empty() {
            continue;
        }
        let entry = current.entry(id.into()).or_insert_with(json::empty);
        for (k, v) in row.object()? {
            let key = std::str::from_utf8(k).unwrap_or("");
            if matches!(v, Value::Null)
                || v.str() == Ok("")
                || key == "owner" && entry.opt("owner").is_some() && row.text("status") != "claimed"
            {
                continue;
            }
            entry.set(key, v.clone())?;
        }
    }
    let active = current
        .values()
        .filter(|r| !["done", "finished", "aborted"].contains(&r.text("status")))
        .cloned()
        .collect::<Vec<_>>();
    let ttl = std::env::var("OMS_EXPERIMENT_CLAIM_TTL")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(3600);
    let stale = active
        .iter()
        .filter(|r| {
            r.text("status") == "claimed"
                && helpers::now_age(r.text("ts")).is_some_and(|a| a >= ttl)
        })
        .cloned()
        .collect();
    let board = object([
        ("healthy", Value::Bool(bad == 0)),
        ("present", Value::Bool(!experiments.is_empty())),
        ("active", Value::Array(active)),
        ("stale", Value::Array(stale)),
    ]);
    state.set("board", board.clone())?;
    state.set("experiments", board)?;
    let dashboard_artifacts = project_artifacts(ctx, state, include_dashboard)?;
    let mut command = vec![
        "git",
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
    ];
    command.extend_from_slice(oms_work_inspect::git::STATUS_ARGS);
    let status = helpers::query(ctx, &command).unwrap_or_default();
    let git = oms_work_inspect::git::snapshot(&status);
    let head = git["head"].as_str().unwrap_or_default();
    let branch = git["branch"].as_str().unwrap_or_default();
    let upstream = git["upstream"].as_str().unwrap_or_default();
    let ahead = git["ahead"].as_u64();
    state.set("git", json::parse(git.to_string().as_bytes())?)?;
    let (ci_rows, _) = rows(ctx, ".oms/ci.jsonl");
    let mut ci = ci_rows.last().cloned().unwrap_or_else(json::empty);
    let fresh = !head.is_empty()
        && !ci.text("sha").is_empty()
        && (head.starts_with(ci.text("sha")) || ci.text("sha").starts_with(head));
    let ci_state = if fresh {
        "current"
    } else if ahead.is_some_and(|a| a > 0)
        && (!ci_rows.is_empty() || ctx.repo.join(".github/workflows").is_dir())
    {
        "unpushed"
    } else if !ci_rows.is_empty() && ahead == Some(0) {
        "pending"
    } else if !ci_rows.is_empty() {
        "stale"
    } else {
        "none"
    };
    for (k, v) in [
        ("state", string(ci_state)),
        ("present", Value::Bool(!ci_rows.is_empty())),
        ("current_sha", string(head)),
        ("current_branch", string(branch)),
        ("upstream", string(upstream)),
        (
            "fresh",
            if ci_rows.is_empty() {
                Value::Null
            } else {
                Value::Bool(fresh)
            },
        ),
        ("ahead", ahead.map(number).unwrap_or(Value::Null)),
        (
            "unpushed",
            ahead.map(|a| Value::Bool(a > 0)).unwrap_or(Value::Null),
        ),
    ] {
        ci.set(k, v)?;
    }
    state.set("ci", ci)?;
    let mut delegations = json_files(ctx, ".oms/delegations")?;
    for row in &mut delegations {
        row.set("live", alive(row).map(Value::Bool).unwrap_or(Value::Null))?;
        if !row.text("worktree").is_empty() {
            let p = Path::new(row.text("worktree"));
            row.set(
                "location",
                string(format!(
                    "worktree:{}/{}",
                    p.parent()
                        .and_then(Path::file_name)
                        .unwrap_or_default()
                        .to_string_lossy(),
                    p.file_name().unwrap_or_default().to_string_lossy()
                )),
            )?;
        }
    }
    state.set("delegations", Value::Array(delegations))?;
    state.set("executors", Value::Array(vec![]))?;
    let souls = json_files(ctx, ".oms/souls")?;
    state.set("souls", Value::Array(souls))?;
    let guard = storage::read(
        &ctx.repo,
        Path::new(".oms/guards/change-guard.tsv"),
        storage::MAX_FILE,
        true,
    )?;
    let mut guard_row = json::empty();
    for line in String::from_utf8_lossy(&guard).lines() {
        if let Some((key, value)) = line.split_once('\t') {
            guard_row.set(key, string(value))?;
        }
    }
    let stale = alive(&guard_row) == Some(false);
    let guard = object([
        ("active", Value::Bool(!guard.is_empty())),
        ("stale", Value::Bool(stale)),
    ]);
    state.set("change_guard", guard.clone())?;
    state.set("guards", guard)?;
    let (landings, _) = rows(ctx, ".oms/landings.jsonl");
    let mut open = BTreeMap::new();
    for row in landings {
        let id = row.text("landing_id").to_owned();
        match row.text("event") {
            "intent" => {
                open.insert(id, row);
            }
            "complete" | "abandoned" => {
                open.remove(&id);
            }
            _ => {}
        }
    }
    state.set(
        "landings",
        object([("outstanding", Value::Array(open.into_values().collect()))]),
    )?;
    let mut threads = Vec::new();
    let current = storage::read_retrying(&ctx.repo, Path::new(".oms/threads/CURRENT"), 4096, true)?;
    let current = String::from_utf8_lossy(&current)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_owned();
    let thread_ttl = std::env::var("OMS_THREAD_STALE_TTL")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(86400);
    let threads_dir = ctx.repo.join(".oms/threads");
    let thread_paths = helpers::shallow_files(ctx, &threads_dir, ".jsonl", 50000)?;
    let canonical_repo = room.and_then(|_| std::fs::canonicalize(&ctx.repo).ok());
    let reusable_room = room.filter(|r| {
        ctx.repo == r.repository() && canonical_repo.as_deref() == Some(r.repository())
    });
    for path in thread_paths {
        let (rows, bad) = thread_rows(&ctx.repo, &threads_dir, &path, reusable_room)?;
        if bad > 0 {
            continue;
        }
        let id = path.file_stem().unwrap_or_default().to_string_lossy();
        if rows.is_empty()
            || rows.iter().any(|r| {
                r.text("role") == "closed" || r.get("room_event").text("kind") == "successor"
            })
        {
            continue;
        }
        let last = rows.last().unwrap();
        threads.push(object([
            ("id", string(&id)),
            ("turns", number(rows.len())),
            ("closed", Value::Bool(false)),
            ("last_ts", last.get("ts").clone()),
            (
                "age_seconds",
                helpers::now_age(last.text("ts"))
                    .map(number)
                    .unwrap_or(Value::Null),
            ),
            (
                "providers",
                json::list(
                    rows.iter()
                        .map(|r| r.text("provider"))
                        .filter(|s| !s.is_empty())
                        .collect::<BTreeSet<_>>(),
                ),
            ),
        ]));
    }
    threads.sort_by(|a, b| b.text("last_ts").cmp(a.text("last_ts")));
    let stale = threads
        .iter()
        .filter(|r| {
            r.text("id") != current
                && r.get("age_seconds")
                    .int()
                    .is_ok_and(|age| age > i128::from(thread_ttl))
        })
        .count();
    state.set(
        "threads",
        object([
            (
                "current",
                if current.is_empty() {
                    Value::Null
                } else {
                    string(current)
                },
            ),
            ("open", number(threads.len())),
            ("stale_open", number(stale)),
            (
                "recent",
                Value::Array(threads.into_iter().take(3).collect()),
            ),
        ]),
    )?;
    let (storage_health, unsealed) = oms_command_journal::cold::journal_storage_with_unsealed_count(
        &ctx.repo,
        None,
        oms_command_journal::tz::current_utc_now(),
    );
    let storage_value = storage_health
        .to_json()
        .into_value()
        .map_err(|e| error(2, format!("{e:?}")))?;
    let storage_bytes =
        oms_json::canonical_json(&storage_value).map_err(|e| error(2, format!("{e:?}")))?;
    let mut journal = json::parse(&storage_bytes)?;
    let status = state.get("work_journal").clone();
    for (k, v) in status.object().unwrap_or(&[]) {
        journal.set(std::str::from_utf8(k).unwrap_or(""), v.clone())?;
    }
    journal.set("unsealed", number(unsealed))?;
    state.set("work_journal", journal)?;
    let mut failure = state.get("failures").clone();
    let all = failure
        .get("failures")
        .array()
        .unwrap_or(&[])
        .iter()
        .filter(|r| !r.get("resolved").truth() && !r.get("expired").truth())
        .cloned()
        .collect::<Vec<_>>();
    let actionable = all.iter().filter(|r| r.get("actionable").truth()).count();
    let stale = all
        .iter()
        .filter(|r| r.text("attention") == "stale")
        .count();
    failure.set(
        "healthy",
        Value::Bool(
            failure.get("healthy").truth() && failure.get("invalid_rows").int().unwrap_or(0) == 0,
        ),
    )?;
    for (k, v) in [
        (
            "present",
            Value::Bool(ctx.repo.join(".oms/failures.jsonl").exists()),
        ),
        ("open_total", number(all.len())),
        ("actionable_total", number(actionable)),
        ("stale_total", number(stale)),
        ("retiring_total", number(all.len() - actionable - stale)),
        ("open", Value::Array(all)),
    ] {
        failure.set(k, v)?;
    }
    state.set("failures", failure)?;
    let canonical = ctx
        .runtime
        .cloned()
        .map(Ok)
        .or_else(|| host.runtime_envelope(&ctx.repo, ctx.interrupted));
    let mut runtime = if let Some(canonical) = canonical {
        match canonical {
            Ok(mut injected) => {
                require(
                    injected.get("schema").is_int(2),
                    "runtime service returned unsupported schema",
                )?;
                injected.set("healthy", Value::Bool(true))?;
                injected
            }
            Err(_) => object([
                ("schema", number(2)),
                ("healthy", Value::Bool(false)),
                ("error", string("canonical-runtime-unavailable")),
                ("criteria", Value::Array(vec![])),
                ("next_actions", Value::Array(vec![])),
                (
                    "warnings",
                    Value::Array(vec![string("canonical-runtime-unavailable")]),
                ),
            ]),
        }
    } else {
        let evidence = file(ctx, ".oms/runtime/evidence.json");
        let healthy = state.get("task").get("healthy").truth()
            && state.get("plan").get("healthy").truth()
            && state.get("project").get("healthy").truth()
            && evidence.is_ok();
        object([
            ("schema", number(2)),
            ("healthy", Value::Bool(healthy)),
            ("scope", json::empty()),
            ("criteria", Value::Array(vec![])),
            (
                "evidence",
                evidence.unwrap_or_else(|_| object([("complete", Value::Bool(false))])),
            ),
            ("next_actions", Value::Array(vec![])),
            ("continuity", json::empty()),
            ("warnings", Value::Array(vec![])),
        ])
    };
    if runtime.opt("objective").is_none() {
        let task = state.get("task");
        let plan = state.get("plan");
        let task_goal = if matches!(task.get("current_goal_eligible"), Value::Bool(true)) {
            let (sections, _) =
                oms_contract_markdown::sections(std::str::from_utf8(&task_text).unwrap_or(""));
            oms_contract_markdown::task_goal(&sections)
        } else {
            String::new()
        };
        let plan_goal = oms_contract_markdown::bounded_line(plan.text("goal"), 1000);
        let (goal, source) = oms_work_inspect::current_objective(
            &task_goal,
            &plan_goal,
            matches!(plan.get("has_unfinished"), Value::Bool(true)),
            state.get("project").text("goal"),
        );
        runtime.set(
            "objective",
            object([("text", string(goal)), ("source", string(source))]),
        )?;
    }
    state.set("runtime", runtime)?;
    let rules = ["AGENTS.md", "CLAUDE.md"]
        .iter()
        .filter(|n| ctx.repo.join(n).is_file())
        .map(|n| string(*n))
        .collect::<Vec<_>>();
    state.set(
        "harness",
        object([
            (
                "rules",
                string(if rules.is_empty() {
                    "missing"
                } else {
                    "unmanaged"
                }),
            ),
            ("rule_files", Value::Array(rules)),
            ("styles", Value::Array(vec![])),
            ("spec", state.get("project").get("state").clone()),
            ("private", string("n/a")),
            ("exposed", Value::Array(vec![])),
        ]),
    )?;
    fleet::collect(ctx, state)?;
    state.set(
        "install",
        object([
            ("auto_update", string("unknown")),
            ("auto_update_detail", string("")),
        ]),
    )?;
    Ok(dashboard_artifacts)
}
pub(super) fn extra_attention(state: &Value, repo: &Path, items: &mut Vec<Value>) -> Result<()> {
    let anchor = quote(&repo.to_string_lossy());
    let mut add = |priority: &str, code: &str, count: usize, message: String, command: String| {
        if count > 0 {
            items.push(object([
                ("priority", string(priority)),
                ("code", string(code)),
                ("count", number(count)),
                ("summary", string(message)),
                ("command", string(command)),
            ]));
        }
    };
    let memory = state.get("memory_review");
    let command = format!("oms agent-memory --repo {anchor} review");
    if memory.get("pending") == &Value::Null {
        add(
            "P2",
            "memory-review-unavailable",
            1,
            "memory review source is unavailable; pending count is unknown".into(),
            command,
        );
    } else {
        let pending = memory.get("pending").int().unwrap_or(0) as usize;
        let skipped = memory.get("skipped").int().unwrap_or(0) as usize;
        add(
            "P2",
            "pending-memory-review",
            pending,
            format!(
                "{pending} memory proposal(s) await explicit review{}; unverified",
                if skipped > 0 {
                    format!(" ({skipped} invalid row(s) skipped)")
                } else {
                    String::new()
                }
            ),
            command.clone(),
        );
        if skipped > 0 {
            add(
                "P2",
                "memory-review-partial",
                skipped,
                format!("memory review skipped {skipped} invalid row(s)"),
                command,
            );
        }
    }
    let orphans = state
        .get("delegations")
        .array()
        .unwrap_or(&[])
        .iter()
        .filter(|r| r.get("live") == &Value::Bool(false))
        .count();
    add(
        "P1",
        "orphan-delegation",
        orphans,
        format!("{orphans} delegation marker(s) reference dead processes"),
        format!("oms gc --repo {anchor} --dry-run"),
    );
    let unsealed = state.get("work_journal").get("unsealed").int().unwrap_or(0) as usize;
    add(
        "P3",
        "unsealed-journal",
        unsealed,
        format!("{unsealed} event(s) in past journal periods can be sealed"),
        format!("oms journal --repo {anchor} seal"),
    );
    if state.get("room_mail").get("bound").truth() && !state.get("room_mail").get("healthy").truth()
    {
        add(
            "P1",
            "room-mail-unavailable",
            1,
            "bound room mailbox is unavailable; check repository and participant".into(),
            format!("oms room --repo {anchor} updates"),
        );
    }
    for (field, code, command) in [
        (
            "artifacts",
            "artifact-store-corrupt",
            "artifact-index validate",
        ),
        ("failures", "failure-ledger-corrupt", "fail-ledger list"),
    ] {
        if state.get(field).get("healthy") == &Value::Bool(false) {
            let (verb, tail) = command.split_once(' ').unwrap();
            add(
                "P1",
                code,
                1,
                format!("{field} store is invalid"),
                format!("oms {verb} --repo {anchor} {tail}"),
            );
        }
    }
    let failures = state
        .get("failures")
        .get("actionable_total")
        .int()
        .unwrap_or(0) as usize;
    add(
        "P1",
        "open-failures",
        failures,
        format!("{failures} actionable failure fingerprint(s)"),
        format!("oms fail-ledger --repo {anchor} list"),
    );
    let repeat = state
        .get("failures")
        .get("recent_repeat")
        .get("count")
        .int()
        .unwrap_or(0) as usize;
    add(
        "P1",
        "recent-failure-repeat",
        repeat,
        format!("the newest {repeat} fail-ledger rows are the same failure"),
        format!("oms fail-ledger --repo {anchor} list --stalls"),
    );
    if std::env::var("OMS_THREAD_ATTENTION").unwrap_or_default() != "0" {
        let n = state.get("threads").get("stale_open").int().unwrap_or(0) as usize;
        add(
            "P2",
            "stale-threads",
            n,
            format!("{n} open cross-agent thread(s) idle past the stale TTL"),
            format!("oms thread --repo {anchor} list --stale"),
        );
    }
    fleet::attention(state, &anchor, &mut add);
    Ok(())
}
pub(super) fn refresh_ci(ctx: &Context<'_>) -> Result<bool> {
    let sha = query(ctx, &["git", "rev-parse", "HEAD"]);
    if sha.is_empty() {
        return Ok(false);
    }
    let gh = std::env::var("OMS_GH_BIN").unwrap_or_else(|_| "gh".into());
    let raw = match helpers::query(
        ctx,
        &[
            &gh,
            "run",
            "list",
            "--commit",
            &sha,
            "--limit",
            "1",
            "--json",
            "headSha,headBranch,status,conclusion,url,databaseId",
        ],
    ) {
        Ok(raw) => raw,
        Err(_) => return Ok(false),
    };
    let data = json::parse(&raw)?;
    let Some(run) = data.array()?.first() else {
        return Ok(false);
    };
    let row = object([
        ("schema", number(1)),
        ("ts", string(timestamp(epoch())?)),
        ("sha", run.get("headSha").clone()),
        ("branch", run.get("headBranch").clone()),
        ("status", run.get("status").clone()),
        ("conclusion", run.get("conclusion").clone()),
        ("url", run.get("url").clone()),
        ("run_id", run.get("databaseId").clone()),
    ]);
    storage::append(&ctx.repo, Path::new(".oms/ci.jsonl"), &row)?;
    Ok(true)
}
pub(super) fn repairs(
    ctx: &Context<'_>,
    host: &dyn HostServices,
    state: &Value,
) -> Result<Vec<Value>> {
    let mut actions = Vec::new();
    if !state
        .get("plan")
        .get("stale")
        .array()
        .unwrap_or(&[])
        .is_empty()
    {
        actions.push(string(
            if plan(ctx, oms_command_plan::Action::Reclaim, host).is_ok() {
                "reclaimed-stale-plan"
            } else {
                "reclaim-stale-plan-failed"
            },
        ));
    }
    if ["stale", "pending"].contains(&state.get("ci").text("state")) {
        actions.push(string(if refresh_ci(ctx)? {
            "refreshed-ci"
        } else {
            "ci-refresh-failed"
        }));
    }
    let path = ctx.repo.join(".oms/artifacts/index.jsonl");
    let _guard = storage::lock(&path)?;
    let (rows, resolved, bad) = artifact_rows(ctx)?;
    require(bad == 0, "artifact index is invalid; repair refused")?;
    let mut winners = HashMap::new();
    let mut resolutions = Vec::new();
    for target in rows.iter().rev() {
        if ["patch-admit", "patch-land"].contains(&target.text("kind"))
            && target.get("exit").is_int(0)
        {
            winners.insert(
                (target.text("patch_sha256"), target.text("provider")),
                target,
            );
        }
        let digest = target.text("patch_sha256");
        if !["patch-admit", "delegate"].contains(&target.text("kind"))
            || !target.get("schema").is_int(1)
            || !target.get("exit").int().is_ok_and(|n| n > 0)
            || resolved.contains(target.text("event_id"))
            || !json::hex(digest, 64)
            || digest == json::sha(b"")
        {
            continue;
        }
        if let Some(winner) = winners.get(&(digest, target.text("provider"))) {
            let row = object([
                ("schema", number(1)),
                ("event_id", string(crate::random_id("evt_resolve_")?)),
                ("operation_id", target.get("operation_id").clone()),
                ("artifact_id", target.get("artifact_id").clone()),
                ("ts", string(timestamp(epoch())?)),
                ("kind", string("artifact-resolution")),
                ("provider", string(helpers::agent())),
                ("exit", number(0)),
                ("parent_event_id", target.get("event_id").clone()),
                ("resolves_event_id", target.get("event_id").clone()),
                ("resolution", string("resolved")),
                (
                    "reason",
                    string(format!(
                        "superseded by {} (same patch bytes later succeeded)",
                        winner.text("event_id")
                    )),
                ),
            ]);
            resolutions.push(row);
        }
    }
    resolutions.reverse();
    storage::append_many_locked(&ctx.repo, &path, &resolutions)?;
    let repaired = resolutions.len();
    if repaired > 0 {
        actions.push(string(format!("resolved-superseded:{repaired}")));
    }
    Ok(actions)
}
pub(super) fn verify(
    ctx: &Context<'_>,
    host: &dyn HostServices,
    state: &Value,
    findings: &mut Vec<Value>,
) -> Result<()> {
    for row in findings.iter_mut() {
        row.set("level", string("fail"))?;
        row.default("family", string("jsonl"))?;
        row.default("remedy", string("oms run validate"))?;
    }
    let mut add = |level: &str, family: &str, message: String, remedy: &str| {
        findings.push(object([
            ("level", string(level)),
            ("family", string(family)),
            ("message", string(message)),
            ("remedy", string(remedy)),
        ]));
    };
    if !ctx.repo.join(".oms/.gitignore").is_file() {
        add(
            "fail",
            "oms",
            ".oms/.gitignore is missing; harness state could reach git".into(),
            "restore .oms/.gitignore with a * entry",
        );
    }
    let task = state.get("task");
    if task.text("status") == "active" && !task.text("closed_at").is_empty() {
        add(
            "fail",
            "task",
            format!(
                "packet is active but carries closed_at={}",
                task.text("closed_at")
            ),
            "oms agent-task close --reason '<why>'",
        );
    }
    if !task.text("last_activity").is_empty()
        && crate::parse_timestamp(task.text("last_activity")).is_none()
    {
        add(
            "warn",
            "task",
            "last_activity does not parse".into(),
            "oms agent-task update",
        );
    }
    for family in ["threads", "runs"] {
        let path = PathBuf::from(format!(".oms/{family}/CURRENT"));
        let raw = if family == "threads" {
            storage::read_retrying(&ctx.repo, &path, 4096, true)?
        } else {
            storage::read(&ctx.repo, &path, 4096, true)?
        };
        let id = String::from_utf8_lossy(&raw)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_owned();
        if id.is_empty() {
            continue;
        }
        let known = if family == "threads" {
            helpers::identifier(&id).is_ok()
                && ctx
                    .repo
                    .join(".oms/threads")
                    .join(format!("{id}.jsonl"))
                    .is_file()
        } else {
            storage::rows(&ctx.repo, &spine::index(ctx))?
                .0
                .iter()
                .any(|r| r.text("run_id") == id)
        };
        if !known {
            add(
                "fail",
                family,
                format!("CURRENT names {id} but no such target exists"),
                "inspect and remove the dangling CURRENT pointer",
            );
        }
    }
    let notion = state.get("work_journal").get("notion");
    let unpublished =
        notion.get("failed").int().unwrap_or(0) + notion.get("pending").int().unwrap_or(0);
    if unpublished > 0 {
        add(
            "warn",
            "work-journal",
            format!("{unpublished} summaries never reached the published journal"),
            "oms journal sync",
        );
    }
    let dirty = state
        .get("work_journal")
        .get("dirty_period_count")
        .int()
        .unwrap_or(0);
    if dirty > 0 {
        add(
            "warn",
            "work-journal",
            format!("{dirty} derived periods are behind events.jsonl"),
            "oms journal rebuild",
        );
    }
    if ctx.repo.join(".oms/plan/retirements.jsonl").exists() {
        let request = oms_command_plan::Request::new(oms_command_plan::Action::Status);
        let mut context = oms_command_plan::Context::from_environment(&request, &ctx.repo)?;
        context.state_repo = ctx.repo.clone();
        context.plan_file = ctx.repo.join(".oms/plan/tasks.json");
        match oms_command_plan::Plan::open(context, host.plan_services(), ctx.interrupted)
            .and_then(|plan| oms_command_plan::retirement::validate_ledger(&plan))
        {
            Ok(_) => {}
            Err(e) => add(
                "fail",
                "plan-retirement",
                format!("retirement integrity error: {e}"),
                "restore exact content-addressed archive and retirement receipt",
            ),
        }
    }
    let (retirements, bad) = rows(ctx, ".oms/plan/retirements.jsonl");
    if bad > 0 {
        add(
            "fail",
            "plan-retirement",
            "invalid plan retirement ledger".into(),
            "restore the retirement archive/receipt",
        );
    }
    for row in retirements {
        if !row.get("schema").is_int(1)
            || !matches!(row.text("kind"), "plan-retirement" | "plan-replacement")
        {
            add(
                "fail",
                "plan-retirement",
                "unsupported retirement row".into(),
                "restore the retirement archive/receipt",
            );
            continue;
        }
        for key in ["archive", "receipt"] {
            if !row.text(key).is_empty() {
                let path = helpers::absolute(&ctx.repo, row.text(key));
                if !path.starts_with(&ctx.repo) || storage::input(&path, storage::MAX_FILE).is_err()
                {
                    add(
                        "fail",
                        "plan-retirement",
                        format!("retirement {key} is missing or invalid"),
                        "restore the retirement archive/receipt",
                    );
                }
            }
        }
    }
    for path in helpers::shallow_files(
        ctx,
        &ctx.repo.join(".oms/plan"),
        ".retire-intent.json",
        50000,
    )? {
        add(
            "fail",
            "plan-retirement",
            format!(
                "incomplete retirement intent {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            "replay the exact oms agent-plan retire --apply command",
        );
    }
    fleet::verify(state, findings);
    Ok(())
}
pub(super) fn render(report: &Value, tool: &str) -> Result<String> {
    let mut out = String::new();
    if tool == "inbox" {
        out.push_str(&format!(
            "oms inbox: {} item(s)\n",
            helpers::display(report.get("actionable"))
        ));
        for row in report.get("items").array()? {
            out.push_str(&format!(
                "{} {}: {}\n  next: {}\n",
                row.text("priority"),
                row.text("code"),
                row.text("summary"),
                row.text("command")
            ));
        }
        let actions = report.get("safe_actions").strings().unwrap_or_default();
        if !actions.is_empty() {
            out.push_str(&format!("safe actions: {}\n", actions.join(", ")));
        }
        return Ok(out);
    }
    if tool == "state-verify" {
        let rows = report.get("findings").array()?;
        for row in rows {
            out.push_str(&format!(
                "{}: {}: {}  (remedy: {})\n",
                row.text("level"),
                row.text("family"),
                row.text("message"),
                row.text("remedy")
            ));
        }
        if rows.is_empty() {
            out.push_str("state-verify: ok\n");
        } else {
            out.push_str(&format!(
                "state-verify: {} finding(s) ({} fail, {} warn)\n",
                rows.len(),
                rows.iter().filter(|r| r.text("level") == "fail").count(),
                rows.iter().filter(|r| r.text("level") == "warn").count()
            ));
        }
        return Ok(out);
    }
    if tool == "dashboard" {
        return dashboard_text(report);
    }
    state_text(report)
}

pub(super) fn dashboard(
    ctx: &Context<'_>,
    state: &Value,
    artifacts: DashboardArtifacts,
) -> Result<Value> {
    let ok = [
        "task",
        "plan",
        "agent_operations",
        "approvals",
        "failures",
        "runtime",
        "artifacts",
    ]
    .iter()
    .all(|k| state.get(k).get("healthy").truth());
    let runtime = state.get("runtime");
    let task = state.get("task");
    let plan = state.get("plan");
    let goal = runtime.get("objective").text("text");
    let source = runtime.get("objective").text("source");
    let goal_source_ok = runtime.get("objective").get("text").str().is_ok()
        && matches!(
            (goal.is_empty(), source),
            (true, "none") | (false, "task" | "plan" | "project")
        )
        && ["task", "plan", "project", "runtime"]
            .iter()
            .all(|key| matches!(state.get(key).get("healthy"), Value::Bool(true)));
    let mut projected_plan = plan.clone();
    projected_plan.set(
        "stale_claims",
        number(plan.get("stale").array().unwrap_or(&[]).len()),
    )?;
    projected_plan.set(
        "stale_reviews",
        number(plan.get("stale_review").array().unwrap_or(&[]).len()),
    )?;
    projected_plan.set(
        "contract_blocker",
        if plan.get("contract").get("bound").truth()
            && !plan.get("contract").get("satisfied").truth()
        {
            plan.get("contract").get("blocker").clone()
        } else {
            Value::Null
        },
    )?;
    let mut attempts = state.get("agent_operations").clone();
    attempts.set("available", attempts.get("healthy").clone())?;
    attempts.set("active_recent", attempts.get("active_latest").clone())?;
    let mut recent = attempts.get("recent").array().unwrap_or(&[]).to_vec();
    recent.reverse();
    for row in &mut recent {
        for (key, input) in [("tokens", "tokens"), ("cost_microusd", "cost_microusd")] {
            row.set(
                key,
                if row.get("usage_reports").get(input).int().unwrap_or(0) > 0 {
                    row.get("usage").get(input).clone()
                } else {
                    Value::Null
                },
            )?;
        }
    }
    attempts.set("recent", Value::Array(recent))?;
    attempts.set(
        "note",
        string("a done attempt ended its work; acceptance is judged by evidence"),
    )?;
    let DashboardArtifacts {
        operations,
        reviews,
    } = artifacts;
    let criteria = runtime.get("criteria").array().unwrap_or(&[]);
    let acceptance = object([
        ("available", runtime.get("healthy").clone()),
        ("total", number(criteria.len())),
        ("counts", runtime.get("evidence").get("counts").clone()),
        (
            "complete",
            Value::Bool(runtime.get("evidence").get("complete").truth()),
        ),
        (
            "criteria",
            Value::Array(criteria.iter().take(8).cloned().collect()),
        ),
        ("omitted", number(criteria.len().saturating_sub(8))),
        ("note", string("worker completion is not acceptance")),
    ]);
    let mut attention = inbox(state, &ctx.repo)?;
    attention.set(
        "omitted",
        number(attention.get("items").array()?.len().saturating_sub(8)),
    )?;
    attention.set(
        "next",
        Value::Array(
            runtime
                .get("next_actions")
                .array()
                .unwrap_or(&[])
                .iter()
                .take(1)
                .cloned()
                .collect(),
        ),
    )?;
    let scope = runtime.get("scope");
    let mut report = object([
        ("schema", number(1)),
        ("kind", string("oms-dashboard")),
        ("git", state.get("git").clone()),
        ("generated_at", string(timestamp(epoch())?)),
        (
            "repo",
            object([
                (
                    "name",
                    string(ctx.repo.file_name().unwrap_or_default().to_string_lossy()),
                ),
                ("branch", state.get("ci").get("current_branch").clone()),
                (
                    "head",
                    string(
                        state
                            .get("ci")
                            .text("current_sha")
                            .chars()
                            .take(12)
                            .collect::<String>(),
                    ),
                ),
            ]),
        ),
        (
            "coverage",
            string(
                "OMS-managed collaboration records only; native provider subagents that never touched OMS state are not observed",
            ),
        ),
        (
            "collection",
            object([
                ("ok", Value::Bool(ok)),
                (
                    "sources",
                    object([
                        ("state", string(if ok { "ok" } else { "degraded" })),
                        (
                            "goal",
                            string(if goal_source_ok { "ok" } else { "degraded" }),
                        ),
                        (
                            "artifacts",
                            string(if operations.get("available").truth() {
                                "ok"
                            } else {
                                "invalid artifact contract"
                            }),
                        ),
                        (
                            "attempts",
                            string(if attempts.get("available").truth() {
                                "ok"
                            } else {
                                "invalid lifecycle contract"
                            }),
                        ),
                    ]),
                ),
            ]),
        ),
        (
            "goal",
            if goal.is_empty() {
                Value::Null
            } else {
                object([("text", string(goal)), ("source", string(source))])
            },
        ),
        (
            "scope",
            if runtime.get("healthy").truth() {
                object([
                    (
                        "allowed",
                        scope
                            .opt("allowed")
                            .cloned()
                            .unwrap_or(Value::Array(vec![])),
                    ),
                    (
                        "forbidden",
                        scope
                            .opt("forbidden")
                            .cloned()
                            .unwrap_or(Value::Array(vec![])),
                    ),
                    (
                        "source",
                        scope
                            .opt("allowed_source")
                            .cloned()
                            .unwrap_or_else(|| string("unbounded")),
                    ),
                ])
            } else {
                Value::Null
            },
        ),
        ("task", task.clone()),
        ("plan", projected_plan),
        ("attempts", attempts),
        ("operations", operations),
        ("reviews", reviews),
        ("delegations", state.get("delegations").clone()),
        ("acceptance", acceptance),
        ("attention", attention),
        ("fleet", fleet::dashboard(state)),
    ]);
    if let Some(scratch) = state.opt("managedScratch") {
        report.set("managedScratch", scratch.clone())?;
    }
    Ok(report)
}

fn build_observation(repo: &Path, dashboard: &Value, report: &Value) -> Value {
    let source = |ok: bool, has_rows: bool| {
        if ok {
            "ok"
        } else if has_rows {
            "partial"
        } else {
            "unavailable"
        }
    };
    let attempts_ok = report.get("attempts").get("available").truth();
    let has_rows = |key: &str| {
        dashboard
            .get(key)
            .array()
            .is_ok_and(|rows| !rows.is_empty())
    };
    let plan = report.get("plan");
    let plan_stale = ["stale_claims", "stale_reviews"]
        .iter()
        .any(|key| plan.get(key).int().unwrap_or(0) > 0);
    let binding = match dashboard.text("conversationBinding") {
        "matched" if dashboard.get("conversationMain").object().is_ok() => "matched",
        "ambiguous" => "ambiguous",
        _ => "unconfirmed",
    };
    let states = [
        (
            "git",
            source(dashboard.get("git").get("available").truth(), false),
        ),
        (
            "task",
            source(report.get("task").get("healthy").truth(), false),
        ),
        (
            "plan",
            if plan_stale {
                "stale"
            } else {
                source(plan.get("healthy").truth(), false)
            },
        ),
        (
            "attempts",
            source(
                attempts_ok,
                has_rows("attempts") || has_rows("activeAttempts"),
            ),
        ),
        (
            "artifacts",
            source(
                report.get("operations").get("available").truth(),
                has_rows("operations"),
            ),
        ),
        (
            "acceptance",
            source(report.get("acceptance").get("available").truth(), false),
        ),
        ("conversation", binding),
    ];
    let scratch = dashboard.opt("managedScratch");
    let scratch_status = scratch.map(|s| match s.text("status") {
        "ready" => "ok",
        "partial" => "partial",
        _ => "unavailable",
    });
    let observed = states
        .iter()
        .any(|(_, value)| !matches!(*value, "unavailable" | "unconfirmed" | "ambiguous"))
        || scratch.is_some_and(|s| s.get("attempts").array().is_ok_and(|rows| !rows.is_empty()));
    let gaps = states
        .iter()
        .any(|(_, value)| !matches!(*value, "ok" | "unconfirmed" | "matched"))
        || scratch_status.is_some_and(|status| status != "ok")
        || dashboard.get("degraded").truth();
    let mut sources = object(states.map(|(name, value)| (name, string(value))));
    if let Some(status) = scratch_status {
        let _ = sources.set("managedScratch", string(status));
    }
    let nonneg = |v: &Value| {
        v.int()
            .ok()
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0)
    };
    let attempts_shown = dashboard
        .get("attempts")
        .array()
        .map_or(0, |rows| rows.len() as u64);
    let active_shown = dashboard
        .get("activeAttempts")
        .array()
        .map_or(0, |rows| rows.len() as u64);
    let attempts_trimmed = nonneg(dashboard.get("attemptsTrimmed"));
    let active_trimmed = nonneg(dashboard.get("activeAttemptsTrimmed"));
    let attempts_omitted = if attempts_ok {
        nonneg(dashboard.get("commandStatus").get("total"))
            .saturating_sub(attempts_shown.saturating_add(attempts_trimmed))
    } else {
        0
    };
    let active_omitted = if attempts_ok {
        nonneg(dashboard.get("active")).saturating_sub(active_shown.saturating_add(active_trimmed))
    } else {
        0
    };
    let acceptance_omitted = nonneg(dashboard.get("acceptance").get("omitted"));
    let evidence_omitted = dashboard
        .get("acceptance")
        .get("criteria")
        .array()
        .unwrap_or(&[])
        .iter()
        .map(|row| nonneg(row.get("evidenceOmitted")))
        .sum::<u64>();
    let readiness_omitted = nonneg(dashboard.get("planReadiness").get("omitted"));
    let omitted = nonneg(report.get("operations").get("omitted"))
        .saturating_add(nonneg(report.get("attention").get("omitted")))
        .saturating_add(acceptance_omitted)
        .saturating_add(nonneg(
            dashboard.get("conversationMain").get("participantsOmitted"),
        ))
        .saturating_add(nonneg(dashboard.get("conversationAttemptsOmitted")))
        .saturating_add(attempts_omitted)
        .saturating_add(active_omitted)
        .saturating_add(scratch.map_or(0, |s| nonneg(s.get("omittedAttempts"))));
    let trimmed = attempts_trimmed
        .saturating_add(nonneg(dashboard.get("operationsTrimmed")))
        .saturating_add(nonneg(dashboard.get("attentionTrimmed")))
        .saturating_add(active_trimmed)
        .saturating_add(nonneg(dashboard.get("conversationAttemptsTrimmed")));
    let conversation_incomplete = dashboard.get("conversationMain").get("incomplete").truth();
    // Bounds apply to each source window. A recent and an active row can refer to the same attempt.
    for (name, bounded) in [
        (
            "attempts",
            attempts_omitted > 0
                || active_omitted > 0
                || attempts_trimmed > 0
                || active_trimmed > 0,
        ),
        (
            "artifacts",
            nonneg(report.get("operations").get("omitted")) > 0
                || nonneg(dashboard.get("operationsTrimmed")) > 0,
        ),
        ("acceptance", acceptance_omitted > 0),
    ] {
        if bounded && sources.text(name) == "ok" {
            let _ = sources.set(name, string("partial"));
        }
    }
    let mut completeness = if !observed {
        "unknown"
    } else if gaps || conversation_incomplete || omitted > 0 || trimmed > 0 {
        "partial"
    } else {
        "complete"
    };
    let head = dashboard
        .get("git")
        .get("head")
        .str()
        .ok()
        .filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|s| string(&s[..12]))
        .unwrap_or(Value::Null);
    let latest_artifact = dashboard
        .get("operations")
        .array()
        .ok()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("artifactId").str().ok())
        .filter(|id| {
            id.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        })
        .map(string)
        .unwrap_or(Value::Null);
    let evidence = object([
        ("scope", string("repository")),
        ("head", head),
        ("latestArtifact", latest_artifact),
        ("omitted", number(omitted)),
        ("trimmed", number(trimmed)),
        ("readinessOmitted", number(readiness_omitted)),
        ("evidenceOmitted", number(evidence_omitted)),
    ]);
    let revision = if completeness == "unknown" {
        Value::Null
    } else {
        let mut stable_dashboard = dashboard.clone();
        // Exclude wall-clock-only read time so repeated equivalent reads share a revision.
        stable_dashboard.remove("updated");
        stable_dashboard.remove("observation");
        let basis = object([
            ("schema", number(1)),
            ("kind", string("multi-source-observation")),
            (
                "checkout",
                string(json::sha(repo.as_os_str().as_encoded_bytes())),
            ),
            ("completeness", string(completeness)),
            ("sources", sources.clone()),
            ("evidence", evidence.clone()),
            ("dashboard", stable_dashboard),
        ]);
        match crate::json::hash(&basis) {
            Ok(digest) => string(format!("obs-v1:{}", &digest[..24])),
            Err(_) => {
                completeness = "unknown";
                Value::Null
            }
        }
    };
    object([
        ("schema", number(1)),
        ("kind", string("multi-source-observation")),
        ("atomic", Value::Bool(false)),
        ("revision", revision),
        ("observedAt", dashboard.get("updated").clone()),
        ("completeness", string(completeness)),
        ("sources", sources),
        ("evidence", evidence),
    ])
}

fn goal_confirmed_absent(report: &Value) -> bool {
    report.opt("goal") == Some(&Value::Null)
        && report.get("collection").get("sources").text("goal") == "ok"
}

/// Compact public columns before command capture. Omit oversized strings whole
/// so that the MCP outbound scanner never receives a credential prefix.
fn project_panel_dashboard(repo: &Path, report: &Value, conversation: &Value) -> Value {
    fn label(v: &Value, bytes: usize) -> Value {
        match v.str() {
            Ok(s) if !s.is_empty() && s.len() <= bytes && !s.chars().any(char::is_control) => {
                string(s)
            }
            _ => Value::Null,
        }
    }
    fn rows(v: &Value, limit: usize, f: impl Fn(&Value) -> Value) -> Value {
        Value::Array(v.array().unwrap_or(&[]).iter().take(limit).map(f).collect())
    }
    fn count(v: &Value) -> Value {
        v.int()
            .ok()
            .filter(|n| *n >= 0)
            .map(number)
            .unwrap_or(Value::Null)
    }
    fn reference(v: &Value) -> Value {
        match v.str() {
            Ok(s)
                if !s.is_empty()
                    && s.len() <= 192
                    && s.bytes().any(|b| b.is_ascii_alphanumeric())
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b)) =>
            {
                string(s)
            }
            _ => Value::Null,
        }
    }
    fn criterion(row: &Value) -> Value {
        let evidence = row.get("evidence").array().unwrap_or(&[]);
        object([
            ("id", reference(row.get("id"))),
            ("label", label(row.get("text"), 2000)),
            ("status", label(row.get("status"), 64)),
            ("source", label(row.get("source"), 48)),
            (
                "task",
                reference(row.opt("plan_task_id").unwrap_or(row.get("active_task_id"))),
            ),
            (
                "evidence",
                // Canonical evidence is oldest first; keep the newest receipts
                // visible without reclassifying the criterion's verdict.
                Value::Array(
                    evidence
                        .iter()
                        .rev()
                        .take(3)
                        .map(|v| {
                            object([
                                ("reference", reference(v.get("evidence_ref"))),
                                ("kind", label(v.get("kind"), 80)),
                                ("status", label(v.get("status"), 64)),
                                (
                                    "stale",
                                    match v.get("stale") {
                                        Value::Bool(stale) => Value::Bool(*stale),
                                        _ => Value::Null,
                                    },
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("evidenceOmitted", number(evidence.len().saturating_sub(3))),
        ])
    }
    fn attempt(row: &Value) -> Value {
        let refs = row.get("refs");
        let scope = attempt_room(row);
        let provider = if row.text("tool") == "panel-main" && !refs.text("panel_harness").is_empty()
        {
            refs.get("panel_harness")
        } else {
            row.get("provider")
        };
        object([
            ("id", label(row.get("attempt_id"), 128)),
            ("provider", label(provider, 64)),
            ("tool", label(row.get("tool"), 96)),
            ("role", label(refs.get("panel_role"), 64)),
            (
                "participant",
                label(refs.get("panel_room_participant"), 160),
            ),
            ("created", label(row.get("created_at"), 64)),
            ("reason", label(row.get("reason_code"), 96)),
            (
                "room",
                match scope {
                    AttemptRoom::Named(room) => string(room),
                    _ => Value::Null,
                },
            ),
            ("task", label(row.get("task_id"), 192)),
            (
                "parentAttempt",
                if scope == AttemptRoom::Invalid {
                    Value::Null
                } else {
                    label(row.get("parent_attempt_id"), 128)
                },
            ),
            ("state", label(row.get("state"), 64)),
            ("updated", label(row.get("updated_at"), 64)),
        ])
    }
    let attempts = report.get("attempts");
    let active = attempts.get("active_recent");
    let mail = report.get("attention").get("room_mail");
    let bound_room = mail.get("bound").truth() && mail.get("healthy").truth();
    let current_room = if bound_room {
        oms_command_room::identifier(mail.text("room_id"), "room").ok()
    } else {
        None
    };
    let window = active.array().unwrap_or(&[]);
    let mut candidates = window.iter().filter(|r| {
        r.text("tool") == "panel-main"
            && (!bound_room
                || matches!(attempt_room(r),
                AttemptRoom::Named(room) if current_room == Some(room)))
    });
    let first = candidates.next();
    // A partial repository-wide window cannot establish uniqueness.
    let main = first.filter(|row| {
        attempt_room(row) != AttemptRoom::Invalid
            && candidates.next().is_none()
            && (current_room.is_some()
                || attempts.get("active").int().ok() == Some(window.len() as i128))
    });
    let goal_text = |v: &Value| {
        v.str()
            .ok()
            .filter(|s| !s.is_empty() && s.chars().count() <= 2000)
            .map(string)
            .unwrap_or(Value::Null)
    };
    let goal = goal_text(report.get("goal").get("text"));
    let goal_availability = if goal != Value::Null {
        "available"
    } else if goal_confirmed_absent(report) {
        "absent"
    } else {
        "unavailable"
    };
    let plan = report.get("plan");
    let acceptance = report.get("acceptance");
    let acceptance_available = matches!(acceptance.get("available"), Value::Bool(true));
    let criteria = acceptance.get("criteria").array().unwrap_or(&[]);
    let omitted = |source: &Value, retained: usize, limit: usize| {
        source
            .int()
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0)
            .saturating_add(retained.saturating_sub(limit))
    };
    let readiness = plan.get("task_readiness");
    let readiness_available =
        matches!(plan.get("healthy"), Value::Bool(true)) && readiness.array().is_ok();
    let next = report
        .get("attention")
        .get("next")
        .array()
        .ok()
        .and_then(|items| items.first())
        .filter(|_| acceptance_available);
    let operations = report.get("operations");
    let reviews = report.get("reviews");
    let operation_count = |key: &str| {
        if attempts.get("available").truth() {
            count(attempts.get(key))
        } else {
            Value::Null
        }
    };
    let plan_count = |key: &str| {
        if plan.get("healthy").truth() {
            count(plan.get(key))
        } else {
            Value::Null
        }
    };
    let plan_progress = match (
        plan.get("task_count").int(),
        plan.get("verified_count").int(),
        plan.get("cancelled_count").int(),
    ) {
        (Ok(tasks), Ok(verified), Ok(cancelled))
            if matches!(plan.get("healthy"), Value::Bool(true))
                && tasks < (1_i128 << 53)
                && (0..=tasks).contains(&cancelled)
                && (0..=tasks - cancelled).contains(&verified) =>
        {
            object([
                ("total", number(tasks - cancelled)),
                ("verified", number(verified)),
            ])
        }
        _ => Value::Null,
    };
    let mut dashboard = object([
        ("scope", string("repository")),
        ("git", report.get("git").clone()),
        (
            "conversationMain",
            conversation.get("conversationMain").clone(),
        ),
        (
            "conversationBinding",
            conversation
                .opt("conversationBinding")
                .cloned()
                .unwrap_or_else(|| string("unconfirmed")),
        ),
        (
            "currentRoom",
            current_room.map(string).unwrap_or(Value::Null),
        ),
        ("project", label(report.get("repo").get("name"), 255)),
        ("branch", label(report.get("repo").get("branch"), 255)),
        ("updated", label(report.get("generated_at"), 64)),
        ("goal", goal),
        ("goalAvailability", string(goal_availability)),
        (
            "degraded",
            Value::Bool(!report.get("collection").get("ok").truth()),
        ),
        ("main", main.map(attempt).unwrap_or(Value::Null)),
        (
            "active",
            if attempts.get("available").truth() {
                count(attempts.get("active"))
            } else {
                Value::Null
            },
        ),
        ("planProgress", plan_progress),
        (
            "planReadiness",
            object([
                ("available", Value::Bool(readiness_available)),
                ("scope", string("repository")),
                (
                    "tasks",
                    if readiness_available {
                        rows(readiness, 8, |v| {
                            object([
                                ("id", reference(v.get("id"))),
                                ("title", label(v.get("title"), 512)),
                                ("state", label(v.get("state"), 32)),
                                ("issue", label(v.get("issue"), 160)),
                                ("reason", label(v.get("reason"), 96)),
                                (
                                    "selectionEligible",
                                    match v.get("selection_eligible") {
                                        Value::Bool(eligible) => Value::Bool(*eligible),
                                        _ => Value::Null,
                                    },
                                ),
                            ])
                        })
                    } else {
                        Value::Array(vec![])
                    },
                ),
                (
                    "omitted",
                    if readiness_available {
                        number(omitted(
                            plan.get("task_readiness_omitted"),
                            readiness.array().unwrap_or(&[]).len(),
                            8,
                        ))
                    } else {
                        Value::Null
                    },
                ),
            ]),
        ),
        ("tasks", plan_count("task_count")),
        ("tasksVerified", plan_count("verified_count")),
        ("taskStatus", label(report.get("task").get("status"), 96)),
        ("attempts", rows(attempts.get("recent"), 16, attempt)),
        ("activeAttempts", rows(active, 8, attempt)),
        (
            "commandStatus",
            object([
                ("total", operation_count("total")),
                ("active", operation_count("active")),
                ("executing", operation_count("executing")),
                (
                    "byState",
                    if attempts.get("available").truth() {
                        attempts.get("by_state").clone()
                    } else {
                        Value::Null
                    },
                ),
                ("available", attempts.get("available").clone()),
            ]),
        ),
        (
            "attention",
            rows(report.get("attention").get("items"), 8, |v| {
                object([
                    ("code", label(v.get("code"), 96)),
                    ("summary", label(v.get("summary"), 512)),
                    ("count", count(v.get("count"))),
                    ("priority", label(v.get("priority"), 8)),
                    ("scope", string("repository")),
                ])
            }),
        ),
        (
            "nextStep",
            next.map(|v| {
                object([
                    ("id", reference(v.get("id"))),
                    ("summary", label(v.get("reason"), 1200)),
                    ("authority", label(v.get("authority"), 160)),
                    ("scope", string("repository")),
                ])
            })
            .unwrap_or(Value::Null),
        ),
        (
            "operations",
            rows(operations.get("recent"), 8, |v| {
                object([
                    ("kind", label(v.get("kind"), 96)),
                    ("provider", label(v.get("provider"), 64)),
                    ("status", label(v.get("status"), 96)),
                    ("updated", label(v.get("ts"), 64)),
                    (
                        "exit",
                        v.get("exit").int().ok().map(number).unwrap_or(Value::Null),
                    ),
                    ("artifactId", label(v.get("artifact_id"), 128)),
                    ("attempt", label(v.get("attempt_id"), 160)),
                    ("task", label(v.get("task_id"), 160)),
                    (
                        "verifyExit",
                        v.get("verify_exit")
                            .int()
                            .ok()
                            .map(number)
                            .unwrap_or(Value::Null),
                    ),
                    ("verifyStatus", label(v.get("verify_status"), 64)),
                ])
            }),
        ),
        (
            "reviews",
            object([
                ("available", operations.get("available").clone()),
                ("passed", count(reviews.get("passed"))),
                ("failed", count(reviews.get("failed"))),
                ("unknown", count(reviews.get("unknown"))),
            ]),
        ),
        (
            "acceptance",
            object([
                ("available", Value::Bool(acceptance_available)),
                ("scope", string("repository")),
                (
                    "total",
                    if acceptance_available {
                        count(acceptance.get("total"))
                    } else {
                        Value::Null
                    },
                ),
                (
                    "verified",
                    if acceptance_available {
                        count(acceptance.get("counts").get("verified"))
                    } else {
                        Value::Null
                    },
                ),
                (
                    "complete",
                    if acceptance_available {
                        acceptance.get("complete").clone()
                    } else {
                        Value::Null
                    },
                ),
                (
                    "criteria",
                    if acceptance_available {
                        rows(acceptance.get("criteria"), 8, criterion)
                    } else {
                        Value::Array(vec![])
                    },
                ),
                (
                    "omitted",
                    if acceptance_available {
                        number(omitted(acceptance.get("omitted"), criteria.len(), 8))
                    } else {
                        Value::Null
                    },
                ),
            ]),
        ),
    ]);
    if conversation.text("conversationBinding") == "matched"
        && conversation.opt("attempts").is_some()
    {
        let _ = dashboard.set(
            "conversationAttempts",
            rows(conversation.get("attempts"), 24, |row| {
                let mut item = attempt(row);
                if let Some(title) = conversation.get("taskTitles").opt(row.text("attempt_id")) {
                    let _ = item.set("taskTitle", title.clone());
                }
                item
            }),
        );
        let _ = dashboard.set(
            "conversationAttemptsOmitted",
            count(conversation.get("attemptsOmitted")),
        );
    }
    // Allocate headroom for MCP framing and escaped multibyte labels. Trim
    // complete rows only, keeping the goal and exact conversation identity.
    let mut bytes = crate::json::encoded(&dashboard).map_or(usize::MAX, |v| v.len());
    if bytes > 52_000 {
        for key in [
            "attempts",
            "operations",
            "attention",
            "activeAttempts",
            "conversationAttempts",
        ] {
            if bytes <= 52_000 {
                break;
            }
            if let Value::Array(mut saved) = dashboard.get(key).clone() {
                let count = saved.len();
                while bytes > 52_000
                    && let Some(row) = saved.pop()
                {
                    bytes = bytes.saturating_sub(
                        crate::json::encoded(&row).map_or(0, |v| v.len().saturating_sub(1)),
                    );
                }
                let _ = dashboard.set(&format!("{key}Trimmed"), number(count - saved.len()));
                let _ = dashboard.set(key, Value::Array(saved));
            }
        }
    }
    for (key, column) in [("acceptance", "criteria"), ("planReadiness", "tasks")] {
        let mut section = dashboard.get(key).clone();
        let mut saved = section.get(column).array().unwrap_or(&[]).to_vec();
        let mut omitted = section.get("omitted").int().unwrap_or(0);
        while bytes > 52_000 && saved.pop().is_some() {
            omitted += 1;
            let _ = section.set("omitted", number(omitted));
            let _ = section.set(column, Value::Array(saved.clone()));
            let _ = dashboard.set(key, section.clone());
            bytes = crate::json::encoded(&dashboard).map_or(usize::MAX, |v| v.len());
        }
    }
    if let Some(scratch) = report.opt("managedScratch") {
        let mut scratch = scratch.clone();
        let encoded_len = |v: &Value| crate::json::encoded(v).map_or(usize::MAX, |b| b.len());
        // Preserve the canonical window. A busy scratch registry cannot displace
        // the existing Room, task, attempt or approval evidence. The observation
        // ranks attempts live-first, so trimming from the tail keeps running ones.
        if bytes.saturating_add(encoded_len(&scratch)) > 52_000 {
            let mut saved_attempts = scratch.get("attempts").array().unwrap_or(&[]).to_vec();
            let all_sources = scratch.get("sources").array().unwrap_or(&[]).to_vec();
            let orig_attempts = saved_attempts.len();
            let orig_sources = all_sources.len();
            if orig_attempts + orig_sources > 0 {
                let base_omitted = scratch
                    .get("omittedAttempts")
                    .int()
                    .ok()
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(0);
                let _ = scratch.set("status", string("partial"));
                let mut kept_sources = all_sources;
                while bytes.saturating_add(encoded_len(&scratch)) > 52_000 {
                    if let Some(pos) = kept_sources.iter().rposition(|s| {
                        let id = s.text("id");
                        id.is_empty() || !saved_attempts.iter().any(|a| a.text("sourceId") == id)
                    }) {
                        kept_sources.remove(pos);
                        let _ = scratch
                            .set("omittedSources", number(orig_sources - kept_sources.len()));
                        let _ = scratch.set("sources", Value::Array(kept_sources.clone()));
                        continue;
                    }
                    if saved_attempts.pop().is_none() {
                        break;
                    }
                    let dropped = orig_attempts - saved_attempts.len();
                    let _ = scratch.set("omittedAttempts", number(base_omitted + dropped));
                    let _ = scratch.set("attempts", Value::Array(saved_attempts.clone()));
                    kept_sources.retain(|s| {
                        let id = s.text("id");
                        !id.is_empty() && saved_attempts.iter().any(|a| a.text("sourceId") == id)
                    });
                    if kept_sources.len() < orig_sources {
                        let _ = scratch
                            .set("omittedSources", number(orig_sources - kept_sources.len()));
                    }
                    let _ = scratch.set("sources", Value::Array(kept_sources.clone()));
                }
            }
        }
        let _ = dashboard.set("managedScratch", scratch);
    }
    let observation = build_observation(repo, &dashboard, report);
    let _ = dashboard.set("observation", observation);
    dashboard
}

pub(super) fn panel_snapshot(repo: &Path, report: &Value, conversation: &Value) -> Value {
    object([
        ("schema", number(1)),
        ("kind", string("oms-panel-snapshot")),
        (
            "dashboard",
            project_panel_dashboard(repo, report, conversation),
        ),
    ])
}
fn scratch_lines(report: &Value) -> Vec<String> {
    let Some(observation) = report.opt("managedScratch") else {
        return vec![];
    };
    let attempts = observation.get("attempts").array().unwrap_or(&[]);
    if attempts.is_empty() && observation.text("status") == "ready" {
        return vec![];
    }
    let sources = observation.get("sources").array().unwrap_or(&[]);
    let mut lines = vec![format!(
        "Managed scratch lifecycle records ({})",
        observation.text("status")
    )];
    for a in attempts {
        let label = sources
            .iter()
            .find(|s| s.text("id") == a.text("sourceId"))
            .map_or("unknown", |s| s.text("label"));
        lines.push(format!(
            "  {label} · {} {} · recorded {} at {}",
            a.text("provider"),
            a.text("tool"),
            a.text("state"),
            a.text("updatedAt")
        ));
    }
    lines
}

fn dashboard_text(report: &Value) -> Result<String> {
    fn shown(v: &Value) -> String {
        if matches!(v, Value::Null) || v.str() == Ok("") {
            "unknown".into()
        } else {
            helpers::display(v)
        }
    }
    fn counts(v: &Value) -> String {
        v.object()
            .unwrap_or(&[])
            .iter()
            .map(|(k, v)| format!("{}={}", String::from_utf8_lossy(k), helpers::display(v)))
            .collect::<Vec<_>>()
            .join(" ")
    }
    let repo = report.get("repo");
    let obs = report.get("observation");
    let obs_rev = obs
        .get("revision")
        .str()
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    let obs_completeness = obs
        .get("completeness")
        .str()
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    let obs_time = obs
        .get("observedAt")
        .str()
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    let obs_atomic = if obs.get("atomic").truth() {
        "atomic"
    } else {
        "non-atomic"
    };
    let obs_gaps: Vec<String> = obs
        .get("sources")
        .object()
        .unwrap_or(&[])
        .iter()
        .filter(|(_, v)| {
            matches!(
                v.str(),
                Ok("unavailable" | "ambiguous" | "stale" | "partial")
            )
        })
        .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
        .collect();
    let mut lines = vec![
        format!(
            "OMS dashboard  repo={}  {}@{}  snapshot={}",
            repo.text("name"),
            if repo.text("branch").is_empty() {
                "?"
            } else {
                repo.text("branch")
            },
            if repo.text("head").is_empty() {
                "?"
            } else {
                repo.text("head")
            },
            report.text("generated_at")
        ),
        format!("coverage: {}", report.text("coverage")),
        format!(
            "observation: {obs_rev} (multi-source, {obs_atomic}, {obs_completeness})  observed={obs_time}"
        ),
    ];
    if !obs_gaps.is_empty() {
        lines.push(format!("  observation source gaps: {}", obs_gaps.join(",")));
    }
    for (source, status) in report.get("collection").get("sources").object()? {
        if status.str() != Ok("ok") {
            lines.push(format!(
                "COLLECTION FAILED {}: {}",
                String::from_utf8_lossy(source),
                helpers::display(status)
            ));
        }
    }
    lines.extend(scratch_lines(report));
    let goal = report.get("goal");
    lines.push(format!(
        "goal: {}",
        if !goal.text("text").is_empty() {
            format!("{} ({})", goal.text("text"), goal.text("source"))
        } else if goal_confirmed_absent(report) {
            "none recorded".into()
        } else {
            "unavailable".into()
        }
    ));
    let scope = report.get("scope");
    lines.push(if matches!(scope, Value::Null) {
        "scope: unavailable (runtime projection)".into()
    } else if scope.get("allowed").array().unwrap_or(&[]).is_empty()
        && scope.get("forbidden").array().unwrap_or(&[]).is_empty()
    {
        "scope: unbounded (none recorded)".into()
    } else {
        format!(
            "scope: allowed={} ({})  forbidden={}",
            scope.get("allowed").strings().unwrap_or_default().join(","),
            scope.text("source"),
            scope
                .get("forbidden")
                .strings()
                .unwrap_or_default()
                .join(",")
        )
    });
    let task = report.get("task");
    lines.push(if !task.get("healthy").truth() {
        "task: UNAVAILABLE or invalid (oms agent-task status --json)".into()
    } else if task.get("present").truth() {
        format!(
            "task: {} status={} verification={}{}",
            shown(task.get("task_id")),
            shown(task.get("status")),
            shown(task.get("verification")),
            if task.get("stale").truth() {
                "  STALE"
            } else if task.get("dormant").truth() {
                "  DORMANT"
            } else {
                ""
            }
        )
    } else {
        "task: none active".into()
    });
    let plan = report.get("plan");
    lines.push(if !plan.get("healthy").truth() {
        "plan: UNAVAILABLE or invalid (oms agent-plan status --json)".into()
    } else if plan.get("present").truth() {
        format!(
            "plan: {} task(s)  {}{}",
            shown(plan.get("task_count")),
            counts(plan.get("by_state")),
            if plan.get("actionable").array().unwrap_or(&[]).is_empty() {
                String::new()
            } else {
                format!(
                    "  actionable={}",
                    plan.get("actionable").strings()?.join(",")
                )
            }
        )
    } else {
        "plan: none".into()
    });
    let attempts = report.get("attempts");
    if !attempts.get("available").truth() {
        lines.push("attempts: UNAVAILABLE (lifecycle stream invalid)".into());
    } else {
        lines.push(format!(
            "attempts (OMS lifecycle): {} total, {} active  {}",
            shown(attempts.get("total")),
            shown(attempts.get("active")),
            counts(attempts.get("by_state"))
        ));
        for row in attempts.get("recent").array().unwrap_or(&[]) {
            lines.push(format!(
                concat!("  {} {} {}/{} task={} to", "kens={} cost_microusd={} {}"),
                row.text("attempt_id"),
                row.text("state"),
                shown(row.get("provider")),
                shown(row.get("tool")),
                shown(row.get("task_id")),
                shown(row.get("tokens")),
                shown(row.get("cost_microusd")),
                row.text("updated_at")
            ));
        }
    }
    for row in report.get("delegations").array().unwrap_or(&[]) {
        lines.push(format!(
            "delegation: {} {} started={} {}",
            row.text("id"),
            shown(row.get("provider")),
            shown(row.get("started_at")),
            if row.get("live").truth() {
                "live"
            } else {
                "ORPHAN (dead pid)"
            }
        ));
    }
    let operations = report.get("operations");
    if operations.get("available").truth() {
        let rows = operations.get("recent").array()?;
        lines.push(if rows.is_empty() {
            "operations (artifact index): none recorded".into()
        } else {
            "operations (artifact index, newest first):".into()
        });
        for row in rows {
            lines.push(format!(
                concat!(
                    "  {} {} model={} route={} exit={} {} served={} to",
                    "kens={} at={}"
                ),
                shown(row.get("provider")),
                shown(row.get("kind")),
                shown(row.get("selected_model")),
                shown(row.get("model_class")),
                shown(row.get("exit")),
                shown(row.get("status")),
                shown(row.get("served_model")),
                shown(row.get("tokens")),
                row.text("ts")
            ));
        }
        let review = report.get("reviews");
        lines.push(if review.get("outcomes").int().unwrap_or(0) > 0 {
            format!(
                "review gate: recorded window pass={} fail={} unknown={}",
                shown(review.get("passed")),
                shown(review.get("failed")),
                shown(review.get("unknown"))
            )
        } else {
            "review gate: none recorded in window (not a pass)".into()
        });
    } else {
        lines.push("operations: UNAVAILABLE (invalid artifact contract)".into());
    }
    let acceptance = report.get("acceptance");
    if !acceptance.get("available").truth() {
        lines.push("acceptance: UNAVAILABLE (oms runtime doctor --strict)".into());
    } else if acceptance.get("total").is_int(0) {
        lines.push("acceptance: no criteria recorded; completion cannot be proven".into());
    } else {
        lines.push(format!(
            "acceptance: {}/{} verified  {}  ({})",
            shown(acceptance.get("counts").get("verified")),
            shown(acceptance.get("total")),
            counts(acceptance.get("counts")),
            acceptance.text("note")
        ));
        for row in acceptance.get("criteria").array()? {
            lines.push(format!(
                "  {} {} [{}] {}",
                row.text("status"),
                row.text("id"),
                row.text("source"),
                row.text("text")
            ));
        }
    }
    for line in fleet::lines(report.get("fleet")) {
        lines.push(format!("fleet {line}"));
    }
    let attention = report.get("attention");
    lines.push(format!(
        "attention: {} item(s)",
        shown(attention.get("actionable"))
    ));
    for row in attention.get("items").array()?.iter().take(8) {
        lines.push(format!(
            "  {} {}: {}",
            row.text("priority"),
            row.text("code"),
            row.text("summary")
        ));
        lines.push("     next:".into());
        lines.push(row.text("command").to_owned());
    }
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(120);
    Ok(lines
        .into_iter()
        .map(|l| {
            if l.starts_with("oms ") || l.starts_with("OMS_ROOM_REPO=") {
                l
            } else {
                l.chars().take(width).collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n")
}

fn state_text(state: &Value) -> Result<String> {
    fn n(v: &Value, key: &str) -> i128 {
        v.get(key).int().unwrap_or(0)
    }
    fn counts(v: &Value) -> String {
        let mut entries = v.object().unwrap_or(&[]).to_vec();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
            .iter()
            .map(|(k, v)| format!("{}={}", String::from_utf8_lossy(k), helpers::display(v)))
            .collect::<Vec<_>>()
            .join(", ")
    }
    let mut out = format!("# state: {}\n", state.text("repo"));
    let objective = state.get("runtime").get("objective");
    if !objective.text("text").is_empty() {
        out.push_str(&format!(
            "objective ({}): {}\n",
            objective.text("source"),
            objective.text("text")
        ));
    }
    for line in scratch_lines(state) {
        out.push_str(&line);
        out.push('\n');
    }
    let mail = state.get("room_mail");
    if mail.get("bound").truth() {
        out.push_str("\n## Room mail\n");
        if !mail.get("healthy").truth() {
            out.push_str(
                "  unavailable; check repository and participant (run: oms room updates)\n",
            );
        } else {
            out.push_str(&format!("  {} unacknowledged; oldest age: {}\n  read: {} (explicit acknowledgment required)\n",n(mail,"pending"),mail.get("oldest_age_seconds").int().map(|n|format!("{n}s")).unwrap_or_else(|_|"-".into()),mail.text("command")));
        }
    }
    let memory = state.get("memory_review");
    if !memory.get("healthy").truth() || n(memory, "pending") > 0 || n(memory, "skipped") > 0 {
        out.push_str("\n## Memory review\n");
        if memory.get("pending") == &Value::Null {
            out.push_str("  unavailable; pending count is unknown\n");
        } else {
            let skipped = n(memory, "skipped");
            if n(memory, "pending") > 0 || skipped == 0 {
                out.push_str(&format!(
                    "  {} proposal(s) await explicit review{}; unverified\n",
                    n(memory, "pending"),
                    if skipped > 0 {
                        format!(" (partial: {skipped} invalid row(s) skipped)")
                    } else {
                        String::new()
                    }
                ));
            } else {
                out.push_str(&format!(
                    "  0 proposal(s) await explicit review (partial: {skipped} invalid row(s) skipped)\n"
                ));
            }
        }
        out.push_str("  inspect: oms agent-memory review\n");
    }
    let journal = state.get("work_journal");
    if journal.get("present").truth() {
        out.push_str(&format!("\n## Work Journal storage\n  {} event lines, {} event bytes; {} files, {} total bytes\n",n(journal,"event_lines"),n(journal,"events_bytes"),n(journal,"file_count"),n(journal,"total_bytes")));
        if !journal.get("healthy").truth() {
            out.push_str(
                "  measurement incomplete; journal storage is unavailable or changed during read\n",
            );
        }
        out.push_str(&format!(
            "  {}{}\n",
            if journal.get("threshold_exceeded").truth() {
                "ATTENTION: "
            } else {
                ""
            },
            journal.text("threshold_note")
        ));
    }
    let harness = state.get("harness");
    if harness.text("rules") == "missing" && harness.text("spec") == "missing" {
        out.push_str("\n## Project harness: none (run: oms apply-project-template auto .)\n");
    } else {
        out.push_str("\n## Project harness\n");
        out.push_str(&format!(
            "  rules: {}, hand-written (no oh-my-setting block)\n  spec: {}\n",
            harness
                .get("rule_files")
                .strings()
                .unwrap_or_default()
                .join(", "),
            harness.text("spec")
        ));
    }
    let threads = state.get("threads");
    if threads.get("current").truth() || n(threads, "open") > 0 {
        out.push_str(&format!(
            "\n## Cross-agent threads ({} open, {} stale)\n",
            n(threads, "open"),
            n(threads, "stale_open")
        ));
        for row in threads.get("recent").array().unwrap_or(&[]) {
            out.push_str(&format!(
                "  {}{:<26} turns={:<3} {}\n",
                if row.text("id") == threads.text("current") {
                    "* "
                } else {
                    "  "
                },
                row.text("id"),
                n(row, "turns"),
                row.get("providers").strings().unwrap_or_default().join(",")
            ));
        }
        if n(threads, "stale_open") > 0 {
            out.push_str("  stale: oms thread list --stale  (advisory; nothing auto-closes)\n");
        }
        if threads.get("current").truth() {
            out.push_str(&format!(
                "  resume: oms consult \"...\"  (joins {})\n",
                threads.text("current")
            ));
        }
    }
    let task = state.get("task");
    if task.get("present").truth() {
        if task.get("dormant").truth()
            || task.get("stale").truth()
            || task.text("status") == "closed"
        {
            out.push_str("\n## Historical task packet\n");
        } else {
            out.push_str("\n## Active task\n");
        }
        if !task.get("healthy").truth() {
            out.push_str("  status unavailable or invalid (run: oms agent-task status --json)\n");
        }
        let freshness = oms_command_task::packet::PacketFreshness {
            idle_seconds: task
                .get("idle_seconds")
                .int()
                .ok()
                .and_then(|s| u64::try_from(s).ok())
                .or_else(|| {
                    helpers::now_age(task.text("last_activity")).and_then(|s| u64::try_from(s).ok())
                }),
            stale: task.get("stale").truth(),
            dormant: task.get("dormant").truth(),
        };
        if let Some(label) = freshness.label() {
            out.push_str(&format!("  status: {label}\n"));
            if !task.text("goal").is_empty() {
                out.push_str(&format!("  goal: {}\n", task.text("goal")));
            }
            if let Some(hint) = freshness.hint() {
                out.push_str(&format!("  hint: {hint}\n"));
            }
        } else {
            for key in ["goal", "next"] {
                if !task.text(key).is_empty() {
                    out.push_str(&format!("  {key}: {}\n", task.text(key)));
                }
            }
        }
    } else {
        out.push_str("\n## Active task: none\n");
    }
    let runtime = state.get("runtime");
    out.push_str("\n## Runtime contract\n");
    if !runtime.get("healthy").truth() {
        out.push_str("  unavailable or invalid (run: oms runtime doctor --strict)\n");
    } else {
        let evidence = runtime.get("evidence");
        let coverage = helpers::display(evidence.get("coverage"))
            .parse::<f64>()
            .unwrap_or(0.0);
        out.push_str(&format!(
            "  evidence coverage: {:.1}%  complete={}\n",
            100.0 * coverage,
            if evidence.get("complete").truth() {
                "yes"
            } else {
                "no"
            }
        ));
        if evidence.get("counts").object().is_ok_and(|o| !o.is_empty()) {
            out.push_str(&format!("  criteria: {}\n", counts(evidence.get("counts"))));
        }
        if let Some(action) = runtime.get("next_actions").array().unwrap_or(&[]).first() {
            out.push_str(&format!("  next: {}\n", action.text("command")));
        }
    }
    let plan = state.get("plan");
    if plan.get("present").truth() {
        out.push_str("\n## Plan\n");
        if !plan.get("healthy").truth() {
            out.push_str("  status unavailable or invalid (run: oms agent-plan status --json)\n");
        }
        if !plan.text("goal").is_empty() {
            out.push_str(&format!("  goal: {}\n", plan.text("goal")));
        }
        if !plan.text("updated_at").is_empty() {
            out.push_str(&format!("  updated: {}\n", plan.text("updated_at")));
        }
        out.push_str(&format!("  by state: {}\n", counts(plan.get("by_state"))));
        let contract = plan.get("contract");
        if contract.get("bound").truth() && !contract.get("satisfied").truth() {
            out.push_str(&format!("  PROJECT contract: BLOCKED ({}; inspect: oms agent-plan --repo . status --json)\n",contract.text("blocker")));
        }
        if let Ok(actionable) = plan.get("actionable").strings()
            && !actionable.is_empty()
        {
            out.push_str(&format!(
                "  actionable now: {}\n",
                format_actionable(&actionable)
            ));
        }
    } else {
        out.push_str("\n## Plan: none\n");
        if plan.get("latest_retirement").truth() {
            out.push_str(&format!(
                "  latest retired: {} {}\n",
                plan.get("latest_retirement").text("disposition"),
                plan.get("latest_retirement")
                    .text("plan_sha256")
                    .chars()
                    .take(12)
                    .collect::<String>()
            ));
        }
    }
    let board = state.get("board");
    if board.get("present").truth() {
        out.push_str("\n## Experiment board\n");
        let rows = board.get("active").array()?;
        if rows.is_empty() {
            out.push_str("  no active experiments\n");
        }
        for row in rows {
            out.push_str(&format!(
                "  {:<8} {} owner={}{}\n",
                row.text("status"),
                row.text("id"),
                row.text("owner"),
                if board.get("stale").array()?.contains(row) {
                    " STALE"
                } else {
                    ""
                }
            ));
        }
    }
    let runs = state.get("runs");
    out.push_str("\n## Runs\n");
    let current = runs.get("current");
    let current = if current.object().is_ok() {
        current.text("run_id")
    } else {
        current.str().unwrap_or("none")
    };
    out.push_str(&format!("  current: {current}\n"));
    if let Ok(rows) = runs.get("open").array()
        && !rows.is_empty()
    {
        out.push_str(&format!(
            "  open: {}\n",
            rows.iter()
                .map(|r| if r.str().is_ok() {
                    r.str().unwrap_or("")
                } else {
                    r.text("run_id")
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let ops = state.get("agent_operations");
    if n(ops, "total") > 0 || !ops.get("healthy").truth() {
        out.push_str(&format!("\n## Agent operations ({} total: {} executing, {} awaiting owner review ({} older than 3d), {} other active)\n",n(ops,"total"),n(ops,"executing"),n(ops,"review"),n(ops,"review_aged"),(n(ops,"active")-n(ops,"executing")-n(ops,"review")).max(0)));
        if !ops.get("healthy").truth() {
            out.push_str("  CORRUPT lifecycle stream (run: oms agent-events validate)\n");
        }
        if ops.get("by_state").object().is_ok_and(|o| !o.is_empty()) {
            out.push_str(&format!("  by state: {}\n", counts(ops.get("by_state"))));
        }
        for row in ops.get("needs_attention").array().unwrap_or(&[]) {
            out.push_str(&format!(
                "  ATTENTION {} {} {}/{}\n",
                row.text("attempt_id"),
                row.text("state"),
                row.text("tool"),
                row.text("provider")
            ));
        }
    }
    let approvals = state.get("approvals");
    if n(approvals, "pending") > 0 || !approvals.get("healthy").truth() {
        out.push_str(&format!(
            "\n## Approvals ({} pending)\n",
            n(approvals, "pending")
        ));
        if !approvals.get("healthy").truth() {
            out.push_str("  CORRUPT private approval stream (run: oms approval-inbox validate)\n");
        }
        for row in approvals.get("latest_pending").array().unwrap_or(&[]) {
            out.push_str(&format!(
                "  {} v{} {} {}\n",
                row.text("approval_id"),
                helpers::display(row.get("version")),
                row.text("state"),
                row.text("summary")
            ));
        }
    }
    if let Ok(rows) = state.get("delegations").array()
        && !rows.is_empty()
    {
        out.push_str("\n## In-flight delegations\n");
        for row in rows {
            out.push_str(&format!(
                "  {} {}{}  {}  [{}]\n",
                row.text("provider"),
                if row.text("role").is_empty() {
                    String::new()
                } else {
                    format!("role={} ", row.text("role"))
                },
                row.text("id"),
                row.text("started_at"),
                if row.get("live") == &Value::Bool(false) {
                    "ORPHAN (dead pid)"
                } else {
                    "live"
                }
            ));
        }
    }
    let ci = state.get("ci");
    if ci.text("state") != "none" {
        out.push_str(&format!(
            "\n## CI ({})\n",
            if ci.text("branch").is_empty() {
                ci.text("current_branch")
            } else {
                ci.text("branch")
            }
        ));
        if ci.text("state") == "unpushed" {
            out.push_str(&format!(
                "  unpushed: {} commit(s) ahead of {} — push to get CI\n",
                n(ci, "ahead"),
                ci.text("upstream")
            ));
        } else {
            out.push_str(&format!(
                "  {} {}  {}{}\n",
                ci.text("status"),
                ci.text("conclusion"),
                ci.text("sha").chars().take(12).collect::<String>(),
                if ci.text("state") == "current" {
                    String::new()
                } else {
                    format!(
                        "  STALE (HEAD is {})",
                        ci.text("current_sha").chars().take(12).collect::<String>()
                    )
                }
            ));
            if ci.text("state") != "current" {
                out.push_str("  refresh: oms state --refresh-ci\n");
            }
        }
    }
    let landings = state
        .get("landings")
        .get("outstanding")
        .array()
        .unwrap_or(&[]);
    if !landings.is_empty() {
        out.push_str(&format!("\n## Interrupted landings ({})\n", landings.len()));
        for row in landings {
            out.push_str(&format!(
                "  {}  {}\n",
                row.text("landing_id"),
                row.text("ts")
            ));
        }
        out.push_str("  recover: oms patch-land --recover\n");
    }
    let failures = state.get("failures");
    if !failures.get("healthy").truth() {
        out.push_str("\n## Unresolved failures: unavailable or invalid (run: oms fail-ledger list --unresolved)\n");
    } else if n(failures, "open_total") > 0 {
        out.push_str(&format!(
            "\n## Unresolved failures ({})\n",
            n(failures, "open_total")
        ));
        for row in failures.get("open").array().unwrap_or(&[]) {
            out.push_str(&format!(
                "  {}  x{}  {}\n",
                row.text("fingerprint"),
                n(row, "count"),
                if row.text("summary").is_empty() {
                    row.text("cmd")
                } else {
                    row.text("summary")
                }
            ));
        }
    }
    let artifacts = state.get("artifacts");
    out.push_str(&format!(
        "\n## Artifacts ({} total)\n",
        n(artifacts, "total")
    ));
    if !artifacts.get("healthy").truth() {
        out.push_str(&format!(
            "  CORRUPT invalid_rows={}\n",
            n(artifacts, "invalid_rows")
        ));
    }
    let c = artifacts.get("counts");
    out.push_str(&format!(
        "  retained outcomes: {}  success={} unresolved={} resolved={}\n",
        n(artifacts, "outcomes_total"),
        n(c, "success"),
        n(c, "unresolved"),
        n(c, "resolved")
    ));
    for row in artifacts.get("latest").array().unwrap_or(&[]) {
        out.push_str(&format!(
            "  {} {}/{} exit={} status={} event={}\n",
            row.text("ts"),
            row.text("kind"),
            row.text("provider"),
            helpers::display(row.get("exit")),
            row.text("status"),
            row.text("event_id")
        ));
    }
    let fleet = fleet::lines(state);
    if !fleet.is_empty() {
        out.push_str("\n## Fleet\n");
        for line in fleet {
            out.push_str(&format!("  {line}\n"));
        }
    }
    let guard = state.get("change_guard");
    if guard.get("active").truth() {
        out.push_str(&format!(
            "\n## Change-guard: ACTIVE{}\n",
            if guard.get("stale").truth() {
                " (STALE — abandoned? end it: change-guard.sh end)"
            } else {
                ""
            }
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod panel_snapshot_tests {
    use super::*;

    fn panel_snapshot(report: &Value, conversation: &Value) -> Value {
        super::panel_snapshot(Path::new("test-fixture"), report, conversation)
    }

    #[test]
    fn goal_summary_bounds_evidence_and_readiness_without_granting_authority() {
        let mut report = json::parse(br#"{
            "goal":{"text":"Finish the project","source":"project"},
            "plan":{"healthy":true,"task_count":2,"verified_count":2,"cancelled_count":0},
            "acceptance":{"available":true,"total":12,"counts":{"verified":1},"complete":false,"omitted":2},
            "attention":{"items":[{"code":"stale-plan-claims","summary":"Inspect retained work","priority":"P1","count":1,"command":"private-command"}],
                "next":[{"id":"review-evidence","reason":"Review the remaining criteria","authority":"parent","command":"private-command"}]}
        }"#).unwrap();
        let criterion = json::parse(br#"{
            "id":"criterion-a","text":"Check the integrated result","source":"project","status":"inconclusive","plan_task_id":"task-a",
            "evidence":[
                {"evidence_ref":"older-a"},{"evidence_ref":"older-b"},
                {"evidence_ref":"/private-source","kind":"review","status":"inconclusive"},
                {"evidence_ref":"task-verification:task-a","kind":"task-verification","status":"stale","stale":true},
                {"evidence_ref":"evidence-a","kind":"acceptance","status":"verified","stale":false,"source":"private-source"}]
        }"#).unwrap();
        let mut acceptance = report.get("acceptance").clone();
        acceptance
            .set("criteria", Value::Array(vec![criterion.clone(); 10]))
            .unwrap();
        report.set("acceptance", acceptance).unwrap();
        let task = json::parse(br#"{"id":"task-a","title":"Existing issue task","state":"ready","issue":"example/project#7","selection_eligible":false,"reason":"brief-required","brief":"private-brief"}"#).unwrap();
        let mut plan = report.get("plan").clone();
        plan.set("task_readiness", Value::Array(vec![task; 10]))
            .unwrap();
        plan.set("task_readiness_omitted", number(2)).unwrap();
        report.set("plan", plan).unwrap();
        let conversation = json::parse(br#"{"conversationBinding":"matched","conversationMain":{"room":"other-room","participant":"main","provider":"codex","state":"joined"}}"#).unwrap();
        let snapshot = panel_snapshot(&report, &conversation);
        let dashboard = snapshot.get("dashboard");
        let acceptance = dashboard.get("acceptance");
        assert_eq!(dashboard.get("planProgress").get("verified"), &number(2));
        assert_eq!(acceptance.get("verified"), &number(1));
        assert_eq!(acceptance.get("complete"), &Value::Bool(false));
        assert_eq!(acceptance.text("scope"), "repository");
        assert!(acceptance.opt("room").is_none());
        let criteria = acceptance.get("criteria").array().unwrap();
        assert_eq!(criteria.len(), 8);
        assert_eq!(acceptance.get("omitted"), &number(4));
        assert_eq!(criteria[0].text("task"), "task-a");
        assert_eq!(criteria[0].get("evidenceOmitted"), &number(2));
        let evidence = criteria[0].get("evidence").array().unwrap();
        assert_eq!(evidence.len(), 3);
        assert_eq!(evidence[0].text("reference"), "evidence-a");
        assert_eq!(evidence[1].get("stale"), &Value::Bool(true));
        assert_eq!(evidence[2].get("reference"), &Value::Null);
        assert_eq!(evidence[2].get("stale"), &Value::Null);
        let readiness = dashboard.get("planReadiness");
        assert_eq!(readiness.text("scope"), "repository");
        assert_eq!(readiness.get("omitted"), &number(4));
        let tasks = readiness.get("tasks").array().unwrap();
        assert_eq!(tasks.len(), 8);
        assert_eq!(tasks[0].text("id"), "task-a");
        assert_eq!(tasks[0].text("issue"), "example/project#7");
        assert_eq!(tasks[0].text("reason"), "brief-required");
        assert_eq!(tasks[0].get("selectionEligible"), &Value::Bool(false));
        assert_eq!(
            dashboard.get("nextStep").text("summary"),
            "Review the remaining criteria"
        );
        assert_eq!(dashboard.get("nextStep").text("scope"), "repository");
        let obs = dashboard.get("observation");
        assert_eq!(obs.text("completeness"), "partial");
        assert_eq!(obs.get("sources").text("plan"), "ok");
        assert_eq!(obs.get("sources").text("acceptance"), "partial");
        assert_eq!(obs.get("evidence").get("omitted"), &number(4));
        assert_eq!(obs.get("evidence").get("readinessOmitted"), &number(4));
        assert_eq!(obs.get("evidence").get("evidenceOmitted"), &number(16));
        let wire = String::from_utf8(json::encoded(&snapshot).unwrap()).unwrap();
        assert!(!wire.contains("private-") && wire.len() < 52_000, "{wire}");

        // Display bounds (8 readiness rows, 3 evidence rows per criterion) do not mark a
        // complete 9-task plan or complete acceptance source as partial.
        let mut complete_report = json::parse(br#"{
            "collection":{"ok":true,"sources":{"state":"ok","goal":"ok","artifacts":"ok","attempts":"ok"}},
            "git":{"available":true,"head":"1234567890abcdef1234567890abcdef12345678"},
            "goal":{"text":"Finish the project","source":"project"},
            "task":{"healthy":true},
            "plan":{"healthy":true,"task_count":9,"verified_count":9,"cancelled_count":0,"task_readiness_omitted":0},
            "attempts":{"available":true,"total":0,"active":0,"executing":0,"by_state":{},"recent":[],"active_recent":[]},
            "operations":{"available":true,"recent":[],"omitted":0},
            "reviews":{"passed":0,"failed":0,"unknown":0},
            "acceptance":{"available":true,"total":1,"counts":{"verified":1},"complete":true,"omitted":0},
            "attention":{"items":[],"omitted":0}
        }"#).unwrap();
        let done_tasks: Vec<Value> = (0..9)
            .map(|i| {
                object([
                    ("id", string(format!("task-{i}"))),
                    ("title", string(format!("Completed task {i}"))),
                    ("state", string("done")),
                    ("issue", Value::Null),
                    ("selection_eligible", Value::Bool(false)),
                    ("reason", string("recorded-done")),
                ])
            })
            .collect();
        let mut complete_plan = complete_report.get("plan").clone();
        complete_plan
            .set("task_readiness", Value::Array(done_tasks))
            .unwrap();
        complete_report.set("plan", complete_plan).unwrap();
        let mut complete_acceptance = complete_report.get("acceptance").clone();
        let mut verified_criterion = criterion.clone();
        verified_criterion
            .set("status", string("verified"))
            .unwrap();
        complete_acceptance
            .set("criteria", Value::Array(vec![verified_criterion]))
            .unwrap();
        complete_report
            .set("acceptance", complete_acceptance)
            .unwrap();
        let complete_snapshot = panel_snapshot(&complete_report, &Value::Null);
        let complete_dashboard = complete_snapshot.get("dashboard");
        assert_eq!(complete_dashboard.get("degraded"), &Value::Bool(false));
        assert_eq!(
            complete_dashboard.get("planReadiness").get("omitted"),
            &number(1)
        );
        let complete_obs = complete_dashboard.get("observation");
        assert_eq!(complete_obs.text("completeness"), "complete");
        assert_eq!(complete_obs.get("sources").text("plan"), "ok");
        assert_eq!(complete_obs.get("sources").text("acceptance"), "ok");
        assert_eq!(complete_obs.get("evidence").get("omitted"), &number(0));
        assert_eq!(
            complete_obs.get("evidence").get("readinessOmitted"),
            &number(1)
        );
        assert_eq!(
            complete_obs.get("evidence").get("evidenceOmitted"),
            &number(2)
        );

        let mut unavailable = report.get("acceptance").clone();
        unavailable.set("available", Value::Bool(false)).unwrap();
        unavailable.set("complete", Value::Bool(true)).unwrap();
        report.set("acceptance", unavailable).unwrap();
        let mut unavailable = report.get("plan").clone();
        unavailable.set("healthy", Value::Bool(false)).unwrap();
        report.set("plan", unavailable).unwrap();
        let snapshot = panel_snapshot(&report, &conversation);
        let dashboard = snapshot.get("dashboard");
        assert_eq!(dashboard.text("goal"), "Finish the project");
        for key in ["complete", "total", "verified", "omitted"] {
            assert_eq!(dashboard.get("acceptance").get(key), &Value::Null, "{key}");
        }
        assert!(
            dashboard
                .get("acceptance")
                .get("criteria")
                .array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(dashboard.get("nextStep"), &Value::Null);
        assert_eq!(
            dashboard.get("planReadiness").get("available"),
            &Value::Bool(false)
        );
        assert!(
            dashboard
                .get("planReadiness")
                .get("tasks")
                .array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn compact_snapshot_binds_room_and_omits_private_or_unbounded_source_columns() {
        let mut report = crate::json::parse(br#"{"kind":"oms-dashboard","schema":1,
            "attempts":{"available":true,"active":1,"active_recent":[{"attempt_id":"main-1","tool":"panel-main","provider":"codex","state":"working","refs":{"panel_harness":"pi","panel_role":"main","panel_room_id":"room-a","panel_label":"Launch note"}}],
            "recent":[{"attempt_id":"historical","tool":"panel-native-assignment","state":"done","refs":{"panel_role":"lead","panel_room":"older"}},{"attempt_id":"bad","parent_attempt_id":"main-1","refs":{"panel_room_id":"room-a","panel_room":"outside"}}]},
            "attention":{"items":[{"code":"stale-plan-claims","summary":"Check results","command":"private command"}],"room_mail":{"bound":true,"healthy":true,"room_id":"room-b"}},
            "acceptance":{"available":true,"total":3,"counts":{"verified":1},"complete":false},
            "reviews":{"passed":2,"failed":1,"unknown":0},
            "operations":{"available":true,"recent":[{"kind":"review","artifact_id":"review-1","exit":1,"status":"failed"}]}}"#).unwrap();
        let raw = "x".repeat(350_000);
        report
            .set(
                "plan",
                object([
                    ("private", string(&raw)),
                    ("healthy", Value::Bool(true)),
                    ("task_count", number(3)),
                    ("verified_count", number(1)),
                    ("cancelled_count", number(1)),
                ]),
            )
            .unwrap();
        report.set("goal", object([("text", string(raw))])).unwrap();
        let projected = panel_snapshot(&report, &Value::Null);
        let d = projected.get("dashboard");
        assert_eq!(d.text("scope"), "repository");
        assert_eq!(d.text("currentRoom"), "room-b");
        assert_eq!(d.get("main"), &Value::Null);
        assert_eq!(d.get("goal"), &Value::Null);
        assert_eq!(d.text("goalAvailability"), "unavailable");
        assert_eq!(
            d.get("planProgress"),
            &object([("total", number(2)), ("verified", number(1))])
        );
        let recent = d.get("attempts").array().unwrap();
        assert_eq!(recent[0].text("role"), "lead");
        assert_eq!(recent[0].text("room"), "older");
        assert_eq!(recent[1].get("room"), &Value::Null);
        assert_eq!(recent[1].get("parentAttempt"), &Value::Null);
        let active = d.get("activeAttempts").array().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].text("room"), "room-a");
        assert_eq!(active[0].text("provider"), "pi");
        assert_eq!(d.get("reviews").get("failed").int().unwrap(), 1);
        assert_eq!(d.get("acceptance").get("verified").int().unwrap(), 1);
        assert_eq!(d.get("tasks").int().unwrap(), 3);
        assert_eq!(d.get("tasksVerified").int().unwrap(), 1);
        assert_eq!(
            d.get("attention").array().unwrap()[0].text("code"),
            "stale-plan-claims"
        );
        assert_eq!(
            d.get("operations").array().unwrap()[0].text("artifactId"),
            "review-1"
        );
        let wire = String::from_utf8(crate::json::encoded(&projected).unwrap()).unwrap();
        assert!(wire.len() < 4096 && !wire.contains("private"));
        report.set("attention", object([])).unwrap();
        report
            .set(
                "plan",
                object([
                    ("healthy", Value::Bool(false)),
                    ("task_count", number(3)),
                    ("verified_count", number(1)),
                ]),
            )
            .unwrap();
        report.set("goal", Value::Null).unwrap();
        let unbound = panel_snapshot(&report, &Value::Null);
        assert_eq!(unbound.get("dashboard").get("goal"), &Value::Null);
        assert_eq!(
            unbound.get("dashboard").text("goalAvailability"),
            "unavailable"
        );
        assert_eq!(unbound.get("dashboard").get("planProgress"), &Value::Null);
        assert_eq!(unbound.get("dashboard").get("tasksVerified"), &Value::Null);
        assert_eq!(unbound.get("dashboard").get("tasks"), &Value::Null);
        assert_eq!(unbound.get("dashboard").get("currentRoom"), &Value::Null);
        assert_eq!(unbound.get("dashboard").get("main").text("id"), "main-1");
        let mut attempts = report.get("attempts").clone();
        let first = attempts.get("active_recent").array().unwrap()[0].clone();
        let mut second = first.clone();
        second.set("attempt_id", string("other-main")).unwrap();
        attempts.set("active", number(2)).unwrap();
        attempts
            .set("active_recent", Value::Array(vec![first, second]))
            .unwrap();
        report.set("attempts", attempts).unwrap();
        let goal = "한".repeat(2000);
        report
            .set("goal", object([("text", string(&goal))]))
            .unwrap();
        let ambiguous = panel_snapshot(&report, &Value::Null);
        assert_eq!(ambiguous.get("dashboard").get("main"), &Value::Null);
        assert_eq!(ambiguous.get("dashboard").text("goal"), goal);
        assert_eq!(
            ambiguous.get("dashboard").text("goalAvailability"),
            "available"
        );
        let before = ambiguous.get("dashboard");
        report
            .set(
                "managedScratch",
                object([
                    ("status", string("ready")),
                    (
                        "sources",
                        Value::Array(vec![object([("id", string("source"))])]),
                    ),
                    (
                        "attempts",
                        Value::Array(vec![object([("label", string("x".repeat(52_000)))])]),
                    ),
                    ("omittedAttempts", number(0)),
                ]),
            )
            .unwrap();
        let capped = panel_snapshot(&report, &Value::Null);
        let after = capped.get("dashboard");
        assert_eq!(after.get("managedScratch").text("status"), "partial");
        assert!(after.get("managedScratch").get("omittedAttempts").is_int(1));
        for key in [
            "active",
            "activeAttempts",
            "attempts",
            "commandStatus",
            "goal",
            "main",
            "currentRoom",
        ] {
            assert_eq!(before.get(key), after.get(key), "{key}");
        }
        assert!(crate::json::encoded(&capped).unwrap().len() < 60_000);

        // When canonical payload leaves bounded headroom below 52 KB, running scratch
        // attempts (and their worktree source label) survive ahead of waiting/finished ones.
        let mut dense = report.clone();
        dense
            .set(
                "attention",
                object([(
                    "items",
                    Value::Array(
                        (0..8)
                            .map(|i| {
                                object([
                                    ("code", string(format!("item-{i}"))),
                                    ("summary", string("s".repeat(500))),
                                    ("count", number(1)),
                                ])
                            })
                            .collect(),
                    ),
                )]),
            )
            .unwrap();
        let pad_sources: Vec<Value> = (0..18)
            .map(|i| {
                object([
                    ("id", string(format!("src-{i:02}"))),
                    ("label", string(format!("wt-{i:012x} (detached)"))),
                    ("head", string("detached")),
                    ("branch", Value::Null),
                    ("status", string("ready")),
                ])
            })
            .collect();
        let mut scratch_attempts: Vec<Value> = (0..17)
            .map(|i| {
                object([
                    ("id", string(format!("{:064x}", i + 1))),
                    ("sourceId", string(format!("src-{i:02}"))),
                    ("provider", string("codex")),
                    ("tool", string("peer-review")),
                    ("state", string(if i % 2 == 0 { "done" } else { "review" })),
                    ("updatedAt", string("2026-11-25T10:00:00Z")),
                    ("pad", string("p".repeat(2900))),
                ])
            })
            .collect();
        // The observation hands attempts over ranked live-first.
        scratch_attempts.insert(
            0,
            object([
                ("id", string("f".repeat(64))),
                ("sourceId", string("src-17")),
                ("provider", string("claude")),
                ("tool", string("peer-delegate")),
                ("state", string("working")),
                ("updatedAt", string("2026-11-20T00:00:00Z")),
                ("pad", string("p".repeat(2900))),
            ]),
        );
        dense
            .set(
                "managedScratch",
                object([
                    ("status", string("ready")),
                    ("sources", Value::Array(pad_sources)),
                    ("attempts", Value::Array(scratch_attempts)),
                    ("omittedAttempts", number(0)),
                ]),
            )
            .unwrap();
        let ranked_snap = panel_snapshot(&dense, &Value::Null);
        let ranked_scratch = ranked_snap.get("dashboard").get("managedScratch");
        assert_eq!(ranked_scratch.text("status"), "partial");
        let kept_attempts = ranked_scratch.get("attempts").array().unwrap();
        assert!(!kept_attempts.is_empty() && kept_attempts.len() < 18);
        assert_eq!(kept_attempts[0].text("state"), "working");
        assert_eq!(kept_attempts[0].text("sourceId"), "src-17");
        assert!(
            ranked_scratch
                .get("sources")
                .array()
                .unwrap()
                .iter()
                .any(|s| s.text("id") == "src-17")
        );
        assert!(crate::json::encoded(&ranked_snap).unwrap().len() < 60_000);
    }

    #[test]
    fn conversation_task_titles_require_the_current_plan_lease_and_room_hierarchy() {
        use serde_json::json;
        let title = "승인 화면 간소화";
        let task = json!({"id":"work","title":title,"state":"claimed","lease_id":"l1","claimed_by_participant":"owner"});
        let plan = json!({"plan_id":"current","tasks":{"work":task}});
        let member = json!({"room":"here","participant":"assignment","parent":"owner","role":"worker","joined":true,"provider":"codex"});
        let refs =
            json!({"panel_role":"main","panel_room_id":"here","plan_id":"current","lease_id":"l1"});
        let main = json!({"attempt_id":"owner","tool":"panel-main","provider":"codex","terminal":false,"task_id":"work","refs":refs});
        let assigned = json!({"attempt_id":"assignment","tool":"panel-native-assignment","provider":"codex","parent_attempt_id":"owner","task_id":"work",
            "refs":{"native_assignment_schema":"1","panel_room_id":"here","plan_id":"current","lease_id":"l1"}});
        let joined = json!({"state":"joined","provider":"codex","participant":"owner","room":"here","participants":[member]});
        let state = json!({"plan":{"healthy":true},"conversation":{"conversationBinding":"matched","conversationMain":joined,"attempts":[main,assigned]}});
        let raw = serde_json::to_vec(&plan).unwrap();
        let digest = json::sha(&raw);
        let snapshot = |mut state: serde_json::Value, plan: &serde_json::Value, pinned: &str| {
            state["plan"]["plan_sha256"] = json!(pinned);
            let state = json::parse(&serde_json::to_vec(&state).unwrap()).unwrap();
            let raw = serde_json::to_vec(plan).unwrap();
            let mut conversation = state.get("conversation").clone();
            if let Some(titles) =
                conversation_task_titles(&state, &raw, &json::parse(&raw).unwrap())
            {
                conversation.set("taskTitles", titles).unwrap();
            }
            panel_snapshot(&Value::Null, &conversation)
        };
        let ok = snapshot(state.clone(), &plan, &digest);
        let dashboard = ok.get("dashboard");
        for row in dashboard.get("conversationAttempts").array().unwrap() {
            assert_eq!(row.text("task"), "work");
            assert_eq!(row.text("taskTitle"), title);
        }
        assert!(dashboard.opt("taskTitles").is_none());
        let mut explicit_lead = state.clone();
        let lead = &mut explicit_lead["conversation"]["attempts"][1];
        lead["tool"] = json!("peer-delegate");
        lead["refs"]["panel_role"] = json!("lead");
        lead["refs"]["panel_room_participant"] = json!("assignment");
        let explicit = snapshot(explicit_lead, &plan, &digest);
        assert_eq!(
            explicit
                .get("dashboard")
                .get("conversationAttempts")
                .array()
                .unwrap()[1]
                .text("taskTitle"),
            title
        );
        let tainted = concat!("/ho", "me/example/private");
        for (field, value) in [
            ("/refs/plan_id", "previous"),
            ("/refs/lease_id", "previous"),
            ("/refs/panel_room_id", "elsewhere"),
            ("/task_id", "missing"),
            ("/parent_attempt_id", "other"),
            ("/attempt_id", "unknown-member"),
            ("/provider", "other"),
            ("/state", "cancelled"),
            ("/refs/panel_role", "reviewer"),
        ] {
            let mut different = state.clone();
            let assignment = &mut different["conversation"]["attempts"][1];
            if let Some(key) = field.strip_prefix("/refs/") {
                assignment["refs"][key] = json!(value);
            } else {
                assignment[&field[1..]] = json!(value);
            }
            let got = snapshot(different, &plan, &digest);
            let rows = got
                .get("dashboard")
                .get("conversationAttempts")
                .array()
                .unwrap();
            assert_eq!(rows[0].text("taskTitle"), title);
            assert!(rows[1].opt("taskTitle").is_none(), "{field}");
        }
        for (key, value, fresh) in [
            ("title", "다른 계획", false),
            ("state", "cancelled", true),
            ("claimed_by_participant", "other", true),
            ("title", "private\ntext", true),
            ("title", tainted, true),
        ] {
            let mut changed = plan.clone();
            changed["tasks"]["work"][key] = json!(value);
            let pin = if fresh {
                json::sha(serde_json::to_vec(&changed).unwrap())
            } else {
                digest.clone()
            };
            let got = snapshot(state.clone(), &changed, &pin);
            for row in got
                .get("dashboard")
                .get("conversationAttempts")
                .array()
                .unwrap()
            {
                assert!(row.opt("taskTitle").is_none(), "{key}");
                assert_eq!(row.text("task"), "work");
            }
        }
    }

    #[test]
    fn active_main_requires_unambiguous_room_tags_when_mail_is_bound() {
        let mut report = json::parse(
            br#"{"attention":{"room_mail":{"bound":true,"healthy":true,"room_id":"work"}}}"#,
        )
        .unwrap();
        for (refs, matches) in [
            (object([("panel_room", string("work"))]), true),
            (
                object([
                    ("panel_room_id", string("work")),
                    ("panel_room", string("other")),
                ]),
                false,
            ),
            (
                object([
                    ("panel_room_id", string("work")),
                    ("panel_room", Value::Null),
                ]),
                false,
            ),
        ] {
            report
                .set(
                    "attempts",
                    object([
                        ("available", Value::Bool(true)),
                        ("active", number(1)),
                        (
                            "active_recent",
                            Value::Array(vec![object([
                                ("tool", string("panel-main")),
                                ("attempt_id", string("main")),
                                ("refs", refs),
                            ])]),
                        ),
                    ]),
                )
                .unwrap();
            let snapshot = panel_snapshot(&report, &Value::Null);
            let main = snapshot.get("dashboard").get("main");
            assert_eq!(main.text("room") == "work", matches);
            assert_eq!(main == &Value::Null, !matches);
        }
    }

    #[test]
    fn omitted_goal_or_inconsistent_plan_does_not_establish_progress() {
        let mut report = json::parse(
            br#"{"goal":null,"collection":{"sources":{"goal":"ok"}},
            "plan":{"healthy":true,"task_count":3,"verified_count":1}}"#,
        )
        .unwrap();
        for cancelled in [
            None,
            Some(number(-1)),
            Some(number(3)),
            Some(number(4)),
            Some(string("1")),
        ] {
            if let Some(value) = cancelled {
                let mut plan = report.get("plan").clone();
                plan.set("cancelled_count", value).unwrap();
                report.set("plan", plan).unwrap();
            }
            let snapshot = panel_snapshot(&report, &Value::Null);
            let dashboard = snapshot.get("dashboard");
            assert_eq!(dashboard.text("goalAvailability"), "absent");
            assert_eq!(dashboard.get("planProgress"), &Value::Null);
            assert!(dashboard.get("tasks").is_int(3));
        }
        report.set("goal", json::empty()).unwrap();
        assert_eq!(
            panel_snapshot(&report, &Value::Null)
                .get("dashboard")
                .text("goalAvailability"),
            "unavailable"
        );
    }

    #[test]
    fn observation_revision_is_stable_across_wall_clock_and_tracks_scope_state_and_partiality() {
        let base = || {
            crate::json::parse(br#"{
                "kind":"oms-dashboard","schema":1,
                "generated_at":"2026-11-24T10:00:00Z",
                "repo":{"name":"demo","branch":"main","head":"1234567890ab"},
                "git":{"available":true,"head":"1234567890abcdef1234567890abcdef12345678","branch":"main","upstream":"origin/main","ahead":0,"behind":0,"dirty":false},
                "coverage":"OMS-managed only",
                "collection":{"ok":true,"sources":{"state":"ok","artifacts":"ok","attempts":"ok"}},
                "goal":{"text":"Ship bounded observation","source":"task"},
                "task":{"healthy":true,"present":true,"status":"active"},
                "plan":{"healthy":true,"present":true,"task_count":2,"verified_count":1},
                "attempts":{"available":true,"total":1,"active":1,"executing":1,"by_state":{"working":1},
                    "active_recent":[{"attempt_id":"att-1","tool":"panel-main","provider":"codex","state":"working","updated_at":"2026-11-24T09:55:00Z","refs":{"panel_role":"main","panel_room_id":"room-a"}}],
                    "recent":[{"attempt_id":"att-1","tool":"panel-main","provider":"codex","state":"working","updated_at":"2026-11-24T09:55:00Z","refs":{"panel_role":"main","panel_room_id":"room-a"}}]},
                "operations":{"available":true,"recent":[{"kind":"verify","artifact_id":"art-1","exit":0,"status":"success","ts":"2026-11-24T09:50:00Z"}],"omitted":0},
                "reviews":{"passed":1,"failed":0,"unknown":0},
                "acceptance":{"available":true,"total":2,"counts":{"verified":1},"complete":false,"criteria":[],"omitted":0,"note":"worker completion is not acceptance"},
                "attention":{"items":[],"omitted":0}
            }"#).unwrap()
        };
        let first_report = base();
        let first = panel_snapshot(&first_report, &Value::Null);
        let first_obs = first.get("dashboard").get("observation");
        assert_eq!(first_obs.get("schema").int().unwrap(), 1);
        assert_eq!(first_obs.text("kind"), "multi-source-observation");
        assert_eq!(first_obs.get("atomic"), &Value::Bool(false));
        assert_eq!(first_obs.text("completeness"), "complete");
        assert_eq!(first_obs.text("observedAt"), "2026-11-24T10:00:00Z");
        assert_eq!(first_obs.get("evidence").text("scope"), "repository");
        assert_eq!(first_obs.get("evidence").text("head"), "1234567890ab");
        assert_eq!(first_obs.get("evidence").text("latestArtifact"), "art-1");
        assert_eq!(first_obs.get("evidence").get("omitted").int().unwrap(), 0);
        let rev = first_obs.text("revision");
        assert!(rev.starts_with("obs-v1:") && rev.len() == 31, "{rev}");

        // Wall-clock-only change advances observedAt without changing revision.
        let mut later_report = base();
        later_report
            .set("generated_at", string("2026-11-24T10:09:45Z"))
            .unwrap();
        let later = panel_snapshot(&later_report, &Value::Null);
        assert_eq!(
            later.get("dashboard").get("observation").text("revision"),
            rev
        );
        assert_eq!(
            later.get("dashboard").get("observation").text("observedAt"),
            "2026-11-24T10:09:45Z"
        );

        // Unprojected private data does not alter the observation revision.
        let mut noisy = base();
        let mut plan = noisy.get("plan").clone();
        plan.set("private_note", string("secret-unprojected"))
            .unwrap();
        noisy.set("plan", plan).unwrap();
        assert_eq!(
            panel_snapshot(&noisy, &Value::Null)
                .get("dashboard")
                .get("observation")
                .text("revision"),
            rev
        );

        // Changed state in the selected scope changes revision.
        let mut changed = base();
        let mut plan = changed.get("plan").clone();
        plan.set("verified_count", number(2)).unwrap();
        changed.set("plan", plan).unwrap();
        assert_ne!(
            panel_snapshot(&changed, &Value::Null)
                .get("dashboard")
                .get("observation")
                .text("revision"),
            rev
        );

        // Conversation-bound scope differs from repository-only and foreign room scopes.
        let conv_a = crate::json::parse(br#"{"conversationBinding":"matched","conversationMain":{"provider":"codex","room":"room-a","participant":"owner","source":"room-session","state":"joined","incomplete":false,"participantsOmitted":0,"participants":[]}}"#).unwrap();
        let conv_b = crate::json::parse(br#"{"conversationBinding":"matched","conversationMain":{"provider":"codex","room":"room-b","participant":"owner","source":"room-session","state":"joined","incomplete":false,"participantsOmitted":0,"participants":[]}}"#).unwrap();
        let bound_a = panel_snapshot(&first_report, &conv_a);
        let bound_b = panel_snapshot(&first_report, &conv_b);
        let obs_a = bound_a.get("dashboard").get("observation");
        let obs_b = bound_b.get("dashboard").get("observation");
        assert_eq!(obs_a.get("evidence").text("scope"), "repository");
        assert!(obs_a.get("evidence").opt("room").is_none());
        assert_ne!(obs_a.text("revision"), rev);
        assert_ne!(obs_a.text("revision"), obs_b.text("revision"));
        // Same bound scope across wall-clock read times keeps the same revision.
        assert_eq!(
            panel_snapshot(&later_report, &conv_a)
                .get("dashboard")
                .get("observation")
                .text("revision"),
            obs_a.text("revision")
        );

        // Partial/degraded source retains a distinct partial revision and names the gap.
        let mut partial_report = base();
        partial_report
            .set(
                "collection",
                object([
                    ("ok", Value::Bool(false)),
                    ("sources", object([("artifacts", string("invalid"))])),
                ]),
            )
            .unwrap();
        partial_report
            .set(
                "operations",
                object([
                    ("available", Value::Bool(false)),
                    ("recent", Value::Array(vec![])),
                    ("omitted", number(0)),
                ]),
            )
            .unwrap();
        let partial = panel_snapshot(&partial_report, &Value::Null);
        let partial_obs = partial.get("dashboard").get("observation");
        assert_eq!(partial_obs.text("completeness"), "partial");
        assert_eq!(partial_obs.get("sources").text("artifacts"), "unavailable");
        assert!(partial_obs.text("revision").starts_with("obs-v1:"));
        assert_ne!(partial_obs.text("revision"), rev);

        // CLI text rendering surfaces the same revision and non-atomic/unavailable markers.
        partial_report
            .set("observation", partial_obs.clone())
            .unwrap();
        let rendered = dashboard_text(&partial_report).unwrap();
        assert!(
            rendered.contains(&format!(
                "observation: {} (multi-source, non-atomic, partial)  observed=2026-11-24T10:00:00Z",
                partial_obs.text("revision")
            )),
            "{rendered}"
        );
        assert!(
            rendered.contains("observation source gaps: artifacts"),
            "{rendered}"
        );

        // A mixed source retains its recorded evidence, and stale claims require native plan evidence.
        let mut mixed = first_report.clone();
        let mut operations = mixed.get("operations").clone();
        operations.set("available", Value::Bool(false)).unwrap();
        mixed.set("operations", operations).unwrap();
        let mut plan = mixed.get("plan").clone();
        plan.set("stale_claims", number(1)).unwrap();
        mixed.set("plan", plan).unwrap();
        let snapshot = panel_snapshot(&mixed, &Value::Null);
        let obs = snapshot.get("dashboard").get("observation");
        assert_eq!(obs.text("completeness"), "partial");
        assert_eq!(obs.get("sources").text("artifacts"), "partial");
        assert_eq!(obs.get("sources").text("plan"), "stale");
        assert_eq!(obs.get("evidence").text("latestArtifact"), "art-1");
        let same_contents_elsewhere = super::panel_snapshot(
            Path::new("different/test-fixture"),
            &first_report,
            &Value::Null,
        );
        assert_ne!(
            same_contents_elsewhere
                .get("dashboard")
                .get("observation")
                .text("revision"),
            rev
        );
        // Completely unknown/unreadable sources emit unknown completeness and null revision.
        let unknown = panel_snapshot(&crate::json::empty(), &Value::Null);
        let unknown_obs = unknown.get("dashboard").get("observation");
        assert_eq!(unknown_obs.text("completeness"), "unknown");
        assert_eq!(unknown_obs.get("revision"), &Value::Null);
        assert_eq!(unknown_obs.get("atomic"), &Value::Bool(false));
    }
}
