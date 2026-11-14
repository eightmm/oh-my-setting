//! Retention decisions use durable references and canonical writer locks.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, epoch, error, failures, helpers, json, json_output,
    parse_timestamp, require, storage, success, timestamp,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
pub(crate) const HELP: &str = "Usage: oms gc [--repo PATH] [--days N] [--dry-run|--apply] [--delete-orphan-files]\nDefault is a read-only retention plan. Live, unknown or referenced evidence is retained.\n";
fn age(path: &Path, cutoff: i64) -> bool {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| !m.is_symlink())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|t| (t.as_secs() as i64) < cutoff)
}
fn document(ctx: &Context<'_>, path: &Path) -> Result<Value> {
    json::parse(&storage::read(&ctx.repo, path, storage::MAX_FILE, false)?)
}
fn compact(
    ctx: &Context<'_>,
    path: &Path,
    keep: impl Fn(&Value) -> bool,
    apply: bool,
) -> Result<usize> {
    let _guard = storage::lock(&ctx.repo.join(path))?;
    let raw = storage::read(&ctx.repo, path, storage::MAX_STREAM, true)?;
    let mut output = Vec::new();
    let mut removed = 0;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let retain = match json::parse_legacy(line.trim_ascii(), storage::MAX_FILE) {
            Ok(row) => keep(&row),
            Err(_) => true,
        };
        if retain {
            output.extend_from_slice(line);
        } else {
            removed += 1;
        }
    }
    if apply && removed > 0 {
        storage::replace_locked(&ctx.repo, path, &output)?;
    }

    Ok(removed)
}
fn candidate(
    ctx: &Context<'_>,
    items: &mut Vec<Value>,
    kind: &str,
    path: &Path,
    apply: bool,
) -> Result<()> {
    let before = std::fs::symlink_metadata(path).map_err(|e| error(2, e))?;
    require(!before.is_symlink(), "GC refuses a symlink candidate")?;
    if before.is_dir() {
        let mut pending = vec![path.to_owned()];
        let mut count = 0;
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).map_err(|e| error(2, e))? {
                let entry = entry.map_err(|e| error(2, e))?;
                count += 1;
                require(count <= 50000, "GC directory entry limit exceeded")?;
                let kind = entry.file_type().map_err(|e| error(2, e))?;
                require(
                    kind.is_dir() || kind.is_file(),
                    "GC candidate contains unknown or symlinked evidence; retained",
                )?;
                if kind.is_dir() {
                    pending.push(entry.path());
                }
            }
        }
    }
    if apply {
        storage::remove(&ctx.repo, path, &before)?;
    }
    items.push(object([
        ("kind", string(kind)),
        (
            "path",
            string(
                path.strip_prefix(&ctx.repo)
                    .unwrap_or(path)
                    .to_string_lossy(),
            ),
        ),
    ]));
    Ok(())
}
fn plan(ctx: &Context<'_>, req: &oms_command_plan::Request) -> Result<oms_command_plan::Response> {
    let mut context = oms_command_plan::Context::from_environment(req, &ctx.repo)?;
    context.state_repo = ctx.repo.clone();
    context.plan_file = ctx.repo.join(".oms/plan/tasks.json");
    let services = oms_command_plan::UnavailableServices;
    oms_command_plan::Plan::open(context, &services, ctx.interrupted)?.execute(req)
}
pub(crate) fn run(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let opts = Options::parse(
        args,
        &["repo", "days"],
        &["dry-run", "apply", "delete-orphan-files", "json"],
    )?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    require(
        opts.pos.is_empty() && !(opts.flag("apply") && opts.flag("dry-run")),
        "choose --dry-run or --apply",
    )?;
    let repo = opts.repo(ctx)?;
    let ctx = Context {
        cwd: repo.clone(),
        repo: repo.clone(),
        state_repo: repo,
        runtime: ctx.runtime,
        interrupted: ctx.interrupted,
    };
    let days = opts.number("days", 30)?;
    let seconds = i64::try_from(days)
        .ok()
        .and_then(|n| n.checked_mul(86400))
        .ok_or_else(|| error(2, "retention days out of range"))?;
    let cutoff = epoch().saturating_sub(seconds);
    let apply = opts.flag("apply");
    let mut items = Vec::new();
    let mut deferred = Vec::new();
    if !ctx.repo.join(".oms").exists() {
        return Ok(success("gc: nothing to reclaim\n"));
    }

    let mut quiescent = true;
    for path in helpers::files(&ctx, &ctx.repo.join(".oms/delegations"), ".json", 50000)? {
        let _guard = storage::lock(&path)?;
        let row = match document(&ctx, &path) {
            Ok(r) => r,
            Err(_) => {
                quiescent = false;
                continue;
            }
        };
        let pid = row
            .get("pid")
            .int()
            .ok()
            .or_else(|| row.text("pid").parse().ok())
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| *n > 0)
            .and_then(rustix::process::Pid::from_raw);
        let dead = pid
            .is_some_and(|p| rustix::process::test_kill_process(p) == Err(rustix::io::Errno::SRCH));
        if !dead {
            quiescent = false;
            continue;
        }
        if !row.text("executor_id").is_empty() {
            deferred.push(string("legacy Soul marker retained"));
            continue;
        }
        if !row.text("task_id").is_empty() {
            let data = storage::read(
                &ctx.repo,
                Path::new(".oms/plan/tasks.json"),
                storage::MAX_FILE,
                true,
            )?;
            if !data.is_empty() {
                let data = json::parse(&data)?;
                if let Some(task) = data.get("tasks").opt(row.text("task_id"))
                    && ["claimed", "running"].contains(&task.text("state"))
                {
                    if !row.text("lease_id").is_empty()
                        && row.text("lease_id") == task.text("lease_id")
                    {
                        let mut request =
                            oms_command_plan::Request::new(oms_command_plan::Action::RecoverLease)
                                .with("id", row.text("task_id"))
                                .with("lease-id", row.text("lease_id"))
                                .with("expected-state", task.text("state"))
                                .with("markers-dir", ctx.repo.join(".oms/delegations"));
                        if !apply {
                            request = request.flag("check");
                        }
                        match plan(&ctx, &request) {
                            Ok(response) if response.code == 0 => {}
                            _ => {
                                deferred
                                    .push(string("orphan task recovery unproven; marker retained"));
                                continue;
                            }
                        }
                    }
                }
            }
        }
        let _markers = storage::lock(&ctx.repo.join(".oms/delegations/.marker-set-lock-target"))?;
        if document(&ctx, &path)? == row {
            candidate(&ctx, &mut items, "orphan-delegation", &path, apply)?;
        }
    }
    for (folder, suffix, kind) in [
        (".oms/task/archive", ".md", "task-archive"),
        (".oms/handoffs", ".md", "handoff"),
    ] {
        for path in helpers::files(&ctx, &ctx.repo.join(folder), suffix, 50000)? {
            if path.parent() == Some(ctx.repo.join(folder).as_path()) && age(&path, cutoff) {
                let _guard = storage::lock(&path)?;
                if age(&path, cutoff) {
                    candidate(&ctx, &mut items, kind, &path, apply)?;
                }
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(ctx.repo.join(".oms/checkpoints")) {
        for entry in entries {
            let entry = entry.map_err(|e| error(2, e))?;
            let path = entry.path();
            if !entry.file_type().map_err(|e| error(2, e))?.is_dir()
                || !entry.file_name().to_string_lossy().starts_with("cp-")
            {
                continue;
            }
            let _guard = storage::lock(&path.join("meta.json"))?;
            if document(&ctx, &path.join("meta.json"))
                .ok()
                .and_then(|v| parse_timestamp(v.text("created_at")))
                .is_some_and(|t| t < cutoff)
            {
                candidate(&ctx, &mut items, "checkpoint", &path, apply)?;
            }
        }
    }
    if quiescent {
        let _markers = storage::lock(&ctx.repo.join(".oms/delegations/.marker-set-lock-target"))?;
        let path = Path::new(".oms/hooks/events.jsonl");
        let n = compact(
            &ctx,
            path,
            |r| parse_timestamp(r.text("ts")).is_none_or(|t| t >= cutoff),
            apply,
        )?;
        if n > 0 {
            items.push(object([
                ("kind", string("hook-events")),
                ("rows", number(n)),
            ]));
        }
        for path in helpers::files(&ctx, &ctx.repo.join(".oms/hooks/sessions"), "", 50000)? {
            if age(&path, cutoff) {
                let _guard = storage::lock(&path)?;
                if age(&path, cutoff) {
                    candidate(&ctx, &mut items, "hook-session", &path, apply)?;
                }
            }
        }
    } else {
        deferred.push(string(
            "hook and dispatch retention deferred while delegation liveness is unknown or live",
        ));
    }
    let spine = crate::spine::index(&ctx);
    let _spine_guard = storage::lock(&spine)?;
    let (runs, bad) = storage::rows(&ctx.repo, &spine)?;
    require(bad == 0, "malformed spine; run retention refused")?;
    let mut last = BTreeMap::new();
    let mut closed = BTreeSet::new();
    let mut capsule_refs: BTreeMap<String, String> = BTreeMap::new();
    for row in &runs {
        let id = row.text("run_id");
        if id.is_empty() {
            continue;
        }
        if let Some(t) = parse_timestamp(row.text("ts")) {
            let item = last.entry(id.to_owned()).or_insert(t);
            *item = (*item).max(t);
        }
        if row.text("tool") == "oms-run" && row.text("event") == "close" {
            closed.insert(id.to_owned());
        }
        if row.text("tool") == "run-capsule" {
            if let Some(name) = Path::new(row.text("path"))
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
            {
                capsule_refs.insert(name.into(), id.into());
            }
        }
    }
    for (id, last) in &last {
        if *last < cutoff && !closed.contains(id) {
            items.push(object([
                ("kind", string("stale-run-close")),
                ("run_id", string(id)),
            ]));
            if apply {
                let row = object([
                    ("run_id", string(id)),
                    ("tool", string("oms-run")),
                    ("event", string("close")),
                    ("ts", string(timestamp(epoch())?)),
                    ("detail", string(format!("gc: no event in {days}d"))),
                ]);
                storage::append_locked(&ctx.repo, &spine, &row)?;
                closed.insert(id.clone());
            }
        }
    }
    drop(_spine_guard);
    let _claims = storage::lock(&ctx.repo.join(".oms/claims.jsonl"))?;
    let _figures = storage::lock(&ctx.repo.join(".oms/figures.jsonl"))?;
    for path in helpers::files(
        &ctx,
        &crate::capsule::directory(&ctx),
        "capsule.json",
        50000,
    )? {
        let Some(bundle) = path.parent() else {
            continue;
        };
        let id = bundle.file_name().unwrap_or_default().to_string_lossy();
        let run = capsule_refs.get(id.as_ref());
        if run.is_some_and(|r| !closed.contains(r)) || !age(bundle, cutoff) {
            continue;
        }
        let mut pinned = false;
        for ledger in [".oms/claims.jsonl", ".oms/figures.jsonl"] {
            let (refs, bad) = storage::rows(&ctx.repo, Path::new(ledger))?;
            if bad > 0
                || refs.iter().any(|r| {
                    r.text("capsule") == id
                        || r.get("capsules")
                            .strings()
                            .unwrap_or_default()
                            .iter()
                            .any(|s| *s == id)
                })
            {
                pinned = true;
            }
        }
        if !pinned {
            let _guard = storage::lock(&path)?;
            if age(bundle, cutoff) {
                candidate(&ctx, &mut items, "capsule", bundle, apply)?;
            }
        }
    }
    drop(_figures);
    drop(_claims);
    let failures_path = std::env::var_os("OMS_FAIL_LEDGER")
        .map(|p| helpers::absolute(&ctx.cwd, p))
        .unwrap_or(ctx.repo.join(".oms/failures.jsonl"));
    let _guard = storage::lock(&failures_path)?;
    let (rows, bad) = storage::rows(&ctx.repo, &failures_path)?;
    require(
        bad == 0 && rows.iter().all(failures::valid_row),
        "malformed failure ledger; compaction refused",
    )?;
    let projected = failures::project(&rows, 0, "", epoch())?;
    let retired: BTreeSet<_> = projected
        .get("failures")
        .array()?
        .iter()
        .filter(|r| r.get("resolved").truth() || r.get("expired").truth())
        .map(|r| r.text("fingerprint"))
        .collect();
    let retained = rows
        .iter()
        .filter(|r| {
            !retired.contains(r.text("fingerprint"))
                || parse_timestamp(r.text("ts")).is_none_or(|t| t >= cutoff)
        })
        .collect::<Vec<_>>();
    let removed = rows.len() - retained.len();
    if removed > 0 {
        if apply {
            let mut bytes = Vec::new();
            for row in retained {
                bytes.extend(json::encoded(row)?);
            }
            storage::replace_locked(&ctx.repo, &failures_path, &bytes)?;
        }
        items.push(object([
            ("kind", string("failures")),
            ("rows", number(removed)),
        ]));
    }
    drop(_guard);
    let guard = ctx.repo.join(".oms/guards/change-guard.tsv");
    if guard.is_file() {
        let _guard = storage::lock(&guard)?;
        let raw = storage::input(&guard, storage::MAX_FILE)?;
        let text = String::from_utf8_lossy(&raw);
        let mut pid = None;
        let mut started = None;
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('\t') {
                if key == "pid" {
                    pid = value
                        .parse::<i32>()
                        .ok()
                        .and_then(rustix::process::Pid::from_raw);
                } else if key == "started" {
                    started = value.parse::<i64>().ok();
                }
            }
        }
        let stale = if let Some(pid) = pid {
            rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH)
        } else {
            started.is_some_and(|t| t < cutoff) || started.is_none() && age(&guard, cutoff)
        };
        if stale {
            candidate(&ctx, &mut items, "stale-change-guard", &guard, apply)?;
        }
    }
    let usage = ctx.repo.join(".oms/usage.jsonl");
    if usage.is_file() {
        let _guard = storage::lock(&usage)?;
        let (rows, bad) = storage::rows(&ctx.repo, &usage)?;
        if bad == 0 {
            let today = epoch().div_euclid(86400);
            let mut counts: BTreeMap<(String, String), i128> = BTreeMap::new();
            for row in &rows {
                let day = row.text("day");
                let stamp = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|t| t.and_utc().timestamp().div_euclid(86400));
                if stamp.is_none_or(|t| t > today || today - t > 30)
                    || row.text("family").is_empty()
                {
                    continue;
                }
                *counts
                    .entry((row.text("family").into(), day.into()))
                    .or_default() += row.get("count").int().unwrap_or(1);
            }
            if counts.len() < rows.len() {
                let mut bytes = Vec::new();
                for ((family, day), count) in &counts {
                    bytes.extend(json::encoded(&object([
                        ("schema", number(1)),
                        ("family", string(family)),
                        ("day", string(day)),
                        ("count", number(count)),
                    ]))?);
                }
                if apply {
                    storage::replace_locked(&ctx.repo, &usage, &bytes)?;
                }
                items.push(object([
                    ("kind", string("usage")),
                    ("rows", number(rows.len() - counts.len())),
                ]));
            }
        } else {
            deferred.push(string("malformed usage rows retained"));
        }
    }
    for operation in ["reconcile", "compact"] {
        if !ctx.repo.join(".oms/lifecycle/events.jsonl").exists() {
            break;
        }
        let mut args = vec![operation.into()];
        if operation == "compact" {
            args.extend(["--days".into(), days.to_string().into()]);
        } else {
            let stale =
                std::env::var("OMS_ATTEMPT_STALE_SECONDS").unwrap_or_else(|_| "86400".into());
            require(
                stale.parse::<u64>().is_ok(),
                "invalid OMS_ATTEMPT_STALE_SECONDS",
            )?;
            args.extend(["--stale-seconds".into(), stale.into()]);
        }
        if apply {
            args.push("--apply".into());
        }
        let result = oms_command_operations::run(
            oms_command_operations::App::Events,
            &args,
            &ctx.repo,
            ctx.interrupted,
        );
        require(result.0 == 0, String::from_utf8_lossy(&result.2))?;
    }
    for path in helpers::files(&ctx, &ctx.repo.join(".oms/threads"), ".jsonl", 50000)? {
        if !age(&path, cutoff) {
            continue;
        }
        let _guard = storage::lock(&path)?;
        let (rows, bad) = storage::rows(&ctx.repo, &path)?;
        if bad > 0 {
            continue;
        }
        let room = rows.iter().any(|r| r.opt("room_event").is_some());
        let collect = if room {
            let raw = json::canonical(&Value::Array(rows.clone()))?;
            let value = oms_command_room::value::parse(&raw, storage::MAX_STREAM)
                .map_err(|e| error(2, e))?;
            match oms_command_room::project(
                value
                    .as_array()
                    .ok_or_else(|| error(2, "invalid room stream"))?,
            ) {
                Ok(snapshot) => {
                    let snapshot = json::parse(
                        &oms_command_room::value::encode(&snapshot, false, true)
                            .map_err(|e| error(2, e))?,
                    )?;
                    snapshot.get("closed").truth()
                        && snapshot
                            .get("participants")
                            .object()
                            .map(|rows| rows.iter().all(|(_, r)| !r.get("joined").truth()))
                            .unwrap_or(false)
                        && snapshot
                            .get("messages")
                            .array()
                            .map(|rows| {
                                rows.iter().all(|r| {
                                    r.get("pending_for")
                                        .array()
                                        .map(|r| r.is_empty())
                                        .unwrap_or(false)
                                })
                            })
                            .unwrap_or(false)
                }
                Err(_) => false,
            }
        } else {
            rows.iter().any(|r| r.text("role") == "closed")
        };
        if collect && age(&path, cutoff) {
            candidate(
                &ctx,
                &mut items,
                if room { "closed-room" } else { "closed-thread" },
                &path,
                apply,
            )?;
        }
    }
    let (pruned, retained) = crate::gc_artifacts::prune(&ctx, apply)?;
    if pruned > 0 {
        items.push(object([
            ("kind", string("artifact-index")),
            ("rows", number(pruned)),
        ]));
    }
    if let Some(reason) = retained {
        deferred.push(string(reason));
    }
    if opts.flag("delete-orphan-files") && !crate::gc_artifacts::receipts_available(&ctx) {
        deferred.push(string(
            "orphan files retained: completion receipts unavailable",
        ));
    } else if opts.flag("delete-orphan-files") {
        let index = ctx.repo.join(".oms/artifacts/index.jsonl");
        let _guard = storage::lock(&index)?;
        let (rows, bad) = storage::rows(&ctx.repo, &index)?;
        require(bad == 0, "invalid artifact index; orphan deletion refused")?;
        let references =
            String::from_utf8(json::canonical(&Value::Array(rows))?).map_err(|e| error(2, e))?;
        let mut references = references;
        for suffix in [".json", ".jsonl", ".md"] {
            for source in helpers::files(&ctx, &ctx.repo.join(".oms"), suffix, 50000)? {
                if source == index {
                    continue;
                }
                let raw = storage::read(&ctx.repo, &source, storage::MAX_STREAM, false)?;
                require(
                    references.len().saturating_add(raw.len()) <= 64 * 1024 * 1024,
                    "reference scan bound exceeded; orphan deletion refused",
                )?;
                references.push_str(&String::from_utf8_lossy(&raw));
            }
        }
        let paths = helpers::files(&ctx, &ctx.repo.join(".oms/artifacts"), "", 50000)?;
        for path in paths {
            let relative = path
                .strip_prefix(&ctx.repo)
                .map_err(|_| error(2, "artifact escaped repository"))?
                .to_string_lossy();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name == "index.jsonl"
                || name.ends_with(".lock")
                || !age(&path, cutoff)
                || references.contains(relative.as_ref())
                || references.contains(name.as_ref())
            {
                continue;
            }
            // Only loose legacy patch files have no independent store ownership.
            if path.parent() == Some(ctx.repo.join(".oms/artifacts").as_path())
                && name.ends_with(".patch")
            {
                candidate(&ctx, &mut items, "orphan-artifact", &path, apply)?;
            }
        }
    }
    let count = items
        .iter()
        .map(|item| {
            item.get("rows")
                .int()
                .ok()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(1)
        })
        .sum::<usize>();
    let report = object([
        ("schema", number(1)),
        ("dry_run", Value::Bool(!apply)),
        ("items", Value::Array(items.clone())),
        ("removed", number(if apply { count } else { 0 })),
        ("deferred", Value::Array(deferred.clone())),
    ]);
    if opts.flag("json") {
        return json_output(0, &report);
    }
    let mut out = String::new();
    for item in items {
        out.push_str(&format!(
            "- {}: {}\n",
            item.text("kind"),
            if !item.text("path").is_empty() {
                item.text("path").to_owned()
            } else if !item.text("run_id").is_empty() {
                item.text("run_id").into()
            } else {
                format!("compact {} rows", helpers::display(item.get("rows")))
            }
        ));
    }
    for item in deferred {
        out.push_str(&format!("- retained: {}\n", item.str()?));
    }
    if count == 0 {
        out.push_str("gc: nothing to reclaim\n");
    } else if apply {
        out.push_str(&format!("gc: {count} item(s) reclaimed\n"));
    } else {
        out.push_str(&format!(
            "gc: {count} item(s) would be reclaimed\ngc: re-run with --apply to remove\n"
        ));
    }
    Ok(success(out))
}
