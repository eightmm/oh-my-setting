//! The run spine is an append-only join index, never an execution authority.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, epoch, error, helpers, json, json_output, random_id,
    require, storage, success, timestamp,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) const HELP: &str = "Usage: oms run new [--note TEXT]\n       oms run current\n       oms run link --tool NAME --event NAME [--path PATH] [--run-id ID] [--detail TEXT] [--agent NAME]\n       oms run show ID\n       oms run ls [N] [--open]\n       oms run close [ID] [--note TEXT]\n       oms run diff ID ID\n       oms run timeline [--since ISO8601|--today] [--limit N] [--agent NAME] [--tool NAME] [--json]\n       oms run validate [--dir DIR]\n       oms run capsule|checkpoints|claims|figure ...\n";
const POINTER: &str = ".oms/runs/CURRENT";

pub(crate) fn index(ctx: &Context<'_>) -> PathBuf {
    std::env::var_os("OMS_RUN_INDEX")
        .filter(|p| !p.is_empty())
        .map(|p| helpers::absolute(&ctx.cwd, p))
        .unwrap_or_else(|| ctx.state_repo.join(".oms/runs/spine.jsonl"))
}

pub(crate) fn current(ctx: &Context<'_>) -> Result<String> {
    if let Ok(value) = std::env::var("OMS_RUN_ID")
        && !value.is_empty()
    {
        return Ok(value);
    }
    let raw = storage::read(&ctx.state_repo, Path::new(POINTER), 4096, true)?;
    let text = std::str::from_utf8(&raw).map_err(|_| error(2, "CURRENT is not UTF-8"))?;
    let fields: Vec<_> = text.split_whitespace().collect();
    let ttl = std::env::var("OMS_RUN_CURRENT_TTL")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(86400);
    if fields.len() == 2
        && fields[1]
            .parse::<i64>()
            .is_ok_and(|t| t <= epoch() && epoch() - t <= ttl)
    {
        return Ok(fields[0].into());
    }
    Err(error(3, "no current run: mint one with oms run new"))
}

pub(crate) fn link(
    ctx: &Context<'_>,
    id: &str,
    tool: &str,
    event: &str,
    path: &str,
    detail: &str,
    agent: &str,
) -> Result<()> {
    require(
        !id.trim().is_empty() && !tool.is_empty() && !event.is_empty(),
        "link requires run id, tool and event",
    )?;
    let mut row = object([
        ("run_id", string(id)),
        ("ts", string(timestamp(epoch())?)),
        ("tool", string(tool)),
        ("event", string(event)),
    ]);
    if !path.is_empty() {
        row.set("path", string(path))?;
    }
    if !detail.is_empty() {
        row.set("detail", string(detail))?;
    }
    if !agent.is_empty() && agent != "agent" {
        row.set("agent", string(agent))?;
    }
    helpers::trace(&mut row, None)?;
    storage::append(&ctx.state_repo, &index(ctx), &row)
}

pub(crate) fn run(
    args: &[OsString],
    ctx: &Context<'_>,
    services: &mut dyn oms_command_runs::Services,
) -> Result<Output> {
    if args.first().is_some_and(|s| s == "--help" || s == "-h") {
        return Ok(success(HELP));
    }
    let action = args
        .first()
        .and_then(|s| s.to_str())
        .ok_or_else(|| error(2, HELP))?;
    let opts = Options::parse(
        &args[1..],
        &[
            "note", "tool", "event", "path", "run-id", "detail", "agent", "dir", "since", "limit",
        ],
        &["json", "open", "today"],
    )?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    match action {
        "new" => {
            require(opts.pos.is_empty(), "new takes no positional arguments")?;
            let id = format!(
                "{}-{}",
                timestamp(epoch())?.replace(['-', ':'], ""),
                &random_id("")?[..12]
            );
            let _guard = storage::lock(&ctx.state_repo.join(POINTER))?;
            link(
                ctx,
                &id,
                "oms-run",
                "new",
                "",
                opts.get("note"),
                &helpers::agent(),
            )?;
            storage::replace_locked(
                &ctx.state_repo,
                Path::new(POINTER),
                format!("{id} {}\n", epoch()).as_bytes(),
            )?;
            Ok(success(format!("{id}\n")))
        }
        "current" => {
            require(opts.pos.is_empty(), "current takes no arguments")?;
            Ok(success(format!("{}\n", current(ctx)?)))
        }
        "link" => {
            require(opts.pos.is_empty(), "link takes no positional arguments")?;
            let id = if opts.get("run-id").is_empty() {
                current(ctx).map_err(|e| error(2, e))?
            } else {
                opts.get("run-id").into()
            };
            let agent = if opts.get("agent").is_empty() {
                helpers::agent()
            } else {
                opts.get("agent").into()
            };
            link(
                ctx,
                &id,
                opts.get("tool"),
                opts.get("event"),
                opts.get("path"),
                opts.get("detail"),
                &agent,
            )?;
            Ok(success(""))
        }
        "close" => {
            require(
                opts.pos.len() <= 1 && (opts.pos.is_empty() || opts.get("run-id").is_empty()),
                "close takes one run id",
            )?;
            let id = opts
                .pos
                .first()
                .cloned()
                .or_else(|| (!opts.get("run-id").is_empty()).then(|| opts.get("run-id").into()))
                .map(Ok)
                .unwrap_or_else(|| current(ctx).map_err(|e| error(2, e)))?;
            let _guard = storage::lock(&ctx.state_repo.join(POINTER))?;
            link(
                ctx,
                &id,
                "oms-run",
                "close",
                "",
                opts.get("note"),
                &helpers::agent(),
            )?;
            let raw = storage::read(&ctx.state_repo, Path::new(POINTER), 4096, true)?;
            if std::str::from_utf8(&raw)
                .ok()
                .and_then(|s| s.split_whitespace().next())
                == Some(&id)
            {
                // Empty is an invalid/absent pointer to lockless readers; publication is atomic.
                storage::replace_locked(&ctx.state_repo, Path::new(POINTER), b"")?;
            }
            if services.available(oms_command_runs::Service::Journal) {
                let row = object([
                    ("run_id", string(&id)),
                    ("event", string("close")),
                    ("ts", string(timestamp(epoch())?)),
                ]);
                services
                    .observe(oms_command_runs::Observation {
                        repo: &ctx.repo,
                        source_type: "oms-run",
                        record_path: &index(ctx),
                        row: &helpers::context(&row)?,
                        event_type: Some("phase_outcome"),
                    })
                    .map_err(|e| error(e.code, e))?;
            }
            Ok((0, Vec::new(), format!("closed: {id}\n").into_bytes()))
        }
        "show" | "ls" => {
            let (rows, _) = storage::rows(&ctx.state_repo, &index(ctx))?;
            if action == "show" {
                require(opts.pos.len() == 1, "show requires one run id")?;
                let found: Vec<_> = rows
                    .into_iter()
                    .filter(|r| r.text("run_id") == opts.pos[0])
                    .collect();
                require(!found.is_empty(), "no records for run id")?;
                if opts.flag("json") {
                    return json_output(0, &Value::Array(found));
                }
                let mut out = format!("run: {}\n", opts.pos[0]);
                for row in found {
                    out.push_str(&format!(
                        "  {}  {:14} {:10} {} {}\n",
                        row.text("ts"),
                        row.text("tool"),
                        row.text("event"),
                        row.text("path"),
                        row.text("detail")
                    ));
                }
                return Ok(success(out));
            }
            require(opts.pos.len() <= 1, "ls takes at most N and --open")?;
            let n = opts
                .pos
                .first()
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| error(2, "ls requires a non-negative integer"))?
                .unwrap_or(10);
            let mut runs: Vec<Value> = Vec::new();
            for row in rows {
                let id = row.text("run_id");
                if id.is_empty() {
                    continue;
                }
                let i = match runs.iter().position(|r| r.text("run_id") == id) {
                    Some(i) => i,
                    None => {
                        runs.push(object([
                            ("run_id", string(id)),
                            ("first", row.get("ts").clone()),
                            ("last", row.get("ts").clone()),
                            ("closed", Value::Bool(false)),
                            ("events", number(0)),
                            ("tools", Value::Array(vec![])),
                        ]));
                        runs.len() - 1
                    }
                };
                let run = &mut runs[i];
                run.set("last", row.get("ts").clone())?;
                run.set("events", number(run.get("events").int()? + 1))?;
                if row.text("tool") == "oms-run" && row.text("event") == "close" {
                    run.set("closed", Value::Bool(true))?;
                }
                let mut tools = run
                    .get("tools")
                    .strings()?
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>();
                if !row.text("tool").is_empty() {
                    tools.insert(row.text("tool").into());
                }
                run.set("tools", json::list(tools.iter().map(String::as_str)))?;
            }
            runs.retain(|r| !opts.flag("open") || !r.get("closed").truth());
            let selected = runs.split_off(if n == 0 {
                0
            } else {
                runs.len().saturating_sub(n)
            });
            if opts.flag("json") {
                return json_output(0, &Value::Array(selected));
            }
            let mut out = String::new();
            for row in selected {
                out.push_str(&format!(
                    "{}  {:6} tools=[{}]  events={}  last={}\n",
                    row.text("run_id"),
                    if row.get("closed").truth() {
                        "closed"
                    } else {
                        "open"
                    },
                    row.get("tools").strings()?.join(","),
                    row.get("events").int()?,
                    row.text("last")
                ));
            }
            Ok(success(out))
        }
        "validate" => {
            require(
                opts.pos.is_empty(),
                "validate takes no positional arguments",
            )?;
            let dir = if opts.get("dir").is_empty() {
                ctx.repo.join(".oms")
            } else {
                ctx.cwd.join(opts.get("dir"))
            };
            let (findings, files) = validate(ctx, &dir)?;
            let code = i32::from(!findings.is_empty());
            if opts.flag("json") {
                return json_output(
                    code,
                    &object([
                        ("schema", number(1)),
                        ("adopted", Value::Bool(dir.is_dir())),
                        ("files", number(files)),
                        ("findings", Value::Array(findings)),
                    ]),
                );
            }
            let mut out = String::new();
            for finding in findings {
                out.push_str(&format!(
                    "{}   {}:{}  {}\n",
                    if finding.text("message") == "schema drift" {
                        "DRIFT"
                    } else {
                        "BAD"
                    },
                    finding.text("path"),
                    finding.get("line").int().unwrap_or(0),
                    finding.text("message")
                ));
            }
            out.push_str(&format!(
                "validate: {files} file(s), {}\n",
                if code == 0 {
                    "clean"
                } else {
                    "invalid rows or schema drift"
                }
            ));
            Ok((code, out.into_bytes(), Vec::new()))
        }
        "timeline" => {
            require(
                opts.pos.is_empty() && !(opts.flag("today") && !opts.get("since").is_empty()),
                "timeline takes --since or --today",
            )?;
            let since = if opts.flag("today") {
                Some(epoch().div_euclid(86400) * 86400)
            } else if !opts.get("since").is_empty() {
                Some(
                    crate::parse_timestamp(opts.get("since"))
                        .ok_or_else(|| error(2, "invalid --since timestamp"))?,
                )
            } else {
                None
            };
            let limit = opts.number("limit", 50)?;
            let mut paths = helpers::files(ctx, &ctx.repo.join(".oms"), ".jsonl", 50000)?;
            let ledger = ctx.repo.join("docs/EXPERIMENTS.jsonl");
            if ledger.is_file() {
                paths.push(ledger.clone());
            }
            let mut rows = Vec::new();
            for path in paths {
                let (stream, _) = storage::rows(&ctx.repo, &path)?;
                let name = if path == ledger {
                    "ledger".into()
                } else {
                    path.strip_prefix(ctx.repo.join(".oms"))
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .trim_end_matches(".jsonl")
                        .to_owned()
                };
                for row in stream {
                    let ts = if row.text("ts").is_empty() {
                        row.text("reconciled_at")
                    } else {
                        row.text("ts")
                    };
                    if ts.is_empty()
                        || since.is_some_and(|s| crate::parse_timestamp(ts).is_none_or(|t| t < s))
                    {
                        continue;
                    }
                    if !row
                        .text("agent")
                        .to_lowercase()
                        .contains(&opts.get("agent").to_lowercase())
                        || !row
                            .text("tool")
                            .to_lowercase()
                            .contains(&opts.get("tool").to_lowercase())
                    {
                        continue;
                    }
                    rows.push(object([
                        ("ts", string(ts)),
                        ("stream", string(&name)),
                        ("row", row),
                    ]));
                    require(rows.len() <= 100000, "timeline row limit exceeded")?;
                }
            }
            rows.sort_by(|a, b| a.text("ts").cmp(b.text("ts")));
            let omitted = if limit == 0 {
                0
            } else {
                rows.len().saturating_sub(limit)
            };
            let rows = rows.split_off(omitted);
            if opts.flag("json") {
                return json_output(
                    0,
                    &object([
                        ("schema", number(1)),
                        ("omitted", number(omitted)),
                        ("events", Value::Array(rows)),
                    ]),
                );
            }
            let mut out = if omitted > 0 {
                format!("({omitted} earlier event(s) omitted; raise --limit)\n")
            } else {
                String::new()
            };
            if rows.is_empty() {
                out.push_str(&format!(
                    "timeline: no events{}\n",
                    if opts.get("since").is_empty() {
                        String::new()
                    } else {
                        format!(" since {}", opts.get("since"))
                    }
                ));
            }
            for event in rows {
                let row = event.get("row");
                let mut parts = Vec::new();
                for key in [
                    "kind",
                    "tool",
                    "event",
                    "agent",
                    "provider",
                    "status",
                    "id",
                    "run_id",
                    "job_id",
                    "exit",
                    "verify_exit",
                    "gate",
                    "detail",
                    "note",
                    "task_id",
                ] {
                    if row.opt(key).is_some()
                        && !matches!(row.get(key), Value::Null)
                        && row.get(key).str() != Ok("")
                    {
                        parts.push(format!(
                            "{key}={}",
                            helpers::display(row.get(key))
                                .replace('\n', " ")
                                .chars()
                                .take(60)
                                .collect::<String>()
                        ));
                    }
                }
                if let Ok(cmd) = row.get("cmd").strings()
                    && !cmd.is_empty()
                {
                    parts.push(format!(
                        "cmd={}",
                        cmd.join(" ").chars().take(60).collect::<String>()
                    ));
                }
                out.push_str(&format!(
                    "{}  {:16} {}\n",
                    event.text("ts"),
                    event.text("stream"),
                    parts.join(" ")
                ));
            }
            Ok(success(out))
        }
        "diff" => {
            require(opts.pos.len() == 2, "diff requires two run ids")?;
            let a = crate::capsule::resolve(ctx, &opts.pos[0])?.1;
            let b = crate::capsule::resolve(ctx, &opts.pos[1])?.1;
            Ok(success(diff(&a, &b, &opts.pos[0], &opts.pos[1])?))
        }

        _ => Err(error(2, format!("unknown run action: {action}\n{HELP}"))),
    }
}

pub(crate) fn validate(ctx: &Context<'_>, dir: &Path) -> Result<(Vec<Value>, usize)> {
    let mut findings = Vec::new();
    if !dir.exists() {
        return Ok((findings, 0));
    }
    let paths = helpers::files(ctx, dir, ".jsonl", 50000)?;
    let root = &ctx.repo;
    for path in &paths {
        let raw = storage::read(root, path, storage::MAX_STREAM, false)?;
        let base = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let parent = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .unwrap_or("");
        for (i, line) in raw.split(|b| *b == b'\n').enumerate() {
            if line.trim_ascii().is_empty() {
                continue;
            }
            let mut problems = Vec::new();
            match json::parse_legacy(line, storage::MAX_FILE) {
                Ok(row) => {
                    if row.object().is_err() {
                        problems.push("row is not an object".to_owned());
                    }
                    let fields: &[&str] = if parent == "threads" {
                        &["thread", "seq", "role"]
                    } else {
                        match (parent, base) {
                            (_, "spine.jsonl") => &["run_id", "tool", "event"],
                            (_, "experiments.jsonl") => &["id"],
                            (_, "failures.jsonl") => &["fingerprint"],
                            (_, "ci.jsonl") => &["sha", "status"],
                            (_, "landings.jsonl") => &["landing_id", "event"],
                            (_, "reconcile.jsonl") => &["job_id"],
                            ("artifacts", "index.jsonl") => &["kind"],
                            ("runs", "index.jsonl") => &["id"],
                            _ => &[],
                        }
                    };
                    for field in fields {
                        if row.opt(field).is_none()
                            || matches!(row.get(field), Value::Null)
                            || row.get(field).str() == Ok("")
                        {
                            problems.push(format!("missing required field {field}"));
                        }
                    }
                    if [
                        "spine.jsonl",
                        "reconcile.jsonl",
                        "experiments.jsonl",
                        "failures.jsonl",
                        "ci.jsonl",
                        "landings.jsonl",
                    ]
                    .contains(&base)
                        && row
                            .opt("schema")
                            .is_some_and(|v| v.int().is_ok_and(|n| n < 1))
                    {
                        problems.push("schema drift".into());
                    }
                    if parent == "threads"
                        && ![
                            "topic", "question", "answer", "note", "decision", "summary", "closed",
                        ]
                        .contains(&row.text("role"))
                    {
                        problems.push("unknown thread role".into());
                    }
                    if base == "landings.jsonl"
                        && !row.text("event").is_empty()
                        && ![
                            "intent",
                            "complete",
                            "abandoned",
                            "applied-pending-receipt",
                            "not-applied-pending-receipt",
                            "capture-reserved",
                            "capture-terminal",
                            "capture-release-ready",
                            "capture-released",
                        ]
                        .contains(&row.text("event"))
                    {
                        problems.push("unknown landing event".into());
                    }
                    if base == "experiments.jsonl"
                        && !row.text("status").is_empty()
                        && !["claimed", "running", "done", "finished", "aborted"]
                            .contains(&row.text("status"))
                    {
                        problems.push("unknown experiment state".into());
                    }
                }
                Err(_) => problems.push("malformed JSON".into()),
            }
            for message in problems {
                findings.push(object([
                    (
                        "path",
                        string(
                            path.strip_prefix(&ctx.repo)
                                .unwrap_or(path)
                                .to_string_lossy(),
                        ),
                    ),
                    ("line", number(i + 1)),
                    ("message", string(message)),
                ]));
            }
            require(findings.len() <= 10000, "validation finding limit exceeded")?;
        }
    }
    let plan = storage::read(root, &dir.join("plan/tasks.json"), storage::MAX_FILE, true)?;
    if !plan.is_empty() {
        let value = match json::parse(&plan) {
            Ok(value) if value.get("tasks").object().is_ok() => value,
            _ => {
                findings.push(object([
                    ("path", string("plan/tasks.json")),
                    ("line", number(0)),
                    (
                        "message",
                        string("malformed plan JSON or tasks is not an object"),
                    ),
                ]));
                return Ok((findings, paths.len() + 1));
            }
        };
        let tasks = value.get("tasks").object()?;
        let states = [
            "ready",
            "claimed",
            "running",
            "review",
            "landing",
            "done",
            "failed",
            "blocked",
            "cancelled",
        ];
        let names: BTreeMap<_, _> = tasks
            .iter()
            .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v))
            .collect();
        for (id, row) in &names {
            let bad_state = !states.contains(&row.text("state"));
            let bad_ref = row
                .get("depends")
                .array()
                .unwrap_or(&[])
                .iter()
                .any(|v| v.str().is_err() || !names.contains_key(v.str().unwrap_or("")));
            if bad_state || bad_ref {
                findings.push(object([
                    ("path", string("plan/tasks.json")),
                    ("line", number(0)),
                    (
                        "message",
                        string(format!("invalid plan task state or dependency: {id}")),
                    ),
                ]));
            }
        }
    }
    Ok((findings, paths.len() + usize::from(!plan.is_empty())))
}

fn diff(a: &Value, b: &Value, aid: &str, bid: &str) -> Result<String> {
    let mut out = format!("diff: {aid}  vs  {bid}\n");
    fn line(out: &mut String, label: &str, a: &Value, b: &Value) {
        out.push_str(&format!(
            "{label:16} {}  {}  |  {}\n",
            if a == b { "==" } else { "!=" },
            helpers::display(a),
            helpers::display(b)
        ));
    }
    for (label, key) in [("commit", "commit_short"), ("dirty", "dirty")] {
        line(
            &mut out,
            label,
            a.get("git").get(key),
            b.get("git").get(key),
        );
    }
    for key in ["python", "torch", "cuda", "conda", "modules", "host"] {
        if ["python", "torch", "cuda"].contains(&key)
            || a.get("env").opt(key).is_some()
            || b.get("env").opt(key).is_some()
        {
            line(&mut out, key, a.get("env").get(key), b.get("env").get(key));
        }
    }
    line(
        &mut out,
        "seeds",
        &string(a.get("seeds").strings().unwrap_or_default().join(",")),
        &string(b.get("seeds").strings().unwrap_or_default().join(",")),
    );
    for family in ["configs", "data"] {
        let key = if family == "configs" { "path" } else { "name" };
        let left: BTreeMap<_, _> = a
            .get(family)
            .array()
            .unwrap_or(&[])
            .iter()
            .map(|r| (r.text(key), r))
            .collect();
        let right: BTreeMap<_, _> = b
            .get(family)
            .array()
            .unwrap_or(&[])
            .iter()
            .map(|r| (r.text(key), r))
            .collect();
        let names: BTreeSet<_> = left.keys().chain(right.keys()).copied().collect();
        for name in names {
            let l = left.get(name).copied().unwrap_or(&json::NULL);
            let r = right.get(name).copied().unwrap_or(&json::NULL);
            if family == "data" {
                line(&mut out, &format!("data:{name}"), l, r);
                continue;
            }
            line(
                &mut out,
                &format!(
                    "cfg:{}",
                    Path::new(name)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
                l.get("sha256"),
                r.get("sha256"),
            );
            if let (Ok(lp), Ok(rp)) = (l.get("params").object(), r.get("params").object()) {
                let keys: BTreeSet<_> = lp
                    .iter()
                    .chain(rp)
                    .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                    .collect();
                let mut count = 0;
                for key in keys {
                    let lv = l.get("params").get(&key);
                    let rv = r.get("params").get(&key);
                    if lv == rv && std::mem::discriminant(lv) == std::mem::discriminant(rv) {
                        continue;
                    }
                    count += 1;
                    if count <= 40 {
                        out.push_str(&format!(
                            "  {key}: {} → {}\n",
                            helpers::display(lv),
                            helpers::display(rv)
                        ));
                    }
                }
                if count > 40 {
                    out.push_str(&format!("  ... {} more differing keys\n", count - 40));
                }
            } else {
                for (id, row) in [(aid, l), (bid, r)] {
                    if !row.text("params_status").is_empty() {
                        out.push_str(&format!(
                            "  (keys of {id}: {})\n",
                            row.text("params_status")
                        ));
                    }
                }
            }
        }
    }
    let left = a.get("result").get("metrics");
    let right = b.get("result").get("metrics");
    let keys: BTreeSet<_> = left
        .object()
        .unwrap_or(&[])
        .iter()
        .chain(right.object().unwrap_or(&[]))
        .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
        .collect();
    for key in keys {
        let l = left.get(&key);
        let r = right.get(&key);
        let delta = match (l, r) {
            (Value::Number(a), Value::Number(b)) => a
                .parse::<f64>()
                .ok()
                .zip(b.parse::<f64>().ok())
                .map(|(a, b)| format!("  (Δ {:+})", b - a))
                .unwrap_or_default(),
            _ => String::new(),
        };
        out.push_str(&format!(
            "{:16}    {}  |  {}{delta}\n",
            format!("metric:{key}"),
            helpers::display(l),
            helpers::display(r)
        ));
    }
    Ok(out)
}
