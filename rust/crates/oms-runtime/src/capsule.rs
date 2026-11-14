//! Portable continuity is schema-checked advisory data and never authority.
use crate::storage::Runtime;
use crate::value::*;
use crate::{Result, error, require, status};
use std::path::Path;
use std::sync::OnceLock;
const MAX: usize = 1024 * 1024;
fn value_pattern() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(||regex::Regex::new(&concat!(r"(?i)(?:api[_-]?key|access[_-]?to",r"ken|client[_-]?sec",r"ret|pass",r"word|private[_-]?key)\s*[:=]|-----BE",r"GIN [A-Z ]+PRIVATE KEY-----|\b(?:ghp|github_pat|sk)-[A-Za-z0-9_-]{12,}\b|\bBearer\s+[A-Za-z0-9._~+/-]{12,}={0,2}\b")).unwrap())
}
fn path_pattern() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(
            r"(?P<prefix>^|[^A-Za-z0-9_.:/-])(?:/(?:[^\s/]+/)+[^\s]+|[A-Za-z]:[\\/][^\s]+)",
        )
        .unwrap()
    })
}
pub fn sensitive_value(s: &str) -> bool {
    value_pattern().is_match(s)
}
pub fn sensitive(s: &str) -> bool {
    sensitive_value(s) || path_pattern().is_match(s)
}
fn blocked(key: &str) -> bool {
    let lower = key.to_lowercase();
    let words = [
        "approval",
        "approval_id",
        "lease",
        "lease_id",
        "command",
        "raw_command",
        "prompt",
        "transcript",
        "log",
        "log_file",
        "artifact",
        "patch",
        "repo",
        "cwd",
        "environment",
        "env",
        "source_path",
    ];
    if words.contains(&lower.as_str()) {
        return true;
    }
    let parts = [
        concat!("to", "ken"),
        concat!("sec", "ret"),
        concat!("pass", "word"),
        concat!("creden", "tial"),
    ];
    parts.iter().any(|p| lower.contains(p))
        || [
            "api_key",
            "api-key",
            "private_key",
            "private-key",
            "auth_key",
            "auth-key",
        ]
        .iter()
        .any(|p| lower.contains(p))
}
pub fn sanitize(value: &Value, key: &str) -> Option<Value> {
    if blocked(key) || matches!(value, Value::Null) {
        return None;
    }
    Some(match value {
        Value::Object(v) => Value::Object(
            v.iter()
                .filter_map(|(k, v)| {
                    let key = std::str::from_utf8(k).ok()?;
                    sanitize(v, key).map(|v| (k.clone(), v))
                })
                .collect(),
        ),
        Value::Array(v) => a(v.iter().filter_map(|v| sanitize(v, key))),
        Value::String(v) => {
            let raw = String::from_utf8_lossy(v);
            let s = bounded(&raw, 1000);
            let s = path_pattern().replace_all(&s, "${prefix}[absolute-path-omitted]");
            string(if sensitive_value(&s) {
                "[sensitive-content-omitted]"
            } else {
                &s
            })
        }
        _ => value.clone(),
    })
}
fn schema_fields(location: &str) -> Option<&'static str> {
    Some(match location {
        "envelope" => "schema capsule_id digest payload",
        "envelope.payload" => "schema created_at authority_transfer project contract continuity",
        "envelope.payload.project" => "head state_digest",
        "envelope.payload.contract" => "objective scope criteria coverage complete budget",
        "envelope.payload.contract.objective" => "text source",
        "envelope.payload.contract.scope" => {
            "allowed allowed_source forbidden authoritative layers"
        }
        "envelope.payload.contract.scope.layers[]" => "source allowed forbidden",
        "envelope.payload.contract.criteria[]" => "id text source status evidence_refs",
        "envelope.payload.continuity" => "next_action_ids failures warnings task_state next_step",
        "envelope.payload.continuity.failures[]" => "id kind summary classification",
        "envelope.payload.continuity.failures[].classification" => {
            "code retryable fallback_allowed context_escalation_allowed recovery matched exit"
        }
        _ => return None,
    })
}
fn validate_fields(value: &Value, location: &str) -> Result<()> {
    if matches!(value, Value::Null) {
        return Ok(());
    }
    if location == "envelope.payload.contract.budget" {
        for (k, v) in value.object()? {
            let k = std::str::from_utf8(k).map_err(error)?;
            require(
                !blocked(k)
                    && regex::Regex::new(r"^[A-Za-z][A-Za-z0-9_.-]*$")
                        .unwrap()
                        .is_match(k),
                "portable capsule has an unknown field",
            )?;
            require(
                !matches!(v, Value::Array(_) | Value::Object(_)),
                "portable capsule expected a scalar",
            )?;
        }
        return Ok(());
    }
    if let Some(fields) = schema_fields(location) {
        for (k, v) in value.object()? {
            let name = std::str::from_utf8(k).map_err(error)?;
            require(
                fields.split_whitespace().any(|s| s == name),
                format!("portable capsule has an unknown field at {location}"),
            )?;
            validate_fields(v, &format!("{location}.{name}"))?;
        }
        return Ok(());
    }
    let list_field = location.ends_with(".criteria")
        || location.ends_with(".layers")
        || location.ends_with(".failures")
        || location.ends_with(".allowed")
        || location.ends_with(".forbidden")
        || location.ends_with(".evidence_refs")
        || location.ends_with(".next_action_ids")
        || location.ends_with(".warnings");
    if list_field {
        for v in value.array()? {
            validate_fields(v, &format!("{location}[]"))?
        }
    } else {
        require(
            !matches!(value, Value::Array(_) | Value::Object(_)),
            format!("portable capsule expected a scalar at {location}"),
        )?;
    }
    Ok(())
}
pub fn validate(raw: &Value) -> Result<()> {
    require(
        raw.get("schema").is_int(1),
        "unsupported portable capsule envelope schema",
    )?;
    let payload = raw.get("payload");
    require(
        payload.get("schema").is_int(1),
        "portable capsule payload is invalid",
    )?;
    validate_fields(raw, "envelope")?;
    require(
        payload.get("authority_transfer") == &b(false),
        "a portable capsule must not transfer authority",
    )?;
    let digest = hash(payload)?;
    require(
        hex(raw.text("digest"), 64) && raw.text("digest") == digest,
        "portable capsule digest mismatch",
    )?;
    require(
        raw.text("capsule_id") == format!("capsule-{}", &digest[..32]),
        "portable capsule id does not match its digest",
    )?;
    let bytes = canonical(raw)?;
    require(bytes.len() <= MAX, "portable capsule exceeds 1048576 bytes")?;
    require(
        !sensitive(std::str::from_utf8(&bytes).map_err(error)?),
        "portable capsule contains sensitive-looking content",
    )
}
pub fn build(rt: &Runtime<'_>) -> Result<Value> {
    let e = crate::projection::envelope(rt)?;
    let criteria = rows(e.get("criteria"))
        .iter()
        .map(|c| {
            let mut row = pick(c, &["id", "text", "source", "status"]);
            row.set(
                "evidence_refs",
                a(rows(c.get("evidence"))
                    .iter()
                    .map(|v| v.get("evidence_ref").clone())),
            )?;
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    let payload = object([
        ("schema", number(1)),
        ("created_at", string(&rt.now)),
        ("authority_transfer", b(false)),
        (
            "project",
            object([
                ("head", e.get("repo").get("head").clone()),
                ("state_digest", e.get("state_digest").clone()),
            ]),
        ),
        (
            "contract",
            object([
                ("objective", e.get("objective").clone()),
                ("scope", e.get("scope").clone()),
                ("criteria", a(criteria)),
                ("coverage", e.get("evidence").get("coverage").clone()),
                ("complete", e.get("evidence").get("complete").clone()),
                ("budget", e.get("budget").clone()),
            ]),
        ),
        (
            "continuity",
            object([
                (
                    "next_action_ids",
                    a(rows(e.get("next_actions"))
                        .iter()
                        .map(|v| v.get("id").clone())),
                ),
                (
                    "failures",
                    a(rows(e.get("failures"))
                        .iter()
                        .map(|v| pick(v, &["id", "kind", "classification", "summary"]))),
                ),
                ("warnings", e.get("warnings").clone()),
                ("task_state", e.get("task").get("state").clone()),
                ("next_step", e.get("task").get("next").clone()),
            ]),
        ),
    ]);
    let payload = sanitize(&payload, "")
        .ok_or_else(|| error("portable capsule sanitization produced an invalid payload"))?;
    let digest = hash(&payload)?;
    let capsule = object([
        ("schema", number(1)),
        ("capsule_id", string(format!("capsule-{}", &digest[..32]))),
        ("digest", string(digest)),
        ("payload", payload),
    ]);
    validate(&capsule)?;
    Ok(capsule)
}
pub fn export(rt: &Runtime<'_>, output: Option<&Path>) -> Result<Value> {
    let raw = build(rt)?;
    let destination = output.map(Path::to_path_buf).unwrap_or_else(|| {
        format!(".oms/portable/capsules/{}.json", raw.text("capsule_id")).into()
    });
    if output.is_some() {
        rt.write_json_output(&destination, &raw)?;
    } else {
        rt.write_json(&destination, &raw)?;
    }
    Ok(object([
        ("schema", number(1)),
        ("capsule_id", raw.get("capsule_id").clone()),
        ("digest", raw.get("digest").clone()),
        ("path", rt.output_label(&destination)),
        ("authority_transfer", b(false)),
    ]))
}
pub fn verify(rt: &Runtime<'_>, path: &std::ffi::OsStr) -> Result<Value> {
    let v = parse(&rt.read_user(path, MAX)?)?;
    validate(&v)?;
    let mut out = pick(&v, &["capsule_id", "digest"]);
    out.set("schema", number(1))?;
    out.set("valid", b(true))?;
    out.set("authority_transfer", b(false))?;
    Ok(out)
}
pub fn import(rt: &Runtime<'_>, path: &std::ffi::OsStr) -> Result<Value> {
    let raw = parse(&rt.read_user(path, MAX)?)?;
    validate(&raw)?;
    let id = raw.text("capsule_id");
    let destination = format!(".oms/portable/imports/{id}.json");
    let p = Path::new(&destination);
    let _guard = rt.lock(Path::new(".oms/portable/imports/LATEST"))?;
    rt.invalidate(p);
    let existing = rt.json(p, Value::Null)?;
    let idempotent = !matches!(existing, Value::Null);
    if idempotent {
        validate(existing.get("capsule"))
            .map_err(|_| status(75, "existing portable capsule import is invalid"))?;
        require(
            existing.get("capsule").get("digest") == raw.get("digest"),
            "portable capsule import conflicts with existing local state",
        )?;
    } else {
        let wrapper = object([
            ("schema", number(1)),
            ("imported_at", string(&rt.now)),
            ("advisory", b(true)),
            ("capsule", raw.clone()),
        ]);
        rt.write_locked(p, &encoded(&wrapper)?, MAX + 4096, &_guard)?;
    }
    rt.write_locked(
        Path::new(".oms/portable/imports/LATEST"),
        format!("{id}\n").as_bytes(),
        128,
        &_guard,
    )?;
    Ok(object([
        ("schema", number(1)),
        ("imported", b(!idempotent)),
        ("idempotent", b(idempotent)),
        ("capsule_id", string(id)),
        ("digest", raw.get("digest").clone()),
        ("path", string(destination)),
        ("authority_transfer", b(false)),
        (
            "message",
            string(
                "imported as advisory continuity state; no task, plan, approval, lease, or publication authority changed",
            ),
        ),
    ]))
}
pub fn latest_import(rt: &Runtime<'_>) -> Result<Value> {
    let raw = match rt.bytes(".oms/portable/imports/LATEST", 128) {
        Ok(None) => return Ok(object([("present", b(false))])),
        Ok(Some(v)) => v,
        Err(_) => return Ok(invalid_import()),
    };
    let checked = (|| {
        let id = std::str::from_utf8(&raw).map_err(error)?;
        require(
            id.ends_with('\n')
                && id.len() == 41
                && id.starts_with("capsule-")
                && hex(&id[8..40], 32),
            "invalid capsule LATEST",
        )?;
        let id = &id[..40];
        let path = format!(".oms/portable/imports/{id}.json");
        let wrapper = parse(
            &rt.bytes(&path, MAX + 4096)?
                .ok_or_else(|| error("latest import is missing"))?,
        )?;
        require(
            wrapper.get("schema").is_int(1) && wrapper.get("advisory") == &b(true),
            "invalid import wrapper",
        )?;
        let row = wrapper.get("capsule");
        validate(row)?;
        require(row.text("capsule_id") == id, "capsule pointer mismatch")?;
        let project = row.get("payload").get("project");
        let head = project.text("head");
        let head = if hex(head, 40) || hex(head, 64) {
            head
        } else {
            ""
        };
        let state = project.text("state_digest");
        let state = if hex(state, 64) { state } else { "" };
        let matches = if head.is_empty() || rt.repository.head.is_empty() {
            Value::Null
        } else {
            b(head == rt.repository.head)
        };
        Ok(object([
            ("present", b(true)),
            ("advisory", b(true)),
            ("authority_transfer", b(false)),
            ("capsule_id", string(id)),
            ("digest", row.get("digest").clone()),
            (
                "imported_at",
                string(bounded(wrapper.text("imported_at"), 40)),
            ),
            ("path", string(path)),
            ("source_head", optional(head)),
            ("source_state_digest", optional(state)),
            ("head_matches", matches.clone()),
            ("state_digest_matches", Value::Null),
            (
                "status",
                string(if matches == b(false) {
                    "head-diverged"
                } else {
                    "unknown"
                }),
            ),
        ]))
    })();
    Ok(checked.unwrap_or_else(|_: crate::Error| invalid_import()))
}
fn invalid_import() -> Value {
    object([
        ("present", b(true)),
        ("advisory", b(true)),
        ("authority_transfer", b(false)),
        ("capsule_id", Value::Null),
        ("head_matches", Value::Null),
        ("state_digest_matches", Value::Null),
        ("status", string("invalid")),
        ("reason", string("local-import-validation-failed")),
    ])
}
pub fn diff(left: &Value, right: &Value) -> Result<Value> {
    validate(left)?;
    validate(right)?;
    let by_id = |v: &Value| {
        rows(v.get("payload").get("contract").get("criteria"))
            .iter()
            .map(|c| (c.text("id").to_owned(), c.clone()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let l = by_id(left);
    let r = by_id(right);
    let ids = l
        .keys()
        .chain(r.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let changes = ids
        .into_iter()
        .filter(|id| l.get(id) != r.get(id))
        .map(|id| {
            object([
                ("criterion_id", string(&id)),
                ("before", l.get(&id).cloned().unwrap_or(Value::Null)),
                ("after", r.get(&id).cloned().unwrap_or(Value::Null)),
            ])
        });
    Ok(object([
        ("schema", number(1)),
        ("left", left.get("digest").clone()),
        ("right", right.get("digest").clone()),
        (
            "head_changed",
            b(left.get("payload").get("project").get("head")
                != right.get("payload").get("project").get("head")),
        ),
        (
            "state_changed",
            b(left.get("payload").get("project").get("state_digest")
                != right.get("payload").get("project").get("state_digest")),
        ),
        ("criterion_changes", a(changes)),
    ]))
}
