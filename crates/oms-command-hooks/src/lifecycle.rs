use crate::{Output, common::*};
use serde_json::{Value, json};
use std::{collections::BTreeMap, ffi::OsString, fs, path::Path};

pub fn run(name: &str, args: &[OsString], raw: &[u8], ctx: &Context<'_>) -> Output {
    if child() && name != "fail-ledger-hook" {
        return empty();
    }
    let Some(v) = document(raw) else {
        return empty();
    };
    match name {
        "telemetry-hook" => {
            telemetry(&v, ctx);
            empty()
        }
        "fail-ledger-hook" => failure(&v, ctx),
        "precompact-handoff" => {
            precompact(&v, args, raw, ctx);
            empty()
        }
        "resume-hook" => resume(&v, ctx),
        "turn-guard" => stop(&v, ctx),
        _ => empty(),
    }
}
pub(crate) fn telemetry(v: &Value, ctx: &Context<'_>) {
    if event(v) == "Notification" {
        if v["notification_type"] != "permission_prompt"
            && !(panel() && v["notification_type"] == "elicitation_dialog")
        {
            return;
        }
        let Some(repo) = repo(v, ctx).filter(|r| adopted(r)) else {
            return;
        };
        activity(v, &repo, ctx);
        if v["notification_type"] == "permission_prompt" {
            event_row(
                ctx,
                &repo,
                v,
                json!({"action":"permission_prompt","notification_type":"permission_prompt"}),
            );
        }
        return;
    }
    let mut containers = vec![v];
    for k in [
        "usage",
        "token_usage",
        "tokenUsage",
        "model_usage",
        "modelUsage",
        "metrics",
    ] {
        if v[k].is_object() {
            containers.push(&v[k]);
        }
    }
    let response = if v["tool_response"].is_object() {
        &v["tool_response"]
    } else {
        &v["toolResponse"]
    };
    if response["usage"].is_object() {
        containers.push(&response["usage"]);
    }
    let mut fields = json!({"action":"telemetry"});
    for (names, key, limit) in [
        (&["tool_name", "toolName"][..], "tool_name", 80),
        (&["model", "model_name", "modelName"][..], "model", 160),
        (
            &["subagent_type", "subagentType", "agent_type"][..],
            "subagent_type",
            80,
        ),
    ] {
        let value = names.iter().find_map(|k| v.get(*k)).unwrap_or(&Value::Null);
        let name = bounded(value, limit);
        if !name.is_empty() {
            fields[key] = json!(name);
        }
    }
    let metrics: [(&[&str], &str); 7] = [
        (&["duration_ms", "durationMs"], "duration_ms"),
        (
            &["input_tokens", "inputTokens", "prompt_tokens"],
            "input_tokens",
        ),
        (
            &["output_tokens", "outputTokens", "completion_tokens"],
            "output_tokens",
        ),
        (
            &[
                "cache_read_input_tokens",
                "cache_read_tokens",
                "cacheReadTokens",
            ],
            "cache_read_tokens",
        ),
        (
            &[
                "cache_creation_input_tokens",
                "cache_creation_tokens",
                "cacheCreationTokens",
            ],
            "cache_creation_tokens",
        ),
        (
            &[
                "reasoning_tokens",
                "reasoning_output_tokens",
                "reasoningTokens",
            ],
            "reasoning_tokens",
        ),
        (&["cost_usd", "total_cost_usd", "costUsd"], "cost_usd"),
    ];
    let mut any = false;
    for (names, key) in metrics {
        let value = containers
            .iter()
            .flat_map(|c| names.iter().map(move |n| &c[*n]))
            .chain(if key == "duration_ms" {
                names.iter().map(|n| &response[*n]).collect::<Vec<_>>()
            } else {
                vec![]
            })
            .find(|n| n.as_f64().is_some_and(|n| n.is_finite() && n >= 0.));
        if let Some(n) = value {
            let n = n.as_f64().unwrap();
            fields[key] = if n.fract() == 0. {
                json!(n as u64)
            } else {
                json!((n * 1000.).round_ties_even() / 1000.)
            };
            any = true;
        }
    }
    if let Some(b) = v["success"]
        .as_bool()
        .or_else(|| response["success"].as_bool())
    {
        fields["success"] = json!(b);
    }
    for (names, key) in [
        (&["agent_id", "agentId"][..], "agent_id_hash"),
        (
            &["parent_agent_id", "parentAgentId"][..],
            "parent_agent_id_hash",
        ),
    ] {
        if let Some(value) = names.iter().find_map(|k| v.get(*k)) {
            let s = if let Some(s) = value.as_str() {
                s.to_owned()
            } else if value.is_i64() || value.is_u64() {
                value.to_string()
            } else {
                String::new()
            };
            if !s.is_empty() {
                fields[key] = json!(&hash(s)[..16]);
            }
        }
    }
    let emit = ["SessionStart", "SessionEnd"].contains(&event(v))
        || any
        || fields["success"] == false
        || env("OMS_TELEMETRY_DEBUG") == "1";
    if !emit && !panel() {
        return;
    }
    let Some(repo) = repo(v, ctx).filter(|r| adopted(r)) else {
        return;
    };
    activity(v, &repo, ctx);
    if emit {
        event_row(ctx, &repo, v, fields);
    }
}
pub(crate) fn activity(v: &Value, repo: &Path, ctx: &Context<'_>) {
    if ctx.snapshot.activity.get().is_some()
        || !panel()
        || !["UserPromptSubmit", "PostToolUse", "Stop", "Notification"].contains(&event(v))
    {
        return;
    }
    let who = env("OMS_ROOM_PARTICIPANT");
    let attempt = env("OMS_PANEL_MAIN_ATTEMPT");
    let room = env("OMS_ROOM_ID");
    if ![&who, &attempt, &room].iter().all(|s| identifier(s, 160)) {
        return;
    }
    let kind = if event(v) == "Notification" {
        match text(v, "notification_type") {
            "permission_prompt" => Some("approval"),
            "elicitation_dialog" => Some("input"),
            _ => return,
        }
    } else {
        None
    };
    let path = repo.join(format!(".oms/hooks/panel-activity/{who}.json"));
    let updated = mutate(&path, 4096, false, |raw| {
        let old = document(raw).unwrap_or_else(|| json!({}));
        let now = ctx.snapshot.seconds();
        let same = text(&old, "attempt") == attempt && text(&old, "room") == room;
        if event(v) == "PostToolUse"
            && same
            && old["state"] == "busy"
            && old["updated_at"]
                .as_f64()
                .is_some_and(|t| t >= 0. && t <= now && now - t < 10.)
        {
            return Ok((raw.to_vec(), ()));
        }
        let mut origin = if same {
            text(&old, "origin").to_owned()
        } else {
            String::new()
        };
        if event(v) == "UserPromptSubmit" {
            origin = if agent(v, ctx) == "claude" && !same || automated_prompt(text(v, "prompt")) {
                "harness"
            } else {
                "user"
            }
            .into();
        }
        let state = if kind.is_some() {
            "waiting"
        } else if event(v) == "Stop" {
            "idle"
        } else {
            "busy"
        };
        let since = if same && text(&old, "state") == state && event(v) == "PostToolUse" {
            old["since"]
                .as_f64()
                .filter(|t| *t >= 0. && *t <= now)
                .unwrap_or(now)
        } else {
            now
        };
        let mut row = json!({"schema":1,"participant":who,"attempt":attempt,"room":room,"state":state,"since":since,"updated_at":now});
        if ["user", "harness"].contains(&origin.as_str()) {
            row["origin"] = json!(origin);
        }
        let transcript = first(v, &["transcript_path", "transcriptPath"]);
        if !transcript.is_empty() && transcript.len() <= 4096 {
            row["transcript_path"] = json!(transcript);
        }
        if let Some(kind) = kind {
            row["kind"] = json!(kind);
        }
        Ok((json_line(&row), ()))
    });
    if updated.is_ok() {
        let _ = ctx.snapshot.activity.set(());
    }
}
pub(crate) fn ledger(args: &[&str], repo: &Path, ctx: &Context<'_>) -> Output {
    let mut argv = vec![OsString::from("--repo"), repo.as_os_str().into()];
    argv.extend(args.iter().map(OsString::from));
    oms_command_state::run_with_source("fail-ledger", &argv, repo, ctx.source, ctx.interrupted)
}
fn failure(v: &Value, ctx: &Context<'_>) -> Output {
    if off("OMS_FAIL_LEDGER_HOOK") || v["tool_name"] != "Bash" {
        return empty();
    }
    let raw = text(&v["tool_input"], "command");
    if raw.trim().is_empty() {
        return empty();
    }
    let command = raw
        .replace('\n', " ")
        .chars()
        .take(2000)
        .collect::<String>();
    let command = normalize_machine(&command);
    let Some(repo) = repo(v, ctx).filter(|r| adopted(r)) else {
        return empty();
    };
    activity(v, &repo, ctx);
    usage(v, &command, &repo, ctx);
    if event(v) == "PostToolUse" {
        if off("OMS_FAIL_LEDGER_RESOLVE")
            || v["tool_response"]["interrupted"] == true
            || v["tool_response"]
                .get("backgroundTaskId")
                .is_some_and(|v| !v.is_null() && v != false && v != "")
            || v["tool_input"]["run_in_background"] == true
        {
            return empty();
        }
        let path = env_path("OMS_FAIL_LEDGER").unwrap_or_else(|| repo.join(".oms/failures.jsonl"));
        if !path.metadata().is_ok_and(|m| m.len() > 0) {
            return empty();
        }
        let mut normalized = command.replace(home().to_string_lossy().as_ref(), "~");
        normalized = cached_regex!(r"/(?:hom[e]|User[s])/[A-Za-z0-9._-]+")
            .replace_all(&normalized, "~")
            .into_owned();
        normalized = cached_regex!(r"\d{8,}")
            .replace_all(
                &normalized.split_whitespace().collect::<Vec<_>>().join(" "),
                "N",
            )
            .into_owned();
        let fp = hash(normalized)[..16].to_owned();
        let mut fails = 0;
        for row in read(&path, 64 * INPUT_MAX)
            .unwrap_or_default()
            .split(|b| *b == b'\n')
            .filter_map(document)
        {
            if text(&row, "fingerprint") != fp {
                continue;
            }
            match text(&row, "event") {
                "resolved" => fails = 0,
                "fail" => fails += 1,
                _ => {}
            }
        }
        if fails > 0 {
            let _ = ledger(
                &[
                    "resolve",
                    "--cmd",
                    &command,
                    "--how",
                    "same command passed (exit 0)",
                ],
                &repo,
                ctx,
            );
        }
        return empty();
    }
    if v["is_interrupt"] == true {
        return empty();
    }
    let exit = if event(v) == "PostToolUseFailure" {
        cached_regex!(r"^Exit code (\d+)")
            .captures(text(v, "error"))
            .and_then(|c| c[1].parse::<u64>().ok())
            .unwrap_or(1)
    } else {
        1
    };
    if [0, 130, 141, 143].contains(&exit) || cached_regex!(r"/tmp/claude-\d+/").is_match(&command) {
        return empty();
    }
    let prior = ledger(&["check", "--cmd", &command], &repo, ctx);
    let recorded = ledger(
        &[
            "record",
            "--cmd",
            &command,
            "--exit",
            &exit.to_string(),
            "--kind",
            "hook",
        ],
        &repo,
        ctx,
    );
    let mut lines = Vec::new();
    if prior.0 != 0 {
        for line in String::from_utf8_lossy(&[prior.1, prior.2].concat()).lines() {
            if line.starts_with("fail-ledger:") && !lines.iter().any(|s| s == line) {
                lines.push(line.to_owned());
            }
        }
    }
    for line in String::from_utf8_lossy(&[recorded.1, recorded.2].concat()).lines() {
        if line.starts_with("fail-ledger:")
            && line.contains("oms advise")
            && !lines.iter().any(|s| s == line)
        {
            lines.push(line.to_owned());
        }
    }
    if lines.is_empty() {
        empty()
    } else {
        (2, vec![], format!("{}\n", lines.join("\n")).into_bytes())
    }
}
/// Canonical default; `config/usage-families.json` under the source may override it.
pub(crate) const EMBEDDED_USAGE_FAMILIES: &str = include_str!("usage-families.json");
fn usage(_v: &Value, command: &str, repo: &Path, ctx: &Context<'_>) {
    if env("OMS_USAGE_TRACK").is_empty() || off("OMS_USAGE_TRACK") {
        return;
    }
    let first = command.split_whitespace().next().unwrap_or("");
    let name = Path::new(first)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    if [
        "grep", "egrep", "fgrep", "rg", "sed", "awk", "cat", "head", "tail", "less", "wc", "find",
        "echo", "printf", "ls", "stat", "file", "diff",
    ]
    .contains(&name.as_ref())
    {
        return;
    }
    let override_path = ctx.source.join("config/usage-families.json");
    let configured;
    let families = if override_path.is_file() {
        configured = compile_usage(&load(&override_path));
        &configured
    } else {
        static FAMILIES: std::sync::OnceLock<Vec<(String, regex::Regex)>> =
            std::sync::OnceLock::new();
        FAMILIES.get_or_init(|| {
            compile_usage(
                &serde_json::from_str(EMBEDDED_USAGE_FAMILIES).unwrap_or_else(|_| json!({})),
            )
        })
    };
    let day = chrono::DateTime::<chrono::Utc>::from(ctx.snapshot.time())
        .format("%Y-%m-%d")
        .to_string();
    let command = command.to_lowercase();
    for (name, pattern) in families {
        if pattern.is_match(&command) {
            let _ = append(
                &repo.join(".oms/usage.jsonl"),
                &json!({"schema":1,"family":name,"day":day}),
                64 * INPUT_MAX,
            );
        }
    }
}
fn compile_usage(value: &Value) -> Vec<(String, regex::Regex)> {
    value["families"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, spec)| {
            let pattern = text(spec, "pattern");
            if pattern.is_empty() {
                return None;
            }
            regex::Regex::new(pattern).ok().map(|re| (name.clone(), re))
        })
        .collect()
}

pub(crate) fn capture(
    v: &Value,
    repo: &Path,
    agent: &str,
    note: &str,
    skip: bool,
    ctx: &Context<'_>,
) -> bool {
    capture_with_callback(v, repo, agent, note, skip, ctx, None)
}

fn capture_with_callback(
    v: &Value,
    repo: &Path,
    agent: &str,
    note: &str,
    skip: bool,
    ctx: &Context<'_>,
    codex_callback: Option<&[u8]>,
) -> bool {
    if interrupted(ctx) || !adopted(repo) {
        return false;
    }
    match crate::handoff::capture(v, repo, agent, note, skip, ctx, codex_callback) {
        Ok(captured) => captured,
        Err(summary) => {
            let summary = safe_text(&summary, 160);
            let cmd = format!("session-handoff capture --agent {agent}");
            let next = format!("run manually: oms session-handoff capture --agent {agent}");
            ctx.host.record_hook_failure(repo, &cmd, 1, &summary, &next);
            let _ = ledger(
                &[
                    "record",
                    "--kind",
                    "hook",
                    "--cmd",
                    &cmd,
                    "--exit",
                    "1",
                    "--summary",
                    &summary,
                    "--next",
                    &next,
                ],
                repo,
                ctx,
            );
            false
        }
    }
}

fn precompact(v: &Value, args: &[OsString], raw: &[u8], ctx: &Context<'_>) {
    if !on("OMS_PRECOMPACT_HANDOFF") || event(v) == "SessionEnd" && !on("OMS_SESSIONEND_HANDOFF") {
        return;
    }
    let Some(repo) = root(payload_cwd(v, ctx.cwd), ctx.cwd).filter(|r| adopted(r)) else {
        return;
    };
    let agent = args.first().and_then(|s| s.to_str()).unwrap_or("claude");
    let sid = session(v);
    let slug = sid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(24)
        .collect::<String>()
        .trim_end_matches('-')
        .to_owned();
    if !slug.is_empty() {
        for entry in fs::read_dir(repo.join(".oms/handoffs"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|m| m.is_file())
                && entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|a| a.as_secs() < 900)
            {
                let matched = name.starts_with(&format!("{agent}-{slug}-"))
                    || read(&entry.path(), 1024 * 1024).ok().is_some_and(|r| {
                        String::from_utf8_lossy(&r)
                            .lines()
                            .take(16)
                            .any(|l| l == format!("- session: {sid}"))
                    });
                if matched {
                    return;
                }
            }
        }
    }
    let note = if event(v) == "SessionEnd" {
        "auto: session end"
    } else {
        "auto: pre-compact snapshot"
    };
    let codex_callback = (ctx.provider == Some("codex") && agent == "codex").then_some(raw);
    let _ = capture_with_callback(v, &repo, agent, note, true, ctx, codex_callback);
}
pub(crate) fn journal(repo: &Path, prompt: bool, ctx: &Context<'_>) -> Vec<u8> {
    if off("OMS_WORK_JOURNAL") || env("OMS_WORK_JOURNAL_ACTIVE") == "1" || !adopted(repo) {
        return vec![];
    }
    let digest = prompt && !off("OMS_WORK_JOURNAL_DIGEST");
    let distill = prompt && !off("OMS_JOURNAL_AUTODISTILL");
    let signature = format!(
        "tick --repo {} --local-only{}{}",
        repo.display(),
        if digest { " --digest" } else { "" },
        if distill { " --autodistill" } else { "" }
    );
    let directory = repo.join(".oms/work-journal");
    let cached = || -> Option<bool> {
        let raw = read(&directory.join("prompt-tick"), 16384).ok()?;
        let text = std::str::from_utf8(&raw).ok()?;
        let lines = text.lines().collect::<Vec<_>>();
        if lines.len() != 6
            || lines[0].parse::<f64>().ok()? <= ctx.snapshot.seconds()
            || head(repo).as_deref() != Some(lines[1])
            || prompt && lines[5] != signature
        {
            return Some(false);
        }
        for (i, name) in ["events.jsonl", "digest.json", "distill.json"]
            .iter()
            .enumerate()
        {
            let size = directory
                .join(name)
                .metadata()
                .ok()
                .map(|m| m.len().to_string())
                .unwrap_or_default();
            if size != lines[i + 2] {
                return Some(false);
            }
        }
        Some(true)
    };
    if cached() == Some(true) {
        return vec![];
    }
    let result = (|| -> Result<String, String> {
        let _guard = lock(&directory.join("state"), repo)?;
        if cached() == Some(true) {
            return Ok(String::new());
        }
        let store = oms_command_journal::store::JournalStore::open_for_write(repo, None)?;
        let now = chrono::DateTime::<chrono::Utc>::from(ctx.snapshot.time());
        store.capture_head_commit(now)?;
        store.materialize()?;
        let mut output = String::new();
        if digest && let Some(text) = store.prompt_digest(now)? {
            output.push_str(&text);
            output.push('\n');
        }
        if distill && store.distill_due_today(now)? {
            let append =
                |repo: &Path, batch: &str, proposals: &[oms_command_journal::MemoryProposal]| {
                    let notes: Vec<_> = proposals
                        .iter()
                        .map(|p| oms_command_memory::JournalProposal {
                            key: &p.key,
                            source_sha256: &p.source_sha256,
                            body: &p.lesson,
                            source_current: p.source_current,
                        })
                        .collect();
                    let result = oms_command_memory::append_journal_batch(
                        repo,
                        batch,
                        &notes,
                        ctx.interrupted,
                    )?;
                    Ok(oms_command_journal::MemoryBatchReport {
                        outcomes: proposals
                            .iter()
                            .zip(result.outcomes)
                            .map(|(p, result)| {
                                let outcome = match result {
                                    oms_command_memory::JournalPublication::Appended(_) => {
                                        oms_command_journal::MemoryBatchOutcome::Appended
                                    }
                                    oms_command_memory::JournalPublication::AlreadyPresent(_) => {
                                        oms_command_journal::MemoryBatchOutcome::AlreadyPresent
                                    }
                                    oms_command_memory::JournalPublication::Refused(reason) => {
                                        oms_command_journal::MemoryBatchOutcome::Refused(reason)
                                    }
                                };
                                (p.key.clone(), outcome)
                            })
                            .collect(),
                        warnings: String::from_utf8_lossy(&result.warnings)
                            .lines()
                            .map(|line| line.strip_prefix("warning: ").unwrap_or(line).to_owned())
                            .collect(),
                    })
                };
            match store.distill_batch(false, Some(&append), now) {
                Ok(report) => output.push_str(&report.prompt_summary()),
                Err(_) => output.push_str("[work-journal] distill unavailable; inspect `oms journal status` before retrying.\n"),
            }
        }
        store.write_prompt_stamp(&signature, distill, now)?;
        Ok(output)
    })();
    result.unwrap_or_default().into_bytes()
}
pub(crate) fn stop(v: &Value, ctx: &Context<'_>) -> Output {
    let Some(repo) = repo(v, ctx) else {
        return empty();
    };
    activity(v, &repo, ctx);
    if env("OMS_TURN_GUARD_OFF") != "1"
        && (cap("OMS_SESSION_BUDGET_TURNS", 0, 0, u64::MAX) > 0
            || cap("OMS_SESSION_BUDGET_HOURS", 0, 0, u64::MAX) > 0)
    {
        match budget(v, &repo, ctx) {
            Ok(Some(reason)) => {
                let raw = json_line(&json!({"decision":"block","reason":reason}));
                injection(ctx, &repo, v, "turn-guard", &raw);
                return out(raw);
            }
            Err(_) => {
                if let Some(failure) = ctx.snapshot.dispatch_failure.get() {
                    failure.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                finish_stop(v, &repo, ctx);
                return out(json_line(
                    &json!({"systemMessage":"oh-my-setting turn guard: unavailable (helper error); this turn was not checked."}),
                ));
            }
            _ => {}
        }
    }
    finish_stop(v, &repo, ctx);
    empty()
}
fn finish_stop(v: &Value, repo: &Path, ctx: &Context<'_>) {
    relay(v, repo, ctx);
    let _ = journal(repo, false, ctx);
    defer_finish(repo, ctx);
}
fn journal_config_path() -> std::path::PathBuf {
    if let Some(p) = env_path("OMS_WORK_JOURNAL_CONFIG") {
        return p;
    }
    env_path("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home().join(".config"))
        .join("oh-my-setting/work-journal.json")
}
fn journal_sync_configured() -> bool {
    !env("OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID").is_empty()
        || !env("OMS_WORK_JOURNAL_NOTION_DATABASE_ID").is_empty()
        || journal_config_path().is_file()
}
fn defer_finish(repo: &Path, ctx: &Context<'_>) {
    if !ctx.host.supports_detach()
        || child()
        || env("OMS_WORK_JOURNAL_ACTIVE") == "1"
        || !adopted(repo)
    {
        return;
    }
    let do_journal = !off("OMS_WORK_JOURNAL")
        && env("OMS_WORK_JOURNAL_SUPPRESS") != "1"
        && journal_sync_configured();
    let mut do_ci = false;
    let gh_raw = env("OMS_GH_BIN");
    let gh_bin = if gh_raw.is_empty() {
        "gh"
    } else {
        gh_raw.as_str()
    };
    if enabled("OMS_CI_TICK") && executable(std::ffi::OsStr::new(gh_bin)).is_some() {
        let remote = git(repo, &["remote", "get-url", "origin"], ctx).unwrap_or_default();
        if remote.contains("github.com:") || remote.contains("github.com/") {
            do_ci = true;
        }
    }
    let current_head = if do_ci { head(repo) } else { None };
    let ledger = env_path("OMS_CI_LEDGER").unwrap_or_else(|| repo.join(".oms/ci.jsonl"));
    defer_finish_with(
        repo,
        do_journal,
        do_ci,
        current_head.as_deref(),
        &ledger,
        ctx,
    );
}
fn defer_finish_with(
    repo: &Path,
    do_journal: bool,
    mut do_ci: bool,
    current_head: Option<&str>,
    ci_ledger: &Path,
    ctx: &Context<'_>,
) {
    if !ctx.host.supports_detach() {
        return;
    }
    if do_ci
        && let Some(current_head) = current_head
        && let Ok(raw) = read(ci_ledger, 64 * INPUT_MAX)
    {
        let mut latest: Option<Value> = None;
        for line in raw.split(|b| *b == b'\n') {
            if let Some(row) = document(line) {
                latest = Some(row);
            }
        }
        if let Some(row) = latest
            && text(&row, "sha") == current_head
            && text(&row, "status") == "completed"
        {
            do_ci = false;
        }
    }
    if do_journal {
        let _ = ctx.host.detach_command(
            "journal",
            &[
                OsString::from("sync"),
                OsString::from("--repo"),
                repo.as_os_str().to_owned(),
                OsString::from("--force"),
                OsString::from("--today"),
            ],
            repo,
        );
    }
    if do_ci {
        let _ = ctx
            .host
            .detach_command("ci-status", &[OsString::from("tick")], repo);
    }
}
fn budget(v: &Value, repo: &Path, ctx: &Context<'_>) -> Result<Option<String>, String> {
    let path = state_path(repo, v, "");
    if !path.exists() {
        return Ok(None);
    }
    let turns_cap = cap("OMS_SESSION_BUDGET_TURNS", 0, 0, u64::MAX);
    let hours_cap = cap("OMS_SESSION_BUDGET_HOURS", 0, 0, u64::MAX);
    let now_secs = ctx.snapshot.seconds();
    let result = mutate(&path, STATE_MAX, false, |raw| {
        let mut state = document(raw).ok_or("invalid budget state")?;
        let began = epoch(text(&state, "started_at")).unwrap_or(now_secs);
        if epoch(text(&state, "started_at")).is_none() {
            state["started_at"] = json!(ctx.snapshot.now());
        }
        let turns = state["budget_turns"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(1);
        let hours = (now_secs - began) / 3600.;
        let band = (4.
            * (if turns_cap > 0 {
                turns as f64 / turns_cap as f64
            } else {
                0.
            })
            .max(if hours_cap > 0 {
                hours / hours_cap as f64
            } else {
                0.
            })) as u64;
        let latched = state["budget_band"].as_u64().unwrap_or(0);
        let continuing = v["stop_hook_active"] == true || v["stopHookActive"] == true;
        let block = band >= 4 && band > latched && !continuing;
        state["budget_turns"] = json!(turns);
        state["updated_at"] = json!(ctx.snapshot.now());
        if block {
            state["budget_band"] = json!(band);
        }
        Ok((json_line(&state), (block, turns, hours)))
    })?;
    let (block, turns, hours) = result;
    if !block {
        return Ok(None);
    }
    let captured = on("OMS_CTX_CAPTURE")
        && capture(
            v,
            repo,
            "claude",
            &format!("auto: session budget ({turns} turns, {hours:.1}h)"),
            false,
            ctx,
        );
    event_row(
        ctx,
        repo,
        v,
        json!({"action":"session_budget","status":"block","turns":turns,"hours":(hours*10.).round_ties_even()/10.,"capture":if captured{"started"}else{"skipped"}}),
    );
    Ok(Some(format!(
        "[oms] session budget: {turns} turns and {hours:.1}h since this session started (budget {turns_cap} turns / {hours_cap}h); a handoff digest capture {}. Land what is verified, summarize the state for the user, and stop; continue only on an explicit go-ahead.",
        if captured {
            "was started"
        } else {
            "was skipped"
        }
    )))
}
fn claude_session_project(v: &Value, ctx: &Context<'_>) -> Option<std::path::PathBuf> {
    let mut project = env("CLAUDE_PROJECT_DIR");
    if project.is_empty() {
        let transcript = first(v, &["transcript_path", "transcriptPath"]);
        if !transcript.is_empty()
            && let Ok(file) = fs::File::open(transcript)
        {
            use std::io::{BufRead, BufReader, Read};
            let reader = BufReader::new(file.take(1024 * 1024));
            for line in reader.lines().take(50).map_while(Result::ok) {
                if let Some(row) = document(line.as_bytes()) {
                    let cwd = text(&row, "cwd");
                    if !cwd.is_empty() {
                        project = cwd.to_owned();
                        break;
                    }
                }
            }
        }
    }
    (!project.is_empty())
        .then(|| root(Path::new(&project), ctx.cwd))
        .flatten()
}
pub(crate) fn pending_background(v: &Value, max_age_secs: f64, now: f64) -> usize {
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{BufRead, BufReader, Read};
    let transcript = first(v, &["transcript_path", "transcriptPath"]);
    if transcript.is_empty() {
        return 0;
    }
    let Ok(file) = fs::File::open(transcript) else {
        return 0;
    };
    let launch_re = cached_regex!(
        r"^(?:Command running in background with ID: |Monitor started \(task )([A-Za-z0-9_-]+)",
    );
    let end_re =
        cached_regex!(r"(?s)<task-id>([A-Za-z0-9_-]+)</task-id>.*?<status>([a-z_]+)</status>");
    let mut launched: BTreeMap<String, f64> = BTreeMap::new();
    let mut ended: BTreeSet<String> = BTreeSet::new();
    let reader = BufReader::new(file.take(16 * 1024 * 1024));
    for line in reader.lines().map_while(Result::ok) {
        if !(line.contains("background with ID")
            || line.contains("Monitor started")
            || line.contains("<task-notification>")
            || line.contains("TaskStop"))
        {
            continue;
        }
        let Some(row) = document(line.as_bytes()) else {
            continue;
        };
        let Some(content_val) = row.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let items: Vec<Value> = match content_val {
            Value::String(s) => vec![json!({"type": "text", "text": s})],
            Value::Array(arr) => arr.clone(),
            _ => continue,
        };
        for item in items {
            let Some(obj) = item.as_object() else {
                continue;
            };
            if obj.get("type").and_then(Value::as_str) == Some("tool_use")
                && obj.get("name").and_then(Value::as_str) == Some("TaskStop")
            {
                if let Some(tid) = obj
                    .get("input")
                    .and_then(|i| i.get("task_id"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    ended.insert(tid.to_owned());
                }
                continue;
            }
            let raw_text = if obj.get("type").and_then(Value::as_str) == Some("tool_result") {
                obj.get("content")
            } else {
                obj.get("text")
            };
            let extracted = match raw_text {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<String>(),
                _ => String::new(),
            };
            let text = extracted.trim_start();
            if let Some(cap) = launch_re.captures(text) {
                if let Some(at) = epoch(self::text(&row, "timestamp"))
                    && let Some(tid) = cap.get(1)
                {
                    launched.insert(tid.as_str().to_owned(), at);
                }
            } else if text.starts_with("<task-notification>") {
                for cap in end_re.captures_iter(text) {
                    if let (Some(tid), Some(status)) = (cap.get(1), cap.get(2))
                        && status.as_str() != "running"
                    {
                        ended.insert(tid.as_str().to_owned());
                    }
                }
            }
        }
    }
    launched
        .into_iter()
        .filter(|(tid, at)| !ended.contains(tid) && (now - *at) <= max_age_secs)
        .count()
}
fn start_codex_notify(target: &Path, v: &Value, message: &str, ctx: &Context<'_>) {
    if env("OMS_PANEL_RESULTS") == "1" || env("OMS_CODEX_NOTIFY") == "0" {
        return;
    }
    let turn_state = crate::notify::turn_state_path(&session_hash(v));
    let socket_exists = crate::notify::socket_path().exists();
    start_codex_notify_with(target, v, message, &turn_state, socket_exists, ctx);
}
fn start_codex_notify_with(
    target: &Path,
    v: &Value,
    message: &str,
    turn_state: &Path,
    socket_exists: bool,
    ctx: &Context<'_>,
) {
    if !socket_exists || !ctx.host.supports_detach() {
        return;
    }
    let state = load(turn_state);
    let Some(started) = state["prompt_at"].as_f64() else {
        return;
    };
    let elapsed = ctx.snapshot.seconds() - started;
    if elapsed < 30.0 || pending_background(v, 86400.0, ctx.snapshot.seconds()) > 0 {
        return;
    }
    let elapsed_secs = elapsed as u64;
    let minutes = elapsed_secs / 60;
    let duration = if minutes > 0 {
        format!("{minutes}분")
    } else {
        format!("{elapsed_secs}초")
    };
    let tail: String = message
        .chars()
        .skip(message.chars().count().saturating_sub(2000))
        .collect();
    let dir_name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let body = format!(
        "🔔 Claude Code 작업 완료 · {dir_name} ({duration})\n{}",
        safe_text(&tail, 300)
    );
    let _ = ctx.host.detach_command(
        "codex-app-notify",
        &[
            OsString::from("send"),
            target.as_os_str().to_owned(),
            OsString::from(body),
        ],
        target,
    );
}
pub(crate) fn relay(v: &Value, repo: &Path, ctx: &Context<'_>) {
    if child()
        || v["stop_hook_active"] == true
        || v["stopHookActive"] == true
        || agent(v, ctx) != "claude"
    {
        return;
    }
    let message = text(v, "last_assistant_message").trim();
    if message.is_empty() {
        return;
    }
    let project = claude_session_project(v, ctx);
    start_codex_notify(project.as_deref().unwrap_or(repo), v, message, ctx);
    if !on("OMS_RELAY") || !adopted(repo) {
        return;
    }
    let tail = message
        .chars()
        .skip(message.chars().count().saturating_sub(4000))
        .collect::<String>();
    let row = json!({"schema":1,"agent":"claude","session":session_hash(v),"ended_at":ctx.snapshot.now(),"head":git(repo,&["rev-parse","--short","HEAD"],ctx).unwrap_or_default(),"branch":git(repo,&["branch","--show-current"],ctx).unwrap_or_default(),"message":safe_text(&tail,cap("OMS_RELAY_BYTES",800,120,4000)as usize)});
    let _ = write(&repo.join(".oms/hooks/relay/claude.json"), &row);
}
const RESUME_VERIFY_INLINE_MAX: usize = 160;
const RESUME_ACTIONABLE_MAX: usize = 5;

fn format_actionable(ids: &[&str]) -> String {
    if ids.len() <= RESUME_ACTIONABLE_MAX {
        format!("{} ({})", ids.len(), ids.join(", "))
    } else {
        format!(
            "{} ({} +{} more)",
            ids.len(),
            ids[..RESUME_ACTIONABLE_MAX].join(", "),
            ids.len() - RESUME_ACTIONABLE_MAX
        )
    }
}

pub(crate) fn resume(v: &Value, ctx: &Context<'_>) -> Output {
    if !on("OMS_RESUME_HOOK") {
        return empty();
    }
    let Some(repo) = root(payload_cwd(v, ctx.cwd), ctx.cwd).filter(|r| adopted(r)) else {
        return empty();
    };
    let mut lines = Vec::new();
    let mut current_plan_goal = String::new();
    let mut plan_unfinished = false;
    let plan_path = repo.join(".oms/plan/tasks.json");
    if plan_path.is_file() {
        let plan = load(&plan_path);
        let plan_goal = text(&plan, "goal").trim();
        if !plan_goal.is_empty() {
            let accept = flat(text(&plan, "accept"));
            if let Ok(plan_state) = oms_command_state::plan_status(&repo, ctx.interrupted)
                && plan_state.object().is_ok()
            {
                current_plan_goal = oms_contract_markdown::bounded_line(plan_goal, 1000);
                plan_unfinished = matches!(
                    plan_state.get("has_unfinished"),
                    oms_command_state::Value::Bool(true)
                );
                if !matches!(
                    plan_state.get("all_closed"),
                    oms_command_state::Value::Bool(true)
                ) {
                    let mut counts = BTreeMap::<String, u64>::new();
                    if let Ok(by_state) = plan_state.get("by_state").object() {
                        for (state, count) in by_state {
                            if let Ok(state) = std::str::from_utf8(state)
                                && let Ok(n) = count.int()
                                && let Ok(n) = u64::try_from(n)
                            {
                                counts.insert(state.to_owned(), n);
                            }
                        }
                    }
                    let summary = counts
                        .iter()
                        .map(|(s, n)| format!("{s}={n}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    lines.push(format!(
                        "- plan: {}{}",
                        clip(&flat(plan_goal), 160),
                        if summary.is_empty() {
                            String::new()
                        } else {
                            format!(" [{summary}]")
                        }
                    ));
                    let contract = plan_state.get("contract");
                    if matches!(contract.get("bound"), oms_command_state::Value::Bool(true))
                        && !matches!(
                            contract.get("satisfied"),
                            oms_command_state::Value::Bool(true)
                        )
                    {
                        let blocker = contract.text("blocker");
                        if blocker.is_empty() {
                            lines.push("  contract: blocked".into());
                        } else {
                            lines.push(format!("  contract: blocked ({blocker})"));
                        }
                    }
                    let actionable_ids: Vec<&str> = plan_state
                        .get("actionable")
                        .array()
                        .map(|rows| rows.iter().filter_map(|row| row.str().ok()).collect())
                        .unwrap_or_default();
                    if !actionable_ids.is_empty() {
                        lines.push(format!(
                            "  actionable: {}",
                            format_actionable(&actionable_ids)
                        ));
                    }
                    if !accept.is_empty() {
                        lines.push(format!("  accept: {}", clip(&accept, 140)));
                    }
                }
            } else {
                lines.push(format!(
                    "- plan: {} [status unavailable]",
                    clip(&flat(plan_goal), 160)
                ));
                if !accept.is_empty() {
                    lines.push(format!("  accept: {}", clip(&accept, 140)));
                }
            }
        }
    }
    let task = read(&repo.join(".oms/task/current.md"), 1024 * 1024).unwrap_or_default();
    let meta = |key: &str| {
        oms_command_task::packet::metadata(&task, key)
            .map(|s| String::from_utf8_lossy(s).trim().to_owned())
            .unwrap_or_default()
    };
    let task_id = meta("task_id");
    let status = if task.is_empty() {
        String::new()
    } else {
        String::from_utf8_lossy(oms_command_task::packet::effective_status(&task)).into_owned()
    };
    let section = |key: &str| {
        String::from_utf8_lossy(&oms_command_task::packet::section(&task, key))
            .lines()
            .find(|s| !s.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let goal = section("Goal");
    let now = chrono::DateTime::<chrono::Utc>::from(ctx.snapshot.time());
    let freshness = oms_command_task::packet::freshness_from_env(&task, now);
    if !task_id.is_empty() && status != "closed" {
        let days = freshness.idle_days().unwrap_or(0);
        let goal_text = if goal.is_empty() {
            "no goal recorded"
        } else {
            &goal
        };
        if let Some(label) = freshness.label() {
            lines.push(format!("- task {task_id} [{label}]: {goal_text}"));
            if let Some(hint) = freshness.hint() {
                lines.push(format!("  hint: {hint}"));
            }
        } else {
            lines.push(format!(
                "- task {task_id} ({status}{}): {goal_text}",
                if days >= 1 {
                    format!(", idle {days}d")
                } else {
                    String::new()
                },
            ));
            let next = section("Next Step");
            if !next.is_empty() {
                lines.push(format!("  next: {next}"));
            }
            let verify_section = oms_command_task::packet::section(&task, "Verify");
            let verify_raw = String::from_utf8_lossy(&verify_section);
            let verify_trimmed = verify_raw.trim();
            if !verify_trimmed.is_empty() {
                let verify_chars = verify_trimmed.chars().count();
                if verify_chars > RESUME_VERIFY_INLINE_MAX {
                    lines.push(format!(
                        "  verify: omitted ({verify_chars} chars; run `oms agent-task show`)"
                    ));
                } else {
                    let verify = section("Verify");
                    lines.push(format!("  verify: {verify} (oms agent-task verify)"));
                }
            }
        }
    }
    let land = oms_command_landing::run_with_source(
        "land",
        &[
            "status".into(),
            "--repo".into(),
            repo.as_os_str().into(),
            "--json".into(),
            "--active-exit".into(),
        ],
        &repo,
        ctx.source,
        ctx.interrupted,
    );
    if [0, 3].contains(&land.0) && document(&land.1).is_some_and(|v| v["active"] == true) {
        lines.push("- land lock held (possibly another worktree); oms land status".into());
    }
    let handoff = handoff_line(
        &repo,
        if status != "closed" { &task_id } else { "" },
        &task,
        v,
        ctx,
    );
    if let Some((line, _)) = &handoff {
        lines.push(line.clone());
    }
    let unpublished = unpublished(&repo, ctx);
    if !unpublished.is_empty() {
        lines.push(unpublished);
    }
    let failures = ledger(&["list", "--unresolved", "--json"], &repo, ctx);
    if failures.0 == 0
        && let Some(failures) = document(&failures.1)
    {
        let rows = failures["failures"].as_array().cloned().unwrap_or_default();
        let actionable = rows
            .iter()
            .filter(|v| v["actionable"] == true)
            .collect::<Vec<_>>();
        if let Some(latest) = actionable.iter().max_by_key(|v| text(v, "ts")) {
            let mut s = format!("- failures: {} actionable", actionable.len());
            let summary = first(latest, &["summary", "cmd"]);
            if !summary.is_empty() {
                s.push_str(&format!("; latest: {}", clip(summary, 120)));
            }
            let fp = text(latest, "fingerprint");
            if !fp.is_empty() {
                s.push_str(&format!(
                    "; fp: {fp} (oms fail-ledger show --fingerprint {fp})"
                ));
            }
            if !text(latest, "next").is_empty() {
                s.push_str(&format!("; next: {}", clip(text(latest, "next"), 120)));
            }
            lines.push(s);
        } else if !rows.is_empty() {
            let retiring = rows.iter().filter(|v| v["retiring"] == true).count();
            let stale = rows.iter().filter(|v| v["attention"] == "stale").count();
            lines.push(format!("- failures: {retiring} one-shot hook failure(s), auto-retire on TTL; {stale} stale on an older commit (oms fail-ledger list --unresolved)"));
        }
    }
    if repo.join(".oms/plan/autopilot-run.json").is_file() {
        let mut args = vec!["--repo".into(), repo.as_os_str().into(), "shadow".into()];
        if !session(v).is_empty() {
            args.extend(["--session".into(), session(v).into()]);
        }
        let r = oms_command_autonomy::run("autopilot", &args, &repo, ctx.interrupted);
        if r.0 == 0 && !r.1.is_empty() {
            lines.push(format!(
                "- autopilot: {}",
                String::from_utf8_lossy(&r.1).trim_end()
            ));
        }
    }
    let update = oms_command_lifecycle::run_with_source(
        "auto-update",
        &["attention".into()],
        ctx.cwd,
        ctx.source,
        ctx.interrupted,
    );
    if update.0 == 0 {
        let line = String::from_utf8_lossy(&update.1);
        let line = line.trim();
        if !line.is_empty()
            && !line.starts_with("attention: ok")
            && !line.starts_with("attention: disabled")
        {
            lines.push(format!(
                "- auto-update {}",
                line.trim_start_matches("attention: ")
            ));
        }
    }
    let relay = crate::routing::relay_hint(v, &repo, ctx);
    if !relay.is_empty() {
        lines.push(relay);
    }
    if panel() && ["startup", "compact", "clear", "resume"].contains(&text(v, "source")) {
        let _ = write(
            &state_path(&repo, v, ".resume"),
            &json!({"schema":1,"updated_at":ctx.snapshot.now(),"source":text(v,"source")}),
        );
        let _ = std::fs::remove_file(state_path(&repo, v, ".hint"));
    }
    if text(v, "source") == "compact" && !lines.is_empty() {
        lines.push("- after compaction: the lines above were re-read from .oms state, not from the summary; OMS skills are invoked by name (/oms-<skill>), and oms state shows the rest".into());
    }
    if env("OMS_RESUME_MEMORY") == "1" {
        let task_goal = if oms_command_task::packet::current_goal_eligible(&task, freshness) {
            let (sections, _) =
                oms_contract_markdown::sections(std::str::from_utf8(&task).unwrap_or(""));
            oms_contract_markdown::task_goal(&sections)
        } else {
            String::new()
        };
        let project = read(&repo.join("PROJECT.md"), 1024 * 1024).unwrap_or_default();
        let (sections, _) =
            oms_contract_markdown::sections(std::str::from_utf8(&project).unwrap_or(""));
        let project_goal = oms_contract_markdown::project_goal(&sections);
        let (objective, _) = oms_work_inspect::current_objective(
            &task_goal,
            &current_plan_goal,
            plan_unfinished,
            &project_goal,
        );
        let result = oms_command_memory::resume_context(&repo, objective, ctx.interrupted);
        if result.0 == 0 && !result.1.is_empty() {
            lines.push(format!(
                "- memory (reference only; not approval or verification evidence):\n{}",
                safe_text(&String::from_utf8_lossy(&result.1), 1200)
            ));
        }
    }
    if lines.is_empty() {
        return empty();
    }
    let mut body = format!(
        "[oms resume] {}\n{}\n",
        repo.file_name().unwrap_or_default().to_string_lossy(),
        lines.join("\n")
    )
    .replace('\r', "");
    let mut footer = "- more: oms state\n".to_owned();
    if body.len() + footer.len() > 4096 {
        footer = format!("\n[resume truncated; inspect current state]\n{footer}");
        body = clip(&body, 4096 - footer.len());
    }
    body.push_str(&footer);
    injection(ctx, &repo, v, "resume", body.as_bytes());
    if let Some((line, name)) = handoff
        && body.contains(&line)
    {
        let sid = if session(v).is_empty() {
            "unknown"
        } else {
            session(v)
        };
        let _ = append(
            &repo.join(".oms/handoffs/consumed.jsonl"),
            &json!({"schema":1,"kind":"handoff-consumed","handoff":name,"session":sid,"ts":ctx.snapshot.now()}),
            64 * INPUT_MAX,
        );
    }
    out(body)
}
fn handoff_line(
    repo: &Path,
    active: &str,
    task: &[u8],
    v: &Value,
    ctx: &Context<'_>,
) -> Option<(String, String)> {
    // Resume is line-oriented and strips carriage returns; do not rewrite paths.
    let repo_text = repo.to_str()?;
    if repo_text.contains(['\r', '\n']) {
        return None;
    }
    let head = git(repo, &["rev-parse", "HEAD"], ctx).unwrap_or_default();
    let digest = hash(task);
    let mut candidates = Vec::new();
    for entry in fs::read_dir(repo.join(".oms/handoffs")).ok()?.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !entry.file_type().ok()?.is_file()
            || !name.ends_with(".md")
            || name.contains(['\r', '\n'])
        {
            continue;
        }
        let age = entry
            .metadata()
            .ok()?
            .modified()
            .ok()?
            .elapsed()
            .ok()?
            .as_secs();
        if age > 72 * 3600 {
            continue;
        }
        let raw = read(&entry.path(), INPUT_MAX).ok()?;
        let header = String::from_utf8_lossy(&raw[..raw.len().min(4096)]);
        let meta = |k: &str| {
            header
                .lines()
                .find_map(|l| l.strip_prefix(&format!("- {k}: ")))
                .unwrap_or("")
                .to_owned()
        };
        let matched = !active.is_empty() && meta("task_id") == active;
        let current =
            matched && !head.is_empty() && meta("head") == head && meta("task_digest") == digest;
        let label = if current {
            "current task snapshot; recheck source"
        } else if matched {
            "same task, recheck changed state"
        } else {
            "historical/unbound reference"
        };
        candidates.push((matched, current, std::cmp::Reverse(age), name, label, age));
    }
    let (_, _, _, name, label, age) = candidates.into_iter().max_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    })?;
    let mut pickup = String::new();
    if let Ok(raw) = read(&repo.join(".oms/handoffs/consumed.jsonl"), 64 * INPUT_MAX) {
        for row in raw.split(|b| *b == b'\n').filter_map(document) {
            if text(&row, "handoff") == name {
                if text(&row, "session") != session(v) {
                    pickup = format!(
                        "; already picked up by {} at {}",
                        safe_text(text(&row, "session"), 120),
                        safe_text(text(&row, "ts"), 40)
                    );
                }
                break;
            }
        }
    }
    let path = repo.join(".oms/handoffs").join(&name);
    Some((
        format!(
            "- handoff ({label}; {}h old{pickup}): oms session-handoff show {} --cwd {}",
            age / 3600,
            shlex::try_quote(path.to_str()?).ok()?,
            shlex::try_quote(repo_text).ok()?
        ),
        name,
    ))
}
fn unpublished(repo: &Path, ctx: &Context<'_>) -> String {
    if git(repo, &["remote"], ctx).is_none_or(|s| s.is_empty()) {
        return String::new();
    }
    let default = git(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        ctx,
    )
    .or_else(|| {
        ["origin/main", "origin/master"]
            .iter()
            .find(|n| {
                git(
                    repo,
                    &["rev-parse", "--verify", "-q", &format!("refs/remotes/{n}")],
                    ctx,
                )
                .is_some()
            })
            .map(|s| (*s).into())
    });
    let ahead = |branch: &str| -> Option<(String, String)> {
        let base = git(
            repo,
            &["rev-parse", "--abbrev-ref", &format!("{branch}@{{u}}")],
            ctx,
        )
        .or_else(|| default.clone())?;
        let range = format!("{base}...refs/heads/{branch}");
        let count = git(
            repo,
            &[
                "rev-list",
                "--cherry-pick",
                "--right-only",
                "--count",
                &range,
            ],
            ctx,
        )?;
        if count.parse::<u64>().ok()? == 0 {
            return None;
        }
        if git(
            repo,
            &[
                "rev-list",
                "--cherry-pick",
                "--right-only",
                "--no-merges",
                "--count",
                &range,
            ],
            ctx,
        )
        .as_deref()
            == Some("0")
        {
            let merged = git(
                repo,
                &[
                    "merge-tree",
                    "--write-tree",
                    &base,
                    &format!("refs/heads/{branch}"),
                ],
                ctx,
            );
            if merged.as_deref().and_then(|s| s.split_whitespace().next())
                == git(repo, &["rev-parse", &format!("{base}^{{tree}}")], ctx).as_deref()
            {
                return None;
            }
        }
        Some((count, base))
    };
    let here = git(repo, &["symbolic-ref", "--short", "HEAD"], ctx).unwrap_or_default();
    let mut parts = Vec::new();
    if let Some((count, base)) = ahead(&here) {
        parts.push(format!("{count} commit(s) on {here} not on {base}"));
    }
    let mut others = Vec::new();
    for l in git(repo, &["worktree", "list", "--porcelain"], ctx)
        .unwrap_or_default()
        .lines()
    {
        if let Some(name) = l.strip_prefix("branch refs/heads/")
            && name != here
            && let Some((count, _)) = ahead(name)
        {
            others.push(format!("{name}({count})"));
        }
    }
    if !others.is_empty() {
        parts.push(format!(
            "other worktrees: {}{}",
            others
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            if others.len() > 8 {
                format!(" +{} more", others.len() - 8)
            } else {
                String::new()
            }
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("- unpublished: {}", parts.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    #[test]
    fn embedded_usage_families_parse_and_compile() {
        let v: Value = serde_json::from_str(EMBEDDED_USAGE_FAMILIES).unwrap();
        let families = v["families"].as_object().unwrap();
        assert!(!families.is_empty());
        for (name, spec) in families {
            assert!(
                regex::Regex::new(text(spec, "pattern")).is_ok()
                    && !text(spec, "pattern").is_empty(),
                "{name}"
            );
        }
    }

    static TEST_SEQ: AtomicU64 = AtomicU64::new(1);

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let n = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir()
                .join(format!("oms-hooks-unit-{label}-{}-{n}", std::process::id()));
            fs::create_dir_all(&p).expect("mkdir temp");
            let p = fs::canonicalize(&p).expect("canonicalize temp");
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct MockHost {
        detached: Mutex<Vec<(String, Vec<String>)>>,
        failures: Mutex<Vec<(String, i32, String)>>,
    }

    impl crate::HostServices for MockHost {
        fn supports_detach(&self) -> bool {
            true
        }

        fn detach_command(&self, tool: &str, args: &[OsString], _cwd: &Path) -> Result<(), String> {
            self.detached.lock().expect("lock").push((
                tool.to_owned(),
                args.iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
            ));
            Ok(())
        }

        fn record_hook_failure(
            &self,
            _repo: &Path,
            cmd: &str,
            exit: i32,
            summary: &str,
            _next: &str,
        ) {
            self.failures
                .lock()
                .expect("lock")
                .push((cmd.to_owned(), exit, summary.to_owned()));
        }
    }

    fn rfc3339_secs_ago(offset: i64) -> String {
        let ts = (seconds() as i64 - offset).max(0);
        chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
            .unwrap_or_default()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    #[test]
    fn pending_background_tracks_launches_and_stops() {
        let tmp = TempDir::new("pending");
        let transcript = tmp.0.join("session.jsonl");
        let recent = rfc3339_secs_ago(120);
        let stale = rfc3339_secs_ago(90_000);
        let rows = [
            json!({"timestamp": recent, "message": {"content": [{"type": "tool_result", "content": "Command running in background with ID: bg_active"}]}}),
            json!({"timestamp": recent, "message": {"content": [{"type": "tool_result", "content": "Monitor started (task bg_stopped"}]}}),
            json!({"timestamp": recent, "message": {"content": [{"type": "tool_use", "name": "TaskStop", "input": {"task_id": "bg_stopped"}}]}}),
            json!({"timestamp": recent, "message": {"content": [{"type": "tool_result", "content": "Command running in background with ID: bg_notified"}]}}),
            json!({"timestamp": recent, "message": {"content": [{"type": "text", "text": "<task-notification><task-id>bg_notified</task-id><status>completed</status></task-notification>"}]}}),
            json!({"timestamp": stale, "message": {"content": [{"type": "tool_result", "content": "Command running in background with ID: bg_stale"}]}}),
        ];
        fs::write(
            &transcript,
            rows.iter()
                .map(|r| r.to_string() + "\n")
                .collect::<String>(),
        )
        .expect("write transcript");

        let payload = json!({"transcript_path": transcript});
        assert_eq!(pending_background(&payload, 86400.0, seconds()), 1);
    }

    #[test]
    fn start_codex_notify_obeys_elapsed_and_pending_background_gates() {
        let tmp = TempDir::new("codex-notify");
        let repo = tmp.0.join("project");
        fs::create_dir_all(&repo).expect("mkdir repo");
        let turn_state = tmp.0.join("turn.json");
        let transcript = tmp.0.join("session.jsonl");
        let recent = rfc3339_secs_ago(45);
        fs::write(
            &transcript,
            json!({"timestamp": recent, "message": {"content": [{"type": "tool_result", "content": "Command running in background with ID: bg_live"}]}}).to_string() + "\n",
        )
        .expect("write transcript");

        let cancel = AtomicBool::new(false);
        let host = MockHost::default();
        let snapshot = Snapshot::default();
        let ctx = Context {
            snapshot: &snapshot,
            provider: Some("claude"),
            cwd: &repo,
            source: &repo,
            interrupted: &cancel,
            host: &host,
        };

        // Too fast (< 30s): silent.
        write(
            &turn_state,
            &json!({"schema": 1, "prompt_at": seconds() - 5.0}),
        )
        .expect("write");
        start_codex_notify_with(&repo, &json!({}), "done", &turn_state, true, &ctx);
        assert!(host.detached.lock().expect("lock").is_empty());

        // Elapsed >= 30s, but pending background task open: silent.
        write(
            &turn_state,
            &json!({"schema": 1, "prompt_at": seconds() - 45.0}),
        )
        .expect("write");
        start_codex_notify_with(
            &repo,
            &json!({"transcript_path": transcript}),
            "done",
            &turn_state,
            true,
            &ctx,
        );
        assert!(host.detached.lock().expect("lock").is_empty());

        // Elapsed >= 30s and zero pending background: detaches notify send.
        start_codex_notify_with(
            &repo,
            &json!({}),
            "All checks passed",
            &turn_state,
            true,
            &ctx,
        );
        let calls = host.detached.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "codex-app-notify");
        assert_eq!(calls[0].1[0], "send");
        assert!(calls[0].1[2].contains("All checks passed"));
    }

    #[test]
    fn defer_finish_detaches_journal_and_skips_completed_ci_sha() {
        let tmp = TempDir::new("defer-finish");
        let repo = tmp.0.join("repo");
        fs::create_dir_all(&repo).expect("mkdir repo");
        let ledger = tmp.0.join("ci.jsonl");
        fs::write(&ledger, "{\"sha\":\"abc123\",\"status\":\"completed\"}\n").expect("write ci");

        let cancel = AtomicBool::new(false);
        let host = MockHost::default();
        let snapshot = Snapshot::default();
        let ctx = Context {
            snapshot: &snapshot,
            provider: Some("claude"),
            cwd: &repo,
            source: &repo,
            interrupted: &cancel,
            host: &host,
        };

        defer_finish_with(&repo, true, true, Some("abc123"), &ledger, &ctx);
        let calls = host.detached.lock().expect("lock").clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "journal");
        assert_eq!(calls[0].1[0], "sync");

        defer_finish_with(&repo, false, true, Some("def456"), &ledger, &ctx);
        let calls = host.detached.lock().expect("lock");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].0, "ci-status");
        assert_eq!(calls[1].1, vec!["tick"]);
    }

    #[test]
    fn capture_records_hook_failure_through_host_adapter() {
        let tmp = TempDir::new("capture-err");
        let repo = tmp.0.join("repo");
        fs::create_dir_all(repo.join(".oms")).expect("mkdir .oms");
        // Occupy .oms/handoffs with a regular file so antigravity/claude capture returns Err.
        fs::write(repo.join(".oms/handoffs"), "not a directory\n").expect("block handoffs");

        let cancel = AtomicBool::new(false);
        let host = MockHost::default();
        let snapshot = Snapshot::default();
        let ctx = Context {
            snapshot: &snapshot,
            provider: Some("claude"),
            cwd: &repo,
            source: &repo,
            interrupted: &cancel,
            host: &host,
        };

        let ok = capture(
            &json!({"session_id": "missing-session-id"}),
            &repo,
            "claude",
            "auto: pre-compact snapshot",
            false,
            &ctx,
        );
        assert!(!ok);
        let failures = host.failures.lock().expect("lock");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "session-handoff capture --agent claude");
        assert_eq!(failures[0].1, 1);
    }

    fn write_packet(
        repo: &Path,
        task_id: &str,
        status: &str,
        last_activity: &str,
        goal: &str,
        next: &str,
        verify: &str,
    ) -> Vec<u8> {
        let raw = format!(
            "# Active Task\n\
             - task_id: {task_id}\n\
             - status: {status}\n\
             - updated: {last_activity}\n\
             - last_activity: {last_activity}\n\
             - closed_at:\n\n\
             ## Goal\n\n{goal}\n\n\
             ## Verify\n\n{verify}\n\n\
             ## Next Step\n\n{next}\n"
        );
        let path = repo.join(".oms/task/current.md");
        fs::create_dir_all(path.parent().expect("task dir")).expect("mkdir task");
        fs::write(&path, &raw).expect("write packet");
        raw.into_bytes()
    }

    fn write_plan_doc(
        repo: &Path,
        goal: &str,
        ready_ids: &[&str],
        updated: &str,
        contract: Option<Value>,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let mut tasks = serde_json::Map::new();
        for (idx, id) in ready_ids.iter().enumerate() {
            let created = format!("2026-11-24T00:00:{:02}Z", idx % 60);
            tasks.insert(
                (*id).to_owned(),
                json!({
                    "id": id,
                    "title": format!("Task {id}"),
                    "state": "ready",
                    "depends": [],
                    "created": created,
                    "updated": updated,
                }),
            );
        }
        let mut doc = json!({
            "schema": 3,
            "goal": goal,
            "tasks": tasks,
        });
        if let Some(contract) = contract {
            doc["project_contract"] = contract;
        }
        let plan_dir = repo.join(".oms/plan");
        fs::create_dir_all(&plan_dir).expect("mkdir plan");
        for dir in [repo, &repo.join(".oms"), &plan_dir] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("chmod dir");
        }
        let path = plan_dir.join("tasks.json");
        fs::write(&path, serde_json::to_vec(&doc).expect("serialize plan")).expect("write plan");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod plan");
    }

    fn write_plan(repo: &Path, goal: &str, ready_ids: &[&str], updated: &str) {
        write_plan_doc(repo, goal, ready_ids, updated, None);
    }

    fn assert_hint_lines_fit_80_cols(text: &str) {
        for line in text.lines() {
            if line.trim_start().starts_with("hint:")
                || line.trim_start().starts_with("verify: omitted")
            {
                assert!(
                    line.chars().count() <= 80,
                    "line exceeds 80 columns ({}): {line}",
                    line.chars().count()
                );
            }
        }
    }

    fn run_isolated_resume_test(test_name: &str, task_ttl: Option<&str>) -> bool {
        if std::env::var_os("OMS_HOOK_TTL_ISOLATED").is_some() {
            return false;
        }
        let mut cmd = std::process::Command::new(std::env::current_exe().expect("exe"));
        cmd.args(["--exact", test_name, "--nocapture"])
            .env("OMS_HOOK_TTL_ISOLATED", "1")
            .env_remove("OMS_RESUME_HOOK")
            .env_remove("OMS_RESUME_MEMORY")
            .env_remove("OMS_STATE_REPO");
        match task_ttl {
            Some(ttl) => {
                cmd.env("OMS_AGENT_TASK_TTL", ttl);
            }
            None => {
                cmd.env_remove("OMS_AGENT_TASK_TTL");
            }
        }
        let output = cmd.output().expect("spawn isolated test");
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    fn run_isolated_memory_test(test_name: &str) -> bool {
        if std::env::var_os("OMS_HOOK_MEMORY_ISOLATED").is_some() {
            return false;
        }
        let tmp = TempDir::new("memory-env");
        let mut cmd = std::process::Command::new(std::env::current_exe().expect("exe"));
        cmd.args(["--exact", test_name, "--nocapture"]);
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(|k| k.starts_with("OMS_")) {
                cmd.env_remove(key);
            }
        }
        let result = cmd
            .env("OMS_HOOK_MEMORY_ISOLATED", "1")
            .env("OMS_RESUME_MEMORY", "1")
            .env(
                "OH_MY_SETTING_GLOBAL_MEMORY",
                tmp.0.join("global/shared.md"),
            )
            .env("OMS_LOCK_DIR", tmp.0.join("locks"))
            .output()
            .expect("spawn memory fixture");
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        true
    }

    #[test]
    fn resume_memory_uses_current_objective_and_reserves_recall_budget() {
        if run_isolated_memory_test(
            "lifecycle::tests::resume_memory_uses_current_objective_and_reserves_recall_budget",
        ) {
            return;
        }
        let tmp = TempDir::new("resume-memory");
        let repo = tmp.0.join("repo");
        fs::create_dir_all(repo.join(".oms")).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        let cancel = AtomicBool::new(false);
        let memory = |args: &[&str]| {
            let args: Vec<_> = args.iter().map(OsString::from).collect();
            let (code, out, err) =
                oms_command_memory::run_with_source(&args, &repo, &repo, &cancel);
            assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
            String::from_utf8(out).unwrap()
        };
        memory(&[
            "append",
            "--text",
            "vectorbridge preserve the calibrated recall lesson",
        ]);
        memory(&["append", "--text", "closedbeacon archived packet lesson"]);
        memory(&["append", "--text", "projectsignal declared project lesson"]);
        memory(&[
            "append",
            "--text",
            &"unrelated recent observation ".repeat(140),
        ]);
        memory(&["pin", "--text", "Explicit project pin stays eligible"]);
        let global =
            std::path::PathBuf::from(std::env::var_os("OH_MY_SETTING_GLOBAL_MEMORY").unwrap());
        fs::create_dir_all(global.parent().unwrap()).unwrap();
        fs::write(
            &global,
            format!(
                "## 2026-01-01T00:00:00Z agent\n\n{}\n",
                "Unrelated global history ".repeat(180)
            ),
        )
        .unwrap();
        let global_pins: String = (0..12)
            .map(|n| {
                format!(
                    "- 2026-01-01T00:00:00Z [agent] Older global pin {n}: {}\n",
                    "keep unrelated preference ".repeat(12)
                )
            })
            .collect();
        fs::write(global.parent().unwrap().join("pins.md"), global_pins).unwrap();
        let old_view = oms_command_memory::provider_context(&repo, "vectorbridge", &cancel);
        assert_eq!(old_view.0, 0);
        assert!(old_view.1.len() > 1200);
        assert!(!String::from_utf8_lossy(&old_view.1[..1200]).contains("vectorbridge"));
        let database = repo.join(".oms/memory/memory.sqlite3");
        let summary = repo.join(".oms/memory/summary.md");
        let before = [fs::read(&database).unwrap(), fs::read(&summary).unwrap()];
        let fresh = rfc3339_secs_ago(30);
        write_plan(&repo, "vectorbridge", &["resume-target"], &fresh);
        let host = MockHost::default();
        let snapshot = Snapshot::default();
        let ctx = Context {
            snapshot: &snapshot,
            provider: Some("claude"),
            cwd: &repo,
            source: &repo,
            interrupted: &cancel,
            host: &host,
        };
        let resume_memory = || {
            let output = resume(&json!({"cwd":repo}), &ctx);
            assert_eq!(output.0, 0);
            let body = String::from_utf8(output.1).unwrap();
            assert!(body.len() <= 4096, "{body}");
            body.split("- memory (reference only; not approval or verification evidence):\n")
                .nth(1)
                .unwrap_or("")
                .split("- more:")
                .next()
                .unwrap()
                .to_owned()
        };
        for (status, activity) in [
            ("", ""),
            ("closed", fresh.as_str()),
            ("active", ""),
            ("active", &rfc3339_secs_ago(6 * 86400)),
            ("active", &rfc3339_secs_ago(8 * 86400)),
        ] {
            if !status.is_empty() {
                write_packet(
                    &repo,
                    "old-packet",
                    status,
                    activity,
                    "closedbeacon",
                    "old next",
                    "old verify",
                );
            }
            let resumed = resume_memory();
            assert!(
                resumed.contains("vectorbridge preserve the calibrated recall lesson"),
                "{status}: {resumed}"
            );
            assert!(
                resumed.contains("Explicit project pin stays eligible"),
                "{resumed}"
            );
            assert!(!resumed.contains("closedbeacon"), "{status}: {resumed}");
            assert!(
                !resumed.contains("unrelated recent observation"),
                "{resumed}"
            );
            assert!(resumed.len() <= 1201, "{resumed}");
        }
        write_packet(
            &repo,
            "fresh-packet",
            "active",
            &fresh,
            "closedbeacon",
            "current next",
            "current verify",
        );
        let resumed = resume_memory();
        assert!(
            resumed.contains("closedbeacon archived packet lesson"),
            "{resumed}"
        );
        assert!(!resumed.contains("vectorbridge"), "{resumed}");
        // A packet without a `- status:` line is active in both resume and state.
        fs::write(
            repo.join(".oms/task/current.md"),
            format!(
                "# Active Task\n\
                 - task_id: statusless-packet\n\
                 - updated: {fresh}\n\
                 - last_activity: {fresh}\n\
                 - closed_at:\n\n\
                 ## Goal\n\nclosedbeacon\n\n\
                 ## Verify\n\ncurrent verify\n\n\
                 ## Next Step\n\ncurrent next\n"
            ),
        )
        .unwrap();
        let statusless_resume = resume(&json!({"cwd": &repo}), &ctx);
        assert_eq!(statusless_resume.0, 0);
        let statusless_body = String::from_utf8(statusless_resume.1).unwrap();
        assert!(
            statusless_body.contains("- task statusless-packet (active): closedbeacon"),
            "{statusless_body}"
        );
        let statusless_memory = resume_memory();
        assert!(
            statusless_memory.contains("closedbeacon archived packet lesson"),
            "{statusless_memory}"
        );
        assert!(
            !statusless_memory.contains("vectorbridge"),
            "{statusless_memory}"
        );
        let statusless_state = oms_command_state::run_with_source(
            "state",
            &["--repo".into(), repo.as_os_str().into(), "--json".into()],
            &repo,
            &repo,
            &cancel,
        );
        assert_eq!(statusless_state.0, 0);
        let state_doc = document(&statusless_state.1).expect("state json");
        assert_eq!(state_doc["task"]["status"], "active", "{state_doc}");
        assert_eq!(
            state_doc["task"]["current_goal_eligible"], true,
            "{state_doc}"
        );
        assert_eq!(
            state_doc["runtime"]["objective"]["source"], "task",
            "{state_doc}"
        );
        assert_eq!(
            state_doc["runtime"]["objective"]["text"], "closedbeacon",
            "{state_doc}"
        );
        write_packet(
            &repo,
            "closed-packet",
            "closed",
            &fresh,
            "closedbeacon",
            "old next",
            "old verify",
        );
        write_plan(&repo, "vectorbridge", &[], &fresh);
        fs::write(
            repo.join("PROJECT.md"),
            "# Project\n\n## Goal\n\nprojectsignal\n",
        )
        .unwrap();
        let resumed = resume_memory();
        assert!(
            resumed.contains("projectsignal declared project lesson"),
            "{resumed}"
        );
        fs::remove_file(repo.join("PROJECT.md")).unwrap();
        let resumed = resume_memory();
        assert!(
            resumed.contains("Explicit project pin stays eligible"),
            "{resumed}"
        );
        assert!(!resumed.contains("- recall"), "{resumed}");
        assert_eq!(
            before,
            [fs::read(&database).unwrap(), fs::read(&summary).unwrap()]
        );
    }

    #[test]
    fn prompt_journal_uses_native_batch_publication_and_safe_report() {
        if run_isolated_memory_test(
            "lifecycle::tests::prompt_journal_uses_native_batch_publication_and_safe_report",
        ) {
            return;
        }
        let tmp = TempDir::new("journal-memory");
        let repo = tmp.0.join("repo");
        fs::create_dir_all(repo.join(".oms")).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        let cancel = AtomicBool::new(false);
        let host = MockHost::default();
        let snapshot = Snapshot::default();
        let ctx = Context {
            snapshot: &snapshot,
            provider: Some("claude"),
            cwd: &repo,
            source: &repo,
            interrupted: &cancel,
            host: &host,
        };
        let now = chrono::DateTime::<chrono::Utc>::from(snapshot.time());
        let store =
            oms_command_journal::store::JournalStore::open_for_write(&repo, Some("UTC")).unwrap();
        for n in 0..2 {
            let event = serde_json::to_vec(&json!({"event_type":"run","verification_status":"not_verified","occurred_at":snapshot.now(),"source":{"type":"manual","id":format!("hook-lesson-{n}")},"decision":format!("Preserve native hook lesson {n}")})).unwrap();
            store
                .record_event(&oms_json::parse_python_ordered(&event).unwrap(), now)
                .unwrap();
        }
        // One failed derived summary refresh proves that the prompt publishes
        // both candidates as one batch and surfaces one refresh warning.
        fs::create_dir_all(repo.join(".oms/memory/summary.md")).unwrap();
        let first = String::from_utf8(journal(&repo, true, &ctx)).unwrap();
        assert!(first.contains("2 queued for memory review"), "{first}");
        assert!(first.contains("1 warning(s)"), "{first}");
        assert!(!first.contains("unsafe or unreadable source"), "{first}");
        let shared = repo.join(".oms/memory/shared.md");
        let before = fs::read_to_string(&shared).unwrap();
        assert_eq!(before.matches("kind: distill").count(), 2);
        assert_eq!(before.matches("journal_candidate:").count(), 2);
        assert_eq!(before.matches("journal_source_sha256:").count(), 2);
        let batches: std::collections::BTreeSet<_> = before
            .lines()
            .filter_map(|line| line.strip_prefix("journal_batch: "))
            .collect();
        assert_eq!(batches.len(), 1);
        assert_eq!(
            oms_command_memory::pending_review(&repo, &cancel)
                .unwrap()
                .pending,
            2
        );
        assert!(journal(&repo, true, &ctx).is_empty());
        assert_eq!(fs::read_to_string(&shared).unwrap(), before);
        let recall = oms_command_memory::resume_context(&repo, "native hook", &cancel);
        assert_eq!(recall.0, 0);
        assert!(recall.1.is_empty());
    }

    #[test]
    fn resume_marks_dormant_and_stale_packets_and_uses_plan_engine_actionable() {
        if run_isolated_resume_test(
            "lifecycle::tests::resume_marks_dormant_and_stale_packets_and_uses_plan_engine_actionable",
            None,
        ) {
            return;
        }
        let tmp = TempDir::new("resume-stale");
        fs::create_dir(tmp.0.join(".git")).expect("mkdir stray ancestor .git");
        let repo = tmp.0.join("repo");
        fs::create_dir_all(repo.join(".oms")).expect("mkdir .oms");
        assert_eq!(
            root(&repo, &repo),
            Some(repo.canonicalize().expect("canonical repo"))
        );
        let outer = tmp.0.join("outer");
        fs::create_dir_all(outer.join(".git")).expect("mkdir outer .git");
        fs::write(outer.join(".git/HEAD"), "ref: refs/heads/main\n").expect("write outer HEAD");
        let leaf = outer.join("stray/work");
        fs::create_dir_all(outer.join("stray/.git")).expect("mkdir inner stray .git");
        fs::create_dir_all(&leaf).expect("mkdir leaf");
        assert_eq!(
            root(&leaf, &leaf),
            Some(outer.canonicalize().expect("canonical outer"))
        );
        let six_days_ago = rfc3339_secs_ago(6 * 86400 + 60);
        let original = write_packet(
            &repo,
            "task-dormant-6d",
            "active",
            &six_days_ago,
            "Old abandoned handoff goal",
            "Do not follow this dormant next step",
            "cargo test --dormant-verify-marker",
        );
        let fresh_ts = rfc3339_secs_ago(300);
        write_plan(
            &repo,
            "Current live plan goal",
            &["t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8"],
            &fresh_ts,
        );

        let cancel = AtomicBool::new(false);
        let host = MockHost::default();
        let snapshot = Snapshot::default();
        let ctx = Context {
            snapshot: &snapshot,
            provider: Some("claude"),
            cwd: &repo,
            source: &repo,
            interrupted: &cancel,
            host: &host,
        };
        // Break an unrelated `oms state` collector (`.oms/runs/spine.jsonl` as a directory)
        // to prove `resume()` reads plan status from the plan engine directly instead of a
        // full `oms state` run.
        let broken_runs_spine = repo.join(".oms/runs/spine.jsonl");
        fs::create_dir_all(&broken_runs_spine).expect("block runs spine");
        let broken_state = oms_command_state::run_with_source(
            "state",
            &["--repo".into(), repo.as_os_str().into(), "--json".into()],
            &repo,
            &repo,
            &cancel,
        );
        assert_ne!(
            broken_state.0, 0,
            "full oms state should fail when runs spine is unreadable"
        );

        let (code, stdout, _) = resume(&json!({"cwd": repo}), &ctx);
        assert_eq!(code, 0);
        let out = String::from_utf8(stdout).expect("utf8");
        fs::remove_dir_all(repo.join(".oms/runs")).expect("remove blocked runs dir");

        assert!(out.contains("dormant (idle 6d)"), "{out}");
        assert!(!out.contains("stale (idle"), "{out}");
        assert!(out.contains("oms agent-task close --reason"), "{out}");
        assert_hint_lines_fit_80_cols(&out);
        assert!(
            !out.contains("Do not follow this dormant next step"),
            "{out}"
        );
        assert!(!out.contains("cargo test --dormant-verify-marker"), "{out}");
        assert!(
            out.contains("- plan: Current live plan goal [ready=8]"),
            "{out}"
        );
        assert!(!out.contains("[status unavailable]"), "{out}");
        assert!(
            out.contains("actionable: 8 (t1, t2, t3, t4, t5 +3 more)"),
            "{out}"
        );
        assert!(!out.contains("t6"), "{out}");
        let plan_pos = out.find("- plan: Current live plan goal").expect("plan");
        let task_pos = out.find("- task task-dormant-6d").expect("task");
        assert!(
            plan_pos < task_pos,
            "running plan must precede dormant task:\n{out}"
        );
        assert_eq!(
            fs::read(repo.join(".oms/task/current.md")).expect("read packet"),
            original,
            "resume must never edit or close the handoff packet"
        );

        // 8 days idle (past default 7d TTL): marked stale (not dormant), hint <= 80 cols.
        let eight_days_ago = rfc3339_secs_ago(8 * 86400 + 60);
        write_packet(
            &repo,
            "task-stale-8d",
            "active",
            &eight_days_ago,
            "Old 8-day handoff goal",
            "Do not follow this stale next step",
            "cargo test --stale-verify-marker",
        );
        let (code, stdout, _) = resume(&json!({"cwd": repo}), &ctx);
        assert_eq!(code, 0);
        let stale_out = String::from_utf8(stdout).expect("utf8");
        assert!(stale_out.contains("stale (idle 8d)"), "{stale_out}");
        assert!(!stale_out.contains("dormant (idle"), "{stale_out}");
        assert!(
            !stale_out.contains("Do not follow this stale next step"),
            "{stale_out}"
        );
        assert!(
            !stale_out.contains("cargo test --stale-verify-marker"),
            "{stale_out}"
        );
        assert_hint_lines_fit_80_cols(&stale_out);

        // Bind a PROJECT.md contract that blocks new work (draft spec):
        // resume must report 0 actionable tasks, matching `oms state`.
        fs::write(
            repo.join("PROJECT.md"),
            "# Project\n\n- State: draft\n\n## Goal\n\nDraft contract blocking work\n",
        )
        .expect("write draft PROJECT.md");
        write_plan_doc(
            &repo,
            "Contract-blocked plan goal",
            &["t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8"],
            &fresh_ts,
            Some(json!({
                "schema": 1,
                "spec_sha256": "a".repeat(64),
            })),
        );
        let state_out = oms_command_state::run_with_source(
            "state",
            &["--repo".into(), repo.as_os_str().into(), "--json".into()],
            &repo,
            &repo,
            &cancel,
        );
        assert_eq!(state_out.0, 0);
        let state_doc = document(&state_out.1).expect("state json");
        assert_eq!(state_doc["plan"]["healthy"], true);
        assert_eq!(state_doc["plan"]["contract"]["bound"], true);
        assert_eq!(state_doc["plan"]["contract"]["satisfied"], false);
        assert_eq!(
            state_doc["plan"]["actionable"].as_array().map(Vec::len),
            Some(0),
            "{state_doc}"
        );

        let (code, stdout, _) = resume(&json!({"cwd": repo}), &ctx);
        assert_eq!(code, 0);
        let blocked_out = String::from_utf8(stdout).expect("utf8");
        assert!(
            blocked_out.contains("- plan: Contract-blocked plan goal [ready=8]"),
            "{blocked_out}"
        );
        assert!(
            blocked_out.contains("contract: blocked (project-draft)"),
            "{blocked_out}"
        );
        assert!(
            !blocked_out.contains("actionable:"),
            "resume must show 0 actionable when PROJECT.md contract blocks work:\n{blocked_out}"
        );

        // When the plan engine itself fails on a malformed plan, keep `[status unavailable]`
        // and the `accept` line.
        fs::write(
            repo.join(".oms/plan/tasks.json"),
            serde_json::to_vec(&json!({
                "schema": 3,
                "goal": "Broken plan goal",
                "accept": "verify green",
                "tasks": "not-an-object",
            }))
            .expect("serialize broken plan"),
        )
        .expect("write broken plan");
        let (code, stdout, _) = resume(&json!({"cwd": repo}), &ctx);
        assert_eq!(code, 0);
        let broken_out = String::from_utf8(stdout).expect("utf8");
        assert!(
            broken_out.contains("- plan: Broken plan goal [status unavailable]"),
            "{broken_out}"
        );
        assert!(
            broken_out.contains("  accept: verify green"),
            "{broken_out}"
        );
    }

    #[test]
    fn resume_state_status_agree_on_dormant_when_ttl_exceeds_72h() {
        if run_isolated_resume_test(
            "lifecycle::tests::resume_state_status_agree_on_dormant_when_ttl_exceeds_72h",
            Some("864000"),
        ) {
            return;
        }
        let tmp = TempDir::new("resume-ttl");
        let repo = tmp.0.join("repo");
        fs::create_dir_all(repo.join(".oms")).expect("mkdir .oms");
        let eight_days_ago = rfc3339_secs_ago(8 * 86400 + 60);
        write_packet(
            &repo,
            "task-8d-ttl",
            "active",
            &eight_days_ago,
            "Eight day packet under 10d TTL",
            "Hidden in text only",
            "cargo test --hidden",
        );

        let cancel = AtomicBool::new(false);
        let host = MockHost::default();
        let snapshot = Snapshot::default();
        let ctx = Context {
            snapshot: &snapshot,
            provider: Some("claude"),
            cwd: &repo,
            source: &repo,
            interrupted: &cancel,
            host: &host,
        };
        let (code, stdout, _) = resume(&json!({"cwd": repo}), &ctx);
        assert_eq!(code, 0);
        let resume_out = String::from_utf8(stdout).expect("utf8");
        assert!(resume_out.contains("dormant (idle 8d)"), "{resume_out}");
        assert!(!resume_out.contains("stale (idle"), "{resume_out}");
        assert!(!resume_out.contains("Hidden in text only"), "{resume_out}");
        assert_hint_lines_fit_80_cols(&resume_out);

        let (code, state_bytes, _) = oms_command_state::run_with_source(
            "state",
            &["--repo".into(), repo.as_os_str().into(), "--json".into()],
            &repo,
            &repo,
            &cancel,
        );
        assert_eq!(code, 0);
        let state_doc = document(&state_bytes).expect("state json");
        assert_eq!(state_doc["task"]["stale"], false, "{state_doc}");
        assert_eq!(state_doc["task"]["dormant"], true, "{state_doc}");
        assert_eq!(state_doc["task"]["next"], "Hidden in text only");

        let (code, status_bytes, _) = oms_command_task::run(
            &["status".into(), "--repo".into(), repo.as_os_str().into()],
            &repo,
            &cancel,
        );
        assert_eq!(code, 0);
        let status_text = String::from_utf8(status_bytes).expect("utf8");
        assert!(status_text.contains("stale: no\n"), "{status_text}");
        assert!(status_text.contains("dormant: yes\n"), "{status_text}");
    }

    #[test]
    fn resume_handoff_command_reads_exact_digest_from_other_working_directories() {
        if run_isolated_resume_test(
            "lifecycle::tests::resume_handoff_command_reads_exact_digest_from_other_working_directories",
            None,
        ) {
            return;
        }
        let tmp = TempDir::new("handoff-command");
        let repo = tmp.0.join("repo with 'quote");
        let nested = repo.join("nested directory");
        let outside = tmp.0.join("other checkout");
        let directory = repo.join(".oms/handoffs");
        for path in [&nested, &outside, &directory] {
            fs::create_dir_all(path).expect("mkdir fixture");
        }
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .expect("git init")
                .success()
        );
        let name = "handoff with 'quote.md";
        let body = b"# Session handoff\n\nStored handoff detail.\n";
        fs::write(directory.join(name), body).expect("write digest");
        for cwd in [&repo, &nested, &outside] {
            fs::write(cwd.join(name), b"unrelated same-name file\n").expect("write decoy");
        }
        let cancel = AtomicBool::new(false);
        let host = MockHost::default();
        let snapshot = Snapshot::default();
        let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let ctx = Context {
            snapshot: &snapshot,
            provider: Some("claude"),
            cwd: &outside,
            source,
            interrupted: &cancel,
            host: &host,
        };
        let (code, stdout, stderr) = resume(&json!({"cwd": nested}), &ctx);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
        let rendered = String::from_utf8(stdout).expect("utf8 resume");
        assert!(!rendered.contains("Stored handoff detail"));
        let command = rendered
            .lines()
            .find(|line| line.starts_with("- handoff ("))
            .and_then(|line| line.split_once(": oms "))
            .map(|(_, command)| format!("oms {command}"))
            .expect("handoff command");
        let words = shlex::split(&command).expect("shell-safe handoff command");
        assert_eq!(&words[..2], ["oms", "session-handoff"]);
        let args = words[2..].iter().map(OsString::from).collect::<Vec<_>>();
        for cwd in [&repo, &nested, &outside] {
            let result =
                oms_command_peer::run_with_source("session-handoff", &args, cwd, source, &cancel);
            assert_eq!(result.0, 0, "{}", String::from_utf8_lossy(&result.2));
            assert_eq!(result.1, body, "caller cwd: {}", cwd.display());
        }
        assert!(
            words
                .windows(2)
                .any(|pair| pair[0] == "--cwd" && Path::new(&pair[1]) == repo),
            "{command}"
        );
        let consumed = fs::read(directory.join("consumed.jsonl")).expect("pickup receipt");
        let receipt = consumed
            .split(|byte| *byte == b'\n')
            .find_map(document)
            .expect("pickup row");
        assert_eq!(text(&receipt, "handoff"), name);

        let leading_dash = directory.join("--handoff.md");
        fs::rename(directory.join(name), &leading_dash).expect("rename digest");
        let (line, _) = handoff_line(&repo, "", &[], &json!({}), &ctx).expect("leading dash hint");
        let (_, command) = line.split_once(": oms ").expect("leading dash command");
        let words = shlex::split(command).expect("parse leading dash command");
        let args = words[1..].iter().map(OsString::from).collect::<Vec<_>>();
        let result =
            oms_command_peer::run_with_source("session-handoff", &args, &outside, source, &cancel);
        assert_eq!(result.0, 0, "{}", String::from_utf8_lossy(&result.2));
        assert_eq!(result.1, body);
        fs::remove_file(leading_dash).expect("remove digest");
        for name in ["line\nbreak.md", "carriage\rreturn.md"] {
            let path = directory.join(name);
            fs::write(&path, body).expect("write control-character name");
            assert!(handoff_line(&repo, "", &[], &json!({}), &ctx).is_none());
            fs::remove_file(path).expect("remove control-character name");
        }
        let control_repo = tmp.0.join("repo\rwith\ncontrols");
        fs::create_dir_all(control_repo.join(".oms/handoffs")).expect("mkdir control repo");
        fs::write(control_repo.join(".oms/handoffs/digest.md"), body)
            .expect("write control repo digest");
        assert!(handoff_line(&control_repo, "", &[], &json!({}), &ctx).is_none());
    }

    #[test]
    fn resume_keeps_fresh_packet_caps_long_verify_and_keeps_plan_before_truncation() {
        if run_isolated_resume_test(
            "lifecycle::tests::resume_keeps_fresh_packet_caps_long_verify_and_keeps_plan_before_truncation",
            None,
        ) {
            return;
        }
        let tmp = TempDir::new("resume-fresh");
        let repo = tmp.0.join("repo");
        fs::create_dir_all(repo.join(".oms")).expect("mkdir .oms");
        let one_hour_ago = rfc3339_secs_ago(3600);
        write_packet(
            &repo,
            "task-fresh-1h",
            "active",
            &one_hour_ago,
            "Fresh handoff goal",
            "Run unit tests next",
            "cargo test --lib",
        );

        let cancel = AtomicBool::new(false);
        let host = MockHost::default();
        let snapshot = Snapshot::default();
        let ctx = Context {
            snapshot: &snapshot,
            provider: Some("claude"),
            cwd: &repo,
            source: &repo,
            interrupted: &cancel,
            host: &host,
        };
        let (code, stdout, _) = resume(&json!({"cwd": repo}), &ctx);
        assert_eq!(code, 0);
        let fresh_out = String::from_utf8(stdout).expect("utf8");
        assert!(
            fresh_out.contains("- task task-fresh-1h (active): Fresh handoff goal"),
            "{fresh_out}"
        );
        assert!(
            fresh_out.contains("  next: Run unit tests next"),
            "{fresh_out}"
        );
        assert!(
            fresh_out.contains("  verify: cargo test --lib (oms agent-task verify)"),
            "{fresh_out}"
        );
        assert!(!fresh_out.contains("stale"), "{fresh_out}");

        // Now write a long verify command (> 160 chars) plus a long lower-priority section
        // (via huge Next Step) and a running Plan to confirm verify is never inlined and the
        // plan goal + bounded actionable list survive 4096-byte truncation.
        let long_verify = format!("cargo test --workspace && echo {}", "v".repeat(500));
        let long_next = "n".repeat(4200);
        write_packet(
            &repo,
            "task-fresh-long",
            "active",
            &one_hour_ago,
            "Fresh task with huge verify",
            &long_next,
            &long_verify,
        );
        write_plan(
            &repo,
            "Must survive resume truncation",
            &["p1", "p2", "p3", "p4", "p5", "p6", "p7"],
            &one_hour_ago,
        );
        let (code, stdout, _) = resume(&json!({"cwd": repo}), &ctx);
        assert_eq!(code, 0);
        let capped_out = String::from_utf8(stdout).expect("utf8");
        assert!(capped_out.len() <= 4096, "len={}", capped_out.len());
        assert!(
            capped_out.contains("[resume truncated; inspect current state]"),
            "{capped_out}"
        );
        assert!(
            capped_out.contains("- plan: Must survive resume truncation [ready=7]"),
            "{capped_out}"
        );
        assert!(!capped_out.contains("[status unavailable]"), "{capped_out}");
        assert!(
            capped_out.contains("actionable: 7 (p1, p2, p3, p4, p5 +2 more)"),
            "{capped_out}"
        );
        assert!(!capped_out.contains(&long_verify), "{capped_out}");

        // Without huge Next Step, verify line itself is visible, shows length and where to read it.
        write_packet(
            &repo,
            "task-fresh-long",
            "active",
            &one_hour_ago,
            "Fresh task with huge verify",
            "short next",
            &long_verify,
        );
        let (code, stdout, _) = resume(&json!({"cwd": repo}), &ctx);
        assert_eq!(code, 0);
        let verify_out = String::from_utf8(stdout).expect("utf8");
        assert!(!verify_out.contains(&long_verify), "{verify_out}");
        assert!(
            verify_out.contains(&format!("{} chars", long_verify.chars().count())),
            "{verify_out}"
        );
        assert!(
            verify_out.contains(".oms/task/current.md")
                || verify_out.contains("oms agent-task show"),
            "{verify_out}"
        );
        assert_hint_lines_fit_80_cols(&verify_out);
    }
}
