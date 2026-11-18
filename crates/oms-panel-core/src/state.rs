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
/// Resolve the canonical shared state repository for a working directory or scratch checkout.
pub fn repository(path: &Path) -> Result<std::path::PathBuf> {
    repository_for(path, false)
}

/// Resolve the canonical shared state repository, optionally allowing foreign room overrides.
pub fn repository_for(path: &Path, allow_foreign: bool) -> Result<std::path::PathBuf> {
    let resolver = oms_reference::repository::Resolver::from_env().map_err(Error::new)?;
    resolve_state_with(&resolver, path, allow_foreign)
}

/// Resolve the physical Git working repository root (or normalized non-Git path) and Git flag.
pub fn work_repository(path: &Path) -> Result<(std::path::PathBuf, bool)> {
    oms_reference::repository::work_repository(path, &AtomicBool::new(false)).map_err(Error::new)
}

/// Resolve the native execution root for a shared state repository, honoring an optional checkout.
pub fn native_repository(
    shared: &Path,
    execution_root: Option<&Path>,
) -> Result<std::path::PathBuf> {
    let mut resolver = oms_reference::repository::Resolver::from_env().map_err(Error::new)?;
    if let Some(root) = execution_root {
        resolver.execution_root = Some(root.to_owned());
    }
    resolver
        .native_repository(shared, &AtomicBool::new(false))
        .map_err(Error::new)
}

fn resolve_state_with(
    resolver: &oms_reference::repository::Resolver,
    path: &Path,
    allow_foreign: bool,
) -> Result<std::path::PathBuf> {
    resolver
        .state_repository(path, allow_foreign, &AtomicBool::new(false))
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
    let mut room_attempts = serde_json::Map::new();
    if let Some(rid) = room_id {
        let main_attempt = text(&room, "main");
        for attempt in array(&attempts) {
            let refs = &attempt["refs"];
            let att_room = text(refs, "panel_room_id");
            let att_room_alt = text(refs, "panel_room");
            let attempt_id = text(attempt, "attempt_id");
            if att_room == rid
                || att_room_alt == rid
                || (!main_attempt.is_empty() && attempt_id == main_attempt)
            {
                if !attempt_id.is_empty() {
                    room_attempts.insert(attempt_id, attempt.clone());
                }
                let participant = text(refs, "panel_room_participant");
                if !participant.is_empty() {
                    room_attempts.insert(participant, attempt.clone());
                }
            }
        }
    }
    let report = serde_json::json!({"schema":1,"generated_at":crate::now(),"room":room,"plan":plan,
        "attempts":{"active":active,"recent":recent},"room_attempts":room_attempts,"collection":{"ok":true},"capacity_stale":false});
    Ok(Snapshot { report, status: 0 })
}
/// Native mains join the room with their attempt id unless an explicit participant is recorded.
pub fn main_participant(attempt: &Value) -> &str {
    attempt["refs"]["panel_room_participant"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| attempt["attempt_id"].as_str())
        .unwrap_or("")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(1);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let id = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "oms-panel-state-{label}-{}-{id}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let canonical = std::fs::canonicalize(&dir).unwrap_or(dir);
            Self(canonical)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_ok(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git execution");
        assert!(
            status.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        git_ok(dir, &["init", "-b", "main"]);
        git_ok(dir, &["config", "user.name", "Test"]);
        git_ok(dir, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(dir.join("README.md"), "ok\n").unwrap();
        git_ok(dir, &["add", "README.md"]);
        git_ok(dir, &["commit", "-m", "init"]);
    }

    #[test]
    fn scratch_worktree_resolves_to_shared_state_and_rejects_foreign_room() {
        let base = TempDir::new("scratch");
        let main_repo = base.0.join("repo");
        let other_repo = base.0.join("other");
        let managed = base.0.join("worktrees");
        std::fs::create_dir_all(&main_repo).unwrap();
        std::fs::create_dir_all(&other_repo).unwrap();
        init_repo(&main_repo);
        init_repo(&other_repo);

        let parent = managed.join("oh-my-setting-scratch.demo1");
        let scratch = parent.join("wt-demo1");
        std::fs::create_dir_all(&parent).unwrap();
        git_ok(
            &main_repo,
            &[
                "worktree",
                "add",
                "-b",
                "oms/demo",
                scratch.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::write(
            parent.join(".oh-my-setting-tmp"),
            format!(
                "kind=oh-my-setting-temp\ntemporary=1\nrepo={}\nworktree={}\n",
                main_repo.display(),
                scratch.display()
            ),
        )
        .unwrap();

        let resolver = oms_reference::repository::Resolver {
            managed_root: managed.clone(),
            child: false,
            inherited_room: Some(main_repo.clone()),
            execution_root: None,
        };
        let resolved = resolve_state_with(&resolver, &scratch, false).unwrap();
        assert_eq!(resolved, main_repo);

        let foreign = oms_reference::repository::Resolver {
            managed_root: managed,
            child: false,
            inherited_room: Some(other_repo),
            execution_root: None,
        };
        let err = resolve_state_with(&foreign, &scratch, false).unwrap_err();
        assert!(
            err.to_string()
                .contains("conflicts with the inherited room repository"),
            "unexpected error: {err}"
        );
        let allowed = resolve_state_with(&foreign, &scratch, true).unwrap();
        assert_eq!(allowed, main_repo);
        let second = parent.join("second");
        git_ok(
            &main_repo,
            &[
                "worktree",
                "add",
                "--detach",
                second.to_str().unwrap(),
                "HEAD",
            ],
        );
        let pointer = std::fs::read(scratch.join(".git")).unwrap();
        std::fs::copy(second.join(".git"), scratch.join(".git")).unwrap();
        assert_eq!(
            resolve_state_with(&resolver, &scratch, true).unwrap(),
            scratch,
            "another registered worktree's metadata cannot prove this checkout"
        );
        assert!(resolve_state_with(&resolver, &scratch, false).is_err());
        std::fs::write(scratch.join(".git"), pointer).unwrap();
        assert_eq!(
            resolve_state_with(&resolver, &scratch, false).unwrap(),
            main_repo
        );
    }
}
