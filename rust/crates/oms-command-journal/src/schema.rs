#![forbid(unsafe_code)]

use crate::sanitize::{
    DETAIL_CHAR_LIMIT, SCHEMA_VERSION, VALUE_CHAR_LIMIT, canonical_json_bytes, sanitize_multiline,
    sanitize_text, sanitize_value, sha256_hex,
};
use crate::tz::{TimeZoneSpec, parse_rfc3339_utc, utc_rfc3339};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const EVENT_TYPES: &[&str] = &[
    "run_started",
    "run_finished",
    "run_recovered",
    "validation",
    "commit",
    "agent_state",
    "decision",
    "handoff",
    "experiment",
    "research",
    "correction",
];

pub const VERIFICATION_STATUSES: &[&str] = &["not_verified", "failed", "passed", "running"];

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
    {
        if out.status.success() {
            if let Some(top) = trim_git_path_output(&out.stdout) {
                return fs::canonicalize(&top).unwrap_or(top);
            }
        }
    }
    base
}

pub fn canonical_github_repository(remote_url: &str) -> Option<(String, String)> {
    let trimmed = remote_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let cleaned = if let Some(stripped) = trimmed.strip_suffix(".git") {
        stripped
    } else {
        trimmed
    };

    let (host, path_part) = if let Some(rest) = cleaned.strip_prefix("git@") {
        let (h, p) = rest.split_once(':')?;
        (h, p)
    } else if let Some(idx) = cleaned.find("://") {
        let after_scheme = &cleaned[idx + 3..];
        let after_auth = after_scheme
            .rsplit_once('@')
            .map(|(_, r)| r)
            .unwrap_or(after_scheme);
        let (h, p) = after_auth.split_once('/')?;
        let host_no_port = h.split(':').next().unwrap_or(h);
        (host_no_port, p)
    } else {
        return None;
    };

    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let parts: Vec<&str> = path_part
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() != 2 {
        return None;
    }
    let slug = format!(
        "{}/{}",
        parts[0].to_ascii_lowercase(),
        parts[1].to_ascii_lowercase()
    );
    let canonical_url = format!("https://github.com/{slug}");
    Some((slug, canonical_url))
}

fn sanitize_remote_for_display(remote: &str) -> String {
    let trimmed = remote.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some((_, url)) = canonical_github_repository(trimmed) {
        return url;
    }
    sanitize_text(trimmed, VALUE_CHAR_LIMIT)
}

pub fn git_origin_remote(repo_root: &Path) -> String {
    if let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", "remote.origin.url"])
        .output()
    {
        if out.status.success() {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !url.is_empty() {
                return url;
            }
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

pub fn work_journal_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".oms").join("work-journal")
}

pub fn project_id_path(repo_root: &Path) -> PathBuf {
    work_journal_dir(repo_root).join("project-id")
}

fn read_regular_no_symlink_string(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return None;
    }
    fs::read_to_string(path).ok()
}

pub fn resolve_project_identity(
    repo_root: &Path,
    existing_events: &[Value],
    persist_if_missing: bool,
    explicit_id: Option<&str>,
    explicit_name: Option<&str>,
) -> Result<ProjectIdentity, String> {
    let raw_origin = git_origin_remote(repo_root);
    let github_pair = canonical_github_repository(&raw_origin);
    let github_repository = github_pair
        .as_ref()
        .map(|(_, url)| url.clone())
        .unwrap_or_default();
    let origin_remote = sanitize_remote_for_display(&raw_origin);

    let default_name = explicit_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| github_pair.as_ref().map(|(slug, _)| slug.clone()))
        .or_else(|| {
            repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .map(ToOwned::to_owned)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "repo".to_string());

    let pid_file = project_id_path(repo_root);
    let mut recovered_name: Option<String> = None;
    let id = if let Some(forced) = explicit_id.map(str::trim).filter(|s| !s.is_empty()) {
        forced.to_string()
    } else if let Some(stored) = read_regular_no_symlink_string(&pid_file) {
        let trimmed = stored.trim();
        if trimmed.starts_with('{') {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) {
                recovered_name = map
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
                map.get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                    .unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            trimmed.to_string()
        }
    } else {
        String::new()
    };

    let (resolved_id, resolved_name) = if !id.is_empty() {
        (id, recovered_name.unwrap_or(default_name))
    } else if let Some((ev_id, ev_name)) = recover_project_from_events(existing_events) {
        let name = if github_pair.is_some() {
            default_name
        } else {
            ev_name.unwrap_or(default_name)
        };
        if persist_if_missing {
            let _ = persist_project_id(&pid_file, &ev_id);
        }
        (ev_id, name)
    } else {
        let computed = detected_project_id(&github_repository, &origin_remote, repo_root);
        if persist_if_missing && explicit_id.is_none() {
            persist_project_id(&pid_file, &computed)?;
        }
        (computed, default_name)
    };

    Ok(ProjectIdentity {
        id: resolved_id,
        name: sanitize_text(&resolved_name, 200),
        origin_remote,
        github_repository,
        repo_root: repo_root.to_path_buf(),
    })
}

fn recover_project_from_events(events: &[Value]) -> Option<(String, Option<String>)> {
    for ev in events {
        if let Some(proj) = ev.get("project").and_then(Value::as_object) {
            if let Some(id) = proj.get("id").and_then(Value::as_str) {
                let id_t = id.trim();
                if !id_t.is_empty() {
                    let name = proj
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned);
                    return Some((id_t.to_string(), name));
                }
            }
        }
    }
    None
}

fn persist_project_id(pid_file: &Path, project_id: &str) -> Result<(), String> {
    let Some(parent) = pid_file.parent() else {
        return Err("invalid project-id path".to_string());
    };
    ensure_oms_gitignore(parent)?;
    fs::create_dir_all(parent).map_err(|e| {
        format!(
            "failed to create {}: {e}",
            crate::sanitize::diagnostic_os_str(parent.as_os_str())
        )
    })?;
    atomic_write_bytes(pid_file, format!("{project_id}\n").as_bytes())
}

pub fn ensure_oms_gitignore(journal_dir: &Path) -> Result<(), String> {
    let Some(oms_dir) = journal_dir.parent() else {
        return Ok(());
    };
    if oms_dir.file_name().and_then(|n| n.to_str()) != Some(".oms") {
        return Ok(());
    }
    fs::create_dir_all(oms_dir).map_err(|e| {
        format!(
            "failed to create {}: {e}",
            crate::sanitize::diagnostic_os_str(oms_dir.as_os_str())
        )
    })?;
    let gi = oms_dir.join(".gitignore");
    if gi.exists() {
        return Ok(());
    }
    let content = b"# Created automatically by oh-my-setting harness tools.\n*\n!.gitignore\n";
    atomic_write_bytes(&gi, content)
}

fn tmp_sibling_name(file_name: &OsStr, pid: u32, attempt: u32) -> OsString {
    let suffix = format!(".tmp.{pid}.{attempt}");
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(suffix);
    name
}

pub fn atomic_write_bytes(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| {
        format!(
            "missing parent directory for {}",
            crate::sanitize::diagnostic_os_str(path.as_os_str())
        )
    })?;
    fs::create_dir_all(parent).map_err(|e| {
        format!(
            "failed to create {}: {e}",
            crate::sanitize::diagnostic_os_str(parent.as_os_str())
        )
    })?;
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(format!(
                "refusing to overwrite symlink {}",
                crate::sanitize::diagnostic_os_str(path.as_os_str())
            ));
        }
    }
    let pid = std::process::id();
    let file_name = path.file_name().unwrap_or_else(|| OsStr::new("tmp"));
    let mut attempt = 0u32;
    let tmp_path = loop {
        let candidate = parent.join(tmp_sibling_name(file_name, pid, attempt));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(content).and_then(|()| f.sync_all()) {
                    let _ = fs::remove_file(&candidate);
                    return Err(format!(
                        "failed writing {}: {e}",
                        crate::sanitize::diagnostic_os_str(candidate.as_os_str())
                    ));
                }
                break candidate;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 64 => {
                attempt += 1;
            }
            Err(e) => {
                return Err(format!(
                    "failed creating temp file {}: {e}",
                    crate::sanitize::diagnostic_os_str(candidate.as_os_str())
                ));
            }
        }
    };
    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!(
            "failed replacing {}: {e}",
            crate::sanitize::diagnostic_os_str(path.as_os_str())
        ));
    }
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

pub fn normalize_event(
    raw: &Value,
    project: &ProjectIdentity,
    tz: &TimeZoneSpec,
    recorded_at: DateTime<Utc>,
) -> Result<Value, String> {
    let Some(obj) = raw.as_object() else {
        return Err("schema error: event payload must be a JSON object".to_string());
    };

    let event_type = obj
        .get("event_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "schema error: event_type is required".to_string())?;
    if !is_valid_event_type(event_type) {
        return Err(format!(
            "schema error: unsupported event_type '{event_type}'; allowed: {}",
            EVENT_TYPES.join(", ")
        ));
    }

    let occurred_dt = match obj.get("occurred_at").and_then(Value::as_str) {
        Some(raw_ts) if !raw_ts.trim().is_empty() => parse_rfc3339_utc(raw_ts)
            .map_err(|e| format!("schema error: occurred_at invalid ({e})"))?,
        Some(_) | None => recorded_at,
    };
    let occurred_at = utc_rfc3339(occurred_dt);
    let recorded_at_str = utc_rfc3339(recorded_at);
    let (_, local_date, iso_week) = tz.local_date_and_week(occurred_dt);

    // Source
    let source_obj = obj
        .get("source")
        .and_then(Value::as_object)
        .ok_or_else(|| "schema error: source object is required".to_string())?;
    let source_type = source_obj
        .get("type")
        .and_then(Value::as_str)
        .map(|s| sanitize_text(s, 120))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "schema error: source.type is required".to_string())?;
    let raw_source_id = source_obj
        .get("id")
        .and_then(Value::as_str)
        .map(|s| sanitize_text(s, 400))
        .unwrap_or_default();

    // Verification status
    let verification_status = match obj.get("verification_status") {
        None | Some(Value::Null) => "not_verified".to_string(),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                "not_verified".to_string()
            } else if is_valid_verification_status(trimmed) {
                trimmed.to_string()
            } else {
                return Err(format!(
                    "schema error: invalid verification-status '{trimmed}'; allowed: {}",
                    allowed_verification_statuses_text()
                ));
            }
        }
        Some(_) => {
            return Err(format!(
                "schema error: verification_status must be one of {}",
                allowed_verification_statuses_text()
            ));
        }
    };

    // Correlation
    let correlation = sanitize_correlation(obj.get("correlation"));

    // Outcome
    let outcome = sanitize_outcome(obj.get("outcome"));

    // Textual judgment fields
    let decision = opt_sanitized_multiline(obj.get("decision"), DETAIL_CHAR_LIMIT);
    let blocker = opt_sanitized_multiline(obj.get("blocker"), DETAIL_CHAR_LIMIT);
    let next_action = opt_sanitized_multiline(obj.get("next_action"), DETAIL_CHAR_LIMIT);
    let note = opt_sanitized_multiline(obj.get("note"), DETAIL_CHAR_LIMIT);

    // Work item
    let work_item = sanitize_optional_object(obj.get("work_item"));

    // Evidence
    let evidence = sanitize_evidence(obj.get("evidence"));

    // Metrics: drop if no evidence
    let metrics = if evidence.is_empty() {
        Vec::new()
    } else {
        sanitize_metrics(obj.get("metrics"))
    };

    // Structural research fields only
    let research = sanitize_research(obj.get("research"));

    // Artifacts
    let artifacts = sanitize_artifacts(obj.get("artifacts"));

    // Tags
    let tags = sanitize_tags(obj.get("tags"));

    // Superseding
    let supersedes_event_id = obj
        .get("supersedes_event_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| sanitize_text(s, 128));

    if event_type == "correction" && supersedes_event_id.is_none() {
        return Err("schema error: correction event requires supersedes_event_id".to_string());
    }

    // Event ID & synthesized source ID priority:
    // 1. explicit event_id (if starting with wj_)
    // 2. source.id non-empty -> deterministic ID from (project.id, event_type, source.type, source.id)
    // 3. correlation.operation_id -> source.id = op_<24>, event_id from same
    // 4. fallback stable content fields (EXCLUDING occurred_at / recorded_at) -> source.id = legacy-<24>
    let (final_source_id, event_id) = compute_event_identity(
        obj.get("event_id").and_then(Value::as_str),
        &project.id,
        event_type,
        &source_type,
        &raw_source_id,
        &correlation,
        &outcome,
        &verification_status,
        decision.as_deref(),
        blocker.as_deref(),
        next_action.as_deref(),
        &evidence,
        &metrics,
        supersedes_event_id.as_deref(),
    )?;

    let mut source_out = Map::new();
    source_out.insert("type".to_string(), Value::String(source_type));
    source_out.insert("id".to_string(), Value::String(final_source_id));
    for extra_key in ["record_path", "tool", "agent", "command", "detail", "scope"] {
        if let Some(v) = source_obj.get(extra_key) {
            if let Some(cleaned) = sanitize_value(v, Some(extra_key)) {
                if !is_empty_value(&cleaned) {
                    source_out.insert(extra_key.to_string(), cleaned);
                }
            }
        }
    }

    let mut project_out = Map::new();
    project_out.insert("id".to_string(), Value::String(project.id.clone()));
    project_out.insert("name".to_string(), Value::String(project.name.clone()));
    if !project.github_repository.is_empty() {
        project_out.insert(
            "github_repository".to_string(),
            Value::String(project.github_repository.clone()),
        );
    }
    if !project.origin_remote.is_empty() {
        project_out.insert(
            "origin_remote".to_string(),
            Value::String(project.origin_remote.clone()),
        );
    }

    let mut out = Map::new();
    out.insert(
        "schema_version".to_string(),
        Value::String(SCHEMA_VERSION.to_string()),
    );
    out.insert("event_id".to_string(), Value::String(event_id));
    out.insert(
        "event_type".to_string(),
        Value::String(event_type.to_string()),
    );
    out.insert("occurred_at".to_string(), Value::String(occurred_at));
    out.insert("recorded_at".to_string(), Value::String(recorded_at_str));
    out.insert("local_date".to_string(), Value::String(local_date));
    out.insert("iso_week".to_string(), Value::String(iso_week));
    out.insert("timezone".to_string(), Value::String(tz.name().to_string()));
    out.insert("project".to_string(), Value::Object(project_out));
    out.insert("source".to_string(), Value::Object(source_out));
    if !correlation.is_empty() {
        out.insert("correlation".to_string(), Value::Object(correlation));
    }
    if let Some(wi) = work_item {
        out.insert("work_item".to_string(), Value::Object(wi));
    }
    if !outcome.is_empty() {
        out.insert("outcome".to_string(), Value::Object(outcome));
    }
    out.insert(
        "verification_status".to_string(),
        Value::String(verification_status),
    );
    if let Some(d) = decision {
        out.insert("decision".to_string(), Value::String(d));
    }
    if let Some(b) = blocker {
        out.insert("blocker".to_string(), Value::String(b));
    }
    if let Some(na) = next_action {
        out.insert("next_action".to_string(), Value::String(na));
    }
    if let Some(n) = note {
        out.insert("note".to_string(), Value::String(n));
    }
    if !evidence.is_empty() {
        out.insert("evidence".to_string(), Value::Array(evidence));
    }
    if !metrics.is_empty() {
        out.insert("metrics".to_string(), Value::Array(metrics));
    }
    if let Some(r) = research {
        out.insert("research".to_string(), Value::Object(r));
    }
    if !artifacts.is_empty() {
        out.insert("artifacts".to_string(), Value::Array(artifacts));
    }
    if !tags.is_empty() {
        out.insert("tags".to_string(), Value::Array(tags));
    }
    if let Some(sup) = supersedes_event_id {
        out.insert("supersedes_event_id".to_string(), Value::String(sup));
    }

    let event = Value::Object(out);
    validate_canonical_event(&event)?;
    Ok(event)
}

#[allow(clippy::too_many_arguments)]
fn compute_event_identity(
    explicit_event_id: Option<&str>,
    project_id: &str,
    event_type: &str,
    source_type: &str,
    source_id: &str,
    correlation: &Map<String, Value>,
    outcome: &Map<String, Value>,
    verification_status: &str,
    decision: Option<&str>,
    blocker: Option<&str>,
    next_action: Option<&str>,
    evidence: &[Value],
    metrics: &[Value],
    supersedes_event_id: Option<&str>,
) -> Result<(String, String), String> {
    let resolved_source_id = if !source_id.is_empty() {
        source_id.to_string()
    } else if let Some(op_id) = correlation.get("operation_id").and_then(Value::as_str) {
        let mut basis = Map::new();
        basis.insert("project_id".into(), Value::String(project_id.into()));
        basis.insert("event_type".into(), Value::String(event_type.into()));
        basis.insert("source_type".into(), Value::String(source_type.into()));
        basis.insert("operation_id".into(), Value::String(op_id.into()));
        let bytes = canonical_json_bytes(&Value::Object(basis))?;
        format!("op_{}", &sha256_hex(&bytes)[..24])
    } else {
        let mut stable = Map::new();
        stable.insert("project_id".into(), Value::String(project_id.into()));
        stable.insert("event_type".into(), Value::String(event_type.into()));
        stable.insert("source_type".into(), Value::String(source_type.into()));
        stable.insert("outcome".into(), Value::Object(outcome.clone()));
        stable.insert(
            "verification_status".into(),
            Value::String(verification_status.into()),
        );
        if let Some(d) = decision {
            stable.insert("decision".into(), Value::String(d.into()));
        }
        if let Some(b) = blocker {
            stable.insert("blocker".into(), Value::String(b.into()));
        }
        if let Some(na) = next_action {
            stable.insert("next_action".into(), Value::String(na.into()));
        }
        if !evidence.is_empty() {
            stable.insert("evidence".into(), Value::Array(evidence.to_vec()));
        }
        if !metrics.is_empty() {
            stable.insert("metrics".into(), Value::Array(metrics.to_vec()));
        }
        if let Some(sup) = supersedes_event_id {
            stable.insert("supersedes_event_id".into(), Value::String(sup.into()));
        }
        let bytes = canonical_json_bytes(&Value::Object(stable))?;
        format!("legacy-{}", &sha256_hex(&bytes)[..24])
    };

    let event_id = if let Some(explicit) = explicit_event_id
        .map(str::trim)
        .filter(|s| s.starts_with("wj_"))
    {
        sanitize_text(explicit, 128)
    } else {
        let mut id_basis = Map::new();
        id_basis.insert("project_id".into(), Value::String(project_id.into()));
        id_basis.insert("event_type".into(), Value::String(event_type.into()));
        id_basis.insert("source_type".into(), Value::String(source_type.into()));
        id_basis.insert(
            "source_id".into(),
            Value::String(resolved_source_id.clone()),
        );
        let bytes = canonical_json_bytes(&Value::Object(id_basis))?;
        format!("wj_{}", &sha256_hex(&bytes)[..32])
    };

    Ok((resolved_source_id, event_id))
}

fn sanitize_correlation(val: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(obj) = val.and_then(Value::as_object) else {
        return out;
    };
    for (k, v) in obj {
        if crate::sanitize::is_forbidden_raw_key(k) {
            continue;
        }
        if let Some(cleaned) = sanitize_value(v, Some(k)) {
            if !is_empty_value(&cleaned) {
                out.insert(k.clone(), cleaned);
            }
        }
    }
    out
}

fn sanitize_outcome(val: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(obj) = val.and_then(Value::as_object) else {
        return out;
    };
    if let Some(summary) = obj.get("summary").and_then(Value::as_str) {
        let cleaned = sanitize_multiline(summary, DETAIL_CHAR_LIMIT);
        if !cleaned.is_empty() {
            out.insert("summary".into(), Value::String(cleaned));
        }
    }
    if let Some(status) = obj.get("status").and_then(Value::as_str) {
        let cleaned = sanitize_text(status, 64).to_ascii_lowercase();
        if !cleaned.is_empty() {
            out.insert("status".into(), Value::String(cleaned));
        }
    }
    if let Some(exit_code) = obj.get("exit_code").and_then(Value::as_i64) {
        out.insert("exit_code".into(), Value::Number(exit_code.into()));
    }
    if let Some(duration_ms) = obj.get("duration_ms").and_then(Value::as_f64) {
        if duration_ms.is_finite() {
            if let Some(num) = serde_json::Number::from_f64(duration_ms) {
                out.insert("duration_ms".into(), Value::Number(num));
            }
        }
    }
    for (k, v) in obj {
        if matches!(
            k.as_str(),
            "summary" | "status" | "exit_code" | "duration_ms"
        ) || crate::sanitize::is_forbidden_raw_key(k)
        {
            continue;
        }
        if let Some(cleaned) = sanitize_value(v, Some(k)) {
            if !is_empty_value(&cleaned) {
                out.insert(k.clone(), cleaned);
            }
        }
    }
    out
}

fn sanitize_optional_object(val: Option<&Value>) -> Option<Map<String, Value>> {
    let obj = val.and_then(Value::as_object)?;
    let mut out = Map::new();
    for (k, v) in obj {
        if crate::sanitize::is_forbidden_raw_key(k) {
            continue;
        }
        if let Some(cleaned) = sanitize_value(v, Some(k)) {
            if !is_empty_value(&cleaned) {
                out.insert(k.clone(), cleaned);
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn sanitize_evidence(val: Option<&Value>) -> Vec<Value> {
    let Some(items) = val.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items.iter().take(64) {
        if let Some(obj) = item.as_object() {
            let mut row = Map::new();
            for (k, v) in obj {
                if crate::sanitize::is_forbidden_raw_key(k) {
                    continue;
                }
                if let Some(cleaned) = sanitize_value(v, Some(k)) {
                    if !is_empty_value(&cleaned) {
                        row.insert(k.clone(), cleaned);
                    }
                }
            }
            if !row.is_empty() {
                out.push(Value::Object(row));
            }
        }
    }
    out
}

fn sanitize_metrics(val: Option<&Value>) -> Vec<Value> {
    let Some(items) = val.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items.iter().take(64) {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(name) = obj
            .get("name")
            .and_then(Value::as_str)
            .map(|s| sanitize_text(s, 120))
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(num) = obj.get("value").and_then(|v| {
            if let Some(i) = v.as_i64() {
                Some(Value::Number(i.into()))
            } else if let Some(f) = v.as_f64() {
                if f.is_finite() {
                    serde_json::Number::from_f64(f).map(Value::Number)
                } else {
                    None
                }
            } else {
                None
            }
        }) else {
            continue;
        };
        let mut row = Map::new();
        row.insert("name".into(), Value::String(name));
        row.insert("value".into(), num);
        for field in [
            "unit",
            "scope",
            "split",
            "dataset",
            "higher_is_better",
            "direction",
        ] {
            if let Some(v) = obj.get(field) {
                if let Some(cleaned) = sanitize_value(v, Some(field)) {
                    if !is_empty_value(&cleaned) {
                        row.insert(field.into(), cleaned);
                    }
                }
            }
        }
        out.push(Value::Object(row));
    }
    out
}

fn sanitize_research(val: Option<&Value>) -> Option<Map<String, Value>> {
    let obj = val.and_then(Value::as_object)?;
    let mut out = Map::new();
    for field in [
        "question",
        "hypothesis",
        "findings",
        "confidence",
        "open_questions",
        "next_step",
        "conclusion",
        "sources",
    ] {
        if let Some(v) = obj.get(field) {
            if let Some(cleaned) = sanitize_value(v, Some(field)) {
                if !is_empty_value(&cleaned) {
                    out.insert(field.into(), cleaned);
                }
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn sanitize_artifacts(val: Option<&Value>) -> Vec<Value> {
    let Some(items) = val.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items.iter().take(64) {
        if let Some(s) = item.as_str() {
            let cleaned = sanitize_text(s, VALUE_CHAR_LIMIT);
            if !cleaned.is_empty() {
                out.push(Value::String(cleaned));
            }
        } else if let Some(cleaned) = sanitize_value(item, None) {
            if !is_empty_value(&cleaned) {
                out.push(cleaned);
            }
        }
    }
    out
}

fn sanitize_tags(val: Option<&Value>) -> Vec<Value> {
    let Some(items) = val.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items.iter().take(32) {
        if let Some(s) = item.as_str() {
            let cleaned = sanitize_text(s, 80);
            if !cleaned.is_empty()
                && !out
                    .iter()
                    .any(|v: &Value| v.as_str() == Some(cleaned.as_str()))
            {
                out.push(Value::String(cleaned));
            }
        }
    }
    out
}

fn opt_sanitized_multiline(val: Option<&Value>, limit: usize) -> Option<String> {
    let s = val.and_then(Value::as_str)?;
    let cleaned = sanitize_multiline(s, limit);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

pub fn validate_canonical_event(event: &Value) -> Result<(), String> {
    let Some(obj) = event.as_object() else {
        return Err("schema error: canonical event must be an object".to_string());
    };
    if obj.get("schema_version").and_then(Value::as_str) != Some(SCHEMA_VERSION) {
        return Err("schema error: unsupported or missing schema_version".to_string());
    }
    let event_id = obj
        .get("event_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "schema error: missing event_id".to_string())?;
    if !event_id.starts_with("wj_") || event_id.len() < 10 {
        return Err("schema error: invalid event_id format".to_string());
    }
    let event_type = obj
        .get("event_type")
        .and_then(Value::as_str)
        .ok_or_else(|| "schema error: missing event_type".to_string())?;
    if !is_valid_event_type(event_type) {
        return Err(format!("schema error: invalid event_type '{event_type}'"));
    }
    let occurred_at = obj
        .get("occurred_at")
        .and_then(Value::as_str)
        .ok_or_else(|| "schema error: missing occurred_at".to_string())?;
    parse_rfc3339_utc(occurred_at).map_err(|e| format!("schema error: {e}"))?;
    let vs = obj
        .get("verification_status")
        .and_then(Value::as_str)
        .ok_or_else(|| "schema error: missing verification_status".to_string())?;
    if !is_valid_verification_status(vs) {
        return Err(format!(
            "schema error: invalid verification_status '{vs}'; allowed: {}",
            allowed_verification_statuses_text()
        ));
    }
    let src = obj
        .get("source")
        .and_then(Value::as_object)
        .ok_or_else(|| "schema error: missing source".to_string())?;
    if src
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
        || src
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    {
        return Err("schema error: source.type and source.id must be non-empty".to_string());
    }
    let proj = obj
        .get("project")
        .and_then(Value::as_object)
        .ok_or_else(|| "schema error: missing project".to_string())?;
    if proj
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
        || proj
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    {
        return Err("schema error: project.id and project.name must be non-empty".to_string());
    }
    if event_type == "correction" {
        let sup = obj
            .get("supersedes_event_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !sup.starts_with("wj_") {
            return Err(
                "schema error: correction requires a valid supersedes_event_id".to_string(),
            );
        }
    }
    Ok(())
}
