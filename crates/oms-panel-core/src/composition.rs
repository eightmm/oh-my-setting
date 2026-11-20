//! Deterministic integration of reviewed, disjoint components for one lease.
//! Rechecking is read-only; only the acceptance owner retains the derived bytes.
use super::replay;
use crate::results::{AcceptRequest, MAX_RESULT, PATCH_MAX, verified_patch};
use crate::storage::Transaction;
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub(super) const MAX_COMPONENTS: usize = 16;

fn patch(store: &Store, components: &[Value], base: &str) -> Result<Vec<u8>> {
    require(
        (2..=MAX_COMPONENTS).contains(&components.len()),
        "composition requires 2..16 components",
    )?;
    let mut paths: Vec<String> = Vec::new();
    let mut body = Vec::new();
    for component in components {
        let candidate = &component["candidate"];
        require(candidate["base_sha"] == base, "component bases differ")?;
        let bytes = verified_patch(store, candidate)?;
        require(
            !bytes.is_empty() && bytes.ends_with(b"\n"),
            "component patch must be complete and nonempty",
        )?;
        let changed = crate::files::patch_paths(&bytes)?;
        for path in &changed {
            require(
                !paths.iter().any(|prior| {
                    Path::new(prior).starts_with(path) || Path::new(path).starts_with(prior)
                }),
                "component patches overlap; resolve them in an owned integration candidate",
            )?;
        }
        paths.extend(changed);
        require(
            body.len()
                .checked_add(bytes.len())
                .is_some_and(|size| size <= PATCH_MAX),
            "composed patch exceeds byte limit",
        )?;
        body.extend(bytes);
    }
    Ok(body)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn snapshot<S: Services + ?Sized>(
    repo: &Path,
    request: &AcceptRequest,
    task: &Value,
    mut components: Vec<Value>,
    allowed: &[String],
    superseded: &[Value],
    service: &S,
    stop: &AtomicBool,
) -> Result<Value> {
    components.sort_by_key(|component| text(&component["worker"], "attempt_id"));
    let base = text(&components[0]["candidate"], "base_sha");
    require(
        crate::hex(&base, 40) || crate::hex(&base, 64),
        "composition needs a full recorded common base",
    )?;
    let body = patch(&Store::open(repo)?, &components, &base)?;
    let digest = crate::digest(&body);
    require(
        request
            .expected_patch_sha256
            .as_ref()
            .is_none_or(|expected| *expected == digest),
        "composed patch differs from the expected digest",
    )?;
    let tree = replay(&request.execution_repo, &base, &body, service, stop)?;
    let manifest = serde_json::json!({
        "schema":1,"kind":"oms-composed-candidate","owner":request.owner,
        "main_attempt_id":request.main_attempt_id,"task_id":request.task_id,
        "plan_id":task["plan_id"],"lease_id":task["lease_id"],
        "verify_sha256":crate::digest(request.verify.as_bytes()),
        "base_sha":base,"tree":tree,"patch_sha256":digest,"components":components,
    });
    let raw = crate::value::encoded(&manifest)?;
    require(
        raw.len() <= 32 * 1024,
        "component manifest exceeds acceptance fence bound",
    )?;
    let manifest_sha = crate::digest(&raw);
    let identity = format!("composition_{manifest_sha}");
    let candidate = serde_json::json!({
        "kind":"composition","event_id":identity,"attempt_id":request.main_attempt_id,
        "task_id":request.task_id,"plan_id":task["plan_id"],"exit":0,"base_sha":base,
        "patch":format!(".oms/artifacts/compositions/{digest}.patch"),"patch_sha256":digest,
        "artifact":format!(".oms/artifacts/compositions/{manifest_sha}.json"),"artifact_sha256":manifest_sha,
    });
    let seats = components
        .iter()
        .flat_map(|component| {
            array(&component["review_gate"]["seat_verdicts"])
                .iter()
                .cloned()
        })
        .collect::<Vec<_>>();
    // These verdicts belong to component reviews. The exact combined verifier
    // result is recorded separately only after the existing landing succeeds.
    let gate = serde_json::json!({"kind":"component-reviews","event_id":identity,
        "patch_sha256":digest,"exit":0,"seat_verdicts":seats});
    Ok(serde_json::json!({"task":task,"candidate":candidate,
        "worker":{"attempt_id":request.main_attempt_id,"kind":"composition-owner"},
        "review_gate":gate,"allowed_paths":allowed,"superseded_attempts":superseded,"composition":manifest}))
}

pub(super) fn retain(store: &Store, tx: &Transaction<'_>, snapshot: &Value) -> Result<()> {
    let manifest = &snapshot["composition"];
    let candidate = &snapshot["candidate"];
    let components = manifest["components"]
        .as_array()
        .ok_or_else(|| Error::new("component manifest unavailable"))?;
    let body = patch(store, components, &text(manifest, "base_sha"))?;
    let raw = crate::value::encoded(manifest)?;
    require(
        crate::digest(&body) == candidate["patch_sha256"]
            && crate::digest(&raw) == candidate["artifact_sha256"],
        "component evidence changed before retention",
    )?;
    crate::outbound(&body)?;
    crate::outbound(&raw)?;
    tx.write(Path::new(&text(candidate, "patch")), &body, true, PATCH_MAX)?;
    tx.write(
        Path::new(&text(candidate, "artifact")),
        &raw,
        true,
        MAX_RESULT,
    )
}

pub(super) fn verify_retained(store: &Store, snapshot: &Value) -> Result<()> {
    let candidate = &snapshot["candidate"];
    let raw = store.read(Path::new(&text(candidate, "artifact")), MAX_RESULT, false)?;
    require(
        raw == crate::value::encoded(&snapshot["composition"])?
            && crate::digest(&raw) == candidate["artifact_sha256"],
        "composed manifest changed",
    )?;
    verified_patch(store, candidate)?;
    Ok(())
}
