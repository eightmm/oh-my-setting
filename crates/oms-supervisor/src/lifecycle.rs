use crate::Result;
use oms_command_operations::{
    self as ops,
    value::{V, obj},
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
pub(crate) fn projection(repo: &Path) -> Result<BTreeMap<String, V>> {
    Ok(ops::load_projection(repo)?
        .1
        .into_iter()
        .map(|v| (v.get("attempt_id").py(), v))
        .collect())
}
pub(crate) fn transition(
    repo: &Path,
    id: &str,
    state: &str,
    reason: &str,
    key: &str,
    interrupted: &AtomicBool,
) -> Result<()> {
    let row = ops::new_event(
        id,
        0usize.into(),
        "attempt.state_changed",
        obj([
            ("from_state", V::Null),
            ("to_state", state.into()),
            ("reason_code", reason.into()),
            (
                "actor",
                obj([
                    ("kind", "supervisor".into()),
                    ("name", "agent-supervisor".into()),
                ]),
            ),
            ("idempotency_key", key.into()),
        ]),
    )?;
    ops::append_lifecycle(repo, row, interrupted)?;
    Ok(())
}
pub(crate) fn report(
    repo: &Path,
    id: &str,
    kind: &str,
    values: Option<V>,
    key: &str,
    interrupted: &AtomicBool,
) -> Result<()> {
    let mut fields = obj([
        (
            "actor",
            obj([("kind", "runner".into()), ("name", "attempt-runner".into())]),
        ),
        ("idempotency_key", key.into()),
    ]);
    if let Some(values) = values {
        fields.set("usage", values);
    }
    ops::append_lifecycle(
        repo,
        ops::new_event(id, 0usize.into(), kind, fields)?,
        interrupted,
    )?;
    Ok(())
}
pub(crate) fn terminal(state: &str) -> bool {
    ops::TERMINAL_STATES.contains(&state)
}
pub(crate) fn nonterminal(state: &str) -> bool {
    ops::NONTERMINAL_STATES.contains(&state)
}
