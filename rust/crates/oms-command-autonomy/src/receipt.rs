//! Frozen schema-1 autopilot contracts and exact proposal approval boundaries.
use crate::cli::Args;
use crate::planning::{self, envelope, invoke};
use crate::storage::{ACTIVE, Context, MAX_FILE, PROGRESS, RECEIPT};
use crate::{Error, Output, Result, require, value, verify};
use oms_command_plan::{Action, Request};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

pub const STAGES: &[&str] = &[
    "proposing",
    "proposal-review",
    "approved",
    "driving",
    "reviewing",
    "publishing",
    "parked",
    "done",
];
pub fn immutable_view(row: &Value) -> Value {
    let mut row = row.clone();
    if let Some(map) = row.as_object_mut() {
        for key in ["stage", "updated", "proposal", "branch", "diagnostic"] {
            map.remove(key);
        }
    }
    row
}
pub fn valid_transition(before: &str, after: &str) -> bool {
    match before {
        "proposing" => ["proposing", "proposal-review", "parked"].contains(&after),
        "proposal-review" => ["proposal-review", "approved", "parked"].contains(&after),
        "approved" => ["proposal-review", "approved", "driving", "parked"].contains(&after),
        "driving" => [
            "proposing",
            "proposal-review",
            "approved",
            "driving",
            "reviewing",
            "parked",
        ]
        .contains(&after),
        "reviewing" => [
            "proposal-review",
            "approved",
            "reviewing",
            "publishing",
            "done",
            "parked",
        ]
        .contains(&after),
        "publishing" => [
            "proposal-review",
            "approved",
            "publishing",
            "done",
            "parked",
        ]
        .contains(&after),
        "parked" => [
            "parked",
            "proposing",
            "proposal-review",
            "approved",
            "driving",
            "reviewing",
            "publishing",
        ]
        .contains(&after),
        "done" => after == "done",
        _ => false,
    }
}
pub fn validate(row: &Value) -> Result<()> {
    value::keys(
        row,
        &[
            "schema",
            "kind",
            "stage",
            "repo",
            "spec_sha256",
            "providers",
            "routing",
            "contract",
            "proposal",
            "branch",
            "owner_id",
            "updated",
        ],
        &["diagnostic"],
    )?;
    require(
        row["schema"].as_u64() == Some(1)
            && row["kind"] == "autopilot-run"
            && STAGES.contains(&value::text(row, "stage")),
        "outer receipt does not match schema 1",
    )?;
    require(
        Path::new(value::text(row, "repo")).is_absolute()
            && value::digest(value::text(row, "spec_sha256")),
        "invalid receipt repository/spec binding",
    )?;
    let owner = value::text(row, "owner_id");
    require(
        owner.is_empty()
            || owner.strip_prefix("owner_").is_some_and(|s| {
                s.len() == 32
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }),
        "invalid autopilot owner id",
    )?;
    value::keys(&row["providers"], &["planner", "worker", "reviewer"], &[])?;
    value::keys(
        &row["routing"],
        &["provider_timeout", "planner", "worker", "reviewer"],
        &[],
    )?;
    verify::duration(value::text(&row["routing"], "provider_timeout"))?;
    for role in ["planner", "worker", "reviewer"] {
        require(
            ["codex", "claude", "antigravity"].contains(&value::text(&row["providers"], role)),
            "invalid provider binding",
        )?;
        let route = &row["routing"][role];
        value::keys(
            route,
            &["model", "fallback_model", "reasoning_effort", "timeout"],
            &[],
        )?;
        for field in ["model", "fallback_model"] {
            require(route[field].is_string(), "route model must be text")?;
            verify::model(value::text(route, field))?;
        }
        verify::effort(value::text(route, "reasoning_effort"))?;
        verify::duration(value::text(route, "timeout"))?;
    }
    let contract = &row["contract"];
    value::keys(
        contract,
        &[
            "allowed",
            "base",
            "base_sha",
            "remote",
            "max_cycles",
            "initial_tasks",
            "replan_tasks",
            "auto_repair",
            "retry_known",
            "review_mode",
            "draft_pr",
            "allow_verifier_change",
            "allow_floor_change",
            "collaboration",
        ],
        &[],
    )?;
    let allowed = envelope(value::text(contract, "allowed"))?;
    require(!allowed.is_empty(), "run envelope cannot be empty")?;
    require(
        regex::Regex::new(r"^[A-Za-z0-9._/-]+$")
            .unwrap()
            .is_match(value::text(contract, "base"))
            && !value::text(contract, "base").starts_with('-')
            && !value::text(contract, "base").contains("..")
            && value::id(value::text(contract, "remote"), 160)
            && value::commit(value::text(contract, "base_sha")),
        "invalid review base/remote binding",
    )?;
    let codex_model_re =
        regex::Regex::new(r"^gpt-([0-9]+)(?:\.[0-9]+)?-(?:astra|sol|terra|luna)$").unwrap();
    value::integer(&contract["max_cycles"], 1, 10, "max cycles")?;
    value::integer(&contract["initial_tasks"], 1, 12, "initial tasks")?;
    value::integer(&contract["replan_tasks"], 1, 2, "replan tasks")?;
    for field in [
        "auto_repair",
        "retry_known",
        "draft_pr",
        "allow_verifier_change",
    ] {
        require(contract[field].is_boolean(), "run flag must be a boolean")?;
    }
    require(
        contract["allow_floor_change"].is_string(),
        "floor consent must be text",
    )?;
    if !value::text(contract, "allow_floor_change").is_empty() {
        verify::reason(value::text(contract, "allow_floor_change"))?;
    }
    require(
        ["shadow", "gate", "off"].contains(&value::text(contract, "review_mode"))
            && ["off", "auto"].contains(&value::text(contract, "collaboration")),
        "invalid review/collaboration mode",
    )?;
    if contract["collaboration"] == "auto" {
        require(
            contract["review_mode"] == "gate"
                && row["providers"]["worker"] != row["providers"]["reviewer"],
            "auto collaboration requires gated independent review",
        )?;
        for role in ["planner", "worker", "reviewer"] {
            let provider = value::text(&row["providers"], role);
            require(
                ["codex", "claude"].contains(&provider),
                "auto collaboration requires codex/claude transports",
            )?;
            if provider == "codex" {
                for field in ["model", "fallback_model"] {
                    let model = value::text(&row["routing"][role], field);
                    if field == "model" || !model.is_empty() {
                        require(
                            codex_model_re
                                .captures(model)
                                .and_then(|c| c[1].parse::<u64>().ok())
                                .is_some_and(|n| n >= 6),
                            "auto collaboration requires GPT-6 or newer Codex routes",
                        )?;
                    }
                }
            }
        }
    }
    value::keys(&row["proposal"], &["path", "sha256"], &[])?;
    let path = value::text(&row["proposal"], "path");
    let hash = value::text(&row["proposal"], "sha256");
    require(
        row["proposal"]["path"].is_string()
            && row["proposal"]["sha256"].is_string()
            && path.is_empty() == hash.is_empty()
            && (path.is_empty() || Path::new(path).is_absolute() && value::digest(hash)),
        "invalid proposal binding",
    )?;
    require(
        row["stage"] != "proposing" || path.is_empty(),
        "proposing receipt cannot bind a proposal",
    )?;
    require(
        row["stage"] != "proposal-review" || !path.is_empty(),
        "proposal-review receipt lacks a proposal",
    )?;
    let branch = format!("oms/autopilot-{}", &value::text(row, "spec_sha256")[..12]);
    require(
        value::text(row, "branch") == branch
            || value::text(row, "branch")
                .strip_prefix(&format!("{branch}-r"))
                .is_some_and(|s| {
                    !s.starts_with('0') && !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
                }),
        "branch is outside deterministic autopilot lineage",
    )?;
    require(
        regex::Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")
            .unwrap()
            .is_match(value::text(row, "updated"))
            && chrono::DateTime::parse_from_rfc3339(value::text(row, "updated")).is_ok(),
        "invalid receipt timestamp",
    )?;
    if let Some(diagnostic) = row.get("diagnostic") {
        require(
            row["stage"] == "parked",
            "diagnostic is allowed only on parked receipts",
        )?;
        value::keys(
            diagnostic,
            &[
                "phase",
                "reason_code",
                "reason",
                "semantic_verdict",
                "evidence_path",
            ],
            &[],
        )?;
        for (key, limit) in [
            ("phase", 48),
            ("reason_code", 96),
            ("reason", 240),
            ("semantic_verdict", 32),
            ("evidence_path", 1024),
        ] {
            require(
                diagnostic[key].is_string()
                    && value::text(diagnostic, key).chars().count() <= limit
                    && value::clean_text(value::text(diagnostic, key)),
                "invalid diagnostic text",
            )?;
        }
        require(
            [
                "unknown",
                "pass",
                "fail",
                "incomplete",
                "advisory-fail",
                "advisory-incomplete",
                "skipped",
            ]
            .contains(&value::text(diagnostic, "semantic_verdict")),
            "invalid diagnostic semantic verdict",
        )?;
    }
    Ok(())
}
fn base(ctx: &Context<'_>, remote: &str, branch: &str) -> Result<String> {
    require(
        !branch.starts_with('-') && !branch.contains("..") && value::id(remote, 160),
        "invalid base or remote",
    )?;
    for reference in [
        format!("refs/remotes/{remote}/{branch}"),
        format!("refs/heads/{branch}"),
    ] {
        if let Ok(commit) = ctx.git(&["rev-parse", "--verify", &format!("{reference}^{{commit}}")])
            && value::commit(&commit)
        {
            return Ok(commit);
        }
    }
    Err(Error::new(
        2,
        "cannot resolve review base to a local commit",
    ))
}
pub(crate) fn live(ctx: &Context<'_>, row: &Value) -> Result<()> {
    validate(row)?;
    require(
        std::fs::canonicalize(value::text(row, "repo"))
            .ok()
            .as_ref()
            == Some(&ctx.repo),
        "outer receipt belongs to a different execution repository",
    )?;
    require(
        value::hash(ctx.source.read("PROJECT.md", MAX_FILE)?) == value::text(row, "spec_sha256"),
        "PROJECT.md differs from frozen outer receipt",
    )?;
    let contract = &row["contract"];
    require(
        base(
            ctx,
            value::text(contract, "remote"),
            value::text(contract, "base"),
        )? == value::text(contract, "base_sha"),
        "bound base ref moved",
    )?;
    let current = ctx
        .git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_default();
    let initial = ["proposing", "proposal-review", "parked"].contains(&value::text(row, "stage"));
    require(
        current == value::text(row, "branch")
            || initial
                && (current == value::text(contract, "base")
                    || current.is_empty()
                        && ctx.git(&["rev-parse", "HEAD"])? == value::text(contract, "base_sha")),
        "current branch differs from outer receipt",
    )?;
    let path = value::text(&row["proposal"], "path");
    if !path.is_empty() {
        require(
            Path::new(path).parent() == Some(ctx.state.join(".oms/plan").as_path()),
            "approved proposal is outside private plan directory",
        )?;
        require(
            value::hash(ctx.store.read(path, MAX_FILE)?) == value::text(&row["proposal"], "sha256"),
            "approved proposal digest changed",
        )?;
    }
    Ok(())
}
fn save(ctx: &Context<'_>, row: &mut Value, old: Option<&[u8]>) -> Result<Vec<u8>> {
    row["updated"] = value::now().into();
    validate(row)?;
    if let Some(old) = old {
        let before = value::parse(old)?;
        require(
            immutable_view(&before) == immutable_view(row),
            "outer receipt immutable contract changed; abandon before binding a new contract",
        )?;
        require(
            valid_transition(value::text(&before, "stage"), value::text(row, "stage")),
            "outer receipt stage transition is invalid",
        )?;
        require(
            before["branch"] == row["branch"],
            "outer receipt branch changed",
        )?;
        if before["proposal"] != row["proposal"] {
            require(
                before["stage"] == "proposing" && row["stage"] == "proposal-review"
                    || ["driving", "parked"].contains(&value::text(&before, "stage"))
                        && row["stage"] == "proposing"
                        && row["proposal"] == json!({"path": "", "sha256": ""}),
                "outer receipt proposal transition is invalid",
            )?;
        }
    }
    let raw = value::pretty(row)?;
    ctx.store.mutate(
        RECEIPT,
        Some(old.unwrap_or_default()),
        &raw,
        64 * 1024,
        true,
    )?;
    Ok(raw)
}
fn park(
    ctx: &Context<'_>,
    row: &mut Value,
    raw: &[u8],
    reason: &str,
    mut out: Output,
) -> Result<Output> {
    row["stage"] = "parked".into();
    row["diagnostic"] = json!({"phase": "autopilot", "reason_code": reason, "reason": reason, "semantic_verdict": "unknown", "evidence_path": ""});
    save(ctx, row, Some(raw))?;
    let _lock = ctx.lock(PROGRESS)?;
    ctx.append(PROGRESS, &json!({"schema": 1, "kind": "terminal", "ts": value::now(), "run_id": value::text(row, "owner_id"), "status": "park", "reason": reason, "base_sha": ctx.git(&["rev-parse", "HEAD"])?, "receipt": value::hash(value::canonical(row)?)}))?;
    out.code = 3;
    out.stderr.extend_from_slice(format!("autopilot: parked reason={reason}; inspect the receipt and abandon before changing its contract\n").as_bytes());
    Ok(out)
}
fn fresh(ctx: &Context<'_>, args: &Args) -> Result<Value> {
    let spec = planning::contract(ctx)?;
    spec.confirmed()?;
    let planner = ctx
        .services
        .normalize_provider(args.get("planner", "claude")?)?;
    let worker = ctx
        .services
        .normalize_provider(args.get("worker", "codex")?)?;
    let reviewer = ctx.services.normalize_provider(args.get(
        "reviewer",
        if worker == "codex" { "claude" } else { "codex" },
    )?)?;
    let provider_timeout = args.get("provider-timeout", "5m")?;
    let mut routing = json!({"provider_timeout": provider_timeout});
    for (role, default) in [("planner", "15m"), ("worker", "30m"), ("reviewer", "20m")] {
        routing[role] = json!({"model": args.text(&format!("{role}-model"))?, "fallback_model": args.text(&format!("{role}-fallback-model"))?, "reasoning_effort": args.get(&format!("{role}-reasoning-effort"), "auto")?, "timeout": args.get(&format!("{role}-timeout"), if args.has("provider-timeout") { provider_timeout } else { default })?});
    }
    if args.get("collaboration", "off")? == "auto" {
        for (role, provider) in [
            ("planner", &planner),
            ("worker", &worker),
            ("reviewer", &reviewer),
        ] {
            if value::text(&routing[role], "model").is_empty() {
                let assignment = json!({"provider": provider, "model": "", "fallback_model": value::text(&routing[role], "fallback_model"), "reasoning_effort": value::text(&routing[role], "reasoning_effort"), "workload": "standard"});
                let resolved =
                    ctx.services
                        .resolve_route(&ctx.repo, &assignment, role, ctx.interrupted)?;
                require(
                    &resolved.provider == provider,
                    "model resolver changed frozen provider",
                )?;
                routing[role]["model"] = resolved.primary.into();
                routing[role]["fallback_model"] = resolved.fallback.into();
                routing[role]["reasoning_effort"] = resolved.effort.into();
            }
        }
    }
    let allowed = if args.has("allowed") {
        envelope(args.required("allowed")?)?
    } else {
        spec.allowed.clone()
    };
    require(
        !allowed.is_empty(),
        "autopilot needs an immutable --allowed envelope or PROJECT.md Scope",
    )?;
    let branch = args.get("base", "")?.to_owned();
    let branch = if branch.is_empty() {
        ctx.git(&["symbolic-ref", "--quiet", "--short", "HEAD"])?
    } else {
        branch
    };
    require(
        args.action != "run" || args.has("base"),
        "--base is required for a new run",
    )?;
    let remote = args.get("remote", "origin")?;
    let review_mode = args.get(
        "review-mode",
        if args.flag("draft-pr") {
            "gate"
        } else {
            "shadow"
        },
    )?;
    let row = json!({"schema": 1, "kind": "autopilot-run", "stage": "proposing", "repo": ctx.repo, "spec_sha256": spec.sha256, "providers": {"planner": planner, "worker": worker, "reviewer": reviewer}, "routing": routing, "contract": {"allowed": allowed.join(","), "base": branch, "base_sha": base(ctx, remote, &branch)?, "remote": remote, "max_cycles": args.number("max-cycles", 10, 1, 10)?, "initial_tasks": args.number("initial-tasks", 6, 1, 12)?, "replan_tasks": args.number("replan-tasks", 2, 1, 2)?, "auto_repair": args.flag("auto-repair"), "retry_known": args.flag("retry-known"), "review_mode": review_mode, "draft_pr": args.flag("draft-pr"), "allow_verifier_change": args.flag("allow-verifier-change"), "allow_floor_change": args.text("allow-floor-change")?, "collaboration": args.get("collaboration", "off")?}, "proposal": {"path": "", "sha256": ""}, "branch": format!("oms/autopilot-{}", &spec.sha256[..12]), "owner_id": value::random("owner_")?, "updated": value::now()});
    validate(&row)?;
    Ok(row)
}
fn options_match(args: &Args, row: &Value) -> Result<()> {
    for role in ["planner", "worker", "reviewer"] {
        if args.has(role) {
            require(
                args.text(role)? == value::text(&row["providers"], role),
                format!("frozen {role} differs; abandon the run before changing it"),
            )?;
        }
        for (option, field) in [
            ("model", "model"),
            ("fallback-model", "fallback_model"),
            ("reasoning-effort", "reasoning_effort"),
            ("timeout", "timeout"),
        ] {
            let key = format!("{role}-{option}");
            if args.has(&key) {
                require(
                    args.text(&key)? == value::text(&row["routing"][role], field),
                    format!("frozen {key} differs"),
                )?;
            }
        }
    }
    for (option, key) in [
        ("base", "base"),
        ("remote", "remote"),
        ("review-mode", "review_mode"),
        ("collaboration", "collaboration"),
        ("allow-floor-change", "allow_floor_change"),
    ] {
        if args.has(option) {
            require(
                args.text(option)? == value::text(&row["contract"], key),
                format!("frozen {option} differs"),
            )?;
        }
    }
    if args.has("allowed") {
        require(
            envelope(args.required("allowed")?)?
                == envelope(value::text(&row["contract"], "allowed"))?,
            "frozen allowed envelope differs",
        )?;
    }
    for (option, key) in [
        ("max-cycles", "max_cycles"),
        ("initial-tasks", "initial_tasks"),
        ("replan-tasks", "replan_tasks"),
    ] {
        if args.has(option) {
            require(
                args.required(option)?.parse::<u64>().ok() == row["contract"][key].as_u64(),
                format!("frozen {option} differs"),
            )?;
        }
    }
    for (option, key) in [
        ("auto-repair", "auto_repair"),
        ("retry-known", "retry_known"),
        ("allow-verifier-change", "allow_verifier_change"),
        ("draft-pr", "draft_pr"),
    ] {
        if args.flag(option) {
            require(
                row["contract"][key] == true,
                format!("cannot add {option} to a frozen contract"),
            )?;
        }
    }
    if args.has("provider-timeout") {
        require(
            args.text("provider-timeout")? == value::text(&row["routing"], "provider_timeout"),
            "frozen provider timeout differs",
        )?;
    }
    Ok(())
}
fn propose(
    ctx: &Context<'_>,
    args: &Args,
    row: &mut Value,
    raw: &[u8],
    remainder: bool,
) -> Result<Output> {
    let mut planning = Args::default();
    planning.set("to", value::text(&row["providers"], "planner"));
    planning.set("worker-provider", value::text(&row["providers"], "worker"));
    planning.set(
        "collaboration",
        value::text(&row["contract"], "collaboration"),
    );
    planning.set("allowed", value::text(&row["contract"], "allowed"));
    planning.set(
        "max-tasks",
        row["contract"][if remainder {
            "replan_tasks"
        } else {
            "initial_tasks"
        }]
        .to_string(),
    );
    for (option, field) in [
        ("model", "model"),
        ("fallback-model", "fallback_model"),
        ("reasoning-effort", "reasoning_effort"),
        ("provider-timeout", "timeout"),
    ] {
        let text = value::text(&row["routing"]["planner"], field);
        if !text.is_empty() {
            planning.set(option, text);
        }
    }
    if remainder {
        planning.set("id-prefix", "r1-");
    }
    if args.flag("discover") && !remainder {
        planning.flags.insert("discover".into());
    }
    if row["contract"]["allow_verifier_change"] == true {
        planning.flags.insert("allow-verifier-change".into());
    }
    let mut out = planning::execute(ctx, &planning)?;
    let text = String::from_utf8_lossy(&out.stdout);
    if args.flag("discover")
        && !remainder
        && text
            .lines()
            .any(|line| line == "plan-from-spec: no proposal; no eligible discovery candidates")
    {
        let predecessor = value::hash(raw);
        ctx.store.publish_once(
            format!(".oms/plan/autopilot-run.{predecessor}.json"),
            raw,
            64 * 1024,
        )?;
        let record = json!({"schema": 1, "kind": "autopilot-discovery-retire", "predecessor": predecessor, "owner_id": row["owner_id"], "spec_sha256": row["spec_sha256"], "reason": "discovery-no-eligible-work", "updated": value::now()});
        ctx.store.publish_once(
            format!(".oms/plan/autopilot-run.{predecessor}.discovery-retired.json"),
            &value::pretty(&record)?,
            64 * 1024,
        )?;
        ctx.store.remove(RECEIPT, raw, None)?;
        return Ok(out);
    }
    let path = text
        .lines()
        .find_map(|line| {
            line.strip_prefix("plan-from-spec: proposed ")
                .and_then(|line| line.split_once(" -> ").map(|(_, path)| path))
        })
        .ok_or_else(|| Error::new(3, "planner returned no durable proposal"))?;
    let bytes = ctx.store.read(path, MAX_FILE)?;
    row["stage"] = "proposal-review".into();
    row["proposal"] = json!({"path": path, "sha256": value::hash(&bytes)});
    save(ctx, row, Some(raw))?;
    out.code = 4;
    out.stdout.extend_from_slice(format!("autopilot: parent review required\noms autopilot run --proposal {path} --expected-proposal-sha256 {}\n", value::hash(bytes)).as_bytes());
    Ok(out)
}
fn assert_scope(ctx: &Context<'_>, row: &Value) -> Result<()> {
    let base = value::text(&row["contract"], "base_sha");
    ctx.plan()?.git().ancestor(base, "HEAD")?;
    let allowed = envelope(value::text(&row["contract"], "allowed"))?;
    let allowed = allowed.iter().map(String::as_str).collect::<Vec<_>>();
    let raw = ctx.plan()?.git().run(&[
        "diff",
        "--name-only",
        "--no-renames",
        "-z",
        base,
        "HEAD",
        "--",
    ])?;
    for path in raw.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path =
            std::str::from_utf8(path).map_err(|_| Error::new(3, "changed path is not UTF-8"))?;
        require(
            oms_contract_markdown::scope::matches_any(path, &allowed)
                .map_err(|e| Error::new(3, e.to_string()))?,
            "work branch changed paths outside immutable envelope",
        )?;
    }
    Ok(())
}
fn drive_result(ctx: &Context<'_>, run: &str, output: &Output) -> Result<Value> {
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| Error::new(3, "goal-drive returned non-text terminal evidence"))?;
    let lines = text
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let terminal_lines = lines
        .iter()
        .filter(|line| line.starts_with("goal-drive: terminal-v1 "))
        .collect::<Vec<_>>();
    require(
        terminal_lines.len() == 1 && lines.last() == terminal_lines.first().copied(),
        "goal-drive terminal must be its single final canonical line",
    )?;
    let rows = ctx.rows(PROGRESS)?;
    let row = rows
        .into_iter()
        .rev()
        .find(|row| row["kind"] == "terminal" && value::text(row, "run_id") == run)
        .ok_or_else(|| Error::new(3, "goal-drive lacks its exact durable terminal row"))?;
    let receipt = value::text(&row, "receipt");
    let status = value::text(&row, "status");
    let reason = value::text(&row, "reason");
    require(
        value::digest(receipt) && ["park", "done"].contains(&status) && value::id(reason, 128),
        "goal-drive terminal row is malformed",
    )?;
    let expected = format!(
        "goal-drive: terminal-v1 run={run} receipt={receipt} status={status} reason={reason}"
    );
    require(
        lines.last().copied() == Some(expected.as_str())
            && (output.code == 0) == (status == "done"),
        "goal-drive result differs from durable terminal evidence",
    )?;
    Ok(row)
}
pub(crate) fn execute(ctx: &Context<'_>, args: &Args) -> Result<Output> {
    let old = ctx.store.optional(RECEIPT, 64 * 1024)?;
    if args.action == "status" {
        let receipt = old.as_ref().map(|r| value::parse(r)).transpose()?;
        let rows = ctx.rows(PROGRESS)?;
        return Output::json(
            &json!({"receipt": receipt, "plan": ctx.plan_value()?, "terminal": rows.iter().rev().find(|r| value::text(r, "kind") == "terminal")}),
        );
    }
    ctx.parent()?;
    // One controller owns the entire foreground run. Receipt writes still use
    // exact byte CAS; a killed owner releases this lock, never its spent cycles.
    let _owner = ctx.lock(".oms/plan/autopilot-controller")?;
    let _receipt = ctx.lock(RECEIPT)?;
    let mut raw = ctx.store.optional(RECEIPT, 64 * 1024)?;
    let mut row = raw.as_ref().map(|r| value::parse(r)).transpose()?;
    if args.action == "abandon" {
        let reason = args.required("reason")?;
        verify::reason(reason)?;
        require(
            reason.chars().count() <= 400,
            "abandon reason exceeds 400 characters",
        )?;
        let old = raw
            .as_ref()
            .ok_or_else(|| Error::new(2, "no live autopilot receipt to abandon"))?;
        let row = row.as_ref().unwrap();
        validate(row)?;
        require(
            row["stage"] != "done",
            "done receipts are archived, not abandoned",
        )?;
        let hash = value::hash(old);
        let path = format!(".oms/plan/autopilot-run.{hash}.json");
        ctx.store.publish_once(&path, old, 64 * 1024)?;
        let record = json!({"schema": 1, "kind": "autopilot-abandon", "predecessor": hash, "stage_at_abandon": row["stage"], "spec_sha256": row["spec_sha256"], "branch": row["branch"], "reason": reason, "updated": value::now()});
        ctx.store.publish_once(
            format!(".oms/plan/autopilot-run.{hash}.abandoned.json"),
            &value::pretty(&record)?,
            64 * 1024,
        )?;
        ctx.store.remove(RECEIPT, old, None)?;
        return Ok(Output::text(format!(
            "autopilot: abandoned {hash}; unfinished tasks remain on the plan; inspect oms agent-plan retire --check and explicit cancellation requirements"
        )));
    }
    if ["shadow", "reenter"].contains(&args.action.as_str()) {
        let Some(bound) = row.as_ref() else {
            return if args.action == "shadow" {
                Ok(Output::default())
            } else {
                Err(Error::new(3, "no live autopilot receipt"))
            };
        };
        let judgment = live(ctx, bound).and_then(|_| {
            require(
                !["proposal-review", "parked", "done"].contains(&value::text(bound, "stage")),
                "reentry cannot supply parent approval, reset a park or resume done work",
            )
        });
        let record = json!({"schema": 1, "kind": if args.action == "shadow" { "autopilot-shadow" } else { "autopilot-reentry" }, "ts": value::now(), "receipt_sha256": value::hash(raw.as_ref().unwrap()), "owner_id": bound["owner_id"], "session": args.text("session")?, "outcome": if judgment.is_ok() { "would-resume" } else { "would-refuse" }, "reason": judgment.as_ref().err().map(|e| e.message.as_str()).unwrap_or("")});
        ctx.append(
            if args.action == "shadow" {
                ".oms/plan/autopilot-shadow.jsonl"
            } else {
                ".oms/plan/autopilot-reentries.jsonl"
            },
            &record,
        )?;
        if args.action == "shadow" {
            return Output::json(&record);
        }
        judgment?;
    }
    if row.is_none() {
        require(
            args.action == "propose" || args.action == "run",
            "autopilot action requires a live receipt",
        )?;
        let mut fresh = fresh(ctx, args)?;
        if args.action == "run" && (args.has("proposal") || args.has("expected-proposal-sha256")) {
            let expected = args.required("expected-proposal-sha256")?;
            require(value::digest(expected), "invalid expected proposal SHA-256")?;
            let reviewed = ctx.bytes(&args.path("proposal", &ctx.repo), MAX_FILE)?;
            require(
                value::hash(&reviewed) == expected,
                "proposal bytes changed after review",
            )?;
            planning::scan(&reviewed)?;
            let proposal = value::parse(&reviewed)?;
            let spec = planning::contract(ctx)?;
            planning::validate(ctx, &proposal, &spec, 12)?;
            require(
                envelope(value::text(&fresh["contract"], "allowed"))?
                    == planning::strings(&proposal["allowed_envelope"])?,
                "reviewed proposal differs from frozen envelope",
            )?;
            let snapshot = format!(".oms/plan/approved-proposal-{expected}.json");
            ctx.store.publish_once(&snapshot, &reviewed, MAX_FILE)?;
            fresh["stage"] = "proposal-review".into();
            fresh["proposal"] = json!({"path": ctx.state.join(snapshot), "sha256": expected});
        }
        let written = save(ctx, &mut fresh, None)?;
        row = Some(fresh);
        raw = Some(written);
    }
    let mut row = row.unwrap();
    let mut raw = raw.unwrap();
    let mut owned = Context::new(
        ctx.repo.clone(),
        ctx.state.clone(),
        ctx.services,
        ctx.interrupted,
    )?;
    owned.owner_id = value::text(&row, "owner_id").to_owned();
    let ctx = &owned;
    live(ctx, &row)?;
    options_match(args, &row)?;
    require(
        !value::text(&row, "owner_id").is_empty(),
        "legacy receipt lacks lifetime owner identity; retire it before starting a native drive",
    )?;
    if args.action == "reenter" {
        let recovered = ctx.action(
            &Request::new(Action::RecoverOwner)
                .with("owner-id", value::text(&row, "owner_id"))
                .flag("json"),
        )?;
        require(recovered.code == 0, "dead-owner lease recovery failed")?;
        ctx.append(".oms/plan/autopilot-reentries.jsonl", &json!({"schema": 1, "kind": "autopilot-reentry-requeue", "ts": value::now(), "owner_id": row["owner_id"], "recovery": value::parse(&recovered.stdout)?}))?;
    }
    require(
        row["stage"] != "parked",
        "autopilot receipt is parked; an operator decision is open; abandon before a new contract",
    )?;
    if row["stage"] == "done" {
        return Ok(Output::text("autopilot: done (acceptance passed)"));
    }
    if args.action == "propose" || row["stage"] == "proposing" {
        require(
            !args.has("proposal") && row["stage"] == "proposing",
            "a reviewed proposal cannot be silently replaced; abandon the receipt first",
        )?;
        let reservations = ctx.rows(".oms/plan/autopilot-replans.jsonl")?;
        let remainder = reservations
            .iter()
            .any(|r| r["owner_id"] == row["owner_id"]);
        return propose(ctx, args, &mut row, &raw, remainder);
    }
    if args.has("proposal") || args.has("expected-proposal-sha256") {
        require(
            args.has("proposal") && args.has("expected-proposal-sha256"),
            "proposal path and reviewed SHA-256 are required together",
        )?;
        let expected = args.required("expected-proposal-sha256")?;
        require(value::digest(expected), "invalid expected proposal SHA-256")?;
        let supplied = args.path("proposal", &ctx.repo);
        let supplied = if supplied.is_absolute() {
            supplied
        } else {
            ctx.repo.join(supplied)
        };
        let frozen = value::text(&row["proposal"], "path");
        require(
            expected == value::text(&row["proposal"], "sha256")
                && value::hash(ctx.bytes(&supplied, MAX_FILE)?) == expected,
            "proposal differs from the exact parent-reviewed binding",
        )?;
        let mut apply_args = Args::default();
        apply_args.set("apply", frozen);
        apply_args.set("expected-proposal-sha256", expected);
        let reviewed = value::parse(&ctx.store.read(frozen, MAX_FILE)?)?;
        let cap = if value::text(&reviewed, "id_prefix") == "r1-" {
            "replan_tasks"
        } else {
            "initial_tasks"
        };
        apply_args.set("max-tasks", row["contract"][cap].to_string());
        apply_args.set("allowed", value::text(&row["contract"], "allowed"));
        apply_args.set(
            "collaboration",
            value::text(&row["contract"], "collaboration"),
        );
        apply_args.set("worker-provider", value::text(&row["providers"], "worker"));
        planning::apply(ctx, &apply_args)?;
        row["stage"] = "approved".into();
        raw = save(ctx, &mut row, Some(&raw))?;
        let approval = json!({"schema": 1, "kind": "autopilot-approval", "ts": value::now(), "owner_id": row["owner_id"], "spec_sha256": row["spec_sha256"], "proposal_sha256": row["proposal"]["sha256"]});
        ctx.append(".oms/plan/autopilot-approvals.jsonl", &approval)?;
    }
    require(
        row["stage"] != "proposal-review",
        "parent proposal review required; pass --proposal and --expected-proposal-sha256",
    )?;
    let base_branch = value::text(&row["contract"], "base").to_owned();
    let work_branch = value::text(&row, "branch").to_owned();
    let current = ctx
        .git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_default();
    if current != work_branch {
        ctx.clean()?;
        require(
            current == base_branch || current.is_empty(),
            "refusing work branch switch from an unrelated checkout",
        )?;
        require(
            ctx.git(&["rev-parse", "HEAD"])? == value::text(&row["contract"], "base_sha"),
            "base checkout moved before work branch switch",
        )?;
        ctx.git(&["switch", "-c", &work_branch])?;
    }
    ctx.clean()?;
    assert_scope(ctx, &row)?;
    row["stage"] = "driving".into();
    raw = save(ctx, &mut row, Some(&raw))?;
    let mut drive = Args::default();
    let drive_run = value::random("goal-")?;
    drive.set("run-id", &drive_run);
    drive.set("to", value::text(&row["providers"], "worker"));
    drive.set("max-cycles", row["contract"]["max_cycles"].to_string());
    drive.set("contract-id", value::text(&row, "owner_id"));
    drive.set("expected-ref", format!("refs/heads/{work_branch}"));
    for (option, field) in [
        ("model", "model"),
        ("fallback-model", "fallback_model"),
        ("reasoning-effort", "reasoning_effort"),
        ("provider-timeout", "timeout"),
    ] {
        let text = value::text(&row["routing"]["worker"], field);
        if !text.is_empty() {
            drive.set(option, text);
        }
    }
    for (flag, field) in [
        ("auto-repair", "auto_repair"),
        ("retry-known", "retry_known"),
        ("allow-verifier-change", "allow_verifier_change"),
    ] {
        if row["contract"][field] == true {
            drive.flags.insert(flag.into());
        }
    }
    if !value::text(&row["contract"], "allow_floor_change").is_empty() {
        drive.set(
            "allow-floor-change",
            value::text(&row["contract"], "allow_floor_change"),
        );
    }
    let mut output = crate::goal::execute(ctx, &drive)?;
    let terminal = match drive_result(ctx, &drive_run, &output) {
        Ok(row) => row,
        Err(_) => return park(ctx, &mut row, &raw, "goal-drive-result-invalid", output),
    };
    if output.code != 0 {
        let reason = value::text(&terminal, "reason");
        let plan = ctx.plan_value()?;
        let all_done = plan["tasks"]
            .as_object()
            .is_some_and(|t| t.values().all(|t| value::text(t, "state") == "done"));
        let replan_used = plan["tasks"]
            .as_object()
            .is_some_and(|t| t.keys().any(|id| id.starts_with("r1-")));
        if reason == "tasks-exhausted" && all_done && !replan_used {
            let reservations = ctx.rows(".oms/plan/autopilot-replans.jsonl")?;
            let matching = reservations
                .iter()
                .filter(|r| r["owner_id"] == row["owner_id"])
                .collect::<Vec<_>>();
            require(
                matching.len() <= 1,
                "replan reservation ledger is ambiguous",
            )?;
            if matching.is_empty() {
                ctx.append(".oms/plan/autopilot-replans.jsonl", &json!({"schema": 1, "kind": "autopilot-replan-reserved", "owner_id": row["owner_id"], "proposal_sha256": row["proposal"]["sha256"], "ts": value::now()}))?;
            } else {
                require(
                    matching[0]["proposal_sha256"] == row["proposal"]["sha256"],
                    "sole remainder reservation already belongs to a different tranche",
                )?;
            }
            row["stage"] = "proposing".into();
            row["proposal"] = json!({"path": "", "sha256": ""});
            raw = save(ctx, &mut row, Some(&raw))?;
            let next = propose(ctx, args, &mut row, &raw, true)?;
            output.append(next);
            return Ok(output);
        }
        return park(
            ctx,
            &mut row,
            &raw,
            if replan_used && reason == "tasks-exhausted" {
                "replan-budget-used"
            } else {
                reason
            },
            output,
        );
    }
    if terminal["status"] != "done" || terminal["reason"] != "acceptance-pass" {
        return park(ctx, &mut row, &raw, "goal-drive-result-invalid", output);
    }
    live(ctx, &row)?;
    ctx.clean()?;
    assert_scope(ctx, &row)?;
    let final_head = ctx.git(&["rev-parse", "HEAD"])?;
    let final_tree = ctx.git(&["rev-parse", "HEAD^{tree}"])?;
    let final_plan = ctx.store.read(ACTIVE, 8 * MAX_FILE)?;
    let acceptance = ctx.action(&Request::new(Action::Accept))?;
    if acceptance.code != 0 {
        output.append(acceptance);
        return park(ctx, &mut row, &raw, "final-acceptance-failed", output);
    }
    row["stage"] = "reviewing".into();
    raw = save(ctx, &mut row, Some(&raw))?;
    let plan = ctx.plan_value()?;
    let accept = value::text(&plan, "accept");
    let reviewer = value::text(&row["providers"], "reviewer").to_owned();
    let mode = value::text(&row["contract"], "review_mode").to_owned();
    if mode != "off" {
        let writers = plan["tasks"]
            .as_object()
            .unwrap()
            .values()
            .filter(|t| value::text(t, "state") == "done")
            .map(|t| value::text(t, "provider"))
            .collect::<std::collections::BTreeSet<_>>();
        if mode == "gate" && writers.contains(reviewer.as_str()) {
            return park(ctx, &mut row, &raw, "reviewer-authored-task", output);
        }
        let mut review = vec![
            "--repo".into(),
            ctx.repo.as_os_str().to_owned(),
            "--base".into(),
            value::text(&row["contract"], "base_sha").into(),
            "--providers".into(),
            reviewer.into(),
            "--gate".into(),
            "--verify".into(),
            accept.into(),
        ];
        for (option, field) in [
            ("model", "model"),
            ("fallback-model", "fallback_model"),
            ("reasoning-effort", "reasoning_effort"),
        ] {
            let text = value::text(&row["routing"]["reviewer"], field);
            if !text.is_empty() {
                review.push(format!("--{option}").into());
                review.push(text.into());
            }
        }
        let checked = invoke(
            ctx,
            "peer-review",
            review,
            verify::duration(value::text(&row["routing"]["reviewer"], "timeout"))?,
        )?;
        let failed = checked.code != 0;
        output.append(checked);
        if failed && mode == "gate" {
            return park(ctx, &mut row, &raw, "semantic-review-failed", output);
        }
        if failed {
            output
                .stderr
                .extend_from_slice(b"autopilot: semantic review: advisory fail\n");
            output.code = 0;
        }
    }
    let final_acceptance = ctx.action(&Request::new(Action::Accept))?;
    if final_acceptance.code != 0 {
        output.append(final_acceptance);
        return park(ctx, &mut row, &raw, "post-review-acceptance-failed", output);
    }
    ctx.clean()?;
    live(ctx, &row)?;
    assert_scope(ctx, &row)?;
    require(
        ctx.git(&["rev-parse", "HEAD"])? == final_head
            && ctx.git(&["rev-parse", "HEAD^{tree}"])? == final_tree
            && ctx.store.read(ACTIVE, 8 * MAX_FILE)? == final_plan,
        "final snapshot changed during semantic review",
    )?;
    if row["contract"]["draft_pr"] == true {
        row["stage"] = "publishing".into();
        raw = save(ctx, &mut row, Some(&raw))?;
        let prepared = invoke(
            ctx,
            "draft-pr",
            vec![
                "prepare".into(),
                "--repo".into(),
                ctx.repo.as_os_str().to_owned(),
                "--remote".into(),
                value::text(&row["contract"], "remote").into(),
                "--base".into(),
                base_branch.into(),
                "--verify".into(),
                accept.into(),
                "--expected-head".into(),
                final_head.clone().into(),
                "--expected-tree".into(),
                final_tree.into(),
                "--expected-base-sha".into(),
                value::text(&row["contract"], "base_sha").into(),
                "--expected-spec-sha256".into(),
                value::text(&row, "spec_sha256").into(),
            ],
            Duration::from_secs(1800),
        )?;
        if prepared.code != 0 {
            output.append(prepared);
            return park(ctx, &mut row, &raw, "draft-prepare-failed", output);
        }
        let text = String::from_utf8_lossy(&prepared.stdout);
        let intent = text
            .lines()
            .filter_map(|line| line.strip_prefix("intent: "))
            .next_back()
            .ok_or_else(|| Error::new(3, "draft-pr prepare returned no publication intent"))?
            .to_owned();
        output.append(prepared);
        ctx.clean()?;
        require(
            ctx.git(&["rev-parse", "HEAD"])? == final_head,
            "HEAD changed before publication",
        )?;
        let published = invoke(
            ctx,
            "draft-pr",
            vec![
                "publish".into(),
                "--repo".into(),
                ctx.repo.as_os_str().to_owned(),
                "--intent".into(),
                intent.into(),
            ],
            Duration::from_secs(1800),
        )?;
        if published.code != 0 {
            output.append(published);
            return park(ctx, &mut row, &raw, "draft-publish-failed", output);
        }
        output.append(published);
    }
    row["stage"] = "done".into();
    save(ctx, &mut row, Some(&raw))?;
    let _ = ctx.services.observe_journal(
        crate::services::JournalEvent {
            repo: &ctx.repo,
            source_kind: "autopilot",
            source_path: &ctx.state.join(RECEIPT),
            source_id: value::text(&row, "owner_id"),
            outcome: "done",
            verification: "passed",
        },
        ctx.interrupted,
    );
    output.append(Output::text("autopilot: done (acceptance passed)"));
    Ok(output)
}
