use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, epoch, error, json, json_output, parse_timestamp,
    require, storage, success, timestamp,
};
use oms_command_task::services::VerificationEvidence;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const STORE: &str = ".oms/failures.jsonl";
const MAX_PRIOR_FAILURES: usize = 128;
const STALE_AGE_SECS: i64 = 14 * 86400;
pub(crate) const HELP: &str = "Usage: oms fail-ledger [--repo PATH] record --cmd CMD --exit N [--kind K] [--summary TEXT] [--next TEXT]\n       oms fail-ledger [--repo PATH] check --cmd CMD [--ignore-state]\n       oms fail-ledger [--repo PATH] resolve (--fingerprint FP | --cmd CMD) [--how TEXT]\n       oms fail-ledger [--repo PATH] list [--unresolved] [--json] [--limit N]\n       oms fail-ledger [--repo PATH] list --stalls [--json]\n       oms fail-ledger [--repo PATH] show --fingerprint FP [--json]\n\nAppend-only failure memory; unchanged retries exit 3. Hook failures expire on\nread after OMS_HOOK_FAIL_TTL (default 86400s). Reads count invalid rows.\n";

pub fn fingerprint(command: &str) -> String {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = Regex::new(r"\d{8,}")
        .expect("static expression")
        .replace_all(&normalized, "N");
    json::sha(normalized.as_bytes())[..16].into()
}

fn ledger_path(ctx: &Context<'_>, state: &Path) -> PathBuf {
    std::env::var_os("OMS_FAIL_LEDGER")
        .filter(|p| !p.is_empty())
        .map(|p| crate::helpers::absolute(&ctx.cwd, p))
        .unwrap_or_else(|| state.join(STORE))
}

/// A partial receipt has its own subject key. Legacy memory readers otherwise
/// treat any top-level fingerprint on a non-resolved row as a fresh failure.
pub(crate) fn row_fingerprint(row: &Value) -> &str {
    if row.text("event") == "verified" && row.opt("observed_fingerprint").is_some() {
        row.text("observed_fingerprint")
    } else {
        row.text("fingerprint")
    }
}

fn verification(row: &Value) -> Option<&Value> {
    let evidence = row.opt("verification")?;
    let digest =
        |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    let attempt = |key: &str| {
        let value = evidence.text(key);
        !value.is_empty()
            && value.len() <= 160
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
    };
    (row.get("schema").int().ok() == Some(2)
        && evidence.text("scope") == "agent-task-reference-v1"
        && evidence.text("action") == "verification-rerun"
        && !evidence.text("active_task_id").is_empty()
        && ["snapshot_attempt_id", "outcome_attempt_id"]
            .iter()
            .all(|key| attempt(key))
        && ["snapshot_sha256", "outcome_sha256", "command_sha256"]
            .iter()
            .all(|key| digest(evidence.text(key)))
        && (matches!(evidence.get("prior_failures"), Value::Null)
            || evidence.get("prior_failures").array().is_ok_and(|keys| {
                keys.len() <= MAX_PRIOR_FAILURES
                    && keys.iter().all(|key| key.str().is_ok_and(digest))
            }))
        && ((evidence.text("result") == "failed" && row.text("event") == "fail")
            || (evidence.text("result") == "accepted"
                && matches!(row.text("event"), "resolved" | "verified")
                && !evidence.text("verified_state").is_empty())))
    .then_some(evidence)
}

fn complete_row(row: &Value) -> bool {
    valid_row(row) && (row.opt("verification").is_none() || verification(row).is_some())
}

type OpenFailures<'a> = BTreeMap<String, &'a Value>;

fn same_task_command(row: &Value, task_id: &[u8], command_sha256: &str) -> bool {
    verification(row).is_some_and(|failure| {
        failure.text("active_task_id").as_bytes() == task_id
            && failure.text("command_sha256") == command_sha256
    })
}

fn remove_verified(pending: &mut OpenFailures<'_>, evidence: &Value) {
    let Ok(prior) = evidence.get("prior_failures").array() else {
        return;
    };
    for key in prior {
        let key = key.str().unwrap_or("");
        if pending.get(key).is_some_and(|row| {
            same_task_command(
                row,
                evidence.text("active_task_id").as_bytes(),
                evidence.text("command_sha256"),
            )
        }) {
            pending.remove(key);
        }
    }
}

fn open_failures<'a>(rows: &'a [Value], fingerprint: &str) -> Result<OpenFailures<'a>> {
    // Both manual resolutions and fully covered native resolutions close the
    // fingerprint's epoch. Older identities cannot consume the current bound.
    let start = rows
        .iter()
        .rposition(|row| row.text("fingerprint") == fingerprint && row.text("event") == "resolved")
        .map_or(0, |index| index + 1);
    let mut pending = BTreeMap::new();
    for row in rows[start..]
        .iter()
        .filter(|row| row_fingerprint(row) == fingerprint)
    {
        if row.text("event") == "fail" {
            pending.insert(json::hash(row)?, row);
        } else if let Some(evidence) = verification(row) {
            remove_verified(&mut pending, evidence);
        }
    }
    Ok(pending)
}

/// Exact open native failure identities known before a verification rerun.
pub fn task_failure_candidates(
    repo: &Path,
    active_task_id: &[u8],
    command: &[u8],
    interrupted: &AtomicBool,
) -> Result<Vec<String>> {
    crate::checkpoint(interrupted)?;
    let ctx = Context::new(repo, interrupted)?;
    let (rows, bad) = storage::rows(&ctx.repo, &ledger_path(&ctx, &ctx.repo))?;
    require(
        bad == 0 && rows.iter().all(complete_row),
        "failure source is incomplete",
    )?;
    let raw = std::str::from_utf8(command).map_err(|_| error(2, "failure command is not UTF-8"))?;
    let fingerprint = fingerprint(&normalize(raw));
    let command_sha256 = oms_command_task::command_hash(command);
    let candidates: Vec<_> = open_failures(&rows, &fingerprint)?
        .into_iter()
        .filter(|(_, row)| same_task_command(row, active_task_id, &command_sha256))
        .map(|(key, _)| key)
        .take(MAX_PRIOR_FAILURES + 1)
        .collect();
    require(
        candidates.len() <= MAX_PRIOR_FAILURES,
        "failure correlation selection exceeds bound",
    )?;
    Ok(candidates)
}

/// The native observer uses the same parser, lock and append owner as the CLI.
/// There is deliberately no CLI flag for supplying accepted receipt evidence.
pub fn observe_task_verification(
    args: &[OsString],
    repo: &Path,
    interrupted: &AtomicBool,
    evidence: Option<&VerificationEvidence>,
) -> Output {
    let result = (|| {
        crate::checkpoint(interrupted)?;
        run_observed(args, &Context::new(repo, interrupted)?, evidence)
    })();
    result.unwrap_or_else(|e| (e.code, vec![], format!("error: {e}\n").into_bytes()))
}

fn observed_evidence(
    evidence: &VerificationEvidence,
    pending: Option<&OpenFailures<'_>>,
) -> Result<Option<Value>> {
    let (Some(snapshot), Some(outcome), Ok(task_id)) = (
        evidence.snapshot_attempt_id(),
        evidence.outcome_attempt_id(),
        std::str::from_utf8(evidence.active_task_id()),
    ) else {
        return Ok(None);
    };
    let prior = evidence.prior_failures().filter(|prior| {
        prior.len() <= MAX_PRIOR_FAILURES
            && pending.is_some_and(|pending| {
                prior.iter().all(|key| {
                    pending.get(key).is_some_and(|row| {
                        same_task_command(row, evidence.active_task_id(), evidence.command_sha256())
                    })
                })
            })
    });
    let mut value = object([
        ("scope", string("agent-task-reference-v1")),
        ("active_task_id", string(task_id)),
        ("action", string("verification-rerun")),
        ("snapshot_attempt_id", string(snapshot)),
        ("snapshot_sha256", string(evidence.snapshot_sha256())),
        ("outcome_attempt_id", string(outcome)),
        ("outcome_sha256", string(evidence.outcome_sha256())),
        ("command_sha256", string(evidence.command_sha256())),
        (
            "result",
            string(if evidence.verified_state().is_some() {
                "accepted"
            } else {
                "failed"
            }),
        ),
        (
            "prior_failures",
            prior.map_or(Value::Null, |prior| {
                Value::Array(prior.iter().map(string).collect())
            }),
        ),
    ]);
    if let Some(state) = evidence.verified_state() {
        value.set("verified_state", string(state))?;
    }
    require(
        oms_scrub::scan_secret(&json::canonical(&value)?, &[])
            .map_err(|e| error(3, format!("{e:?}")))?
            .is_empty(),
        "verification identity looks sensitive",
    )?;
    Ok(Some(value))
}

/// A native pass may clear the command aggregate only when every currently
/// open row has exact accepted evidence. Partial passes remain observations;
/// a manual/legacy resolve retains its established fingerprint-wide meaning.
fn covers_fingerprint(pending: Option<OpenFailures<'_>>, current: &Value) -> bool {
    let Some(mut pending) = pending else {
        return false;
    };
    if current.get("prior_failures").array().is_err() {
        return false;
    }
    remove_verified(&mut pending, current);
    pending.is_empty()
}

fn normalize(value: &str) -> String {
    let mut text = value.to_owned();
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        text = text.replace(home.to_string_lossy().as_ref(), "~");
    }
    text
}

pub fn hook_expired(row: &Value, now: i64, ttl: i64) -> bool {
    row.text("event") == "fail"
        && row.text("kind") == "hook"
        && parse_timestamp(row.text("ts")).is_some_and(|t| now - t >= ttl)
}

fn ttl() -> i64 {
    std::env::var("OMS_HOOK_FAIL_TTL")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n >= 0)
        .unwrap_or(86400)
}

pub fn valid_row(row: &Value) -> bool {
    let schema = row
        .opt("schema")
        .unwrap_or(&Value::Number("1".into()))
        .int()
        .unwrap_or(0);
    if ![1, 2].contains(&schema)
        || !["fail", "resolved", "verified"].contains(&row.text("event"))
        || row_fingerprint(row).trim().is_empty()
        || (row.opt("observed_fingerprint").is_some()
            && (row.text("event") != "verified" || row.opt("fingerprint").is_some()))
    {
        return false;
    }
    if row.text("event") == "verified" && (schema != 2 || verification(row).is_none()) {
        return false;
    }
    for key in [
        "ts",
        "agent",
        "kind",
        "cmd",
        "summary",
        "next",
        "how",
        "state_fingerprint",
        "failure_code",
        "recovery",
    ] {
        if row.opt(key).is_some_and(|v| v.str().is_err()) {
            return false;
        }
    }
    for key in ["exit", "count"] {
        if row
            .opt(key)
            .is_some_and(|v| v.int().is_err() || v.int().is_ok_and(|n| n < 0))
        {
            return false;
        }
    }
    if row
        .opt("resolved")
        .is_some_and(|v| !matches!(v, Value::Bool(_)))
    {
        return false;
    }
    schema == 1
        || (!row.text("ts").trim().is_empty()
            && !row.text("agent").trim().is_empty()
            && (matches!(row.text("event"), "resolved" | "verified")
                || (!row.text("kind").trim().is_empty()
                    && !row.text("cmd").trim().is_empty()
                    && row.get("exit").int().is_ok_and(|n| n >= 0))))
}

fn head_stamp(row: &Value) -> &str {
    row.text("state_fingerprint")
        .split(['|', ':'])
        .next()
        .unwrap_or("")
}

pub fn project(rows: &[Value], invalid: usize, head: &str, now: i64) -> Result<Value> {
    struct Aggregate {
        row: Value,
        count: usize,
        expired: usize,
        resolved: bool,
        how: Value,
        resolution_verification: Value,
        heads: BTreeSet<String>,
        last_expired: Value,
    }
    let mut found: Vec<(String, Aggregate)> = Vec::new();
    let mut bad = invalid;
    for row in rows {
        if !valid_row(row) {
            bad += 1;
            continue;
        }
        let fp = row_fingerprint(row);
        let index = match found.iter().position(|(key, _)| key == fp) {
            Some(i) => i,
            None => {
                found.push((
                    fp.into(),
                    Aggregate {
                        row: Value::Null,
                        count: 0,
                        expired: 0,
                        resolved: false,
                        how: Value::Null,
                        resolution_verification: Value::Null,
                        heads: BTreeSet::new(),
                        last_expired: Value::Null,
                    },
                ));
                found.len() - 1
            }
        };
        let entry = &mut found[index].1;
        if row.text("event") == "verified" {
            // A receipt racing a completed resolution still belongs in history.
            // Its proof must not replace that resolution's own provenance.
            if !entry.resolved {
                entry.resolution_verification = verification(row).cloned().unwrap_or(Value::Null);
            }
            continue;
        }
        if row.text("event") == "resolved" {
            entry.count = 0;
            entry.expired = 0;
            entry.resolved = true;
            entry.how = row.get("how").clone();
            entry.resolution_verification = verification(row).cloned().unwrap_or(Value::Null);
            entry.heads.clear();
            entry.last_expired = Value::Null;
        } else {
            entry.resolved = false;
            entry.how = Value::Null;
            entry.resolution_verification = Value::Null;
            if hook_expired(row, now, ttl()) {
                entry.expired += 1;
                entry.last_expired = row.clone();
            } else {
                entry.count += 1;
                entry.row = row.clone();
                let stamp = head_stamp(row);
                entry.heads.insert(if stamp.is_empty() {
                    format!("row:{}", entry.count)
                } else {
                    stamp.into()
                });
            }
        }
    }
    let mut output = Vec::new();
    for (fp, entry) in found {
        let last = if matches!(entry.row, Value::Null) {
            &entry.last_expired
        } else {
            &entry.row
        };
        let expired = !entry.resolved && entry.count == 0 && entry.expired > 0;
        let all_stamped =
            !entry.heads.is_empty() && entry.heads.iter().all(|h| !h.starts_with("row:"));
        let non_current = !head.is_empty() && all_stamped && !entry.heads.contains(head);
        let aged = parse_timestamp(last.text("ts")).is_some_and(|t| now - t >= STALE_AGE_SECS);
        let legacy_read_guard = last.text("failure_code") == "read_guard_changed";
        let attention = if entry.resolved || expired {
            "none"
        } else if last.text("kind") == "hook" && entry.count < 2 {
            "retiring"
        } else if non_current && (entry.heads.len() < 2 || aged || legacy_read_guard) {
            "stale"
        } else {
            "actionable"
        };
        let mut row = object([
            ("fingerprint", string(fp)),
            ("resolved", Value::Bool(entry.resolved)),
            ("expired", Value::Bool(expired)),
            ("count", number(entry.count)),
            ("attention", string(attention)),
            ("actionable", Value::Bool(attention == "actionable")),
            ("retiring", Value::Bool(attention == "retiring")),
            ("non_current_head", Value::Bool(non_current)),
        ]);
        for key in ["exit", "ts", "kind", "cmd", "summary", "failure_code"] {
            row.set(key, last.get(key).clone())?;
        }
        if let Some(evidence) = verification(last) {
            row.set("failure_sha256", string(json::hash(last)?))?;
            row.set("verification", evidence.clone())?;
        }
        if !matches!(entry.resolution_verification, Value::Null) {
            row.set("resolution_verification", entry.resolution_verification)?;
        }
        if entry.expired > 0 {
            row.set("expired_count", number(entry.expired))?;
        }
        if last.get("next").truth() {
            row.set("next", last.get("next").clone())?;
        }
        if entry.how.truth() {
            row.set("how", entry.how)?;
        }
        output.push(row);
    }
    Ok(object([
        ("schema", number(1)),
        ("failures", Value::Array(output)),
        ("invalid_rows", number(bad)),
    ]))
}

pub(crate) fn run(args: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    run_observed(args, ctx, None)
}

fn run_observed(
    args: &[OsString],
    ctx: &Context<'_>,
    evidence: Option<&VerificationEvidence>,
) -> Result<Output> {
    let native_observation = evidence.is_some();
    let opts = Options::parse(
        args,
        &[
            "repo",
            "cmd",
            "exit",
            "kind",
            "summary",
            "next",
            "failure-code",
            "outbound-evidence",
            "fingerprint",
            "how",
            "limit",
        ],
        &["unresolved", "stalls", "json", "ignore-state"],
    )?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    require(opts.pos.len() <= 1, "expected one fail-ledger action")?;
    let action = opts.pos.first().map(String::as_str).unwrap_or("list");
    require(
        ["record", "check", "resolve", "list", "show"].contains(&action),
        "unknown fail-ledger action",
    )?;
    require(
        !opts.flag("json") || ["list", "show"].contains(&action),
        "--json is only supported by list and show",
    )?;
    let limit = opts.number("limit", 0)?;
    require(
        limit == 0 || action == "list",
        "--limit is only supported by list",
    )?;
    require(
        !opts.flag("ignore-state") || action == "check",
        "--ignore-state is only supported by check",
    )?;
    require(
        !opts.flag("stalls") || (action == "list" && !opts.flag("unresolved") && limit == 0),
        "--stalls does not combine with --unresolved or --limit",
    )?;
    let repo = opts.repo(ctx)?;
    let state = if opts.get("repo").is_empty() {
        &ctx.state_repo
    } else {
        &repo
    };
    let path = ledger_path(ctx, state);
    let cmd = normalize(opts.get("cmd"));
    let fp = if opts.get("fingerprint").is_empty() {
        fingerprint(&cmd)
    } else {
        opts.get("fingerprint").to_owned()
    };
    let _guard = if ["record", "resolve"].contains(&action) {
        Some(storage::lock(&path)?)
    } else {
        None
    };
    let (rows, bad) = storage::rows(state, &path)?;
    if let Some(evidence) = evidence {
        require(
            oms_command_task::command_hash(opts.get("cmd").as_bytes()) == evidence.command_sha256()
                && ((action == "resolve") == evidence.verified_state().is_some()),
            "verification evidence does not match this observation",
        )?;
    }
    let pending = evidence
        .filter(|_| bad == 0 && rows.iter().all(complete_row))
        .map(|_| open_failures(&rows, &fp))
        .transpose()?;
    let evidence = evidence
        .map(|evidence| observed_evidence(evidence, pending.as_ref()))
        .transpose()?
        .flatten();
    let now = epoch();
    let head = oms_command_operations::storage::git_head(&repo);
    let mut report = project(&rows, bad, &head, now)?;
    if action == "record" {
        require(!cmd.trim().is_empty(), "record requires nonblank --cmd")?;
        let exit: u64 = opts
            .get("exit")
            .parse()
            .map_err(|_| error(2, "record requires --exit N (non-negative integer)"))?;
        let kind = if opts.get("kind").is_empty() {
            "cmd"
        } else {
            opts.get("kind")
        };
        require(
            [
                "cmd",
                "hook",
                "verify",
                "plan-run",
                "delegate",
                "patch-land",
            ]
            .contains(&kind),
            "invalid --kind",
        )?;
        let summary = normalize(opts.get("summary"));
        let next = normalize(opts.get("next"));
        for value in [&cmd, &summary, &next] {
            if !oms_scrub::scan_secret(value.as_bytes(), &[])
                .map_err(|e| error(3, format!("{e:?}")))?
                .is_empty()
            {
                return Err(error(
                    3,
                    "fail-ledger: refused; command or note looks sensitive",
                ));
            }
        }
        let classification = oms_failure::classify(
            &format!("{cmd} {summary}"),
            i64::try_from(exit).ok(),
            opts.get("failure-code"),
        );
        let state_fp = oms_command_task::state_fingerprint(&repo, ctx.interrupted)
            .map_err(|e| error(e.code, e))?;
        let mut row = object([
            ("schema", number(2)),
            ("event", string("fail")),
            ("ts", string(timestamp(now)?)),
            (
                "agent",
                string(std::env::var("OMS_AGENT").unwrap_or_else(|_| "agent".into())),
            ),
            ("fingerprint", string(&fp)),
            ("kind", string(kind)),
            ("cmd", string(cmd)),
            ("exit", number(exit)),
            ("state_fingerprint", string(state_fp)),
            ("failure_code", string(classification.code)),
            ("recovery", string(classification.properties.recovery)),
        ]);
        if !summary.is_empty() {
            row.set("summary", string(summary))?;
        }
        if !next.is_empty() {
            row.set("next", string(next))?;
        }
        if let Some(evidence) = evidence {
            row.set("verification", evidence)?;
        }
        storage::append_locked(state, &path, &row)?;
        return Ok((
            0,
            Vec::new(),
            format!("fail-ledger: recorded {fp} (exit {exit})\n").into_bytes(),
        ));
    }
    if action == "resolve" {
        require(
            !opts.get("fingerprint").is_empty() || !cmd.is_empty(),
            "resolve requires --fingerprint or --cmd",
        )?;
        require(
            rows.iter().any(|r| r.text("fingerprint") == fp),
            "unknown fingerprint",
        )?;
        let how = normalize(opts.get("how"));
        if !oms_scrub::scan_secret(how.as_bytes(), &[])
            .map_err(|e| error(3, format!("{e:?}")))?
            .is_empty()
        {
            return Err(error(3, "--how looks sensitive"));
        }
        let resolves = if native_observation {
            let evidence = evidence.as_ref().ok_or_else(|| {
                error(
                    2,
                    "native verification identity unavailable; fingerprint was not resolved",
                )
            })?;
            covers_fingerprint(pending, evidence)
        } else {
            true
        };
        let event = if resolves { "resolved" } else { "verified" };
        let mut row = object([
            ("schema", number(2)),
            ("event", string(event)),
            ("ts", string(timestamp(now)?)),
            ("agent", string("agent")),
            (
                if resolves {
                    "fingerprint"
                } else {
                    "observed_fingerprint"
                },
                string(&fp),
            ),
            ("how", string(how)),
        ]);
        if let Some(evidence) = evidence {
            row.set("verification", evidence)?;
        }
        storage::append_locked(state, &path, &row)?;
        return Ok((
            0,
            Vec::new(),
            format!("fail-ledger: {event} {fp}\n").into_bytes(),
        ));
    }
    if action == "check" {
        require(!cmd.is_empty(), "check requires --cmd")?;
        let mut last = None;
        let mut count = 0;
        for row in &rows {
            if row.text("fingerprint") != fp {
                continue;
            }
            if row.text("event") == "resolved" {
                last = None;
                count = 0;
            } else if row.text("event") == "fail" && !hook_expired(row, now, ttl()) {
                last = Some(row);
                count += 1;
            }
        }
        if let Some(last) = last {
            let state_fp = oms_command_task::state_fingerprint(&repo, ctx.interrupted)
                .map_err(|e| error(e.code, e))?;
            if !opts.flag("ignore-state")
                && !last.text("state_fingerprint").is_empty()
                && last.text("state_fingerprint") != state_fp
            {
                return Ok((
                    0,
                    Vec::new(),
                    format!(
                        "fail-ledger: {fp} failed before, but git state changed; retry allowed\n"
                    )
                    .into_bytes(),
                ));
            }
            return Ok((
                3,
                Vec::new(),
                format!(
                    "fail-ledger: {fp} already failed {count}x (last exit {}): {}\n",
                    last.get("exit").int().unwrap_or(0),
                    last.text("summary")
                )
                .into_bytes(),
            ));
        }
        return Ok(success(""));
    }
    let mut selected = report.get("failures").array()?.to_vec();
    if action == "show" {
        require(
            !opts.get("fingerprint").is_empty(),
            "show requires --fingerprint",
        )?;
        let mut row = selected
            .into_iter()
            .find(|r| r.text("fingerprint") == fp)
            .ok_or_else(|| error(1, "unknown fingerprint"))?;
        for key in ["next", "how"] {
            if !oms_scrub::scan_secret(row.text(key).as_bytes(), &[])
                .map_err(|e| error(2, format!("{e:?}")))?
                .is_empty()
            {
                row.set(key, string("(withheld: looks like a credential)"))?;
            }
        }
        if opts.flag("json") {
            return json_output(
                0,
                &object([
                    ("schema", number(1)),
                    ("failure", row),
                    ("invalid_rows", report.get("invalid_rows").clone()),
                ]),
            );
        }
        let mut text = String::new();
        for (key, value) in row.object()? {
            text.push_str(&format!(
                "{}: {}\n",
                String::from_utf8_lossy(key),
                display(value)
            ));
        }
        return Ok(success(text));
    }
    if opts.flag("stalls") {
        return stalls(&rows, &opts);
    }
    if opts.flag("unresolved") {
        selected.retain(|r| !r.get("resolved").truth() && r.get("count").int().unwrap_or(0) > 0);
    }
    if limit > 0 && selected.len() > limit {
        let omitted = selected.len() - limit;
        let mut newest: Vec<_> = selected.iter().collect();
        newest.sort_by_key(|r| std::cmp::Reverse(r.text("ts")));
        let ids: BTreeSet<_> = newest[..limit]
            .iter()
            .map(|r| r.text("fingerprint").to_owned())
            .collect();
        selected.retain(|r| ids.contains(r.text("fingerprint")));
        report.set("omitted", number(omitted))?;
    }
    report.set("failures", Value::Array(selected.clone()))?;
    if opts.flag("json") {
        return json_output(0, &report);
    }
    let mut text = String::new();
    for row in selected {
        let tag = if row.get("resolved").truth() {
            "resolved"
        } else if row.get("expired").truth() {
            "EXPIRED"
        } else if row.text("attention") == "stale" {
            "stale"
        } else {
            "OPEN"
        };
        text.push_str(&format!(
            "{} {tag:8} count={} exit={} {}\n",
            row.text("fingerprint"),
            row.get("count").int()?,
            display(row.get("exit")),
            if row.text("summary").is_empty() {
                row.text("cmd")
            } else {
                row.text("summary")
            }
        ));
        if row.get("next").truth() {
            text.push_str(&format!("  next: {}\n", row.text("next")));
        }
    }
    if text.is_empty() {
        text.push_str("no failures\n");
    }
    Ok(success(text))
}

fn display(v: &Value) -> String {
    v.str().map(str::to_owned).unwrap_or_else(|_| {
        String::from_utf8_lossy(&json::canonical(v).unwrap_or_default()).into_owned()
    })
}

pub(crate) fn stall_findings(rows: &[Value]) -> Vec<Value> {
    let newest: Vec<_> = rows.iter().rev().take(20).collect();
    let mut findings = Vec::new();
    if let Some(last) = newest.first() {
        let count = newest
            .iter()
            .take_while(|r| {
                r.text("fingerprint") == last.text("fingerprint")
                    && r.text("event") == last.text("event")
            })
            .count();
        let limit = std::env::var("OMS_STALL_ERROR_REPEAT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        if last.text("event") == "fail" && limit > 0 && count >= limit {
            findings.push(object([
                ("rule", string("error-repeat")),
                ("count", number(count)),
                ("stream", string("cmd")),
                ("key", string(last.text("cmd"))),
                ("result", last.get("exit").clone()),
            ]));
        }
    }
    findings
}

fn stalls(rows: &[Value], opts: &Options) -> Result<Output> {
    let findings = stall_findings(rows);
    if opts.flag("json") {
        return json_output(
            0,
            &object([("schema", number(1)), ("findings", Value::Array(findings))]),
        );
    }
    let text = findings
        .iter()
        .map(|r| {
            format!(
                "{}: {}x {}\n",
                r.text("rule"),
                r.get("count").int().unwrap_or(0),
                r.text("key")
            )
        })
        .collect::<String>();
    Ok(success(if text.is_empty() {
        "no stalls\n".into()
    } else {
        text
    }))
}
