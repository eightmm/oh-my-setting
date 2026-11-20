//! Schema-2 panel acceptance retains every component and an exact combined tree.
use crate::engine::{clean_path, receipt_id};
use crate::json::{self, Value};
use crate::storage::{MAX_JSON, MAX_PATCH};
use crate::{Plan, Result, require};
use std::collections::BTreeSet;

pub(crate) fn validate(plan: &Plan<'_>, task: &Value, record: &Value, patch: &[u8]) -> Result<()> {
    let reference = record.get("composition");
    json::keys(reference, &["path", "sha256"], &[])?;
    let path = reference.text("path");
    let digest = reference.text("sha256");
    require(
        json::hex(digest, 64)
            && path == format!(".oms/artifacts/compositions/{digest}.json")
            && record.text("artifact") == path
            && record.text("candidate_event_id") == format!("composition_{digest}"),
        "composed acceptance manifest identity changed",
    )?;
    let raw = plan.evidence_read(path, MAX_JSON)?;
    require(
        raw.len() <= 32 * 1024 && json::sha(&raw) == digest,
        "composed acceptance manifest digest changed",
    )?;
    let manifest = json::parse(&raw)?;
    json::keys(
        &manifest,
        &[
            "schema",
            "kind",
            "owner",
            "main_attempt_id",
            "task_id",
            "plan_id",
            "lease_id",
            "verify_sha256",
            "base_sha",
            "tree",
            "patch_sha256",
            "components",
        ],
        &[],
    )?;
    require(
        manifest.get("schema").is_int(1)
            && manifest.text("kind") == "oms-composed-candidate"
            && ["task_id", "plan_id", "lease_id", "tree", "patch_sha256"]
                .iter()
                .all(|key| manifest.get(key) == record.get(key))
            && manifest.text("base_sha") == record.text("base")
            && manifest.text("main_attempt_id") == record.text("candidate_attempt_id")
            && receipt_id(manifest.text("main_attempt_id"), 160)
            && !manifest.text("owner").is_empty(),
        "composed acceptance belongs to another owner, task, lease or tree",
    )?;
    let verify = record.get("integration_verify");
    json::keys(verify, &["command", "tree", "exit"], &[])?;
    require(
        verify.get("exit").is_int(0)
            && verify.get("tree") == record.get("tree")
            && !verify.text("command").is_empty()
            && verify.text("command").replace('\r', "") == task.text("verify").replace('\r', "")
            && json::sha(verify.text("command").as_bytes()) == manifest.text("verify_sha256"),
        "composed acceptance lacks exact integration verification",
    )?;
    let components = manifest.get("components").array()?;
    require(
        (2..=16).contains(&components.len()),
        "invalid composed component count",
    )?;
    let mut joined = Vec::new();
    let mut attempts = BTreeSet::new();
    let mut seats = Vec::new();
    let mut previous = "";
    for component in components {
        json::keys(component, &["candidate", "worker", "review_gate"], &[])?;
        let candidate = component.get("candidate");
        let worker = component.get("worker");
        let attempt = worker.text("attempt_id");
        require(
            receipt_id(attempt, 160)
                && previous < attempt
                && attempts.insert(attempt)
                && candidate.text("attempt_id") == attempt
                && candidate.text("kind") == "delegate"
                && candidate.get("exit").is_int(0)
                && candidate.get("task_id") == record.get("task_id")
                && worker.get("task_id") == record.get("task_id")
                && candidate.get("plan_id") == record.get("plan_id")
                && candidate.text("base_sha") == record.text("base")
                && worker.get("refs").get("panel_lease_id") == record.get("lease_id")
                && worker.get("refs").text("panel_role") == "worker"
                && worker.get("refs").text("panel_access") == "write"
                && worker.get("refs").text("panel_purpose") != "investigate",
            "composed component binding changed",
        )?;
        previous = attempt;
        for field in ["artifact", "patch"] {
            let value = candidate.text(field);
            require(
                value.starts_with(".oms/artifacts/") && clean_path(value)? == value,
                "composed component leaves evidence store",
            )?;
        }
        let artifact = plan.evidence_read(candidate.text("artifact"), MAX_JSON)?;
        require(
            json::sha(&artifact) == candidate.text("artifact_sha256"),
            "composed component artifact changed",
        )?;
        let bytes = plan.evidence_read(candidate.text("patch"), MAX_PATCH)?;
        require(
            !bytes.is_empty()
                && bytes.ends_with(b"\n")
                && json::sha(&bytes) == candidate.text("patch_sha256")
                && joined
                    .len()
                    .checked_add(bytes.len())
                    .is_some_and(|length| length <= MAX_PATCH),
            "composed component patch changed or exceeds bound",
        )?;
        joined.extend(bytes);
        let gate = component.get("review_gate");
        let verdicts = gate.get("seat_verdicts").strings()?;
        require(
            gate.get("patch_sha256") == candidate.get("patch_sha256")
                && receipt_id(gate.text("event_id"), 160)
                && gate.get("exit").is_int(0)
                && gate.get("gate_verify_exit").is_int(0)
                && !verdicts.is_empty()
                && verdicts.iter().all(|verdict| *verdict == "pass"),
            "composed component review failed",
        )?;
        seats.extend(verdicts);
    }
    require(
        joined == patch
            && record.get("review_gate").get("seat_verdicts").strings()? == seats
            && record.get("review_gate").text("event_id") == record.text("candidate_event_id"),
        "composed patch omits or changes a reviewed component",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{number, object, string};
    use crate::{Action, Context, Request, UnavailableServices};
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn native_composition_record_keeps_components_and_integration_verification() {
        let root = std::env::temp_dir().join(format!(
            "oms-composition-{}-{}",
            std::process::id(),
            crate::storage::random_hex(8).unwrap()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        struct Temp(std::path::PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = Temp(std::fs::canonicalize(root).unwrap());
        let stop = AtomicBool::new(false);
        let context = Context::from_selected_state_before(
            &Request::new(Action::Status),
            &root.0,
            &root.0,
            &root.0,
            &stop,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        let plan = Plan::open(context, &UnavailableServices, &stop).unwrap();
        let mut components = Vec::new();
        let mut patch = Vec::new();
        for name in ["a", "b"] {
            let body = format!(
                "diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -1 +1 @@\n-before\n+after\n"
            );
            let (patch_ref, _) = plan
                .store
                .immutable(
                    ".oms/artifacts/components",
                    body.as_bytes(),
                    ".patch",
                    MAX_PATCH,
                )
                .unwrap();
            let (artifact, _) = plan
                .store
                .immutable(
                    ".oms/artifacts/components",
                    format!("reviewed {name}\n").as_bytes(),
                    ".md",
                    MAX_JSON,
                )
                .unwrap();
            components.push(object([
                (
                    "candidate",
                    object([
                        ("kind", string("delegate")),
                        ("attempt_id", string(name)),
                        ("exit", number(0)),
                        ("task_id", string("task-a")),
                        ("plan_id", string("plan-a")),
                        ("base_sha", string("a".repeat(40))),
                        ("patch", patch_ref.get("path").clone()),
                        ("patch_sha256", patch_ref.get("sha256").clone()),
                        ("artifact", artifact.get("path").clone()),
                        ("artifact_sha256", artifact.get("sha256").clone()),
                    ]),
                ),
                (
                    "worker",
                    object([
                        ("attempt_id", string(name)),
                        ("task_id", string("task-a")),
                        (
                            "refs",
                            object([
                                ("panel_lease_id", string("lease-a")),
                                ("panel_role", string("worker")),
                                ("panel_access", string("write")),
                            ]),
                        ),
                    ]),
                ),
                (
                    "review_gate",
                    object([
                        ("event_id", string(format!("review-{name}"))),
                        ("patch_sha256", patch_ref.get("sha256").clone()),
                        ("exit", number(0)),
                        ("gate_verify_exit", number(0)),
                        ("seat_verdicts", Value::Array(vec![string("pass")])),
                    ]),
                ),
            ]));
            patch.extend(body.as_bytes());
        }
        let manifest = object([
            ("schema", number(1)),
            ("kind", string("oms-composed-candidate")),
            ("owner", string("codex")),
            ("main_attempt_id", string("main-a")),
            ("task_id", string("task-a")),
            ("plan_id", string("plan-a")),
            ("lease_id", string("lease-a")),
            ("verify_sha256", string(json::sha(b"true"))),
            ("base_sha", string("a".repeat(40))),
            ("tree", string("b".repeat(40))),
            ("patch_sha256", string(json::sha(&patch))),
            ("components", Value::Array(components)),
        ]);
        let record_for = |manifest: &Value| {
            let raw = json::canonical(manifest).unwrap();
            let (reference, _) = plan
                .store
                .immutable(".oms/artifacts/compositions", &raw, ".json", MAX_JSON)
                .unwrap();
            let identity = format!("composition_{}", reference.text("sha256"));
            object([
                ("composition", reference.clone()),
                ("artifact", reference.get("path").clone()),
                ("candidate_event_id", string(&identity)),
                ("candidate_attempt_id", string("main-a")),
                ("task_id", string("task-a")),
                ("plan_id", string("plan-a")),
                ("lease_id", string("lease-a")),
                ("tree", string("b".repeat(40))),
                ("base", string("a".repeat(40))),
                ("patch_sha256", string(json::sha(&patch))),
                (
                    "integration_verify",
                    object([
                        ("command", string("true")),
                        ("tree", string("b".repeat(40))),
                        ("exit", number(0)),
                    ]),
                ),
                (
                    "review_gate",
                    object([
                        ("event_id", string(identity)),
                        (
                            "seat_verdicts",
                            Value::Array(vec![string("pass"), string("pass")]),
                        ),
                    ]),
                ),
            ])
        };
        let record = record_for(&manifest);
        let task = object([("verify", string("true"))]);
        validate(&plan, &task, &record, &patch).unwrap();
        let mut missing = manifest.clone();
        missing
            .set(
                "components",
                Value::Array(vec![manifest.get("components").array().unwrap()[0].clone()]),
            )
            .unwrap();
        assert!(validate(&plan, &task, &record_for(&missing), &patch).is_err());
        let mut changed = record.clone();
        changed
            .set(
                "integration_verify",
                object([
                    ("command", string("false")),
                    ("tree", string("b".repeat(40))),
                    ("exit", number(0)),
                ]),
            )
            .unwrap();
        assert!(validate(&plan, &task, &changed, &patch).is_err());
        assert!(validate(&plan, &task, &record, &patch[..patch.len() - 1]).is_err());
        let artifact = manifest.get("components").array().unwrap()[0]
            .get("candidate")
            .text("artifact");
        std::fs::write(root.0.join(artifact), b"substituted evidence\n").unwrap();
        assert!(validate(&plan, &task, &record, &patch).is_err());
    }
}
