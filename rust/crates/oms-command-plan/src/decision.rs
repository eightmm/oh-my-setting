//! Fresh reviewed decision bytes and historical immutable digest resolution.
use crate::json::{self, Value, object, string};
use crate::storage::Store;
use crate::{Result, error, require};
pub const CATALOG: &str = ".oms/plan/decision-artifacts-v1";
pub fn relative(value: &str) -> Result<&str> {
    require(
        !value.is_empty()
            && value.chars().count() <= 240
            && !value.starts_with('/')
            && !value.contains('\\')
            && !value.chars().any(|c| c < ' ' || c == '\u{7f}')
            && value.split('/').all(|p| !matches!(p, "" | "." | "..")),
        "decision artifact path must be a bounded repo-relative path",
    )?;
    Ok(value)
}
pub fn snapshot(store: &Store, rel: &str, expected: &str) -> Result<Value> {
    require(
        store.directory_exists(".oms/plan")?,
        "decision catalog requires an existing real repo-local plan directory",
    )?;
    relative(rel)?;
    require(
        json::hex(expected, 64),
        "decision artifact requires a lowercase SHA-256",
    )?;
    let raw = store.read(rel, 64 * 1024)?;
    require(
        json::sha(&raw) == expected,
        "decision artifact digest changed or is not the reviewed SHA-256",
    )?;
    let (reference, created) = store.immutable(CATALOG, &raw, "", 64 * 1024)?;
    Ok(object([
        ("snapshot_path", reference.get("path").clone()),
        ("sha256", string(expected)),
        ("created", Value::Bool(created)),
    ]))
}
pub fn historical(store: &Store, rel: &str, expected: &str) -> Result<Vec<u8>> {
    relative(rel)?;
    require(
        json::hex(expected, 64),
        "decision artifact requires a lowercase SHA-256",
    )?;
    if let Some(raw) = store.optional(rel, 64 * 1024)? {
        if json::sha(&raw) == expected {
            return Ok(raw);
        }
    }
    let raw=store.private_read(format!("{CATALOG}/{expected}"),64*1024).map_err(|_|error("cancellation decision artifact digest changed or exact snapshot is missing/corrupt"))?;
    require(
        json::sha(&raw) == expected,
        "cancellation decision artifact digest changed or exact snapshot is missing/corrupt",
    )?;
    Ok(raw)
}
pub fn validate(store: &Store, task: &Value) -> Result<()> {
    let receipt = task.get("cancellation");
    if task.text("state") != "cancelled" {
        require(
            matches!(receipt, Value::Null),
            "cancellation evidence without cancelled state",
        )?;
        return Ok(());
    }
    json::keys(
        receipt,
        &[
            "schema",
            "kind",
            "ts",
            "operator_consent",
            "reason",
            "state",
            "original_updated",
            "lease_id_sha256",
            "plan_sha256",
            "task_sha256",
            "decision_artifact",
            "decision_artifact_sha256",
        ],
        &[],
    )?;
    require(
        receipt.get("schema").is_int(1)
            && receipt.text("kind") == "operator-cancellation"
            && !receipt.text("ts").is_empty()
            && receipt.text("operator_consent") == "cancel"
            && ["ready", "blocked"].contains(&receipt.text("state"))
            && receipt.get("original_updated").str().is_ok()
            && task.text("updated") == receipt.text("ts"),
        "invalid cancellation evidence",
    )?;
    let reason = receipt.get("reason").str()?;
    require(
        !reason.is_empty()
            && reason.chars().count() <= 500
            && !reason.chars().any(|c| c < ' ' || c == '\u{7f}'),
        "invalid cancellation reason",
    )?;
    for name in [
        "lease_id_sha256",
        "plan_sha256",
        "task_sha256",
        "decision_artifact_sha256",
    ] {
        json::digest(receipt.get(name))?;
    }
    let mut original = task.clone();
    original.remove("cancellation");
    original.set("state", receipt.get("state").clone())?;
    original.set("updated", receipt.get("original_updated").clone())?;
    require(
        json::sha(original.text("lease_id").as_bytes()) == receipt.text("lease_id_sha256")
            && json::hash(&original)? == receipt.text("task_sha256"),
        "cancellation evidence does not reconstruct its original task",
    )?;
    historical(
        store,
        receipt.get("decision_artifact").str()?,
        receipt.text("decision_artifact_sha256"),
    )?;
    Ok(())
}
