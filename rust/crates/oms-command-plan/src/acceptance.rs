//! Goal acceptance executes only the stored user command, outside writer locks.
use crate::json::{self, Value, number, object, string};
use crate::retirement::PreparedRetirement;
use crate::services::SupervisedCommand;
use crate::storage::{ACTIVE, MAX_JSON};
use crate::{Plan, Request, Response, Result, error, require};
use std::ffi::OsString;

fn manifest(plan: &Plan<'_>, state: &Value) -> Result<String> {
    let contract = state.get("project_contract");
    if matches!(contract, Value::Null) {
        return Ok("legacy".into());
    }
    require(
        contract.get("schema").is_int(1),
        "acceptance files changed or stored manifest invalid",
    )?;
    let files = contract.get("acceptance_files").strings()?;
    let items = contract.get("acceptance_manifest").array()?;
    require(
        files.len() <= 64
            && files.len() == items.len()
            && files.windows(2).all(|pair| pair[0] < pair[1]),
        "acceptance manifest paths must be sorted and unique",
    )?;
    let mut current = Vec::new();
    for (rel, item) in files.iter().zip(items) {
        require(rel.len() <= 240, "acceptance path exceeds 240 UTF-8 bytes")?;
        crate::decision::relative(rel)?;
        require(
            item.text("path") == *rel,
            "acceptance manifest path mismatch",
        )?;
        let snapshot = plan
            .source
            .snapshot(rel, 8 * 1024 * 1024, false)?
            .ok_or_else(|| error("acceptance file missing"))?;
        let digest = json::sha(snapshot.raw);
        require(digest == item.text("sha256"), "acceptance files changed")?;
        current.push(object([
            ("path", string(rel)),
            ("sha256", string(digest)),
            ("mode", number(snapshot.mode)),
        ]));
    }
    Ok(json::sha(json::ascii_canonical(&Value::Array(current))?))
}
fn reuse(
    plan: &Plan<'_>,
    req: &Request,
    rows: &[Value],
    context: &str,
    plan_sha: &str,
    repo_sha: &str,
    manifest: &str,
    command: &str,
) -> Result<Option<Response>> {
    let run = req.text("reuse-pass")?;
    if run.is_empty() || context.is_empty() || !crate::engine::valid_id(run) || run.len() > 64 {
        return Ok(None);
    }
    let Some(row) = rows.iter().rev().find(|r| r.text("kind") == "acceptance") else {
        return Ok(None);
    };
    let expected = object([
        ("schema", number(1)),
        ("status", string("pass")),
        ("exit", number(0)),
        ("reason", string("")),
        ("timed_out", Value::Bool(false)),
        ("output_limited", Value::Bool(false)),
        ("run_id", string(run)),
        ("reuse_context_sha256", string(context)),
        ("plan_sha256", string(plan_sha)),
        ("repo_snapshot_sha256", string(repo_sha)),
        ("manifest_sha256", string(manifest)),
        ("accept_sha256_full", string(json::sha(command))),
    ]);
    for (k, v) in expected.object()? {
        if row.get(std::str::from_utf8(k).map_err(|_| error("invalid acceptance field"))?) != v {
            return Ok(None);
        }
    }
    let now = crate::engine::parse_ts(&plan.context.ts)
        .ok_or_else(|| error("invalid acceptance time"))?;
    let Some(then) = crate::engine::parse_ts(row.text("ts")) else {
        return Ok(None);
    };
    if !(0..=600).contains(&(now - then)) {
        return Ok(None);
    }
    if plan.git().acceptance_snapshot()? != repo_sha
        || json::sha(plan.store.read(ACTIVE, MAX_JSON)?) != plan_sha
        || self::manifest(plan, &json::parse(&plan.store.read(ACTIVE, MAX_JSON)?)?)? != manifest
        || plan
            .services
            .acceptance_reuse_context(&plan.context.execution_repo)?
            .as_deref()
            != Some(context)
    {
        return Ok(None);
    }
    Ok(Some(Response::text(format!(
        "plan-accept: reused pass run={run} receipt={}",
        json::sha(json::default_bytes(row, true)?)
    ))))
}
pub fn execute(
    plan: &Plan<'_>,
    req: &Request,
    retirement: Option<&PreparedRetirement>,
) -> Result<Response> {
    require(
        plan.context.canonical(),
        "acceptance requires safe repo-local plan file",
    )?;
    let state = plan.locked(false, None, || {
        crate::retirement::cleanup_for_accept(plan, retirement)?;
        let s = plan.load()?;
        require(s.unverified.is_empty(), "research completion not verified")?;
        Ok(s)
    })?;
    let raw = state
        .raw
        .as_ref()
        .ok_or_else(|| error("no plan; initialize with a goal and acceptance command"))?;
    let command = state.plan.get("accept").str()?;
    require(
        !command.is_empty(),
        "plan has no acceptance command; set one with agent-plan init --goal ... --accept CMD",
    )?;
    let rows = plan.store.rows(".oms/plan/progress.jsonl", true)?;
    let git = plan.git();
    git.guard()?;
    let manifest_before = manifest(plan, &state.plan)?;
    let repo_before = git.acceptance_snapshot()?;
    let plan_before = json::sha(raw);
    require(
        json::sha(plan.store.read(ACTIVE, MAX_JSON)?) == plan_before,
        "acceptance-command-changed: plan changed while freezing acceptance command",
    )?;
    if let Some(retire) = retirement {
        crate::retirement::accept_context(plan, retire)?;
    }
    let reuse_context = if !plan.context.run_id.is_empty() || req.has("reuse-pass") {
        plan.services
            .acceptance_reuse_context(&plan.context.execution_repo)?
            .unwrap_or_default()
    } else {
        String::new()
    };
    if retirement.is_none() && req.has("reuse-pass") {
        if let Some(response) = reuse(
            plan,
            req,
            &rows,
            &reuse_context,
            &plan_before,
            &repo_before,
            &manifest_before,
            command,
        )? {
            return Ok(response);
        }
    }
    let wall = crate::completion::timeout(
        &std::env::var("OMS_PLAN_ACCEPT_TIMEOUT").unwrap_or_else(|_| "10m".into()),
        false,
    )?;
    let execution = plan.services.supervise(&SupervisedCommand {
        argv: vec![
            OsString::from("bash"),
            OsString::from("-c"),
            OsString::from(command),
        ],
        cwd: plan.context.execution_repo.clone(),
        wall,
        output_limit: 1024 * 1024,
        label: "acceptance",
        remove_environment: vec![
            "OMS_PLAN_ACCEPT_PROOF_ID",
            "OMS_PLAN_ACCEPT_EXPECTED_PLAN_SHA",
            "OMS_PLAN_ACCEPT_EXPECTED_HEAD",
            "OMS_PLAN_ACCEPT_EXPECTED_REF",
            "OMS_PLAN_ACCEPT_GENERATION",
        ],
        environment: vec![],
    })?;
    let safe = git.guard().is_ok();
    let base = if safe {
        git.text(&["rev-parse", "HEAD"])
            .unwrap_or_else(|_| "unborn".into())
    } else {
        "unsafe".into()
    };
    let plan_after = plan.store.read(ACTIVE, MAX_JSON).ok();
    let manifest_after = plan_after
        .as_ref()
        .and_then(|r| json::parse(r).ok())
        .and_then(|d| manifest(plan, &d).ok());
    let repo_after = if safe {
        git.acceptance_snapshot().ok()
    } else {
        None
    };
    let reason = if !safe {
        "acceptance-mutated-repository"
    } else if plan_after
        .as_ref()
        .is_none_or(|r| json::sha(r) != plan_before)
    {
        "acceptance-command-changed"
    } else if manifest_after.as_deref() != Some(manifest_before.as_str()) {
        "acceptance-files-changed"
    } else if repo_after.as_deref() != Some(repo_before.as_str()) {
        "acceptance-mutated-repository"
    } else if retirement.is_some_and(|p| crate::retirement::accept_context(plan, p).is_err()) {
        "acceptance-retire-context-changed"
    } else if execution.runner_exit != execution.exit
        || execution.launch_error
        || execution.supervision_error
    {
        "acceptance-supervision-failed"
    } else if execution.timed_out {
        "acceptance-timeout"
    } else if execution.output_limited || execution.output.len() > 1024 * 1024 {
        "acceptance-output-limit"
    } else {
        ""
    };
    let verdict = if !reason.is_empty() {
        "error"
    } else if execution.exit == 0 {
        "pass"
    } else {
        "fail"
    };
    let output_sha = json::sha(&execution.output);
    let accept_sha = json::sha(command);
    let git_ref = if safe {
        git.snapshot()
            .ok()
            .map(|v| v.text("git_ref").to_owned())
            .unwrap_or_else(|| format!("DETACHED@{base}"))
    } else {
        format!("DETACHED@{base}")
    };
    let context_after = if !reuse_context.is_empty() {
        plan.services
            .acceptance_reuse_context(&plan.context.execution_repo)
            .ok()
            .flatten()
            .filter(|s| s == &reuse_context)
            .unwrap_or_default()
    } else {
        String::new()
    };
    let mut row = object([
        ("schema", number(1)),
        ("kind", string("acceptance")),
        ("ts", string(&plan.context.ts)),
        ("base_sha", string(&base)),
        ("status", string(verdict)),
        ("exit", number(execution.exit)),
        ("accept_sha256", string(&accept_sha[..16])),
        ("output_sha256", string(&output_sha[..16])),
        ("duration_s", number(execution.duration_s)),
        ("reason", string(reason)),
        ("timed_out", Value::Bool(execution.timed_out)),
        ("output_limited", Value::Bool(execution.output_limited)),
        ("plan_sha256", string(&plan_before)),
        ("git_ref", string(git_ref)),
        ("accept_sha256_full", string(accept_sha)),
        ("output_sha256_full", string(&output_sha)),
        ("repo_snapshot_sha256", string(repo_before)),
        ("reuse_context_sha256", string(context_after)),
        ("manifest_sha256", string(manifest_before)),
    ]);
    if !plan.context.run_id.is_empty() {
        row.set("run_id", string(&plan.context.run_id))?;
    }
    if let Some(cycle) = plan.context.cycle {
        row.set("cycle", number(cycle))?;
    }
    if let Some(p) = retirement {
        row.set("retire_proof_id", p.snapshot.get("proof_id").clone())?;
        row.set("plan_generation", p.snapshot.get("plan_generation").clone())?;
        row.set("git_clean", Value::Bool(true))?;
    }
    plan.store.append(".oms/plan/progress.jsonl", &row)?;
    let mut log = None;
    let mut stderr = Vec::new();
    if verdict != "pass" {
        let body = if crate::engine::screen(&execution.output).is_err() {
            b"redacted: acceptance output matched the sensitive-content guard\n".to_vec()
        } else {
            oms_scrub::redact(&execution.output, &[])
                .map_err(|_| error("acceptance output redaction unavailable"))?
                .bytes
        };
        let path = format!(".oms/plan/acceptance/{}.log", &output_sha[..16]);
        if !body.is_empty()
            && plan
                .store
                .mutate(&path, None, &body, 1024 * 1024, true)
                .is_ok()
        {
            log = Some(path);
        }
        stderr.extend_from_slice(b"--- acceptance output (last 20 lines) ---\n");
        let output = String::from_utf8_lossy(&body);
        let lines = output.lines().collect::<Vec<_>>();
        for line in lines.iter().skip(lines.len().saturating_sub(20)) {
            stderr.extend(format!("{line}\n").as_bytes());
        }
        if let Some(ref path) = log {
            stderr.extend(format!("full output: {path}\n").as_bytes());
        }
        if log.is_some() && prune_logs(plan).is_err() {
            stderr.extend_from_slice(
                b"plan-accept: warning: acceptance output retention was not completed\n",
            );
        }
    }
    let mut response = Response::text(format!(
        "plan-accept: {verdict} (exit {}, {}s) base={base}{}{}",
        execution.exit,
        execution.duration_s,
        if reason.is_empty() {
            String::new()
        } else {
            format!(" reason={reason}")
        },
        log.map(|p| format!(" output-log={p}")).unwrap_or_default()
    ));
    response.code = match verdict {
        "pass" => 0,
        "fail" => 3,
        _ => 2,
    };
    response.stderr = stderr;
    Ok(response)
}
fn prune_logs(plan: &Plan<'_>) -> Result<()> {
    let names = plan.store.names(".oms/plan/acceptance", 4096, true)?;
    let mut logs = Vec::new();
    for name in names {
        if !name.as_encoded_bytes().ends_with(b".log") {
            continue;
        }
        let path = std::path::Path::new(".oms/plan/acceptance").join(&name);
        let snap = plan
            .store
            .snapshot(&path, 1024 * 1024, false)?
            .ok_or_else(|| error("acceptance log disappeared"))?;
        let stamp = snap.generation.get("mtime_ns").int()?;
        logs.push((stamp, path, snap.raw));
    }
    logs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, path, raw) in logs.into_iter().skip(20) {
        plan.store.remove(path, &raw, None)?;
    }
    Ok(())
}
