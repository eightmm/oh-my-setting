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
fn git_status(status: &str) -> (String, String, String, Option<u64>) {
    let mut head = String::new();
    let mut branch = String::new();
    let mut upstream = String::new();
    let mut ahead = None;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("# branch.oid ") {
            if value != "(initial)" {
                head = value.into();
            }
        } else if let Some(value) = line.strip_prefix("# branch.head ") {
            if value != "(detached)" {
                branch = value.into();
            }
        } else if let Some(value) = line.strip_prefix("# branch.upstream ") {
            upstream = value.into();
        } else if let Some(value) = line.strip_prefix("# branch.ab +") {
            ahead = value.split_whitespace().next().and_then(|n| n.parse().ok());
        }
    }
    (head, branch, upstream, ahead)
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
    let mut resolved = BTreeSet::new();
    let mut previous = HashMap::new();
    for row in &rows {
        let target = previous.get(row.text("resolves_event_id")).copied();
        previous.entry(row.text("event_id")).or_insert(row);
        if !valid(row) {
            bad += 1;
            continue;
        }
        if row.text("kind") == "artifact-resolution" {
            if target.is_some_and(|t| {
                valid(t)
                    && t.get("schema").is_int(1)
                    && t.get("exit").int().is_ok_and(|n| n > 0)
                    && t.text("kind") != "artifact-resolution"
                    && t.text("operation_id") == row.text("operation_id")
                    && t.text("artifact_id") == row.text("artifact_id")
                    && row.get("exit").is_int(0)
                    && row.text("resolution") == "resolved"
                    && row.text("parent_event_id") == t.text("event_id")
            }) {
                resolved.insert(row.text("resolves_event_id").to_owned());
            } else {
                bad += 1;
            }
        }
    }
    Ok((rows, resolved, bad))
}
pub(super) fn enrich(ctx: &Context<'_>, host: &dyn HostServices, state: &mut Value) -> Result<()> {
    let task_text = storage::read(
        &ctx.repo,
        Path::new(".oms/task/current.md"),
        storage::MAX_FILE,
        true,
    )
    .unwrap_or_default();
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
    task.set(
        "goal",
        string(goal.join(" ").chars().take(200).collect::<String>()),
    )?;
    task.set(
        "next",
        string(next.join(" ").chars().take(200).collect::<String>()),
    )?;
    state.set("task", task)?;
    let plan = file(ctx, ".oms/plan/tasks.json");
    let mut projected = state.get("plan").clone();
    if let Ok(plan) = &plan {
        projected.set("goal", plan.get("goal").clone())?;
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
    let (arts, resolved, bad) = artifact_rows(ctx)?;
    let mut counts = object([
        ("success", number(0)),
        ("resolved", number(0)),
        ("unresolved", number(0)),
    ]);
    let mut outcomes = Vec::new();
    for mut row in arts
        .iter()
        .filter(|r| r.text("kind") != "artifact-resolution")
        .cloned()
    {
        let status = if row.get("exit").is_int(0) {
            "success"
        } else if resolved.contains(row.text("event_id")) {
            "resolved"
        } else {
            "unresolved"
        };
        counts.set(status, number(counts.get(status).int()? + 1))?;
        row.set("status", string(status))?;
        outcomes.push(row);
    }
    state.set(
        "artifacts",
        object([
            ("healthy", Value::Bool(bad == 0)),
            ("invalid_rows", number(bad)),
            ("total", number(arts.len())),
            ("outcomes_total", number(outcomes.len())),
            ("resolution_events", number(resolved.len())),
            ("counts", counts),
            (
                "latest",
                Value::Array(outcomes.into_iter().rev().take(5).collect()),
            ),
        ]),
    )?;
    let status = query(ctx, &["git", "status", "--porcelain=v2", "--branch"]);
    let (head, branch, upstream, ahead) = git_status(&status);
    let (ci_rows, _) = rows(ctx, ".oms/ci.jsonl");
    let mut ci = ci_rows.last().cloned().unwrap_or_else(json::empty);
    let fresh = !head.is_empty()
        && !ci.text("sha").is_empty()
        && (head.starts_with(ci.text("sha")) || ci.text("sha").starts_with(&head));
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
    let current = storage::read(&ctx.repo, Path::new(".oms/threads/CURRENT"), 4096, true)?;
    let current = String::from_utf8_lossy(&current)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_owned();
    let thread_ttl = std::env::var("OMS_THREAD_STALE_TTL")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(86400);
    for path in helpers::shallow_files(ctx, &ctx.repo.join(".oms/threads"), ".jsonl", 50000)? {
        let (rows, bad) = storage::rows(&ctx.repo, &path)?;
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
    let storage_health = oms_command_journal::cold::journal_storage(&ctx.repo).to_json();
    let storage_value = storage_health
        .into_value()
        .map_err(|e| error(2, format!("{e:?}")))?;
    let storage_bytes =
        oms_json::canonical_json(&storage_value).map_err(|e| error(2, format!("{e:?}")))?;
    let mut journal = json::parse(&storage_bytes)?;
    let status = state.get("work_journal").clone();
    for (k, v) in status.object().unwrap_or(&[]) {
        journal.set(std::str::from_utf8(k).unwrap_or(""), v.clone())?;
    }
    let unsealed = oms_command_journal::cold::replay_snapshot(&ctx.repo.join(".oms/work-journal"))
        .ok()
        .map(|(_, rows, _)| {
            rows.iter()
                .filter(|r| {
                    r.get("local_date")
                        .and_then(|v| v.as_string())
                        .and_then(|s| s.to_str_checked())
                        .is_some_and(|date| {
                            date.get(..7)
                                .is_some_and(|m| m < &timestamp(epoch()).unwrap_or_default()[..7])
                        })
                })
                .count()
        })
        .unwrap_or(0);
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
    let runtime = if let Some(canonical) = canonical {
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
            (
                "objective",
                object([
                    ("text", state.get("task").get("goal").clone()),
                    ("source", string("task")),
                ]),
            ),
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
    Ok(())
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
        let raw = storage::read(
            &ctx.repo,
            &PathBuf::from(format!(".oms/{family}/CURRENT")),
            4096,
            true,
        )?;
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

pub(super) fn dashboard(ctx: &Context<'_>, state: &Value) -> Result<Value> {
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
    let (goal, source) = if !runtime.get("objective").text("text").is_empty() {
        (
            runtime.get("objective").text("text"),
            runtime.get("objective").text("source"),
        )
    } else if !task.text("goal").is_empty() {
        (task.text("goal"), "task")
    } else {
        (plan.text("goal"), "plan")
    };
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
    let (artifacts, _, bad) = artifact_rows(ctx)?;
    let outcomes = artifacts
        .into_iter()
        .filter(|r| r.text("kind") != "artifact-resolution")
        .collect::<Vec<_>>();
    let mut reviews = object([
        ("outcomes", number(0)),
        ("passed", number(0)),
        ("failed", number(0)),
        ("unknown", number(0)),
        ("seat_answers", number(0)),
    ]);
    for row in &outcomes {
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
            Value::Array(outcomes.iter().rev().take(8).cloned().collect()),
        ),
        ("omitted", number(outcomes.len().saturating_sub(8))),
    ]);
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
    Ok(object([
        ("schema", number(1)),
        ("kind", string("oms-dashboard")),
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
                            "artifacts",
                            string(if bad == 0 {
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
    ]))
}
/// Compact public columns before command capture. Omit oversized strings whole
/// so that the MCP outbound scanner never receives a credential prefix.
pub(super) fn panel_snapshot(report: &Value, conversation: &Value) -> Value {
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
    fn attempt(row: &Value) -> Value {
        let refs = row.get("refs");
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
            ("room", label(refs.get("panel_room_id"), 128)),
            ("task", label(row.get("task_id"), 192)),
            ("state", label(row.get("state"), 64)),
            ("updated", label(row.get("updated_at"), 64)),
        ])
    }
    let attempts = report.get("attempts");
    let active = attempts.get("active_recent");
    let mail = report.get("attention").get("room_mail");
    let current_room = if mail.get("bound").truth() && mail.get("healthy").truth() {
        label(mail.get("room_id"), 128)
    } else {
        Value::Null
    };
    let window = active.array().unwrap_or(&[]);
    let mut candidates = window.iter().filter(|r| {
        r.text("tool") == "panel-main"
            && (current_room == Value::Null || r.get("refs").get("panel_room_id") == &current_room)
    });
    let first = candidates.next();
    // A partial repository-wide window cannot establish uniqueness.
    let main = first.filter(|_| {
        candidates.next().is_none()
            && (current_room != Value::Null
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
    let plan = report.get("plan");
    let acceptance = report.get("acceptance");
    let operations = report.get("operations");
    let reviews = report.get("reviews");
    let dashboard = object([
        ("scope", string("repository")),
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
        ("currentRoom", current_room),
        ("project", label(report.get("repo").get("name"), 255)),
        ("branch", label(report.get("repo").get("branch"), 255)),
        ("updated", label(report.get("generated_at"), 64)),
        (
            "goal",
            if goal != Value::Null {
                goal
            } else {
                main.map(|v| goal_text(v.get("refs").get("panel_label")))
                    .unwrap_or(Value::Null)
            },
        ),
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
        (
            "tasks",
            if plan.get("healthy").truth() {
                count(plan.get("task_count"))
            } else {
                Value::Null
            },
        ),
        ("taskStatus", label(report.get("task").get("status"), 96)),
        ("attempts", rows(attempts.get("recent"), 16, attempt)),
        ("activeAttempts", rows(active, 8, attempt)),
        (
            "attention",
            rows(report.get("attention").get("items"), 8, |v| {
                object([
                    ("summary", label(v.get("summary"), 512)),
                    ("count", count(v.get("count"))),
                ])
            }),
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
                ("available", acceptance.get("available").clone()),
                ("total", count(acceptance.get("total"))),
                ("verified", count(acceptance.get("counts").get("verified"))),
                ("complete", acceptance.get("complete").clone()),
            ]),
        ),
    ]);
    object([
        ("schema", number(1)),
        ("kind", string("oms-panel-snapshot")),
        ("dashboard", dashboard),
    ])
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
    ];
    for (source, status) in report.get("collection").get("sources").object()? {
        if status.str() != Ok("ok") {
            lines.push(format!(
                "COLLECTION FAILED {}: {}",
                String::from_utf8_lossy(source),
                helpers::display(status)
            ));
        }
    }
    let goal = report.get("goal");
    lines.push(format!(
        "goal: {}",
        if goal.truth() {
            format!("{} ({})", goal.text("text"), goal.text("source"))
        } else {
            "none recorded".into()
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
        out.push_str("\n## Active task\n");
        if !task.get("healthy").truth() {
            out.push_str("  status unavailable or invalid (run: oms agent-task status --json)\n");
        }
        for key in ["goal", "next"] {
            if !task.text(key).is_empty() {
                out.push_str(&format!("  {key}: {}\n", task.text(key)));
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
        out.push_str(&format!("  by state: {}\n", counts(plan.get("by_state"))));
        let contract = plan.get("contract");
        if contract.get("bound").truth() && !contract.get("satisfied").truth() {
            out.push_str(&format!("  PROJECT contract: BLOCKED ({}; inspect: oms agent-plan --repo . status --json)\n",contract.text("blocker")));
        }
        if let Ok(actionable) = plan.get("actionable").strings()
            && !actionable.is_empty()
        {
            out.push_str(&format!("  actionable now: {}\n", actionable.join(", ")));
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

    #[test]
    fn compact_snapshot_binds_room_and_omits_private_or_unbounded_source_columns() {
        let mut report = crate::json::parse(br#"{"kind":"oms-dashboard","schema":1,
            "attempts":{"available":true,"active":1,"active_recent":[{"attempt_id":"main-1","tool":"panel-main","provider":"codex","state":"working","refs":{"panel_harness":"pi","panel_role":"main","panel_room_id":"room-a"}}],
            "recent":[{"attempt_id":"historical","tool":"panel-native-assignment","state":"done","refs":{"panel_role":"lead","panel_room_id":"older"}}]},
            "attention":{"items":[{"summary":"Check results","command":"private command"}],"room_mail":{"bound":true,"healthy":true,"room_id":"room-b"}},
            "acceptance":{"available":true,"total":3,"counts":{"verified":1},"complete":false},
            "reviews":{"passed":2,"failed":1,"unknown":0},
            "operations":{"available":true,"recent":[{"kind":"review","artifact_id":"review-1","exit":1,"status":"failed"}]}}"#).unwrap();
        let raw = "x".repeat(350_000);
        report
            .set("plan", object([("private", string(&raw))]))
            .unwrap();
        report.set("goal", object([("text", string(raw))])).unwrap();
        let projected = panel_snapshot(&report, &Value::Null);
        let d = projected.get("dashboard");
        assert_eq!(d.text("scope"), "repository");
        assert_eq!(d.text("currentRoom"), "room-b");
        assert_eq!(d.get("main"), &Value::Null);
        assert_eq!(d.get("goal"), &Value::Null);
        assert_eq!(d.get("attempts").array().unwrap()[0].text("role"), "lead");
        let active = d.get("activeAttempts").array().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].text("room"), "room-a");
        assert_eq!(active[0].text("provider"), "pi");
        assert_eq!(d.get("reviews").get("failed").int().unwrap(), 1);
        assert_eq!(d.get("acceptance").get("verified").int().unwrap(), 1);
        assert_eq!(
            d.get("operations").array().unwrap()[0].text("artifactId"),
            "review-1"
        );
        let wire = String::from_utf8(crate::json::encoded(&projected).unwrap()).unwrap();
        assert!(wire.len() < 4096 && !wire.contains("private"));
        report.set("attention", object([])).unwrap();
        let unbound = panel_snapshot(&report, &Value::Null);
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
    }
}
