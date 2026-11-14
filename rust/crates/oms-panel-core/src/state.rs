//! Canonical producer acquisition and panel-state serialization.
use crate::services::{NativeCommand, query};
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
pub const LIVE_STATES: &[&str] = &[
    "queued",
    "starting",
    "working",
    "running",
    "waiting_input",
    "waiting_approval",
    "verifying",
    "review",
    "blocked",
];
pub const TERMINAL_STATES: &[&str] = &["done", "failed", "abandoned", "cancelled", "timed_out"];
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub report: Value,
    pub status: i32,
}
pub fn repository(path: &Path) -> Result<std::path::PathBuf> {
    oms_reference::repository::work_repository(path, &AtomicBool::new(false))
        .map(|v| v.0)
        .map_err(Error::new)
}
pub fn snapshot(
    repo: &Path,
    room_id: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Snapshot> {
    let attempts = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    require(attempts.is_array(), "invalid lifecycle attempt projection")?;
    let room = if let Some(room) = room_id {
        crate::identifier(room)?;
        query(
            service,
            NativeCommand::Room,
            repo,
            &["show", "--id", room, "--json"],
            stop,
        )?
    } else {
        serde_json::json!({})
    };
    let store = Store::open(repo)?;
    let plan = store
        .json(Path::new(".oms/plan/tasks.json"), 4 * 1024 * 1024, true)?
        .unwrap_or_else(|| serde_json::json!({"tasks":{}}));
    let active: Vec<_> = array(&attempts)
        .iter()
        .filter(|a| LIVE_STATES.contains(&text(a, "state").as_str()))
        .cloned()
        .collect();
    let recent: Vec<_> = array(&attempts).iter().rev().take(100).cloned().collect();
    let report = serde_json::json!({"schema":1,"generated_at":crate::now(),"room":room,"plan":plan,
        "attempts":{"active":active,"recent":recent},"collection":{"ok":true},"capacity_stale":false});
    Ok(Snapshot { report, status: 0 })
}
pub fn active_mains(attempts: &[Value], room: &str, owner: &str) -> Vec<Value> {
    attempts
        .iter()
        .filter(|v| {
            v["tool"] == "panel-main"
                && v["provider"] == owner
                && v["refs"]["panel_room_id"] == room
                && v["terminal"] == false
        })
        .cloned()
        .collect()
}
pub fn hold_state(previous: &Value, failed: &Value) -> Value {
    let mut retained = previous.clone();
    retained["collection"] =
        serde_json::json!({"ok":false,"error":failed.get("error").cloned().unwrap_or(Value::Null)});
    retained["capacity_stale"] = true.into();
    retained
}
pub fn save(store: &Store, key: &str, state: &Value) -> Result<()> {
    crate::parent_only()?;
    crate::identifier(key)?;
    let tx = store.transaction("panel-state", Duration::from_secs(10))?;
    tx.write_json(
        Path::new(&format!(".oms/artifacts/panel-state/{key}.json")),
        state,
        false,
        crate::storage::MAX_STATE,
    )?;
    Ok(())
}
pub fn load(store: &Store, key: &str) -> Result<Option<Value>> {
    crate::identifier(key)?;
    store.json(
        Path::new(&format!(".oms/artifacts/panel-state/{key}.json")),
        crate::storage::MAX_STATE,
        true,
    )
}
pub fn evidence_matrix(report: &Value, limit: usize, task: Option<&str>) -> Result<Value> {
    let raw = oms_context::parse_experiment_json(&crate::value::encoded(report)?, usize::MAX)
        .map_err(Error::new)?;
    oms_work_inspect::evidence_matrix_value(&raw, limit, task)
        .map_err(Error::new)?
        .as_value()
        .cloned()
        .map_err(Error::new)
}
pub fn action_inbox(report: &Value, limit: usize, task: Option<&str>) -> Result<Value> {
    let raw = oms_context::parse_experiment_json(&crate::value::encoded(report)?, usize::MAX)
        .map_err(Error::new)?;
    oms_work_inspect::action_inbox_value(&raw, limit, task)
        .map_err(Error::new)?
        .as_value()
        .cloned()
        .map_err(Error::new)
}
pub fn return_brief(report: &Value, before: Option<&Value>, limit: usize) -> Result<Value> {
    let raw = oms_context::parse_experiment_json(&crate::value::encoded(report)?, usize::MAX)
        .map_err(Error::new)?;
    let before = before
        .map(|v| {
            crate::value::encoded(v).and_then(|b| {
                oms_context::parse_experiment_json(&b, usize::MAX).map_err(Error::new)
            })
        })
        .transpose()?;
    oms_work_inspect::return_brief_value(&raw, before.as_ref(), limit)
        .map_err(Error::new)?
        .as_value()
        .cloned()
        .map_err(Error::new)
}

/// Heartbeat belongs to the canonical operations producer. Room membership is
/// observed separately; a heartbeat is never a room lease or release proof.
pub fn heartbeat(
    repo: &Path,
    attempt: &str,
    actor: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    crate::identifier(attempt)?;
    crate::identifier(actor)?;
    service
        .command(
            NativeCommand::Events,
            repo,
            &crate::services::args(&["heartbeat", "--attempt", attempt, "--actor", actor]),
            stop,
        )?
        .success()
}
