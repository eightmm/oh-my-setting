//! Bounded mechanical helpers shared by state and run writers.
use crate::json::{number, object, string};
use crate::{Context, Result, Value, checkpoint, epoch, error, json, random_id, require, storage};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub fn identifier(raw: &str) -> Result<&str> {
    require(
        !raw.is_empty()
            && raw.len() <= 96
            && raw.as_bytes()[0].is_ascii_alphanumeric()
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && !raw.contains(".."),
        "invalid record id",
    )?;
    Ok(raw)
}

pub fn relative(raw: &str) -> Result<&Path> {
    let path = Path::new(raw);
    require(
        !raw.is_empty()
            && !raw.contains(['\\', ':'])
            && !raw.starts_with('~')
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "expected a normalized repository-relative path",
    )?;
    Ok(path)
}

pub fn scrub(value: &Value) -> Result<()> {
    let raw = json::canonical(value)?;
    require(
        oms_scrub::scan(&raw, &[])
            .map_err(|e| error(2, format!("{e:?}")))?
            .is_empty(),
        "record contains sensitive-looking material; nothing was written",
    )
}

pub fn context(value: &Value) -> Result<oms_context::Json> {
    oms_context::parse_json(&json::canonical(value)?).map_err(|e| error(2, e))
}

pub fn from_context(value: &oms_context::Json) -> Value {
    use oms_context::Json as J;
    match value {
        J::Null => Value::Null,
        J::Bool(v) => Value::Bool(*v),
        J::Number(v) => Value::Number(v.clone()),
        J::String(v) => Value::String(v.clone()),
        J::Array(v) => Value::Array(v.iter().map(from_context).collect()),
        J::Object(v) => Value::Object(
            v.iter()
                .map(|(k, v)| (k.clone(), from_context(v)))
                .collect(),
        ),
    }
}

pub(crate) fn query(ctx: &Context<'_>, command: &[&str]) -> Result<Vec<u8>> {
    let native =
        oms_command_runs::Context::new(&ctx.cwd, ctx.interrupted).map_err(|e| error(e.code, e))?;
    let output = oms_command_runs::process::execute(
        &native,
        &command.iter().map(OsString::from).collect::<Vec<_>>(),
        true,
        storage::MAX_FILE,
        Some(Duration::from_secs(30)),
        &mut std::io::sink(),
        &mut std::io::sink(),
    )
    .map_err(|e| error(e.code, e))?;
    require(
        output.code == 0,
        format!(
            "{} query failed (exit {})",
            command.first().copied().unwrap_or("command"),
            output.code
        ),
    )?;
    Ok(output.bytes)
}

pub fn agent() -> String {
    std::env::var("OMS_AGENT").unwrap_or_else(|_| "agent".into())
}

pub fn trace(row: &mut Value, previous: Option<&Value>) -> Result<()> {
    fn nonzero(raw: &str, size: usize) -> bool {
        json::hex(raw, size) && raw.bytes().any(|b| b != b'0')
    }
    let persisted = previous.filter(|r| {
        nonzero(r.text("trace_id"), 32)
            && nonzero(r.text("span_id"), 16)
            && (r.opt("trace_flags").is_none() || json::hex(r.text("trace_flags"), 2))
            && (r.opt("parent_span_id").is_none() || nonzero(r.text("parent_span_id"), 16))
    });
    let (id, parent, flags) = if let Some(old) = persisted {
        (
            old.text("trace_id").to_owned(),
            old.text("span_id").to_owned(),
            old.opt("trace_flags")
                .and_then(|v| v.str().ok())
                .unwrap_or("00")
                .to_owned(),
        )
    } else {
        let raw = std::env::var("OMS_TRACEPARENT").unwrap_or_default();
        let parts: Vec<_> = raw.split('-').collect();
        if parts.len() != 4
            || !json::hex(parts[0], 2)
            || parts[0] == "ff"
            || !nonzero(parts[1], 32)
            || !nonzero(parts[2], 16)
            || !json::hex(parts[3], 2)
        {
            return Ok(());
        }
        (parts[1].into(), parts[2].into(), parts[3].into())
    };
    row.set("trace_id", string(id))?;
    row.set("span_id", string(&random_id("")?[..16]))?;
    row.set("parent_span_id", string(parent))?;
    row.set("trace_flags", string(flags))
}

pub fn duration(raw: &str) -> Result<Duration> {
    let (raw, factor) = match raw.as_bytes().last() {
        Some(b's') => (&raw[..raw.len() - 1], 1.0),
        Some(b'm') => (&raw[..raw.len() - 1], 60.0),
        Some(b'h') => (&raw[..raw.len() - 1], 3600.0),
        Some(b'd') => (&raw[..raw.len() - 1], 86400.0),
        _ => (raw, 1.0),
    };
    require(
        !raw.is_empty()
            && raw.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            && raw.bytes().filter(|b| *b == b'.').count() <= 1,
        "invalid duration",
    )?;
    let seconds = raw
        .parse::<f64>()
        .map_err(|_| error(2, "invalid duration"))?
        * factor;
    require(
        seconds.is_finite() && seconds > 0.0,
        "duration must be positive and finite",
    )?;
    Duration::try_from_secs_f64(seconds).map_err(|_| error(2, "duration out of range"))
}

/// The caller supplies the native helper; limits never modify the embedding process.
#[allow(clippy::too_many_arguments)]
pub fn bounded(
    command: &[OsString],
    cwd: &Path,
    interrupted: &std::sync::atomic::AtomicBool,
    wall: Duration,
    policy: &oms_command_runs::process::ProcessPolicy,
    services: &dyn oms_command_runs::Services,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> Result<oms_command_runs::process::Output> {
    let native = oms_command_runs::Context::new(cwd, interrupted).map_err(|e| error(e.code, e))?;
    let mut policy = policy.clone();
    policy.rlimit_helper = services.rlimit_helper().or(policy.rlimit_helper);
    oms_command_runs::process::execute_with_policy(
        &native,
        command,
        &policy,
        false,
        0,
        Some(wall),
        out,
        err,
    )
    .map_err(|e| error(e.code, e))
}

pub(crate) fn files(
    ctx: &Context<'_>,
    directory: &Path,
    suffix: &str,
    limit: usize,
) -> Result<Vec<PathBuf>> {
    let mut stack = vec![directory.to_owned()];
    let mut found = Vec::new();
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        checkpoint(ctx.interrupted)?;
        let meta = match std::fs::symlink_metadata(&dir) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(error(2, e)),
        };
        require(
            meta.is_dir() && !meta.is_symlink(),
            "scan root must be a non-symlink directory",
        )?;
        for entry in std::fs::read_dir(dir).map_err(|e| error(2, e))? {
            let entry = entry.map_err(|e| error(2, e))?;
            seen += 1;
            require(seen <= limit, "state scan entry limit exceeded")?;
            let kind = entry.file_type().map_err(|e| error(2, e))?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(suffix) {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Direct children only: every matching entry with its kind, so callers can report symlinks.
/// A missing directory is empty; reading never creates anything.
pub(crate) fn shallow_entries(
    ctx: &Context<'_>,
    directory: &Path,
    suffix: &str,
    limit: usize,
) -> Result<Vec<(PathBuf, std::fs::FileType)>> {
    checkpoint(ctx.interrupted)?;
    match std::fs::symlink_metadata(directory) {
        Ok(m) => require(
            m.is_dir() && !m.is_symlink(),
            "scan root must be a non-symlink directory",
        )?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(error(2, e)),
    }
    let mut found = Vec::new();
    for (seen, entry) in std::fs::read_dir(directory)
        .map_err(|e| error(2, e))?
        .enumerate()
    {
        let entry = entry.map_err(|e| error(2, e))?;
        require(seen < limit, "state scan entry limit exceeded")?;
        if entry.file_name().to_string_lossy().ends_with(suffix) {
            found.push((entry.path(), entry.file_type().map_err(|e| error(2, e))?));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

/// Like [`files`] for one directory level: subdirectories are never entered, and symlinks are
/// skipped.
pub(crate) fn shallow_files(
    ctx: &Context<'_>,
    directory: &Path,
    suffix: &str,
    limit: usize,
) -> Result<Vec<PathBuf>> {
    Ok(shallow_entries(ctx, directory, suffix, limit)?
        .into_iter()
        .filter(|(_, kind)| kind.is_file())
        .map(|(path, _)| path)
        .collect())
}

pub fn notebook(raw: &[u8]) -> Result<Value> {
    require(raw.len() <= storage::MAX_STREAM, "notebook exceeds 32 MiB")?;
    let value = json::parse(raw)?;
    let mut with_outputs = 0;
    let mut output_bytes = 0;
    let mut previous = None;
    let mut index = 0;
    let mut inversions = Vec::new();
    for cell in value.get("cells").array()? {
        if cell.text("cell_type") != "code" {
            continue;
        }
        index += 1;
        if cell.get("outputs").array().is_ok_and(|a| !a.is_empty()) {
            with_outputs += 1;
            output_bytes += json::default_bytes(cell.get("outputs"), false)?.len();
        }
        if let Ok(count) = cell.get("execution_count").int() {
            if previous.is_some_and(|n| count <= n) {
                inversions.push(number(index));
            }
            previous = Some(count);
        }
    }
    Ok(object([
        ("with_outputs", number(with_outputs)),
        ("output_bytes", number(output_bytes)),
        ("inversions", Value::Array(inversions)),
    ]))
}

pub fn cache_age(last_seen: Option<i64>, now: i64) -> Option<i64> {
    last_seen.map(|t| now.saturating_sub(t).max(0))
}

pub fn cache_note(subject: &str, age: Option<i64>, change: &str) -> Option<String> {
    let window = std::env::var("OMS_CACHE_WARM_SECONDS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| (0..=86400).contains(v))
        .unwrap_or(300);
    let age = age?;
    (window > 0 && age <= window).then(|| format!("note: {subject} was active {age}s ago (warm window {window}s); changing {change} rebuilds the provider prompt-cache prefix. Proceeding."))
}

pub fn project(root: &Path) -> Value {
    let mut row = object([
        ("schema", number(1)),
        ("present", Value::Bool(true)),
        ("healthy", Value::Bool(true)),
        ("state", string("invalid")),
        ("sha256", string("")),
        ("error", string("")),
    ]);
    match storage::read(root, Path::new("PROJECT.md"), 1024 * 1024, true) {
        Ok(bytes) => {
            let missing = bytes.is_empty() && !root.join("PROJECT.md").exists();
            let _ = row.set("present", Value::Bool(!missing));
            if missing {
                let _ = row.set("state", string("missing"));
                return row;
            }
            match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    let states: Vec<_> = text
                        .lines()
                        .filter_map(|l| l.strip_prefix("- State:").map(str::trim))
                        .collect();
                    let state = match states.as_slice() {
                        [] => "missing",
                        ["draft"] => "draft",
                        ["confirmed"] => "confirmed",
                        ["active"] => "legacy-active",
                        _ => "invalid",
                    };
                    let _ = row.set("state", string(state));
                    let _ = row.set("sha256", string(json::sha(&bytes)));
                }
                Err(_) => {
                    let _ = row.set("healthy", Value::Bool(false));
                    let _ = row.set("error", string("project-not-utf8"));
                }
            }
        }
        Err(_) => {
            let _ = row.set("healthy", Value::Bool(false));
            let _ = row.set("error", string("project-unreadable"));
        }
    }
    row
}

pub fn now_age(stamp: &str) -> Option<i64> {
    cache_age(crate::parse_timestamp(stamp), epoch())
}

pub fn absolute(base: &Path, raw: impl AsRef<Path>) -> PathBuf {
    let path = base.join(raw);
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

pub fn display(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(v) => if *v { "True" } else { "False" }.into(),
        Value::Number(v) => v.clone(),
        Value::String(_) => value.str().unwrap_or("").into(),
        _ => String::from_utf8(json::default_bytes(value, false).unwrap_or_default())
            .unwrap_or_default(),
    }
}
