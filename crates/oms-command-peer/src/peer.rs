use crate::{
    Context, Error, Output, Result, V,
    cli::Args,
    fail, now, obj, out, process,
    providers::{self, Target},
    safety, storage, thread,
};
use oms_command_plan::discussion;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) const VALUES: &[&str] = &[
    "prompt",
    "prompt-file",
    "brief-file",
    "repo",
    "providers",
    "to",
    "artifact-dir",
    "model",
    "account",
    "fallback-model",
    "reasoning-effort",
    "thread",
    "topic",
    "operation",
    "print-timeout",
    "debate",
    "max-rounds",
    "base",
    "writer",
    "verify",
    "covers",
    "security-audit",
    "confirm",
    "candidate-patch",
    "candidate-sha256",
    "candidate-base",
    "synthesize",
    "role",
    "strategy",
    "session-id",
    "review-artifact",
    "workload",
    "repair",
    "scope",
    "task-id",
    "plan-task",
    "plan-binding",
    "context-pack",
    "expected-context-pack-sha256",
    "max-turns",
    "idle-timeout",
    "team-id",
    "team-spec-sha256",
    "team-member",
    "team-member-generation",
    "team-request-actions",
];
pub(crate) const FLAGS: &[&str] = &[
    "dry-run",
    "export-only",
    "repo-context",
    "diff",
    "no-diff",
    "memory",
    "no-memory",
    "task",
    "no-task",
    "ml-context",
    "no-ml-context",
    "hypothesis",
    "ml",
    "require-complete",
    "deliberation",
    "until-agreement",
    "new-thread",
    "all",
    "quiet",
    "session",
    "allow-sensitive",
    "no-strategy",
    "no-failures",
    "gate",
    "no-verify",
    "require-dispositions",
    "since-last",
    "read-only",
    "interactive",
    "allow-no-change",
    "allow-restructure",
    "allow-verifier-change",
    "allow-test-reduction",
    "keep-worktree",
    "context-manifest",
    "graph-context",
    "no-graph-context",
    "strict-brief",
    "json",
    "terminalize-resumed-repair",
    "team-requests",
    "synthesis-json",
    "output-json",
];
pub(crate) fn parse(argv: &[OsString]) -> Result<Args> {
    parse_with_flags(argv, FLAGS)
}
fn parse_with_flags(argv: &[OsString], flags: &[&str]) -> Result<Args> {
    let mut expanded = Vec::new();
    for (i, value) in argv.iter().enumerate() {
        expanded.push(value.clone());
        if value == "--synthesize"
            && argv
                .get(i + 1)
                .is_none_or(|s| s.to_string_lossy().starts_with('-'))
        {
            expanded.push("claude".into());
        }
    }
    let args = Args::parse(&expanded, VALUES, flags)?;
    let team = process::TeamLocator::from_args(&args)?;
    if args.has("team-request-actions") && !args.has("team-requests") {
        return fail("--team-request-actions requires --team-requests");
    }
    if args.has("team-request-actions") && args.get("team-request-actions").is_empty() {
        return fail("--team-request-actions must name an explicit bounded action list");
    }
    process::TeamRequestAction::parse_list(args.get("team-request-actions"))?;
    if args.has("team-requests")
        && (team.is_none()
            || args.has("fallback-model")
            || args.has("interactive")
            || args.has("export-only")
            || args.has("dry-run"))
    {
        return fail(
            "--team-requests requires an explicit Team and one read-only request transport",
        );
    }
    Ok(args)
}
pub(crate) fn run(tool: &str, argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = if tool == "peer-ask" {
        parse_with_flags(argv, &[FLAGS, &["no-current"]].concat())?
    } else {
        parse(argv)?
    };
    if args.has("no-current")
        && (!storage::identifier(args.get("thread")) || args.has("new-thread"))
    {
        return fail("--no-current requires peer-ask --thread ID on an existing thread");
    }
    if args.has("team-requests") && tool != "peer-ask" {
        return fail("Team requests are supported only by read-only peer-ask");
    }
    if args.has("synthesis-json")
        && (tool != "peer-ask"
            || !args.has("synthesize")
            || !args.has("require-complete")
            || args.has("deliberation") != args.has("debate")
            || args.has("until-agreement")
            || args.has("dry-run")
            || args.has("export-only"))
    {
        return fail(
            "--synthesis-json requires peer-ask --synthesize TARGET --require-complete, with both --debate N and --deliberation for rebuttals",
        );
    }
    if args.has("output-json")
        && (tool != "agent-call"
            || args.has("debate")
            || args.has("synthesize")
            || args.has("until-agreement")
            || args.has("dry-run")
            || args.has("export-only")
            || args.has("deliberation"))
    {
        return fail(
            "--output-json requires one completed agent-call without council or dry-run options",
        );
    }
    if args.has("help") {
        return Ok(out(help(tool).to_vec()));
    }
    if tool == "peer-delegate" {
        return crate::delegate::run(&args, context);
    }
    council(tool, &args, context)
}
pub(crate) fn help(tool: &str) -> &'static [u8] {
    match tool {
        "peer-ask" => include_bytes!("help/peer-ask.txt"),
        "peer-review" => include_bytes!("help/peer-review.txt"),
        "peer-delegate" => include_bytes!("help/peer-delegate.txt"),
        "consult" => include_bytes!("help/consult.txt"),
        "advise" => include_bytes!("help/advise.txt"),
        _ => include_bytes!("help/agent-call.txt"),
    }
}
pub(crate) fn run_id() -> Result<String> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let stamp = chrono::DateTime::from_timestamp(crate::seconds() as i64, 0)
        .ok_or_else(|| Error::new(2, "clock unavailable"))?;
    Ok(format!(
        "{}-{}{}",
        stamp.format("%Y%m%dT%H%M%SZ"),
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ))
}
pub(crate) fn question(args: &Args, context: &Context<'_>) -> Result<String> {
    args.exact_operands(1)?;
    for key in ["prompt-file", "brief-file"] {
        if args.has(key) {
            let bytes = storage::file(&args.path(key, context.cwd))?;
            if bytes.len() > 1024 * 1024 {
                return fail("brief exceeds 1 MiB");
            }
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    let prompt = args.value("prompt", args.operand(0));
    if prompt.is_empty() {
        if args.has("ml") {
            return Ok("Review this change for silent ML failures before training.".into());
        }
        return fail("a prompt is required");
    }
    Ok(prompt.into())
}
pub(crate) fn detect_caller() -> Result<Option<String>> {
    let env_nonempty = |key: &str| std::env::var_os(key).is_some_and(|v| !v.is_empty());
    if let Ok(raw) = std::env::var("OMS_AGENT")
        && !raw.trim().is_empty()
    {
        return providers::normalize(raw.trim()).map(Some);
    }
    if env_nonempty("CLAUDECODE") || env_nonempty("CLAUDE_CODE_ENTRYPOINT") {
        return Ok(Some("claude".into()));
    }
    if env_nonempty("CODEX_SANDBOX") || env_nonempty("CODEX_THREAD_ID") {
        return Ok(Some("codex".into()));
    }
    if env_nonempty("ANTIGRAVITY_CLI")
        || env_nonempty("ANTIGRAVITY_AGENT")
        || env_nonempty("AGY_SESSION")
    {
        return Ok(Some("antigravity".into()));
    }
    Ok(None)
}
fn seat_cmd(tool: &str, provider: &str) -> (&'static str, String) {
    let kind = match tool {
        "peer-ask" => "ask",
        "advise" => "advise",
        "consult" => "consult",
        "agent-call" => "call",
        _ => "review",
    };
    (kind, format!("{tool} seat {provider}"))
}
fn seat_fingerprint(cmd: &str) -> String {
    let normalized = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = cached_regex!(r"\d{8,}").replace_all(&normalized, "N");
    storage::hash(normalized.as_bytes())[..16].to_owned()
}
fn seat_unresolved(repo: &Path, cmd: &str) -> (usize, i64) {
    let Ok(raw) = storage::read(repo, Path::new(".oms/failures.jsonl"), true) else {
        return (0, 0);
    };
    let Ok(rows) = storage::rows(&raw) else {
        return (0, 0);
    };
    let fp = seat_fingerprint(cmd);
    let mut count = 0usize;
    let mut last_exit = 0i64;
    for row in &rows {
        if crate::text(row, "fingerprint") != fp {
            continue;
        }
        if crate::text(row, "event") == "resolved" {
            count = 0;
        } else if crate::text(row, "event") == "fail" {
            count += 1;
            if let V::Num(n) = row.get("exit") {
                last_exit = n.parse().unwrap_or(0);
            }
        }
    }
    (count, last_exit)
}
fn seat_resolve(repo: &Path, cmd: &str, interrupted: &std::sync::atomic::AtomicBool) -> Result<()> {
    let fp = seat_fingerprint(cmd);
    let row = obj([
        ("schema", 2usize.into()),
        ("event", "resolved".into()),
        ("ts", now().into()),
        ("agent", "agent".into()),
        ("fingerprint", fp.into()),
        ("how", "auto-resolved after healthy seat response".into()),
    ]);
    storage::append(repo, Path::new(".oms/failures.jsonl"), &row, interrupted)
}
fn unresolved_failure_summary(repo: &Path) -> String {
    let Ok(raw) = storage::read(repo, Path::new(".oms/failures.jsonl"), true) else {
        return String::new();
    };
    let Ok(rows) = storage::rows(&raw) else {
        return String::new();
    };
    let mut by_fp: std::collections::BTreeMap<String, (String, String)> =
        std::collections::BTreeMap::new();
    for row in &rows {
        let fp = crate::text(row, "fingerprint").to_owned();
        if fp.is_empty() {
            continue;
        }
        if crate::text(row, "event") == "resolved" {
            by_fp.remove(&fp);
        } else if crate::text(row, "event") == "fail" {
            by_fp.insert(
                fp,
                (
                    crate::text(row, "cmd").to_owned(),
                    crate::text(row, "summary").to_owned(),
                ),
            );
        }
    }
    by_fp
        .into_iter()
        .take(5)
        .map(|(fp, (cmd, summary))| format!("- {fp} `{cmd}`: {summary}\n"))
        .collect()
}
/// Applies `--account` to the single selected provider target.
pub(crate) fn apply_account(args: &Args, targets: &mut [Target]) -> Result<()> {
    if !args.has("account") {
        return Ok(());
    }
    let account = args.get("account");
    providers::account_id(account)?;
    let [target] = targets else {
        return fail("--account requires exactly one provider");
    };
    if !target.account.is_empty() && target.account != account {
        return fail("--account conflicts with the :account= target selector");
    }
    target.account = account.into();
    Ok(())
}
pub(crate) fn directory(args: &Args, repo: &Path, kind: &str) -> Result<PathBuf> {
    let path = if args.has("artifact-dir") {
        PathBuf::from(args.get("artifact-dir"))
    } else {
        Path::new(".oms/artifacts").join(kind)
    };
    let path = if path.is_absolute() {
        path.strip_prefix(repo)
            .map_err(|_| {
                Error::new(
                    2,
                    "artifact directory must remain inside the selected repository",
                )
            })?
            .to_owned()
    } else {
        path
    };
    storage::relative(&path)?;
    Ok(path)
}
pub(crate) fn timeout(args: &Args, tool: &str) -> Result<u64> {
    let fallback = if tool == "peer-ask" { 900 } else { 1200 };
    let raw = std::env::var("OMS_PEER_TIMEOUT").ok();
    let raw = if args.has("print-timeout") {
        Some(args.get("print-timeout").to_owned())
    } else {
        raw
    };
    let Some(raw) = raw else { return Ok(fallback) };
    let (digits, scale) = if let Some(s) = raw.strip_suffix('m') {
        (s, 60.0)
    } else if let Some(s) = raw.strip_suffix('h') {
        (s, 3600.0)
    } else {
        (raw.strip_suffix('s').unwrap_or(&raw), 1.0)
    };
    let value = digits
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0 && *v * scale <= 31_536_000.0)
        .ok_or_else(|| Error::new(2, "timeout must be a positive bounded duration"))?;
    Ok((value * scale).ceil() as u64)
}
pub(crate) const LARGE_DIFF_BYTES: usize = 16 * 1024;
pub(crate) fn diff_budget() -> usize {
    std::env::var("OMS_PROMPT_DIFF_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(65536)
}
pub(crate) fn format_duration(secs: u64) -> String {
    if secs > 0 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs > 0 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}
pub(crate) fn rounded_secs(duration: std::time::Duration) -> u64 {
    duration.as_secs() + u64::from(duration.subsec_millis() >= 500)
}
pub(crate) fn timeout_message(timeout_secs: u64, diff_bytes: usize) -> String {
    let duration = format_duration(timeout_secs);
    if diff_bytes >= LARGE_DIFF_BYTES {
        format!(
            "timed out after {duration} (diff is {diff_bytes} bytes; consider raising OMS_PEER_TIMEOUT)"
        )
    } else {
        format!("timed out after {duration}")
    }
}
pub(crate) fn is_interrupted_line(line: &str) -> bool {
    matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "interrupted"
            | "error: interrupted"
            | "operation interrupted"
            | "error: operation interrupted"
    )
}
pub(crate) fn dry(args: &Args, tool: &str) -> bool {
    let variable = match tool {
        "peer-ask" => "OH_MY_SETTING_ASK_DRY_RUN",
        "peer-review" => "OH_MY_SETTING_REVIEW_DRY_RUN",
        "peer-delegate" => "OH_MY_SETTING_DELEGATE_DRY_RUN",
        _ => "OH_MY_SETTING_CALL_DRY_RUN",
    };
    args.has("dry-run") || std::env::var_os(variable).is_some_and(|v| v == "1")
}
pub(crate) fn prompt(
    tool: &str,
    args: &Args,
    repo: &Path,
    question: &str,
    frozen_diff: Option<&[u8]>,
    context: &Context<'_>,
) -> Result<crate::request_context::Prompt> {
    let mut body = crate::request_context::Prompt::framing(if tool == "peer-review" {
        "You are an independent reviewer. Review the diff and report concrete bugs with source evidence. Do not modify files.\n"
    } else if tool == "advise" {
        "You are an adversarial decision advisor. Examine the decision, evidence, alternatives, risks and next action. Do not modify files.\n"
    } else {
        discussion::COUNCIL_INITIAL_ROLE
    });
    if tool == "peer-review" && args.has("_review-incremental-note") {
        body.untrusted(
            "history",
            "review history",
            "advisory context",
            args.get("_review-incremental-note").as_bytes(),
        )?;
    }
    body.push_str(discussion::COUNCIL_INITIAL_PRECEDENCE);
    if args.has("hypothesis") {
        body.push_str("Attack this experiment design before compute is spent: falsifiability, cheapest test, confounds, tuned baseline fairness, split/leakage, metric fit, variance and preregistered effect size.\n");
    }
    if args.has("ml") {
        body.push_str("Check silent ML bugs: leakage, splits, preprocessing fit boundary, loss, eval mode, seeds, precision, distributed sampling and checkpoint reproducibility.\n");
    }
    let role = args.value("strategy", args.get("role"));
    if !role.is_empty() && !args.has("no-strategy") {
        let (code, bytes, _) = oms_command_agent_role::run_with_source(
            &[
                "show".into(),
                "--name".into(),
                role.into(),
                "--repo".into(),
                repo.as_os_str().into(),
            ],
            repo,
            context.source,
            context.interrupted,
        );
        if code != 0 {
            return fail("role profile could not be resolved");
        }
        body.push_layer("contract", &String::from_utf8_lossy(&bytes));
        body.push('\n');
    }
    if args.has("memory") && !args.has("no-memory") {
        let (code, bytes, _) =
            oms_command_memory::provider_context(repo, question, context.interrupted);
        if code != 0 {
            return fail("shared memory context failed");
        }
        body.untrusted("retrieved", "memory", "recorded context", &bytes)?;
    }
    if args.has("task") && !args.has("no-task") {
        let bytes = storage::read(repo, Path::new(".oms/task/current.md"), true)?;
        if !bytes.is_empty() {
            body.untrusted("task", "task", "recorded context", &bytes)?;
        }
    }
    if args.has("ml-context") && !args.has("no-ml-context") {
        let digest = crate::ml_context::build(repo, context)?;
        if !digest.is_empty() {
            body.untrusted(
                "retrieved",
                "ML project",
                "context digest",
                digest.as_bytes(),
            )?;
        }
    }
    if args.has("thread") {
        body.push_layer(
            "history",
            &thread::context(repo, args.get("thread"), 6000, 12, false, true)?,
        );
    }
    if tool == "advise" {
        body.push_layer("framing", "## Decision context\n\n");
        body.push_layer("task", question);
        body.push_layer("framing", "\n\n");
        if !args.has("no-failures") {
            let summary = unresolved_failure_summary(repo);
            if !summary.is_empty() {
                body.untrusted(
                    "retrieved",
                    "fail-ledger",
                    "unresolved failures",
                    summary.as_bytes(),
                )?;
            }
        }
        if args.has("session") || args.has("session-id") {
            let digest = crate::session::advice_digest(
                repo,
                args.get("session-id"),
                args.has("allow-sensitive"),
                context,
            )?;
            safety::outbound(digest.as_bytes())?;
            body.push_str("## Caller session history\n\n");
            body.untrusted(
                "history",
                "calling session",
                "session digest",
                digest.as_bytes(),
            )?;
        }
    } else {
        body.push_layer("framing", "Question:\n");
        body.push_layer("task", question);
        body.push_layer("framing", "\n\n");
    }
    let include_diff = (tool == "peer-review" && !args.has("no-diff")) || args.has("diff");
    if include_diff || args.has("repo-context") {
        let status = process::git(context, repo, &["status", "--short"])?;
        body.push_str("Git status:\n");
        body.untrusted("tool_result", "repository", "git status", &status)?;
    } else {
        body.push_str("Repository context: omitted.\n");
    }
    if include_diff {
        let base = args.value("_review-base", args.value("base", "HEAD"));
        if base.starts_with('-') {
            return fail("invalid diff base");
        }
        let captured;
        let diff = if let Some(bytes) = frozen_diff {
            bytes
        } else {
            captured = process::git(
                context,
                repo,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-renames",
                    base,
                    "--",
                ],
            )?;
            captured.as_slice()
        };
        let budget = diff_budget();
        if args.has("gate") && diff.len() > budget {
            return fail(
                "blocking gate requires the complete diff within OMS_PROMPT_DIFF_BYTES; split the change or raise the budget",
            );
        }
        body.push_str("Diff:\n");
        if diff.len() > budget {
            body.untrusted("tool_result", "repository", "git diff", &diff[..budget])?;
            body.push_layer(
                "framing",
                &format!(
                    "\n[git diff truncated: {} of {} bytes shown]\n",
                    budget,
                    diff.len()
                ),
            );
        } else {
            body.untrusted("tool_result", "repository", "git diff", diff)?;
        }
    }
    if args.has("synthesis-json") {
        body.push_str(discussion::COUNCIL_INITIAL_SECTIONS);
    } else if args.has("gate") {
        body.push_str(crate::review::GATE_INSTRUCTION);
    } else {
        body.push_str("\nAnswer:\nTradeoffs:\nEvidence:\nRisks:\nRecommendation:\nVerification:\n");
    }
    safety::outbound(body.as_bytes())?;
    Ok(body)
}
fn validate_council_round(seats: &[Seat]) -> Result<()> {
    if seats.iter().any(|seat| seat.code == 130) {
        return Err(Error::new(
            130,
            "council interrupted; synthesis was not started",
        ));
    }
    if seats
        .iter()
        .any(|seat| !seat.complete || seat.code != 0 || seat.quality != "ok")
    {
        return fail("council rounds are incomplete; synthesis was not started");
    }
    for seat in seats {
        discussion::lint_council_response(&seat.answer, seat.round as u64, false)
            .map_err(|e| Error::new(e.code, e.message))?;
    }
    Ok(())
}

// Bound the number of disposable worktrees and supervised provider calls. Each
// seat publishes its own completed turn before the round barrier is released.
const COUNCIL_CONCURRENCY: usize = 4;

#[allow(clippy::too_many_arguments)]
fn call_round(
    tool: &str,
    args: &Args,
    repo: &Path,
    dir: &Path,
    run: &str,
    targets: &[Target],
    routes: &[providers::Route],
    body: &crate::request_context::Prompt,
    round: usize,
    warnings: &mut Vec<u8>,
    context: &Context<'_>,
) -> Result<Vec<Seat>> {
    let mut seats = Vec::with_capacity(targets.len());
    let parallelism = context
        .executor
        .council_parallelism()
        .clamp(1, COUNCIL_CONCURRENCY);
    for (targets, routes) in targets.chunks(parallelism).zip(routes.chunks(parallelism)) {
        crate::check(context.interrupted)?;
        if targets.len() == 1 {
            seats.push(call_routed(
                tool,
                args,
                repo,
                dir,
                run,
                &targets[0],
                body,
                round,
                warnings,
                context,
                routes[0].clone(),
            )?);
            continue;
        }
        let outcomes = std::thread::scope(|scope| {
            let mut workers = Vec::with_capacity(targets.len());
            let mut launch_error = None;
            for (target, route) in targets.iter().zip(routes) {
                let worker = std::thread::Builder::new().spawn_scoped(scope, move || {
                    let mut warnings = Vec::new();
                    let seat = call_routed(
                        tool,
                        args,
                        repo,
                        dir,
                        run,
                        target,
                        body,
                        round,
                        &mut warnings,
                        context,
                        route.clone(),
                    );
                    (seat, warnings)
                });
                match worker {
                    Ok(worker) => workers.push(worker),
                    Err(_) => {
                        launch_error = Some(Error::new(75, "council worker could not start"));
                        break;
                    }
                }
            }
            // Join every started call even after a sibling's error. Dropping a
            // result must never detach provider supervision or invent cleanup.
            let outcomes = workers
                .into_iter()
                .map(|worker| {
                    worker.join().unwrap_or_else(|_| {
                        (
                            Err(Error::new(
                                75,
                                "council seat outcome unknown; reconcile before retrying",
                            )),
                            Vec::new(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            (outcomes, launch_error)
        });
        let (outcomes, mut error) = outcomes;
        for (result, diagnostics) in outcomes {
            warnings.extend(diagnostics);
            match result {
                Ok(seat) => seats.push(seat),
                Err(err) if error.is_none() || err.code == 130 => error = Some(err),
                Err(_) => {}
            }
        }
        if let Some(error) = error {
            return Err(error);
        }
    }
    Ok(seats)
}

#[derive(Clone)]
pub(crate) struct Seat {
    pub target: Target,
    pub route: providers::Route,
    pub requested_model: String,
    pub artifact: PathBuf,
    pub answer: String,
    pub findings: Option<crate::review::ParsedFindings>,
    pub code: i32,
    pub complete: bool,
    pub quality: String,
    /// Review verdict read from the raw answer before redaction, so a redacted tail cannot change it.
    pub verdict: Option<(&'static str, Option<f64>)>,
    pub round: usize,
    pub elapsed: std::time::Duration,
    pub timeout_secs: u64,
    pub diff_bytes: usize,
    pub artifact_sha256: String,
    pub output_start: usize,
}
impl Seat {
    fn council_output(&self) -> Result<oms_command_plan::discussion::CouncilOutput> {
        Ok(oms_command_plan::discussion::CouncilOutput {
            provider: self.route.provider.clone(),
            requested_model: self.requested_model.clone(),
            round: self.round as u64,
            artifact: self
                .artifact
                .file_name()
                .and_then(|v| v.to_str())
                .ok_or_else(|| Error::new(2, "council artifact filename unavailable"))?
                .to_owned(),
            artifact_sha256: self.artifact_sha256.clone(),
            output_start: self.output_start,
            output_bytes: self.answer.len(),
            output_sha256: storage::hash(self.answer.as_bytes()),
            complete: self.complete,
            exit: self.code,
            quality: self.quality.clone(),
        })
    }
}
pub(crate) fn index(repo: &Path, row: &V, context: &Context<'_>) -> Result<()> {
    storage::append(
        repo,
        Path::new(".oms/artifacts/index.jsonl"),
        row,
        context.interrupted,
    )
}
fn apply_target_explicitness(route: &mut providers::Route, args: &Args) {
    if providers::panel_implicit_target() && !route.account_pinned {
        route.explicit = false;
    } else {
        route.explicit |= args.has("to") || args.has("providers");
    }
}

fn output_mode(args: &Args) -> providers::OutputMode<'static> {
    if args.has("output-json") || args.has("synthesis-json") {
        providers::OutputMode::Native
    } else {
        providers::OutputMode::Legacy
    }
}

fn call_route(
    tool: &str,
    args: &Args,
    repo: &Path,
    target: &Target,
    context: &Context<'_>,
) -> Result<providers::Route> {
    let mut route = providers::route(
        context,
        target,
        args.get("model"),
        args.value("reasoning-effort", "auto"),
        if !args.get("role").is_empty() {
            args.get("role")
        } else if tool == "peer-review" {
            "reviewer"
        } else {
            "advisor"
        },
    )?;
    apply_target_explicitness(&mut route, args);
    if !dry(args, tool) && !args.has("export-only") {
        if matches!(output_mode(args), providers::OutputMode::Native) {
            providers::require_native_output(&route.provider)?;
        }
        providers::bind_account(&mut route, repo)?;
    }
    Ok(route)
}

fn mcp_operation_id(repo: &Path, dir: &Path) -> Option<String> {
    if let Ok(rest) = dir.strip_prefix(oms_command_operations::peer_operations::RUN_ROOT) {
        let mut parts = rest.components();
        if let Some(std::path::Component::Normal(op)) = parts.next()
            && matches!(parts.next(), None | Some(std::path::Component::Normal(_)))
            && parts.next().is_none()
            && let Some(op_str) = op.to_str()
            && oms_command_operations::peer_operations::is_mcp_operation_id(op_str)
        {
            return Some(op_str.to_owned());
        }
    }
    let cancel_dir = std::env::var_os("OMS_PEER_CANCEL_DIR").map(PathBuf::from)?;
    let rel = cancel_dir.strip_prefix(repo).ok()?;
    let mut parts = rel
        .strip_prefix(oms_command_operations::peer_operations::RUN_ROOT)
        .ok()?
        .components();
    let std::path::Component::Normal(op) = parts.next()? else {
        return None;
    };
    if parts.next().is_some() {
        return None;
    }
    let op_str = op.to_str()?;
    oms_command_operations::peer_operations::is_mcp_operation_id(op_str).then(|| op_str.to_owned())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn call(
    tool: &str,
    args: &Args,
    repo: &Path,
    dir: &Path,
    run: &str,
    target: &Target,
    body: &crate::request_context::Prompt,
    round: usize,
    warnings: &mut Vec<u8>,
    context: &Context<'_>,
) -> Result<Seat> {
    let route = call_route(tool, args, repo, target, context)?;
    call_routed(
        tool, args, repo, dir, run, target, body, round, warnings, context, route,
    )
}

#[allow(clippy::too_many_arguments)]
fn call_routed(
    tool: &str,
    args: &Args,
    repo: &Path,
    dir: &Path,
    run: &str,
    target: &Target,
    body: &crate::request_context::Prompt,
    round: usize,
    warnings: &mut Vec<u8>,
    context: &Context<'_>,
    mut route: providers::Route,
) -> Result<Seat> {
    let (seat_kind, cmd_key) = seat_cmd(tool, &target.provider);
    let (unresolved_count, last_exit) = if round == 0 && tool != "_synthesis" {
        seat_unresolved(repo, &cmd_key)
    } else {
        (0, 0)
    };
    if unresolved_count > 0 {
        warnings.extend_from_slice(
            format!(
                "warning: {} {seat_kind} seat has unresolved no-answer history (count={unresolved_count}, last exit {last_exit})\n",
                target.provider
            )
            .as_bytes(),
        );
    }
    let requested_model = route.model.clone();
    let suffix = if round == 0 {
        String::new()
    } else {
        format!("-r{round}")
    };
    let artifact = if tool == "_synthesis" {
        dir.join(format!("_synthesis-{run}{suffix}.md"))
    } else {
        dir.join(format!(
            "{}-{}{}-{run}{suffix}.md",
            target.provider,
            crate::board::slug(args.value("prompt", "call")),
            if target.account.is_empty() && target.model.is_empty() {
                String::new()
            } else {
                format!(
                    "-{}",
                    &storage::hash(format!("{}:{}", target.account, target.model).as_bytes())[..12]
                )
            }
        ))
    };
    let seat_line = if unresolved_count > 0 {
        "- seat-health: unresolved no-answer history\n"
    } else {
        ""
    };
    let header = format!(
        "# OMS {tool}\n\n- provider: {}\n- model: {}\n- run: {run}\n- round: {round}\n{seat_line}\n## Prompt\n\n{body}\n",
        target.provider, route.model
    );
    let dispatch = crate::dispatch_context::DispatchContext::from_environment(args.get("task-id"));
    let panel_council = tool == "peer-ask"
        && args.has("deliberation")
        && args.has("require-complete")
        && args.has("thread")
        && dispatch.panel.is_some();
    let publication_error = |stage| {
        move |error: Error| {
            if panel_council {
                Error {
                    message: format!(
                        "council seat {} round {round}: {stage} failed; reconcile before retrying",
                        target.provider
                    ),
                    ..error
                }
            } else {
                error
            }
        }
    };
    storage::write(
        repo,
        &artifact,
        header.as_bytes().to_vec(),
        false,
        context.interrupted,
    )
    .map_err(publication_error("artifact write"))?;
    let diff_bytes = args.get("_review-diff-bytes").parse::<usize>().unwrap_or(0);
    if dry(args, tool) || args.has("export-only") {
        return Ok(Seat {
            target: target.clone(),
            route,
            artifact,
            answer: String::new(),
            findings: None,
            code: 0,
            complete: false,
            quality: "empty".into(),
            verdict: None,
            round,
            artifact_sha256: String::new(),
            output_start: 0,
            requested_model,
            elapsed: std::time::Duration::ZERO,
            timeout_secs: timeout(args, tool).unwrap_or(0),
            diff_bytes,
        });
    }
    let before = process::git(context, repo, &["diff", "--binary", "HEAD", "--"]);
    let private = if before.is_ok() {
        Some(crate::delegate::Worktree::create(repo, "HEAD", context)?)
    } else if target.provider == "antigravity" {
        return fail("Antigravity read access requires a disposable repository worktree");
    } else {
        None
    };
    if let (Some(tree), Ok(patch)) = (&private, &before)
        && !patch.is_empty()
    {
        process::git_input(
            context,
            &tree.path,
            &["apply", "--binary", "--whitespace=nowarn", "-"],
            Some(patch.clone()),
        )?;
    }
    let workdir = private.as_ref().map_or(repo, |tree| tree.path.as_path());
    let live_attempt = if repo.join(".oms").is_dir() && tool != "_synthesis" {
        let mut refs = obj([("artifact", artifact.to_string_lossy().into_owned().into())]);
        if let Some(mcp_op) = mcp_operation_id(repo, dir) {
            refs.set("mcp_operation", mcp_op);
        }
        dispatch.peer_refs(&mut refs, &route, tool);
        let req = oms_command_operations::StartRequest {
            provider: route.provider.clone(),
            tool: tool.to_owned(),
            task_id: dispatch.task_id.clone(),
            parent_attempt_id: dispatch.parent_attempt_id.clone().unwrap_or_default(),
            refs,
            ..Default::default()
        };
        oms_command_operations::create_attempt(repo, &req, context.interrupted)
            .ok()
            .and_then(|id| {
                if crate::delegate::transition(repo, &id, "starting", obj([]), context).is_ok()
                    && crate::delegate::transition(repo, &id, "working", obj([]), context).is_ok()
                {
                    Some(id)
                } else {
                    let _ = crate::delegate::transition(repo, &id, "failed", obj([]), context);
                    None
                }
            })
    } else {
        None
    };
    if args.has("team-requests") && live_attempt.is_none() {
        return fail("Team request lead lifecycle enrollment unavailable");
    }
    let mut usage_env = crate::usage::operation_env(&[])?;
    let cargo_guard = private
        .as_ref()
        .map(|tree| tree.cargo_env(&route.provider, &mut usage_env));
    if args.has("team-requests") {
        usage_env.push((
            "OMS_TEAM_REQUEST_PROTOCOL".into(),
            "oms-team-request-v1".into(),
        ));
        if args.has("team-request-actions") {
            usage_env.push((
                "OMS_TEAM_REQUEST_ACTIONS".into(),
                args.get("team-request-actions").into(),
            ));
        }
    }
    process::TeamLocator::append_args(args, &mut usage_env)?;
    if let Some(id) = &live_attempt {
        usage_env.push(("OMS_ATTEMPT_ID".into(), id.into()));
    }
    if !args.get("task-id").is_empty() {
        usage_env.push(("OMS_TASK_ID".into(), args.get("task-id").into()));
    }
    let operation_id = usage_env
        .iter()
        .rev()
        .find(|(k, _)| k == "OMS_OPERATION_ID")
        .unwrap()
        .1
        .to_string_lossy()
        .into_owned();
    let timeout_secs = timeout(args, tool)?;
    let call_started = std::time::Instant::now();
    let scope = (tool == "peer-ask"
        && round == 0
        && args.has("no-current")
        && args.get("providers") == target.provider
        && !args.has("debate")
        && matches!(target.provider.as_str(), "claude" | "antigravity"))
    .then(|| {
        crate::lead_observation::Scope::from_owner(
            repo,
            dir,
            args.get("thread"),
            &target.provider,
            context,
        )
    })
    .flatten();
    let observed = std::cell::Cell::new(None);
    let result = providers::execute_with_fallback_output(
        &mut route,
        args.get("fallback-model"),
        body,
        workdir,
        false,
        timeout_secs,
        repo,
        &usage_env,
        if scope.is_some() {
            providers::OutputMode::Observed(&observed)
        } else {
            output_mode(args)
        },
        context,
    );
    drop(cargo_guard);
    let mut result = match result {
        Ok(r) => r,
        Err(error) => crate::process::Completed {
            code: error.code,
            stdout: Vec::new(),
            stderr: error.message.into_bytes(),
            complete: false,
            cleanup_complete: None,
            owned_cleanup: None,
            elapsed: call_started.elapsed(),
        },
    };
    if let Some(id) = live_attempt {
        // Re-recorded because a capacity overflow can move the call to another account.
        let account = providers::panel_account(&route);
        let retry_transition = |state: &str| {
            let mut last_err = None;
            for attempt_idx in 0..3 {
                let refs = if account.is_empty() {
                    obj([])
                } else {
                    obj([("panel_account", account.clone().into())])
                };
                let transitioned = if args.has("team-requests") {
                    // Cleanup/terminal bookkeeping has an independent finite
                    // budget; cancellation never authorizes a new provider call.
                    let settling = std::sync::atomic::AtomicBool::new(false);
                    let finished = std::sync::atomic::AtomicBool::new(false);
                    std::thread::scope(|scope| {
                        let watchdog = scope.spawn(|| {
                            std::thread::park_timeout(std::time::Duration::from_secs(2));
                            if !finished.load(Ordering::Acquire) {
                                settling.store(true, Ordering::Release);
                            }
                        });
                        let bounded = Context {
                            cwd: context.cwd,
                            source: context.source,
                            interrupted: &settling,
                            executor: context.executor,
                        };
                        let result = crate::delegate::transition(repo, &id, state, refs, &bounded);
                        finished.store(true, Ordering::Release);
                        watchdog.thread().unpark();
                        result
                    })
                } else {
                    crate::delegate::transition(repo, &id, state, refs, context)
                };
                match transitioned {
                    Ok(()) => return Ok(()),
                    Err(err) => {
                        last_err = Some(err);
                        if attempt_idx < 2 {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    }
                }
            }
            Err(last_err.unwrap())
        };
        let outcome = if result.code == 0 && result.complete {
            retry_transition("review").and_then(|()| retry_transition("done"))
        } else {
            let to_state = if result.code == 124 {
                "timed_out"
            } else if result.code == 130 {
                "cancelled"
            } else {
                "failed"
            };
            retry_transition(to_state)
        };
        if let Err(err) = outcome {
            result.code = if result.code == 0 { 1 } else { result.code };
            result
                .stderr
                .extend_from_slice(format!("\nfailed finalizing attempt {id}: {err}\n").as_bytes());
        }
    }
    if let Some(tree) = private {
        let unchanged = tree.identity(context).is_ok()
            && process::git(context, &tree.path, &["diff", "--binary", "HEAD", "--"])
                .ok()
                .as_ref()
                == before.as_ref().ok()
            && process::git(
                context,
                &tree.path,
                &["ls-files", "--others", "--exclude-standard", "-z"],
            )?
            .is_empty();
        if !unchanged {
            result.code = 125;
            result.complete = false;
            result.stderr.extend_from_slice(
                b"read-only provider changed its isolated worktree; retained for inspection\n",
            );
        } else if tree.remove(context).is_err() {
            result.code = 125;
            result.complete = false;
            result.stderr.extend_from_slice(
                b"read worktree cleanup failed; reconcile before repeating the call\n",
            );
        }
    }
    let answer = String::from_utf8_lossy(&safety::redact(&result.stdout)?).into_owned();
    let findings = if tool == "peer-review" {
        Some(crate::review::seat_findings(&String::from_utf8_lossy(
            &result.stdout,
        )))
    } else {
        None
    };
    let quality = if result.complete {
        providers::answer_quality(&answer).to_owned()
    } else {
        "truncated".into()
    };
    let timed_out = !result.complete && result.code == 124;
    let interrupted_seat = !result.complete && result.code == 130;
    let mut diagnostics = Vec::new();
    if timed_out {
        diagnostics.extend_from_slice(
            format!("error: {}\n", timeout_message(timeout_secs, diff_bytes)).as_bytes(),
        );
        // Strip bare interruption lines emitted when a timed-out child handles SIGTERM/SIGINT.
        let redacted = safety::redact(&result.stderr)?;
        for line in String::from_utf8_lossy(&redacted).lines() {
            if line.trim().is_empty() || is_interrupted_line(line) {
                continue;
            }
            diagnostics.extend_from_slice(line.as_bytes());
            diagnostics.push(b'\n');
        }
    } else if interrupted_seat {
        diagnostics.extend_from_slice(b"error: interrupted\n");
        let redacted = safety::redact(&result.stderr)?;
        for line in String::from_utf8_lossy(&redacted).lines() {
            if line.trim().is_empty() || is_interrupted_line(line) {
                continue;
            }
            diagnostics.extend_from_slice(line.as_bytes());
            diagnostics.push(b'\n');
        }
    } else if !result.stderr.is_empty() {
        diagnostics.extend_from_slice(&safety::redact(&result.stderr)?);
    }
    // Only a completed answer can lose its verdict to redaction; timeouts keep the diagnostics path.
    let verdict = (tool == "peer-review" && result.complete).then(|| {
        crate::review::verdict(
            &String::from_utf8_lossy(&result.stdout),
            Some(result.code),
            result.complete,
        )
    });
    // The verdict goes right under the title, before any provider-controlled text.
    let title = header.find("\n\n").map_or(0, |at| at + 2);
    let mut bytes = header.as_bytes()[..title].to_vec();
    if let Some((state, confidence)) = verdict {
        bytes.extend_from_slice(format!("- verdict: {state}\n").as_bytes());
        if let Some(confidence) = confidence {
            bytes.extend_from_slice(format!("- confidence: {confidence}\n").as_bytes());
        }
    }
    bytes.extend_from_slice(&header.as_bytes()[title..]);
    bytes.extend_from_slice(b"\n## Output\n\n");
    let output_start = bytes.len();
    bytes.extend_from_slice(answer.as_bytes());
    bytes.push(b'\n');
    if !diagnostics.is_empty() {
        bytes.extend_from_slice(b"\n## Diagnostics\n\n");
        bytes.extend_from_slice(&diagnostics);
    }
    if result.complete {
        bytes.extend_from_slice(format!("\n## Exit\n\n{}\n", result.code).as_bytes());
        if result.code == 0 && unresolved_count > 0 {
            seat_resolve(repo, &cmd_key, context.interrupted)?;
        }
    }
    let artifact_sha256 = storage::hash(&bytes);
    storage::write(repo, &artifact, bytes, true, context.interrupted)
        .map_err(publication_error("artifact write"))?;
    index(
        repo,
        &obj([
            ("schema", 1usize.into()),
            ("event_id", crate::id("evt_")?.into()),
            ("ts", now().into()),
            ("artifact_id", crate::id("art_")?.into()),
            ("kind", tool.trim_start_matches("peer-").into()),
            ("run", run.into()),
            ("operation_id", operation_id.into()),
            ("provider", target.provider.clone().into()),
            ("model", route.model.clone().into()),
            (
                "model_family",
                providers::family(&route.provider, &route.model).into(),
            ),
            ("artifact", artifact.to_string_lossy().into_owned().into()),
            ("exit", V::Num(result.code.to_string())),
            ("quality", quality.clone().into()),
        ]),
        context,
    )
    .map_err(publication_error("artifact index write"))?;
    if args.has("thread") {
        let role = if quality == "ok" { "answer" } else { "note" };
        let quoted = if quality == "ok" {
            answer.clone()
        } else {
            format!(
                "{} returned {quality}; see {}",
                route.provider,
                artifact.display()
            )
        };
        thread::append(
            repo,
            args.get("thread"),
            role,
            &quoted,
            &[
                ("provider", route.provider.clone().into()),
                ("model", route.model.clone().into()),
                ("artifact", artifact.to_string_lossy().into_owned().into()),
                ("quality", quality.clone().into()),
            ],
            context,
        )
        .map_err(publication_error("thread append"))?;
    }
    if result.code == 0
        && result.complete
        && quality == "ok"
        && let (Some(scope), Some(observation)) = (scope, observed.take())
    {
        scope.publish(&observation, &artifact, &artifact_sha256);
    }
    Ok(Seat {
        target: target.clone(),
        route,
        artifact,
        answer,
        findings,
        code: result.code,
        complete: result.complete,
        quality,
        verdict,
        round,
        artifact_sha256,
        output_start,
        requested_model,
        elapsed: result.elapsed,
        timeout_secs,
        diff_bytes,
    })
}
pub(crate) fn council(tool: &str, args: &Args, context: &Context<'_>) -> Result<Output> {
    safety::parent()?;
    let mut resolved = args.clone();
    if tool == "advise" && !args.has("strategy") && !args.has("role") && !args.has("no-strategy") {
        resolved.set_value("strategy", "decision-advisor");
    }
    if tool == "consult" && !args.has("no-task") {
        resolved.set_flag("task");
    }
    if tool == "consult" && !dry(args, tool) && !args.has("export-only") && !args.has("thread") {
        let repo = storage::repository(context, &args.path("repo", context.cwd))?;
        let ident = if args.has("new-thread") {
            None
        } else {
            thread::current(&repo)
        }
        .unwrap_or(crate::id("th-")?);
        resolved.set_value("thread", ident);
    }
    let args = &resolved;
    let synthesis_json = args.has("synthesis-json");
    if synthesis_json && dry(args, tool) {
        return fail("--synthesis-json requires completed calls, not dry-run artifacts");
    }
    if args.has("output-json") && dry(args, tool) {
        return fail("--output-json requires completed output, not a dry-run artifact");
    }
    if args.has("interactive") {
        return fail("--interactive belongs to peer-delegate");
    }
    if args.has("allow-sensitive") && !args.has("session") && !args.has("session-id") {
        return fail("--allow-sensitive requires --session or --session-id");
    }
    let question = question(args, context)?;
    let repo = storage::repository(context, &args.path("repo", context.cwd))?;
    let kind = match tool {
        "agent-call" => "call",
        "peer-ask" => "ask",
        "peer-review" => "review",
        other => other,
    };
    let dir = directory(args, &repo, kind)?;
    let single = matches!(tool, "agent-call" | "advise" | "consult");
    let caller = detect_caller()?.unwrap_or_default();
    let default = if tool == "advise" {
        std::env::var("OMS_ADVISOR_PROVIDER")
            .ok()
            .filter(|s| !s.is_empty())
    } else if tool == "consult" {
        std::env::var("OMS_CONSULT_PROVIDER")
            .ok()
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let mut warnings = Vec::new();
    let mut targets = if single {
        let explicit = args.all("to");
        if args.has("all") && !explicit.is_empty() {
            return fail("--all and --to are mutually exclusive");
        }
        if !explicit.is_empty() {
            explicit
                .into_iter()
                .map(|s| Target::parse(&s))
                .collect::<Result<Vec<_>>>()?
        } else if let Some(default) = default {
            vec![Target::parse(&default)?]
        } else if tool == "agent-call" {
            return fail("--to is required");
        } else {
            let peers = ["codex", "claude", "antigravity"]
                .iter()
                .filter(|p| **p != caller)
                .filter(|p| providers::discovered(p))
                .take(if args.has("all") { 3 } else { 1 })
                .map(|p| Target::parse(p))
                .collect::<Result<Vec<_>>>()?;
            if peers.is_empty() && !caller.is_empty() && providers::discovered(&caller) {
                warnings.extend_from_slice(
                    b"note: no independent provider available; answering from the caller's own family (self-advice)\n",
                );
                vec![Target::parse(&caller)?]
            } else {
                peers
            }
        }
    } else {
        args.value("providers", "codex,claude,antigravity")
            .split(',')
            .map(str::trim)
            .map(Target::parse)
            .collect::<Result<Vec<_>>>()?
    };
    if tool == "peer-review" && args.has("writer") && !args.has("providers") {
        let writer = providers::normalize(args.get("writer"))?;
        targets.retain(|s| s.provider != writer);
    }
    if tool == "peer-review" && args.has("writer") && args.has("providers") {
        let writer = providers::normalize(args.required("writer")?)?;
        let writer_family = providers::family(&writer, "");
        let mut correlated = false;
        for target in &targets {
            let route = providers::route(
                context,
                target,
                args.get("model"),
                args.value("reasoning-effort", "auto"),
                "reviewer",
            )?;
            correlated |= providers::family(&route.provider, &route.model) == writer_family;
        }
        if correlated {
            warnings.extend_from_slice(b"warning: explicit reviewer council includes the writer's model family; correlated judgment is not independent\n");
        }
    }
    if args.has("team-requests")
        && (targets.len() != 1
            || !args.has("providers")
            || args.has("debate")
            || args.has("until-agreement")
            || args.has("synthesize")
            || args.has("deliberation")
            || args.has("max-rounds")
            || args.has("all"))
    {
        return fail(
            "Team request channel requires exactly one explicit --providers target and one round",
        );
    }
    apply_account(args, &mut targets)?;
    if targets.is_empty() {
        return fail("no available peer target");
    }
    if targets.iter().all(|t| t.optional) {
        return fail("at least one seat must be required");
    }
    if args.has("output-json") && (targets.len() != 1 || targets[0].optional) {
        return fail("--output-json requires exactly one required provider");
    }
    if synthesis_json && targets.iter().any(|t| t.optional) {
        return fail("--synthesis-json requires every declared council seat");
    }
    if synthesis_json
        && targets
            .iter()
            .map(|target| &target.provider)
            .collect::<BTreeSet<_>>()
            .len()
            != targets.len()
    {
        return fail("--synthesis-json requires one seat per provider");
    }
    let mut seen = BTreeSet::new();
    if targets
        .iter()
        .any(|t| !seen.insert(format!("{}:{}:{}", t.provider, t.model, t.account)))
    {
        return fail("duplicate council seat");
    }
    if (args.has("model") || args.has("fallback-model")) && targets.len() != 1 {
        return fail("model overrides require exactly one provider");
    }
    if matches!(tool, "consult" | "advise")
        && (args.has("model") || args.has("fallback-model"))
        && (!args.has("to") || !targets[0].model.is_empty())
    {
        return fail("exact models require one plain explicit --to provider");
    }
    if args.has("debate") && args.has("until-agreement") {
        return fail("--debate and --until-agreement are mutually exclusive");
    }
    let rounds = if args.has("until-agreement") {
        args.limit("max-rounds", 4, 1, 6)?.saturating_sub(1)
    } else if args.has("debate") {
        args.limit("debate", 0, 1, 3)?
    } else {
        0
    };
    if args.has("max-rounds") && !args.has("until-agreement") {
        return fail("--max-rounds requires --until-agreement");
    }
    if args.has("deliberation") && rounds == 0 {
        return fail("--deliberation requires --debate");
    }
    let mut synthesis_route = None;
    let mut synthesis_options = args.clone();
    synthesis_options.remove("model");
    synthesis_options.remove("fallback-model");
    if synthesis_json {
        let target = Target::parse(args.required("synthesize")?)?;
        if target.optional || target.model.is_empty() || target.model == "provider-default" {
            return fail("--synthesis-json requires an exact synthesis target model");
        }
        let mut route = call_route("_synthesis", &synthesis_options, &repo, &target, context)?;
        // The account selected during preflight must serve the same exact route
        // when synthesis starts after the rounds.
        route.account_pinned |= !route.account.is_empty();
        synthesis_route = Some((target, route));
    }
    let mut routes = Vec::with_capacity(targets.len());
    if !dry(args, tool) && !args.has("export-only") {
        let mut valid = Vec::new();
        for target in targets {
            let checked = call_route(tool, args, &repo, &target, context);
            match checked {
                Ok(route) => {
                    valid.push(target);
                    routes.push(route);
                }
                Err(error) if target.optional => warnings.extend_from_slice(
                    format!(
                        "warning: optional {} seat unavailable: {}\n",
                        target.provider, error.message
                    )
                    .as_bytes(),
                ),
                Err(error) => return Err(error),
            }
        }
        targets = valid;
    } else {
        routes = targets
            .iter()
            .map(|target| call_route(tool, args, &repo, target, context))
            .collect::<Result<Vec<_>>>()?;
    }
    if args.has("thread") && !dry(args, tool) && !args.has("export-only") {
        // Background callers already seeded their own explicit live thread.
        // append requires that exact log to exist and checks closure under its lock.
        if !args.has("no-current") && thread::current(&repo).as_deref() != Some(args.get("thread"))
        {
            thread::create(&repo, args.get("thread"), args.get("topic"), context)?;
        }
        thread::append(
            &repo,
            args.get("thread"),
            "question",
            &question,
            &[],
            context,
        )?;
    }
    let run = run_id()?;
    let captured = if tool == "peer-review" {
        crate::review::pin_base(&mut resolved, &repo, context)?;
        let scoped = crate::review::scope(&resolved, &repo, context)?;
        let diff_bytes = if resolved.has("no-diff") {
            0
        } else {
            scoped.1.len().min(diff_budget())
        };
        resolved.set_value("_review-diff-bytes", diff_bytes.to_string());
        Some(scoped)
    } else {
        None
    };
    let args = &resolved;
    let body = prompt(
        tool,
        args,
        &repo,
        &question,
        captured.as_ref().map(|(_, patch)| patch.as_slice()),
        context,
    )?;
    let mut output = args.get("_review-incremental-note").to_owned();
    if tool == "consult" && args.has("thread") {
        output.push_str(&format!("{}\n", args.get("thread")));
    }
    if tool == "peer-review" && !dry(args, tool) && !args.has("export-only") {
        let expected = &captured.as_ref().unwrap().0;
        let actual = crate::review::scope(args, &repo, context)?.0;
        if expected.get("diff_sha256") != actual.get("diff_sha256")
            || expected.get("head") != actual.get("head")
        {
            return fail("review source changed while composing prompts; no reviewer started");
        }
        crate::review::prepare(expected, &repo, &dir, &run, context)?;
    }
    let mut seats = call_round(
        tool,
        args,
        &repo,
        &dir,
        &run,
        &targets,
        &routes,
        &body,
        0,
        &mut warnings,
        context,
    )?;
    if args.has("output-json") {
        let result = &seats[0];
        if result.code == 130 {
            return Err(Error::new(130, "native output interrupted"));
        }
        if !result.complete || result.code != 0 || result.quality != "ok" {
            return fail("native call did not complete with usable output");
        }
        let raw = oms_command_plan::discussion::encode_agent_output(
            &question,
            &run,
            &result.council_output()?,
        )
        .map_err(|e| Error::new(e.code, e.message))?;
        return Ok((0, raw, warnings));
    }
    let mut council_history = if synthesis_json {
        validate_council_round(&seats)?;
        seats.clone()
    } else {
        Vec::new()
    };
    if !dry(args, tool) && !args.has("export-only") {
        for round in 1..=rounds {
            if seats
                .iter()
                .any(|s| !s.complete || s.code != 0 || s.quality != "ok")
            {
                break;
            }
            if args.has("until-agreement") && agree(&seats) {
                break;
            }
            let budget = if synthesis_json {
                discussion::COUNCIL_ROUND_BYTES
            } else {
                std::env::var("OMS_DEBATE_ROUND_BYTES")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(65536)
                    .clamp(1024, 1024 * 1024)
            } / seats.len();
            let mut debate = format!("Question:\n{question}{}", discussion::COUNCIL_REBUTTAL_TASK);
            for seat in &seats {
                if synthesis_json {
                    debate.push_str(&discussion::council_peer_answer(
                        &seat.target.provider,
                        &seat.answer,
                        budget,
                        &crate::id("")?,
                    ));
                } else {
                    debate.push_str(&safety::untrusted(
                        &seat.target.provider,
                        "peer answer",
                        safety::truncate(&seat.answer, budget).as_bytes(),
                    )?);
                }
            }
            if synthesis_json {
                debate.push_str(discussion::COUNCIL_REBUTTAL_SECTIONS);
                safety::outbound(debate.as_bytes())?;
            } else {
                debate.push_str("\nAnswer:\nEvidence:\nRisks:\nRecommendation:\nVerification:\nChanged from previous round:\nRemaining disagreements:\n");
            }
            if args.has("gate") {
                debate.push_str(crate::review::GATE_INSTRUCTION);
            }
            let debate = crate::request_context::Prompt::unknown(debate);
            let next = call_round(
                tool,
                args,
                &repo,
                &dir,
                &run,
                &targets,
                &routes,
                &debate,
                round,
                &mut warnings,
                context,
            )?;
            let stable = next.iter().all(|s| {
                s.answer
                    .to_lowercase()
                    .contains("changed from previous round:\nnone")
            });
            seats = next;
            if synthesis_json {
                validate_council_round(&seats)?;
                council_history.extend(seats.iter().cloned());
            }
            if stable && !synthesis_json {
                break;
            }
        }
    }
    if dry(args, tool) || args.has("export-only") {
        for seat in &seats {
            output.push_str(&format!(
                "dry-run: {} -> {}\n",
                seat.target.provider,
                repo.join(&seat.artifact).display()
            ));
        }
        return Ok((0, output.into_bytes(), warnings));
    }
    for seat in &seats {
        output.push_str(&format!("{}\n", repo.join(&seat.artifact).display()));
        if !args.has("quiet") && !seat.answer.is_empty() {
            output.push_str(&format!("\n{}:\n{}\n", seat.target.provider, seat.answer));
        }
    }
    if args.has("synthesize") {
        if synthesis_json && council_history.len() != targets.len() * (rounds + 1) {
            return fail("council rounds are incomplete; synthesis was not started");
        }
        let mut synthesis = format!("{}{question}\n", discussion::COUNCIL_SYNTHESIS_PREFIX);
        for seat in &seats {
            if synthesis_json {
                synthesis.push_str(&discussion::council_peer_answer(
                    &seat.target.provider,
                    &seat.answer,
                    discussion::COUNCIL_SYNTHESIS_BYTES,
                    &crate::id("")?,
                ));
            } else {
                synthesis.push_str(&safety::untrusted(
                    &seat.target.provider,
                    "peer answer",
                    safety::truncate(&seat.answer, 16384).as_bytes(),
                )?);
            }
        }
        if synthesis_json {
            synthesis.push_str(discussion::COUNCIL_SYNTHESIS_SECTIONS);
            safety::outbound(synthesis.as_bytes())?;
        }
        let synthesis = crate::request_context::Prompt::unknown(synthesis);
        let result = if let Some((target, route)) = synthesis_route {
            call_routed(
                "_synthesis",
                &synthesis_options,
                &repo,
                &dir,
                &run,
                &target,
                &synthesis,
                0,
                &mut warnings,
                context,
                route,
            )?
        } else {
            let target = Target::parse(args.get("synthesize"))?;
            call(
                "_synthesis",
                &synthesis_options,
                &repo,
                &dir,
                &run,
                &target,
                &synthesis,
                0,
                &mut warnings,
                context,
            )?
        };
        if synthesis_json {
            if result.code == 130 {
                return Err(Error::new(130, "council synthesis interrupted"));
            }
            if !result.complete || result.code != 0 || result.quality != "ok" {
                return fail("council synthesis did not complete with usable output");
            }
            oms_command_plan::discussion::lint_council_response(&result.answer, 0, true)
                .map_err(|e| Error::new(e.code, e.message))?;
            let participants = council_history
                .iter()
                .map(Seat::council_output)
                .collect::<Result<Vec<_>>>()?;
            let raw = oms_command_plan::discussion::encode_council_synthesis(
                &question,
                &run,
                rounds as u64,
                &participants,
                &result.council_output()?,
            )
            .map_err(|e| Error::new(e.code, e.message))?;
            return Ok((0, raw, warnings));
        }
        output.push_str(&format!(
            "synthesis: {}\n{}\n",
            result.artifact.display(),
            result.answer
        ));
    }
    if tool == "peer-review" {
        let mut result = crate::review::finish(args, &repo, &dir, &run, &seats, output, context)?;
        result.2.extend_from_slice(&warnings);
        return Ok(result);
    }
    let usable = seats
        .iter()
        .filter(|s| s.complete && s.code == 0 && s.quality == "ok")
        .count();
    let families = seats
        .iter()
        .filter(|s| s.complete && s.code == 0 && s.quality == "ok")
        .map(|s| providers::family(&s.route.provider, &s.route.model))
        .collect::<BTreeSet<_>>()
        .len();
    output.push_str(&format!(
        "{tool}: {usable}/{} target(s) answered, {families} independent model family(ies)\n",
        seats.len()
    ));
    if tool == "agent-call" {
        // Native references follow all untrusted prose. Existing autonomy
        // consumers select the last artifact line, never a provider's claim.
        for seat in &seats {
            output.push_str(&format!(
                "artifact: {}\n",
                repo.join(&seat.artifact).display()
            ));
        }
    }
    let complete = seats
        .iter()
        .all(|s| s.target.optional || s.complete && s.code == 0 && s.quality == "ok");
    Ok((
        i32::from(
            usable == 0 || (args.has("require-complete") || args.has("deliberation")) && !complete,
        ),
        output.into_bytes(),
        warnings,
    ))
}
fn agree(seats: &[Seat]) -> bool {
    if seats.len() < 2 {
        return false;
    }
    let keys = seats
        .iter()
        .map(|s| {
            s.answer
                .lines()
                .find(|l| {
                    ["Verdict:", "Recommendation:", "Answer:"]
                        .iter()
                        .any(|p| l.starts_with(p))
                        && l.split_once(':').is_some_and(|(_, v)| !v.trim().is_empty())
                })
                .map(|s| s.to_lowercase())
        })
        .collect::<Vec<_>>();
    keys.first().is_some_and(Option::is_some) && keys.windows(2).all(|p| p[0] == p[1])
}

#[cfg(test)]
mod tests {
    use super::mcp_operation_id;
    use std::path::Path;

    #[test]
    fn mcp_operation_id_links_downstream_consultation_artifact_dirs() {
        let repo = Path::new("/tmp/oms-mcp-op-repo");
        assert_eq!(
            mcp_operation_id(
                repo,
                Path::new(".oms/artifacts/mcp/consult-20261125T120000Z-aabbccdd/answers")
            ),
            Some("consult-20261125T120000Z-aabbccdd".to_owned())
        );
        assert_eq!(
            mcp_operation_id(
                repo,
                Path::new(".oms/artifacts/mcp/ask-20261125T120000Z-11223344/answers")
            ),
            Some("ask-20261125T120000Z-11223344".to_owned())
        );
        assert_eq!(
            mcp_operation_id(repo, Path::new(".oms/artifacts/consult")),
            None
        );
        assert_eq!(
            mcp_operation_id(repo, Path::new(".oms/artifacts/mcp/not_an_mcp_id/answers")),
            None
        );
    }
}
