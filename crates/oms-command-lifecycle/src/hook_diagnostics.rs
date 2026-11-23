//! Read-only hook configuration and retained callback evidence, never live authority.
use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use oms_safe_fs::{Policy, Root};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, os::unix::fs::MetadataExt, path::Path};

const TRACE: &str = ".oms/hooks/codex-trace.jsonl";
const TRACE_LIMIT: usize = 1024 * 1024;

#[derive(Default)]
struct Attempt {
    handler: String,
    received_at: String,
    event: String,
    received: bool,
    entered: bool,
    outcome: Option<String>,
    invalid: bool,
}

fn trace_summary(raw: &[u8], expected: &Value) -> Value {
    let mut handlers = BTreeMap::<String, Vec<&str>>::new();
    for (event, groups) in expected["hooks"].as_object().into_iter().flatten() {
        for group in groups.as_array().into_iter().flatten() {
            for hook in group["hooks"].as_array().into_iter().flatten() {
                if let Some(words) = hook["command"]
                    .as_str()
                    .and_then(crate::integrations::command_words)
                    && let Some(handler) = words.get(2)
                {
                    handlers.entry(handler.clone()).or_default().push(event);
                }
            }
        }
    }
    let mut attempts = BTreeMap::<String, Attempt>::new();
    let mut invalid_records = 0_u64;
    for bytes in raw.split_inclusive(|byte| *byte == b'\n') {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let row = if bytes.last() == Some(&b'\n') {
            r::parse(bytes).ok()
        } else {
            None
        };
        let Some(row) = row.filter(|row| row["schema"] == 1) else {
            invalid_records += 1;
            continue;
        };
        let nonce = row["nonce"].as_str().unwrap_or("");
        let handler = row["handler"].as_str().unwrap_or("");
        let event = row["event"].as_str().unwrap_or("");
        if nonce.len() != 32
            || !nonce
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !handlers
                .get(handler)
                .is_some_and(|events| event == "unknown" || events.contains(&event))
            || row["received_at"]
                .as_str()
                .is_none_or(|value| chrono::DateTime::parse_from_rfc3339(value).is_err())
        {
            invalid_records += 1;
            continue;
        }
        let attempt = attempts.entry(nonce.into()).or_default();
        if attempt.handler.is_empty() {
            attempt.handler = handler.into();
            attempt.received_at = row["received_at"].as_str().unwrap().into();
        }
        let valid = attempt.handler == handler
            && attempt.received_at == row["received_at"].as_str().unwrap()
            && match row["phase"].as_str() {
                Some("received")
                    if !attempt.received
                        && !attempt.entered
                        && attempt.outcome.is_none()
                        && event == "unknown" =>
                {
                    attempt.received = true;
                    attempt.event = event.into();
                    true
                }
                Some("entry")
                    if attempt.received && !attempt.entered && attempt.outcome.is_none() =>
                {
                    attempt.entered = true;
                    attempt.event = event.into();
                    true
                }
                Some("exit")
                    if attempt.received && attempt.outcome.is_none() && attempt.event == event =>
                {
                    let outcome = row["outcome"].as_str().unwrap_or("");
                    let accepted = match outcome {
                        "returned" | "internal_failure" => {
                            attempt.entered && row["inner_exit"] == 0
                        }
                        "nonzero" => {
                            attempt.entered
                                && row["inner_exit"].as_i64().is_some_and(|exit| exit != 0)
                        }
                        "input_unreadable" | "input_oversize" | "runtime_unavailable" => {
                            !attempt.entered && row["inner_exit"].is_null()
                        }
                        _ => false,
                    } && row["elapsed_us"].as_u64().is_some();
                    if accepted {
                        attempt.outcome = Some(outcome.into());
                    }
                    accepted
                }
                _ => false,
            };
        if !valid {
            attempt.invalid = true;
            invalid_records += 1;
        }
    }
    let mut counts = BTreeMap::<&str, u64>::from([
        ("returned", 0),
        ("handler_nonzero", 0),
        ("handler_internal_failure", 0),
        ("before_handler_failure", 0),
        ("received_without_entry", 0),
        ("entry_without_exit", 0),
        ("invalid_attempts", 0),
    ]);
    let mut handlers = BTreeMap::<&str, BTreeMap<&str, u64>>::new();
    for attempt in attempts.values() {
        let category = if attempt.invalid {
            "invalid_attempts"
        } else {
            match attempt.outcome.as_deref() {
                Some("returned") => "returned",
                Some("nonzero") => "handler_nonzero",
                Some("internal_failure") => "handler_internal_failure",
                Some(_) => "before_handler_failure",
                None if attempt.entered => "entry_without_exit",
                None => "received_without_entry",
            }
        };
        *counts.get_mut(category).unwrap() += 1;
        *handlers
            .entry(&attempt.handler)
            .or_default()
            .entry(category)
            .or_default() += 1;
    }
    let diagnosis = if invalid_records != 0 {
        "incomplete_evidence"
    } else if counts["before_handler_failure"] > 0 {
        "before_handler_failure_observed"
    } else if counts["handler_nonzero"] + counts["handler_internal_failure"] > 0 {
        "handler_failure_observed"
    } else if counts["received_without_entry"] > 0 {
        "receipt_without_handler_entry"
    } else if counts["entry_without_exit"] > 0 {
        "handler_outcome_unconfirmed"
    } else if counts["returned"] > 0 {
        "retained_callbacks_returned"
    } else {
        "no_callback_evidence"
    };
    json!({"status":"read", "coverage":"bounded_retained", "diagnosis":diagnosis,
        "retained_bytes":raw.len(), "read_limit_bytes":TRACE_LIMIT,
        "attempts":attempts.len(), "counts":counts, "handlers":handlers, "invalid_records":invalid_records,
        "active_session":"unknown", "provider_loading":"unverified"})
}

fn read_trace(repo: &Path) -> Option<Vec<u8>> {
    // This is a checkout sidecar, not an account registry. An ordinary shared
    // ancestor such as sticky /tmp must not hide valid private local evidence.
    // Keep the local directory/leaf policy and recheck it around the existing
    // bounded, owned, single-link, no-follow repository reader.
    let uid = rustix::process::getuid().as_raw();
    let snapshot = || {
        let metadata = [
            fs::symlink_metadata(repo).ok()?,
            fs::symlink_metadata(repo.join(".oms")).ok()?,
            fs::symlink_metadata(repo.join(".oms/hooks")).ok()?,
            fs::symlink_metadata(repo.join(TRACE)).ok()?,
        ];
        if metadata[..3].iter().any(|info| {
            !info.is_dir() || ![0, uid].contains(&info.uid()) || info.mode() & 0o022 != 0
        }) || !metadata[3].is_file()
            || metadata[3].uid() != uid
            || metadata[3].nlink() != 1
            || metadata[3].mode() & 0o7077 != 0
        {
            return None;
        }
        Some(metadata.map(|info| {
            (
                info.dev(),
                info.ino(),
                info.uid(),
                info.mode(),
                info.nlink(),
            )
        }))
    };
    let before = snapshot()?;
    let raw = Root::open(repo)
        .ok()?
        .read(TRACE, TRACE_LIMIT, Policy::manifest())
        .ok()?;
    (snapshot()? == before).then_some(raw)
}

fn trace(paths: &Paths, expected: &Value) -> Value {
    // Match the callback writer's command-checkout selection, including subdirectories.
    let repo = paths
        .cwd
        .ancestors()
        .find(|path| {
            fs::symlink_metadata(path.join(".git"))
                .is_ok_and(|meta| meta.is_file() || meta.is_dir())
        })
        .unwrap_or(&paths.cwd);
    let missing = || {
        json!({"status":"missing", "diagnosis":"no_callback_evidence",
        "coverage":"unknown", "active_session":"unknown", "provider_loading":"unverified"})
    };
    if matches!(fs::symlink_metadata(repo.join(TRACE)), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return missing();
    }
    let raw = read_trace(repo);
    match raw {
        Some(raw) => trace_summary(&raw, expected),
        None => json!({"status":"unavailable", "diagnosis":"trace_unavailable",
            "coverage":"unknown", "active_session":"unknown", "provider_loading":"unverified"}),
    }
}

fn codex_configuration(paths: &Paths, expected: &Value) -> Value {
    let config = paths
        .codex_config()
        .and_then(|path| crate::config::read_toml(&path));
    let enabled = match config {
        Ok(value) => {
            match value["plugins"]["oh-my-setting@oh-my-setting-local"]["enabled"].as_bool() {
                Some(true) => "declared_enabled",
                Some(false) => "declared_disabled",
                None => "not_declared",
            }
        }
        Err(_) => "unavailable",
    };
    let cached = crate::plugins::cache(paths).and_then(|path| r::value(&path.join("hooks.json")));
    let cache = match cached {
        Ok(value) if &value == expected => "matches_expected",
        Ok(_) => "drifted",
        Err(_) => "unavailable",
    };
    json!({"plugin_configuration":enabled, "generated_hooks":cache,
        "selected_account":"current_environment", "native_inventory":"not_probed",
        "runtime_loading":"unverified"})
}

pub(crate) fn append(paths: &Paths, findings: &mut Vec<Value>) -> Result<()> {
    let registration = findings
        .iter()
        .find_map(|row| {
            if row["section"] != "hook surfaces" {
                return None;
            }
            match row["message"].as_str()? {
                "PreToolUse main-write-guard-hook: registered" => Some("registered"),
                "PreToolUse main-write-guard-hook: configuration unavailable" => {
                    Some("unavailable")
                }
                "PreToolUse main-write-guard-hook: missing or drifted" => {
                    Some("missing_or_drifted")
                }
                _ => None,
            }
        })
        .unwrap_or("unavailable");
    let disabled = std::env::var_os("OMS_MAIN_WRITE_GUARD").is_some_and(|value| value == "0");
    findings.push(json!({"section":"main write guard", "level":"info", "provider":"claude",
        "registration":registration,
        "file_tool_coverage":["Edit","Write","MultiEdit","NotebookEdit"],
        "scope":"panel_main_managed_checkout_linked_worktrees_and_submodules", "shell_write_enforcement":"unsupported",
        "disabled_in_current_environment":disabled, "effective_running_main":"unknown",
        "message":format!("Claude Main file-edit guard: {}; shell writes are advisory; running Main coverage unverified", if disabled {"disabled in this environment"} else {registration})}));
    for provider in ["codex", "antigravity", "pi"] {
        findings.push(json!({"section":"main write guard", "level":"info", "provider":provider,
            "registration":"unsupported", "file_tool_coverage":[], "shell_write_enforcement":"unsupported",
            "effective_running_main":"unknown",
            "message":format!("{provider} Main edit policy: advisory; no OMS Main-specific write guard")}));
    }
    let expected = crate::plugins::codex_hooks(paths)?;
    let trace = trace(paths, &expected);
    let diagnosis = trace["diagnosis"]
        .as_str()
        .unwrap_or("trace_unavailable")
        .to_owned();
    let failures = trace["handlers"]
        .as_object()
        .into_iter()
        .flat_map(|handlers| handlers.iter())
        .flat_map(|(handler, counts)| {
            counts.as_object().into_iter().flat_map(move |counts| {
                counts
                    .iter()
                    .filter(|(category, _)| *category != "returned")
                    .map(move |(category, count)| {
                        format!("{handler} {category}={}", count.as_u64().unwrap_or(0))
                    })
            })
        })
        .collect::<Vec<_>>()
        .join(", ");
    let configuration = codex_configuration(paths, &expected);
    findings.push(json!({"section":"codex hook diagnostics", "level":"info",
        "configuration":configuration, "trace":trace,
        "message":format!("Codex hooks: plugin {}; generated hooks {}; diagnosis {diagnosis}{}; retained evidence cannot prove current account/session delivery. Missing receipts may mean disabled/unavailable tracing, no callback, or provider loading failure.", configuration["plugin_configuration"].as_str().unwrap(), configuration["generated_hooks"].as_str().unwrap(), if failures.is_empty() {String::new()} else {format!(" ({failures})")})}));
    Ok(())
}
