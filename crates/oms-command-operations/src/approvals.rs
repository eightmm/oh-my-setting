use crate::cli::Args;
use crate::lock::FileLock;
use crate::projection::{self, identity};
use crate::storage::{self, append_row, approval_path, read_rows};
use crate::value::{V, add, obj};
use crate::{Result, checkpoint, digest, line, now, now_seconds, print_json, safe, stamp_at, uuid};
use std::collections::{BTreeMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::{OnceLock, atomic::AtomicBool};

pub(crate) fn state(s: &str) -> bool {
    matches!(
        s,
        "requested"
            | "approved"
            | "denied"
            | "expired"
            | "consuming"
            | "consumed"
            | "failed"
            | "interrupted"
    )
}
fn bounded(value: &str, label: &str, limit: usize) -> Result<String> {
    if value.is_empty() || value.chars().count() > limit || value.contains(['\r', '\n', '\t']) {
        return Err(format!("{label} must be one line of 1-{limit} characters"));
    }
    static SENSITIVE: OnceLock<regex::Regex> = OnceLock::new();
    let expression = SENSITIVE.get_or_init(|| {
        regex::Regex::new(concat!(
            r"(?i)(api[_-]?key|access[_-]?token|secret|password|private[_-]?key)\s*[:",
            "=",
            r"]|-----BE",
            r"GIN [A-Z ]+PRIVATE ",
            r"KEY-----|(^|\s)(/Us",
            r"ers/|/ho",
            r"me/|[A-Za-z]:[\\/])"
        ))
        .expect("constant bounded text regex")
    });
    if expression.is_match(value) {
        return Err(format!(
            "{label} contains sensitive-looking content or an absolute machine path"
        ));
    }
    Ok(value.into())
}
pub fn parse_scalar_map(raw: &str, label: &str) -> Result<V> {
    if raw.is_empty() {
        return Ok(obj([]));
    }
    let value = V::parse(raw.as_bytes()).map_err(|_| format!("{label} is not valid JSON"))?;
    scalar_map(&value, label)?;
    Ok(value)
}
fn scalar_map(value: &V, label: &str) -> Result<()> {
    let V::Obj(items) = value else {
        return Err(format!("{label} must be an object with at most 32 fields"));
    };
    if items.len() > 32 {
        return Err(format!("{label} must be an object with at most 32 fields"));
    }
    for (key, value) in items {
        safe(&V::Str(key.clone()).py(), &format!("{label} key"), false)?;
        match value {
            V::Str(_) => {
                bounded(&value.py(), &format!("{label} value"), 160)?;
            }
            V::Num(n)
                if n.contains(['.', 'e', 'E'])
                    || matches!(n.as_str(), "NaN" | "Infinity" | "-Infinity") =>
            {
                if !n.parse::<f64>().is_ok_and(f64::is_finite) {
                    return Err(format!("{label} values must be finite"));
                }
            }
            V::Null | V::Bool(_) | V::Num(_) => (),
            _ => return Err(format!("{label} values must be bounded scalars")),
        }
    }
    Ok(())
}
fn repo_hash(repo: &Path) -> Result<String> {
    Ok(digest(storage::resolved(repo)?.as_os_str().as_bytes()))
}
pub fn approval_action_digest(repo: &Path, values: &V) -> Result<String> {
    let V::Obj(values) = values else {
        return Err("approval values must be an object".into());
    };
    let mut canonical = obj([("repo_hash", repo_hash(repo)?.into())]);
    if let V::Obj(ref mut fields) = canonical {
        fields.extend(values.clone());
    }
    Ok(digest(&canonical.json(false)?))
}
pub fn approval_projection(rows: &[V], repo: &Path) -> Result<Vec<V>> {
    let hash = repo_hash(repo)?;
    let mut projected = Vec::<V>::new();
    let mut indices = BTreeMap::new();
    let mut events = HashSet::new();
    for (index, row) in rows.iter().enumerate() {
        let number = index + 1;
        if identity(row.get("schema")) != "n:1" {
            return Err(format!("approval row {number} has unsupported schema"));
        }
        let event = row.default("event_id", "");
        safe(&event.py(), "approval event_id", false)?;
        if !events.insert(identity(&event)) {
            return Err(format!("duplicate approval event_id: {}", event.py()));
        }
        let id = row.default("approval_id", "");
        safe(&id.py(), "approval_id", false)?;
        if projection::stamp(row.get("ts")).is_none() {
            return Err(format!(
                "invalid UTC timestamp: {}",
                row.default("ts", "").py()
            ));
        }
        let version = row.get("version");
        if !version.integer(true) {
            return Err(format!("approval row {number} has invalid version"));
        }
        let state = row.get("state");
        if !state.text().is_some_and(self::state) {
            return Err(format!("approval row {number} has invalid state"));
        }
        if ["trace_id", "span_id", "parent_span_id", "trace_flags"]
            .iter()
            .any(|s| row.has(s))
            && !storage::trace_valid(row)
        {
            return Err(format!(
                "approval row {number} has invalid persisted trace context"
            ));
        }
        if ["traceparent", "tracestate", "baggage"]
            .iter()
            .any(|s| row.has(s))
        {
            return Err(format!(
                "approval row {number} stores forbidden raw trace context"
            ));
        }
        let event_type = row.get("event_type").text().unwrap_or("");
        let key = identity(&id);
        let at = indices.get(&key).copied();
        if event_type == "approval.requested" {
            if at.is_some() || version.py() != "1" || state.text() != Some("requested") {
                return Err(format!("invalid initial approval row for {}", id.py()));
            }
            if row.get("repo_hash").text() != Some(&hash) {
                return Err("approval belongs to a different repository".into());
            }
            if projection::stamp(row.get("expires_at")).is_none() {
                return Err(format!(
                    "invalid UTC timestamp: {}",
                    row.default("expires_at", "").py()
                ));
            }
            let mut values = obj([]);
            for (field, label, optional) in [
                ("attempt_id", "attempt", true),
                ("action", "action", false),
                ("object_id", "object_id", false),
                ("task_id", "task_id", true),
                ("lease_id", "lease_id", true),
            ] {
                values.set(field, safe(&row.default(field, "").py(), label, optional)?);
            }
            values.set(
                "summary",
                bounded(&row.default("summary", "").py(), "summary", 200)?,
            );
            values.set(
                "profile",
                safe(
                    &row.default("profile", "trusted-local").py(),
                    "profile",
                    false,
                )?,
            );
            for field in ["base_sha", "patch_sha"] {
                values.set(field, row.default(field, "").py());
            }
            if values.get("base_sha").truth() && !git_id(&values.get("base_sha").py()) {
                return Err(format!("approval row {number} has invalid base_sha"));
            }
            if values.get("patch_sha").truth()
                && !projection::hex(values.get("patch_sha"), 64, false)
            {
                return Err(format!("approval row {number} has invalid patch_sha"));
            }
            let parameters = row.default("parameters", obj([]));
            scalar_map(&parameters, "parameters")?;
            values.set("parameters", parameters.clone());
            if row.get("action_digest").text() != Some(&approval_action_digest(repo, &values)?) {
                return Err(format!(
                    "approval row {number} action digest does not match"
                ));
            }
            let mut current = obj([
                ("schema", 1usize.into()),
                ("approval_id", id),
                ("version", 1usize.into()),
                ("state", "requested".into()),
                ("parameters", parameters),
                ("requested_at", row.get("ts").clone()),
                ("expires_at", row.get("expires_at").clone()),
                ("updated_at", row.get("ts").clone()),
            ]);
            for field in [
                "attempt_id",
                "action",
                "object_id",
                "summary",
                "action_digest",
                "base_sha",
                "patch_sha",
                "task_id",
                "lease_id",
            ] {
                current.set(field, row.default(field, ""));
            }
            current.set("profile", row.default("profile", "trusted-local"));
            indices.insert(key, projected.len());
            projected.push(current);
            continue;
        }
        let at: usize =
            at.ok_or_else(|| format!("approval event precedes request for {}", id.py()))?;
        let current = &mut projected[at];
        let expected = add(&current.get("version").py(), "1");
        if version.py() != expected {
            return Err(format!(
                "approval {} version is {}, expected {expected}",
                id.py(),
                version.py()
            ));
        }
        if row.get("expected_version") != current.get("version") {
            return Err(format!(
                "approval {} expected_version does not match",
                id.py()
            ));
        }
        let allowed = match event_type {
            "approval.approved" => {
                current.get("state").text() == Some("requested") && state.text() == Some("approved")
            }
            "approval.denied" => {
                current.get("state").text() == Some("requested") && state.text() == Some("denied")
            }
            "approval.expired" => {
                matches!(current.get("state").text(), Some("requested" | "approved"))
                    && state.text() == Some("expired")
            }
            "approval.consuming" => {
                current.get("state").text() == Some("approved") && state.text() == Some("consuming")
            }
            "approval.consumed" => {
                current.get("state").text() == Some("consuming") && state.text() == Some("consumed")
            }
            "approval.failed" => {
                current.get("state").text() == Some("consuming") && state.text() == Some("failed")
            }
            "approval.interrupted" => {
                current.get("state").text() == Some("consuming")
                    && state.text() == Some("interrupted")
            }
            _ => return Err(format!("approval row {number} has invalid event_type")),
        };
        if !allowed {
            return Err(format!(
                "illegal approval transition: {} -> {}",
                current.get("state").py(),
                state.py()
            ));
        }
        current.set("state", state.clone());
        current.set("version", version.clone());
        current.set("updated_at", row.get("ts").clone());
        if event_type == "approval.interrupted" {
            if row.get("outcome").text() != Some("unknown") {
                return Err("interrupted approval outcome must be unknown".into());
            }
            current.set("outcome", "unknown");
        }
        if row.get("actor").truth() {
            current.set("decided_by", row.get("actor").clone());
        }
        if row.get("changes") != &V::Null {
            current.set("changes", row.get("changes").clone());
        }
        if row.get("grant_id").truth() {
            current.set("_grant_id", row.get("grant_id").clone());
        }
        if row.get("grant_hash").truth() {
            current.set("_grant_hash", row.get("grant_hash").clone());
        }
    }
    Ok(projected)
}
fn git_id(s: &str) -> bool {
    (40..=64).contains(&s.len()) && projection::hex(&V::from(s), s.len(), false)
}
pub fn approval_effective_state(row: &V, at: Option<f64>) -> Result<String> {
    let state = row.get("state").or("unknown").py();
    if !matches!(state.as_str(), "requested" | "approved") {
        return Ok(state);
    }
    let expiry = projection::stamp(row.get("expires_at")).ok_or_else(|| {
        format!(
            "invalid UTC timestamp: {}",
            row.default("expires_at", "").py()
        )
    })?;
    Ok(if at.unwrap_or_else(now_seconds) >= expiry as f64 {
        "expired".into()
    } else {
        state
    })
}
pub fn public_approval(row: &V) -> Result<V> {
    let V::Obj(fields) = row else {
        return Err("approval must be an object".into());
    };
    let mut value = V::Obj(
        fields
            .iter()
            .filter(|(key, _)| !key.starts_with(b"_"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    value.set("effective_state", approval_effective_state(row, None)?);
    Ok(value)
}
pub fn load_approvals(repo: &Path) -> Result<Vec<V>> {
    approval_projection(&read_rows(&approval_path(repo, false)?)?, repo)
}
fn event(current: &V, state: &str) -> Result<V> {
    Ok(obj([
        ("schema", 1usize.into()),
        ("event_id", uuid("apevt_")?.into()),
        ("ts", now().into()),
        ("approval_id", current.get("approval_id").clone()),
        ("version", V::Num(add(&current.get("version").py(), "1"))),
        ("expected_version", current.get("version").clone()),
        ("event_type", format!("approval.{state}").into()),
        ("state", state.into()),
    ]))
}
fn selected<'a>(rows: &'a [V], id: &str) -> Result<&'a V> {
    rows.iter()
        .find(|v| v.get("approval_id").text() == Some(id))
        .ok_or_else(|| format!("unknown approval: {id}"))
}
fn cas(current: &V, expected: &V, state: &str) -> Result<()> {
    if current.get("version") != expected || current.get("state").text() != Some(state) {
        return Err(format!(
            "approval compare-and-set failed: expected {state} version {}",
            expected.py()
        ));
    }
    Ok(())
}
fn unexpired(current: &V) -> Result<()> {
    if approval_effective_state(current, None)? == "expired" {
        Err("approval expired".into())
    } else {
        Ok(())
    }
}
fn proof() -> Result<String> {
    let bytes = crate::random_bytes(32)?;
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (chunk.get(1).copied().unwrap_or(0) as u32) << 8
            | chunk.get(2).copied().unwrap_or(0) as u32;
        for i in 0..chunk.len() + 1 {
            out.push(alphabet[((n >> (18 - i * 6)) & 63) as usize] as char);
        }
    }
    Ok(out)
}
fn split_grant(raw: &str) -> Result<(&str, &str)> {
    let (id, proof) = raw.split_once('.').ok_or("invalid approval grant token")?;
    safe(id, "grant id", false)?;
    if !(32..=128).contains(&proof.len())
        || !proof
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("invalid approval grant token".into());
    }
    Ok((id, proof))
}
fn constant_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // Keep every byte comparison observable, preventing conversion of the
    // reduction into an early-exit memcmp. Lengths of stored hashes are public.
    let difference = std::sync::atomic::AtomicU8::new(0);
    for (a, b) in a.bytes().zip(b.bytes()) {
        std::hint::black_box(&difference).fetch_or(a ^ b, std::sync::atomic::Ordering::Relaxed);
    }
    difference.load(std::sync::atomic::Ordering::Relaxed) == 0
}

pub(crate) fn execute(
    repo: &Path,
    args: &Args,
    interrupted: &AtomicBool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    match args.command.as_str() {
        "request" => request(repo, args, interrupted, out),
        "decide" | "begin-consume" | "finish-consume" => {
            let path = approval_path(repo, false)?;
            let id = safe(&args.string("approval"), "approval", false)?;
            let actor = safe(
                &args.string(if args.command == "decide" {
                    "actor"
                } else {
                    "consumer"
                }),
                if args.command == "decide" {
                    "actor"
                } else {
                    "consumer"
                },
                false,
            )?;
            let expected = args.integer("expected-version")?;
            if args.command == "decide" && !expected.integer(true) {
                return Err("expected-version must be positive".into());
            }
            let supplied = args.string("token");
            let grant = if args.command == "begin-consume" {
                Some(split_grant(&supplied)?)
            } else {
                None
            };
            let _lock = FileLock::acquire(&path, interrupted)?;
            let projected = load_approvals(repo)?;
            let current = selected(&projected, &id)?;
            let state = match args.command.as_str() {
                "decide" => "requested",
                "begin-consume" => "approved",
                _ => "consuming",
            };
            cas(current, &expected, state)?;
            if args.command != "finish-consume" {
                unexpired(current)?;
            }
            let mut output = id.clone();
            let mut row = if args.command == "decide" {
                if args.string("decision") == "approve" {
                    let grant = uuid("grant_")?;
                    let proof = proof()?;
                    let hash = digest(format!("{id}:{proof}").as_bytes());
                    let mut row = event(current, "approved")?;
                    row.set("grant_id", grant.clone());
                    row.set("grant_hash", hash);
                    if !args.string("changes-json").is_empty() {
                        row.set(
                            "changes",
                            parse_scalar_map(&args.string("changes-json"), "changes-json")?,
                        );
                    }
                    output = format!("{grant}.{proof}");
                    row
                } else {
                    event(current, "denied")?
                }
            } else if let Some((grant, proof)) = grant {
                let hash = digest(format!("{id}:{proof}").as_bytes());
                if current.get("_grant_id").text() != Some(grant)
                    || !constant_equal(current.get("_grant_hash").text().unwrap_or(""), &hash)
                {
                    return Err("approval grant does not match this action".into());
                }
                event(current, "consuming")?
            } else {
                event(current, &args.string("result"))?
            };
            row.set("actor", actor);
            checkpoint(interrupted)?;
            append_row(&path, &mut row, true)?;
            line(out, output)
        }
        "expire" | "reconcile" => reconcile(repo, args, interrupted, out),
        "show" => {
            let id = safe(&args.string("approval"), "approval", false)?;
            let rows = load_approvals(repo)?;
            let value = public_approval(selected(&rows, &id)?)?;
            if args.flag("json") {
                return print_json(out, &value);
            }
            line(
                out,
                format!(
                    "approval: {id}\nstate/version: {}/{}",
                    value.get("state").py(),
                    value.get("version").py()
                ),
            )?;
            if value.get("state") != value.get("effective_state") {
                line(
                    out,
                    format!("effective state: {}", value.get("effective_state").py()),
                )?;
            }
            line(
                out,
                format!(
                    "action/object: {}/{}\nexpires: {}",
                    value.get("action").py(),
                    value.get("object_id").py(),
                    value.get("expires_at").py()
                ),
            )
        }
        "list" => {
            let mut values = load_approvals(repo)?
                .iter()
                .map(public_approval)
                .collect::<Result<Vec<_>>>()?;
            values.sort_by_key(|v| v.get("updated_at").py());
            values.retain(|v| {
                (!args.flag("pending")
                    || matches!(
                        v.get("effective_state").text(),
                        Some("requested" | "approved" | "consuming")
                    ))
                    && (!args.flag("state") || v.get("state").text() == Some(&args.string("state")))
                    && (!args.flag("effective-state")
                        || v.get("effective_state").text() == Some(&args.string("effective-state")))
            });
            if args.flag("json") {
                return print_json(out, &V::Arr(values));
            }
            if values.is_empty() {
                return line(out, "no approvals");
            }
            for value in values {
                line(
                    out,
                    format!(
                        "{:<38} v{:<3} {:<12} {:<18} {}",
                        value.get("approval_id").py(),
                        value.get("version").py(),
                        value.get("state").py(),
                        value.get("action").py(),
                        value.get("summary").py()
                    ),
                )?;
            }
            Ok(())
        }
        "validate" => {
            use std::os::unix::fs::PermissionsExt;
            let path = approval_path(repo, false)?;
            let rows = read_rows(&path)?;
            approval_projection(&rows, repo)?;
            if let Ok(metadata) = std::fs::metadata(&path) {
                let mode = metadata.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    return Err(format!(
                        "approval store permissions are too broad: {mode:03o}"
                    ));
                }
            }
            line(out, format!("approvals: ok ({} events)", rows.len()))
        }
        "path" => out
            .write_all(approval_path(repo, false)?.as_os_str().as_bytes())
            .and_then(|_| out.write_all(b"\n"))
            .map_err(|e| e.to_string()),
        _ => Err("unknown approval command".into()),
    }
}
fn request(
    repo: &Path,
    args: &Args,
    interrupted: &AtomicBool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let path = approval_path(repo, true)?;
    let id = uuid("apr_")?;
    let mut values = obj([]);
    for (option, field, optional) in [
        ("action", "action", false),
        ("object-id", "object_id", false),
        ("attempt", "attempt_id", true),
        ("task-id", "task_id", true),
        ("lease-id", "lease_id", true),
        ("profile", "profile", false),
    ] {
        values.set(
            field,
            safe(
                &args.string(option),
                if option == "attempt" {
                    "attempt"
                } else {
                    field
                },
                optional,
            )?,
        );
    }
    values.set("summary", bounded(&args.string("summary"), "summary", 200)?);
    values.set(
        "parameters",
        parse_scalar_map(&args.string("parameters-json"), "parameters-json")?,
    );
    let expires = args.number("expires-in")?;
    if !(1..=604800).contains(&expires) {
        return Err("expires-in must be between 1 and 604800 seconds".into());
    }
    let base = args.string("base-sha");
    let base = if base.is_empty() {
        storage::git_head(repo)
    } else {
        base
    };
    if !base.is_empty() && !git_id(&base) {
        return Err("base-sha must be a Git object id".into());
    }
    let patch = args.string("patch-sha");
    if !patch.is_empty() && !projection::hex(&V::from(patch.clone()), 64, false) {
        return Err("patch-sha must be 64 lowercase hex characters".into());
    }
    values.set("base_sha", base);
    values.set("patch_sha", patch);
    let seconds = now_seconds() as i64;
    let mut row = obj([
        ("schema", 1usize.into()),
        ("event_id", uuid("apevt_")?.into()),
        ("ts", stamp_at(seconds).into()),
        ("approval_id", id.clone().into()),
        ("version", 1usize.into()),
        ("event_type", "approval.requested".into()),
        ("state", "requested".into()),
        ("repo_hash", repo_hash(repo)?.into()),
        ("expires_at", stamp_at(seconds + expires).into()),
        (
            "action_digest",
            approval_action_digest(repo, &values)?.into(),
        ),
    ]);
    if let V::Obj(fields) = values
        && let V::Obj(ref mut target) = row
    {
        target.extend(fields.into_iter().filter(|(_, value)| value.truth()));
    }
    let _lock = FileLock::acquire(&path, interrupted)?;
    approval_projection(&read_rows(&path)?, repo)?;
    checkpoint(interrupted)?;
    append_row(&path, &mut row, true)?;
    line(out, id)
}
fn reconcile(
    repo: &Path,
    args: &Args,
    interrupted: &AtomicBool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let reconcile = args.command == "reconcile";
    let older = if reconcile {
        args.number("older-than-seconds")?
    } else {
        0
    };
    if reconcile && !(1..=604800).contains(&older) {
        return Err("older-than-seconds must be between 1 and 604800 seconds".into());
    }
    let path = approval_path(repo, false)?;
    let _lock = FileLock::acquire(&path, interrupted)?;
    let projected = load_approvals(repo)?;
    let now = now_seconds();
    let mut selected = Vec::new();
    for current in projected {
        if reconcile {
            if current.get("state").text() != Some("consuming") {
                continue;
            }
            let age = ((now
                - projection::stamp(current.get("updated_at"))
                    .ok_or("invalid reservation timestamp")? as f64) as i64)
                .max(0);
            if age < older {
                continue;
            }
            let mut item = obj([
                ("approval_id", current.get("approval_id").clone()),
                ("version", current.get("version").clone()),
                ("state", "consuming".into()),
                ("age_seconds", age.into()),
                ("outcome", "unknown".into()),
            ]);
            if current.get("action").text() == Some("patch-land") {
                item.set("recovery", "patch-land");
            } else if args.flag("apply") {
                let mut row = event(&current, "interrupted")?;
                row.set("outcome", "unknown");
                row.set("actor", "approval-reconciler");
                checkpoint(interrupted)?;
                append_row(&path, &mut row, true)?;
            }
            selected.push(item);
        } else {
            if !matches!(current.get("state").text(), Some("requested" | "approved"))
                || approval_effective_state(&current, Some(now))? != "expired"
            {
                continue;
            }
            selected.push(current.get("approval_id").clone());
            if args.flag("apply") {
                let mut row = event(&current, "expired")?;
                checkpoint(interrupted)?;
                append_row(&path, &mut row, true)?;
            }
        }
    }
    if args.flag("json") {
        return print_json(out, &V::Arr(selected));
    }
    if selected.is_empty() {
        return line(
            out,
            if reconcile {
                "no stale consuming approvals"
            } else {
                "no expired approvals"
            },
        );
    }
    for item in selected {
        if !reconcile {
            line(out, item.py())?;
            continue;
        }
        let suffix = if item.get("recovery").text() == Some("patch-land") {
            if args.flag("apply") {
                " recovery=patch-land -> deferred"
            } else {
                " recovery=patch-land"
            }
        } else if args.flag("apply") {
            " -> interrupted"
        } else {
            ""
        };
        line(
            out,
            format!(
                "{} consuming age={}s outcome=unknown{suffix}",
                item.get("approval_id").py(),
                item.get("age_seconds").py()
            ),
        )?;
    }
    Ok(())
}
