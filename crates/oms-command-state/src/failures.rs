use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, epoch, error, json, json_output, parse_timestamp,
    require, storage, success, timestamp,
};
use regex::Regex;
use std::collections::BTreeSet;
use std::ffi::OsString;

const STORE: &str = ".oms/failures.jsonl";
pub(crate) const HELP: &str = "Usage: oms fail-ledger [--repo PATH] record --cmd CMD --exit N [--kind K] [--summary TEXT] [--next TEXT]\n       oms fail-ledger [--repo PATH] check --cmd CMD [--ignore-state]\n       oms fail-ledger [--repo PATH] resolve (--fingerprint FP | --cmd CMD) [--how TEXT]\n       oms fail-ledger [--repo PATH] list [--unresolved] [--json] [--limit N]\n       oms fail-ledger [--repo PATH] list --stalls [--json]\n       oms fail-ledger [--repo PATH] show --fingerprint FP [--json]\n\nAppend-only failure memory; unchanged retries exit 3. Hook failures expire on\nread after OMS_HOOK_FAIL_TTL (default 86400s). Reads count invalid rows.\n";

pub fn fingerprint(command: &str) -> String {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = Regex::new(r"\d{8,}")
        .expect("static expression")
        .replace_all(&normalized, "N");
    json::sha(normalized.as_bytes())[..16].into()
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
        || !["fail", "resolved"].contains(&row.text("event"))
        || row.text("fingerprint").trim().is_empty()
    {
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
            && (row.text("event") == "resolved"
                || (!row.text("kind").trim().is_empty()
                    && !row.text("cmd").trim().is_empty()
                    && row.get("exit").int().is_ok_and(|n| n >= 0))))
}

pub fn project(rows: &[Value], invalid: usize, head: &str, now: i64) -> Result<Value> {
    struct Aggregate {
        row: Value,
        count: usize,
        expired: usize,
        resolved: bool,
        how: Value,
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
        let fp = row.text("fingerprint");
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
                        heads: BTreeSet::new(),
                        last_expired: Value::Null,
                    },
                ));
                found.len() - 1
            }
        };
        let entry = &mut found[index].1;
        if row.text("event") == "resolved" {
            entry.count = 0;
            entry.expired = 0;
            entry.resolved = true;
            entry.how = row.get("how").clone();
            entry.heads.clear();
            entry.last_expired = Value::Null;
        } else {
            entry.resolved = false;
            entry.how = Value::Null;
            if hook_expired(row, now, ttl()) {
                entry.expired += 1;
                entry.last_expired = row.clone();
            } else {
                entry.count += 1;
                entry.row = row.clone();
                let stamp = row
                    .text("state_fingerprint")
                    .split('|')
                    .next()
                    .unwrap_or("");
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
        let old_head = last
            .text("state_fingerprint")
            .split('|')
            .next()
            .unwrap_or("");
        let attention = if entry.resolved || expired {
            "none"
        } else if last.text("kind") == "hook" && entry.count < 2 {
            "retiring"
        } else if entry.heads.len() < 2 && !old_head.is_empty() && old_head != head {
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
        ]);
        for key in ["exit", "ts", "kind", "cmd", "summary", "failure_code"] {
            row.set(key, last.get(key).clone())?;
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
    let path = std::env::var_os("OMS_FAIL_LEDGER")
        .filter(|p| !p.is_empty())
        .map(|p| crate::helpers::absolute(&ctx.cwd, p))
        .unwrap_or_else(|| state.join(STORE));
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
        let row = object([
            ("schema", number(2)),
            ("event", string("resolved")),
            ("ts", string(timestamp(now)?)),
            ("agent", string("agent")),
            ("fingerprint", string(&fp)),
            ("how", string(how)),
        ]);
        storage::append_locked(state, &path, &row)?;
        return Ok((
            0,
            Vec::new(),
            format!("fail-ledger: resolved {fp}\n").into_bytes(),
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
