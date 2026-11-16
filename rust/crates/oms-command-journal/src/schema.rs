#![forbid(unsafe_code)]

use crate::json::{JsonExt, Map, MapExt, Value};
use crate::sanitize::{
    SCHEMA_VERSION, canonical_json_bytes, sanitize_text, sanitize_value, sha256_hex,
};
use crate::tz::{TimeZoneSpec, parse_rfc3339_utc, utc_rfc3339};
use chrono::{DateTime, Utc};
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const EVENT_TYPES: &[&str] = &[
    "agent_state",
    "annotation",
    "ci",
    "commit",
    "correction",
    "experiment",
    "handoff",
    "job",
    "patch_admit",
    "patch_review",
    "phase_outcome",
    "pull_request",
    "run",
    "task_outcome",
    "validation",
];
pub const VERIFICATION_STATUSES: &[&str] = &[
    "failed",
    "not_applicable",
    "not_verified",
    "passed",
    "skipped",
];

pub fn is_valid_event_type(s: &str) -> bool {
    EVENT_TYPES.contains(&s)
}

pub fn is_valid_verification_status(s: &str) -> bool {
    VERIFICATION_STATUSES.contains(&s)
}

pub fn allowed_verification_statuses_text() -> String {
    VERIFICATION_STATUSES.join(", ")
}

#[derive(Clone, Debug)]
pub struct ProjectIdentity {
    pub id: String,
    pub name: String,
    pub origin_remote: String,
    pub github_repository: String,
    pub repo_root: PathBuf,
}

fn trim_git_path_output(raw: &[u8]) -> Option<PathBuf> {
    let trimmed = raw
        .strip_suffix(b"\n")
        .unwrap_or(raw)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| raw.strip_suffix(b"\n").unwrap_or(raw));
    if trimmed.is_empty() {
        return None;
    }
    #[cfg(unix)]
    {
        Some(PathBuf::from(OsString::from_vec(trimmed.to_vec())))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(trimmed).ok().map(PathBuf::from)
    }
}

pub fn canonical_repo_root(repo: &Path) -> PathBuf {
    let base = fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    if let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(&base)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        && out.status.success()
        && let Some(top) = trim_git_path_output(&out.stdout)
    {
        return fs::canonicalize(&top).unwrap_or(top);
    }
    base
}

pub fn canonical_github_repository(remote_url: &str) -> Option<(String, String)> {
    let value = remote_url.trim().replace('\\', "/");
    let (host, path) = if let Some((_, rest)) = value.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()?.split(':').next()?, path)
    } else {
        let (authority, path) = value.split_once(':')?;
        (authority.rsplit('@').next()?, path)
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path.split(['?', '#']).next()?.trim_matches('/');
    let path = if path.to_ascii_lowercase().ends_with(".git") {
        &path[..path.len() - 4]
    } else {
        path
    };
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() != 2 || parts.iter().any(|p| matches!(*p, "." | "..")) {
        return None;
    }
    let slug = format!("{}/{}", parts[0].to_lowercase(), parts[1].to_lowercase());
    Some((format!("github.com/{slug}"), slug))
}

pub fn git_origin_remote(repo_root: &Path) -> String {
    if let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        && out.status.success()
    {
        let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !url.is_empty() {
            return url;
        }
    }
    String::new()
}

pub fn detected_project_id(
    github_repository: &str,
    origin_remote: &str,
    repo_root: &Path,
) -> String {
    let digest = if !github_repository.is_empty() {
        sha256_hex(github_repository.as_bytes())
    } else if !origin_remote.is_empty() {
        sha256_hex(origin_remote.as_bytes())
    } else {
        #[cfg(unix)]
        {
            sha256_hex(repo_root.as_os_str().as_bytes())
        }
        #[cfg(not(unix))]
        {
            sha256_hex(repo_root.to_string_lossy().as_bytes())
        }
    };
    format!("proj_{}", &digest[..20])
}

pub(crate) fn detected_identity(repo: &Path) -> (String, String) {
    let origin = git_origin_remote(repo);
    let github = canonical_github_repository(&origin);
    let key = github
        .as_ref()
        .map(|(key, _)| key.clone())
        .unwrap_or_default();
    let name = github.map(|(_, name)| name).unwrap_or_else(|| {
        let name = sanitize_text(
            &repo
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default(),
            120,
        );
        if name.is_empty() {
            "project".into()
        } else {
            name
        }
    });
    (
        detected_project_id(&key, &sanitize_text(&origin, 1000), repo),
        name,
    )
}

pub fn work_journal_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".oms").join("work-journal")
}

pub fn project_id_path(repo_root: &Path) -> PathBuf {
    work_journal_dir(repo_root).join("project.json")
}

pub fn resolve_project_identity(
    repo_root: &Path,
    existing_events: &[Value],
    persist_if_missing: bool,
    explicit_id: Option<&str>,
    explicit_name: Option<&str>,
) -> Result<ProjectIdentity, String> {
    let raw_origin = git_origin_remote(repo_root);
    let github = canonical_github_repository(&raw_origin);
    let origin_remote = sanitize_text(&raw_origin, 1000);
    let github_repository = github
        .as_ref()
        .map(|(identity, _)| identity.clone())
        .unwrap_or_default();
    let detected_name = github
        .as_ref()
        .map(|(_, name)| name.clone())
        .unwrap_or_else(|| {
            let name = sanitize_text(
                &repo_root
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default(),
                120,
            );
            if name.is_empty() {
                "project".into()
            } else {
                name
            }
        });
    let project_path = project_id_path(repo_root);
    let stored = crate::cold::read_regular_no_symlink_bytes(&project_path)?
        .and_then(|bytes| crate::json::from_slice(&bytes).ok())
        .filter(|row| row.get("schema_version").and_then(Value::as_u64) == Some(1))
        .and_then(|row| {
            Some((
                row.get("project_id")?.as_str()?.to_owned(),
                row.get("project_name")?.as_str()?.to_owned(),
            ))
        });
    let recovered = stored.or_else(|| {
        existing_events.iter().find_map(|event| {
            validate_canonical_event(event).ok()?;
            Some((
                event.get("project_id")?.as_str()?.to_owned(),
                event.get("project_name")?.as_str()?.to_owned(),
            ))
        })
    });
    let id = explicit_id
        .map(str::to_owned)
        .or_else(|| recovered.as_ref().map(|(id, _)| id.clone()))
        .unwrap_or_else(|| detected_project_id(&github_repository, &origin_remote, repo_root));
    let name = explicit_name
        .map(str::to_owned)
        .or_else(|| recovered.map(|(_, name)| name))
        .unwrap_or(detected_name);
    if persist_if_missing && crate::cold::read_regular_no_symlink_bytes(&project_path)?.is_none() {
        let value =
            crate::json::json!({"schema_version": 1, "project_id": id, "project_name": name});
        atomic_write_bytes(&project_path, &crate::sanitize::pretty_json_bytes(&value)?)?;
    }
    Ok(ProjectIdentity {
        id,
        name,
        origin_remote,
        github_repository,
        repo_root: repo_root.to_path_buf(),
    })
}

pub fn ensure_oms_gitignore(journal_dir: &Path) -> Result<(), String> {
    let Some(oms) = journal_dir.parent() else {
        return Ok(());
    };
    if oms.file_name().is_none_or(|n| n != ".oms") {
        return Ok(());
    }
    let path = oms.join(".gitignore");
    if crate::cold::read_regular_no_symlink_bytes(&path)?.is_none() {
        atomic_write_bytes(&path, b"*\n")?;
    }
    Ok(())
}

/// Anchor journal paths above .oms so none of its components can resolve through links.
/// Other explicit file adapters anchor at the nearest existing parent; the shared
/// core proves its physical spelling and checks every relative component.
pub(crate) fn storage_root(path: &Path) -> Result<(oms_safe_fs::Root, PathBuf), String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let components: Vec<_> = absolute.components().collect();
    let boundary = components.iter().position(|p| p.as_os_str() == ".oms");
    let mut base = if let Some(index) = boundary {
        components[..index].iter().collect::<PathBuf>()
    } else {
        absolute
            .parent()
            .ok_or("missing storage parent")?
            .to_path_buf()
    };
    loop {
        match fs::symlink_metadata(&base) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !base.pop() {
                    return Err("missing storage root".into());
                }
            }
            Err(e) => return Err(format!("storage root unavailable: {e}")),
        }
    }
    let relative = absolute
        .strip_prefix(&base)
        .map_err(|_| "invalid storage path")?
        .to_path_buf();
    let root =
        oms_safe_fs::Root::open(&base).map_err(|e| format!("storage root unavailable: {e:?}"))?;
    Ok((root, relative))
}

pub fn atomic_write_bytes(path: &Path, content: &[u8]) -> Result<(), String> {
    crate::files::replace_bytes(path, content)
}

fn clean_record(value: &Value, allowed: &[&str], required: &[&str]) -> Result<Map, String> {
    let obj = value
        .as_object()
        .ok_or("structured field must be an object")?;
    let mut output = Map::new();
    for &key in allowed {
        if let Some(value) = obj.get(key).and_then(|v| sanitize_value(v, Some(key)))
            && !is_empty_value(&value)
        {
            crate::sanitize::require_scalar_strings(&value)?;
            output.insert(key.into(), value);
        }
    }
    for &key in required {
        if !output.get(key).is_some_and(json_truthy) {
            return Err(format!("missing required field: {key}"));
        }
    }
    Ok(output)
}

fn clean_records(value: Option<&Value>, allowed: &[&str]) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(64)
        .filter_map(|v| clean_record(v, allowed, &[]).ok())
        .filter(|row| !row.is_empty())
        .map(Value::Object)
        .collect()
}

pub(crate) fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Integer(n) => n.as_decimal() != "0",
        Value::Float(n) => *n != 0.0,
        Value::String(s) => !s.codepoints().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

fn python_string_repr(text: &oms_json::PyString) -> String {
    let points = text.codepoints();
    let quote = if points.contains(&u32::from('\'')) && !points.contains(&u32::from('"')) {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for &point in points {
        match char::from_u32(point) {
            Some('\\') => out.push_str("\\\\"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some('\t') => out.push_str("\\t"),
            Some(ch) if ch == quote => {
                out.push('\\');
                out.push(ch);
            }
            Some(ch) if crate::unicode::printable(ch) => out.push(ch),
            _ => {
                use std::fmt::Write;
                if point <= 0xff {
                    let _ = write!(&mut out, "\\x{point:02x}");
                } else if point <= 0xffff {
                    let _ = write!(&mut out, "\\u{point:04x}");
                } else {
                    let _ = write!(&mut out, "\\U{point:08x}");
                }
            }
        }
    }
    out.push(quote);
    out
}

fn python_repr(value: &Value) -> String {
    enum Frame<'a> {
        Node(&'a Value),
        Text(String),
    }
    let mut pending = vec![Frame::Node(value)];
    let mut out = String::new();
    while let Some(frame) = pending.pop() {
        let Frame::Node(value) = frame else {
            if let Frame::Text(text) = frame {
                out.push_str(&text);
            }
            continue;
        };
        match value {
            Value::String(text) => out.push_str(&python_string_repr(text)),
            Value::Array(items) => {
                out.push('[');
                pending.push(Frame::Text("]".into()));
                for (index, item) in items.iter().enumerate().rev() {
                    pending.push(Frame::Node(item));
                    if index > 0 {
                        pending.push(Frame::Text(", ".into()));
                    }
                }
            }
            Value::Object(object) => {
                out.push('{');
                pending.push(Frame::Text("}".into()));
                for (index, (key, value)) in object.iter().enumerate().rev() {
                    pending.push(Frame::Node(value));
                    pending.push(Frame::Text(": ".into()));
                    pending.push(Frame::Text(python_string_repr(key)));
                    if index > 0 {
                        pending.push(Frame::Text(", ".into()));
                    }
                }
            }
            _ => out.push_str(&python_str(value)),
        }
    }
    out
}
pub(crate) fn python_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => crate::sanitize::display_py_string(s),
        Value::Integer(n) => n.as_decimal().into(),
        Value::Float(n) if n.is_nan() => "nan".into(),
        Value::Float(n) if *n == f64::INFINITY => "inf".into(),
        Value::Float(n) if *n == f64::NEG_INFINITY => "-inf".into(),
        Value::Float(_) => {
            crate::sanitize::canonical_json_string(value).expect("finite JSON float")
        }
        _ => python_repr(value),
    }
}

pub(crate) fn python_str_checked(value: &Value) -> Result<String, String> {
    if let Value::String(text) = value {
        return text
            .to_str_checked()
            .ok_or_else(|| "canonical JSON error: lone surrogate".into());
    }
    Ok(python_str(value))
}

pub fn normalize_event(
    raw: &Value,
    project: &ProjectIdentity,
    tz: &TimeZoneSpec,
    recorded_at: DateTime<Utc>,
) -> Result<Value, String> {
    let obj = raw.as_object().ok_or("event must be an object")?;
    if obj
        .get("schema_version")
        .is_some_and(|v| v.as_u64() != Some(SCHEMA_VERSION))
    {
        return Err("unsupported schema version".into());
    }
    let event_type = sanitize_text(
        &obj.get("event_type")
            .map(python_str_checked)
            .transpose()?
            .unwrap_or_default(),
        80,
    );
    if !is_valid_event_type(&event_type) {
        return Err("invalid event_type".into());
    }
    let occurred = parse_rfc3339_utc(
        &obj.get("occurred_at")
            .map(python_str_checked)
            .transpose()?
            .unwrap_or_default(),
    )?;
    let (_, local_date, iso_week) = tz.local_date_and_week(occurred);
    let mut source = clean_record(
        obj.get("source").unwrap_or(&Value::Null),
        &["type", "id", "path", "url"],
        &["type"],
    )?;
    let authoritative = source.get("id").is_some_and(json_truthy);
    let empty = crate::json::json!({});
    let correlation = clean_record(
        obj.get("correlation").unwrap_or(&empty),
        &[
            "task_id",
            "session_id",
            "phase_id",
            "run_id",
            "operation_id",
        ],
        &[],
    )?;
    let operation = correlation
        .get("operation_id")
        .filter(|v| json_truthy(v))
        .map(python_str_checked)
        .transpose()?;
    if !authoritative {
        let id = if let Some(operation) = &operation {
            format!("op_{}", &sha256_hex(operation.as_bytes())[..24])
        } else {
            let stable: Map = crate::json::object(
                obj.iter()
                    .filter(|(k, _)| {
                        !matches!(
                            k.to_str_checked().as_deref().unwrap_or(""),
                            "recorded_at" | "occurred_at" | "schema_version"
                        )
                    })
                    .map(|(k, v)| (k.clone(), v.copy())),
            );
            let clean = sanitize_value(&Value::Object(stable), None).unwrap_or(Value::Null);
            format!("src_{}", &sha256_hex(&canonical_json_bytes(&clean)?)[..24])
        };
        source.insert("id".into(), Value::String(id.as_str().into()));
    }
    let verification = sanitize_text(
        &obj.get("verification_status")
            .map(python_str_checked)
            .transpose()?
            .unwrap_or_default(),
        40,
    );
    if !is_valid_verification_status(&verification) {
        return Err("invalid verification_status".into());
    }
    let mut normalized = crate::json::json!({
        "schema_version": SCHEMA_VERSION,
        "project_id": sanitize_text(&project.id, 120),
        "project_name": sanitize_text(&project.name, 120),
        "event_type": event_type,
        "occurred_at": utc_rfc3339(occurred),
        "recorded_at": utc_rfc3339(recorded_at),
        "timezone": sanitize_text(tz.name(), 120),
        "local_date": local_date, "iso_week": iso_week,
        "source": source, "verification_status": verification
    });
    let out = normalized.as_object_mut().expect("object");
    if !correlation.is_empty() {
        out.insert("correlation".into(), Value::Object(correlation));
    }
    for (field, allowed) in [
        ("outcome", &["summary", "status", "interpretation"][..]),
        (
            "provenance",
            &[
                "code_commit",
                "config_path",
                "config_hash",
                "dataset",
                "split",
                "seed",
                "checkpoint",
                "artifact_id",
                "job_id",
                "run_id",
                "gpu",
                "execution_environment",
                "verdict",
                "command_summary",
                "exit_code",
                "duration_seconds",
                "workflow",
                "attempt",
                "hypothesis",
                "prediction",
                "baseline",
                "primary_metric",
                "success_criterion",
                "independent_change",
            ][..],
        ),
    ] {
        if let Some(value) = obj.get(field) {
            let row = clean_record(value, allowed, &[])?;
            if !row.is_empty() {
                out.insert(field.into(), Value::Object(row));
            }
        }
    }
    for field in ["goal", "decision", "blocker", "next_action"] {
        if let Some(clean) = obj.get(field).and_then(|v| sanitize_value(v, Some(field)))
            && matches!(&clean, Value::String(s) if !s.codepoints().is_empty())
        {
            crate::sanitize::require_scalar_strings(&clean)?;
            out.insert(field.into(), clean);
        }
    }
    let refs = clean_records(obj.get("refs"), &["type", "id", "path", "url", "label"]);
    if !refs.is_empty() {
        out.insert("refs".into(), Value::Array(refs));
    }
    let evidence = clean_records(obj.get("evidence"), &["type", "ref", "label"]);
    let has_evidence = !evidence.is_empty();
    if has_evidence {
        out.insert("evidence".into(), Value::Array(evidence));
    }
    let metrics: Vec<Value> = clean_records(
        obj.get("metrics"),
        &[
            "name",
            "value",
            "unit",
            "dataset",
            "split",
            "conditions",
            "evidence_ref",
        ],
    )
    .into_iter()
    .filter(|row| {
        row.get("name").is_some_and(json_truthy)
            && row.get("value").is_some_and(Value::is_number)
            && (row.get("evidence_ref").is_some_and(json_truthy) || has_evidence)
    })
    .collect();
    if !metrics.is_empty() {
        out.insert("metrics".into(), Value::Array(metrics));
    }
    let supersedes = sanitize_text(
        &obj.get("supersedes_event_id")
            .map(python_str_checked)
            .transpose()?
            .unwrap_or_default(),
        120,
    );
    if !supersedes.is_empty() {
        if event_type != "correction" {
            return Err("only correction events may supersede".into());
        }
        out.insert(
            "supersedes_event_id".into(),
            Value::String(supersedes.as_str().into()),
        );
    }
    let basis = if authoritative {
        crate::json::json!({"project_id": out.at("project_id"), "source_type": out.at("source").at("type"), "source_id": out.at("source").at("id")})
    } else if let Some(operation) = operation {
        crate::json::json!({"project_id": out.at("project_id"), "operation_id": operation, "event_type": out.at("event_type")})
    } else {
        Value::Object(crate::json::object(
            out.iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.to_str_checked().as_deref().unwrap_or(""),
                        "event_id"
                            | "recorded_at"
                            | "occurred_at"
                            | "local_date"
                            | "iso_week"
                            | "timezone"
                    )
                })
                .map(|(k, v)| (k.clone(), v.copy())),
        ))
    };
    out.insert(
        "event_id".into(),
        Value::String(
            format!("wj_{}", &sha256_hex(&canonical_json_bytes(&basis)?)[..32])
                .as_str()
                .into(),
        ),
    );
    validate_canonical_event(&normalized)?;
    Ok(normalized)
}

fn is_empty_value(v: &Value) -> bool {
    matches!(v, Value::Null)
        || matches!(v, Value::String(s) if s.codepoints().is_empty())
        || matches!(v, Value::Array(a) if a.is_empty())
        || matches!(v, Value::Object(m) if m.is_empty())
}

pub fn validate_canonical_event(event: &Value) -> Result<(), String> {
    let obj = event.as_object().ok_or("invalid canonical object")?;
    if obj.get("schema_version").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
        return Err("unsupported schema version".into());
    }
    for key in [
        "event_id",
        "project_id",
        "project_name",
        "event_type",
        "occurred_at",
        "recorded_at",
        "timezone",
        "local_date",
        "iso_week",
        "source",
        "verification_status",
    ] {
        if !obj.has_key(key) {
            return Err("missing canonical field".into());
        }
    }
    if !obj
        .get("event_type")
        .and_then(Value::as_str)
        .is_some_and(|s| is_valid_event_type(&s))
    {
        return Err("invalid canonical event_type".into());
    }
    if !obj
        .get("verification_status")
        .and_then(Value::as_str)
        .is_some_and(|s| is_valid_verification_status(&s))
    {
        return Err("invalid canonical verification_status".into());
    }
    let id = obj
        .get("event_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if id.len() != 35
        || !id.starts_with("wj_")
        || !id.as_bytes()[3..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err("invalid canonical event_id".into());
    }
    for key in ["occurred_at", "recorded_at"] {
        parse_rfc3339_utc(&python_str(obj.at(key)))?;
    }
    let source = obj
        .get("source")
        .and_then(Value::as_object)
        .ok_or("invalid canonical source")?;
    if !source.get("type").is_some_and(json_truthy) || !source.get("id").is_some_and(json_truthy) {
        return Err("invalid canonical source".into());
    }
    Ok(())
}
