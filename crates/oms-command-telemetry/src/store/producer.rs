use super::*;
use crate::value::{encode, obj};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Explicit producer inputs. `env_overrides` uses the canonical OMS_* names;
/// an empty override suppresses inheritance without changing process globals.
#[derive(Debug, Clone)]
pub struct AppendArtifactRequest {
    pub kind: String,
    pub provider: String,
    pub exit_code: i32,
    pub artifact: Option<PathBuf>,
    pub patch: Option<PathBuf>,
    pub prompt: Option<PathBuf>,
    pub verify_exit: Option<i32>,
    pub source_artifact: Option<PathBuf>,
    pub task_id: Option<String>,
    pub index: Option<PathBuf>,
    pub task_goal: Option<String>,
    pub env_overrides: BTreeMap<String, String>,
    pub inherit_env: bool,
    pub append: AppendOptions,
}
impl Default for AppendArtifactRequest {
    fn default() -> Self {
        Self {
            kind: String::new(),
            provider: String::new(),
            exit_code: 0,
            artifact: None,
            patch: None,
            prompt: None,
            verify_exit: None,
            source_artifact: None,
            task_id: None,
            index: None,
            task_goal: None,
            env_overrides: BTreeMap::new(),
            inherit_env: true,
            append: AppendOptions::default(),
        }
    }
}
pub(super) fn random_hex<const N: usize>() -> Result<String, StoreError> {
    let mut raw = [0u8; N];
    let fd = rustix::fs::open(
        "/dev/urandom",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    std::fs::File::from(fd).read_exact(&mut raw)?;
    // Event/operation identifiers use UUIDv4's version and variant bits.
    if N == 16 {
        raw[6] = (raw[6] & 0x0f) | 0x40;
        raw[8] = (raw[8] & 0x3f) | 0x80;
    }
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}
pub(super) fn now_text() -> Result<String, StoreError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::Io)?
        .as_secs();
    let dt = chrono::DateTime::from_timestamp(i64::try_from(now).map_err(|_| StoreError::Io)?, 0)
        .ok_or(StoreError::Io)?;
    Ok(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}
fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
fn hex(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn bounded_model(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 160 && !value.contains(['\r', '\n', '\t'])
}
fn field(row: &mut V, name: &str, value: String) {
    if !value.is_empty() {
        row[name] = value.into();
    }
}
fn pick(get: &impl Fn(&str) -> String, names: &[&str]) -> String {
    names
        .iter()
        .map(|name| get(name))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}
fn safe_field(row: &mut V, key: &str, value: String) {
    if safe_id(&value) {
        field(row, key, value);
    }
}

/// Register owner results with fresh lineage while retaining clean run/trace context.
pub fn register_panel_result(
    repo: &Path,
    owner: &str,
    task_id: &str,
    artifact_relative: &Path,
    interrupted: &AtomicBool,
) -> Result<String, StoreError> {
    if !safe_id(task_id) || artifact_relative.is_absolute() {
        return Err(StoreError::Invalid(
            "panel result requires task id and relative artifact",
        ));
    }
    let mut request = AppendArtifactRequest {
        kind: "panel-result".into(),
        provider: owner.into(),
        artifact: Some(artifact_relative.to_path_buf()),
        task_id: Some(task_id.into()),
        ..AppendArtifactRequest::default()
    };
    for key in [
        "OMS_INDEX_PLAN_ID",
        "OMS_INDEX_ACTIVE_TASK_ID",
        "OMS_INDEX_PARENT_EVENT_ID",
        "OMS_PARENT_EVENT_ID",
        "OMS_INDEX_ATTEMPT_ID",
        "OMS_ATTEMPT_ID",
        "OMS_LAST_ATTEMPT_ID",
        "OMS_INDEX_OPERATION_ID",
        "OMS_OPERATION_ID",
        "OMS_HARNESS_CALL_ID",
    ] {
        request.env_overrides.insert(key.into(), String::new());
    }
    append_artifact_index(repo, &request, interrupted)
}

/// Hash payloads and append the canonical row under one writer lock.
/// Returns the durable event id; accounting errors only omit metric fields.
pub fn append_artifact_index(
    repo: &Path,
    request: &AppendArtifactRequest,
    interrupted: &AtomicBool,
) -> Result<String, StoreError> {
    cancelled(interrupted)?;
    if request.kind.is_empty() || request.exit_code < 0 {
        return Err(StoreError::Invalid(
            "kind and non-negative exit are required",
        ));
    }
    let inherited: BTreeMap<String, String> = if request.inherit_env {
        std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect()
    } else {
        BTreeMap::new()
    };
    let get = |key: &str| {
        request
            .env_overrides
            .get(key)
            .or_else(|| inherited.get(key))
            .cloned()
            .unwrap_or_default()
    };
    let root = Root::open(repo)?;
    let index_env = get("OMS_ARTIFACT_INDEX");
    let index = request
        .index
        .as_deref()
        .or_else(|| (!index_env.is_empty()).then(|| Path::new(&index_env)));
    let target = canonical_index(repo, index, false, false)?;
    let (keep, high) = retention_limits(request.append.keep, request.append.high_water)?;
    let _guard = acquire(&target, interrupted)?;
    let event = format!("evt_{}", random_hex::<16>()?);
    let operation = pick(
        &get,
        &[
            "OMS_INDEX_OPERATION_ID",
            "OMS_OPERATION_ID",
            "OMS_HARNESS_CALL_ID",
        ],
    );
    let operation = if safe_id(&operation) {
        operation
    } else {
        format!("op_{}", random_hex::<16>()?)
    };
    let mut row = obj!("schema":1, "event_id":event.clone(), "operation_id":operation, "ts":now_text()?, "kind":request.kind.clone(), "provider":request.provider.clone(), "exit":request.exit_code);
    context(&root, &mut row, request, &get)?;
    let mut primary = None;
    for (label, path) in [
        ("artifact", request.artifact.as_deref()),
        ("patch", request.patch.as_deref()),
        ("source", request.source_artifact.as_deref()),
    ] {
        if let Some(path) = path.filter(|p| !p.as_os_str().is_empty()) {
            let digest = path_fields(&root, repo, &mut row, label, path)?;
            if primary.is_none() && label != "source" {
                primary = digest;
            }
        }
    }
    row["artifact_id"] = format!(
        "sha256:{}",
        primary.unwrap_or_else(|| format!("{:x}", Sha256::digest(event.as_bytes())))
    )
    .into();
    if let Some(prompt) = &request.prompt {
        // Prompt metadata is advisory. Never persist the prompt text or its path.
        if let Ok(body) = payload(&root, repo, prompt) {
            row["prompt_sha256"] = format!("{:x}", Sha256::digest(&body)).into();
            row["prompt_bytes"] = body.len().into();
        }
    }
    if let Some(exit) = request.verify_exit {
        row["verify_exit"] = exit.into();
    }
    if let Some(goal) = &request.task_goal {
        row["task_goal"] = goal.chars().take(200).collect::<String>().into();
    }
    if row["artifact"].truth()
        && let Ok(metrics) = crate::metrics::metrics(root.physical_root(), &row)
    {
        if let Some(duration) = metrics.duration.filter(|v| v.is_finite()) {
            row["duration_s"] = duration.into();
        }
        let attribution = if metrics.ambiguous {
            "ambiguous"
        } else if metrics.served_model.is_some() {
            "transport"
        } else if metrics.configured_model.is_some() {
            "configured-default"
        } else {
            "unknown"
        };
        row["model_attribution"] = attribution.into();
        for (key, value) in [
            ("tokens", metrics.tokens),
            ("provider_usage", metrics.detail),
            ("served_model", metrics.served_model),
            ("configured_model", metrics.configured_model),
        ] {
            if let Some(value) = value {
                row[key] = value;
            }
        }
        if let Some(cost) = metrics.cost.filter(|v| v.is_finite()) {
            row["cost_usd"] = cost.into();
        }
    }
    let encoded = encode(&row).map_err(|_| StoreError::Invalid("cannot encode artifact event"))?;
    row_check(&encoded)?;
    cancelled(interrupted)?;
    ensure_ignore(root.physical_root(), interrupted)?;
    append_locked(
        &root,
        &target,
        &[&encoded],
        keep,
        high,
        request.append.expected.as_deref(),
        std::slice::from_ref(&event),
        interrupted,
    )?;
    Ok(event)
}
fn row_check(encoded: &[u8]) -> Result<(), StoreError> {
    row(encoded).map(|_| ())
}

pub(super) fn payload(root: &Root, repo: &Path, path: &Path) -> Result<Vec<u8>, StoreError> {
    if let Ok(target) = paths::target(root, repo, path) {
        paths::validate(root, &target, false)?;
        return Ok(root.read_path(
            target
                .strip_prefix(root.physical_root())
                .map_err(|_| StoreError::Changed)?,
            MAX_INPUT_BYTES,
            oms_safe_fs::Policy {
                single_link: true,
                owner: Some(rustix::process::getuid().as_raw()),
                max_components: None,
            },
        )?);
    }
    if !path.is_absolute() {
        return Err(StoreError::Invalid("invalid payload path"));
    }
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?;
    let before = rustix::fs::fstat(&fd)?;
    if !paths::regular(&before)
        || before.st_size < 0
        || before.st_size as u64 > MAX_INPUT_BYTES as u64
    {
        return Err(StoreError::Invalid("unsafe or oversized external payload"));
    }
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    (&mut file)
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let after = rustix::fs::fstat(&file)?;
    if !paths::unchanged(&before, &after) || bytes.len() as u64 != before.st_size as u64 {
        return Err(StoreError::Changed);
    }
    Ok(bytes)
}
fn path_fields(
    root: &Root,
    repo: &Path,
    row: &mut V,
    label: &str,
    path: &Path,
) -> Result<Option<String>, StoreError> {
    let internal = paths::target(root, repo, path);
    let digest = payload(root, repo, path).map(|bytes| format!("{:x}", Sha256::digest(bytes)));
    if let Ok(target) = internal {
        let digest = digest?;
        let relative = target
            .strip_prefix(root.physical_root())
            .map_err(|_| StoreError::Invalid("external payload"))?;
        let text = relative
            .to_str()
            .ok_or(StoreError::Invalid("payload path is not UTF-8"))?;
        row[label] = text.into();
        row[&format!("{label}_sha256")] = digest.clone().into();
        Ok(Some(digest))
    } else {
        // External paths are never owned or persisted, only their basename and hash.
        let mut external =
            obj!("name":path.file_name().unwrap_or_default().as_bytes().to_vec(), "owned":false);
        let digest = digest.ok();
        if let Some(digest) = &digest {
            external["sha256"] = digest.clone().into();
            if label == "patch" {
                row["patch_sha256"] = digest.clone().into();
            }
        }
        row[&format!("{label}_external")] = external;
        Ok(digest)
    }
}

fn context(
    root: &Root,
    row: &mut V,
    request: &AppendArtifactRequest,
    get: &impl Fn(&str) -> String,
) -> Result<(), StoreError> {
    let mut run = pick(get, &["OMS_INDEX_RUN_ID", "OMS_RUN_ID"]);
    if !safe_id(&run) {
        run.clear();
        if let Ok(body) = root.read_no_follow(
            Path::new(".oms/runs/CURRENT"),
            ByteReadOptions {
                missing_ok: false,
                max_bytes: MAX_ROW_BYTES,
            },
        ) && let Ok(text) = std::str::from_utf8(&body)
        {
            let fields: Vec<_> = text.split_whitespace().collect();
            let ttl = get("OMS_RUN_CURRENT_TTL").parse::<i128>().unwrap_or(86400);
            if fields.len() > 1
                && fields[1].bytes().all(|b| b.is_ascii_digit())
                && let Ok(stamp) = fields[1].parse::<i128>()
            {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| StoreError::Io)?
                    .as_secs() as i128;
                if now - stamp <= ttl && safe_id(fields[0]) {
                    run = fields[0].into();
                }
            }
        }
    }
    safe_field(row, "run_id", run);
    let task = request
        .task_id
        .clone()
        .unwrap_or_else(|| pick(get, &["OMS_INDEX_TASK_ID", "OMS_TASK_ID"]));
    safe_field(row, "task_id", task);
    for (key, names) in [
        ("active_task_id", &["OMS_INDEX_ACTIVE_TASK_ID"][..]),
        (
            "delegation_id",
            &["OMS_INDEX_DELEGATION_ID", "OMS_DELEGATION_ID"][..],
        ),
        (
            "attempt_id",
            &[
                "OMS_INDEX_ATTEMPT_ID",
                "OMS_ATTEMPT_ID",
                "OMS_LAST_ATTEMPT_ID",
            ][..],
        ),
        (
            "parent_event_id",
            &["OMS_INDEX_PARENT_EVENT_ID", "OMS_PARENT_EVENT_ID"][..],
        ),
    ] {
        safe_field(row, key, pick(get, names));
    }
    let plan = get("OMS_INDEX_PLAN_ID");
    if !plan.is_empty() {
        if !plan.starts_with("plan_") || !hex(&plan[5..], 32, 32) {
            return Err(StoreError::Invalid(
                "plan_id must be immutable plan lineage",
            ));
        }
        row["plan_id"] = plan.into();
    }
    field(row, "base_sha", get("OMS_INDEX_BASE_SHA"));
    if request.kind == "delegate" {
        safe_field(
            row,
            "role",
            pick(get, &["OMS_INDEX_ROLE", "OMS_MODEL_ROLE"]),
        );
    }
    for (key, vars) in [
        (
            "requested_model",
            &["OMS_INDEX_REQUESTED_MODEL", "OMS_MODEL_PRIMARY"][..],
        ),
        (
            "selected_model",
            &["OMS_INDEX_SELECTED_MODEL", "OMS_MODEL_SELECTED"][..],
        ),
        (
            "fallback_model",
            &["OMS_INDEX_FALLBACK_MODEL", "OMS_MODEL_FALLBACK"][..],
        ),
    ] {
        let value = pick(get, vars);
        if bounded_model(&value) {
            field(row, key, value);
        }
    }
    let model_class = pick(get, &["OMS_INDEX_MODEL_CLASS", "OMS_MODEL_RESOLVED_CLASS"]);
    if [
        "explicit",
        "provider-default",
        "role-default",
        "fast",
        "balanced",
        "deep",
    ]
    .contains(&model_class.as_str())
    {
        field(row, "model_class", model_class);
    }
    row["fallback_used"] =
        (pick(get, &["OMS_INDEX_FALLBACK_USED", "OMS_MODEL_FALLBACK_USED"]) == "1").into();
    let reason = pick(
        get,
        &["OMS_INDEX_FALLBACK_REASON", "OMS_MODEL_FALLBACK_REASON"],
    );
    if [
        "capacity",
        "capacity-no-fallback",
        "capacity-dirty-worktree",
        "model-unavailable",
        "policy-declined",
        "model-safeguard",
        "provider-model-unavailable",
        "provider-capacity",
        "provider-down",
    ]
    .contains(&reason.as_str())
    {
        field(row, "fallback_reason", reason);
    }
    for (key, vars) in [
        (
            "reasoning_effort",
            &["OMS_INDEX_REASONING_EFFORT", "OMS_REASONING_RESOLVED"][..],
        ),
        (
            "selected_reasoning_effort",
            &[
                "OMS_INDEX_SELECTED_REASONING_EFFORT",
                "OMS_REASONING_SELECTED",
            ][..],
        ),
        (
            "fallback_reasoning_effort",
            &[
                "OMS_INDEX_FALLBACK_REASONING_EFFORT",
                "OMS_REASONING_FALLBACK",
            ][..],
        ),
    ] {
        let value = pick(get, vars);
        if ["low", "medium", "high", "xhigh", "max", "ultra"].contains(&value.as_str()) {
            field(row, key, value);
        }
    }
    let quality = get("OMS_INDEX_ANSWER_QUALITY");
    if ["empty", "truncated", "blocked", "invalid-deliberation"].contains(&quality.as_str()) {
        field(row, "answer_quality", quality);
    }
    for (key, env, min, max) in [
        (
            "context_manifest_digest",
            "OMS_INDEX_CONTEXT_MANIFEST_DIGEST",
            16,
            64,
        ),
        (
            "context_bundle_sha256",
            "OMS_INDEX_CONTEXT_BUNDLE_SHA256",
            64,
            64,
        ),
    ] {
        let value = get(env);
        if !value.is_empty() {
            if !hex(&value, min, max) {
                return Err(StoreError::Invalid("invalid context digest"));
            }
            field(row, key, value);
        }
    }
    for (key, env) in [
        ("context_selected_bytes", "OMS_INDEX_CONTEXT_SELECTED_BYTES"),
        ("context_debt", "OMS_INDEX_CONTEXT_DEBT"),
    ] {
        let value = get(env);
        if !value.is_empty() {
            if !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(StoreError::Invalid(
                    "context count must be non-negative integer",
                ));
            }
            row[key] = V::Int(value.trim_start_matches('0').to_string());
            if row[key] == V::Int(String::new()) {
                row[key] = 0.into();
            }
        }
    }
    structured(row, get)?;
    let trace = get("OMS_TRACEPARENT");
    let parts: Vec<_> = trace.split('-').collect();
    if parts.len() == 4
        && hex(parts[0], 2, 2)
        && parts[0] != "ff"
        && hex(parts[1], 32, 32)
        && parts[1].bytes().any(|b| b != b'0')
        && hex(parts[2], 16, 16)
        && parts[2].bytes().any(|b| b != b'0')
        && hex(parts[3], 2, 2)
    {
        row["trace_id"] = parts[1].into();
        row["parent_span_id"] = parts[2].into();
        row["trace_flags"] = parts[3].into();
        row["span_id"] = random_hex::<8>()?.into();
    }
    Ok(())
}
fn structured(row: &mut V, get: &impl Fn(&str) -> String) -> Result<(), StoreError> {
    let review = get("OMS_INDEX_REVIEW_OUTCOME_JSON");
    if !review.is_empty() {
        if review.len() > 16384 {
            return Err(StoreError::Invalid("review payload exceeds 16KiB"));
        }
        let value = parse(review.as_bytes(), true)
            .map_err(|_| StoreError::Invalid("invalid review JSON"))?;
        if !matches!(value, V::Object(_)) || !matches!(value["seats"], V::Array(_)) {
            return Err(StoreError::Invalid("review payload requires seats"));
        }
        row["review"] = value;
    }
    let scope = get("OMS_INDEX_REVIEW_SCOPE_JSON");
    if [Some("review-outcome"), Some("review-incremental")].contains(&row["kind"].text())
        && scope.len() <= 2048
        && let Ok(value) = parse(scope.as_bytes(), true)
        && matches!(value, V::Object(_))
    {
        row["review_scope"] = value;
    }
    let covers = get("OMS_INDEX_COVERS_JSON");
    if !covers.is_empty() {
        if covers.len() > 8192 {
            return Err(StoreError::Invalid("coverage payload exceeds 8KiB"));
        }
        let value = parse(covers.as_bytes(), true)
            .map_err(|_| StoreError::Invalid("invalid coverage JSON"))?;
        if !matches!(value, V::Object(_)) {
            return Err(StoreError::Invalid("coverage must be object"));
        }
        if let Some(ids) = value.get("covers") {
            if !matches!(ids, V::Array(_))
                || ids.array().len() > 16
                || ids.array().iter().any(|id| !id.text().is_some_and(safe_id))
            {
                return Err(StoreError::Invalid("invalid coverage ids"));
            }
            if !ids.array().is_empty() {
                row["covers"] = ids.clone();
            }
        }
        if value["status"].truth() {
            if ![
                Some("verified"),
                Some("failed"),
                Some("inconclusive"),
                Some("skipped_with_reason"),
            ]
            .contains(&value["status"].text())
            {
                return Err(StoreError::Invalid("invalid coverage status"));
            }
            row["status"] = value["status"].clone();
        }
        if value["scope_digest"].truth() {
            if !value["scope_digest"].text().is_some_and(|s| hex(s, 16, 64)) {
                return Err(StoreError::Invalid("invalid coverage scope digest"));
            }
            row["scope_digest"] = value["scope_digest"].clone();
        }
        if value["dependency_digests"].truth() {
            let V::Object(deps) = &value["dependency_digests"] else {
                return Err(StoreError::Invalid("invalid dependency digests"));
            };
            if deps.len() > 64
                || deps.iter().any(|(k, v)| {
                    let Ok(path) = std::str::from_utf8(k) else {
                        return true;
                    };
                    path.is_empty()
                        || path.chars().count() > 300
                        || path.starts_with('/')
                        || path.split('/').any(|s| s == "..")
                        || !v.text().is_some_and(|s| hex(s, 16, 64))
                })
            {
                return Err(StoreError::Invalid("invalid dependency digests"));
            }
            row["dependency_digests"] = value["dependency_digests"].clone();
        }
    }
    Ok(())
}
