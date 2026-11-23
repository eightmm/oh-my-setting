use crate::cli::Args;
use crate::lock::FileLock;
use crate::projection::{self, select};
use crate::storage::{self, append_row, event_path, read_rows, write_json_atomic};
use crate::value::{V, add, obj};
use crate::{
    Result, actor, checkpoint, line, now, now_seconds, print_json, reason, safe, stamp_at, uuid,
};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub struct StartRequest {
    pub provider: String,
    pub tool: String,
    pub task_id: String,
    pub run_id: String,
    pub parent_attempt_id: String,
    pub budget: V,
    pub refs: V,
    pub idempotency_key: String,
}
impl Default for StartRequest {
    fn default() -> Self {
        Self {
            provider: "local".into(),
            tool: String::new(),
            task_id: String::new(),
            run_id: String::new(),
            parent_attempt_id: String::new(),
            budget: obj([]),
            refs: obj([]),
            idempotency_key: String::new(),
        }
    }
}

/// Insertion order follows first creation in the ledger, including equal-time
/// list/reconcile ordering. Validation includes unrelated attempts.
pub fn project_attempts(rows: &[V]) -> Result<Vec<V>> {
    let mut projection = projection::Projection::default();
    projection.extend(rows)?;
    Ok(projection.into_ordered_attempts())
}

pub(crate) fn sort_list_attempts(values: &mut [V]) {
    values.sort_by_key(|v| v.get("updated_at").py());
}
pub fn lifecycle_semantics(row: &V) -> V {
    let mut value = row.clone();
    for key in [
        "event_id",
        "ts",
        "seq",
        "from_state",
        "trace_id",
        "span_id",
        "parent_span_id",
        "trace_flags",
    ] {
        if let V::Obj(ref mut items) = value {
            items.remove(key.as_bytes());
        }
    }
    value
}
pub fn existing_idempotent(rows: &[V], candidate: &V) -> Result<Option<V>> {
    let key = candidate.get("idempotency_key");
    if !key.truth() {
        return Ok(None);
    }
    for row in rows {
        if row.get("attempt_id") == candidate.get("attempt_id") && row.get("idempotency_key") == key
        {
            if lifecycle_semantics(row) == lifecycle_semantics(candidate) {
                return Ok(Some(row.clone()));
            }
            return Err(format!(
                "idempotency key already names a different lifecycle event: {}",
                key.py()
            ));
        }
    }
    Ok(None)
}
pub fn new_event(attempt_id: &str, seq: V, event_type: &str, fields: V) -> Result<V> {
    let mut row = obj([
        ("schema", 1usize.into()),
        ("event_id", uuid("aevt_")?.into()),
        ("ts", now().into()),
        ("attempt_id", attempt_id.into()),
        ("seq", seq),
        ("event_type", event_type.into()),
    ]);
    let V::Obj(fields) = fields else {
        return Err("event fields must be an object".into());
    };
    for (key, value) in fields {
        if (!matches!(&value, V::Null | V::Str(_) | V::Obj(_) | V::Arr(_))
            || value.truth()
            || key == b"from_state")
            && let V::Obj(ref mut items) = row
        {
            items.insert(key, value);
        }
    }
    Ok(row)
}
pub fn append_lifecycle(repo: &Path, candidate: V, interrupted: &AtomicBool) -> Result<V> {
    append_lifecycle_checked(repo, candidate, interrupted, |_, _| Ok(()))
}

/// Check the ledger and current attempt under the lifecycle lock before appending.
/// The callback must not reacquire that lock. An error appends no event.
pub fn append_lifecycle_checked<F>(
    repo: &Path,
    candidate: V,
    interrupted: &AtomicBool,
    check: F,
) -> Result<V>
where
    F: FnOnce(&[V], Option<&V>) -> Result<()>,
{
    append_lifecycle_checked_with_projection(repo, candidate, interrupted, |rows, _, current| {
        check(rows, current)
    })
}

/// Check the ledger, its projected attempts, and the current attempt under the
/// lifecycle lock before appending. The callback must not reacquire that lock.
/// An error appends no event.
pub fn append_lifecycle_checked_with_projection<F>(
    repo: &Path,
    candidate: V,
    interrupted: &AtomicBool,
    check: F,
) -> Result<V>
where
    F: FnOnce(&[V], &BTreeMap<String, V>, Option<&V>) -> Result<()>,
{
    append_lifecycle_checked_inner(repo, candidate, interrupted, check, |_| {})
}

#[derive(Clone, Copy)]
pub enum CheckedAppendPhase {
    LockAcquired,
    ProjectionReady,
}

/// Observe preparation inside the canonical append. The observer runs under
/// the lifecycle lock and must not reacquire it or decide admission.
pub fn append_lifecycle_checked_observed(
    repo: &Path,
    candidate: V,
    interrupted: &AtomicBool,
    check: impl FnOnce(&[V], Option<&V>) -> Result<()>,
    observe: impl FnMut(CheckedAppendPhase),
) -> Result<V> {
    append_lifecycle_checked_inner(
        repo,
        candidate,
        interrupted,
        |rows, _, current| check(rows, current),
        observe,
    )
}

fn append_lifecycle_checked_inner(
    repo: &Path,
    mut candidate: V,
    interrupted: &AtomicBool,
    check: impl FnOnce(&[V], &BTreeMap<String, V>, Option<&V>) -> Result<()>,
    mut observe: impl FnMut(CheckedAppendPhase),
) -> Result<V> {
    storage::ensure_oms(repo)?;
    let path = event_path(repo);
    let _lock = FileLock::acquire(&path, interrupted)?;
    observe(CheckedAppendPhase::LockAcquired);
    let rows = read_rows(&path)?;
    let projection = projection::project_values(&rows)?;
    let attempt = candidate.get("attempt_id").py();
    let current = select(&projection, &attempt);
    observe(CheckedAppendPhase::ProjectionReady);
    check(&rows, &projection, current)?;
    if let Some(prior) = existing_idempotent(&rows, &candidate)? {
        return Ok(prior);
    }
    prepare_candidate(&mut candidate, current)?;
    checkpoint(interrupted)?;
    append_row(&path, &mut candidate, false)?;
    Ok(candidate)
}

fn prepare_candidate(candidate: &mut V, current: Option<&V>) -> Result<()> {
    let attempt = candidate.get("attempt_id").py();
    let event_type = candidate.get("event_type").py();
    if event_type == "attempt.created" {
        if current.is_some() {
            return Err(format!("attempt already exists: {attempt}"));
        }
        candidate.set("seq", 1usize);
    } else {
        let current = current.ok_or_else(|| format!("unknown attempt: {attempt}"))?;
        candidate.set("seq", V::Num(add(&current.get("sequence").py(), "1")));
        if event_type == "attempt.state_changed" {
            let old = current.get("state");
            if candidate.default("expected_state", old.clone()) != *old {
                return Err(format!(
                    "expected lifecycle state {}, found {}",
                    candidate.get("expected_state").py(),
                    old.py()
                ));
            }
            let sequence = current.get("sequence");
            if candidate.default("expected_seq", sequence.clone()) != *sequence {
                return Err(format!(
                    "expected lifecycle sequence {}, found {}",
                    candidate.get("expected_seq").py(),
                    sequence.py()
                ));
            }
            candidate.set("from_state", old.clone());
            check_transition(old, candidate.get("to_state"))?;
        }
    }
    projection::validate(candidate, 0)?;
    Ok(())
}

fn append_lifecycle_batch(
    repo: &Path,
    candidates: Vec<V>,
    interrupted: &AtomicBool,
) -> Result<Vec<V>> {
    storage::ensure_oms(repo)?;
    let path = event_path(repo);
    let _lock = FileLock::acquire(&path, interrupted)?;
    let mut ledger = storage::Ledger::open(&path, true, false)?;
    let mut history = storage::parse_rows(&path, &ledger.read()?)?;
    let original = history.len();
    let mut projection = projection::Projection::default();
    projection.extend(&history)?;
    let mut batch = Vec::new();
    let mut results = Vec::new();
    for mut candidate in candidates {
        checkpoint(interrupted)?;
        if let Some(prior) = existing_idempotent(&history, &candidate)? {
            results.push(prior);
            continue;
        }
        let attempt = candidate.get("attempt_id").py();
        prepare_candidate(&mut candidate, select(&projection.attempts, &attempt))?;
        projection.extend(std::slice::from_ref(&candidate))?;
        history.push(candidate.clone());
        results.push(candidate.clone());
        batch.push(candidate);
    }
    checkpoint(interrupted)?;
    ledger.append_batch(&mut batch, &history[..original])?;
    Ok(results)
}
fn check_transition(old: &V, new: &V) -> Result<()> {
    if projection::terminal(old.text().unwrap_or(""))
        || !projection::transition(old.text().unwrap_or(""), new.text().unwrap_or(""))
    {
        Err(format!(
            "illegal lifecycle transition: {} -> {}",
            old.py(),
            new.py()
        ))
    } else {
        Ok(())
    }
}
pub fn attempt_spec_path(repo: &Path, attempt: &str) -> PathBuf {
    repo.join(".oms/lifecycle/attempts")
        .join(attempt)
        .join("spec.json")
}
pub fn attempt_spec_from_created(row: &V) -> V {
    let mut spec = obj([
        ("schema", 1usize.into()),
        ("attempt_id", row.get("attempt_id").clone()),
        ("budget", row.default("budget", obj([]))),
        ("refs", row.default("refs", obj([]))),
        ("created_at", row.get("ts").clone()),
    ]);
    for key in [
        "run_id",
        "task_id",
        "parent_attempt_id",
        "provider",
        "tool",
        "base_sha",
    ] {
        spec.set(key, row.default(key, ""));
    }
    spec
}
fn start_semantics(row: &V, include_run: bool) -> V {
    let mut value = obj([]);
    for key in ["provider", "tool", "task_id", "parent_attempt_id"] {
        value.set(key, row.default(key, ""));
    }
    value.set("budget", row.default("budget", obj([])));
    value.set("refs", row.default("refs", obj([])));
    if include_run {
        value.set("run_id", row.default("run_id", ""));
    }
    value
}
pub fn effective_run_id(repo: &Path) -> Result<String> {
    if let Ok(explicit) = std::env::var("OMS_RUN_ID")
        && !explicit.is_empty()
    {
        return safe(&explicit, "OMS_RUN_ID", false);
    }
    if let Ok(bytes) = storage::read_bytes(&repo.join(".oms/runs/CURRENT")) {
        let text = String::from_utf8_lossy(&bytes);
        let parts: Vec<_> = text.split_whitespace().collect();
        let ttl = std::env::var("OMS_RUN_CURRENT_TTL")
            .unwrap_or_else(|_| "86400".into())
            .parse::<i64>();
        if parts.len() >= 2
            && parts[1].bytes().all(|b| b.is_ascii_digit())
            && let (Ok(start), Ok(ttl)) = (parts[1].parse::<f64>(), ttl)
            && now_seconds() - start <= ttl as f64
            && let Ok(id) = safe(parts[0], "current run id", false)
        {
            return Ok(id);
        }
    }
    uuid("run_")
}
pub fn create_attempt(
    repo: &Path,
    request: &StartRequest,
    interrupted: &AtomicBool,
) -> Result<String> {
    storage::ensure_oms(repo)?;
    let provider = safe(
        if request.provider.is_empty() {
            "local"
        } else {
            &request.provider
        },
        "provider",
        false,
    )?;
    let tool = safe(&request.tool, "tool", false)?;
    let task = safe(&request.task_id, "task_id", true)?;
    let parent = safe(&request.parent_attempt_id, "parent_attempt_id", true)?;
    let requested_run = safe(&request.run_id, "run_id", true)?;
    let key = safe(&request.idempotency_key, "idempotency_key", true)?;
    let supplied_budget = if request.budget.truth() {
        request.budget.clone()
    } else {
        obj([])
    };
    let V::Obj(budget) = &supplied_budget else {
        return Err("attempt budget must be an object".into());
    };
    let mut clean_budget = obj([]);
    for (key, value) in budget {
        if value == &V::Null {
            continue;
        }
        let value = crate::value::integer_value(value)?;
        if ![
            b"max_wall_seconds".as_slice(),
            b"max_tokens",
            b"max_cost_microusd",
        ]
        .contains(&key.as_slice())
            || !value.integer(false)
        {
            return Err(format!(
                "invalid attempt budget: {}",
                String::from_utf8_lossy(key)
            ));
        }
        if let V::Obj(ref mut values) = clean_budget {
            values.insert(key.clone(), value);
        }
    }
    let mut requested = obj([
        ("provider", provider.clone().into()),
        ("tool", tool.clone().into()),
        ("task_id", task.clone().into()),
        ("parent_attempt_id", parent.clone().into()),
        ("budget", clean_budget),
        (
            "refs",
            projection::dictionary(&if request.refs.truth() {
                request.refs.clone()
            } else {
                obj([])
            })?,
        ),
    ]);
    if !requested_run.is_empty() {
        requested.set("run_id", requested_run.clone());
    }
    let path = event_path(repo);
    let _lock = FileLock::acquire(&path, interrupted)?;
    let rows = read_rows(&path)?;
    projection::project_values(&rows)?;
    if !key.is_empty() {
        let matches: Vec<_> = rows
            .iter()
            .filter(|r| {
                r.get("event_type").text() == Some("attempt.created")
                    && r.get("idempotency_key").text() == Some(&key)
            })
            .collect();
        if matches.len() > 1 {
            return Err(format!(
                "start idempotency key names multiple attempts: {key}"
            ));
        }
        if let Some(prior) = matches.first() {
            if start_semantics(prior, !requested_run.is_empty()) != requested {
                return Err(format!(
                    "idempotency key already names a different start request: {key}"
                ));
            }
            let spec = attempt_spec_from_created(prior);
            let id = prior.get("attempt_id").py();
            let path = attempt_spec_path(repo, &id);
            if path.exists() {
                let stored = storage::read_bytes(&path)
                    .and_then(|b| V::parse(&b))
                    .map_err(|_| format!("attempt has no valid durable spec: {id}"))?;
                let V::Obj(expected) = &spec else {
                    unreachable!()
                };
                if !matches!(stored, V::Obj(_))
                    || expected.iter().any(|(key, value)| {
                        stored.get(std::str::from_utf8(key).unwrap_or("")) != value
                    })
                {
                    return Err(format!(
                        "attempt spec does not match its creation event: {id}"
                    ));
                }
            } else {
                write_json_atomic(&path, &spec, 0o600)?;
            }
            return Ok(id);
        }
    }
    let run = if requested_run.is_empty() {
        effective_run_id(repo)?
    } else {
        requested_run
    };
    let attempt = uuid("att_")?;
    let mut fields = requested;
    fields.set("run_id", run);
    fields.set("base_sha", storage::git_head(repo));
    fields.set("from_state", V::Null);
    fields.set("to_state", "queued");
    fields.set("actor", actor("owner", "agent-events")?);
    fields.set("idempotency_key", key);
    let mut row = new_event(&attempt, 1usize.into(), "attempt.created", fields)?;
    projection::validate(&row, 0)?;
    checkpoint(interrupted)?;
    append_row(&path, &mut row, false)?;
    write_json_atomic(
        &attempt_spec_path(repo, &attempt),
        &attempt_spec_from_created(&row),
        0o600,
    )?;
    Ok(attempt)
}
fn refs(args: &Args) -> Result<V> {
    let mut items = BTreeMap::new();
    for raw in args.all("ref") {
        let (name, value) = raw
            .split_once('=')
            .ok_or("--ref requires a unique NAME=VALUE")?;
        if name.is_empty()
            || items
                .insert(name.as_bytes().to_vec(), V::from(value))
                .is_some()
        {
            return Err("--ref requires a unique NAME=VALUE".into());
        }
    }
    Ok(V::Obj(items))
}
fn then(spec: &str) -> Result<(String, String)> {
    let (state, key) = spec.split_once(':').unwrap_or((spec, ""));
    if !projection::state(state) {
        return Err(format!("invalid lifecycle state: {state}"));
    }
    Ok((state.into(), key.into()))
}
fn transition_event(attempt: &str, state: &str, key: &str, args: &Args, first: bool) -> Result<V> {
    let mut fields = obj([
        ("from_state", V::Null),
        ("to_state", state.into()),
        ("reason_code", reason(&args.string("reason-code"))?.into()),
        (
            "actor",
            actor(&args.string("actor-kind"), &args.string("actor"))?,
        ),
        (
            "idempotency_key",
            safe(key, "idempotency_key", true)?.into(),
        ),
        ("refs", refs(args)?),
    ]);
    if first && args.flag("expected-state") {
        fields.set("expected_state", args.string("expected-state"));
    }
    if first && args.flag("expected-seq") {
        fields.set("expected_seq", args.integer("expected-seq")?);
    }
    new_event(attempt, 0usize.into(), "attempt.state_changed", fields)
}

pub(crate) fn execute(
    repo: &Path,
    args: &Args,
    interrupted: &AtomicBool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    match args.command.as_str() {
        "start" => {
            let mut budget = obj([]);
            for (option, name) in [
                ("max-wall-seconds", "max_wall_seconds"),
                ("max-tokens", "max_tokens"),
                ("max-cost-microusd", "max_cost_microusd"),
            ] {
                if args.flag(option) {
                    budget.set(name, args.integer(option)?);
                }
            }
            let request = StartRequest {
                provider: args.string("provider"),
                tool: args.string("tool"),
                task_id: args.string("task-id"),
                run_id: args.string("run-id"),
                parent_attempt_id: args.string("parent-attempt-id"),
                budget,
                refs: refs(args)?,
                idempotency_key: args.string("idempotency-key"),
            };
            let attempt = create_attempt(repo, &request, interrupted)?;
            let mut candidates = Vec::new();
            for spec in args.all("then") {
                let (state, key) = then(&spec)?;
                let row = new_event(
                    &attempt,
                    0usize.into(),
                    "attempt.state_changed",
                    obj([
                        ("from_state", V::Null),
                        ("to_state", state.into()),
                        (
                            "actor",
                            actor(&args.string("then-actor-kind"), &args.string("then-actor"))?,
                        ),
                        (
                            "idempotency_key",
                            safe(&key, "idempotency_key", true)?.into(),
                        ),
                    ]),
                )?;
                candidates.push(row);
            }
            if !candidates.is_empty() {
                append_lifecycle_batch(repo, candidates, interrupted)?;
            }
            line(out, attempt)
        }
        "transition" => {
            let attempt = safe(&args.string("attempt"), "attempt", false)?;
            if args.flag("expected-seq") && !args.integer("expected-seq")?.integer(true) {
                return Err("expected sequence must be positive".into());
            }
            let mut steps = vec![(args.string("state"), args.string("idempotency-key"))];
            for spec in args.all("then") {
                steps.push(then(&spec)?);
            }
            for (state, _) in &steps {
                if !projection::state(state) {
                    return Err(format!("invalid lifecycle state: {state}"));
                }
            }
            let candidates = steps
                .iter()
                .enumerate()
                .map(|(index, (state, key))| {
                    transition_event(&attempt, state, key, args, index == 0)
                })
                .collect::<Result<Vec<_>>>()?;
            for row in append_lifecycle_batch(repo, candidates, interrupted)? {
                line(out, row.get("event_id").py())?;
            }
            Ok(())
        }
        "recover-verify-heartbeat" => recover(repo, args, interrupted, out),
        "heartbeat" | "usage" => {
            let attempt = safe(&args.string("attempt"), "attempt", false)?;
            let heartbeat = args.command == "heartbeat";
            let mut fields = obj([
                (
                    "actor",
                    actor(
                        if heartbeat { "runner" } else { "telemetry" },
                        &args.string("actor"),
                    )?,
                ),
                (
                    "idempotency_key",
                    safe(&args.string("idempotency-key"), "idempotency_key", true)?.into(),
                ),
            ]);
            if !heartbeat {
                let mut usage = obj([]);
                for (option, name) in [
                    ("tokens", "tokens"),
                    ("duration-ms", "duration_ms"),
                    ("cost-microusd", "cost_microusd"),
                ] {
                    if args.flag(option) {
                        let value = args.integer(option)?;
                        if !value.integer(false) {
                            return Err("usage values must be non-negative integers".into());
                        }
                        usage.set(name, value);
                    }
                }
                let refs = refs(args)?;
                if !usage.truth() && !refs.truth() {
                    return Err("usage requires at least one measured value or ref".into());
                }
                fields.set("usage", usage);
                fields.set("refs", refs);
            }
            let row = new_event(
                &attempt,
                0usize.into(),
                if heartbeat {
                    "attempt.heartbeat"
                } else {
                    "attempt.usage"
                },
                fields,
            )?;
            line(
                out,
                append_lifecycle(repo, row, interrupted)?
                    .get("event_id")
                    .py(),
            )
        }
        "show" => {
            let values = project_attempts(&read_rows(&event_path(repo))?)?;
            let id = safe(&args.string("attempt"), "attempt", false)?;
            let value = values
                .iter()
                .find(|v| v.get("attempt_id").text() == Some(&id))
                .ok_or_else(|| format!("unknown attempt: {id}"))?;
            if !args.string("transcript").is_empty() && !args.flag("bundle") {
                return Err("--transcript needs --bundle".into());
            }
            if args.flag("bundle") {
                return Err("bundle selector was not owned by the admitted bundle router".into());
            }
            if args.flag("json") {
                return print_json(out, value);
            }
            line(
                out,
                format!(
                    "attempt: {}\nstate: {}\nprovider/tool: {}/{}\nrun/task: {}/{}\nusage: tokens={} duration_ms={} cost_microusd={}",
                    id,
                    value.get("state").py(),
                    value.get("provider").or("-").py(),
                    value.get("tool").or("-").py(),
                    value.get("run_id").or("-").py(),
                    value.get("task_id").or("-").py(),
                    value.get("usage").get("tokens").py(),
                    value.get("usage").get("duration_ms").py(),
                    value.get("usage").get("cost_microusd").py()
                ),
            )
        }
        "list" => {
            let mut values = project_attempts(&read_rows(&event_path(repo))?)?;
            sort_list_attempts(&mut values);
            values.retain(|v| {
                (!args.flag("active") || !v.get("terminal").truth())
                    && (!args.flag("state") || v.get("state").text() == Some(&args.string("state")))
            });
            let limit = args.number("limit")?;
            if limit > 0 {
                values = values.split_off(values.len().saturating_sub(limit as usize));
            } else if limit < 0 {
                values = values.split_off(limit.unsigned_abs().min(values.len() as u64) as usize);
            }
            if args.flag("json") {
                return print_json(out, &V::Arr(values));
            }
            if values.is_empty() {
                return line(out, "no attempts");
            }
            for value in values {
                line(
                    out,
                    format!(
                        "{:<38} {:<18} {:<12} {:<16} {}",
                        value.get("attempt_id").py(),
                        value.get("state").py(),
                        value.get("provider").or("-").py(),
                        value.get("tool").or("-").py(),
                        value.get("updated_at").py()
                    ),
                )?;
            }
            Ok(())
        }
        "resume" => {
            let values = project_attempts(&read_rows(&event_path(repo))?)?;
            let id = safe(&args.string("attempt"), "attempt", false)?;
            let parent = values
                .iter()
                .find(|v| v.get("attempt_id").text() == Some(&id))
                .ok_or_else(|| format!("unknown attempt: {id}"))?;
            if !parent.get("terminal").truth() && parent.get("state").text() != Some("blocked") {
                return Err(
                    "attempt is still live; attach or wait instead of duplicating it".into(),
                );
            }
            let spec = storage::read_bytes(&attempt_spec_path(repo, &id))
                .and_then(|b| V::parse(&b))
                .map_err(|_| format!("attempt has no valid durable spec: {id}"))?;
            if !matches!(spec, V::Obj(_)) {
                return Err(format!("attempt has no valid durable spec: {id}"));
            }
            if spec.get("base_sha").truth()
                && spec.get("base_sha").text() != Some(&storage::git_head(repo))
            {
                return Err(
                    "repository HEAD changed; the prior attempt cannot be resumed safely".into(),
                );
            }
            line(
                out,
                create_attempt(
                    repo,
                    &StartRequest {
                        provider: spec_string(&spec, "provider", "local")?,
                        tool: spec_string(&spec, "tool", "agent-call")?,
                        task_id: spec_string(&spec, "task_id", "")?,
                        run_id: spec_string(&spec, "run_id", "")?,
                        parent_attempt_id: id,
                        budget: spec.default("budget", obj([])),
                        refs: spec.default("refs", obj([])),
                        idempotency_key: String::new(),
                    },
                    interrupted,
                )?,
            )
        }
        "follow" => follow(repo, args, interrupted, out),
        "reconcile" => reconcile(repo, args, interrupted, out),
        "validate" => {
            let rows = read_rows(&event_path(repo))?;
            project_attempts(&rows)?;
            line(out, format!("lifecycle: ok ({} events)", rows.len()))
        }
        "compact" => compact(repo, args, interrupted, out),
        _ => Err("unknown lifecycle command".into()),
    }
}
fn recover(
    repo: &Path,
    args: &Args,
    interrupted: &AtomicBool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let attempt = safe(&args.string("attempt"), "attempt", false)?;
    let expected = args.integer("expected-sequence")?;
    if !expected.integer(true) {
        return Err("expected sequence must be positive".into());
    }
    let path = event_path(repo);
    let _lock = FileLock::acquire(&path, interrupted)?;
    let mut ledger = storage::Ledger::open(&path, true, false)?;
    let rows = storage::parse_rows(&path, &ledger.read()?)?;
    let projection = projection::project_values(&rows)?;
    let mut current = select(&projection, &attempt)
        .cloned()
        .ok_or("attempt is no longer the expected heartbeat-expired generation")?;
    if current.get("sequence") != &expected
        || current.get("state").text() != Some("blocked")
        || current.get("reason_code").text() != Some("heartbeat_expired")
    {
        return Err("attempt is no longer the expected heartbeat-expired generation".into());
    }
    let mut batch = Vec::new();
    for state in ["working", "verifying", "review"] {
        checkpoint(interrupted)?;
        let row = new_event(
            &attempt,
            V::Num(add(&current.get("sequence").py(), "1")),
            "attempt.state_changed",
            obj([
                ("from_state", current.get("state").clone()),
                ("to_state", state.into()),
                ("actor", actor("owner", "peer-delegate")?),
                (
                    "idempotency_key",
                    format!("verify-heartbeat-{state}-{}", expected.py()).into(),
                ),
            ]),
        )?;
        check_transition(current.get("state"), row.get("to_state"))?;
        projection::validate(&row, 0)?;
        current.set("state", state);
        current.set("sequence", row.get("seq").clone());
        batch.push(row);
    }
    checkpoint(interrupted)?;
    ledger.append_batch(&mut batch, &rows)?;
    line(out, attempt)
}
fn follow(
    repo: &Path,
    args: &Args,
    interrupted: &AtomicBool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let after = args.string("after-event");
    let timeout = args.float("timeout")?;
    let interval = args.float("interval")?;
    let limit = args.number("limit")?;
    let started = Instant::now();
    let mut tail = storage::FollowTail::default();
    let mut initialized = false;
    let mut emitted = 0i64;
    let mut seen = after.is_empty();
    loop {
        checkpoint(interrupted)?;
        let (reset, rows) = tail.read(&event_path(repo))?;
        if reset {
            initialized = false;
        }
        let mut start = 0;
        if !after.is_empty() && !initialized {
            start = rows.len();
            for (index, (row, _)) in rows.iter().enumerate() {
                if row.get("event_id").text() == Some(&after) {
                    start = index + 1;
                    seen = true;
                    break;
                }
            }
            if !seen {
                return Err(format!("after-event was not found: {after}"));
            }
        }
        for (_, ordered) in rows.iter().skip(start) {
            out.write_all(&crate::value::ordered_json(ordered)?)
                .and_then(|_| out.write_all(b"\n"))
                .and_then(|_| out.flush())
                .map_err(|e| e.to_string())?;
            emitted += 1;
            if limit != 0 && emitted >= limit {
                return Ok(());
            }
        }
        initialized = true;
        if timeout == 0.0 || timeout >= 0.0 && started.elapsed().as_secs_f64() >= timeout {
            return Ok(());
        }
        if !interval.is_finite() || interval < 0.0 {
            return Err("sleep length must be non-negative and finite".into());
        }
        let mut remaining = interval;
        while remaining > 0.0 {
            checkpoint(interrupted)?;
            let sleep = remaining.min(0.05);
            std::thread::sleep(Duration::from_secs_f64(sleep));
            remaining -= sleep;
        }
    }
}
fn reconcile(
    repo: &Path,
    args: &Args,
    interrupted: &AtomicBool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let seconds = args.number("stale-seconds")?;
    if seconds < 0 {
        return Err("stale-seconds must be non-negative".into());
    }
    let path = event_path(repo);
    let _lock = FileLock::acquire(&path, interrupted)?;
    let mut rows = read_rows(&path)?;
    let attempts = project_attempts(&rows)?;
    let now = now_seconds();
    let mut stale = Vec::new();
    for value in attempts {
        if !matches!(
            value.get("state").text(),
            Some("starting" | "working" | "verifying")
        ) {
            continue;
        }
        let age = (now
            - projection::stamp(value.get("last_heartbeat_at"))
                .ok_or("invalid heartbeat timestamp")? as f64) as i64;
        if age < seconds {
            continue;
        }
        let item = obj([
            ("attempt_id", value.get("attempt_id").clone()),
            ("state", value.get("state").clone()),
            ("age_seconds", age.into()),
        ]);
        if args.flag("apply") {
            checkpoint(interrupted)?;
            let mut row = new_event(
                &value.get("attempt_id").py(),
                V::Num(add(&value.get("sequence").py(), "1")),
                "attempt.state_changed",
                obj([
                    ("from_state", value.get("state").clone()),
                    ("to_state", "blocked".into()),
                    ("reason_code", "heartbeat_expired".into()),
                    ("actor", actor("reconciler", "agent-events")?),
                    (
                        "idempotency_key",
                        format!("reconcile-stale-{}", value.get("sequence").py()).into(),
                    ),
                ]),
            )?;
            if existing_idempotent(&rows, &row)?.is_some() {
                return Err(format!(
                    "reconcile idempotency key already exists for this stale generation: {}",
                    row.get("idempotency_key").py()
                ));
            }
            projection::validate(&row, 0)?;
            append_row(&path, &mut row, false)?;
            rows.push(row);
        }
        stale.push(item);
    }
    if args.flag("json") {
        return print_json(out, &V::Arr(stale));
    }
    if stale.is_empty() {
        return line(out, "no stale attempts");
    }
    for item in stale {
        line(
            out,
            format!(
                "{} {} age={}s{}",
                item.get("attempt_id").py(),
                item.get("state").py(),
                item.get("age_seconds").py(),
                if args.flag("apply") {
                    " -> blocked"
                } else {
                    ""
                }
            ),
        )?;
    }
    Ok(())
}
fn compact(
    repo: &Path,
    args: &Args,
    interrupted: &AtomicBool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let path = event_path(repo);
    if !path.exists() {
        return line(out, "lifecycle-compact: no events file");
    }
    let days = args.number("days")?;
    if days < 0 {
        return Err("--days must be non-negative".into());
    }
    let cutoff = stamp_at(
        (now_seconds() as i64)
            .checked_sub(days.checked_mul(86400).ok_or("date value out of range")?)
            .ok_or("date value out of range")?,
    );
    if cutoff.is_empty() {
        return Err("date value out of range".into());
    }
    let _lock = if args.flag("apply") {
        Some(FileLock::acquire(&path, interrupted)?)
    } else {
        None
    };
    let bytes = storage::read_bytes(&path)?;
    let rows = storage::parse_rows(&path, &bytes)?;
    let attempts = project_attempts(&rows)?;
    let mut drop: HashSet<_> = attempts
        .iter()
        .filter(|v| v.get("terminal").truth() && v.get("updated_at").py() < cutoff)
        .map(|v| v.get("attempt_id").py())
        .collect();
    let mut pending: Vec<_> = attempts
        .iter()
        .filter(|v| !drop.contains(&v.get("attempt_id").py()))
        .map(|v| v.get("parent_attempt_id").py())
        .collect();
    while let Some(parent) = pending.pop() {
        if drop.remove(&parent)
            && let Some(value) = attempts.iter().find(|v| v.get("attempt_id").py() == parent)
        {
            pending.push(value.get("parent_attempt_id").py());
        }
    }
    let text = String::from_utf8_lossy(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let raws: Vec<_> = text
        .split_inclusive('\n')
        .filter(|line| {
            !line
                .chars()
                .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        })
        .collect();
    let mut kept = Vec::new();
    let mut kept_rows = Vec::new();
    for (row, raw) in rows.iter().zip(raws) {
        if drop.contains(&row.get("attempt_id").py()) {
            continue;
        }
        kept.extend_from_slice(raw.as_bytes());
        if !raw.ends_with('\n') {
            kept.push(b'\n');
        }
        kept_rows.push(row.clone());
    }
    let removed = rows.len() - kept_rows.len();
    let kept_ids: HashSet<String> = kept_rows.iter().map(|r| r.get("attempt_id").py()).collect();
    let stale_dirs = eligible_attempt_dirs(repo, &kept_ids)?;
    if !args.flag("apply") {
        let dirs = stale_dirs.len();
        return line(
            out,
            format!(
                "lifecycle-compact: would drop {removed} event(s) across {} terminal attempt(s) older than {days}d (keeping {}); {dirs} attempt dir(s) to remove",
                drop.len(),
                kept_rows.len()
            ),
        );
    }
    if drop.is_empty() && stale_dirs.is_empty() {
        return line(
            out,
            format!("lifecycle-compact: nothing to drop ({} events)", rows.len()),
        );
    }
    project_attempts(&kept_rows)?;
    checkpoint(interrupted)?;
    if !drop.is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o777;
        storage::atomic(&path, &kept, mode, false)?;
    }
    let mut dirs = 0;
    for dir in &stale_dirs {
        checkpoint(interrupted)?;
        dirs += usize::from(remove_attempt_dir(repo, dir)?);
    }
    line(
        out,
        format!(
            "lifecycle-compact: dropped {removed} event(s) across {} terminal attempt(s) older than {days}d (kept {}); removed {dirs} attempt dir(s)",
            drop.len(),
            kept_rows.len()
        ),
    )
}

/// Attempt directories whose id no retained row references: this run's dropped
/// attempts and strays from an interrupted earlier compaction. A directory is
/// eligible only when every ancestor is a real directory and it holds at most
/// 64 regular files; anything else is left untouched.
fn eligible_attempt_dirs(repo: &Path, kept: &HashSet<String>) -> Result<Vec<PathBuf>> {
    let mut root = repo.to_path_buf();
    for part in [".oms", "lifecycle", "attempts"] {
        root.push(part);
        match std::fs::symlink_metadata(&root) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Ok(Vec::new()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        }
    }
    let mut found = Vec::new();
    for (seen, entry) in std::fs::read_dir(&root)
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if seen >= 50000 {
            return Err("attempt directory scan limit exceeded".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if kept.contains(&id) || !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let mut files = 0;
        let mut plain = true;
        for child in std::fs::read_dir(entry.path()).map_err(|e| e.to_string())? {
            let child = child.map_err(|e| e.to_string())?;
            files += 1;
            plain &= files <= 64 && child.file_type().map_err(|e| e.to_string())?.is_file();
            if !plain {
                break;
            }
        }
        if plain {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}
/// Caller holds the ledger lock. Opens `.oms/lifecycle/attempts/<id>` using
/// no-follow directory descriptors, inspects regular files up to 64 entries
/// before unlinking anything via `unlinkat`, and removes the emptied dir.
pub(crate) fn remove_attempt_dir(repo: &Path, dir: &Path) -> Result<bool> {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat, unlinkat};
    use rustix::io::Errno;
    let Some(id) = dir.file_name().and_then(|n| n.to_str()) else {
        return Ok(false);
    };
    if dir != repo.join(".oms/lifecycle/attempts").join(id) {
        return Ok(false);
    }
    let mut cur = match open(
        repo,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    for part in [".oms", "lifecycle", "attempts"] {
        cur = match openat(
            &cur,
            part,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => return Ok(false),
            Err(e) => return Err(e.to_string()),
        };
    }
    let before = match statat(&cur, id, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(s) => s,
        Err(Errno::NOENT) => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
        return Ok(false);
    }
    let dir_fd = match openat(
        &cur,
        id,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    let opened = fstat(&dir_fd).map_err(|e| e.to_string())?;
    if opened.st_dev != before.st_dev || opened.st_ino != before.st_ino {
        return Ok(false);
    }
    let mut children = Vec::new();
    for entry in Dir::read_from(&dir_fd).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if [b".".as_slice(), b"..".as_slice()].contains(&name.to_bytes()) {
            continue;
        }
        if children.len() >= 64 {
            return Ok(false);
        }
        let child_stat = match statat(&dir_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) => s,
            Err(Errno::NOENT) => continue,
            Err(e) => return Err(e.to_string()),
        };
        if FileType::from_raw_mode(child_stat.st_mode) != FileType::RegularFile {
            return Ok(false);
        }
        children.push((name.to_owned(), child_stat));
    }
    for (name, prev) in children {
        let current = match statat(&dir_fd, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) => s,
            Err(Errno::NOENT) => continue,
            Err(e) => return Err(e.to_string()),
        };
        if current.st_dev != prev.st_dev
            || current.st_ino != prev.st_ino
            || FileType::from_raw_mode(current.st_mode) != FileType::RegularFile
        {
            return Err("attempt file changed before removal".into());
        }
        match unlinkat(&dir_fd, &name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    drop(dir_fd);
    match unlinkat(&cur, id, AtFlags::REMOVEDIR) {
        Ok(()) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

/// Cold authoritative read. No untrusted cache can substitute for ledger
/// validation; the optional legacy performance cache is intentionally ignored.
pub fn load_projection(repo: &Path) -> Result<(Vec<V>, Vec<V>)> {
    let rows = read_rows(&event_path(repo))?;
    let attempts = project_attempts(&rows)?;
    Ok((rows, attempts))
}
pub fn validate_event_row(row: &V, number: usize) -> Result<()> {
    projection::validate(row, number)
}

fn spec_string(spec: &V, key: &str, fallback: &str) -> Result<String> {
    let value = spec.default(key, fallback);
    if !value.truth() {
        return Ok(String::new());
    }
    value
        .text()
        .map(str::to_owned)
        .ok_or_else(|| format!("attempt spec has invalid {key}"))
}
