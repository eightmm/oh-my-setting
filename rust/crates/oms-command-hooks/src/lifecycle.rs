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
            precompact(&v, args, ctx);
            empty()
        }
        "resume-hook" => resume(&v, ctx),
        "turn-guard" => stop(&v, ctx),
        _ => empty(),
    }
}
pub(crate) fn telemetry(v: &Value, ctx: &Context<'_>) {
    let Some(repo) = repo(v, ctx).filter(|r| adopted(r)) else {
        return;
    };
    activity(v, &repo, ctx);
    if event(v) == "Notification" {
        if v["notification_type"] == "permission_prompt" {
            event_row(
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
    if ["SessionStart", "SessionEnd"].contains(&event(v))
        || any
        || fields["success"] == false
        || env("OMS_TELEMETRY_DEBUG") == "1"
    {
        event_row(&repo, v, fields);
    }
}
pub(crate) fn activity(v: &Value, repo: &Path, ctx: &Context<'_>) {
    if !panel() || !["UserPromptSubmit", "PostToolUse", "Stop", "Notification"].contains(&event(v))
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
    let _ = mutate(&path, 4096, false, |raw| {
        let old = document(raw).unwrap_or_else(|| json!({}));
        let now = seconds();
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
            origin=if agent(v, ctx)=="claude"&&!same||regex::Regex::new(r"^\s*(?:<task-notification(?:\s|>)|\[SYSTEM NOTIFICATION\b|Another Claude session sent a message:|Your claude\.ai usage limit has reset\b)").unwrap().is_match(text(v,"prompt")){"harness"}else{"user"}.into();
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
        normalized = regex::Regex::new(r"/(?:hom[e]|User[s])/[A-Za-z0-9._-]+")
            .unwrap()
            .replace_all(&normalized, "~")
            .into_owned();
        normalized = regex::Regex::new(r"\d{8,}")
            .unwrap()
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
        regex::Regex::new(r"^Exit code (\d+)")
            .unwrap()
            .captures(text(v, "error"))
            .and_then(|c| c[1].parse::<u64>().ok())
            .unwrap_or(1)
    } else {
        1
    };
    if [0, 130, 141, 143].contains(&exit)
        || regex::Regex::new(r"/tmp/claude-\d+/")
            .unwrap()
            .is_match(&command)
    {
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
    let families = load(&ctx.source.join("scripts/lib/usage-families.json"));
    let day = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .format("%Y-%m-%d")
        .to_string();
    if let Some(families) = families["families"].as_object() {
        for (name, spec) in families {
            if regex::Regex::new(text(spec, "pattern"))
                .ok()
                .is_some_and(|re| {
                    !text(spec, "pattern").is_empty() && re.is_match(&command.to_lowercase())
                })
            {
                let _ = append(
                    &repo.join(".oms/usage.jsonl"),
                    &json!({"schema":1,"family":name,"day":day}),
                    64 * INPUT_MAX,
                );
            }
        }
    }
}
pub(crate) fn capture(
    v: &Value,
    repo: &Path,
    agent: &str,
    note: &str,
    skip: bool,
    ctx: &Context<'_>,
) -> bool {
    if interrupted(ctx) || !adopted(repo) {
        return false;
    }
    match crate::handoff::capture(v, repo, agent, note, skip, ctx) {
        Ok(captured) => captured,
        Err(summary) => {
            let summary = safe_text(&summary, 160);
            let _ = ledger(
                &[
                    "record",
                    "--kind",
                    "hook",
                    "--cmd",
                    &format!("session-handoff capture --agent {agent}"),
                    "--exit",
                    "1",
                    "--summary",
                    &summary,
                    "--next",
                    &format!("run manually: oms session-handoff capture --agent {agent}"),
                ],
                repo,
                ctx,
            );
            false
        }
    }
}

fn precompact(v: &Value, args: &[OsString], ctx: &Context<'_>) {
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
    let _ = capture(v, &repo, agent, note, true, ctx);
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
            || lines[0].parse::<f64>().ok()? <= seconds()
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
        let now = oms_command_journal::tz::current_utc_now();
        store.capture_head_commit(now)?;
        store.materialize()?;
        let mut output = String::new();
        if digest && let Some(text) = store.prompt_digest(now)? {
            output.push_str(&text);
            output.push('\n');
        }
        if distill && store.distill_due_today(now)? {
            let append = |repo: &Path, agent: &str, body: &str| {
                let result = oms_command_memory::append_project_note(
                    repo,
                    agent,
                    "journal",
                    body,
                    ctx.interrupted,
                );
                Ok(if result.0 == 0 {
                    oms_command_journal::MemoryAppendOutcome::Appended
                } else {
                    oms_command_journal::MemoryAppendOutcome::Refused(
                        "native memory producer refused".into(),
                    )
                })
            };
            let _ = store.distill(false, Some(&append), now);
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
                injection(&repo, v, "turn-guard", &raw);
                return out(raw);
            }
            Err(_) => {
                relay(v, &repo, ctx);
                let _ = journal(&repo, false, ctx);
                return out(json_line(
                    &json!({"systemMessage":"oh-my-setting turn guard: unavailable (helper error); this turn was not checked."}),
                ));
            }
            _ => {}
        }
    }
    relay(v, &repo, ctx);
    let _ = journal(&repo, false, ctx);
    empty()
}
fn budget(v: &Value, repo: &Path, ctx: &Context<'_>) -> Result<Option<String>, String> {
    let path = state_path(repo, v, "");
    if !path.exists() {
        return Ok(None);
    }
    let turns_cap = cap("OMS_SESSION_BUDGET_TURNS", 0, 0, u64::MAX);
    let hours_cap = cap("OMS_SESSION_BUDGET_HOURS", 0, 0, u64::MAX);
    let now_secs = seconds();
    let result = mutate(&path, STATE_MAX, false, |raw| {
        let mut state = document(raw).ok_or("invalid budget state")?;
        let began = epoch(text(&state, "started_at")).unwrap_or(now_secs);
        if epoch(text(&state, "started_at")).is_none() {
            state["started_at"] = json!(now());
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
        state["updated_at"] = json!(now());
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
fn relay(v: &Value, repo: &Path, ctx: &Context<'_>) {
    if !on("OMS_RELAY")
        || !adopted(repo)
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
    let tail = message
        .chars()
        .skip(message.chars().count().saturating_sub(4000))
        .collect::<String>();
    let row = json!({"schema":1,"agent":"claude","session":session_hash(v),"ended_at":now(),"head":git(repo,&["rev-parse","--short","HEAD"],ctx).unwrap_or_default(),"branch":git(repo,&["branch","--show-current"],ctx).unwrap_or_default(),"message":safe_text(&tail,cap("OMS_RELAY_BYTES",800,120,4000)as usize)});
    let _ = write(&repo.join(".oms/hooks/relay/claude.json"), &row);
}
pub(crate) fn resume(v: &Value, ctx: &Context<'_>) -> Output {
    if !on("OMS_RESUME_HOOK") {
        return empty();
    }
    let Some(repo) = root(payload_cwd(v, ctx.cwd), ctx.cwd).filter(|r| adopted(r)) else {
        return empty();
    };
    let mut lines = Vec::new();
    let task = read(&repo.join(".oms/task/current.md"), 1024 * 1024).unwrap_or_default();
    let meta = |key: &str| {
        oms_command_task::packet::metadata(&task, key)
            .map(|s| String::from_utf8_lossy(s).trim().to_owned())
            .unwrap_or_default()
    };
    let task_id = meta("task_id");
    let status = meta("status");
    let section = |key: &str| {
        String::from_utf8_lossy(&oms_command_task::packet::section(&task, key))
            .lines()
            .find(|s| !s.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let goal = section("Goal");
    if !task_id.is_empty() && status != "closed" {
        let days = epoch(&meta("last_activity"))
            .map(|s| ((seconds() - s) / 86400.).floor() as i64)
            .unwrap_or(0);
        lines.push(format!(
            "- task {task_id} ({status}{}): {}",
            if days >= 1 {
                format!(", idle {days}d")
            } else {
                String::new()
            },
            if goal.is_empty() {
                "no goal recorded"
            } else {
                &goal
            }
        ));
        let next = section("Next Step");
        if !next.is_empty() {
            lines.push(format!("  next: {next}"));
        }
        let verify = section("Verify");
        if !verify.is_empty() {
            lines.push(format!("  verify: {verify} (oms agent-task verify)"));
        }
    }
    let plan = load(&repo.join(".oms/plan/tasks.json"));
    let tasks = crate::routing::plan_tasks(&plan);
    if !text(&plan, "goal").trim().is_empty()
        && !(!tasks.is_empty()
            && tasks
                .iter()
                .all(|t| ["done", "cancelled"].contains(&text(t, "state"))))
    {
        let mut counts = BTreeMap::<String, usize>::new();
        for t in &tasks {
            *counts.entry(text(t, "state").into()).or_default() += 1;
        }
        let summary = counts
            .iter()
            .map(|(s, n)| format!("{s}={n}"))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!(
            "- plan: {}{}",
            clip(&flat(text(&plan, "goal")), 160),
            if summary.is_empty() {
                String::new()
            } else {
                format!(" [{summary}]")
            }
        ));
        let accept = flat(text(&plan, "accept"));
        if !accept.is_empty() {
            lines.push(format!("  accept: {}", clip(&accept, 140)));
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
            &json!({"schema":1,"updated_at":now(),"source":text(v,"source")}),
        );
    }
    if text(v, "source") == "compact" && !lines.is_empty() {
        lines.push("- after compaction: the lines above were re-read from .oms state, not from the summary; OMS skills are invoked by name (/oms-<skill>), and oms state shows the rest".into());
    }
    if env("OMS_RESUME_MEMORY") == "1" {
        let result =
            oms_command_memory::provider_context(&repo, &clip(&flat(&goal), 300), ctx.interrupted);
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
    injection(&repo, v, "resume", body.as_bytes());
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
            &json!({"schema":1,"kind":"handoff-consumed","handoff":name,"session":sid,"ts":now()}),
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
    let head = git(repo, &["rev-parse", "HEAD"], ctx).unwrap_or_default();
    let digest = hash(task);
    let mut candidates = Vec::new();
    for entry in fs::read_dir(repo.join(".oms/handoffs")).ok()?.flatten() {
        if !entry.file_type().ok()?.is_file()
            || !entry.file_name().to_string_lossy().ends_with(".md")
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
        let name = entry.file_name().to_string_lossy().into_owned();
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
    Some((
        format!(
            "- handoff ({label}; {}h old{pickup}): oms session-handoff show {}",
            age / 3600,
            shlex::try_quote(&name).ok()?
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
