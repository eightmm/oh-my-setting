use crate::{
    AccountingFormat, AccountingSnapshot, Context, DigestRequest, Error, MemoryNote, Result,
    Service, Services,
};
use crate::{ledger, process, store, usage, value::*};
use chrono::{DateTime, Utc};
use oms_context::Json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const HELP: &str = include_str!("reconcile-help.txt");
const FIELDS: &str = "JobID,State,ExitCode,Elapsed,Cluster,Submit,Start,End,AllocTRES,NodeList,Timelimit,AllocCPUS,TotalCPU,JobName,Restarts";
const TERMINAL: [&str; 9] = [
    "COMPLETED",
    "FAILED",
    "CANCELLED",
    "TIMEOUT",
    "OUT_OF_MEMORY",
    "NODE_FAIL",
    "BOOT_FAIL",
    "DEADLINE",
    "PREEMPTED",
];
pub fn terminal(state: &str) -> bool {
    TERMINAL.contains(&state)
}
fn valid_job(id: &str) -> bool {
    regex::Regex::new(r"^[0-9]+(?:_[0-9]+|\+[0-9]+)?$")
        .unwrap()
        .is_match(id)
}
fn require_job(id: &str) -> Result<()> {
    if valid_job(id) {
        Ok(())
    } else {
        Err(Error::usage(
            "job id must be decimal, an array task or a heterogeneous job component",
        ))
    }
}
fn known(s: &str) -> &str {
    if ["Unknown", "None", "None assigned", "N/A"].contains(&s) {
        ""
    } else {
        s
    }
}
/// Parsed evidence is shared with digest rendering; the renderer never needs another query.
pub struct JobObservation {
    pub job: Json,
    pub accounting: Vec<u8>,
    pub accounting_stderr: Vec<u8>,
    pub accounting_exit: i32,
}
fn query_binary(ctx: &Context<'_>, key: &str, default: &str) -> OsString {
    ctx.variable(key)
        .filter(|x| !x.is_empty())
        .unwrap_or_else(|| default.into())
}
fn queue(ctx: &Context<'_>, id: &str) -> Result<process::Output> {
    process::query(
        ctx,
        &[
            query_binary(ctx, "OMS_SQUEUE_CMD", "squeue"),
            "-j".into(),
            id.into(),
            "-h".into(),
            "-o".into(),
            "%T".into(),
        ],
        65536,
    )
}
/// Fixed read-only scheduler argv; partial or failed observations cannot prove a terminal state.
pub fn job_query(ctx: &Context<'_>, id: &str) -> Result<JobObservation> {
    require_job(id)?;
    ctx.check()?;
    if ctx.query_timeout.is_zero() {
        return Err(Error::usage("query timeout must be positive"));
    }
    let r = process::query(
        ctx,
        &[
            query_binary(ctx, "OMS_SACCT_CMD", "sacct"),
            "-X".into(),
            "-D".into(),
            "-j".into(),
            id.into(),
            format!("--format={FIELDS}").into(),
            "-n".into(),
            "-P".into(),
        ],
        16 * 1024 * 1024,
    )?;
    ctx.check()?;
    if r.code == 124 {
        return Err(Error {
            code: 124,
            message: "scheduler query deadline exhausted; no terminal outcome recorded".into(),
        });
    }
    let mut result = parse_accounting(id, if r.code == 0 { &r.bytes } else { b"" })?;
    let state = s(&result, "state");
    let needs_queue = s(&result, "source") == "none" || (terminal(&state) && has_requeue(&result));
    if needs_queue {
        let q = queue(ctx, id)?;
        ctx.check()?;
        if q.code == 124 {
            return Err(Error {
                code: 124,
                message: "queue query deadline exhausted; no terminal outcome recorded".into(),
            });
        }
        if q.code == 0 {
            let line = String::from_utf8_lossy(&q.bytes)
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_owned());
            if let Some(line) = line {
                if s(&result, "source") == "none" {
                    put(&mut result, "state", string(line));
                    put(&mut result, "source", string("squeue"));
                } else {
                    put(&mut result, "ended_as", string(state));
                    put(&mut result, "state", string("REQUEUED"));
                    put(&mut result, "queue_state", string(line));
                    let attempts =
                        get(&result, "attempts").and_then(number).unwrap_or(1.0) as usize;
                    put(&mut result, "attempts", num(attempts + 1));
                    put(&mut result, "requeues", num(attempts));
                    remove(&mut result, "end");
                }
            }
        } else if s(&result, "source") != "none" {
            // Missing or failed squeue cannot establish that a requeue-capable ending is final.
            put(&mut result, "ended_as", string(state));
            put(&mut result, "state", string("UNKNOWN"));
            put(&mut result, "queue_state", string("UNKNOWN"));
            remove(&mut result, "end");
        }
    }
    Ok(JobObservation {
        job: result,
        accounting: r.bytes,
        accounting_stderr: r.stderr,
        accounting_exit: r.code,
    })
}
fn has_requeue(job: &Json) -> bool {
    if let Some(Json::Object(states)) = get(job, "task_states") {
        states
            .iter()
            .any(|(k, _)| k == b"PREEMPTED" || k == b"NODE_FAIL")
    } else {
        ["PREEMPTED", "NODE_FAIL"].contains(&s(job, "state").as_str())
    }
}
pub fn parse_accounting(id: &str, raw: &[u8]) -> Result<Json> {
    let mut elements: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    let mut order = vec![];
    let raw = String::from_utf8_lossy(raw);
    for (index, line) in raw.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let mut cells = line.split('|').collect::<Vec<_>>();
        let ident;
        if cells.len() >= 10 {
            ident = if cells[0].is_empty() {
                id.into()
            } else {
                cells[0].into()
            };
            cells.remove(0);
        } else {
            ident = format!("{id}#{index}");
            cells.resize(cells.len() + 9, "");
        }
        let state = cells
            .first()
            .and_then(|x| x.split_whitespace().next())
            .unwrap_or("");
        if state.is_empty() {
            continue;
        }
        if !elements.contains_key(&ident) {
            order.push(ident.clone());
            elements.insert(ident.clone(), vec![]);
        }
        let rows = elements.get_mut(&ident).unwrap();
        if cells.get(13).is_some_and(|s| s.trim() == "0") && !rows.is_empty() {
            rows.clear();
        }
        let mut row = object(vec![("id", string(&ident)), ("state", string(state))]);
        for (i, k) in [
            "state",
            "exit_code",
            "elapsed",
            "cluster",
            "submit",
            "start",
            "end",
            "alloc_tres",
            "nodelist",
            "timelimit",
            "alloc_cpus",
            "total_cpu",
        ]
        .iter()
        .enumerate()
        .skip(1)
        {
            let value = cells.get(i).copied().unwrap_or("");
            put(
                &mut row,
                k,
                string(if (3..=8).contains(&i) {
                    known(value)
                } else {
                    value
                }),
            );
        }
        rows.push(row);
    }
    let mut out = object(vec![
        ("job_id", string(id)),
        ("state", string("UNKNOWN")),
        ("exit_code", string("")),
        ("elapsed", string("")),
        ("source", string("none")),
    ]);
    if order.is_empty() {
        return Ok(out);
    }
    let newest = order
        .iter()
        .map(|i| elements[i].last().unwrap())
        .collect::<Vec<_>>();
    let pending = newest.iter().find(|r| !terminal(&s(r, "state")));
    let failed = newest.iter().find(|r| s(r, "state") != "COMPLETED");
    let pick = pending.or(failed).copied().unwrap_or(newest[0]);
    for key in ["state", "exit_code", "elapsed"] {
        put(&mut out, key, copy(get(pick, key).unwrap()));
    }
    put(&mut out, "source", string("sacct"));
    let lead = &elements[&order[0]];
    for key in ["cluster", "submit", "start", "alloc_tres", "nodelist"] {
        let row = if key == "submit" {
            &lead[0]
        } else {
            lead.last().unwrap()
        };
        let value = s(row, key);
        if !value.is_empty() {
            put(&mut out, key, string(value));
        }
    }
    if pending.is_none()
        && let Some(end) = newest
            .iter()
            .map(|r| s(r, "end"))
            .filter(|e| !e.is_empty())
            .max()
    {
        put(&mut out, "end", string(end));
    }
    let attempts = elements.values().map(Vec::len).max().unwrap();
    if attempts > 1 {
        put(&mut out, "attempts", num(attempts));
        put(&mut out, "requeues", num(attempts - 1));
    }
    if order.len() > 1 {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut tasks = vec![];
        for row in &newest {
            *counts.entry(s(row, "state")).or_default() += 1;
        }
        for row in newest.iter().take(200) {
            tasks.push(object(
                ["id", "state", "exit_code", "elapsed"]
                    .iter()
                    .map(|k| (*k, copy(get(row, k).unwrap())))
                    .collect(),
            ));
        }
        put(
            &mut out,
            "task_states",
            Json::Object(
                counts
                    .into_iter()
                    .map(|(s, n)| (s.into_bytes(), num(n)))
                    .collect(),
            ),
        );
        put(&mut out, "tasks", Json::Array(tasks));
        if newest.len() > 200 {
            put(&mut out, "tasks_omitted", num(newest.len() - 200));
        }
    }
    if pending.is_none() {
        let records = order
            .iter()
            .flat_map(|id| elements[id].iter().map(copy))
            .collect::<Vec<_>>();
        let spent = usage::consumed(&records);
        if matches!(&spent,Json::Object(o) if !o.is_empty()) {
            put(&mut out, "usage", spent);
        }
    }
    Ok(out)
}
fn detail(job: &Json) -> String {
    let mut parts = vec![];
    if s(job, "state") == "REQUEUED" {
        parts.push(format!(
            "requeued after {}, attempt {}",
            s(job, "ended_as"),
            s(job, "attempts")
        ));
    } else if get(job, "requeues").is_some_and(truth) {
        parts.push(format!("attempt {}", s(job, "attempts")));
    }
    if let Some(Json::Object(states)) = get(job, "task_states") {
        parts.push(format!(
            "tasks: {}",
            states
                .iter()
                .map(|(k, n)| format!("{}={}", String::from_utf8_lossy(k), display(n)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    parts.join("; ")
}
fn already(path: &Path, id: &str, submit: &str, root: &Path) -> Result<bool> {
    let mut found = false;
    store::rows_in(path, true, root, |r| {
        if s(&r, "job_id") == id && (submit.is_empty() || s(&r, "submit") == submit) {
            found = true;
        }
        Ok(())
    })?;
    Ok(found)
}
struct Options {
    ledger: PathBuf,
    one: Option<String>,
    memory: bool,
    agent: OsString,
}
fn take(args: &[OsString], i: &mut usize, name: &str) -> Result<OsString> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| Error::usage(format!("{name} requires a value")))
}
fn options(args: &[OsString], repo: &Path, ctx: &Context<'_>) -> Result<Options> {
    let mut o = Options {
        ledger: repo.join("docs/EXPERIMENTS.jsonl"),
        one: None,
        memory: false,
        agent: ctx
            .variable("OMS_AGENT")
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| {
                if !ctx.setting("CLAUDECODE").is_empty()
                    || !ctx.setting("CLAUDE_CODE_ENTRYPOINT").is_empty()
                {
                    "claude".into()
                } else if !ctx.setting("CODEX_SANDBOX").is_empty() {
                    "codex".into()
                } else {
                    "agent".into()
                }
            }),
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].to_string_lossy();
        match a.as_ref() {
            "--ledger" => {
                o.ledger = store::absolute(&ctx.cwd, Path::new(&take(args, &mut i, "--ledger")?))
            }
            "--job" => {
                let id = take(args, &mut i, "--job")?
                    .to_str()
                    .ok_or_else(|| Error::usage("invalid job id"))?
                    .to_owned();
                if !id.is_empty() {
                    require_job(&id)?;
                    o.one = Some(id);
                } else {
                    o.one = None;
                }
            }
            "--memory" => o.memory = true,
            "--agent" => o.agent = take(args, &mut i, "--agent")?,
            _ => return Err(Error::usage(format!("unknown argument: {a}"))),
        }
        i += 1;
    }
    Ok(o)
}
fn ids(o: &Options, root: &Path) -> Result<Vec<String>> {
    if let Some(id) = &o.one {
        return Ok(vec![id.clone()]);
    }
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    store::rows_in(&o.ledger, true, root, |row| {
        if !get(&row, "slurm_job_id").is_some_and(truth) {
            return Ok(());
        }
        let id = s(&row, "slurm_job_id").trim().to_owned();
        if !id.is_empty() && id != "None" && seen.insert(id.clone()) {
            require_job(&id)?;
            out.push(id);
        }
        Ok(())
    })?;
    Ok(out)
}
fn reconcile_file(ctx: &Context<'_>, repo: &Path) -> PathBuf {
    store::absolute(
        &ctx.cwd,
        Path::new(
            &ctx.variable("OMS_RECONCILE_FILE")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| repo.join(".oms/runs/reconcile.jsonl").into_os_string()),
        ),
    )
}
fn list(args: &[OsString], file: &Path, root: &Path, out: &mut dyn Write) -> Result<i32> {
    if args.len() > 1 {
        return Err(Error::usage("list takes at most N"));
    }
    let n = if let Some(n) = args.first() {
        let text = n.to_str().unwrap_or("");
        if text.is_empty() || !text.bytes().all(|c| c.is_ascii_digit()) {
            return Err(Error::usage("N must be a positive integer"));
        }
        text.parse::<usize>()
            .map_err(|_| Error::usage("N must be a positive integer"))?
    } else {
        10
    };
    if store::open_in(file, true, root)?.is_none() {
        writeln!(out, "no reconcile records")?;
        return Ok(0);
    }
    if n == 0 {
        return Ok(0);
    }
    let mut last = VecDeque::new();
    let mut total = 0;
    store::lines_in(file, false, root, |b| {
        if last.len() == n {
            let old: Vec<u8> = last.pop_front().unwrap();
            total -= old.len();
        }
        if total + b.len() > store::LEDGER_BYTES {
            return Err(Error::usage(
                "requested reconcile tail exceeds its byte limit",
            ));
        }
        total += b.len();
        last.push_back(b.to_vec());
        Ok(())
    })?;
    for line in last {
        let Ok(r) = parse(&line) else { continue };
        if !matches!(r, Json::Object(_)) {
            continue;
        }
        let mut extra = String::new();
        if get(&r, "cluster").is_some_and(truth) {
            extra.push_str(&format!("  on {}", s(&r, "cluster")));
        }
        if get(&r, "requeues").is_some_and(truth) {
            extra.push_str(&format!("  requeues={}", s(&r, "requeues")));
        }
        if let Some(Json::Object(states)) = get(&r, "task_states") {
            extra.push_str(&format!(
                "  tasks={}",
                states
                    .iter()
                    .map(|(k, n)| format!("{}:{}", String::from_utf8_lossy(k), display(n)))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        writeln!(
            out,
            "{}  job={}  {}  exit={}  {}{extra}",
            s(&r, "reconciled_at"),
            s(&r, "job_id"),
            s(&r, "state"),
            s(&r, "exit_code"),
            s(&r, "elapsed")
        )?;
    }
    Ok(0)
}
fn usage_cmd(
    args: &[OsString],
    file: &Path,
    repo: &Path,
    ctx: &Context<'_>,
    out: &mut dyn Write,
) -> Result<i32> {
    let mut experiment = String::new();
    let mut job = String::new();
    let mut json = false;
    let mut ledger = repo.join("docs/EXPERIMENTS.jsonl");
    let mut i = 0;
    while i < args.len() {
        let a = args[i].to_string_lossy();
        match a.as_ref() {
            "--experiment" => {
                experiment = take(args, &mut i, "--experiment")?
                    .to_string_lossy()
                    .into_owned()
            }
            "--job" => job = take(args, &mut i, "--job")?.to_string_lossy().into_owned(),
            "--ledger" => {
                ledger = store::absolute(&ctx.cwd, Path::new(&take(args, &mut i, "--ledger")?))
            }
            "--json" => json = true,
            _ => return Err(Error::usage(format!("unknown argument: {a}"))),
        }
        i += 1;
    }
    let mut jobs = if experiment.is_empty() {
        None
    } else {
        Some(usage::experiment_jobs_in(&ledger, &experiment, &ctx.cwd)?)
    };
    let mut scope = if experiment.is_empty() {
        String::new()
    } else {
        format!(" for experiment {experiment}")
    };
    if !job.is_empty() {
        if let Some(jobs) = &mut jobs {
            jobs.retain(|j| j == &job);
        } else {
            jobs = Some(BTreeSet::from([job.clone()]));
        }
        scope.push_str(&format!(" for job {job}"));
    }
    let mut report = usage::summary_in(file, jobs.as_ref(), &ctx.cwd)?;
    if !experiment.is_empty() {
        put(&mut report, "experiment", string(experiment));
    }
    if json {
        out.write_all(&encode(&report, false, true)?)?;
        writeln!(out)?;
    } else {
        writeln!(out, "{}", usage::render(&report, &scope))?;
    }
    Ok(0)
}
pub fn run(
    args: &[OsString],
    ctx: &Context<'_>,
    services: &mut dyn Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let mode = args.first().and_then(|a| a.to_str()).unwrap_or("");
    if ["-h", "--help"].contains(&mode) {
        out.write_all(HELP.as_bytes())?;
        return Ok(0);
    }
    if mode.is_empty() {
        return Err(Error::usage(HELP));
    }
    if !["scan", "apply", "list", "usage"].contains(&mode) {
        return Err(Error::usage(format!("unknown subcommand: {mode}")));
    }
    let repo = ledger::root(ctx, &ctx.cwd)?;
    let file = reconcile_file(ctx, &repo);
    if mode == "list" {
        return list(&args[1..], &file, &repo, out);
    }
    if mode == "usage" {
        return usage_cmd(&args[1..], &file, &repo, ctx, out);
    }
    let o = options(&args[1..], &repo, ctx)?;
    let ids = ids(&o, &repo)?;
    let mut done = 0;
    let mut pending = 0;
    if mode == "scan" && ids.is_empty() {
        writeln!(err, "run-reconcile: no tracked job ids")?;
    }
    for id in ids {
        ctx.check()?;
        let observation = job_query(ctx, &id)?;
        let job = &observation.job;
        let state = s(job, "state");
        if mode == "scan" {
            let d = detail(job);
            writeln!(
                out,
                "{id}\t{state}{}",
                if d.is_empty() {
                    String::new()
                } else {
                    format!("\t{d}")
                }
            )?;
            continue;
        }
        // This lock covers duplicate admission, digest publication and the receipt append.
        let guard = store::lock(ctx, &file)?;
        if already(&file, &id, &s(job, "submit"), &repo)? {
            guard.release().map_err(Error::lock)?;
            continue;
        }
        if !terminal(&state) {
            pending += 1;
            if state == "REQUEUED" {
                writeln!(
                    out,
                    "requeued: job {id} ({}); not recorded as finished",
                    detail(job)
                )?;
            }
            guard.release().map_err(Error::lock)?;
            continue;
        }
        if ledger::journal_enabled(ctx) {
            ledger::require(services, Service::Journal, "journal")?;
        }
        if o.memory {
            ledger::require(services, Service::Memory, "memory")?;
        }
        ledger::require(services, Service::Digest, "captured-accounting job-digest")?;
        let dir = store::absolute(
            &ctx.cwd,
            Path::new(
                &ctx.variable("OMS_RECONCILE_DIGEST_DIR")
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| repo.join(".oms/runs/reconcile").into_os_string()),
            ),
        );
        let digest_path = dir.join(format!("{id}.md"));
        if store::normalized(&digest_path) == store::normalized(&file) {
            return Err(Error::usage(
                "digest path and reconcile ledger must be distinct",
            ));
        }
        let digest = match services.digest(DigestRequest {
            repo: &repo,
            job,
            accounting: AccountingSnapshot {
                format: AccountingFormat::SacctAllocationParsable2,
                columns: FIELDS,
                available: observation.accounting_exit != 127,
                exit_code: observation.accounting_exit,
                stdout: &observation.accounting,
                stderr: &observation.accounting_stderr,
            },
            interrupted: ctx.interrupted,
        }) {
            Ok(bytes) if bytes.len() <= store::ROW_BYTES => bytes,
            Ok(_) => return Err(Error::usage("job digest exceeds its byte limit")),
            Err(e) if e.code == 124 || e.code >= 129 => return Err(e),
            Err(_) => format!("# job {id} digest unavailable\n").into_bytes(),
        };
        let dg = store::lock(ctx, &digest_path)?;
        store::write_locked(&digest_path, &digest, false, &dg, &ctx.cwd)?;
        dg.release().map_err(Error::lock)?;
        let mut row = object(vec![
            ("schema", num(1)),
            ("job_id", string(&id)),
            ("state", string(&state)),
            ("exit_code", copy(get(job, "exit_code").unwrap())),
            ("elapsed", copy(get(job, "elapsed").unwrap())),
            ("source", copy(get(job, "source").unwrap())),
            (
                "reconciled_at",
                string(
                    DateTime::<Utc>::from(SystemTime::now())
                        .format("%Y-%m-%dT%H:%M:%SZ")
                        .to_string(),
                ),
            ),
            ("digest", os(digest_path.as_os_str())),
        ]);
        for key in [
            "cluster",
            "submit",
            "start",
            "end",
            "alloc_tres",
            "nodelist",
            "attempts",
            "requeues",
            "task_states",
            "tasks",
            "tasks_omitted",
            "usage",
        ] {
            if let Some(v) = get(job, key) {
                put(&mut row, key, copy(v));
            }
        }
        let mut bytes = ordered(&row)?;
        bytes.push(b'\n');
        store::write_locked(&file, &bytes, true, &guard, &ctx.cwd)?;
        guard.release().map_err(Error::lock)?;
        ledger::observe(ctx, services, &repo, &file, &row, "run-reconcile", err)?;
        done += 1;
        writeln!(
            out,
            "reconciled: job {id} -> {state} (exit {}, {})",
            s(job, "exit_code"),
            s(job, "elapsed")
        )?;
        if o.memory {
            let text = format!(
                "Slurm job {id} finished: {state} (exit {}, {}). Digest: {}",
                s(job, "exit_code"),
                s(job, "elapsed"),
                digest_path.display()
            );
            if services
                .memory(MemoryNote {
                    repo: &repo,
                    agent: &o.agent,
                    text: &text,
                })
                .is_err()
            {
                writeln!(err, "run-reconcile: memory note skipped for {id}")?;
            }
        }
    }
    if mode == "apply" {
        writeln!(
            err,
            "run-reconcile: {done} reconciled, {pending} still running/pending"
        )?;
    }
    Ok(0)
}
