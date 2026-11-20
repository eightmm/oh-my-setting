//! Canonical producer acquisition and panel-state serialization.
use crate::services::{NativeCommand, query};
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
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
// One projection per session bounds memory and prevents cross-repository reuse.
#[derive(Default)]
pub struct AttemptCache(Mutex<Option<(String, Value)>>);

fn ledger_generation(repo: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let root = std::fs::canonicalize(repo).ok()?;
    let mut path = root.clone();
    let root_meta = std::fs::metadata(&root).ok()?;
    let mut parts = vec![format!("{}:{}", root_meta.dev(), root_meta.ino())];
    for (name, directory) in [(".oms", true), ("lifecycle", true), ("events.jsonl", false)] {
        path.push(name);
        let m = std::fs::symlink_metadata(&path).ok()?;
        if m.file_type().is_symlink()
            || (directory && !m.is_dir())
            || (!directory && (!m.is_file() || m.nlink() != 1))
        {
            return None;
        }
        parts.push(format!(
            "{}:{}:{}:{}:{}:{}:{}",
            m.dev(),
            m.ino(),
            m.size(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec()
        ));
    }
    Some(format!("{:?}|{}", root, parts.join("|")))
}

fn unique_attempt<'a>(attempts: &'a Value, attempt: &str) -> Option<&'a Value> {
    if attempt.is_empty() {
        return None;
    }
    let mut selected = None;
    for row in attempts.as_array()? {
        let id = row.get("attempt_id")?.as_str()?;
        if id.is_empty() || (id == attempt && selected.replace(row).is_some()) {
            return None;
        }
    }
    selected
}

impl AttemptCache {
    /// Read one exact attempt from a stable cached ledger without replaying it.
    pub fn current_attempt(&self, repo: &Path, attempt: &str, stop: &AtomicBool) -> Option<Value> {
        if attempt.is_empty() || stop.load(Ordering::Relaxed) {
            return None;
        }
        let cache = self.0.lock().ok()?;
        let (generation, attempts) = cache.as_ref()?;
        if ledger_generation(repo).as_ref() != Some(generation) {
            return None;
        }
        let selected = unique_attempt(attempts, attempt)?.clone();
        (!stop.load(Ordering::Relaxed) && ledger_generation(repo).as_ref() == Some(generation))
            .then_some(selected)
    }

    pub fn attempts(
        &self,
        repo: &Path,
        service: &dyn Services,
        stop: &AtomicBool,
    ) -> Result<Value> {
        let mut cache = self
            .0
            .lock()
            .map_err(|_| Error::new("attempt cache unavailable"))?;
        let before = ledger_generation(repo);
        if !stop.load(Ordering::Relaxed)
            && let Some(ref generation) = before
            && let Some((key, value)) = cache.as_ref()
            && key == generation
            && ledger_generation(repo).as_ref() == Some(generation)
        {
            return Ok(value.clone());
        }
        *cache = None;
        let attempts = query(
            service,
            NativeCommand::Events,
            repo,
            &["list", "--json"],
            stop,
        )?;
        require(attempts.is_array(), "invalid lifecycle attempt projection")?;
        if !stop.load(Ordering::Relaxed)
            && let Some(generation) = before
            && ledger_generation(repo).as_ref() == Some(&generation)
        {
            *cache = Some((generation, attempts.clone()));
        }
        Ok(attempts)
    }

    /// Resolve one exact attempt, refreshing only when the lifecycle ledger generation changes.
    pub fn resolve_attempt(
        &self,
        repo: &Path,
        attempt: &str,
        service: &dyn Services,
        stop: &AtomicBool,
    ) -> Result<Option<Value>> {
        if attempt.is_empty() {
            return Ok(None);
        }
        crate::checkpoint(stop)?;
        let mut cache = self
            .0
            .lock()
            .map_err(|_| Error::new("attempt cache unavailable"))?;
        let before = ledger_generation(repo);
        if let Some(ref generation) = before
            && let Some((key, attempts)) = cache.as_ref()
            && key == generation
        {
            let selected = unique_attempt(attempts, attempt).cloned();
            crate::checkpoint(stop)?;
            if ledger_generation(repo).as_ref() == Some(generation) {
                return Ok(selected);
            }
        }
        for _ in 0..2 {
            crate::checkpoint(stop)?;
            *cache = None;
            let before = ledger_generation(repo);
            let attempts = query(
                service,
                NativeCommand::Events,
                repo,
                &["list", "--json"],
                stop,
            )?;
            require(attempts.is_array(), "invalid lifecycle attempt projection")?;
            let selected = unique_attempt(&attempts, attempt).cloned();
            crate::checkpoint(stop)?;
            let after = ledger_generation(repo);
            if before != after {
                *cache = None;
                continue;
            }
            if let Some(generation) = after {
                *cache = Some((generation, attempts));
            }
            return Ok(selected);
        }
        Err(Error::new("lifecycle ledger changed during verification"))
    }
}

pub fn snapshot(
    repo: &Path,
    room_id: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Snapshot> {
    snapshot_with_cache(repo, room_id, service, stop, None)
}

pub fn snapshot_with_cache(
    repo: &Path,
    room_id: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
    cache: Option<&AttemptCache>,
) -> Result<Snapshot> {
    let result = snapshot_inner(repo, room_id, service, stop, cache);
    if result.is_err()
        && let Some(cache) = cache
        && let Ok(mut entry) = cache.0.lock()
    {
        *entry = None;
    }
    result
}

fn snapshot_inner(
    repo: &Path,
    room_id: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
    cache: Option<&AttemptCache>,
) -> Result<Snapshot> {
    let attempts = if let Some(cache) = cache {
        cache.attempts(repo, service, stop)?
    } else {
        query(
            service,
            NativeCommand::Events,
            repo,
            &["list", "--json"],
            stop,
        )?
    };
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

    #[test]
    fn resolve_attempt_caches_unchanged_ledger_and_refreshes_on_append_replace_symlink_or_delete() {
        use crate::services::{CommandOutput, ExecutionRequest, NativeCommand, PeerExecutor};
        use std::ffi::OsString;
        use std::io::Write;
        use std::os::unix::fs::{MetadataExt, symlink};
        use std::sync::atomic::AtomicUsize;

        struct EventsCounterService {
            calls: AtomicUsize,
            override_reply: Mutex<Option<Value>>,
            on_query: Mutex<Option<Box<dyn FnMut() + Send>>>,
        }

        impl EventsCounterService {
            fn new() -> Self {
                Self {
                    calls: AtomicUsize::new(0),
                    override_reply: Mutex::new(None),
                    on_query: Mutex::new(None),
                }
            }
        }

        impl PeerExecutor for EventsCounterService {
            fn execute(
                &self,
                _request: &ExecutionRequest,
                _interrupted: &AtomicBool,
            ) -> Result<CommandOutput> {
                Err(Error::new("unexpected external process"))
            }
        }

        impl Services for EventsCounterService {
            fn command(
                &self,
                command: NativeCommand,
                repo: &Path,
                args: &[OsString],
                interrupted: &AtomicBool,
            ) -> Result<CommandOutput> {
                assert_eq!(command, NativeCommand::Events);
                assert_eq!(args, ["list", "--json"]);
                self.calls.fetch_add(1, Ordering::Relaxed);
                if let Some(cb) = self.on_query.lock().unwrap().as_mut() {
                    cb();
                }
                if let Some(reply) = self.override_reply.lock().unwrap().as_ref() {
                    return Ok(CommandOutput {
                        code: 0,
                        stdout: serde_json::to_vec(reply).unwrap(),
                        stderr: Vec::new(),
                        cleanup_complete: true,
                        output_complete: true,
                        limited: false,
                    });
                }
                let (code, stdout, stderr) = oms_command_operations::run(
                    oms_command_operations::App::Events,
                    args,
                    repo,
                    interrupted,
                );
                Ok(CommandOutput {
                    code,
                    stdout,
                    stderr,
                    cleanup_complete: true,
                    output_complete: true,
                    limited: false,
                })
            }
        }

        let base = TempDir::new("attempt-cache-resolve");
        let repo = base.0.join("repo");
        let lifecycle = repo.join(".oms/lifecycle");
        std::fs::create_dir_all(&lifecycle).unwrap();
        let events_path = lifecycle.join("events.jsonl");

        let attempt_id = "att_11111111111111111111111111111111";
        let missing_id = "att_99999999999999999999999999999999";
        let created = serde_json::json!({
            "schema": 1,
            "event_id": "aevt_00000000000000000000000000000001",
            "ts": "2026-11-19T00:00:00Z",
            "attempt_id": attempt_id,
            "seq": 1,
            "event_type": "attempt.created",
            "provider": "codex",
            "tool": "panel-main",
            "to_state": "queued",
            "actor": {"kind": "owner", "name": "agent-events"},
            "refs": {"panel_role": "main", "panel_room_id": "room-acct", "panel_native_scheduling": "unmanaged"}
        });
        let starting = serde_json::json!({
            "schema": 1,
            "event_id": "aevt_00000000000000000000000000000002",
            "ts": "2026-11-19T00:00:01Z",
            "attempt_id": attempt_id,
            "seq": 2,
            "event_type": "attempt.state_changed",
            "from_state": "queued",
            "to_state": "starting",
            "actor": {"kind": "owner", "name": "agent-events"}
        });
        let working = serde_json::json!({
            "schema": 1,
            "event_id": "aevt_00000000000000000000000000000003",
            "ts": "2026-11-19T00:00:02Z",
            "attempt_id": attempt_id,
            "seq": 3,
            "event_type": "attempt.state_changed",
            "from_state": "starting",
            "to_state": "working",
            "actor": {"kind": "owner", "name": "agent-events"}
        });
        let initial_bytes = format!("{created}\n{starting}\n{working}\n");
        std::fs::write(&events_path, &initial_bytes).unwrap();

        let service = EventsCounterService::new();
        let cache = AttemptCache::default();
        let stop = AtomicBool::new(false);

        // Empty attempt returns Ok(None) with 0 queries.
        assert_eq!(
            cache.resolve_attempt(&repo, "", &service, &stop).unwrap(),
            None
        );
        assert_eq!(service.calls.load(Ordering::Relaxed), 0);

        // Cold call triggers 1 Events query and returns working Main.
        let got = cache
            .resolve_attempt(&repo, attempt_id, &service, &stop)
            .unwrap()
            .unwrap();
        assert_eq!(got["state"], "working");
        assert_eq!(got["terminal"], false);
        assert_eq!(service.calls.load(Ordering::Relaxed), 1);

        // Unchanged calls trigger 0 additional queries (present or missing attempt).
        let cached = cache
            .resolve_attempt(&repo, attempt_id, &service, &stop)
            .unwrap()
            .unwrap();
        assert_eq!(cached["state"], "working");
        assert_eq!(
            cache
                .resolve_attempt(&repo, missing_id, &service, &stop)
                .unwrap(),
            None
        );
        assert_eq!(service.calls.load(Ordering::Relaxed), 1);

        // Appending a row invalidates & refreshes (+1 query) and observes updated state.
        let failed = serde_json::json!({
            "schema": 1,
            "event_id": "aevt_00000000000000000000000000000004",
            "ts": "2026-11-19T00:00:03Z",
            "attempt_id": attempt_id,
            "seq": 4,
            "event_type": "attempt.state_changed",
            "from_state": "working",
            "to_state": "failed",
            "actor": {"kind": "owner", "name": "agent-events"}
        });
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&events_path)
                .unwrap();
            writeln!(f, "{failed}").unwrap();
        }
        let refreshed = cache
            .resolve_attempt(&repo, attempt_id, &service, &stop)
            .unwrap()
            .unwrap();
        assert_eq!(refreshed["state"], "failed");
        assert_eq!(refreshed["terminal"], true);
        assert_eq!(service.calls.load(Ordering::Relaxed), 2);
        // Unchanged after refresh hits cache with 0 queries.
        assert_eq!(
            cache
                .resolve_attempt(&repo, attempt_id, &service, &stop)
                .unwrap()
                .unwrap()["state"],
            "failed"
        );
        assert_eq!(service.calls.load(Ordering::Relaxed), 2);

        // Atomic rename replacement with IDENTICAL byte length (same size, changed inode) invalidates & refreshes (+1 query).
        let orig_meta = std::fs::metadata(&events_path).unwrap();
        std::fs::write(&events_path, &initial_bytes).unwrap();
        let before_swap = cache
            .resolve_attempt(&repo, attempt_id, &service, &stop)
            .unwrap()
            .unwrap();
        assert_eq!(before_swap["state"], "working");
        assert_eq!(service.calls.load(Ordering::Relaxed), 3);
        let pre_meta = std::fs::metadata(&events_path).unwrap();
        // Same byte length: "working" + "agent-events" (19) == "failed" + "agent-events1" (19).
        let same_size_bytes = initial_bytes
            .replacen("\"to_state\":\"working\"", "\"to_state\":\"failed\"", 1)
            .replacen("\"name\":\"agent-events\"", "\"name\":\"agent-events1\"", 1);
        assert_eq!(same_size_bytes.len(), initial_bytes.len());
        let staging = lifecycle.join("events.jsonl.tmp");
        std::fs::write(&staging, &same_size_bytes).unwrap();
        std::fs::rename(&staging, &events_path).unwrap();
        let post_meta = std::fs::metadata(&events_path).unwrap();
        assert_eq!(post_meta.size(), pre_meta.size());
        assert_ne!(post_meta.ino(), pre_meta.ino());
        assert_ne!(orig_meta.size(), 0);
        let replaced = cache
            .resolve_attempt(&repo, attempt_id, &service, &stop)
            .unwrap()
            .unwrap();
        assert_eq!(replaced["state"], "failed");
        assert_eq!(replaced["terminal"], true);
        assert_eq!(service.calls.load(Ordering::Relaxed), 4);

        // Duplicate attempt_id in projection returns Ok(None) and caches unchanged generation (0 queries on next call).
        let dup_cache = AttemptCache::default();
        *service.override_reply.lock().unwrap() = Some(serde_json::json!([
            {"attempt_id": attempt_id, "state": "working"},
            {"attempt_id": attempt_id, "state": "working"}
        ]));
        assert_eq!(
            dup_cache
                .resolve_attempt(&repo, attempt_id, &service, &stop)
                .unwrap(),
            None
        );
        assert_eq!(service.calls.load(Ordering::Relaxed), 5);
        assert_eq!(
            dup_cache
                .resolve_attempt(&repo, attempt_id, &service, &stop)
                .unwrap(),
            None
        );
        assert_eq!(service.calls.load(Ordering::Relaxed), 5);
        *service.override_reply.lock().unwrap() = None;

        // Symlink replacement never returns stale cached data.
        let decoy = lifecycle.join("decoy.jsonl");
        std::fs::write(&decoy, &initial_bytes).unwrap();
        std::fs::remove_file(&events_path).unwrap();
        symlink(&decoy, &events_path).unwrap();
        assert!(
            cache
                .resolve_attempt(&repo, attempt_id, &service, &stop)
                .is_err()
        );
        assert_eq!(service.calls.load(Ordering::Relaxed), 6);

        // Deleting events.jsonl never returns stale cached data and leaves cache unpopulated.
        std::fs::write(&decoy, &initial_bytes).unwrap();
        std::fs::remove_file(&events_path).unwrap();
        std::fs::rename(&decoy, &events_path).unwrap();
        assert_eq!(
            cache
                .resolve_attempt(&repo, attempt_id, &service, &stop)
                .unwrap()
                .unwrap()["state"],
            "working"
        );
        assert_eq!(service.calls.load(Ordering::Relaxed), 7);
        std::fs::remove_file(&events_path).unwrap();
        assert_eq!(
            cache
                .resolve_attempt(&repo, attempt_id, &service, &stop)
                .unwrap(),
            None
        );
        assert_eq!(service.calls.load(Ordering::Relaxed), 8);

        // Mid-verification change retries once and fails closed if it changes again on retry.
        std::fs::write(&events_path, &initial_bytes).unwrap();
        let mut seq_num = 10u64;
        let p_clone = events_path.clone();
        *service.on_query.lock().unwrap() = Some(Box::new(move || {
            seq_num += 1;
            let row = serde_json::json!({
                "schema": 1,
                "event_id": format!("aevt_000000000000000000000000000000{seq_num}"),
                "ts": "2026-11-19T00:00:05Z",
                "attempt_id": attempt_id,
                "seq": seq_num - 7,
                "event_type": "attempt.heartbeat",
                "actor": {"kind": "runner", "name": "attempt-runner"}
            });
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&p_clone)
                .unwrap();
            writeln!(f, "{row}").unwrap();
        }));
        let err = cache
            .resolve_attempt(&repo, attempt_id, &service, &stop)
            .unwrap_err();
        assert!(err.message.contains("changed during verification"), "{err}");
        assert_eq!(service.calls.load(Ordering::Relaxed), 10);
    }
}
