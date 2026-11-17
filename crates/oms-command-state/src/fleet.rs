//! Read-only projections of the multi-host/parallel runtime files (targets, execution snapshots,
//! remote operations, Slurm submissions, plan contracts, late results).
//!
//! Collectors never create directories, files, locks or reconciliation records, never follow
//! symlinks, and turn corrupt input into a degraded section (`healthy: false` plus bounded
//! `problems`) instead of an error. Stored command output and host paths are never copied.
use super::*;
use crate::json::{number, object, string};
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;

const DIR_LIMIT: usize = 10_000;
const FILE_LIMIT: usize = 1024 * 1024;
const PROBLEMS: usize = 8;
const RECENT: usize = 5;
const PROVENANCE: [&str; 3] = ["task_id", "attempt_id", "exec_snapshot_sha256"];

pub(super) const SECTIONS: [&str; 6] = [
    "remote_targets",
    "exec_snapshots",
    "remote_operations",
    "slurm_submissions",
    "plan_contracts",
    "late_results",
];

#[derive(Default)]
struct Section {
    present: bool,
    total: usize,
    invalid: usize,
    problems: Vec<String>,
    omitted: usize,
}
impl Section {
    fn problem(&mut self, message: impl Into<String>) {
        self.invalid += 1;
        if self.problems.len() < PROBLEMS {
            self.problems.push(message.into());
        } else {
            self.omitted += 1;
        }
    }
    fn value(&self) -> Value {
        object([
            ("healthy", Value::Bool(self.invalid == 0)),
            ("present", Value::Bool(self.present)),
            ("total", number(self.total)),
            ("invalid", number(self.invalid)),
            (
                "problems",
                json::list(self.problems.iter().map(String::as_str)),
            ),
            ("problems_omitted", number(self.omitted)),
        ])
    }
    fn into_value(self, extra: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        let mut value = self.value();
        for (k, v) in extra {
            let _ = value.set(k, v);
        }
        value
    }
}

fn clean(raw: &str, max: usize) -> String {
    raw.chars().filter(|c| !c.is_control()).take(max).collect()
}
fn field(row: &Value, key: &str, max: usize) -> Value {
    row.get(key)
        .str()
        .map(|s| string(clean(s, max)))
        .unwrap_or(Value::Null)
}
fn scalar(row: &Value, key: &str) -> Value {
    match row.get(key) {
        v @ Value::Number(_) => v.clone(),
        _ => Value::Null,
    }
}

/// `Err` names the first symlink or non-directory on the path below the repository.
fn plain_ancestors(ctx: &Context<'_>, rel: &str) -> std::result::Result<(), String> {
    let mut current = ctx.repo.clone();
    let parts: Vec<_> = rel.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(m) if m.is_symlink() => return Err(format!("{rel} traverses a symlink")),
            Ok(m) if i + 1 < parts.len() && !m.is_dir() => {
                return Err(format!("{rel} traverses a non-directory"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{rel} is unreadable: {e}")),
        }
    }
    Ok(())
}

/// Parsed regular file, or `None` for a missing file; failures become a section problem.
fn read(ctx: &Context<'_>, section: &mut Section, rel: &str) -> Option<Value> {
    if let Err(e) = plain_ancestors(ctx, rel) {
        section.present = true;
        section.problem(e);
        return None;
    }
    let path = ctx.repo.join(rel);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            section.present = true;
            section.problem(format!("{rel} is unreadable: {e}"));
            return None;
        }
    };
    section.present = true;
    if !meta.is_file() {
        section.problem(format!("{rel} is not a regular file"));
        return None;
    }
    match storage::read(&ctx.repo, &path, FILE_LIMIT.max(storage::MAX_FILE), false)
        .and_then(|raw| json::parse(&raw))
    {
        Ok(v) => Some(v),
        Err(_) => {
            section.problem(format!("{rel} is unreadable or not valid JSON"));
            None
        }
    }
}

fn directory(ctx: &Context<'_>, section: &mut Section, rel: &str) -> Vec<(PathBuf, bool)> {
    if let Err(e) = plain_ancestors(ctx, rel) {
        section.present = true;
        section.problem(e);
        return vec![];
    }
    match helpers::shallow_entries(ctx, &ctx.repo.join(rel), ".json", DIR_LIMIT) {
        Ok(entries) => {
            section.present = ctx.repo.join(rel).is_dir();
            entries
                .into_iter()
                .map(|(p, kind)| (p, kind.is_file()))
                .collect()
        }
        Err(e) => {
            section.present = true;
            section.problem(format!("{rel} cannot be scanned: {e}"));
            vec![]
        }
    }
}
fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
fn file_row(
    ctx: &Context<'_>,
    section: &mut Section,
    path: &Path,
    regular: bool,
    private: bool,
) -> Option<Value> {
    section.total += 1;
    let name = clean(&path.file_name().unwrap_or_default().to_string_lossy(), 80);
    if !regular {
        section.problem(format!("{name} is a symlink or not a regular file"));
        return None;
    }
    if private && std::fs::symlink_metadata(path).is_ok_and(|m| m.mode() & 0o777 != 0o600) {
        section.problem(format!("{name} must have mode 0600"));
        return None;
    }
    match storage::read(&ctx.repo, path, FILE_LIMIT, false).and_then(|raw| json::parse(&raw)) {
        Ok(v) if matches!(v, Value::Object(_)) => Some(v),
        _ => {
            section.problem(format!("{name} is unreadable or not a JSON object"));
            None
        }
    }
}

fn targets(ctx: &Context<'_>) -> Value {
    let mut s = Section::default();
    let (mut approved, mut slurm) = (0, 0);
    let mut rows = Vec::new();
    if let Some(file) = read(ctx, &mut s, ".oms/runtime/targets.json") {
        match (file.get("schema").is_int(1), file.get("targets").object()) {
            (true, Ok(entries)) => {
                for (name, row) in entries {
                    s.total += 1;
                    let name = String::from_utf8_lossy(name).into_owned();
                    let fingerprint = row.text("approved_fingerprint");
                    if !matches!(row, Value::Object(_)) || row.text("ssh_alias").is_empty() {
                        s.problem(format!("target {} is malformed", clean(&name, 64)));
                        continue;
                    }
                    let ok = row.get("execution_approved") == &Value::Bool(true)
                        && json::hex(fingerprint, 64);
                    let scheduler = if row.text("scheduler") == "slurm" {
                        "slurm"
                    } else {
                        "none"
                    };
                    approved += usize::from(ok);
                    slurm += usize::from(scheduler == "slurm");
                    if rows.len() < 16 {
                        rows.push(object([
                            ("name", string(clean(&name, 64))),
                            ("ssh_alias", field(row, "ssh_alias", 64)),
                            ("scheduler", string(scheduler)),
                            ("approved", Value::Bool(ok)),
                        ]));
                    }
                }
            }
            _ => s.problem("targets.json has an unsupported schema or no targets object"),
        }
    }
    let total = s.total;
    s.into_value([
        ("approved", number(approved)),
        ("unapproved", number(total.saturating_sub(approved))),
        ("slurm_targets", number(slurm)),
        ("targets", Value::Array(rows)),
    ])
}

fn snapshots(ctx: &Context<'_>) -> Value {
    let mut s = Section::default();
    for (path, regular) in directory(ctx, &mut s, ".oms/runtime/snapshots") {
        let Some(row) = file_row(ctx, &mut s, &path, regular, false) else {
            continue;
        };
        let name = stem(&path);
        let mut payload = row.clone();
        payload.remove("snapshot_sha256");
        let digest = json::hash(&payload).unwrap_or_default();
        if !(row.get("schema").is_int(1)
            && json::hex(&name, 64)
            && row.text("snapshot_sha256") == name
            && digest == name)
        {
            s.problem(format!(
                "snapshot {} fails schema or digest verification",
                clean(&name, 64)
            ));
        }
    }
    s.into_value([])
}

fn remote_operations(ctx: &Context<'_>) -> Value {
    let mut s = Section::default();
    let mut by_state = BTreeMap::<String, usize>::new();
    let mut unknown_ids = Vec::new();
    let mut recent = Vec::new();
    for (path, regular) in directory(ctx, &mut s, ".oms/runtime/remote-ops") {
        let Some(row) = file_row(ctx, &mut s, &path, regular, true) else {
            continue;
        };
        let id = stem(&path);
        if !row.get("schema").is_int(1) || row.text("op_id") != id {
            s.problem(format!(
                "remote operation {} has a bad schema or mismatched op_id",
                clean(&id, 64)
            ));
            continue;
        }
        let state: String = clean(row.text("state"), 24)
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let state = if state.is_empty() {
            "invalid".into()
        } else {
            state
        };
        if state == "unknown" && unknown_ids.len() < 3 {
            unknown_ids.push(clean(&id, 64));
        }
        *by_state.entry(state.clone()).or_default() += 1;
        recent.push((
            clean(row.text("updated_at"), 40),
            object([
                ("op_id", string(clean(&id, 64))),
                ("state", string(&state)),
                ("action", field(&row, "action", 24)),
                ("target", field(&row, "target", 64)),
                ("started_at", field(&row, "started_at", 40)),
                ("updated_at", field(&row, "updated_at", 40)),
                ("exit_code", scalar(&row, "exit_code")),
            ]),
        ));
    }
    recent.sort_by(|a, b| b.0.cmp(&a.0));
    let count = |k: &str| by_state.get(k).copied().unwrap_or(0);
    let extra = [
        ("running", number(count("running"))),
        ("unknown", number(count("unknown"))),
        ("failed", number(count("failed"))),
        (
            "by_state",
            Value::Object(
                by_state
                    .iter()
                    .map(|(k, v)| (k.clone().into_bytes(), number(*v)))
                    .collect(),
            ),
        ),
        (
            "unknown_ids",
            json::list(unknown_ids.iter().map(String::as_str)),
        ),
        (
            "recent",
            Value::Array(recent.into_iter().take(RECENT).map(|r| r.1).collect()),
        ),
    ];
    s.into_value(extra)
}

fn identity(row: &Value, key: &str) -> Option<String> {
    match row.get(key) {
        Value::String(_) => Some(row.text(key).to_owned()),
        Value::Number(n) => Some(n.clone()),
        _ => None,
    }
}

fn slurm(ctx: &Context<'_>) -> Value {
    #[derive(Default)]
    struct Attempt {
        intent: Option<usize>,
        last: usize,
        result: Option<(String, Value)>,
        reconciled: Option<String>,
        provenance: Vec<(&'static str, String)>,
    }
    let mut s = Section::default();
    let rel = ".oms/slurm-submits.jsonl";
    let mut rows = Vec::new();
    if let Err(e) = plain_ancestors(ctx, rel) {
        s.present = true;
        s.problem(e);
    } else if ctx.repo.join(rel).symlink_metadata().is_ok() {
        s.present = true;
        match storage::rows(&ctx.repo, Path::new(rel)) {
            Ok((r, bad)) => {
                rows = r;
                for _ in 0..bad {
                    s.problem("malformed ledger row");
                }
            }
            Err(_) => s.problem("slurm-submits.jsonl is unreadable"),
        }
    }
    let mut keys = BTreeMap::<String, Vec<(usize, &Value)>>::new();
    for (index, row) in rows.iter().enumerate() {
        let kind = row.text("type");
        match identity(row, "key") {
            Some(key)
                if row.get("schema").is_int(1)
                    && ["intent", "submitted", "failed", "reconciled"].contains(&kind)
                    && (kind == "reconciled" || identity(row, "attempt").is_some())
                    && (kind != "reconciled" || row.get("attempt") != &Value::Null) =>
            {
                keys.entry(key).or_default().push((index, row));
            }
            _ => s.problem(format!("row {} is not a valid ledger row", index + 1)),
        }
    }
    let (mut active, mut unknown, mut failed, mut reconciled) = (0, 0, 0, 0);
    let mut recent = Vec::new();
    s.total = keys.len();
    for entries in keys.values() {
        // A forced intent that did not fail supersedes everything before it, as `ledger::state`.
        let mut start = 0;
        let mut results = BTreeMap::<String, bool>::new();
        for (_, r) in entries {
            if ["submitted", "failed"].contains(&r.text("type"))
                && let Some(a) = identity(r, "attempt")
            {
                results.insert(a, r.text("type") == "failed");
            }
        }
        for (i, (_, r)) in entries.iter().enumerate() {
            if r.text("type") == "intent"
                && r.get("forced").truth()
                && identity(r, "attempt")
                    .and_then(|a| results.get(&a).copied())
                    .is_none_or(|failed| !failed)
            {
                start = i;
            }
        }
        let window: std::collections::BTreeSet<_> = entries[start..]
            .iter()
            .filter(|(_, r)| r.text("type") == "intent")
            .filter_map(|(_, r)| identity(r, "attempt"))
            .collect();
        let mut attempts = BTreeMap::<String, Attempt>::new();
        for (index, r) in entries {
            let Some(id) = identity(r, "attempt") else {
                continue;
            };
            if start > 0 && !window.contains(&id) {
                continue;
            }
            let entry = attempts.entry(id).or_default();
            entry.last = *index;
            for name in PROVENANCE {
                if let Some(v) = identity(r, name) {
                    entry.provenance.retain(|(k, _)| *k != name);
                    entry.provenance.push((name, clean(&v, 96)));
                }
            }
            match r.text("type") {
                "intent" => entry.intent = Some(*index),
                "submitted" | "failed" => {
                    entry.result = Some((r.text("type").into(), (*r).clone()))
                }
                _ => entry.reconciled = Some(clean(r.text("state"), 24)),
            }
        }
        for attempt in attempts.values() {
            let mut state = "unknown";
            match &attempt.result {
                None if attempt.intent.is_some() => unknown += 1,
                None => state = "reconciled",
                Some((kind, _)) if kind == "failed" => {
                    failed += 1;
                    state = "failed";
                }
                Some(_) => {
                    state = "submitted";
                    if attempt
                        .reconciled
                        .as_deref()
                        .is_none_or(|r| ["active", "unknown"].contains(&r))
                    {
                        active += 1;
                    }
                }
            }
            reconciled += usize::from(attempt.reconciled.is_some());
            let (cluster, job) = match &attempt.result {
                Some((_, r)) => (field(r, "cluster_name", 64), field(r, "job_id", 32)),
                None => (Value::Null, Value::Null),
            };
            let mut row = object([
                ("state", string(state)),
                ("cluster", cluster),
                ("job_id", job),
                (
                    "reconciled_state",
                    attempt.reconciled.as_deref().map_or(Value::Null, string),
                ),
            ]);
            for name in PROVENANCE {
                let v = attempt.provenance.iter().find(|(k, _)| *k == name);
                let _ = row.set(name, v.map_or(Value::Null, |(_, v)| string(v)));
            }
            recent.push((attempt.last, row));
        }
    }
    recent.sort_by_key(|r| std::cmp::Reverse(r.0));
    let extra = [
        ("total_keys", number(keys.len())),
        ("active_submitted", number(active)),
        ("unknown", number(unknown)),
        ("failed", number(failed)),
        ("reconciled", number(reconciled)),
        (
            "recent",
            Value::Array(recent.into_iter().take(RECENT).map(|r| r.1).collect()),
        ),
    ];
    s.into_value(extra)
}

fn contracts(ctx: &Context<'_>, state: &Value) -> Value {
    let mut s = Section::default();
    let (mut accepted, mut proposed) = (0, 0);
    let rel = ".oms/plan/contracts.json";
    if std::fs::symlink_metadata(ctx.repo.join(rel))
        .is_ok_and(|m| m.is_file() && m.mode() & 0o777 != 0o600)
    {
        s.present = true;
        s.problem("contracts.json must have mode 0600");
    } else if let Some(file) = read(ctx, &mut s, rel) {
        match (file.get("schema").is_int(1), file.get("contracts").object()) {
            (true, Ok(entries)) => {
                for (key, contract) in entries {
                    s.total += 1;
                    let id = clean(&String::from_utf8_lossy(key), 64);
                    let current = contract.get("accepted_version").int().unwrap_or(-1);
                    let mut newest = 0;
                    let mut sound = current >= 0 && contract.text("id") == id;
                    for (name, version) in contract.get("versions").object().unwrap_or(&[]) {
                        let n = String::from_utf8_lossy(name).parse::<i128>().unwrap_or(0);
                        newest = newest.max(n);
                        let mut payload = version.clone();
                        payload.remove("status");
                        payload.remove("sha256");
                        sound &= n > 0
                            && version.get("version").int() == Ok(n)
                            && ["proposed", "accepted"].contains(&version.text("status"))
                            && json::hash(&payload).is_ok_and(|d| d == version.text("sha256"));
                    }
                    if !sound {
                        s.problem(format!("contract {id} fails version verification"));
                        continue;
                    }
                    accepted += usize::from(current > 0);
                    proposed += usize::from(newest > current);
                }
            }
            _ => s.problem("contracts.json has an unsupported schema or no contracts object"),
        }
    }
    let stale = state
        .get("plan")
        .get("contract_stale_tasks")
        .array()
        .map_or(0, <[_]>::len);
    s.into_value([
        ("accepted", number(accepted)),
        ("proposed_unaccepted", number(proposed)),
        ("stale_task_count", number(stale)),
    ])
}

fn late_results(ctx: &Context<'_>) -> Value {
    let mut s = Section::default();
    let rel = ".oms/plan/late-results.jsonl";
    let mut recent = Vec::new();
    if let Err(e) = plain_ancestors(ctx, rel) {
        s.present = true;
        s.problem(e);
    } else if ctx.repo.join(rel).symlink_metadata().is_ok() {
        s.present = true;
        match storage::rows(&ctx.repo, Path::new(rel)) {
            Ok((rows, bad)) => {
                s.total = rows.len() + bad;
                for _ in 0..bad {
                    s.problem("malformed late-result row");
                }
                for (i, row) in rows.iter().enumerate() {
                    if !row.get("schema").is_int(1) || row.text("task_id").is_empty() {
                        s.problem(format!("late-result row {} is invalid", i + 1));
                        continue;
                    }
                    recent.push(object([
                        ("ts", field(row, "ts", 40)),
                        ("task_id", field(row, "task_id", 64)),
                        ("reason", field(row, "reason", 40)),
                        ("supplied_lease_id", field(row, "supplied_lease_id", 64)),
                        ("current_lease_id", field(row, "current_lease_id", 64)),
                    ]));
                }
            }
            Err(_) => s.problem("late-results.jsonl is unreadable"),
        }
    }
    recent.reverse();
    recent.truncate(RECENT);
    s.into_value([("recent", Value::Array(recent))])
}

/// Adds every section to `state`; `plan` must already be present.
pub(super) fn collect(ctx: &Context<'_>, state: &mut Value) -> Result<()> {
    let projected = [
        ("remote_targets", targets(ctx)),
        ("exec_snapshots", snapshots(ctx)),
        ("remote_operations", remote_operations(ctx)),
        ("slurm_submissions", slurm(ctx)),
        ("plan_contracts", contracts(ctx, state)),
        ("late_results", late_results(ctx)),
    ];
    for (key, value) in projected {
        state.set(key, value)?;
    }
    Ok(())
}

pub(super) fn attention(
    state: &Value,
    anchor: &str,
    add: &mut impl FnMut(&str, &str, usize, String, String),
) {
    let n = |section: &str, key: &str| state.get(section).get(key).int().unwrap_or(0) as usize;
    let ops = n("remote_operations", "unknown");
    let id = state
        .get("remote_operations")
        .get("unknown_ids")
        .array()
        .ok()
        .and_then(|a| a.first())
        .and_then(|v| v.str().ok())
        .unwrap_or("OP_ID");
    add(
        "P1",
        "unknown-remote-operations",
        ops,
        format!("{ops} remote operation(s) ended in unknown state"),
        format!("oms runtime ssh-adapter reconcile --repo {anchor} --op-id {id}"),
    );
    let slurm = n("slurm_submissions", "unknown");
    add(
        "P1",
        "unknown-slurm-submissions",
        slurm,
        format!("{slurm} Slurm submission intent(s) have no recorded result"),
        format!("oms slurm-submit reconcile --repo {anchor} --key KEY"),
    );
    let stale = n("plan_contracts", "stale_task_count");
    add(
        "P2",
        "stale-plan-contracts",
        stale,
        format!("{stale} task(s) reference a stale or unaccepted contract"),
        format!("oms agent-plan --repo {anchor} contract-list"),
    );
    let late = n("late_results", "total");
    add(
        "P2",
        "quarantined-late-results",
        late,
        format!("{late} late result(s) were quarantined and are not completion evidence"),
        format!("oms agent-plan --repo {anchor} status"),
    );
}

pub(super) fn verify(state: &Value, findings: &mut Vec<Value>) {
    for family in SECTIONS {
        let section = state.get(family);
        for problem in section.get("problems").array().unwrap_or(&[]) {
            findings.push(object([
                ("level", string("fail")),
                ("family", string(family)),
                ("message", string(problem.str().unwrap_or(""))),
                (
                    "remedy",
                    string("inspect and repair or remove the named record; see oms state --json"),
                ),
            ]));
        }
        let omitted = section.get("problems_omitted").int().unwrap_or(0);
        if omitted > 0 {
            findings.push(object([
                ("level", string("fail")),
                ("family", string(family)),
                (
                    "message",
                    string(format!("{omitted} further problem(s) omitted")),
                ),
                (
                    "remedy",
                    string("fix the listed records and re-run oms state-verify"),
                ),
            ]));
        }
    }
}

pub(super) fn lines(state: &Value) -> Vec<String> {
    let n = |section: &str, key: &str| state.get(section).get(key).int().unwrap_or(0);
    let mut out = Vec::new();
    for (section, label) in [
        ("remote_targets", "targets"),
        ("exec_snapshots", "exec snapshots"),
        ("remote_operations", "remote ops"),
        ("slurm_submissions", "slurm submits"),
        ("plan_contracts", "contracts"),
        ("late_results", "late results"),
    ] {
        let s = state.get(section);
        if !s.get("present").truth() && s.get("healthy").truth() {
            continue;
        }
        let detail = match section {
            "remote_targets" => format!(
                "{} approved, {} unapproved, {} slurm",
                n(section, "approved"),
                n(section, "unapproved"),
                n(section, "slurm_targets")
            ),
            "remote_operations" => format!(
                "{} running, {} unknown, {} failed",
                n(section, "running"),
                n(section, "unknown"),
                n(section, "failed")
            ),
            "slurm_submissions" => format!(
                "{} active, {} unknown, {} failed, {} reconciled",
                n(section, "active_submitted"),
                n(section, "unknown"),
                n(section, "failed"),
                n(section, "reconciled")
            ),
            "plan_contracts" => format!(
                "{} accepted, {} proposed, {} stale task(s)",
                n(section, "accepted"),
                n(section, "proposed_unaccepted"),
                n(section, "stale_task_count")
            ),
            _ => String::new(),
        };
        out.push(format!(
            "{label}: {} total{}{}{}",
            n(section, "total"),
            if detail.is_empty() { "" } else { ", " },
            detail,
            if s.get("healthy").truth() {
                String::new()
            } else {
                format!(
                    "  DEGRADED invalid={} (run: oms state-verify)",
                    n(section, "invalid")
                )
            }
        ));
    }
    out
}

pub(super) fn dashboard(state: &Value) -> Value {
    let mut fleet = json::empty();
    for family in SECTIONS {
        let mut section = state.get(family).clone();
        if matches!(section, Value::Null) {
            section = json::empty();
        }
        let _ = fleet.set(family, section);
    }
    fleet
}
