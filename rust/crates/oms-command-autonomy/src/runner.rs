//! One authorized plan task, with an exact lease and one durable landing repair.
use crate::cli::Args;
use crate::planning::{invoke, strings};
use crate::storage::{Context, MAX_FILE};
use crate::{Error, Output, Result, require, value, verify};
use oms_command_plan::{Action, Request};
use serde_json::{Value, json};
use std::time::Duration;

fn show(ctx: &Context<'_>, id: &str) -> Result<Value> {
    let out = ctx.action(&Request::new(Action::Show).with("id", id))?;
    require(out.code == 0, "cannot read plan task")?;
    value::parse(&out.stdout)
}
fn action(ctx: &Context<'_>, kind: Action, id: &str, lease: &str) -> Result<Output> {
    ctx.action(&Request::new(kind).with("id", id).with("lease-id", lease))
}
fn route(ctx: &Context<'_>, task: &Value, args: &Args) -> Result<Value> {
    let defaults = json!({"provider": ctx.services.normalize_provider(args.required("to")?)?, "model": args.text("model")?, "fallback_model": args.text("fallback-model")?, "reasoning_effort": args.get("reasoning-effort", "auto")?, "workload": "standard"});
    let task = oms_context::parse_json(&value::canonical(task)?)
        .map_err(|_| Error::new(2, "invalid task assignment context"))?;
    let defaults = oms_context::parse_json(&value::canonical(&defaults)?)
        .map_err(|_| Error::new(2, "invalid assignment defaults"))?;
    let resolved = oms_command_task::assignment::resolve(&task, &defaults)
        .map_err(|e| Error::new(e.code, e.to_string()))?;
    // The shared assignment producer validates canonical transport names and
    // prevents a reviewed assignment from inheriting another route's model.
    let raw = oms_command_task::json::encode(&resolved);
    value::parse(&raw)
}
fn delegate(
    ctx: &Context<'_>,
    args: &Args,
    id: &str,
    route: &Value,
    repair: u64,
    artifact: Option<&str>,
    wall: Duration,
) -> Result<Output> {
    let mut call = vec![
        "--repo".into(),
        ctx.repo.as_os_str().to_owned(),
        "--to".into(),
        value::text(route, "provider").into(),
        "--plan-task".into(),
        id.into(),
        "--repair".into(),
        repair.to_string().into(),
    ];
    for (field, option) in [
        ("model", "model"),
        ("fallback_model", "fallback-model"),
        ("reasoning_effort", "reasoning-effort"),
        ("workload", "workload"),
    ] {
        let raw = value::text(route, field);
        if !raw.is_empty() && raw != "auto" && raw != "standard" {
            call.push(format!("--{option}").into());
            call.push(raw.into());
        }
    }
    if args.has("context-pack") {
        call.push("--context-pack".into());
        call.push(args.values["context-pack"].clone());
    }
    if let Some(artifact) = artifact {
        call.extend([
            "--review-artifact".into(),
            artifact.into(),
            "--terminalize-resumed-repair".into(),
        ]);
    }
    crate::planning::invoke_configured(
        ctx,
        "peer-delegate",
        call,
        wall,
        vec![
            (
                "OMS_MODEL_WORKLOAD".into(),
                value::text(route, "workload").into(),
            ),
            (
                "OMS_MODEL_EXPLICIT".into(),
                value::text(route, "model").into(),
            ),
            (
                "OMS_MODEL_FALLBACK_EXPLICIT".into(),
                value::text(route, "fallback_model").into(),
            ),
            (
                "OMS_REASONING_EFFORT_REQUEST".into(),
                value::text(route, "reasoning_effort").into(),
            ),
            (
                "OMS_REASONING_FALLBACK_EXPLICIT".into(),
                value::text(route, "fallback_effort").into(),
            ),
        ],
    )
}
fn land(ctx: &Context<'_>, args: &Args, task: &Value, wall: Duration) -> Result<Output> {
    let mut call = vec![
        "--repo".into(),
        ctx.repo.as_os_str().to_owned(),
        "--plan-task".into(),
        value::text(task, "id").into(),
        "--verify".into(),
        value::text(task, "verify").into(),
    ];
    if args.flag("allow-verifier-change") {
        call.push("--allow-verifier-change".into());
    }
    if args.has("allow-floor-change") {
        call.extend([
            "--allow-floor-change".into(),
            args.required("allow-floor-change")?.into(),
        ]);
    }
    invoke(ctx, "patch-land", call, wall)
}
pub(crate) fn execute(ctx: &Context<'_>, args: &Args) -> Result<Output> {
    execute_bound(ctx, args, None)
}
pub(crate) fn execute_bound(
    ctx: &Context<'_>,
    args: &Args,
    intent: Option<&Value>,
) -> Result<Output> {
    ctx.parent()?;
    args.required("to")?;
    require(
        args.flag("next") != args.has("id"),
        "use exactly one of --id or --next",
    )?;
    let repair = args.number("repair", if args.flag("auto-repair") { 1 } else { 0 }, 0, 3)?;
    require(
        !args.flag("auto-repair") || args.flag("land"),
        "--auto-repair requires --land",
    )?;
    if args.has("id") {
        require(
            value::id(args.required("id")?, 160),
            "task ids must match [A-Za-z0-9._-]+",
        )?;
    }
    verify::model(args.text("model")?)?;
    verify::model(args.text("fallback-model")?)?;
    verify::effort(args.get("reasoning-effort", "auto")?)?;
    if args.has("allow-floor-change") {
        verify::reason(args.required("allow-floor-change")?)?;
    }
    if args.has("context-pack") {
        ctx.services.validate_context_pack(
            &ctx.repo,
            &args.path("context-pack", &ctx.repo),
            ctx.interrupted,
        )?;
    }
    ctx.git(&["rev-parse", "--git-dir"])?;
    let wall = ctx.peer_wall.unwrap_or(verify::duration(
        &std::env::var("OMS_PEER_TIMEOUT").unwrap_or_else(|_| "5m".into()),
    )?);
    let provider = ctx.services.normalize_provider(args.required("to")?)?;
    if !args.flag("dry-run") {
        let _ = ctx.action(&Request::new(Action::Reclaim));
    }
    let mut claimed = false;
    let mut task = if args.flag("next") {
        let mut request = Request::new(Action::Next)
            .with("provider", &provider)
            .flag("json");
        if !args.flag("dry-run") {
            request = request.flag("claim");
            claimed = true;
        }
        let out = ctx.action(&request)?;
        require(out.code == 0, "no actionable task")?;
        value::parse(&out.stdout)?
    } else {
        show(ctx, args.required("id")?)?
    };
    let id = value::text(&task, "id").to_owned();
    let mut lease = value::text(&task, "lease_id").to_owned();
    let result = (|| {
        let mut route = route(ctx, &task, args)?;
        let resolved = ctx.services.resolve_route(
            &ctx.repo,
            &route,
            value::text(&task, "role"),
            ctx.interrupted,
        )?;
        require(
            resolved.provider == value::text(&route, "provider")
                && !resolved.primary.is_empty()
                && value::id(&resolved.model_class, 160),
            "native model route changed reviewed transport or lacks identity",
        )?;
        verify::model(&resolved.primary)?;
        verify::model(&resolved.fallback)?;
        verify::effort(&resolved.effort)?;
        if !resolved.fallback_effort.is_empty() {
            verify::effort(&resolved.fallback_effort)?;
        }
        route["model"] = resolved.primary.clone().into();
        route["fallback_model"] = resolved.fallback.clone().into();
        route["reasoning_effort"] = resolved.effort.clone().into();
        route["fallback_effort"] = resolved.fallback_effort.clone().into();
        let provider = value::text(&route, "provider");
        let state = value::text(&task, "state");
        let resume = state == "claimed"
            && task["repair_count"].as_u64() == Some(1)
            && !lease.is_empty()
            && value::text(&task, "review_lease_id") == lease;
        let review = state == "review" && args.flag("land");
        require(
            state == "ready" || claimed || review || resume,
            format!("task {id} is {state}, not ready; release it or continue review with --land"),
        )?;
        if resume {
            require(
                args.flag("land") && args.flag("auto-repair"),
                "interrupted bounded repair requires --land --auto-repair",
            )?;
        }
        if review || resume {
            require(
                !lease.is_empty()
                    && value::text(&task, "provider") == provider
                    && !value::text(&task, "artifact").is_empty()
                    && !value::text(&task, "patch").is_empty(),
                "review/repair requires exact provider, active lease and stored artifact/patch",
            )?;
        }
        require(
            value::text(&task, "executor_id").is_empty()
                && value::text(&task, "executor_soul_sha256").is_empty(),
            "Soul executor receipt is retired; create a fresh task",
        )?;
        let allowed = strings(&task["allowed_paths"])?;
        let verify = value::text(&task, "verify").to_owned();
        crate::selectors::validate(&ctx.repo, &verify)?;
        require(
            !allowed.is_empty() && !verify.trim().is_empty(),
            "task must declare non-empty allowed_paths and a mechanical verify command",
        )?;
        if let Some(path) = verify::missing_base(&ctx.repo, &verify, &allowed, ctx.interrupted)? {
            return Err(Error::new(
                2,
                format!(
                    "task {id} verify names {path}, which does not exist at the base commit; admission restores the verification surface from base"
                ),
            ));
        }
        require(
            verify::floor_reads(
                &verify,
                &allowed.iter().map(String::as_str).collect::<Vec<_>>(),
            )
            .is_empty(),
            "floor_incompatible_verifier: task verify reads content it may modify",
        )?;
        let base = ctx.git(&["rev-parse", "HEAD"])?;
        let mut stable = serde_json::Map::new();
        for key in [
            "id",
            "title",
            "brief",
            "depends",
            "allowed_paths",
            "forbidden_paths",
            "verify",
            "role",
            "assignment",
        ] {
            if key != "assignment"
                || task
                    .get(key)
                    .is_some_and(|v| v.as_object().is_some_and(|o| !o.is_empty()))
            {
                stable.insert(key.into(), task.get(key).cloned().unwrap_or(Value::Null));
            }
        }
        let identity = format!(
            "plan-run task={id} base={base} provider={provider} verify={} contract={} route={}",
            &value::hash(&verify)[..16],
            &value::hash(value::canonical(&Value::Object(stable))?)[..16],
            &value::hash(resolved.contract())[..16]
        );
        let failure = invoke(
            ctx,
            "fail-ledger",
            vec!["check".into(), "--cmd".into(), identity.clone().into()],
            Duration::from_secs(30),
        )?;
        require(
            failure.code == 0 || failure.code == 3,
            "could not check failure ledger",
        )?;
        require(
            failure.code != 3 || args.flag("retry-known"),
            "known unchanged plan-run failure; change the contract or use --retry-known",
        )?;
        let mut output = Output::text(format!(
            "plan-run: task={id} provider={provider} title={}\nplan-run: scope={} verify={verify}",
            value::text(&task, "title"),
            allowed.join(",")
        ));
        if args.flag("dry-run") {
            output.append(Output::text("plan-run: dry-run; no claim or provider call"));
            return Ok(output);
        }
        if !claimed && !review && !resume {
            ctx.action(
                &Request::new(Action::Claim)
                    .with("id", &id)
                    .with("provider", provider),
            )?;
            claimed = true;
            task = show(ctx, &id)?;
            lease = value::text(&task, "lease_id").into();
        }
        if !review {
            let review_artifact = if resume {
                let path = value::text(&task, "repair_artifact");
                Some(if path.is_empty() {
                    value::text(&task, "artifact")
                } else {
                    path
                })
            } else {
                None
            };
            let delegated = delegate(
                ctx,
                args,
                &id,
                &route,
                if resume { 0 } else { repair },
                review_artifact,
                wall,
            )?;
            let code = delegated.code;
            output.append(delegated);
            if code != 0 {
                if resume {
                    terminalize_repair(ctx, &id, &lease)?;
                }
                let _ = invoke(
                    ctx,
                    "fail-ledger",
                    vec![
                        "record".into(),
                        "--kind".into(),
                        "plan-run".into(),
                        "--cmd".into(),
                        identity.into(),
                        "--exit".into(),
                        code.to_string().into(),
                        "--summary".into(),
                        format!("plan-run failed: task {id} on {provider}").into(),
                    ],
                    Duration::from_secs(30),
                );
                return Ok(output);
            }
        }
        task = show(ctx, &id)?;
        require(
            value::text(&task, "state") == "review"
                && value::text(&task, "lease_id") == lease
                && !value::text(&task, "artifact").is_empty()
                && !value::text(&task, "patch").is_empty(),
            "worker success lacks exact reviewed lease/patch evidence",
        )?;
        if args.flag("land") {
            let mut landed = land(ctx, args, &task, wall)?;
            if landed.code != 0
                && args.flag("auto-repair")
                && !resume
                && task["repair_count"].as_u64().unwrap_or(0) == 0
            {
                let mut log = landed.stdout.clone();
                log.extend(&landed.stderr);
                let path = format!(".oms/plan/landing-repair-{}.log", value::random("")?);
                ctx.store.publish_once(&path, &log, 8 * MAX_FILE)?;
                let absolute = ctx.state.join(&path);
                if let Some(intent) = intent {
                    crate::goal::mark_repair(ctx, intent)?;
                }
                ctx.action(
                    &Request::new(Action::Repair)
                        .with("id", &id)
                        .with("lease-id", &lease)
                        .with("artifact", absolute.as_os_str()),
                )?;
                output.append(landed);
                let repaired = delegate(ctx, args, &id, &route, 0, absolute.to_str(), wall)?;
                let repair_code = repaired.code;
                output.append(repaired);
                if repair_code == 0 {
                    task = show(ctx, &id)?;
                    require(
                        value::text(&task, "state") == "review"
                            && value::text(&task, "lease_id") == lease
                            && task["repair_count"].as_u64() == Some(1),
                        "repair did not retain its exact one-shot review",
                    )?;
                    landed = land(ctx, args, &task, wall)?;
                } else {
                    terminalize_repair(ctx, &id, &lease)?;
                    output.code = repair_code;
                    return Ok(output);
                }
            }
            let code = landed.code;
            output.append(landed);
            if code != 0 {
                output.stderr.extend_from_slice(
                    format!(
                        "plan-run: parked task={id} state={} next=advise\n",
                        value::text(&show(ctx, &id)?, "state")
                    )
                    .as_bytes(),
                );
                return Ok(output);
            }
            task = show(ctx, &id)?;
            require(
                value::text(&task, "state") == "done",
                "landing returned success without a completed native task receipt",
            )?;
        }
        output.append(Output::text(format!(
            "plan-run: result task={id} state={} artifact={} patch={} next={}",
            if args.flag("land") { "done" } else { "review" },
            value::text(&task, "artifact"),
            value::text(&task, "patch"),
            if args.flag("land") {
                "continue-plan"
            } else {
                "review-or-land"
            }
        )));
        Ok(output)
    })();
    // Cleanup releases only the lease this call owns, never a replacement
    // claimant or the durable review/repair evidence left by another producer.
    if claimed
        && let Ok(current) = show(ctx, &id)
        && ["claimed", "running"].contains(&value::text(&current, "state"))
        && value::text(&current, "lease_id") == lease
        && current["repair_count"].as_u64().unwrap_or(0) == 0
    {
        let _ = action(ctx, Action::Release, &id, &lease);
    }
    result
}
fn terminalize_repair(ctx: &Context<'_>, id: &str, lease: &str) -> Result<()> {
    let current = show(ctx, id)?;
    require(
        current["repair_count"].as_u64() == Some(1)
            && value::text(&current, "review_lease_id") == lease,
        "failed repair lost its exact one-shot receipt",
    )?;
    if value::text(&current, "state") != "blocked" {
        require(
            ["claimed", "running", "review", "landing"].contains(&value::text(&current, "state"))
                && value::text(&current, "lease_id") == lease,
            "failed repair cannot be blocked without its current lease",
        )?;
        ctx.action(
            &Request::new(Action::Block)
                .with("id", id)
                .with("lease-id", lease)
                .with("reason", "bounded landing repair failed"),
        )?;
    }
    let current = show(ctx, id)?;
    require(
        value::text(&current, "state") == "blocked"
            && current["repair_count"].as_u64() == Some(1)
            && value::text(&current, "review_lease_id") == lease,
        "failed repair did not converge to the blocked receipt",
    )
}
