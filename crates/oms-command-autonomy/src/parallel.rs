//! Opt-in `plan-run --parallel`: one controller keeps up to `--max-writers`
//! non-overlapping ready tasks running and refills a slot the moment its task
//! ends (no wave barrier). Failures release only exact unstarted claims.
use crate::cli::Args;
use crate::runner::{preflight, release_unstarted, run_task};
use crate::storage::{Context, Gates};
use crate::{Error, Output, Result, checkpoint, require, value};
use oms_command_plan::{Action, Request};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::time::Duration;

struct Flight {
    paths: Vec<String>,
    resources: Vec<String>,
    lease: String,
    issued_sha: String,
}
struct Done {
    id: String,
    result: Option<Result<Output>>,
}
enum Pick {
    Task(Value, String),
    Backpressure,
    Idle,
}

/// Same notion of "touches the same files" as the plan's wave view: a glob
/// reduces to its parent directory and an empty scope overlaps everything.
fn roots(paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return vec![String::new()];
    }
    paths
        .iter()
        .map(|raw| {
            let mut p = raw.trim().replace('\\', "/");
            if let Some(at) = p.find(['*', '?', '[']) {
                p = p[..at]
                    .rsplit_once('/')
                    .map(|(parent, _)| parent.to_owned())
                    .unwrap_or_default();
            }
            let mut p = p.trim_matches('/');
            while let Some(rest) = p.strip_prefix("./") {
                p = rest;
            }
            if p == "." {
                String::new()
            } else {
                p.to_owned()
            }
        })
        .collect()
}
fn overlaps(a: &[String], b: &[String]) -> bool {
    let (a, b) = (roots(a), roots(b));
    a.iter().any(|a| {
        b.iter().any(|b| {
            a.is_empty()
                || b.is_empty()
                || a == b
                || a.starts_with(&format!("{b}/"))
                || b.starts_with(&format!("{a}/"))
        })
    })
}
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
/// Claims the exact ready snapshot that fits beside the running ones.
fn pick(
    ctx: &Context<'_>,
    provider: &str,
    flights: &BTreeMap<String, Flight>,
    attempted: &BTreeSet<String>,
    backlog: Option<usize>,
) -> Result<Pick> {
    let base = ctx.git(&["rev-parse", "HEAD"])?;
    let out = ctx.ready_snapshot()?;
    require(out.code == 0, "cannot list ready tasks")?;
    let ready = value::parse(&out.stdout)?;
    let candidates = ready["tasks"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|t| {
                    let id = value::text(t, "id");
                    let paths = strings(&t["allowed"]);
                    let resources = strings(&t["resources"]);
                    !value::text(t, "brief").trim().is_empty()
                        && !attempted.contains(id)
                        && !flights.contains_key(id)
                        && !flights.values().any(|f| {
                            overlaps(&f.paths, &paths)
                                || resources.iter().any(|r| f.resources.contains(r))
                        })
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if candidates.is_empty() {
        return Ok(Pick::Idle);
    }
    if let Some(limit) = backlog
        && ready["review_backlog"]
            .as_u64()
            .ok_or_else(|| Error::new(2, "invalid review backlog"))?
            >= limit as u64
    {
        return Ok(Pick::Backpressure);
    }
    for candidate in candidates {
        require(
            ctx.git(&["rev-parse", "HEAD"])? == base,
            "Plan ready selection base moved before claiming",
        )?;
        let id = value::text(&candidate, "id");
        let claimed = ctx.action(
            &Request::new(Action::Claim)
                .with("id", id)
                .with("provider", provider)
                .with(
                    "expected-task-sha256",
                    value::text(&candidate, "task_sha256"),
                )
                .flag("json"),
        );
        if let Ok(issued) = claimed
            && issued.code == 0
        {
            return Ok(Pick::Task(value::parse(&issued.stdout)?, base));
        }
    }
    Ok(Pick::Idle)
}
pub(crate) fn execute(ctx: &Context<'_>, args: &Args) -> Result<Output> {
    ctx.parent()?;
    let provider = ctx.services.normalize_provider(args.required("to")?)?;
    require(
        !args.has("id"),
        "--parallel dispatches ready tasks itself; --id is refused",
    )?;
    require(
        !args.flag("dry-run"),
        "--dry-run is not supported with --parallel",
    )?;
    let writers = args.number("max-writers", 2, 1, 16)? as usize;
    let backlog = if args.has("max-review-backlog") {
        Some(args.number("max-review-backlog", 1, 1, 64)? as usize)
    } else {
        None
    };
    let wall = preflight(ctx, args)?;
    let path = ctx.state.join(".oms/plan/controller.lock");
    let settings = oms_lock::Settings::from_env().map_err(|e| Error::new(2, e.to_string()))?;
    let _controller = oms_lock::Lock::for_file(&path, settings)
        .and_then(|lock| lock.try_acquire())
        .map_err(|e| match e {
            oms_lock::Error::Busy => {
                Error::new(75, "another plan-run --parallel controller is active")
            }
            other => Error::new(2, other.to_string()),
        })?;
    let _ = ctx.action(&Request::new(Action::Reclaim));

    let gates = Gates::default();
    let mut controller = Context::new(
        ctx.repo.clone(),
        ctx.state.clone(),
        ctx.services,
        ctx.interrupted,
    )?;
    controller.gates = Some(&gates);
    controller.owner_id = ctx.owner_id.clone();
    controller.run_id = ctx.run_id.clone();
    controller.cycle = ctx.cycle;
    controller.peer_wall = Some(wall);
    let controller = &controller;

    let mut worker_args = args.clone();
    for flag in ["parallel", "next"] {
        worker_args.flags.remove(flag);
    }
    for name in ["max-writers", "max-review-backlog"] {
        worker_args.values.remove(name);
    }

    let mut output = Output::default();
    let mut flights: BTreeMap<String, Flight> = BTreeMap::new();
    let mut attempted = BTreeSet::new();
    let (mut failed, mut started) = (0usize, 0usize);
    let (mut backpressure, mut interrupted, mut halted) = (false, false, false);
    let (tx, rx) = mpsc::channel::<Done>();
    let failure = std::thread::scope(|scope| -> Result<()> {
        loop {
            if checkpoint(ctx.interrupted).is_err() {
                interrupted = true;
            }
            while !interrupted && !backpressure && !halted && flights.len() < writers {
                match pick(controller, &provider, &flights, &attempted, backlog) {
                    Ok(Pick::Task(task, base)) => {
                        let id = value::text(&task, "id").to_owned();
                        attempted.insert(id.clone());
                        flights.insert(
                            id.clone(),
                            Flight {
                                paths: strings(&task["allowed_paths"]),
                                resources: strings(&task["resources"]),
                                lease: value::text(&task, "lease_id").to_owned(),
                                issued_sha: value::text(&task, "task_sha256").to_owned(),
                            },
                        );
                        started += 1;
                        let tx = tx.clone();
                        let worker_args = worker_args.clone();
                        let (repo, state) = (ctx.repo.clone(), ctx.state.clone());
                        let (owner, run, cycle) =
                            (ctx.owner_id.clone(), ctx.run_id.clone(), ctx.cycle);
                        let (services, stop, gates) = (ctx.services, ctx.interrupted, &gates);
                        scope.spawn(move || {
                            let result = catch_unwind(AssertUnwindSafe(|| {
                                let mut worker = Context::new(repo, state, services, stop)?;
                                worker.gates = Some(gates);
                                worker.owner_id = owner;
                                worker.run_id = run;
                                worker.cycle = cycle;
                                worker.peer_wall = Some(wall);
                                run_task(&worker, &worker_args, None, task, true, &base, wall)
                            }));
                            let _ = tx.send(Done {
                                id,
                                result: result.ok(),
                            });
                        });
                    }
                    Ok(Pick::Backpressure) => backpressure = true,
                    Ok(Pick::Idle) => break,
                    Err(error) => {
                        // Stop claiming but let healthy siblings finish.
                        output.stderr.extend_from_slice(
                            format!("plan-run: parallel selection stopped: {}\n", error.message)
                                .as_bytes(),
                        );
                        failed += 1;
                        interrupted = interrupted || error.code == 130;
                        halted = true;
                        break;
                    }
                }
            }
            if flights.is_empty() {
                return Ok(());
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(done) => {
                    let flight = flights.remove(&done.id);
                    let line = match done.result {
                        Some(Ok(task)) => {
                            let code = task.code;
                            failed += usize::from(code != 0);
                            output.stdout.extend(task.stdout);
                            output.stderr.extend(task.stderr);
                            format!("plan-run: parallel task={} exit={code}\n", done.id)
                        }
                        Some(Err(error)) => {
                            failed += 1;
                            format!(
                                "plan-run: parallel task={} exit={} error={}\n",
                                done.id, error.code, error.message
                            )
                        }
                        None => {
                            failed += 1;
                            let released = flight.as_ref().is_some_and(|flight| {
                                release_unstarted(
                                    controller,
                                    &done.id,
                                    &flight.lease,
                                    &flight.issued_sha,
                                )
                            });
                            format!(
                                "plan-run: parallel task={} panicked; {}\n",
                                done.id,
                                if released {
                                    "unstarted claim released"
                                } else {
                                    "inspect the retained lease"
                                },
                            )
                        }
                    };
                    output.stdout.extend(line.into_bytes());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    });
    failure?;
    output.code = if interrupted {
        130
    } else if backpressure {
        3
    } else if failed > 0 {
        1
    } else if started == 0 {
        return Err(Error::new(
            2,
            "no actionable task (ready tasks without a work brief are skipped; agent-plan next counts them)",
        ));
    } else {
        0
    };
    let summary = format!(
        "plan-run: parallel started={started} failed={failed} writers={writers}{}{}\n",
        backlog.map_or(String::new(), |n| format!(" max_review_backlog={n}")),
        if backpressure {
            " backpressure=review-backlog: no new task claimed; review or land before rerun"
        } else {
            ""
        }
    );
    output.stdout.extend(summary.into_bytes());
    Ok(output)
}
